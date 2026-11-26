//! Puts the folder of programs this one runs from in a person's own prefix, so that the desktop finds the application:
//! the first installation from a downloaded archive (`peersus install`), and the way a development deploy installs.
//!
//! The programs go to `<prefix>/lib/peersus`, each written beside the file it replaces and moved over it once whole, so
//! whoever has the old one open keeps it. Once all of them are on disk and read back equal, `<prefix>/bin/peersus` is
//! published, in one step, as a relative link to the real command line. The desktop entry starts that link by its
//! absolute path; the entry, the AppStream file, the icon and the licences are the ones this program was built with.
//!
//! When the release's signed list lies beside the programs (`SHA256SUMS`, `SHA256SUMS.sig`, as the first installation
//! downloads them), it is verified as an update is, every program must be the one it names, and it is kept with them:
//! what the owner checks the installation against at each start, and puts damaged programs back from. Without it (a
//! development deploy) any list an earlier installation left is removed, so local programs are never "repaired" back
//! to a release.
//!
//! Nothing else is touched: no data, no preferences, no extension; nothing is started or stopped. From then on the
//! application keeps this installation current itself (`update_install`), because it is a writable folder of programs.
use crate::{
    app_update::{self, Kind, SignedList, Target},
    update_install::{self, ProgramFolder},
};
use std::{
    collections::BTreeSet,
    fs,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};

/// The programs of a desktop installation, side by side; the first is the command line the launcher names.
pub const PROGRAMS: [&str; 7] = [
    "peersus",
    "peersus-ui",
    "peersus-wasm-host",
    "peersus-wasm-canvas-host",
    "peersus-native-host",
    "peersus-store-fetch",
    "peersus-diag",
];

/// The application's identity on the desktop (see `peersus_ui::APPLICATION_ID`, which a test keeps equal to it).
pub const APPLICATION_ID: &str = "io.github.DarkPhilosophy.Peersus";

const ENTRY: &str =
    include_str!("../../../packaging/linux/io.github.DarkPhilosophy.Peersus.desktop");
const METAINFO: &[u8] =
    include_bytes!("../../../packaging/linux/io.github.DarkPhilosophy.Peersus.metainfo.xml");
const ICON: &[u8] = include_bytes!("../../peersus-ui/assets/peersus-icon.svg");
const LICENSE: &[u8] = include_bytes!("../../../LICENSE");
const NOTICE: &[u8] = include_bytes!("../../../NOTICE");

/// What one installation put in place, relative to its prefix, and what the person should know about it.
#[derive(Debug)]
pub struct Installed {
    pub files: Vec<PathBuf>,
    /// The launcher was a plain file of an older layout and is now the link.
    pub replaced_plain_launcher: bool,
}

/// Installs the programs that lie in `source` under `prefix`. Every refusal is said before anything is written, except
/// a disk that fails while writing, which leaves the programs of the previous installation where they were.
pub fn install(source: &Path, prefix: &Path) -> Result<Installed, String> {
    install_with(
        source,
        prefix,
        app_update::UPDATE_PUBLIC_KEY,
        app_update::CHANNEL,
    )
}

