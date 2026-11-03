//! Desktop connects private paths; Android supplies owner-connected descriptors over Binder.
use crate::{DataRequest, Transport};
use peersus_plugin_host::{data::DataHello, frame::{self, CONTROL_LIMIT}, link::Hello};
use std::{io, os::unix::net::UnixStream, path::{Path, PathBuf}, time::Duration};

/// The bootstrap token is private to the worker and never enters guest memory.
pub struct SocketTransport {
    control: UnixStream,
    open_data: Box<dyn FnMut() -> io::Result<UnixStream> + Send>,
    token: String,
}

impl SocketTransport {
    pub fn connect(control_socket: &Path, data_socket: PathBuf, token: String) -> io::Result<Self> {
        let control = UnixStream::connect(control_socket)?;
        control.set_write_timeout(Some(Duration::from_secs(5)))?;
        Self::from_connected(control, token, move || {
            let stream = UnixStream::connect(&data_socket)?;
            stream.set_read_timeout(Some(Duration::from_secs(30)))?;
            stream.set_write_timeout(Some(Duration::from_secs(30)))?;
            Ok(stream)
        })
    }

    /// Uses already-blocking connected descriptors without opening paths in the isolated worker.
    /// The owner sets blocking mode and I/O timeouts before transfer. An isolated worker
    /// can read/write but cannot mutate the owner's socket options under Android SELinux.
    pub fn from_connected(
        mut control: UnixStream,
        token: String,
        open_data: impl FnMut() -> io::Result<UnixStream> + Send + 'static,
    ) -> io::Result<Self> {
        frame::write_blocking(&mut control, &Hello { token: token.clone() }, CONTROL_LIMIT)?;
        Ok(Self { control, open_data: Box::new(open_data), token })
    }
}

impl Transport for SocketTransport {
    type Stream = UnixStream;

    fn receive(&mut self, into: &mut [u8]) -> io::Result<usize> {
        frame::read_blocking_into(&mut self.control, into, CONTROL_LIMIT)
    }

    fn send(&mut self, message: &[u8]) -> io::Result<()> {
        frame::write_blocking_bytes(&mut self.control, message, CONTROL_LIMIT)
    }

    fn open_data(&mut self, request: DataRequest) -> io::Result<Self::Stream> {
        let mut stream = (self.open_data)()?;
        let (ticket, claim) = match request {
            DataRequest::Ticket(ticket) => (ticket, None),
            DataRequest::Claim(claim) => (0, Some(claim)),
        };
        frame::write_blocking(&mut stream, &DataHello { token: self.token.clone(), ticket, claim }, 1024)?;
        Ok(stream)
    }

    fn shutdown_data(&mut self, stream: &mut Self::Stream) -> io::Result<()> {
        stream.shutdown(std::net::Shutdown::Write)
    }
}
