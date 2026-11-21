//! Desktop connects to the application's endpoints; Android supplies owner-connected
//! descriptors over Binder.
use crate::{DataRequest, Transport};
use peersus_plugin::{
    data::{DataHello, canvas},
    frame::{self, CONTROL_LIMIT},
    link::Hello,
    transport::{BlockingStream as Stream, connect_blocking},
};
use std::{
    io,
    path::{Path, PathBuf},
    time::Duration,
};

/// The bootstrap token is private to the worker and never enters guest memory.
pub struct SocketTransport {
    control: Stream,
    open_data: Box<dyn FnMut(DataRequest) -> io::Result<Stream> + Send>,
    token: String,
    next_ready: usize,
    desktop: bool,
}

impl SocketTransport {
    pub fn connect(control_socket: &Path, data_socket: PathBuf, token: String) -> io::Result<Self> {
        let control = connect_blocking(control_socket)?;
        control.set_write_timeout(Some(Duration::from_secs(5)))?;
        let mut transport = Self::from_connected(control, token, move |request| {
            let stream = connect_blocking(&data_socket)?;
            let wait = match request {
                DataRequest::CanvasClaim(_) => canvas::ROUTE_WAIT,
                _ => Duration::from_secs(30),
            };
            stream.set_read_timeout(Some(wait))?;
            stream.set_write_timeout(Some(wait))?;
            Ok(stream)
        })?;
        transport.desktop = true;
        Ok(transport)
    }

    /// Uses already-blocking connected descriptors without opening paths in the isolated worker.
    /// The owner sets blocking mode and I/O timeouts before transfer. An isolated worker
    /// can read/write but cannot mutate the owner's socket options under Android SELinux.
    /// The opener receives the data request so it can select the Canvas-specific write deadline.
    pub fn from_connected(
        mut control: Stream,
        token: String,
        open_data: impl FnMut(DataRequest) -> io::Result<Stream> + Send + 'static,
    ) -> io::Result<Self> {
        frame::write_blocking(
            &mut control,
            &Hello {
                token: token.clone(),
            },
            CONTROL_LIMIT,
        )?;
        Ok(Self {
            control,
            open_data: Box::new(open_data),
            token,
            next_ready: 0,
            desktop: false,
        })
    }
}

impl Transport for SocketTransport {
    type Stream = Stream;

    fn receive(&mut self, into: &mut [u8]) -> io::Result<usize> {
        frame::read_blocking_into(&mut self.control, into, CONTROL_LIMIT)
    }

    fn send(&mut self, message: &[u8]) -> io::Result<()> {
        frame::write_blocking_bytes(&mut self.control, message, CONTROL_LIMIT)
    }

    fn open_data(&mut self, request: DataRequest) -> io::Result<Self::Stream> {
        let (kind, ticket, claim) = match request {
            DataRequest::Ticket(ticket) => ("ticket", ticket, None),
            DataRequest::Claim(claim) => ("claim", 0, Some(claim)),
            DataRequest::CanvasClaim(claim) => ("canvas", 0, Some(claim)),
        };
        let started = std::time::Instant::now();
        let result = (|| {
            let mut stream = (self.open_data)(request)?;
            frame::write_blocking(
                &mut stream,
                &DataHello {
                    token: self.token.clone(),
                    ticket,
                    claim,
                },
                1024,
            )?;
            if self.desktop && matches!(request, DataRequest::CanvasClaim(_)) {
                stream = canvas::Redirect::follow(stream)?;
                stream.set_write_timeout(Some(crate::CANVAS_STREAM_IO_WAIT))?;
            }
            Ok(stream)
        })();
        let outcome = if result.is_ok() { "ok" } else { "failed" };
        peersus_diagnostics::dev_trace!(
            kind,
            number = ticket,
            stage = "stream-open",
            outcome,
            direction = "out",
            elapsed_ms = started.elapsed().as_millis() as u64,
            "Plugin data stream open finished"
        );
        result
    }

    fn shutdown_data(&mut self, stream: &mut Self::Stream) -> io::Result<()> {
        stream.shutdown(std::net::Shutdown::Write)
    }

    fn poll(
        &mut self,
        streams: &[(i32, &Self::Stream)],
        timeout_ms: i32,
    ) -> io::Result<Option<i32>> {
        use rustix::event::{PollFd, PollFlags, Timespec, poll};
        if !(-1..=30_000).contains(&timeout_ms) || streams.len() > crate::MAX_OPEN_STREAMS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid readiness request",
            ));
        }
        let mut fds = Vec::with_capacity(streams.len() + 1);
        fds.push(PollFd::new(&self.control, PollFlags::IN));
        fds.extend(
            streams
                .iter()
                .map(|(_, stream)| PollFd::new(*stream, PollFlags::IN)),
        );
        // Range-checked above, so the conversion cannot fail.
        let timeout = u64::try_from(timeout_ms)
            .ok()
            .map(|ms| Timespec::try_from(Duration::from_millis(ms)))
            .transpose()
            .map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "invalid readiness request")
            })?;
        // Interrupted waits report the interruption rather than silently extending a deadline.
        if poll(&mut fds, timeout.as_ref())? == 0 {
            return Ok(None);
        }
        for offset in 0..fds.len() {
            let index = (self.next_ready + offset) % fds.len();
            let events = fds[index].revents();
            if events.intersects(PollFlags::NVAL) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid readiness descriptor",
                ));
            }
            if events.intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR) {
                self.next_ready = (index + 1) % fds.len();
                return Ok(Some(if index == 0 { 0 } else { streams[index - 1].0 }));
            }
        }
        Err(io::Error::other(
            "readiness returned no readable descriptor",
        ))
    }
}
