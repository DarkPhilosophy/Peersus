//! What crosses between two clipboards: one text, its size said first and bounded.
//!
//! This is the same format as the process variant of this plugin, written again here because a
//! freestanding guest has no `std::io` and one dependency only. The two must interoperate, so
//! `tests::cases_shared_with_the_process_variant` states the behaviour both obey.
//!
//! Layout, all integers big-endian: magic `PCB1`, one kind byte (1 = text), a four byte length, the body.
//! Nothing is believed about its own size: the header is checked BEFORE any body is read.

pub const MAGIC: [u8; 4] = *b"PCB1";
/// Most text accepted, in bytes.
pub const MAX_TEXT: usize = 1024 * 1024;
/// Bytes before an item's body: its magic, its kind and its length.
pub const HEADER: usize = 9;
const KIND_TEXT: u8 = 1;
const KIND_PNG: u8 = 2;
/// Most PNG data the process variant accepts. A text-only guest never takes it, but must still refuse it by name.
const MAX_PNG: usize = 32 * 1024 * 1024;

/// Why an item was refused. These variants carry no bytes from the item.
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    NotAnItem,
    UnknownKind,
    TooLarge,
    Truncated,
    BadText,
    /// A picture is a valid item of the format, but this guest takes only text.
    NotText,
}

/// The bytes to send for one text.
pub fn encode(text: &str) -> Result<Vec<u8>, Error> {
    if text.len() > MAX_TEXT {
        return Err(Error::TooLarge);
    }
    let length = u32::try_from(text.len()).map_err(|_| Error::TooLarge)?;
    let mut out = Vec::with_capacity(HEADER + text.len());
    out.extend_from_slice(&MAGIC);
    out.push(KIND_TEXT);
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(text.as_bytes());
    Ok(out)
}

/// What the header of an item says, checked before the body is looked at.
#[derive(Debug, PartialEq, Eq)]
pub struct Header {
    pub length: usize,
}

/// Read the first [`HEADER`] bytes. Nothing of the body is needed, so a hostile length costs nothing.
pub fn header(bytes: &[u8]) -> Result<Header, Error> {
    if bytes.len() < HEADER {
        // Fewer bytes than a header: either not an item at all or one that ended too soon.
        return Err(
            if MAGIC.starts_with(&bytes[..bytes.len().min(MAGIC.len())]) {
                Error::Truncated
            } else {
                Error::NotAnItem
            },
        );
    }
    if bytes[..4] != MAGIC {
        return Err(Error::NotAnItem);
    }
    let length = u32::from_be_bytes([bytes[5], bytes[6], bytes[7], bytes[8]]) as usize;
    match bytes[4] {
        KIND_TEXT if length > MAX_TEXT => Err(Error::TooLarge),
        KIND_TEXT => Ok(Header { length }),
        KIND_PNG if length > MAX_PNG => Err(Error::TooLarge),
        // Valid, bounded, and not something this guest places on a clipboard.
        KIND_PNG => Err(Error::NotText),
        _ => Err(Error::UnknownKind),
    }
}

/// The text of exactly one item. Bytes after it are the caller's to refuse: see [`whole`].
pub fn decode(bytes: &[u8]) -> Result<String, Error> {
    let header = header(bytes)?;
    let end = HEADER + header.length;
    let body = bytes.get(HEADER..end).ok_or(Error::Truncated)?;
    String::from_utf8(body.to_vec()).map_err(|_| Error::BadText)
}

/// Whether `bytes` is exactly one item and nothing after it: a sender that attaches more is not sending what was agreed.
pub fn whole(bytes: &[u8]) -> bool {
    header(bytes).is_ok_and(|header| bytes.len() == HEADER + header.length)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(kind: u8, length: u32) -> Vec<u8> {
        let mut bytes = MAGIC.to_vec();
        bytes.push(kind);
        bytes.extend_from_slice(&length.to_be_bytes());
        bytes
    }

    #[test]
    fn text_survives_the_trip_exactly_including_what_is_not_ascii() {
        for text in ["", "hello", "naïve — 日本語 🙂", "line\r\nbreak\ttab\0nul"] {
            assert_eq!(decode(&encode(text).unwrap()).unwrap(), text);
        }
    }

    #[test]
    fn the_limits_are_exact_not_one_off() {
        let at = "a".repeat(MAX_TEXT);
        assert!(encode(&at).is_ok(), "exactly the limit is accepted");
        assert_eq!(decode(&encode(&at).unwrap()).unwrap(), at);
        assert_eq!(encode(&"a".repeat(MAX_TEXT + 1)), Err(Error::TooLarge));
    }

    /// The same cases the process variant's `wire` tests state. Both implementations must give these answers.
    #[test]
    fn cases_shared_with_the_process_variant() {
        // A length over the limit is refused from the header alone: no body is given.
        for (kind, length, expected) in [
            (KIND_TEXT, MAX_TEXT as u32 + 1, Error::TooLarge),
            (KIND_PNG, MAX_PNG as u32 + 1, Error::TooLarge),
            (KIND_TEXT, u32::MAX, Error::TooLarge),
            (KIND_PNG, u32::MAX, Error::TooLarge),
        ] {
            assert_eq!(
                decode(&head(kind, length)),
                Err(expected),
                "kind {kind}, length {length}"
            );
        }
        // A promised body that never comes is truncated, however large the promise.
        assert_eq!(
            decode(&head(KIND_TEXT, MAX_TEXT as u32)),
            Err(Error::Truncated)
        );
        let mut short = head(KIND_TEXT, 10);
        short.extend_from_slice(b"abc");
        assert_eq!(decode(&short), Err(Error::Truncated));
        // Something that is not an item.
        for bytes in [
            b"".to_vec(),
            b"PCB".to_vec(),
            b"PCB2\x01\x00\x00\x00\x00".to_vec(),
            b"HTTP/1.1 200 OK\r\n".to_vec(),
            vec![0u8; 9],
        ] {
            let result = decode(&bytes);
            assert!(
                matches!(result, Err(Error::NotAnItem | Error::Truncated)),
                "{bytes:?} gave {result:?}"
            );
        }
        assert_eq!(decode(&head(9, 0)), Err(Error::UnknownKind));
        // Text that is not UTF-8.
        let mut bad = head(KIND_TEXT, 2);
        bad.extend_from_slice(&[0xC3, 0x28]);
        assert_eq!(decode(&bad), Err(Error::BadText));
    }

    #[test]
    fn bytes_past_the_promised_length_are_not_part_of_the_item_and_are_seen() {
        let mut bytes = head(KIND_TEXT, 3);
        bytes.extend_from_slice(b"abcTRAILING");
        assert_eq!(decode(&bytes).unwrap(), "abc");
        assert!(
            !whole(&bytes),
            "an item with something after it is not whole"
        );
        assert!(whole(&encode("abc").unwrap()));
    }

    #[test]
    fn a_picture_is_a_valid_item_this_guest_refuses_by_name() {
        let mut picture = head(KIND_PNG, 4);
        picture.extend_from_slice(&[1, 2, 3, 4]);
        assert_eq!(decode(&picture), Err(Error::NotText));
    }
}
