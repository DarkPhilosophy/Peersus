//! The only door a guest has to the application: `peersus_v1`. Control messages are JSON, data streams are raw bytes.
//! Both are traits so every rule in the guest is tested against a script, with no device and no host.
use serde_json::Value;

/// Largest control message in either direction: the broker admits 48 KiB serialized.
#[cfg(target_family = "wasm")]
pub const LIMIT: usize = 48 * 1024;
/// A data handle is a small number the host issued; a negative one is an error.
pub type Handle = i32;
/// What the host acknowledges a delivery with: a length-prefixed JSON byte count of at most this many bytes.
pub const ACK_LIMIT: usize = 1024;

pub trait Wire {
    fn receive(&mut self) -> Result<Value, &'static str>;
    fn send(&mut self, value: Value) -> Result<(), &'static str>;
}

/// Bulk bytes. `kind` 0 names a hub ticket, 1 an application-issued claim; the number is never an identity.
pub trait Data {
    fn open(&mut self, claim: bool, number: u64) -> Result<Handle, &'static str>;
    fn read(&mut self, handle: Handle, into: &mut [u8]) -> Result<usize, &'static str>;
    fn write(&mut self, handle: Handle, from: &[u8]) -> Result<usize, &'static str>;
    fn shutdown(&mut self, handle: Handle) -> Result<(), &'static str>;
    fn close(&mut self, handle: Handle);
    /// Wait for control (0) or one selected readable stream; no bytes are consumed.
    fn poll(&mut self, handles: &[Handle], timeout_ms: i32) -> Result<Option<Handle>, &'static str>;
}

/// Write all of `bytes`, however the host splits it: a stream operation need not take the requested length.
pub fn write_all<D: Data>(
    data: &mut D,
    handle: Handle,
    mut bytes: &[u8],
) -> Result<(), &'static str> {
    while !bytes.is_empty() {
        let written = data.write(handle, bytes)?;
        if written == 0 || written > bytes.len() {
            return Err("The data connection stopped accepting bytes");
        }
        bytes = &bytes[written..];
    }
    Ok(())
}

/// The acknowledgement of a delivery: four bytes of length, then JSON holding one unsigned byte count.
/// A missing, oversize, malformed or non-numeric one is a failure, never a success.
pub fn read_ack<D: Data>(data: &mut D, handle: Handle) -> Result<u64, &'static str> {
    let mut prefix = [0u8; 4];
    read_exact(data, handle, &mut prefix)?;
    let length = u32::from_be_bytes(prefix) as usize;
    if length == 0 || length > ACK_LIMIT {
        return Err("The delivery acknowledgement is not valid");
    }
    let mut body = vec![0u8; length];
    read_exact(data, handle, &mut body)?;
    serde_json::from_slice::<u64>(&body).map_err(|_| "The delivery acknowledgement is not valid")
}

fn read_exact<D: Data>(
    data: &mut D,
    handle: Handle,
    mut into: &mut [u8],
) -> Result<(), &'static str> {
    while !into.is_empty() {
        let count = data.read(handle, into)?;
        if count == 0 || count > into.len() {
            return Err("The data connection ended early");
        }
        into = &mut into[count..];
    }
    Ok(())
}

#[cfg(target_family = "wasm")]
mod import {
    #[link(wasm_import_module = "peersus_v1")]
    unsafe extern "C" {
        // Pointers and lengths are `usize`: 32 bits in a wasm32 module and 64 in a wasm64 one, which is exactly what the
        // host expects of each. The module's own memory, not this file, decides which.
        #[link_name = "receive"]
        pub fn receive(buffer: *mut u8, capacity: usize) -> i32;
        #[link_name = "send"]
        pub fn send(buffer: *const u8, length: usize) -> i32;
        #[link_name = "data_open"]
        pub fn data_open(kind: i32, number: u64) -> i32;
        #[link_name = "data_read"]
        pub fn data_read(handle: i32, buffer: *mut u8, capacity: usize) -> i32;
        #[link_name = "data_write"]
        pub fn data_write(handle: i32, buffer: *const u8, length: usize) -> i32;
        #[link_name = "data_close"]
        pub fn data_close(handle: i32) -> i32;
        #[link_name = "data_shutdown"]
        pub fn data_shutdown(handle: i32) -> i32;
        #[link_name = "poll"]
        pub fn poll(handles: *const i32, count: usize, timeout_ms: i32) -> i32;
    }
}

