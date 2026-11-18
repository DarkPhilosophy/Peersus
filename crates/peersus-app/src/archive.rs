//! A plugin package given as one `.tar.gz` (or plain `.tar`) file instead of a folder.
//!
//! WHAT THIS DECIDES. Only how an archive becomes a folder: where its files may go and how much of
//! it is unpacked. It decides nothing about what a package is. The folder it makes is handed to the
//! same reading, staging, confirmation and digest as a folder chosen by hand, so a package from an
//! archive is refused for exactly the reasons a folder is, in the same words, and nothing here is a
//! second copy of those rules.
//!
//! WHAT IT REFUSES. A file that is not an archive. An entry whose name leaves the folder it is
//! unpacked into: absolute, `..`, a NUL or control character, a backslash. A link, a device, a
//! fifo: anything that is not a file or a directory. Too many entries, a file or a whole package
//! that unpacks to more than the package limits, whatever the archive claims about its own sizes:
//! sizes are counted from what is actually written. The same name twice. Nothing is left behind
//! when it refuses.
//!
//! WHERE THE PACKAGE IS. An archive made from a folder usually has that folder as its one top
//! directory, so `plugin.json` is one level down. The package is `plugin.json` at the top, or under
//! the ONE directory that is the only thing at the top. Two directories, or a loose file next to
//! one, is ambiguous and is refused rather than guessed. The folder's name is the plugin's id, so
//! when `plugin.json` is at the top the folder is named after the archive file, not after a random
//! temporary name.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read, Write};
#[cfg(all(test, unix))]
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use crate::discovery::MANIFEST_FILES;
use crate::plugin_manager::{MAX_BYTES, MAX_FILES};

/// Why an archive was not opened. The words are shown to a person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refused(pub &'static str);

/// An unpacked package that removes itself when dropped, so that a refusal, a cancel and a finished
/// install all leave nothing behind.
#[derive(Debug)]
pub struct Unpacked {
    _holder: tempfile::TempDir,
    root: PathBuf,
}

impl Unpacked {
    /// The folder that holds `plugin.json`.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// The two magic numbers an archive may start with: gzip, and the `ustar` marker of a plain tar.
const GZIP: [u8; 2] = [0x1f, 0x8b];

/// Unpack the archive at `archive` into a private folder and find the package in it.
pub fn unpack(archive: &Path) -> Result<Unpacked, Refused> {
    unpack_in(archive, &std::env::temp_dir())
}

/// Most bytes the tar parser may be given from an archive, after any decompression. Above what a
/// package may hold (`MAX_BYTES`), because a package may sit in an archive beside files it does not
/// name, which are read past and never kept; and finite, because the parser reads some records
/// (a long file name, extended attributes) whole into memory before this module sees them, and
/// nothing inside it can stop that. The decoded stream is the one place that can.
pub const MAX_STREAM: u64 = 4 * MAX_BYTES;

/// A selected provider/download stream copied into a private, self-cleaning source.
/// It is only an archive source, never an approval; the ordinary unpacker/installer still validates it.
pub struct SourceArchive {
    _holder: tempfile::TempDir,
    path: PathBuf,
}

impl SourceArchive {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub fn keep_source(
    reader: impl Read,
    name: &str,
    scratch: &Path,
) -> Result<SourceArchive, Refused> {
    let mut components = Path::new(name).components();
    if name.len() > 255
        || name.contains('\\')
        || name.chars().any(char::is_control)
        || !matches!(components.next(), Some(Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(Refused("The selected archive has no usable name"));
    }
    let holder = tempfile::Builder::new()
        .prefix(".peersus-source")
        .tempdir_in(scratch)
        .map_err(|_| Refused("The selected archive could not be staged"))?;
    make_private(holder.path()).map_err(|_| Refused("The selected archive could not be staged"))?;
    let path = holder.path().join(name);
    let mut output = new_file(0o600)
        .open(&path)
        .map_err(|_| Refused("The selected archive could not be staged"))?;
    io::copy(&mut Bounded::new(reader), &mut output).map_err(bad)?;
    Ok(SourceArchive {
        _holder: holder,
        path,
    })
}

/// The prefixes of the scratch folders [`keep_source`] and [`unpack_in`] make. A folder of them is removed when its holder is
/// dropped; one a killed process left behind is only removed by [`sweep_leftovers`].
const SCRATCH_PREFIXES: [&str; 2] = [".peersus-source", ".peersus-archive"];

/// Remove the scratch folders a process that was killed while it unpacked left in `scratch`. Only run where no unpacking
/// can be going on: when the one owner of that folder starts. Nothing else in the folder is touched, and a folder that
/// cannot be read or removed is left as it is. Returns how many were removed.
pub fn sweep_leftovers(scratch: &Path) -> usize {
    let Ok(entries) = fs::read_dir(scratch) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            SCRATCH_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
                // Only a real folder: a link of that name is left alone, and what it points at is never reached.
                && entry.file_type().is_ok_and(|kind| kind.is_dir())
        })
        .filter(|entry| fs::remove_dir_all(entry.path()).is_ok())
        .count()
}

use crate::private_fs::{make_private, new_file, private_folders};

/// A reader that gives at most [`MAX_STREAM`] bytes and then fails with an error that says why,
/// rather than ending quietly, which would read as a damaged archive.
struct Bounded<R> {
    inner: R,
    left: u64,
}

impl<R> Bounded<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            left: MAX_STREAM,
        }
    }
}

/// What a [`Bounded`] reader fails with when the archive is more than it allows.
const OVER: &str = "archive over the stream limit";

