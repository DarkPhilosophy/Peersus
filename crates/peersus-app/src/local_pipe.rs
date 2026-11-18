//! The owner's control channel on Windows: a named pipe, in place of the Unix socket.
//!
//! WHO CAN REACH IT. A pipe's name is global to the machine, so a fixed one could be made first by
//! another user, and this user's windows would talk to them. The name is therefore a random one,
//! chosen by the owner when it starts and written in the control file inside the application's own
//! folder, which only this user can read (it sits in the user's profile and inherits its ACL); a
//! client learns the name only from that file, so nobody can make it in advance. The owner makes the
//! pipe as its FIRST instance, so a pipe of that name made by anyone else is a failure to start and
//! never a shared channel, and remote clients are refused outright. Once it exists the name can be
//! listed by anyone, and the pipe's default security is what holds: full control for this user (and
//! the system and administrators, who own the machine anyway), and READ only for everyone else. A
//! request has to be written, so nobody else can send one, and making another instance of the pipe
//! needs a right only its owner has.
use std::{io, path::Path, time::Duration};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};

const PREFIX: &str = r"\\.\pipe\peersus-";
/// `ERROR_PIPE_BUSY`: every instance is connected at this moment; another is made right after.
const PIPE_BUSY: i32 = 231;

fn new_instance(name: &str, first: bool) -> io::Result<NamedPipeServer> {
    ServerOptions::new()
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .create(name)
}

/// The listening end, always with one unconnected instance waiting for the next client.
pub struct Listener {
    name: String,
    waiting: NamedPipeServer,
}

impl Listener {
    /// Make a pipe with a new random name and write the name to `file`, replacing a stale one left by
    /// an owner that is gone (the single-instance lock is held, so no live owner uses it).
    pub fn bind(file: &Path) -> io::Result<Self> {
        let name = format!("{PREFIX}{}", hex::encode(rand::random::<[u8; 16]>()));
        let waiting = new_instance(&name, true)?;
        std::fs::write(file, name.as_bytes())?;
        Ok(Self { name, waiting })
    }
    /// The next client. The same shape as a Unix listener's `accept`, so the server is written once.
    pub async fn accept(&mut self) -> io::Result<(NamedPipeServer, ())> {
        self.waiting.connect().await?;
        let next = new_instance(&self.name, false)?;
        Ok((std::mem::replace(&mut self.waiting, next), ()))
    }
}

/// Connect to the owner whose pipe name is in `file`. No file, a file that does not hold a name of
/// this application, or no pipe of that name is "nobody is there" (`ConnectionRefused`, as a Unix
/// socket nobody listens on), so a window starts an owner instead of failing.
pub async fn connect(file: &Path) -> io::Result<NamedPipeClient> {
    let refused = || io::Error::from(io::ErrorKind::ConnectionRefused);
    let name = match std::fs::read_to_string(file) {
        Ok(name) => name,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(refused()),
        Err(error) => return Err(error),
    };
    let valid = name
        .strip_prefix(PREFIX)
        .is_some_and(|rest| rest.len() == 32 && rest.bytes().all(|b| b.is_ascii_hexdigit()));
    if !valid {
        return Err(refused());
    }
    loop {
        match ClientOptions::new().open(&name) {
            Ok(client) => return Ok(client),
            Err(error) if error.raw_os_error() == Some(PIPE_BUSY) => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(refused()),
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn a_client_reaches_the_owner_only_through_the_name_in_the_file() {
        let folder = tempfile::tempdir().unwrap();
        let file = folder.path().join("control.sock");
        assert_eq!(
            connect(&file).await.unwrap_err().kind(),
            io::ErrorKind::ConnectionRefused,
            "no owner must read as nobody there"
        );
        let mut listener = Listener::bind(&file).unwrap();
        let served = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut stream, ()) = listener.accept().await.unwrap();
                let mut byte = [0_u8; 1];
                stream.read_exact(&mut byte).await.unwrap();
                stream.write_all(&[byte[0] + 1]).await.unwrap();
            }
        });
        // Two clients one after the other: each finds an instance waiting.
        for sent in [1_u8, 7] {
            let mut client = connect(&file).await.unwrap();
            client.write_all(&[sent]).await.unwrap();
            let mut back = [0_u8; 1];
            client.read_exact(&mut back).await.unwrap();
            assert_eq!(back[0], sent + 1);
        }
        served.await.unwrap();
    }

    #[test]
    fn a_name_taken_first_by_someone_else_is_never_shared() {
        let folder = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _guard = runtime.enter();
        let first = Listener::bind(&folder.path().join("a")).unwrap();
        assert!(
            new_instance(&first.name, true).is_err(),
            "a second first instance of a taken name was made"
        );
    }

    #[tokio::test]
    async fn a_file_that_names_something_else_is_nobody_there() {
        let folder = tempfile::tempdir().unwrap();
        let file = folder.path().join("control.sock");
        std::fs::write(&file, r"\\.\pipe\someone-else").unwrap();
        assert_eq!(
            connect(&file).await.unwrap_err().kind(),
            io::ErrorKind::ConnectionRefused
        );
    }
}
