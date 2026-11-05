//! What crosses between two clipboards: one item, with its size said first and bounded.
//!
//! A device on the other end may be misbehaving, so nothing here believes what it is told about
//! its own size. The header is read and checked BEFORE any body is read, and the body is read
//! through a limit equal to the checked length, so a hostile header cannot make this allocate
//! more than the limits below, and a body that lies about its length is cut off and refused.
//!
//! Layout, all integers big-endian:
//!
//! ```text
//! magic  4 bytes  "PCB1"
//! kind   1 byte   1 = text (UTF-8), 2 = picture (PNG)
//! length 4 bytes  size of the body that follows
//! body   `length` bytes
//! ```
use std::io::{self, Read};

/// First bytes of every item, so something that is not one is refused at once.
pub const MAGIC: [u8; 4] = *b"PCB1";
/// Most text accepted, in bytes.
pub const MAX_TEXT: u32 = peersus_clipboard_platform::MAX_TEXT as u32;
/// Most PNG data accepted, in bytes, before it is even looked at as a picture.
pub const MAX_PNG: u32 = 32 * 1024 * 1024;
/// Widest or tallest picture accepted, in pixels.
pub const MAX_DIMENSION: u32 = 8192;
/// Most memory a decoded picture may take, in bytes.
pub const MAX_PIXEL_BYTES: u64 = 64 * 1024 * 1024;

const KIND_TEXT: u8 = 1;
const KIND_PNG: u8 = 2;
/// Bytes before an item's body: its magic, its kind and its length.
pub const HEADER: usize = 9;

/// One thing a clipboard held.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    Text(String),
    /// PNG data, already known to be within the size limit. Whether it is a picture within
    /// the dimension limits is decided when it is decoded.
    Png(Vec<u8>),
}

/// Why an item was refused. Fixed words: nothing of what was received is repeated back.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("not a clipboard item")]
    NotAnItem,
    #[error("an item of a kind this does not take")]
    UnknownKind,
    #[error("an item larger than is accepted")]
    TooLarge,
    #[error("an item that ended before it should have")]
    Truncated,
    #[error("text that is not valid")]
    BadText,
    #[error("a picture that is not valid or is larger than is accepted")]
    BadPicture,
    #[error("the connection failed")]
    Io,
}

/// An item as the bytes to send.
pub fn encode(item: &Item) -> Result<Vec<u8>, Error> {
    let (kind, body, maximum): (u8, &[u8], u32) = match item {
        Item::Text(text) => (KIND_TEXT, text.as_bytes(), MAX_TEXT),
        Item::Png(bytes) => (KIND_PNG, bytes, MAX_PNG),
    };
    let length = u32::try_from(body.len()).map_err(|_| Error::TooLarge)?;
    if length > maximum {
        return Err(Error::TooLarge);
    }
    let mut out = Vec::with_capacity(HEADER + body.len());
    out.extend_from_slice(&MAGIC);
    out.push(kind);
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(body);
    Ok(out)
}

/// Read one item. The header is checked before the body is read at all.
pub fn decode(mut from: impl Read) -> Result<Item, Error> {
    let mut header = [0u8; HEADER];
    read_all(&mut from, &mut header)?;
    if header[..4] != MAGIC {
        return Err(Error::NotAnItem);
    }
    let kind = header[4];
    let length = u32::from_be_bytes([header[5], header[6], header[7], header[8]]);
    let maximum = match kind {
        KIND_TEXT => MAX_TEXT,
        KIND_PNG => MAX_PNG,
        _ => return Err(Error::UnknownKind),
    };
    if length > maximum {
        return Err(Error::TooLarge);
    }
    // Read through a limit equal to the checked length, and only grow as bytes really arrive:
    // a sender that promises a large body and sends none costs nothing.
    let mut body = Vec::new();
    let read = from
        .take(u64::from(length))
        .read_to_end(&mut body)
        .map_err(|_| Error::Io)?;
    if read as u64 != u64::from(length) {
        return Err(Error::Truncated);
    }
    match kind {
        KIND_TEXT => String::from_utf8(body)
            .map(Item::Text)
            .map_err(|_| Error::BadText),
        _ => Ok(Item::Png(body)),
    }
}

