//! Length-prefixed JSON frames with a hard size limit checked before allocation.
//!
//! A peer that announces a huge frame is refused without the host ever allocating for
//! it, and a frame that is not exactly one valid message ends the connection.
use serde::{Serialize, de::DeserializeOwned};
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Largest control frame. Matches the hub's message bound with room for the envelope.
pub const CONTROL_LIMIT: usize = 64 * 1024;

fn checked_length(length: usize, maximum: usize) -> io::Result<u32> {
    if length == 0 || length > maximum {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid frame size"));
    }
    u32::try_from(length).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid frame size"))
}

/// Read directly into a worker's bounded buffer, without decoding and re-encoding JSON.
/// A clean EOF before the prefix returns zero; a partial prefix/body is an error.
pub fn read_blocking_into(stream: &mut impl io::Read, into: &mut [u8], maximum: usize) -> io::Result<usize> {
    let mut prefix = [0; 4];
    match stream.read_exact(&mut prefix[..1]) {
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(0),
        result => result?,
    }
    stream.read_exact(&mut prefix[1..])?;
    let length = u32::from_be_bytes(prefix) as usize;
    checked_length(length, maximum.min(into.len()))?;
    stream.read_exact(&mut into[..length])?;
    Ok(length)
}

/// The same wire frame for a single-threaded worker's already encoded message.
pub fn write_blocking_bytes(stream: &mut impl io::Write, bytes: &[u8], maximum: usize) -> io::Result<()> {
    let length = checked_length(bytes.len(), maximum)?;
    stream.write_all(&length.to_be_bytes())?;
    stream.write_all(bytes)?;
    stream.flush()
}

pub fn write_blocking<T: Serialize>(stream: &mut impl io::Write, value: &T, maximum: usize) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|_| io::Error::other("Could not encode frame"))?;
    write_blocking_bytes(stream, &bytes, maximum)
}

pub async fn read<T: DeserializeOwned>(
    stream: &mut (impl AsyncRead + Unpin),
    maximum: usize,
) -> io::Result<T> {
    let length = stream.read_u32().await? as usize;
    // The size is validated first so a hostile length can never drive an allocation.
    checked_length(length, maximum)?;
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid frame"))
}

pub async fn write<T: Serialize>(
    stream: &mut (impl AsyncWrite + Unpin),
    value: &T,
    maximum: usize,
) -> io::Result<()> {
    let bytes =
        serde_json::to_vec(value).map_err(|_| io::Error::other("Could not encode frame"))?;
    let length = checked_length(bytes.len(), maximum)?;
    // One write keeps the prefix and body together so a reader never sees half a frame
    // from a single message.
    let mut out = Vec::with_capacity(4 + bytes.len());
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(&bytes);
    stream.write_all(&out).await?;
    stream.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use tokio::io::duplex;

    #[tokio::test]
    async fn round_trips_and_keeps_message_boundaries() {
        let (mut a, mut b) = duplex(4096);
        write(&mut a, &json!({"n": 1}), 1024).await.unwrap();
        write(&mut a, &json!({"n": 2}), 1024).await.unwrap();
        assert_eq!(read::<Value>(&mut b, 1024).await.unwrap(), json!({"n": 1}));
        assert_eq!(read::<Value>(&mut b, 1024).await.unwrap(), json!({"n": 2}));
    }

    #[tokio::test]
    async fn a_hostile_length_is_refused_without_allocating_it() {
        for announced in [0u32, 1025, u32::MAX] {
            let (mut a, mut b) = duplex(64);
            a.write_u32(announced).await.unwrap();
            // Only four bytes were sent. If the reader tried to allocate and read the
            // announced body it would hang or exhaust memory; it must fail at once.
            let error = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                read::<Value>(&mut b, 1024),
            )
            .await
            .expect("must not wait for a body that was refused")
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{announced}");
        }
    }

    #[tokio::test]
    async fn malformed_json_and_truncated_bodies_end_the_connection() {
        let (mut a, mut b) = duplex(64);
        a.write_u32(3).await.unwrap();
        a.write_all(b"{{{").await.unwrap();
        let error = read::<Value>(&mut b, 1024).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        let (mut a, mut b) = duplex(64);
        a.write_u32(10).await.unwrap();
        a.write_all(b"{}").await.unwrap();
        drop(a);
        assert!(read::<Value>(&mut b, 1024).await.is_err());
    }

    #[tokio::test]
    async fn writing_over_the_limit_fails_before_anything_is_sent() {
        let (mut a, mut b) = duplex(4096);
        let error = write(&mut a, &json!("x".repeat(2000)), 1024)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        drop(a);
        // Nothing reached the reader, not even a length prefix.
        assert!(read::<Value>(&mut b, 1024).await.is_err());
    }

    #[test]
    fn blocking_frames_keep_boundaries_and_detect_truncation_and_eof() {
        let mut encoded = Vec::new();
        write_blocking(&mut encoded, &json!({"n":1}), 1024).unwrap();
        write_blocking(&mut encoded, &json!({"n":2}), 1024).unwrap();
        let mut input = io::Cursor::new(encoded.clone());
        let mut buffer = [0; 1024];
        for expected in [json!({"n":1}), json!({"n":2})] {
            let length = read_blocking_into(&mut input, &mut buffer, 1024).unwrap();
            assert_eq!(serde_json::from_slice::<Value>(&buffer[..length]).unwrap(), expected);
        }
        assert_eq!(read_blocking_into(&mut input, &mut buffer, 1024).unwrap(), 0);
        for truncated in [encoded[..2].to_vec(), encoded[..6].to_vec()] {
            assert!(read_blocking_into(&mut io::Cursor::new(truncated), &mut buffer, 1024).is_err());
        }
    }

    #[test]
    fn blocking_frames_refuse_hostile_sizes_before_reading_or_writing_the_body() {
        for announced in [0u32, 1025, u32::MAX] {
            let mut input = io::Cursor::new(announced.to_be_bytes());
            let mut buffer = [7; 1024];
            assert_eq!(read_blocking_into(&mut input, &mut buffer, 1024).unwrap_err().kind(), io::ErrorKind::InvalidData);
            assert_eq!(buffer, [7; 1024]);
        }
        let mut output = Vec::new();
        assert!(write_blocking_bytes(&mut output, &[1;1025], 1024).is_err());
        assert!(output.is_empty());
    }

    #[tokio::test]
    async fn blocking_and_async_frames_are_wire_compatible() {
        let mut bytes = Vec::new();
        write_blocking(&mut bytes, &json!({"direction":"blocking"}), 1024).unwrap();
        assert_eq!(read::<Value>(&mut bytes.as_slice(), 1024).await.unwrap(), json!({"direction":"blocking"}));
        let (mut sender, mut receiver) = duplex(1024);
        write(&mut sender, &json!({"direction":"async"}), 1024).await.unwrap();
        drop(sender);
        let mut encoded = Vec::new();
        receiver.read_to_end(&mut encoded).await.unwrap();
        let mut buffer = [0; 1024];
        let length = read_blocking_into(&mut io::Cursor::new(encoded), &mut buffer, 1024).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&buffer[..length]).unwrap(), json!({"direction":"async"}));
    }
}
