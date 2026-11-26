//! Which display server is this person's session's, asked of the session itself and not only of
//! what this process was started with; and the authentication of that same-user local server,
//! recovered without changing process-wide environment variables or looking through unrelated
//! authentication files.
//!
//! WHY NOT THE ENVIRONMENT ALONE. The owner is started by whatever needed it first (a window, a
//! command line, another program) and outlives it; what it inherited may name no Wayland display,
//! or an X display that is not this session's or is gone. A pointer drawn by what was inherited is
//! then not drawn at all, or drawn where nobody looks, with no error anywhere: the device moves
//! and the person sees nothing. So the environment is the first thing asked, and never the only.
use std::{
    fs::{self, OpenOptions},
    io::{self, Read},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};
use x11rb::{
    reexports::x11rb_protocol::parse_display::{self, ConnectAddress},
    rust_connection::{DefaultStream, RustConnection},
};
use zeroize::Zeroizing;

const MAX_AUTHORITY: usize = 64 * 1024;
const PROTOCOL: &[u8] = b"MIT-MAGIC-COOKIE-1";

fn unavailable() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Local display authentication is unavailable",
    )
}

/// Whether this person's desktop is a Wayland one: this process was told of a Wayland display, or
/// a compositor of this user listens in the session's runtime directory. A socket nobody listens
/// on any more (a compositor that ended) does not count.
pub(super) fn wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some_and(|name| !name.is_empty())
        || std::env::var_os("XDG_RUNTIME_DIR").is_some_and(|directory| {
            wayland_in(Path::new(&directory), rustix::process::getuid().as_raw())
        })
}

fn wayland_in(directory: &Path, uid: u32) -> bool {
    use std::os::unix::fs::FileTypeExt;
    let Ok(entries) = fs::read_dir(directory) else {
        return false;
    };
    entries.flatten().take(256).any(|entry| {
        entry.file_name().to_str().is_some_and(|name| {
            name.strip_prefix("wayland-").is_some_and(|number| {
                !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
            })
        }) && fs::symlink_metadata(entry.path())
            .is_ok_and(|file| file.file_type().is_socket() && file.uid() == uid)
            && std::os::unix::net::UnixStream::connect(entry.path()).is_ok()
    })
}

/// The local displays to try, in order, each with its screen: the one this process was told of,
/// then every other one that has a socket here, lowest number first (a session's own is the first
/// it made).
fn candidates(told: Option<(u16, u16)>, sockets: &Path) -> Vec<(u16, u16)> {
    let mut others: Vec<u16> = fs::read_dir(sockets)
        .into_iter()
        .flatten()
        .flatten()
        .take(256)
        .filter_map(|entry| entry.file_name().to_str()?.strip_prefix('X')?.parse().ok())
        .filter(|display| told.is_none_or(|(named, _)| named != *display))
        .collect();
    others.sort_unstable();
    others.dedup();
    told.into_iter()
        .chain(others.into_iter().map(|display| (display, 0)))
        .collect()
}

/// Connect to the display server of this person's session.
///
/// First the session's own server, proved to be one: a local socket whose other end is this
/// user's, authenticated with the exact authority file named by that server (or its Xwayland
/// child), for the display this process was told of and then for every other local one. An
/// inherited Wayland session can omit XAUTHORITY, and an inherited display can be another
/// session's or gone. Only then as any program connects, by what the process was started with: a
/// server that names no authority file is reached that way.
/// Never scan authority filenames, change the environment, or change the server's access policy.
pub(super) fn connect() -> io::Result<(RustConnection, usize)> {
    let told = parse_display::parse_display(None)
        .ok()
        .map(|display| (display.display, display.screen));
    candidates(told, Path::new("/tmp/.X11-unix"))
        .into_iter()
        .find_map(|(display, screen)| connect_to(display, screen).ok())
        .map_or_else(|| x11rb::connect(None).map_err(|_| unavailable()), Ok)
}