fn install_with(
    source: &Path,
    prefix: &Path,
    key: Option<[u8; 32]>,
    channel: &str,
) -> Result<Installed, String> {
    if update_install::managed().is_some() {
        return Err(
            "This copy of Peersus is kept current by its package; it is not installed again from inside".into(),
        );
    }
    for program in PROGRAMS {
        let path = source.join(program);
        if !fs::symlink_metadata(&path).is_ok_and(|found| found.is_file()) {
            return Err(format!(
                "{program} is not beside this program as a plain file: unpack the whole archive and run the peersus it holds"
            ));
        }
    }
    let list = signed_list(source, key, channel)?;
    let (bin, library) = (prefix.join("bin"), prefix.join("lib/peersus"));
    if fs::symlink_metadata(&library).is_ok_and(|found| found.file_type().is_symlink()) {
        return Err(format!(
            "{} is a link; the programs are not written through it",
            library.display()
        ));
    }
    if same_place(source, &library) {
        return Err(
            "This is the installed copy already; to bring it up to date run `peersus update`"
                .into(),
        );
    }
    let launcher = bin.join(PROGRAMS[0]);
    let replaced_plain_launcher = match fs::symlink_metadata(&launcher) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(_) => return Err(format!("{} cannot be read", launcher.display())),
        Ok(found) if found.is_file() => true,
        Ok(found)
            if found.file_type().is_symlink()
                && same_place(&launcher, &library.join(PROGRAMS[0])) =>
        {
            false
        }
        Ok(_) => {
            return Err(format!(
                "{} is neither the installed link nor a plain file; it is left as it is",
                launcher.display()
            ));
        }
    };
    let entry = entry_for(ENTRY, &absolute(&launcher)?)?;

    let mut files = Vec::new();
    for program in PROGRAMS {
        let from = source.join(program);
        let bytes = fs::read(&from).map_err(|_| format!("{program} cannot be read"))?;
        put(&bytes, &library.join(program), 0o755)?;
        files.push(Path::new("lib/peersus").join(program));
    }
    for program in PROGRAMS {
        if !same_bytes(&source.join(program), &library.join(program)) {
            return Err(
                "The programs written differ from the ones beside this program; the launcher was not changed".into(),
            );
        }
    }
    let kept = [app_update::SUMS_FILE, app_update::SIGNATURE_FILE];
    match &list {
        Some(list) => {
            update_install::keep_list(&library, list)
                .map_err(|_| "The signed list could not be kept beside the programs".to_owned())?;
            files.extend(kept.map(|name| Path::new("lib/peersus").join(name)));
        }
        None => {
            for name in kept {
                match fs::remove_file(library.join(name)) {
                    Err(error) if error.kind() != io::ErrorKind::NotFound => {
                        return Err(format!(
                            "{} of an earlier installation could not be removed",
                            library.join(name).display()
                        ));
                    }
                    _ => {}
                }
            }
        }
    }
    link(&library.join(PROGRAMS[0]), &launcher)?;
    files.push(Path::new("bin").join(PROGRAMS[0]));
    let shared: [(PathBuf, &[u8]); 5] = [
        (
            Path::new("share/applications").join(format!("{APPLICATION_ID}.desktop")),
            entry.as_bytes(),
        ),
        (
            Path::new("share/metainfo").join(format!("{APPLICATION_ID}.metainfo.xml")),
            METAINFO,
        ),
        (
            Path::new("share/icons/hicolor/scalable/apps").join(format!("{APPLICATION_ID}.svg")),
            ICON,
        ),
        (Path::new("share/licenses/peersus/LICENSE").into(), LICENSE),
        (Path::new("share/licenses/peersus/NOTICE").into(), NOTICE),
    ];
    for (relative, bytes) in shared {
        put(bytes, &prefix.join(&relative), 0o644)?;
        files.push(relative);
    }
    refresh_desktop(prefix);
    Ok(Installed {
        files,
        replaced_plain_launcher,
    })
}

/// Whether the launcher's folder is one the shell looks in, so that `peersus` works in a terminal as it is.
pub fn on_path(prefix: &Path) -> bool {
    let bin = prefix.join("bin");
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|entry| same_place(&entry, &bin)))
}

