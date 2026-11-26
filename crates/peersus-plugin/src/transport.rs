//! The local connection between the application and one plugin process, per platform.
//!
//! Every connection must present the plugin's bootstrap token before anything else is read
//! (`link::Hello`, `data::DataHello`), so the transport only has to carry bytes, support a
//! half-close (a sender finishes its bytes and still reads the reply) and keep strangers out
//! as far as the platform allows.
//!
//! * Unix: a socket file in the plugin's private directory (0700), and every accepted peer is
//!   checked to run as this user.
//! * Elsewhere (Windows): a TCP listener on the loopback address at a port the system picks.
//!   Named pipes would restrict who can connect, but they have no half-close, which every
//!   data exchange depends on. Loopback accepts only local peers; a local peer that is not the
//!   plugin cannot present the 256-bit token, which only reaches the plugin through its own
//!   environment, and is dropped. The endpoint the plugin receives is the `address:port` text.
use std::{io, path::Path, path::PathBuf};

/// The application's end of one accepted connection.
#[cfg(unix)]
pub type Stream = tokio::net::UnixStream;
#[cfg(not(unix))]
pub type Stream = tokio::net::TcpStream;

/// A plugin's blocking end of a connection, for executors without an async runtime.
#[cfg(unix)]
pub type BlockingStream = std::os::unix::net::UnixStream;
#[cfg(not(unix))]
pub type BlockingStream = std::net::TcpStream;

/// Accepts the plugin's connections at one endpoint until dropped.
pub struct Listener {
    #[cfg(unix)]
    inner: tokio::net::UnixListener,
    #[cfg(not(unix))]
    inner: tokio::net::TcpListener,
    endpoint: PathBuf,
}

impl Listener {
    /// Listen for connections. `path` names the socket file on Unix, inside a directory only
    /// this user may enter; a stale file of that name is replaced. Elsewhere it is unused and
    /// the endpoint is the loopback address the system assigned.
    pub fn bind(path: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        {
            // The directory is the caller's, but it may have been emptied or removed since the
            // listener last ran; bind needs it to exist, so a missing parent is restored here
            // too. A stale socket file of the same name is replaced.
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::remove_file(path);
            Ok(Self {
                inner: tokio::net::UnixListener::bind(path)?,
                endpoint: path.to_owned(),
            })
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            let std = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
            std.set_nonblocking(true)?;
            let endpoint = PathBuf::from(std.local_addr()?.to_string());
            Ok(Self {
                inner: tokio::net::TcpListener::from_std(std)?,
                endpoint,
            })
        }
    }

    /// What the plugin is told to connect to.
    pub fn endpoint(&self) -> &Path {
        &self.endpoint
    }

    /// The next connection from a peer the platform lets through. Peers that are not this
    /// user (Unix) or not on this machine are closed and the next one is awaited.
    pub async fn accept(&self) -> io::Result<Stream> {
        loop {
            let (stream, _peer) = self.inner.accept().await?;
            #[cfg(unix)]
            if !stream
                .peer_cred()
                .is_ok_and(|cred| cred.uid() == rustix::process::geteuid().as_raw())
            {
                continue;
            }
            #[cfg(not(unix))]
            {
                if !_peer.ip().is_loopback() {
                    continue;
                }
                // Control frames are small and latency-bound.
                let _ = stream.set_nodelay(true);
            }
            return Ok(stream);
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        #[cfg(unix)]
        let _ = std::fs::remove_file(&self.endpoint);
    }
}

/// Connect to an endpoint given by the application.
pub async fn connect(endpoint: impl AsRef<Path>) -> io::Result<Stream> {
    let endpoint = endpoint.as_ref();
    #[cfg(unix)]
    {
        tokio::net::UnixStream::connect(endpoint).await
    }
    #[cfg(not(unix))]
    {
        let stream = tokio::net::TcpStream::connect(loopback(endpoint)?).await?;
        let _ = stream.set_nodelay(true);
        Ok(stream)
    }
}

/// Blocking form of [`connect`].
pub fn connect_blocking(endpoint: impl AsRef<Path>) -> io::Result<BlockingStream> {
    let endpoint = endpoint.as_ref();
    #[cfg(unix)]
    {
        std::os::unix::net::UnixStream::connect(endpoint)
    }
    #[cfg(not(unix))]
    {
        let stream = std::net::TcpStream::connect(loopback(endpoint)?)?;
        let _ = stream.set_nodelay(true);
        Ok(stream)
    }
}

pub(crate) fn local_endpoint(endpoint: &Path) -> bool {
    #[cfg(unix)]
    {
        endpoint.is_absolute()
    }
    #[cfg(not(unix))]
    {
        loopback(endpoint).is_ok()
    }
}

/// Only a loopback address is ever a plugin endpoint; anything else is refused rather than
/// connected to.
#[cfg(not(unix))]
fn loopback(endpoint: &Path) -> io::Result<std::net::SocketAddr> {
    endpoint
        .to_str()
        .and_then(|text| text.parse::<std::net::SocketAddr>().ok())
        .filter(|address| address.ip().is_loopback())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a plugin endpoint"))
}

/// Two connected ends over the platform's real transport, the first as the application
/// accepts it and the second as a plugin connects. For tests of code that takes a [`Stream`];
/// `directory` holds the Unix socket file only while connecting.
pub async fn connected_pair(directory: &Path) -> io::Result<(Stream, Stream)> {
    let listener = Listener::bind(&directory.join("pair.sock"))?;
    let (accepted, connected) = tokio::join!(listener.accept(), connect(listener.endpoint()));
    Ok((accepted?, connected?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn a_sender_can_finish_its_bytes_and_still_read_the_reply() {
        let directory = tempfile::tempdir().unwrap();
        let listener = Listener::bind(&directory.path().join("t.sock")).unwrap();
        let endpoint = listener.endpoint().to_owned();
        let server = tokio::spawn(async move {
            let mut stream = listener.accept().await.unwrap();
            let mut received = Vec::new();
            stream.read_to_end(&mut received).await.unwrap();
            stream
                .write_all(&(received.len() as u32).to_be_bytes())
                .await
                .unwrap();
        });
        let mut client = connect(&endpoint).await.unwrap();
        client.write_all(b"payload").await.unwrap();
        client.shutdown().await.unwrap();
        let mut reply = [0u8; 4];
        client.read_exact(&mut reply).await.unwrap();
        assert_eq!(u32::from_be_bytes(reply), 7);
        server.await.unwrap();
    }

    #[cfg(not(unix))]
    #[test]
    fn only_loopback_endpoints_are_connected_to() {
        assert!(loopback(Path::new("127.0.0.1:4000")).is_ok());
        assert!(loopback(Path::new("10.0.0.1:4000")).is_err());
        assert!(loopback(Path::new("C:\\not\\an\\address")).is_err());
    }
}