/// The real host. Only meaningful inside the interpreter.
#[cfg(target_family = "wasm")]
pub struct Host;

#[cfg(target_family = "wasm")]
impl Wire for Host {
    fn receive(&mut self) -> Result<Value, &'static str> {
        let mut buffer = vec![0u8; LIMIT];
        let count = unsafe { import::receive(buffer.as_mut_ptr(), buffer.len()) };
        if count <= 0 || count as usize > LIMIT {
            return Err("The host disconnected");
        }
        serde_json::from_slice(&buffer[..count as usize]).map_err(|_| "Invalid host response")
    }
    fn send(&mut self, value: Value) -> Result<(), &'static str> {
        let bytes = serde_json::to_vec(&value).map_err(|_| "Invalid extension message")?;
        if bytes.len() > LIMIT {
            return Err("Extension message exceeds its bound");
        }
        if unsafe { import::send(bytes.as_ptr(), bytes.len()) } != 0 {
            return Err("The host disconnected");
        }
        Ok(())
    }
}

#[cfg(target_family = "wasm")]
impl Data for Host {
    fn open(&mut self, claim: bool, number: u64) -> Result<Handle, &'static str> {
        let handle = unsafe { import::data_open(i32::from(claim), number) };
        if handle < 0 {
            Err("The data connection could not be opened")
        } else {
            Ok(handle)
        }
    }
    fn read(&mut self, handle: Handle, into: &mut [u8]) -> Result<usize, &'static str> {
        let count = unsafe { import::data_read(handle, into.as_mut_ptr(), into.len()) };
        usize::try_from(count).map_err(|_| "The data connection failed")
    }
    fn write(&mut self, handle: Handle, from: &[u8]) -> Result<usize, &'static str> {
        let count = unsafe { import::data_write(handle, from.as_ptr(), from.len()) };
        usize::try_from(count).map_err(|_| "The data connection failed")
    }
    fn shutdown(&mut self, handle: Handle) -> Result<(), &'static str> {
        if unsafe { import::data_shutdown(handle) } < 0 {
            Err("The data connection failed")
        } else {
            Ok(())
        }
    }
    fn close(&mut self, handle: Handle) {
        let _ = unsafe { import::data_close(handle) };
    }
    fn poll(&mut self, handles: &[Handle], timeout_ms: i32) -> Result<Option<Handle>, &'static str> {
        match unsafe { import::poll(handles.as_ptr(), handles.len(), timeout_ms) } {
            -1 => Ok(None),
            value if value >= 0 => Ok(Some(value)),
            _ => Err("The host readiness wait failed"),
        }
    }
}

#[cfg(test)]
pub(crate) mod scripted {
    use super::*;
    use std::collections::VecDeque;

    /// A host that answers control calls and data streams from a script, and records what was sent.
    #[derive(Default)]
    pub struct Script {
        pub results: VecDeque<Value>,
        pub deliveries: VecDeque<Value>,
        pub sent: Vec<Value>,
        pub incoming: VecDeque<Vec<u8>>,
        pub written: Vec<u8>,
        pub opened: Vec<(bool, u64)>,
        pub shutdowns: usize,
        pub closed: usize,
        /// The most bytes a write accepts at once: real streams split writes.
        pub write_chunk: usize,
        /// Acknowledge each stream with exactly the bytes written on it, as a well-behaved host does.
        pub acks: bool,
        pub stream_start: usize,
    }