/// The release's signed list beside the programs in `source`, verified, with every program the one it names; `None`
/// when there is none.
fn signed_list(
    source: &Path,
    key: Option<[u8; 32]>,
    channel: &str,
) -> Result<Option<SignedList>, String> {
    let present = |name: &str| fs::symlink_metadata(source.join(name)).is_ok();
    match (
        present(app_update::SUMS_FILE),
        present(app_update::SIGNATURE_FILE),
    ) {
        (false, false) => return Ok(None),
        (true, true) => {}
        _ => {
            return Err(
                "Only one of the signed list's two files is beside this program; unpack the whole download"
                    .into(),
            );
        }
    }
    let folder = ProgramFolder::beside(&absolute(&source.join(PROGRAMS[0]))?, None)
        .map_err(|_| "Where this program is cannot be found")?;
    let list = app_update::Installer::installed_list(&folder)
        .ok_or("The signed list beside this program cannot be read")?;
    let key = app_update::trust_root(key).map_err(|refusal| refusal.0)?;
    let release = app_update::verify(&key, &list.sums, &list.signature, channel)
        .map_err(|refusal| refusal.0)?;
    let target = Target {
        platform: std::env::consts::OS,
        architecture: std::env::consts::ARCH,
        kind: Kind::Archive,
    };
    let artifact = release
        .artifact(&target)
        .ok_or("The signed list names no archive for this system")?;
    if artifact
        .programs
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        != PROGRAMS.into_iter().collect()
    {
        return Err("The signed list names other programs than an installation holds".into());
    }
    for (program, digest) in &artifact.programs {
        if app_update::Installer::program_digest(&folder, program).as_ref() != Some(digest) {
            return Err(format!(
                "{program} is not the one the signed list names; download the archive again"
            ));
        }
    }
    Ok(Some(list))
}

fn same_place(a: &Path, b: &Path) -> bool {
    matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b)
}

fn absolute(path: &Path) -> Result<PathBuf, String> {
    std::path::absolute(path).map_err(|_| "The installation folder cannot be named".into())
}

/// One argument of an Exec line, quoted as the Desktop Entry Specification says when it has to be.
fn exec_argument(path: &Path) -> Result<String, String> {
    let text = path
        .to_str()
        .ok_or("The installation folder's name is not text a desktop entry can hold")?;
    const PLAIN: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_./+@:,";
    if text.chars().all(|c| PLAIN.contains(c)) {
        return Ok(text.to_owned());
    }
    let mut quoted = String::with_capacity(text.len() + 2);
    for c in text.chars() {
        // `\`, `"`, `` ` `` and `$` are escaped inside a quoted argument; a backslash is then written twice in the file,
        // once for the argument and once for the entry's own string.
        match c {
            '\\' => quoted.push_str("\\\\\\\\"),
            '"' | '`' | '$' => {
                quoted.push_str("\\\\");
                quoted.push(c);
            }
            _ => quoted.push(c),
        }
    }
    Ok(format!("\"{quoted}\""))
}

/// The packaged entry with its one `Exec=peersus` line pointed at the launcher, keeping the `%u` a store link comes in.
fn entry_for(text: &str, launcher: &Path) -> Result<String, String> {
    let program = PROGRAMS[0];
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let execs: Vec<usize> = (0..lines.len())
        .filter(|&at| lines[at].starts_with("Exec="))
        .collect();
    let [at] = execs[..] else {
        return Err(
            "The desktop entry does not start peersus in the way the installer writes it".into(),
        );
    };
    let tail = match lines[at]
        .strip_prefix("Exec=")
        .and_then(|rest| rest.strip_prefix(program))
    {
        Some("") => "",
        Some(" %u") => " %u",
        _ => {
            return Err(
                "The desktop entry does not start peersus in the way the installer writes it"
                    .into(),
            );
        }
    };
    lines[at] = format!("Exec={}{tail}", exec_argument(launcher)?);
    Ok(lines.join("\n") + "\n")
}

/// Writes `bytes` as `target` in one step: whoever has the old file open keeps it whole.
fn put(bytes: &[u8], target: &Path, mode: u32) -> Result<(), String> {
    let failed = |_| format!("{} could not be written", target.display());
    let folder = target
        .parent()
        .ok_or("The installation folder cannot be named")?;
    fs::create_dir_all(folder).map_err(failed)?;
    let mut file = tempfile::Builder::new()
        .prefix(".peersus-")
        .tempfile_in(folder)
        .map_err(failed)?;
    file.write_all(bytes).map_err(failed)?;
    file.as_file().sync_all().map_err(failed)?;
    set_mode(file.path(), mode).map_err(failed)?;
    file.persist(target).map_err(|error| failed(error.error))?;
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}
#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