fn read_all(from: &mut impl Read, buffer: &mut [u8]) -> Result<(), Error> {
    from.read_exact(buffer).map_err(|error| match error.kind() {
        io::ErrorKind::UnexpectedEof => Error::Truncated,
        _ => Error::Io,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn roundtrip(item: &Item) -> Item {
        decode(Cursor::new(encode(item).unwrap())).unwrap()
    }

    #[test]
    fn text_survives_the_trip_exactly_including_what_is_not_ascii() {
        for text in ["", "hello", "naïve — 日本語 🙂", "line\r\nbreak\ttab\0nul"] {
            assert_eq!(
                roundtrip(&Item::Text(text.to_owned())),
                Item::Text(text.to_owned())
            );
        }
    }

    #[test]
    fn png_bytes_survive_the_trip_exactly() {
        let png: Vec<u8> = (0..5000u32)
            .map(|n| (n.wrapping_mul(31) % 251) as u8)
            .collect();
        assert_eq!(roundtrip(&Item::Png(png.clone())), Item::Png(png));
    }

    #[test]
    fn the_limits_are_exact_not_one_off() {
        let at = Item::Text("a".repeat(MAX_TEXT as usize));
        assert!(encode(&at).is_ok(), "exactly the limit is accepted");
        let over = Item::Text("a".repeat(MAX_TEXT as usize + 1));
        assert_eq!(encode(&over), Err(Error::TooLarge));
        assert_eq!(roundtrip(&at), at);
    }

    /// A header followed by nothing: what a hostile sender sends to see what is allocated.
    fn header(kind: u8, length: u32) -> Vec<u8> {
        let mut bytes = MAGIC.to_vec();
        bytes.push(kind);
        bytes.extend_from_slice(&length.to_be_bytes());
        bytes
    }

    #[test]
    fn a_length_over_the_limit_is_refused_before_any_body_is_read() {
        // The reader here would fail the test if asked for more than the header.
        struct HeaderOnly(Cursor<Vec<u8>>);
        impl Read for HeaderOnly {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                let before = self.0.position();
                let read = self.0.read(buffer)?;
                assert!(
                    before + read as u64 <= HEADER as u64,
                    "the body was read although the header was already refused"
                );
                Ok(read)
            }
        }
        for (kind, length) in [
            (KIND_TEXT, MAX_TEXT + 1),
            (KIND_PNG, MAX_PNG + 1),
            (KIND_TEXT, u32::MAX),
            (KIND_PNG, u32::MAX),
        ] {
            assert_eq!(
                decode(HeaderOnly(Cursor::new(header(kind, length)))),
                Err(Error::TooLarge),
                "kind {kind}, length {length}"
            );
        }
    }

    #[test]
    fn a_promised_body_that_never_comes_costs_nothing_and_is_truncated() {
        for (kind, length) in [(KIND_TEXT, MAX_TEXT), (KIND_PNG, MAX_PNG)] {
            assert_eq!(
                decode(Cursor::new(header(kind, length))),
                Err(Error::Truncated)
            );
        }
        // A body shorter than promised, however little.
        let mut short = header(KIND_TEXT, 10);
        short.extend_from_slice(b"abc");
        assert_eq!(decode(Cursor::new(short)), Err(Error::Truncated));
    }

    #[test]
    fn bytes_past_the_promised_length_are_not_read_into_the_item() {
        let mut bytes = header(KIND_TEXT, 3);
        bytes.extend_from_slice(b"abcTRAILING");
        let mut cursor = Cursor::new(bytes);
        assert_eq!(decode(&mut cursor).unwrap(), Item::Text("abc".into()));
        assert_eq!(
            cursor.position(),
            (HEADER + 3) as u64,
            "read exactly the item"
        );
    }

    #[test]
    fn something_that_is_not_an_item_is_refused() {
        for bytes in [
            b"".to_vec(),
            b"PCB".to_vec(),
            b"PCB2\x01\x00\x00\x00\x00".to_vec(),
            b"HTTP/1.1 200 OK\r\n".to_vec(),
            vec![0u8; 9],
        ] {
            let result = decode(Cursor::new(bytes.clone()));
            assert!(
                matches!(result, Err(Error::NotAnItem | Error::Truncated)),
                "{bytes:?} gave {result:?}"
            );
        }
        assert_eq!(
            decode(Cursor::new(header(9, 0))),
            Err(Error::UnknownKind),
            "a kind this does not take"
        );
    }

    #[test]
    fn text_that_is_not_utf8_is_refused() {
        let mut bytes = header(KIND_TEXT, 2);
        bytes.extend_from_slice(&[0xC3, 0x28]);
        assert_eq!(decode(Cursor::new(bytes)), Err(Error::BadText));
    }

    #[test]
    fn what_the_error_says_never_repeats_what_was_received() {
        let words: Vec<String> = [
            Error::NotAnItem,
            Error::UnknownKind,
            Error::TooLarge,
            Error::Truncated,
            Error::BadText,
            Error::BadPicture,
            Error::Io,
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        for text in &words {
            assert!(!text.is_empty() && text.len() < 80, "{text}");
            assert!(!text.contains('{') && !text.contains('"'), "{text}");
        }
    }
}