fn connect_to(number: u16, screen: u16) -> io::Result<(RustConnection, usize)> {
    let display = parse_display::parse_display(Some(&format!(":{number}.{screen}")))
        .map_err(|_| unavailable())?;
    let screen = usize::from(display.screen);
    let uid = rustix::process::getuid().as_raw();
    for address in display.connect_instruction() {
        let Some((stream, path)) = own_server(&address, uid, display.display) else {
            continue;
        };
        let bytes = read_authority(&path, uid)?;
        let auth = cookie(&bytes, display.display)?;
        let connection = RustConnection::connect_to_stream_with_auth_info(
            stream,
            screen,
            PROTOCOL.to_vec(),
            auth,
        )
        .map_err(|_| unavailable())?;
        return Ok((connection, screen));
    }
    Err(unavailable())
}

/// A connection to this user's own local server for `display` at `address`, with the authority file that server (or its
/// Xwayland child) was started with. `None` for anything else: not a local socket, nobody there, another user's server.
fn own_server(
    address: &ConnectAddress,
    uid: u32,
    display: u16,
) -> Option<(DefaultStream, PathBuf)> {
    if !matches!(address, ConnectAddress::Socket(_)) {
        return None;
    }
    let (stream, _) = DefaultStream::connect(address).ok()?;
    let peer = rustix::net::sockopt::socket_peercred(&stream).ok()?;
    // Another user's server is never asked for anything.
    if peer.uid.as_raw() != uid {
        return None;
    }
    let path = authority_path(
        Path::new("/proc"),
        peer.pid.as_raw_nonzero().get() as u32,
        uid,
        display,
    )
    .ok()?;
    Some((stream, path))
}

/// The authority file of this user's own local X server for display `number`, for a program this process starts on that
/// display (the panel of an offer) when it inherited none: a Wayland session can leave XAUTHORITY out, and its Xwayland
/// refuses a client that brings no cookie. The same file the server names, checked as [`connect`] checks it; nothing is
/// read out of it here and nothing in this process's environment changes.
pub(crate) fn authority_file(number: u16) -> Option<PathBuf> {
    let display = parse_display::parse_display(Some(&format!(":{number}"))).ok()?;
    let uid = rustix::process::getuid().as_raw();
    display.connect_instruction().find_map(|address| {
        let (_, path) = own_server(&address, uid, display.display)?;
        read_authority(&path, uid).ok()?;
        Some(path)
    })
}

fn authority_path(proc: &Path, peer: u32, uid: u32, display: u16) -> io::Result<PathBuf> {
    fn server(proc: &Path, pid: u32, uid: u32, display: u16) -> Option<PathBuf> {
        let process = proc.join(pid.to_string());
        if fs::metadata(&process).ok()?.uid() != uid {
            return None;
        }
        let mut name = String::new();
        fs::File::open(process.join("comm"))
            .ok()?
            .take(64)
            .read_to_string(&mut name)
            .ok()?;
        if name.trim() != "Xwayland" {
            return None;
        }
        let mut command = Vec::new();
        fs::File::open(process.join("cmdline"))
            .ok()?
            .take(16 * 1024)
            .read_to_end(&mut command)
            .ok()?;
        let args: Vec<_> = command.split(|b| *b == 0).collect();
        let expected = format!(":{display}");
        if !args.contains(&expected.as_bytes()) {
            return None;
        }
        let path = args.windows(2).find(|pair| pair[0] == b"-auth")?[1];
        use std::os::unix::ffi::OsStrExt;
        let path = PathBuf::from(std::ffi::OsStr::from_bytes(path));
        path.is_absolute().then_some(path)
    }
    if fs::metadata(proc.join(peer.to_string()))?.uid() != uid {
        return Err(unavailable());
    }
    if let Some(path) = server(proc, peer, uid, display) {
        return Ok(path);
    }
    let mut children = String::new();
    fs::File::open(proc.join(format!("{peer}/task/{peer}/children")))?
        .take(4096)
        .read_to_string(&mut children)?;
    children
        .split_whitespace()
        .take(64)
        .filter_map(|pid| pid.parse().ok())
        .find_map(|pid| server(proc, pid, uid, display))
        .ok_or_else(unavailable)
}