impl<R: Read> Read for Bounded<R> {
    fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
        if self.left == 0 {
            // Anything still to come is more than the limit; at the true end this reads nothing.
            let mut probe = [0u8; 1];
            return match self.inner.read(&mut probe)? {
                0 => Ok(0),
                _ => Err(io::Error::other(OVER)),
            };
        }
        let room = into
            .len()
            .min(usize::try_from(self.left).unwrap_or(usize::MAX));
        let read = self.inner.read(&mut into[..room])?;
        self.left -= read as u64;
        Ok(read)
    }
}

/// Whether `error` is a [`Bounded`] reader refusing, possibly after the tar parser wrapped it.
fn is_over(error: &io::Error) -> bool {
    let mut current: Option<&(dyn std::error::Error + 'static)> = error.get_ref().map(|e| e as _);
    while let Some(inner) = current {
        if inner.to_string() == OVER {
            return true;
        }
        current = inner.source();
    }
    error.to_string() == OVER
}

/// What an error from the tar parser or from reading an entry means: the archive is more than the
/// stream limit allows, or it could not be read. One place decides, so that no path reports a
/// package that is too large as one that is damaged.
fn bad(error: io::Error) -> Refused {
    if is_over(&error) {
        Refused("The package is too large")
    } else {
        Refused("The archive could not be read")
    }
}

/// [`unpack`], working in `scratch` instead of the system's temporary folder.
pub fn unpack_in(archive: &Path, scratch: &Path) -> Result<Unpacked, Refused> {
    const UNREADABLE: Refused = Refused("The archive could not be read");
    let file = fs::File::open(archive).map_err(|_| UNREADABLE)?;
    let mut head = [0u8; 2];
    let mut reader = io::BufReader::new(file);
    let read = peek(&mut reader, &mut head).map_err(|_| UNREADABLE)?;
    let stem = archive_stem(archive);

    let holder = tempfile::Builder::new()
        .prefix(".peersus-archive")
        .tempdir_in(scratch)
        .map_err(|_| Refused("The archive could not be unpacked"))?;
    // The temporary folder is private to this user from the first byte written into it.
    make_private(holder.path()).map_err(|_| Refused("The archive could not be unpacked"))?;

    // First pass: read the manifest and nothing else, to learn which files the package names. It
    // writes nothing, so what the package does not name is never on the disk at all.
    let wanted = if read == 2 && head == GZIP {
        read_wanted(Bounded::new(flate2::read::GzDecoder::new(
            io::BufReader::new(fs::File::open(archive).map_err(|_| UNREADABLE)?),
        )))?
    } else {
        read_wanted(Bounded::new(io::BufReader::new(
            fs::File::open(archive).map_err(|_| UNREADABLE)?,
        )))?
    };
    if read == 2 && head == GZIP {
        extract(
            Bounded::new(flate2::read::GzDecoder::new(reader)),
            holder.path(),
            &wanted,
        )?;
    } else {
        extract(Bounded::new(reader), holder.path(), &wanted)?;
    }
    let root = find_root(holder.path(), &stem)?;
    Ok(Unpacked {
        _holder: holder,
        root,
    })
}

/// Look at the first bytes of `reader` without consuming them.
fn peek<R: io::BufRead>(reader: &mut R, into: &mut [u8]) -> io::Result<usize> {
    let buffer = reader.fill_buf()?;
    let n = buffer.len().min(into.len());
    into[..n].copy_from_slice(&buffer[..n]);
    Ok(n)
}

/// The archive's file name without the extensions that say what it is: `clipboard.tar.gz` and
/// `clipboard.tgz` are both `clipboard`.
fn archive_stem(archive: &Path) -> String {
    let name = archive
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    for suffix in [".tar.gz", ".tgz", ".tar"] {
        if let Some(stem) = name.strip_suffix(suffix) {
            return stem.to_owned();
        }
    }
    name
}

/// A name inside the archive that stays inside the folder: only ordinary components, nothing that
/// can mean something else on the way down (a backslash, a control character).
fn safe_relative(name: &Path) -> Option<PathBuf> {
    let text = name.to_str()?;
    if text.is_empty() || text.chars().any(|c| c.is_control() || c == '\\') {
        return None;
    }
    let mut clean = PathBuf::new();
    for component in name.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!clean.as_os_str().is_empty()).then_some(clean)
}

/// What the first pass learned: where the package sits in the archive and which files it names.
struct Wanted {
    /// The one directory the package is under, or nothing when the manifest is at the top.
    prefix: PathBuf,
    /// The file name of the manifest that was found, which is the only manifest the package has: a
    /// second one lying beside it is not part of the package and is never extracted, or the package
    /// would refuse itself for having two.
    manifest: std::ffi::OsString,
    /// The file name of the program the manifest names, or `plugin`: always part of the package.
    program: String,
    /// The files the manifest names, relative to the package; `None` when it names none, which
    /// means every file.
    files: Option<BTreeSet<PathBuf>>,
}

/// Whether `name` is one of the ways a manifest is written.
fn is_manifest(name: &std::ffi::OsStr) -> bool {
    MANIFEST_FILES.iter().any(|manifest| name == *manifest)
}

impl Wanted {
    /// Whether the entry `relative` (as it is in the archive) is part of the package.
    fn takes(&self, relative: &Path) -> bool {
        let Some(files) = &self.files else {
            return true;
        };
        let Ok(inside) = relative.strip_prefix(&self.prefix) else {
            return false;
        };
        inside == Path::new(&self.manifest)
            || inside == Path::new(&self.program)
            || files.contains(inside)
            // The directories on the way to a named file are made when it is written.
            || files.iter().any(|file| file.starts_with(inside))
    }
}

