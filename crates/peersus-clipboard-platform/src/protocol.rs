//! Private framed protocol between the owner and its disposable clipboard helper.
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

// JSON can escape every UTF-8 byte into six bytes. Reject the frame before allocating it.
pub const MAX_FRAME: usize = crate::MAX_TEXT * 6 + 4096;

#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", deny_unknown_fields)]
pub enum Request {
    /// Start the metadata listener without reading or resetting observation.
    Ready,
    Read {
        observe: bool,
    },
    Write {
        text: String,
    },
    Reset,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Answer {
    pub ok: bool,
    pub text: Option<String>,
    pub changed: bool,
}

pub fn read<R: Read, T: serde::de::DeserializeOwned>(reader: &mut R) -> io::Result<Option<T>> {
    let mut header = [0; 4];
    if reader.read(&mut header[..1])? == 0 {
        return Ok(None);
    }
    reader.read_exact(&mut header[1..])?;
    let size = u32::from_be_bytes(header) as usize;
    if size == 0 || size > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid clipboard frame",
        ));
    }
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes)?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(io::Error::other)
}

pub fn write<W: Write, T: Serialize>(writer: &mut W, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if bytes.len() > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Oversized clipboard frame",
        ));
    }
    writer.write_all(&(bytes.len() as u32).to_be_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frames_preserve_unicode_and_reject_oversized_or_truncated_input() {
        let mut bytes = Vec::new();
        write(
            &mut bytes,
            &Request::Write {
                text: "é🙂\0".into(),
            },
        )
        .unwrap();
        let request: Request = read(&mut bytes.as_slice()).unwrap().unwrap();
        assert!(matches!(request, Request::Write { text } if text == "é🙂\0"));
        assert!(
            read::<_, Request>(&mut ((MAX_FRAME + 1) as u32).to_be_bytes().as_slice()).is_err()
        );
        assert!(read::<_, Request>(&mut &bytes[..bytes.len() - 1]).is_err());
        assert!(read::<_, Request>(&mut &[][..]).unwrap().is_none());
    }
}