    impl Wire for Script {
        fn send(&mut self, value: Value) -> Result<(), &'static str> {
            self.sent.push(value);
            Ok(())
        }
        fn receive(&mut self) -> Result<Value, &'static str> {
            if let Some(delivery) = self.deliveries.pop_front() {
                return Ok(delivery);
            }
            let result = self
                .results
                .pop_front()
                .ok_or("Unexpected extra operation")?;
            let id = self
                .sent
                .last()
                .and_then(|call| call.get("id"))
                .cloned()
                .unwrap_or(Value::Null);
            Ok(serde_json::json!({"type":"result","id":id,"result":result}))
        }
    }

    impl Data for Script {
        fn open(&mut self, claim: bool, number: u64) -> Result<Handle, &'static str> {
            self.opened.push((claim, number));
            self.stream_start = self.written.len();
            Ok(7)
        }
        fn read(&mut self, _: Handle, into: &mut [u8]) -> Result<usize, &'static str> {
            let Some(front) = self.incoming.front_mut() else {
                return Ok(0);
            };
            let count = front.len().min(into.len());
            into[..count].copy_from_slice(&front[..count]);
            front.drain(..count);
            if front.is_empty() {
                self.incoming.pop_front();
            }
            Ok(count)
        }
        fn write(&mut self, _: Handle, from: &[u8]) -> Result<usize, &'static str> {
            let count = if self.write_chunk == 0 {
                from.len()
            } else {
                from.len().min(self.write_chunk)
            };
            self.written.extend_from_slice(&from[..count]);
            Ok(count)
        }
        fn shutdown(&mut self, _: Handle) -> Result<(), &'static str> {
            self.shutdowns += 1;
            if self.acks {
                self.incoming.push_front(ack((self.written.len() - self.stream_start) as u64));
            }
            Ok(())
        }
        fn close(&mut self, _: Handle) {
            self.closed += 1;
        }
        fn poll(&mut self, handles: &[Handle], _: i32) -> Result<Option<Handle>, &'static str> {
            Ok(Some(if self.deliveries.is_empty() { handles.first().copied().unwrap_or(0) } else { 0 }))
        }
    }

    /// A host acknowledgement for `count` bytes, as the application frames it.
    pub fn ack(count: u64) -> Vec<u8> {
        let body = serde_json::to_vec(&count).unwrap();
        let mut framed = (body.len() as u32).to_be_bytes().to_vec();
        framed.extend_from_slice(&body);
        framed
    }
}

#[cfg(test)]
mod tests {
    use super::scripted::*;
    use super::*;

    #[test]
    fn a_write_split_by_the_host_is_still_written_whole() {
        let mut host = Script {
            write_chunk: 3,
            ..Script::default()
        };
        write_all(&mut host, 7, b"abcdefghij").unwrap();
        assert_eq!(host.written, b"abcdefghij");
    }

    #[test]
    fn an_acknowledgement_is_read_even_when_it_arrives_in_pieces() {
        let framed = ack(1234);
        let mut host = Script::default();
        for byte in &framed {
            host.incoming.push_back(vec![*byte]);
        }
        assert_eq!(read_ack(&mut host, 7).unwrap(), 1234);
    }

    #[test]
    fn a_missing_oversize_or_malformed_acknowledgement_is_a_failure() {
        let mut none = Script::default();
        assert!(read_ack(&mut none, 7).is_err());
        let mut oversize = Script::default();
        oversize
            .incoming
            .push_back((ACK_LIMIT as u32 + 1).to_be_bytes().to_vec());
        assert!(read_ack(&mut oversize, 7).is_err());
        let mut text = Script::default();
        let body = br#""12""#;
        let mut framed = (body.len() as u32).to_be_bytes().to_vec();
        framed.extend_from_slice(body);
        text.incoming.push_back(framed);
        assert!(
            read_ack(&mut text, 7).is_err(),
            "a count in text is not a count"
        );
        let mut zero = Script::default();
        zero.incoming.push_back(vec![0, 0, 0, 0]);
        assert!(read_ack(&mut zero, 7).is_err());
    }
}