/// Read only the manifest out of the archive: no file is written. What it needs is just the
/// `files` list, read by the SAME reader as a manifest in a folder, under the same size limit, so
/// that a manifest means the same thing whichever way it came and written either way; whether the
/// manifest as a whole is valid is decided later, by the same step that decides it for a folder.
///
/// It does not stop at the first manifest it meets. An archive with two is refused, because which
/// of them was found first is the order the entries happen to be in, and that would let the order
/// choose which list decides what is extracted.
fn read_wanted<R: Read>(source: R) -> Result<Wanted, Refused> {
    let mut archive = tar::Archive::new(source);
    let mut found: Option<Wanted> = None;
    for entry in archive.entries().map_err(bad)? {
        let mut entry = entry.map_err(bad)?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path().map_err(bad)?.into_owned();
        let Some(relative) = safe_relative(&path) else {
            continue; // refused by the second pass, with its own words
        };
        let depth = relative.components().count();
        let Some(name) = relative.file_name().filter(|name| is_manifest(name)) else {
            continue;
        };
        if depth > 2 {
            continue;
        }
        if found.is_some() {
            return Err(Refused("It has two manifests"));
        }
        let mut text = Vec::new();
        (&mut entry)
            .take(crate::discovery::MAX_MANIFEST + 1)
            .read_to_end(&mut text)
            .map_err(bad)?;
        if text.len() as u64 > crate::discovery::MAX_MANIFEST {
            return Err(Refused("Its manifest is too large"));
        }
        let prefix = relative.parent().map(Path::to_path_buf).unwrap_or_default();
        // A manifest that cannot be read names nothing here, so everything is taken: the step that
        // reads it for real refuses it afterwards, as it does for a folder. One reading gives both
        // what it lists and which file is its program.
        let read = crate::discovery::parse_manifest(&name.to_string_lossy(), &text).ok();
        let program = read
            .as_ref()
            .map_or(crate::discovery::PROGRAM_FILE, |manifest| {
                manifest.program_file()
            })
            .to_owned();
        let files = read
            .and_then(|manifest| manifest.files)
            .map(|names| names.iter().map(PathBuf::from).collect());
        found = Some(Wanted {
            prefix,
            manifest: name.to_owned(),
            program,
            files,
        });
    }
    // No manifest anywhere: nothing is named, and the root search refuses it afterwards.
    Ok(found.unwrap_or(Wanted {
        prefix: PathBuf::new(),
        manifest: std::ffi::OsString::new(),
        program: crate::discovery::PROGRAM_FILE.to_owned(),
        files: None,
    }))
}

fn extract<R: Read>(source: R, into: &Path, wanted: &Wanted) -> Result<(), Refused> {
    const BAD: Refused = Refused("The archive could not be read");
    let mut archive = tar::Archive::new(source);
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    let (mut entries, mut bytes) = (0usize, 0u64);
    for entry in archive.entries().map_err(bad)? {
        let mut entry = entry.map_err(bad)?;
        entries += 1;
        if entries > MAX_FILES + 8 {
            return Err(Refused("The package has too many files"));
        }
        let kind = entry.header().entry_type();
        let raw = entry.path().map_err(bad)?.into_owned();
        let relative = safe_relative(&raw).ok_or(Refused("A file of the package is outside it"))?;
        // What the package does not name is not read, written or counted.
        if !wanted.takes(&relative) {
            continue;
        }
        if !seen.insert(relative.clone()) {
            return Err(Refused("The package names the same file twice"));
        }
        let target = into.join(&relative);
        if kind.is_dir() {
            private_folders().create(&target).map_err(|_| BAD)?;
        } else if kind.is_file() {
            if let Some(parent) = target.parent() {
                private_folders().create(parent).map_err(|_| BAD)?;
            }
            // Whether a file may be run is the one thing about its mode a package decides, and it
            // decides it for ANY of its files, exactly as a folder does (`copy_package` keeps the
            // run bit of every file): a package may carry helpers and libraries it starts itself.
            // What is kept is that one bit and only for the owner; nothing is ever group or world.
            let runnable = entry.header().mode().map_err(|_| BAD)? & 0o111 != 0;
            // Counted as it is written, never from the size the header claims.
            let allowed = MAX_BYTES.saturating_sub(bytes);
            let mut out = new_file(if runnable { 0o700 } else { 0o600 })
                .open(&target)
                .map_err(|_| BAD)?;
            let written = io::copy(&mut (&mut entry).take(allowed + 1), &mut out).map_err(bad)?;
            out.flush().map_err(|_| BAD)?;
            bytes = bytes.saturating_add(written);
            if bytes > MAX_BYTES {
                return Err(Refused("The package is too large"));
            }
        } else {
            return Err(Refused("A plugin package holds only files and directories"));
        }
    }
    Ok(())
}

