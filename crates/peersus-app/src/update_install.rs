//! How an installation that is a plain folder of programs replaces them.
//!
//! The application's programs sit side by side in one folder (the owner, the window, the helpers).
//! An update for such an installation is an archive of exactly those: plain files, no folders, no
//! links. Each is written beside the program it replaces and moved over it only once every one of
//! them is on disk, so an update that cannot be unpacked changes nothing. Programs that are running
//! keep running what they were started from; the new ones run from the next start.
//!
//! Only files the archive names are touched. Whatever else is in the folder is not this module's.
//!
//! An installation something else keeps current (a system package, a sandboxed bundle) is not
//! replaced from inside: it says so, in words, and nothing is offered.
use crate::app_update::{Installer, Kind, Target, Verified};
use std::{
    ffi::OsString,
    fs,
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

/// The most programs an update may hold.
const MAX_PROGRAMS: usize = 32;
/// The most an update may unpack to. Far past the application, and a bound on a hostile archive.
const MAX_UNPACKED: u64 = 2 * 1024 * 1024 * 1024;

const UNREADABLE: &str = "The update could not be unpacked";
const SHAPE: &str = "The update is not a plain set of programs";
const UNWRITABLE: &str = "The update could not be written beside the application";

/// A folder of programs that can be replaced file by file.
pub struct ProgramFolder {
    directory: PathBuf,
    /// The owner's own file name: an update that does not hold it is not for this installation.
    program: OsString,
    /// Why this installation is not replaced from inside, when something else keeps it current.
    managed: Option<&'static str>,
}

/// Why the running installation is kept current by something else, read from where it runs.
/// `None` for a plain folder of programs.
pub fn managed() -> Option<&'static str> {
    if std::env::var_os("FLATPAK_ID").is_some() || Path::new("/.flatpak-info").exists() {
        return Some("This installation is updated by Flatpak");
    }
    if std::env::var_os("APPIMAGE").is_some() {
        return Some("An AppImage does not replace itself; download the new one");
    }
    None
}

impl ProgramFolder {
    /// The folder `program` (the running owner, as it was at start) is in.
    pub fn beside(program: &Path, managed: Option<&'static str>) -> io::Result<Self> {
        let missing = || io::Error::from(io::ErrorKind::NotFound);
        Ok(Self {
            directory: program
                .parent()
                .filter(|directory| directory.is_absolute())
                .ok_or_else(missing)?
                .to_owned(),
            program: program.file_name().ok_or_else(missing)?.to_owned(),
            managed,
        })
    }
}

/// The staged files of one update. Whatever was not moved into place is removed with it, so a
/// refused update leaves nothing in the folder.
#[derive(Default)]
struct Staged(Vec<(PathBuf, PathBuf)>);

impl Drop for Staged {
    fn drop(&mut self) {
        for (temporary, _) in &self.0 {
            let _ = fs::remove_file(temporary);
        }
    }
}