fn same_bytes(a: &Path, b: &Path) -> bool {
    let (Ok(mut a), Ok(mut b)) = (fs::File::open(a), fs::File::open(b)) else {
        return false;
    };
    let (mut left, mut right) = (vec![0; 64 * 1024], vec![0; 64 * 1024]);
    loop {
        let Ok(read) = a.read(&mut left) else {
            return false;
        };
        if read == 0 {
            return b.read(&mut right[..1]).is_ok_and(|more| more == 0);
        }
        if b.read_exact(&mut right[..read]).is_err() || left[..read] != right[..read] {
            return false;
        }
    }
}

/// Publishes `launcher` as a relative link to `program` in one step.
#[cfg(unix)]
fn link(program: &Path, launcher: &Path) -> Result<(), String> {
    let failed = |_| format!("{} could not be published", launcher.display());
    let folder = launcher
        .parent()
        .ok_or("The installation folder cannot be named")?;
    fs::create_dir_all(folder).map_err(failed)?;
    let target = relative(
        &fs::canonicalize(program).map_err(failed)?,
        &fs::canonicalize(folder).map_err(failed)?,
    );
    let scratch = tempfile::Builder::new()
        .prefix(".peersus-link-")
        .tempdir_in(folder)
        .map_err(failed)?;
    let candidate = scratch.path().join(PROGRAMS[0]);
    std::os::unix::fs::symlink(&target, &candidate).map_err(failed)?;
    fs::rename(&candidate, launcher).map_err(failed)
}
#[cfg(not(unix))]
fn link(_program: &Path, _launcher: &Path) -> Result<(), String> {
    Err(UNSUPPORTED.into())
}

/// `to` as seen from the folder `from`; both are absolute and free of links.
fn relative(to: &Path, from: &Path) -> PathBuf {
    let (to, from): (Vec<Component>, Vec<Component>) =
        (to.components().collect(), from.components().collect());
    let shared = to.iter().zip(&from).take_while(|(a, b)| a == b).count();
    let mut path = PathBuf::new();
    for _ in shared..from.len() {
        path.push("..");
    }
    for component in &to[shared..] {
        path.push(component);
    }
    path
}

/// The desktop reads its entries and icons from caches where it has them: they are made again where the tool is there.
/// Without it, what is in place is found after the next login.
fn refresh_desktop(prefix: &Path) {
    let quiet = |program: &str, arguments: &[&std::ffi::OsStr]| {
        let _ = std::process::Command::new(program)
            .args(arguments)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    };
    quiet(
        "update-desktop-database",
        &[prefix.join("share/applications").as_os_str()],
    );
    // A running desktop keeps the icons it read until the theme folder says it changed: its time is moved, as the icon
    // theme specification's installers do, and its cache, where the person's theme has one, is made again.
    let theme = prefix.join("share/icons/hicolor");
    if let Ok(folder) = fs::File::open(&theme) {
        let _ = folder.set_modified(std::time::SystemTime::now());
    }
    if theme.join("icon-theme.cache").is_file() {
        quiet(
            "gtk-update-icon-cache",
            &["--force".as_ref(), "--quiet".as_ref(), theme.as_os_str()],
        );
    }
}

