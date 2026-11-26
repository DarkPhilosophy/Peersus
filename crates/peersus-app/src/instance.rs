//! Private same-user control directory and an exclusive application owner.
use fs2::FileExt;
use std::{
    fs::{File, OpenOptions},
    io,
    path::{Path, PathBuf},
};

/// Where the application keeps everything of its own: `$XDG_DATA_HOME/peersus`, or
/// `~/.local/share/peersus` when that is unset or empty. The program and every window of it
/// ask here, so that they cannot disagree about where the one owner's control socket is.
///
/// The rule is a separate function over plain values because the environment is shared by
/// every test of the process and cannot be changed safely from one of them.
pub fn data_directory_from(
    xdg_data_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    xdg_data_home
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.map(|home| PathBuf::from(home).join(".local/share")))
        .map(|base| base.join("peersus"))
}

/// [`data_directory_from`] for this process's environment. Windows keeps it in the user's local
/// application data (`%LOCALAPPDATA%\peersus`), which only the user can read.
pub fn data_directory() -> Option<PathBuf> {
    #[cfg(windows)]
    return std::env::var_os("LOCALAPPDATA")
        .filter(|value| !value.is_empty())
        .map(|base| PathBuf::from(base).join("peersus"));
    #[cfg(not(windows))]
    data_directory_from(std::env::var_os("XDG_DATA_HOME"), std::env::var_os("HOME"))
}

pub struct Instance {
    _lock: File,
    socket: PathBuf,
    bound: bool,
    #[cfg(unix)]
    socket_identity: Option<(u64, u64)>,
}
impl Instance {
    pub fn acquire(directory: &Path) -> io::Result<Option<Self>> {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(directory)?;
        let metadata = std::fs::symlink_metadata(directory)?;
        if !metadata.is_dir() {
            return Err(io::Error::other("Application directory is not a directory"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o077 != 0
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Application directory must be private to the current user",
                ));
            }
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
        }
        let lock = options.open(directory.join("instance.lock"))?;
        match lock.try_lock_exclusive() {
            Ok(()) => Ok(Some(Self {
                _lock: lock,
                socket: directory.join("control.sock"),
                bound: false,
                #[cfg(unix)]
                socket_identity: None,
            })),
            // Held by the running owner: `EWOULDBLOCK` on Unix, `ERROR_LOCK_VIOLATION` on Windows,
            // which `fs2` names as its contended error.
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    || error.raw_os_error() == fs2::lock_contended_error().raw_os_error() =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }
    #[cfg(unix)]
    pub fn listen(&mut self) -> io::Result<tokio::net::UnixListener> {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        match std::fs::symlink_metadata(&self.socket) {
            Ok(metadata) => {
                if !metadata.file_type().is_socket()
                    || metadata.uid() != rustix::process::geteuid().as_raw()
                {
                    return Err(io::Error::other(
                        "Existing control path is not an owned socket",
                    ));
                }
                match std::os::unix::net::UnixStream::connect(&self.socket) {
                    Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
                        std::fs::remove_file(&self.socket)?
                    }
                    _ => {
                        return Err(io::Error::other(
                            "Existing control socket may still be active",
                        ));
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let listener = tokio::net::UnixListener::bind(&self.socket)?;
        self.bound = true;
        let metadata = std::fs::symlink_metadata(&self.socket)?;
        self.socket_identity = Some((metadata.dev(), metadata.ino()));
        Ok(listener)
    }
    /// The control pipe, its name written to the control file. The single-instance lock is held,
    /// so a file already there was left by an owner that is gone, and is replaced.
    #[cfg(windows)]
    pub fn listen(&mut self) -> io::Result<crate::local_pipe::Listener> {
        let listener = crate::local_pipe::Listener::bind(&self.socket)?;
        self.bound = true;
        Ok(listener)
    }
}
impl Drop for Instance {
    fn drop(&mut self) {
        if self.bound {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if std::fs::symlink_metadata(&self.socket).is_ok_and(|metadata| {
                    self.socket_identity == Some((metadata.dev(), metadata.ino()))
                }) {
                    let _ = std::fs::remove_file(&self.socket);
                }
            }
            // The file names this owner's pipe, which goes with it.
            #[cfg(windows)]
            let _ = std::fs::remove_file(&self.socket);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn only_owner_binds_and_only_its_socket_is_removed() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("state");
        let mut first = Instance::acquire(&root).unwrap().unwrap();
        assert!(Instance::acquire(&root).unwrap().is_none());
        let stale = std::os::unix::net::UnixListener::bind(root.join("control.sock")).unwrap();
        drop(stale);
        let listener = first.listen().unwrap();
        assert!(root.join("control.sock").exists());
        drop(listener);
        drop(first);
        assert!(!root.join("control.sock").exists());
        let second = Instance::acquire(&root).unwrap().unwrap();
        std::fs::write(root.join("control.sock"), b"not our socket").unwrap();
        drop(second);
        assert_eq!(
            std::fs::read(root.join("control.sock")).unwrap(),
            b"not our socket"
        );
    }

    fn os(text: &str) -> Option<std::ffi::OsString> {
        Some(text.into())
    }

    #[test]
    fn the_state_lives_under_xdg_data_home_when_it_is_set() {
        assert_eq!(
            data_directory_from(os("/data"), os("/home/me")),
            Some(PathBuf::from("/data/peersus"))
        );
    }

    #[test]
    fn without_xdg_data_home_it_lives_under_the_home_directory() {
        assert_eq!(
            data_directory_from(None, os("/home/me")),
            Some(PathBuf::from("/home/me/.local/share/peersus"))
        );
    }

    #[test]
    fn an_empty_xdg_data_home_is_the_same_as_none() {
        // An exported but empty variable means "unset" in the specification. Taking it as
        // a path would put the state in `peersus` under the current directory, wherever the
        // program happened to be started.
        assert_eq!(
            data_directory_from(os(""), os("/home/me")),
            Some(PathBuf::from("/home/me/.local/share/peersus"))
        );
    }

    #[test]
    fn with_neither_there_is_no_directory_rather_than_a_guess() {
        assert_eq!(data_directory_from(None, None), None);
        assert_eq!(data_directory_from(os(""), None), None);
    }
}