impl Installer for ProgramFolder {
    fn target(&self) -> Result<Target, &'static str> {
        if let Some(words) = self.managed {
            return Err(words);
        }
        #[cfg(unix)]
        {
            // Replacing a program needs the folder, not the program, to be writable.
            if rustix::fs::access(&self.directory, rustix::fs::Access::WRITE_OK).is_err() {
                return Err("This installation is managed by the system and is updated with it");
            }
            Ok(Target {
                platform: std::env::consts::OS,
                architecture: std::env::consts::ARCH,
                kind: Kind::Archive,
            })
        }
        // A running program cannot be moved over the same way everywhere, and nothing here was
        // seen working anywhere else: said, not tried.
        #[cfg(not(unix))]
        Err("The application cannot replace itself on this system yet")
    }

    fn install(&self, update: Verified, _reopen: bool) -> Result<(), &'static str> {
        self.target()?;
        if update.kind != Kind::Archive {
            return Err("This kind of update cannot be applied to this installation");
        }
        let mut staged = Staged::default();
        let decoded = flate2::read::GzDecoder::new(update.bytes.as_slice()).take(MAX_UNPACKED);
        let mut archive = tar::Archive::new(decoded);
        for entry in archive.entries().map_err(|_| UNREADABLE)? {
            let mut entry = entry.map_err(|_| UNREADABLE)?;
            if entry.header().entry_type() != tar::EntryType::Regular {
                return Err(SHAPE);
            }
            let path = entry.path().map_err(|_| UNREADABLE)?.into_owned();
            let mut parts = path.components();
            let (Some(Component::Normal(name)), None) = (parts.next(), parts.next()) else {
                return Err(SHAPE);
            };
            let name = name
                .to_str()
                .filter(|name| {
                    !name.is_empty() && !name.starts_with('.') && peersus_api::name::plain(name)
                })
                .ok_or(SHAPE)?;
            let place = self.directory.join(name);
            if staged.0.len() == MAX_PROGRAMS || staged.0.iter().any(|(_, taken)| *taken == place) {
                return Err(SHAPE);
            }
            let temporary = self.directory.join(format!(".{name}.update"));
            // Left by an update that was cut short. Never anything else: the name is this module's.
            let _ = fs::remove_file(&temporary);
            let mut options = fs::OpenOptions::new();
            // `create_new` never follows a link someone put where the file goes.
            options.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o755);
            let mut file = options.open(&temporary).map_err(|_| UNWRITABLE)?;
            staged.0.push((temporary, place));
            io::copy(&mut entry, &mut file).map_err(|_| UNREADABLE)?;
            file.sync_all().map_err(|_| UNWRITABLE)?;
        }
        if !staged
            .0
            .iter()
            .any(|(_, place)| place.file_name() == Some(self.program.as_os_str()))
        {
            return Err("The update is not for this installation");
        }
        // Everything is on disk. From here each move replaces one program whole; they are in one
        // folder on one file system, so a move that the first one allowed is allowed for the rest.
        for (temporary, place) in &staged.0 {
            fs::rename(temporary, place).map_err(
                |_| "The update was only partly put in place; install the application again",
            )?;
        }
        if let Ok(directory) = fs::File::open(&self.directory) {
            let _ = directory.sync_all();
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::app_update::Build;
    use peersus_api::package::Version;
    use std::os::unix::fs::PermissionsExt;

    enum Member<'a> {
        File(&'a str, &'a [u8]),
        Link(&'a str, &'a str),
        Folder(&'a str),
    }

    fn archive(members: &[Member<'_>]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for member in members {
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o600);
            match member {
                Member::File(name, bytes) => {
                    header.set_entry_type(tar::EntryType::Regular);
                    header.set_size(bytes.len() as u64);
                    // `set_path` refuses `..`; a hostile archive does not ask: write the name raw.
                    let field = &mut header.as_old_mut().name;
                    field[..name.len()].copy_from_slice(name.as_bytes());
                    header.set_cksum();
                    builder.append(&header, *bytes).unwrap();
                }
                Member::Link(name, to) => {
                    header.set_entry_type(tar::EntryType::Symlink);
                    header.set_size(0);
                    builder.append_link(&mut header, name, to).unwrap();
                }
                Member::Folder(name) => {
                    header.set_entry_type(tar::EntryType::Directory);
                    header.set_size(0);
                    builder.append_data(&mut header, name, io::empty()).unwrap();
                }
            }
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn update(bytes: Vec<u8>) -> Verified {
        Verified {
            build: Build {
                version: Version(0, 1, 0),
                number: 2,
            },
            kind: Kind::Archive,
            bytes,
        }
    }

    fn installation() -> (tempfile::TempDir, ProgramFolder) {
        let folder = tempfile::tempdir().unwrap();
        fs::write(folder.path().join("peersus"), b"old owner").unwrap();
        fs::write(folder.path().join("peersus-ui"), b"old window").unwrap();
        fs::write(folder.path().join("notes.txt"), b"someone's").unwrap();
        let installer = ProgramFolder::beside(&folder.path().join("peersus"), None).unwrap();
        (folder, installer)
    }

    fn names(folder: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(folder)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn the_named_programs_are_replaced_whole_and_nothing_else_is_touched() {
        let (folder, installer) = installation();
        // A program that is running holds its file open: it must keep reading what it started from.
        let mut running = fs::File::open(folder.path().join("peersus")).unwrap();
        installer
            .install(
                update(archive(&[
                    Member::File("peersus", b"new owner"),
                    Member::File("peersus-ui", b"new window"),
                    Member::File("peersus-helper", b"a new helper"),
                ])),
                false,
            )
            .unwrap();
        let read = |name: &str| fs::read(folder.path().join(name)).unwrap();
        assert_eq!(read("peersus"), b"new owner");
        assert_eq!(read("peersus-ui"), b"new window");
        assert_eq!(read("peersus-helper"), b"a new helper");
        assert_eq!(read("notes.txt"), b"someone's");
        assert_eq!(
            names(folder.path()),
            ["notes.txt", "peersus", "peersus-helper", "peersus-ui"]
        );
        let mode = fs::metadata(folder.path().join("peersus-helper"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111 & !current_umask());
        let mut still = Vec::new();
        running.read_to_end(&mut still).unwrap();
        assert_eq!(still, b"old owner");
    }

    // `mode_t` is 16 bits on Apple targets and 32 on the others, so the conversion is the identity on some of them.
    #[allow(clippy::useless_conversion)]
    fn current_umask() -> u32 {
        let mask = rustix::process::umask(rustix::fs::Mode::empty());
        rustix::process::umask(mask);
        mask.bits().into()
    }

    #[test]
    fn an_archive_that_is_not_a_plain_set_of_programs_changes_nothing() {
        let cases: Vec<(Vec<Member<'_>>, &str)> = vec![
            (
                vec![
                    Member::File("peersus", b"x"),
                    Member::File("../escaped", b"x"),
                ],
                SHAPE,
            ),
            (
                vec![
                    Member::File("peersus", b"x"),
                    Member::File("bin/peersus-ui", b"x"),
                ],
                SHAPE,
            ),
            (
                vec![
                    Member::File("peersus", b"x"),
                    Member::File("/tmp/absolute", b"x"),
                ],
                SHAPE,
            ),
            (
                vec![Member::File("peersus", b"x"), Member::File(".hidden", b"x")],
                SHAPE,
            ),
            (
                vec![Member::File("peersus", b"x"), Member::File("a b", b"x")],
                SHAPE,
            ),
            (
                vec![
                    Member::File("peersus", b"x"),
                    Member::Link("peersus-ui", "/etc/passwd"),
                ],
                SHAPE,
            ),
            (
                vec![Member::File("peersus", b"x"), Member::Folder("share")],
                SHAPE,
            ),
            (
                vec![Member::File("peersus", b"x"), Member::File("peersus", b"y")],
                SHAPE,
            ),
            (
                vec![Member::File("peersus-ui", b"x")],
                "The update is not for this installation",
            ),
        ];
        for (members, words) in cases {
            let (folder, installer) = installation();
            assert_eq!(
                installer.install(update(archive(&members)), false),
                Err(words)
            );
            assert_eq!(
                fs::read(folder.path().join("peersus")).unwrap(),
                b"old owner"
            );
            assert_eq!(
                fs::read(folder.path().join("peersus-ui")).unwrap(),
                b"old window"
            );
            // Nothing staged is left behind, and nothing new appeared.
            assert_eq!(names(folder.path()), ["notes.txt", "peersus", "peersus-ui"]);
        }
        let (folder, installer) = installation();
        assert_eq!(
            installer.install(update(b"not an archive".to_vec()), false),
            Err(UNREADABLE)
        );
        assert_eq!(names(folder.path()), ["notes.txt", "peersus", "peersus-ui"]);
    }

    #[test]
    fn another_kind_and_a_managed_or_read_only_installation_are_refused_in_words() {
        let (folder, installer) = installation();
        let mut image = update(archive(&[Member::File("peersus", b"new owner")]));
        image.kind = Kind::Image;
        assert!(installer.install(image, false).is_err());
        assert_eq!(
            fs::read(folder.path().join("peersus")).unwrap(),
            b"old owner"
        );

        let managed = ProgramFolder::beside(
            &folder.path().join("peersus"),
            Some("Kept by something else"),
        )
        .unwrap();
        assert_eq!(managed.target(), Err("Kept by something else"));
        assert!(
            managed
                .install(
                    update(archive(&[Member::File("peersus", b"new owner")])),
                    false
                )
                .is_err()
        );

        // A folder this user cannot write (as root everything is writable: nothing to learn then).
        fs::set_permissions(folder.path(), fs::Permissions::from_mode(0o500)).unwrap();
        if rustix::fs::access(folder.path(), rustix::fs::Access::WRITE_OK).is_err() {
            assert!(installer.target().is_err());
        }
        fs::set_permissions(folder.path(), fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            installer.target().map(|target| target.kind),
            Ok(Kind::Archive)
        );
        // A relative program path names no folder that can be trusted to be the installation.
        assert!(ProgramFolder::beside(Path::new("peersus"), None).is_err());
    }
}