fn read_authority(path: &Path, uid: u32) -> io::Result<Zeroizing<Vec<u8>>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(
            rustix::fs::OFlags::NOFOLLOW.bits() as i32 | rustix::fs::OFlags::NONBLOCK.bits() as i32,
        )
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.mode() & 0o077 != 0
        || metadata.len() > MAX_AUTHORITY as u64
    {
        return Err(unavailable());
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(MAX_AUTHORITY as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_AUTHORITY {
        return Err(unavailable());
    }
    Ok(bytes)
}

fn cookie(mut bytes: &[u8], display: u16) -> io::Result<Vec<u8>> {
    fn field<'a>(bytes: &mut &'a [u8]) -> io::Result<&'a [u8]> {
        let (size, rest) = bytes.split_first_chunk::<2>().ok_or_else(unavailable)?;
        let length = usize::from(u16::from_be_bytes(*size));
        let (value, rest) = rest.split_at_checked(length).ok_or_else(unavailable)?;
        *bytes = rest;
        Ok(value)
    }
    let expected = display.to_string();
    let mut generic = None;
    while !bytes.is_empty() {
        let (family, rest) = bytes.split_first_chunk::<2>().ok_or_else(unavailable)?;
        let local = matches!(u16::from_be_bytes(*family), 256 | 65535);
        bytes = rest;
        let _address = field(&mut bytes)?;
        let number = field(&mut bytes)?;
        let name = field(&mut bytes)?;
        let data = field(&mut bytes)?;
        if local && name == PROTOCOL && !data.is_empty() {
            if number == expected.as_bytes() {
                return Ok(data.to_vec());
            }
            if number.is_empty() && generic.is_none() {
                generic = Some(data);
            }
        }
    }
    generic.map(<[u8]>::to_vec).ok_or_else(unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::Path};

    fn server(proc: &Path, pid: u32, args: &[&str]) {
        let root = proc.join(pid.to_string());
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("comm"), "Xwayland\n").unwrap();
        fs::write(root.join("cmdline"), args.join("\0") + "\0").unwrap();
    }

    #[test]
    fn only_the_matching_display_server_child_supplies_its_authority_path() {
        let root = tempfile::tempdir().unwrap();
        let proc = root.path();
        fs::create_dir_all(proc.join("10/task/10")).unwrap();
        fs::write(proc.join("10/comm"), "gnome-shell\n").unwrap();
        fs::write(proc.join("10/task/10/children"), "11 12").unwrap();
        server(proc, 11, &["Xwayland", ":7", "-auth", "/session/wrong"]);
        server(proc, 12, &["Xwayland", ":8", "-auth", "/session/current"]);
        let uid = rustix::process::getuid().as_raw();
        assert_eq!(
            authority_path(proc, 10, uid, 8).unwrap(),
            Path::new("/session/current")
        );
        assert!(authority_path(proc, 10, uid, 9).is_err());
        assert!(authority_path(proc, 10, uid.wrapping_add(1), 8).is_err());
        fs::write(proc.join("12/comm"), "unrelated\n").unwrap();
        assert!(authority_path(proc, 10, uid, 8).is_err());
    }

    fn record(family: u16, display: &[u8], name: &[u8], data: &[u8]) -> Vec<u8> {
        let mut bytes = family.to_be_bytes().to_vec();
        for field in [b"local".as_slice(), display, name, data] {
            bytes.extend_from_slice(&(field.len() as u16).to_be_bytes());
            bytes.extend_from_slice(field);
        }
        bytes
    }

    #[test]
    fn authentication_selects_only_the_local_matching_display_and_protocol() {
        let mut bytes = record(256, b"7", b"MIT-MAGIC-COOKIE-1", b"wrong-display");
        bytes.extend(record(0, b"8", b"MIT-MAGIC-COOKIE-1", b"not-local"));
        bytes.extend(record(256, b"8", b"other-protocol", b"wrong-protocol"));
        bytes.extend(record(
            256,
            b"8",
            b"MIT-MAGIC-COOKIE-1",
            b"synthetic-test-cookie",
        ));
        assert_eq!(cookie(&bytes, 8).unwrap(), b"synthetic-test-cookie");
        assert!(cookie(&bytes, 9).is_err());
        for length in 0..bytes.len() {
            assert!(cookie(&bytes[..length], 8).is_err());
        }
    }

    #[test]
    fn a_server_owned_authority_can_use_a_display_independent_record() {
        for family in [256, 65535] {
            let bytes = record(family, b"", PROTOCOL, b"synthetic-session-cookie");
            assert_eq!(cookie(&bytes, 8).unwrap(), b"synthetic-session-cookie");
        }
        assert!(cookie(&record(0, b"", PROTOCOL, b"not-local"), 8).is_err());
    }

    #[test]
    fn the_display_this_process_was_told_of_is_tried_first_and_then_every_other_local_one() {
        let sockets = tempfile::tempdir().unwrap();
        for name in ["X2", "X0", "X97", "X11-lock", "notes", "X"] {
            fs::write(sockets.path().join(name), "").unwrap();
        }
        // Told of one that has no socket here (it is gone): still first, and the rest follow in order.
        assert_eq!(
            candidates(Some((5, 1)), sockets.path()),
            [(5, 1), (0, 0), (2, 0), (97, 0)]
        );
        // Told of one that is here: once, with the screen it was told, not twice.
        assert_eq!(
            candidates(Some((97, 0)), sockets.path()),
            [(97, 0), (0, 0), (2, 0)]
        );
        // Told of none: every local one.
        assert_eq!(candidates(None, sockets.path()), [(0, 0), (2, 0), (97, 0)]);
        // No directory of sockets at all: only what was told.
        assert_eq!(
            candidates(Some((3, 0)), &sockets.path().join("absent")),
            [(3, 0)]
        );
    }

    #[test]
    fn a_wayland_session_is_one_whose_compositor_listens_not_one_that_left_a_socket_behind() {
        let runtime = tempfile::tempdir().unwrap();
        let uid = rustix::process::getuid().as_raw();
        assert!(!wayland_in(runtime.path(), uid), "an empty directory");
        // What a compositor that ended leaves: its lock, and a socket nobody listens on.
        fs::write(runtime.path().join("wayland-0.lock"), "").unwrap();
        drop(std::os::unix::net::UnixListener::bind(runtime.path().join("wayland-0")).unwrap());
        // Sockets of other things, and a file with the name of a display.
        let _bus = std::os::unix::net::UnixListener::bind(runtime.path().join("bus")).unwrap();
        fs::write(runtime.path().join("wayland-7"), "").unwrap();
        assert!(
            !wayland_in(runtime.path(), uid),
            "nothing here listens as a compositor"
        );
        let _compositor =
            std::os::unix::net::UnixListener::bind(runtime.path().join("wayland-1")).unwrap();
        assert!(wayland_in(runtime.path(), uid));
        // Another user's compositor is not this person's session.
        assert!(!wayland_in(runtime.path(), uid.wrapping_add(1)));
        assert!(!wayland_in(&runtime.path().join("absent"), uid));
    }

    #[test]
    fn exact_display_authentication_takes_precedence_over_a_generic_record() {
        let mut bytes = record(256, b"", PROTOCOL, b"generic");
        bytes.extend(record(256, b"8", PROTOCOL, b"exact"));
        assert_eq!(cookie(&bytes, 8).unwrap(), b"exact");
    }

    #[test]
    fn authority_file_must_be_private_regular_owned_and_not_a_symlink() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("authority");
        fs::write(
            &path,
            record(256, b"8", b"MIT-MAGIC-COOKIE-1", b"synthetic"),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let uid = rustix::process::getuid().as_raw();
        assert!(read_authority(&path, uid).is_ok());
        assert!(read_authority(&path, uid.wrapping_add(1)).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_authority(&path, uid).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let link = root.path().join("link");
        symlink(&path, &link).unwrap();
        assert!(read_authority(&link, uid).is_err());
        fs::write(&path, vec![0; MAX_AUTHORITY + 1]).unwrap();
        assert!(read_authority(&path, uid).is_err());
    }
}