/// What `peersus install` answers where this way of installing does not exist.
pub const UNSUPPORTED: &str =
    "`peersus install` is for Linux; on this system install the package for it";

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn programs(folder: &Path, word: &str) {
        fs::create_dir_all(folder).unwrap();
        for program in PROGRAMS {
            fs::write(folder.join(program), format!("{word} {program}")).unwrap();
        }
    }

    #[test]
    fn a_first_installation_puts_the_programs_the_link_and_the_desktop_entry_in_place() {
        let root = tempfile::tempdir().unwrap();
        let (source, prefix) = (
            root.path().join("unpacked"),
            root.path().join("home/.local"),
        );
        programs(&source, "new");
        let installed = install(&source, &prefix).unwrap();
        assert!(!installed.replaced_plain_launcher);
        for program in PROGRAMS {
            assert_eq!(
                fs::read_to_string(prefix.join("lib/peersus").join(program)).unwrap(),
                format!("new {program}")
            );
        }
        let launcher = prefix.join("bin/peersus");
        assert_eq!(
            fs::read_link(&launcher).unwrap(),
            Path::new("../lib/peersus/peersus")
        );
        assert_eq!(fs::read_to_string(&launcher).unwrap(), "new peersus");
        let entry =
            fs::read_to_string(prefix.join(format!("share/applications/{APPLICATION_ID}.desktop")))
                .unwrap();
        let exec: Vec<_> = entry
            .lines()
            .filter(|line| line.starts_with("Exec="))
            .collect();
        assert_eq!(
            exec,
            [format!(
                "Exec={} %u",
                std::path::absolute(&launcher).unwrap().display()
            )]
        );
        assert!(
            prefix
                .join(format!(
                    "share/icons/hicolor/scalable/apps/{APPLICATION_ID}.svg"
                ))
                .is_file()
        );
        assert!(prefix.join("share/licenses/peersus/LICENSE").is_file());
    }

    #[test]
    fn installing_again_replaces_the_programs_and_keeps_the_link_and_what_is_not_ours() {
        let root = tempfile::tempdir().unwrap();
        let (first, second, prefix) = (
            root.path().join("a"),
            root.path().join("b"),
            root.path().join("p"),
        );
        programs(&first, "old");
        programs(&second, "new");
        install(&first, &prefix).unwrap();
        fs::write(prefix.join("lib/peersus/notes.txt"), "someone's").unwrap();
        // An older layout's plain launcher becomes the link.
        fs::remove_file(prefix.join("bin/peersus")).unwrap();
        fs::write(prefix.join("bin/peersus"), "plain").unwrap();
        let installed = install(&second, &prefix).unwrap();
        assert!(installed.replaced_plain_launcher);
        assert_eq!(
            fs::read_to_string(prefix.join("bin/peersus")).unwrap(),
            "new peersus"
        );
        assert_eq!(
            fs::read_to_string(prefix.join("lib/peersus/notes.txt")).unwrap(),
            "someone's"
        );
        // Nothing half-written is left beside the programs.
        let names: Vec<_> = fs::read_dir(prefix.join("lib/peersus"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.starts_with('.'))
            .collect();
        assert!(names.is_empty(), "{names:?}");
    }

    #[test]
    fn what_cannot_be_installed_is_refused_before_anything_is_written() {
        let root = tempfile::tempdir().unwrap();
        let (source, prefix) = (root.path().join("a"), root.path().join("p"));
        programs(&source, "new");
        // A program missing from the archive.
        fs::remove_file(source.join("peersus-store-fetch")).unwrap();
        assert!(install(&source, &prefix).is_err());
        assert!(!prefix.exists());
        programs(&source, "new");
        // A launcher that is something else.
        fs::create_dir_all(prefix.join("bin/peersus")).unwrap();
        assert!(install(&source, &prefix).is_err());
        assert!(!prefix.join("lib").exists());
        fs::remove_dir(prefix.join("bin/peersus")).unwrap();
        // The installed copy itself.
        install(&source, &prefix).unwrap();
        assert!(install(&prefix.join("lib/peersus"), &prefix).is_err());
    }

    /// Writes beside the programs in `folder` the list a release of them would be signed with.
    fn sign(folder: &Path, seed: [u8; 32]) {
        use ed25519_dalek::Signer;
        use sha2::{Digest, Sha256};
        let stem = format!(
            "peersus-0.2.0-{}.5-{}-{}",
            app_update::CHANNEL,
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        let mut sums = format!("{}  {stem}.tar.gz\n", "a".repeat(64));
        for program in PROGRAMS {
            let digest = hex::encode(Sha256::digest(fs::read(folder.join(program)).unwrap()));
            sums.push_str(&format!("{digest}  {stem}/{program}\n"));
        }
        let mut signed = app_update::SIGNATURE_CONTEXT.to_vec();
        signed.extend_from_slice(sums.as_bytes());
        let signature = ed25519_dalek::SigningKey::from_bytes(&seed).sign(&signed);
        fs::write(folder.join(app_update::SUMS_FILE), sums).unwrap();
        fs::write(
            folder.join(app_update::SIGNATURE_FILE),
            signature.to_bytes(),
        )
        .unwrap();
    }

    #[test]
    fn a_signed_list_beside_the_programs_is_checked_and_kept_and_removed_when_absent() {
        let seed = [5; 32];
        let key = Some(
            ed25519_dalek::SigningKey::from_bytes(&seed)
                .verifying_key()
                .to_bytes(),
        );
        let install =
            |source: &Path, prefix: &Path| install_with(source, prefix, key, app_update::CHANNEL);
        let root = tempfile::tempdir().unwrap();
        let (source, prefix) = (root.path().join("a"), root.path().join("p"));
        let library = prefix.join("lib/peersus");
        programs(&source, "new");
        sign(&source, seed);
        // A program that is not the one the list names: nothing is written.
        fs::write(source.join("peersus-ui"), "other").unwrap();
        assert!(
            install(&source, &prefix)
                .unwrap_err()
                .contains("peersus-ui")
        );
        assert!(!prefix.exists());
        // A list another key signed.
        programs(&source, "new");
        sign(&source, [6; 32]);
        assert!(install(&source, &prefix).is_err());
        assert!(!prefix.exists());
        // Only half of the list.
        sign(&source, seed);
        fs::remove_file(source.join(app_update::SIGNATURE_FILE)).unwrap();
        assert!(install(&source, &prefix).is_err());
        assert!(!prefix.exists());
        sign(&source, seed);
        install(&source, &prefix).unwrap();
        for name in [app_update::SUMS_FILE, app_update::SIGNATURE_FILE] {
            assert_eq!(
                fs::read(library.join(name)).unwrap(),
                fs::read(source.join(name)).unwrap()
            );
        }
        // What the owner will find: the installed programs are whole against the list kept.
        let installed = ProgramFolder::beside(&library.join("peersus"), None).unwrap();
        let target = Target {
            platform: std::env::consts::OS,
            architecture: std::env::consts::ARCH,
            kind: Kind::Archive,
        };
        let verifying = app_update::trust_root(key).unwrap();
        assert!(
            app_update::inspect(&verifying, app_update::CHANNEL, &target, &installed).is_empty()
        );
        fs::write(library.join("peersus-wasm-host"), "damaged").unwrap();
        assert_eq!(
            app_update::inspect(&verifying, app_update::CHANNEL, &target, &installed),
            ["peersus-wasm-host"]
        );
        // A development deploy (no list): local programs, and no list to "repair" them back from.
        let local = root.path().join("local");
        programs(&local, "local");
        install(&local, &prefix).unwrap();
        assert!(!library.join(app_update::SUMS_FILE).exists());
        assert!(!library.join(app_update::SIGNATURE_FILE).exists());
    }

    #[test]
    fn a_launcher_whose_path_needs_quoting_is_quoted_as_the_specification_says() {
        assert_eq!(
            exec_argument(Path::new("/home/a/.local/bin/peersus")).unwrap(),
            "/home/a/.local/bin/peersus"
        );
        assert_eq!(
            exec_argument(Path::new("/home/a b/$x/peersus")).unwrap(),
            r#""/home/a b/\\$x/peersus""#
        );
        assert!(entry_for("Exec=other\n", Path::new("/x")).is_err());
        assert!(entry_for("Exec=peersus\nExec=peersus %u\n", Path::new("/x")).is_err());
        assert_eq!(
            entry_for("A=1\nExec=peersus %u\n", Path::new("/x")).unwrap(),
            "A=1\nExec=/x %u\n"
        );
    }

    #[test]
    fn the_packaged_entry_is_one_the_installer_can_point_at_the_launcher() {
        entry_for(ENTRY, Path::new("/x")).unwrap();
        assert!(ENTRY.contains(&format!("StartupWMClass={APPLICATION_ID}")));
    }
}