/// The folder that holds the manifest: the top of what was unpacked, or the one directory that is
/// the only thing there. When it is the top, it is given the archive's name so that the plugin has
/// a meaningful id.
fn find_root(top: &Path, stem: &str) -> Result<PathBuf, Refused> {
    const NONE: Refused = Refused("The archive has no manifest");
    let mut names: Vec<(PathBuf, bool)> = Vec::new();
    for entry in fs::read_dir(top).map_err(|_| NONE)? {
        let entry = entry.map_err(|_| NONE)?;
        let is_dir = entry.file_type().map_err(|_| NONE)?.is_dir();
        names.push((entry.path(), is_dir));
    }
    if names
        .iter()
        .any(|(path, dir)| !dir && path.file_name().is_some_and(is_manifest))
    {
        let named = top.join(if stem.is_empty() { "plugin" } else { stem });
        // Everything at the top moves under one folder with the archive's name.
        let holder = tempfile::Builder::new()
            .prefix(".peersus-wrap")
            .tempdir_in(top)
            .map_err(|_| NONE)?;
        for (path, _) in &names {
            let to = holder.path().join(path.file_name().ok_or(NONE)?);
            fs::rename(path, to).map_err(|_| NONE)?;
        }
        let inner = holder.keep();
        fs::rename(&inner, &named).map_err(|_| NONE)?;
        return Ok(named);
    }
    match names.as_slice() {
        [(directory, true)] if crate::discovery::manifest_file(directory).is_ok() => {
            Ok(directory.clone())
        }
        _ => Err(NONE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_descriptor_archive_uses_the_same_unpacker_and_releases_its_source_after_staging() {
        let scratch = tempfile::tempdir().unwrap();
        let bytes = tar_of(&[
            ("plugin.json", b"{}", 0o600, FILE),
            ("plugin", b"module bytes", 0o600, FILE),
        ]);
        let source = keep_source(bytes.as_slice(), "chosen.tar", scratch.path()).unwrap();
        let source_path = source.path().to_path_buf();
        let unpacked = unpack_in(source.path(), scratch.path()).unwrap();
        drop(source);
        assert!(
            !source_path.exists(),
            "an imported descriptor must not leave a raw archive behind"
        );
        assert_eq!(
            fs::read(unpacked.root().join("plugin")).unwrap(),
            b"module bytes"
        );
        drop(unpacked);
        assert_eq!(fs::read_dir(scratch.path()).unwrap().count(), 0);
    }

    #[test]
    fn descriptor_names_never_escape_the_private_source_and_reader_failures_leave_nothing() {
        let scratch = tempfile::tempdir().unwrap();
        for name in [
            "",
            ".",
            "..",
            "../outside.tar",
            "/outside.tar",
            "folder/name.tar",
            "folder\\name.tar",
            "bad\nname.tar",
        ] {
            assert!(keep_source(&b"not installed"[..], name, scratch.path()).is_err());
        }
        struct Failed;
        impl Read for Failed {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("provider failed"))
            }
        }
        assert!(keep_source(Failed, "chosen.tar", scratch.path()).is_err());
        assert_eq!(fs::read_dir(scratch.path()).unwrap().count(), 0);
    }

    /// An archive in memory, written by hand so that a name no tool would write can be written:
    /// each entry is (name, bytes, mode, type).
    fn tar_of(entries: &[(&str, &[u8], u32, tar::EntryType)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, bytes, mode, kind) in entries {
            let mut header = tar::Header::new_gnu();
            // Written into the header bytes directly: `set_path` itself refuses `..`.
            let field = &mut header.as_old_mut().name;
            field.fill(0);
            assert!(name.len() < field.len(), "name too long for the test");
            field[..name.len()].copy_from_slice(name.as_bytes());
            header.set_size(bytes.len() as u64);
            header.set_mode(*mode);
            header.set_entry_type(*kind);
            if *kind == tar::EntryType::Symlink {
                header.set_link_name("/etc/passwd").unwrap();
                header.set_size(0);
            }
            header.set_cksum();
            out.extend_from_slice(header.as_bytes());
            if *kind != tar::EntryType::Symlink {
                out.extend_from_slice(bytes);
                out.resize(out.len().div_ceil(512) * 512, 0);
            }
        }
        out.extend_from_slice(&[0u8; 1024]);
        out
    }

    fn gz(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    const FILE: tar::EntryType = tar::EntryType::Regular;
    const DIR: tar::EntryType = tar::EntryType::Directory;
    const MANIFEST: &[u8] = b"{}";

    fn open_named(bytes: Vec<u8>, name: &str) -> Result<Unpacked, Refused> {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join(name);
        fs::write(&path, bytes).unwrap();
        unpack(&path)
    }

    fn open(bytes: Vec<u8>) -> Result<Unpacked, Refused> {
        open_named(bytes, "package.tar.gz")
    }

    fn package(entries: &[(&str, &[u8], u32, tar::EntryType)]) -> Vec<u8> {
        gz(&tar_of(entries))
    }

    #[test]
    fn a_package_at_the_top_of_the_archive_is_found_and_named_after_the_archive() {
        let unpacked = open_named(
            package(&[
                ("plugin.json", MANIFEST, 0o644, FILE),
                ("plugin", b"#!/bin/sh\n", 0o755, FILE),
            ]),
            "clipboard.tar.gz",
        )
        .unwrap();
        assert_eq!(unpacked.root().file_name().unwrap(), "clipboard");
        assert_eq!(
            fs::read(unpacked.root().join("plugin.json")).unwrap(),
            MANIFEST
        );
        assert_eq!(
            fs::read(unpacked.root().join("plugin")).unwrap(),
            b"#!/bin/sh\n"
        );
    }

    #[test]
    fn an_archive_made_from_a_folder_is_found_one_level_down() {
        // What `tar czf clipboard.tar.gz clipboard/` makes: the folder is the one thing at the top.
        let unpacked = open(package(&[
            ("clipboard/", b"", 0o755, DIR),
            ("clipboard/plugin.json", MANIFEST, 0o644, FILE),
            ("clipboard/plugin", b"x", 0o755, FILE),
        ]))
        .unwrap();
        assert_eq!(unpacked.root().file_name().unwrap(), "clipboard");
        assert!(unpacked.root().join("plugin.json").is_file());
    }

    #[test]
    fn a_plain_tar_and_a_gzipped_one_both_open() {
        let plain = tar_of(&[("plugin.json", MANIFEST, 0o644, FILE)]);
        assert!(open_named(plain.clone(), "a.tar").is_ok());
        assert!(open_named(gz(&plain), "a.tgz").is_ok());
    }

    #[test]
    fn what_is_ambiguous_about_where_the_package_is_is_refused_not_guessed() {
        type Case = (
            &'static str,
            Vec<(&'static str, &'static [u8], u32, tar::EntryType)>,
        );
        let cases: [Case; 4] = [
            (
                "two directories",
                vec![
                    ("a/plugin.json", MANIFEST, 0o644, FILE),
                    ("b/plugin.json", MANIFEST, 0o644, FILE),
                ],
            ),
            (
                "a loose file beside the directory",
                vec![
                    ("a/plugin.json", MANIFEST, 0o644, FILE),
                    ("README", b"x", 0o644, FILE),
                ],
            ),
            (
                "two levels down",
                vec![("a/b/plugin.json", MANIFEST, 0o644, FILE)],
            ),
            ("no manifest at all", vec![("plugin", b"x", 0o755, FILE)]),
        ];
        for (name, entries) in cases {
            assert!(open(package(&entries)).is_err(), "{name} was accepted");
        }
    }

    #[test]
    fn a_name_that_leaves_the_folder_is_refused_and_nothing_is_written_outside() {
        let hostile = [
            "../escaped",
            "a/../../escaped",
            "/tmp/peersus-escaped-by-test",
            "a/./../../escaped",
            "a\\..\\..\\escaped",
            "tab\tname",
        ];
        for name in hostile {
            let archive = package(&[
                ("plugin.json", MANIFEST, 0o644, FILE),
                (name, b"x", 0o644, FILE),
            ]);
            assert!(open(archive).is_err(), "{name:?} was accepted");
        }
        assert!(!Path::new("/tmp/peersus-escaped-by-test").exists());
    }

    #[test]
    fn a_link_is_refused() {
        let archive = package(&[
            ("plugin.json", MANIFEST, 0o644, FILE),
            ("plugin", b"", 0o777, tar::EntryType::Symlink),
        ]);
        assert_eq!(
            open(archive).unwrap_err(),
            Refused("A plugin package holds only files and directories")
        );
    }

    #[test]
    fn a_name_that_appears_twice_is_refused() {
        let archive = package(&[
            ("plugin.json", MANIFEST, 0o644, FILE),
            ("plugin.json", b"{\"other\":1}", 0o644, FILE),
        ]);
        assert!(open(archive).is_err());
    }

    #[test]
    fn too_many_entries_are_refused() {
        let names: Vec<String> = (0..MAX_FILES + 16).map(|n| format!("f{n}")).collect();
        let mut entries: Vec<(&str, &[u8], u32, tar::EntryType)> =
            vec![("plugin.json", MANIFEST, 0o644, FILE)];
        entries.extend(names.iter().map(|n| (n.as_str(), &b"x"[..], 0o644, FILE)));
        assert!(open(package(&entries)).is_err());
    }

    #[test]
    fn what_it_unpacks_to_is_counted_from_what_is_written_and_a_small_archive_cannot_fill_the_disk()
    {
        // A small archive that expands to more than the whole package may hold.
        let big = vec![0u8; (MAX_BYTES + 1) as usize];
        let archive = package(&[
            ("plugin.json", MANIFEST, 0o644, FILE),
            ("plugin", &big, 0o755, FILE),
        ]);
        assert!(
            archive.len() < 1024 * 1024,
            "the bomb is not small, so this proves nothing"
        );
        assert_eq!(
            open(archive).unwrap_err(),
            Refused("The package is too large")
        );
    }

    #[test]
    fn something_that_is_not_an_archive_is_refused() {
        assert!(open(b"this is not an archive".to_vec()).is_err());
        assert!(open(Vec::new()).is_err());
        assert!(open(gz(b"gzip of something that is not a tar")).is_err());
    }

    #[test]
    fn a_refused_archive_leaves_nothing_behind_and_a_finished_one_cleans_up_when_dropped() {
        // Its own folder, so that tests running at the same time cannot be counted by mistake.
        let work = tempfile::tempdir().unwrap();
        let file = work.path().join("a.tar.gz");
        let left = || {
            fs::read_dir(work.path())
                .unwrap()
                .filter_map(Result::ok)
                .count()
        };

        fs::write(&file, package(&[("plugin", b"x", 0o755, FILE)])).unwrap();
        assert!(unpack_in(&file, work.path()).is_err());
        assert_eq!(left(), 1, "a refusal left files besides the archive itself");

        fs::write(&file, package(&[("plugin.json", MANIFEST, 0o644, FILE)])).unwrap();
        let unpacked = unpack_in(&file, work.path()).unwrap();
        let root = unpacked.root().to_path_buf();
        assert!(root.exists());
        drop(unpacked);
        assert!(!root.exists(), "the unpacked package outlived its owner");
        assert_eq!(left(), 1, "something outlived the package");
    }

    #[cfg(unix)]
    #[test]
    fn a_file_keeps_the_run_bit_it_had_as_a_folder_does_for_its_owner_only() {
        // A package may carry a second program and libraries it starts itself: PlugMeIn has two
        // programs and three shared libraries. What a file may do is the one thing its mode says.
        let unpacked = open(package(&[
            ("plugin.json", MANIFEST, 0o644, FILE),
            ("plugin", b"x", 0o755, FILE),
            ("helper", b"h", 0o755, FILE),
            ("libwork.so", b"l", 0o755, FILE),
            ("settings.toml", b"t", 0o666, FILE),
            ("README.md", b"r", 0o644, FILE),
        ]))
        .unwrap();
        let mode = |name: &str| {
            fs::metadata(unpacked.root().join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        for runnable in ["plugin", "helper", "libwork.so"] {
            assert_eq!(
                mode(runnable),
                0o700,
                "{runnable} lost the run bit, or is open to others"
            );
        }
        for plain in ["plugin.json", "settings.toml", "README.md"] {
            assert_eq!(
                mode(plain),
                0o600,
                "{plain} became runnable, or is open to others"
            );
        }
        // Even a file marked writable by everyone is never writable by anyone but its owner.
        assert_eq!(mode("settings.toml") & 0o077, 0);
    }

    /// A manifest, as bytes, that names these files.
    fn naming(files: &[&str]) -> Vec<u8> {
        let list: Vec<String> = files.iter().map(|f| format!("{f:?}")).collect();
        format!(
            "{{\"manifest_version\":1,\"capabilities\":{{}},\"imports\":[],\"exports\":[],\"files\":[{}]}}",
            list.join(",")
        )
        .into_bytes()
    }

    #[test]
    fn an_archive_that_names_its_files_extracts_only_those_and_the_rest_never_reaches_the_disk() {
        // PlugMeIn: programs and libraries and settings, and a README nobody wants installed. The
        // README is not left out of the store afterwards; it is never written anywhere.
        let unpacked = open(package(&[
            (
                "plugin.json",
                &naming(&["lib/libwork.so", "settings.toml"]),
                0o644,
                FILE,
            ),
            ("plugin", b"x", 0o755, FILE),
            ("lib/libwork.so", b"l", 0o755, FILE),
            ("settings.toml", b"t", 0o644, FILE),
            ("README.md", b"read me", 0o644, FILE),
            ("Documents/private.txt", b"mine", 0o644, FILE),
        ]))
        .unwrap();
        let root = unpacked.root();
        for named in ["plugin.json", "plugin", "lib/libwork.so", "settings.toml"] {
            assert!(root.join(named).is_file(), "{named} was not extracted");
        }
        assert!(
            !root.join("README.md").exists(),
            "a file the package did not name was written"
        );
        assert!(
            !root.join("Documents").exists(),
            "a folder the package did not name was written"
        );
    }

    #[test]
    fn what_is_not_named_does_not_count_against_the_limits_of_the_package() {
        // A small package inside a big archive: the unnamed part is skipped, not counted, so a
        // package is not refused for the size of what it will not install.
        let big = vec![0u8; (MAX_BYTES + 1) as usize];
        let unpacked = open(package(&[
            ("plugin.json", &naming(&[]), 0o644, FILE),
            ("plugin", b"x", 0o755, FILE),
            ("sources.bin", &big, 0o644, FILE),
        ]));
        assert!(
            unpacked.is_ok(),
            "the size of a file that is not installed refused the package"
        );
    }

    #[test]
    fn an_archive_without_a_file_list_still_extracts_everything() {
        let unpacked = open(package(&[
            ("plugin.json", MANIFEST, 0o644, FILE),
            ("plugin", b"x", 0o755, FILE),
            ("extra.txt", b"e", 0o644, FILE),
        ]))
        .unwrap();
        assert!(unpacked.root().join("extra.txt").is_file());
    }

    #[test]
    fn what_is_skipped_is_still_held_to_the_rules_for_a_name_that_leaves_the_folder() {
        // Not named, so never written; but a name that tries to leave the folder is refused anyway,
        // because an archive that carries one is not one to trust, named or not.
        let archive = package(&[
            ("plugin.json", &naming(&[]), 0o644, FILE),
            ("plugin", b"x", 0o755, FILE),
            ("../escaped", b"x", 0o644, FILE),
        ]);
        assert_eq!(
            open(archive).unwrap_err(),
            Refused("A file of the package is outside it")
        );
    }

    #[test]
    fn a_list_in_a_package_under_a_top_directory_is_relative_to_that_directory() {
        // `tar czf plugmein.tar.gz plugmein/`: the list says `lib/libwork.so`, the archive says
        // `plugmein/lib/libwork.so`, and a README beside the manifest is still left out.
        let unpacked = open(package(&[
            ("plugmein/", b"", 0o755, DIR),
            (
                "plugmein/plugin.json",
                &naming(&["lib/libwork.so"]),
                0o644,
                FILE,
            ),
            ("plugmein/plugin", b"x", 0o755, FILE),
            ("plugmein/lib/libwork.so", b"l", 0o755, FILE),
            ("plugmein/README.md", b"r", 0o644, FILE),
        ]))
        .unwrap();
        assert_eq!(unpacked.root().file_name().unwrap(), "plugmein");
        assert!(unpacked.root().join("lib/libwork.so").is_file());
        assert!(!unpacked.root().join("README.md").exists());
    }

    /// A manifest, as JSON, that names its program and these files.
    fn naming_program(program: &str, files: &[&str]) -> Vec<u8> {
        let list: Vec<String> = files.iter().map(|f| format!("{f:?}")).collect();
        format!(
            "{{\"manifest_version\":1,\"capabilities\":{{}},\"imports\":[],\"exports\":[],\"program\":{program:?},\"files\":[{}]}}",
            list.join(",")
        )
        .into_bytes()
    }

    #[test]
    fn an_archive_that_names_its_program_extracts_that_file_and_not_a_stray_one_called_plugin() {
        let unpacked = open(package(&[
            (
                "plugin.json",
                &naming_program("peersus-thing", &["settings.toml"]),
                0o644,
                FILE,
            ),
            ("peersus-thing", b"p", 0o755, FILE),
            ("settings.toml", b"t", 0o644, FILE),
            ("plugin", b"stray", 0o755, FILE),
            ("README.md", b"r", 0o644, FILE),
        ]))
        .unwrap();
        let root = unpacked.root();
        for named in ["plugin.json", "peersus-thing", "settings.toml"] {
            assert!(root.join(named).is_file(), "{named} was not extracted");
        }
        assert!(
            !root.join("plugin").exists(),
            "a file called plugin that is not the program was extracted"
        );
        assert!(!root.join("README.md").exists());
    }

    #[cfg(unix)]
    #[test]
    fn the_program_keeps_its_run_bit_under_its_own_name_and_a_plain_file_called_plugin_does_not_get_one()
     {
        use std::os::unix::fs::PermissionsExt;
        let unpacked = open(package(&[
            (
                "plugin.json",
                &naming_program("peersus-thing", &["plugin"]),
                0o644,
                FILE,
            ),
            ("peersus-thing", b"p", 0o755, FILE),
            ("plugin", b"data", 0o644, FILE),
        ]))
        .unwrap();
        let mode = |name: &str| {
            fs::metadata(unpacked.root().join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode("peersus-thing"), 0o700);
        assert_eq!(mode("plugin"), 0o600);
    }

    #[test]
    fn a_manifest_that_is_there_twice_is_refused_so_the_list_that_decided_cannot_be_the_other_one()
    {
        let archive = package(&[
            ("plugin.json", &naming(&["a"]), 0o644, FILE),
            ("plugin", b"x", 0o755, FILE),
            ("a", b"a", 0o644, FILE),
            ("plugin.json", &naming(&["b"]), 0o644, FILE),
        ]);
        assert!(open(archive).is_err());
    }

    #[test]
    fn a_manifest_of_the_other_kind_lying_in_a_directory_the_package_never_named_is_left_out() {
        // Only one manifest decides (the TOML one), and a second manifest lies in a directory the
        // package never named. Were it extracted, the installed package would refuse itself at the
        // next start for having two manifests.
        let unpacked = open(package(&[
            ("plugmein/", b"", 0o755, DIR),
            (
                "plugmein/plugin.toml",
                &naming_toml(&["lib/a.so"]),
                0o644,
                FILE,
            ),
            ("plugmein/plugin", b"x", 0o755, FILE),
            ("plugmein/lib/a.so", b"a", 0o755, FILE),
            ("plugmein/other/plugin.json", b"{}", 0o644, FILE),
        ]))
        .unwrap();
        assert!(unpacked.root().join("plugin.toml").is_file());
        assert!(!unpacked.root().join("plugin.json").exists());
        assert!(!unpacked.root().join("other").exists());
        // The package as it was unpacked is one the application would load: exactly one manifest.
        assert!(crate::discovery::manifest_file(unpacked.root()).is_ok());
    }

    #[test]
    fn a_manifest_too_large_to_be_one_is_refused_in_the_first_pass_without_extracting_anything() {
        let mut huge = naming(&[]);
        huge.extend(std::iter::repeat_n(
            b' ',
            (crate::discovery::MAX_MANIFEST as usize) + 1,
        ));
        let archive = package(&[
            ("plugin.json", &huge, 0o644, FILE),
            ("plugin", b"x", 0o755, FILE),
        ]);
        assert_eq!(
            open(archive).unwrap_err(),
            Refused("Its manifest is too large")
        );
    }

    /// A manifest written as TOML that names these files.
    fn naming_toml(files: &[&str]) -> Vec<u8> {
        let list: Vec<String> = files.iter().map(|f| format!("{f:?}")).collect();
        format!(
            "manifest_version = 1\nimports = []\nexports = []\nfiles = [{}]\n\n[capabilities]\n",
            list.join(", ")
        )
        .into_bytes()
    }

    #[test]
    fn an_archive_whose_manifest_is_toml_is_held_to_its_file_list_as_a_json_one_is() {
        // The trap: reading a TOML manifest as JSON finds no list, and no list means EVERYTHING is
        // extracted, silently wider than the same package as a folder.
        let unpacked = open(package(&[
            ("plugin.toml", &naming_toml(&["settings.toml"]), 0o644, FILE),
            ("plugin", b"x", 0o755, FILE),
            ("settings.toml", b"t", 0o644, FILE),
            ("README.md", b"r", 0o644, FILE),
            ("Documents/private.txt", b"mine", 0o644, FILE),
        ]))
        .unwrap();
        let root = unpacked.root();
        for named in ["plugin.toml", "plugin", "settings.toml"] {
            assert!(root.join(named).is_file(), "{named} was not extracted");
        }
        assert!(
            !root.join("README.md").exists(),
            "a TOML manifest's list was ignored"
        );
        assert!(!root.join("Documents").exists());
    }

    /// An archive whose first entry is a GNU long-name record with a body of `body` bytes: small
    /// once gzipped, enormous decoded, and read whole into memory by the tar parser itself.
    fn long_name_bomb(body: usize) -> Vec<u8> {
        let mut header = tar::Header::new_gnu();
        let name = b"././@LongLink";
        header.as_old_mut().name[..name.len()].copy_from_slice(name);
        header.set_size(body as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::GNULongName);
        header.set_cksum();
        let mut out = header.as_bytes().to_vec();
        out.extend(std::iter::repeat_n(0u8, body));
        out.resize(out.len().div_ceil(512) * 512, 0);
        out.extend_from_slice(&[0u8; 1024]);
        gz(&out)
    }

    #[test]
    fn a_small_archive_that_decodes_to_a_huge_metadata_record_is_refused_before_it_is_buffered() {
        // tar reads a long-name body whole into memory before this module sees the entry, so the
        // only place to stop it is the decoded stream. Larger than anything a package may hold.
        let bomb = long_name_bomb((MAX_STREAM + 1) as usize);
        assert!(
            (bomb.len() as u64) * 100 < MAX_STREAM,
            "the bomb is not small ({} bytes for {MAX_STREAM}), so this proves nothing",
            bomb.len()
        );
        assert_eq!(open(bomb).unwrap_err(), Refused("The package is too large"));
    }

    #[test]
    fn a_large_file_the_package_does_not_name_still_passes_under_the_stream_limit() {
        // The case the stream limit must not break: more than a package may hold, but not selected,
        // so it is read past and never kept.
        let big = vec![0u8; (MAX_BYTES + 1) as usize];
        let unpacked = open(package(&[
            ("plugin.json", &naming(&[]), 0o644, FILE),
            ("plugin", b"x", 0o755, FILE),
            ("sources.bin", &big, 0o644, FILE),
        ]));
        assert!(unpacked.is_ok());
        const {
            assert!(
                MAX_STREAM > MAX_BYTES,
                "the stream limit must leave room above a package's own size"
            )
        };
    }

    #[test]
    fn an_archive_whose_manifest_is_toml_under_a_top_directory_is_found_and_held_to_its_list() {
        let unpacked = open(package(&[
            ("plugmein/", b"", 0o755, DIR),
            (
                "plugmein/plugin.toml",
                &naming_toml(&["lib/libwork.so"]),
                0o644,
                FILE,
            ),
            ("plugmein/plugin", b"x", 0o755, FILE),
            ("plugmein/lib/libwork.so", b"l", 0o755, FILE),
            ("plugmein/README.md", b"r", 0o644, FILE),
        ]))
        .unwrap();
        assert_eq!(unpacked.root().file_name().unwrap(), "plugmein");
        assert!(unpacked.root().join("lib/libwork.so").is_file());
        assert!(!unpacked.root().join("README.md").exists());
    }

    #[test]
    fn an_archive_with_both_manifests_is_refused_whichever_comes_first() {
        let json = naming(&[]);
        let toml = naming_toml(&[]);
        for entries in [
            vec![
                ("plugin.json", &json, 0o644, FILE),
                ("plugin.toml", &toml, 0o644, FILE),
            ],
            vec![
                ("plugin.toml", &toml, 0o644, FILE),
                ("plugin.json", &json, 0o644, FILE),
            ],
        ] {
            let mut all = entries;
            let program = b"x".to_vec();
            all.push(("plugin", &program, 0o755, FILE));
            let owned: Vec<(&str, &[u8], u32, tar::EntryType)> = all
                .iter()
                .map(|(n, b, m, k)| (*n, b.as_slice(), *m, *k))
                .collect();
            assert!(
                open(package(&owned)).is_err(),
                "an archive with two manifests was accepted"
            );
        }
    }

    /// The archive a person was actually handed, made by the system's own `tar`, not by this file's
    /// helpers. Run it by hand with `PEERSUS_REAL_ARCHIVE=<path> cargo test -- --ignored real_`.
    #[test]
    #[ignore = "needs a real archive made by the system tar"]
    fn real_archive_made_by_the_system_tar_opens_as_a_package() {
        let Some(path) = std::env::var_os("PEERSUS_REAL_ARCHIVE") else {
            panic!("set PEERSUS_REAL_ARCHIVE to the archive to open");
        };
        let unpacked = unpack(Path::new(&path)).expect("the archive opens");
        let root = unpacked.root();
        assert!(root.join("plugin.json").is_file());
        // And it is what the owner reads and would show: the same manifest, valid.
        let manifest = crate::discovery::read_package(root).expect("the manifest reads");
        manifest.validate().expect("the manifest is valid");
        // The program is the file the manifest names, which is also what a process list will show.
        let program = root.join(manifest.program_file());
        assert!(
            program.is_file(),
            "the program the manifest names is not in the archive"
        );
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&program).unwrap().permissions().mode() & 0o777,
            0o700
        );
        if manifest.program_file() != crate::discovery::PROGRAM_FILE {
            assert!(
                !root.join(crate::discovery::PROGRAM_FILE).exists(),
                "a stray file called plugin came along"
            );
        }
        assert_eq!(
            root.file_name().unwrap(),
            "clipboard",
            "the folder name is the plugin id"
        );
    }

    #[test]
    fn a_scratch_folder_a_killed_process_left_is_swept_and_nothing_else_is() {
        let scratch = tempfile::tempdir().unwrap();
        let left_by_unpack = scratch.path().join(".peersus-archiveAbC123");
        fs::create_dir_all(left_by_unpack.join("test")).unwrap();
        fs::write(left_by_unpack.join("test/plugin.json"), b"{}").unwrap();
        fs::create_dir(scratch.path().join(".peersus-sourceXyZ")).unwrap();
        // What is not scratch stays: an installed plugin, a file that only looks like scratch, and what a link points at.
        fs::create_dir(scratch.path().join("test")).unwrap();
        fs::write(scratch.path().join(".peersus-archive-note"), b"a file").unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        fs::write(elsewhere.path().join("kept"), b"x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            elsewhere.path(),
            scratch.path().join(".peersus-archiveLink"),
        )
        .unwrap();
        assert_eq!(sweep_leftovers(scratch.path()), 2);
        assert!(!left_by_unpack.exists());
        assert!(!scratch.path().join(".peersus-sourceXyZ").exists());
        assert!(scratch.path().join("test").is_dir());
        assert!(scratch.path().join(".peersus-archive-note").is_file());
        assert!(elsewhere.path().join("kept").is_file());
        // A folder that is not there is nothing to sweep.
        assert_eq!(sweep_leftovers(&scratch.path().join("missing")), 0);
    }
}
