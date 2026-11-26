//! The picture a person chooses to stand for their device.
//!
//! WHAT IT IS. A small square-ish image kept as plain RGBA pixels, not as a picture file. Pixels
//! are the one form that needs no decoder when it is read back, so nothing that reads a stored or
//! received profile ever runs a PNG or JPEG decoder on bytes someone else made. A picture file is
//! read once, where the person chose it, and turned into pixels there; from then on only these
//! pixels exist.
//!
//! WHAT IT BOUNDS. Width and height are at most [`MAX_EDGE`], and the length is exactly
//! `width * height * 4`, so an avatar can never be larger than [`MAX_BYTES`] and a value whose
//! numbers disagree is refused rather than trusted.
//!
//! WHAT IT DOES NOT HOLD. A name, a device or an identity: the picture is only the picture. It is
//! kept apart from the preferences because it is large next to them and would otherwise travel in
//! every snapshot and every change of an unrelated setting.

use serde::{Deserialize, Deserializer, Serialize};

/// Longest side, in pixels. Large enough for a round picture beside a name, small enough that the
/// whole picture is a few tens of kilobytes.
pub const MAX_EDGE: u32 = 128;
/// Most bytes a picture can hold: `MAX_EDGE * MAX_EDGE * 4`.
pub const MAX_BYTES: usize = (MAX_EDGE * MAX_EDGE * 4) as usize;
/// Longest side of a picture an extension keeps for its pages (`peersus.pictures`): a thumbnail made
/// in the extension's own process, never decoded by the application. Held to the same checks as a
/// profile picture, with this edge instead of [`MAX_EDGE`].
pub const KEPT_EDGE: u32 = peersus_api::pictures::MAX_EDGE;
/// Most bytes a kept picture can hold: `KEPT_EDGE * KEPT_EDGE * 4`.
pub const KEPT_BYTES: usize = (KEPT_EDGE * KEPT_EDGE * 4) as usize;

/// Why pixels were not accepted as a profile picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// Nothing to show: a side is zero.
    Empty,
    /// A side is longer than the picture's longest edge.
    TooBig,
    /// The number of bytes is not `width * height * 4`.
    Mismatch,
}

impl Refused {
    /// A sentence for the person.
    pub fn words(self) -> &'static str {
        match self {
            Self::Empty => "That picture is empty.",
            Self::TooBig => "That picture is larger than the profile picture may be.",
            Self::Mismatch => "That picture is damaged.",
        }
    }
}

/// Plain RGBA pixels, row by row, four bytes each, no side longer than `EDGE`. There is no way to hold
/// one that does not satisfy that: it is checked when it is made and when it is read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Image<const EDGE: u32> {
    width: u32,
    height: u32,
    #[serde(serialize_with = "bytes::serialize")]
    rgba: Vec<u8>,
}

/// A profile picture: the pixels a person chose to stand for their device.
pub type Picture = Image<MAX_EDGE>;
/// A picture an extension keeps for its own pages, made in its own process.
pub type Kept = Image<KEPT_EDGE>;

impl<const EDGE: u32> Image<EDGE> {
    /// Take `rgba` as a picture `width` by `height` pixels.
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Result<Self, Refused> {
        if width == 0 || height == 0 {
            return Err(Refused::Empty);
        }
        if width > EDGE || height > EDGE {
            return Err(Refused::TooBig);
        }
        if rgba.len() as u64 != u64::from(width) * u64::from(height) * 4 {
            return Err(Refused::Mismatch);
        }
        Ok(Self {
            width,
            height,
            rgba,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// The pixels, four bytes each: red, green, blue, alpha.
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }
}

/// What names a picture without carrying it: a SHA-256 over the picture's whole content AND its
/// shape. Two devices that hold the same picture compute the same digest, so one can tell the other
/// "my picture is this" in 32 bytes, and the other, which keeps the pictures it has received under
/// their digests, asks for the picture only when it does not have that digest. Never a way to
/// identify a person or a device: it names pixels, nothing else.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest([u8; 32]);

impl Digest {
    /// The 32 bytes, as they travel.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// A digest from the 32 bytes that were received.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl std::fmt::Debug for Digest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Digest({self})")
    }
}

/// Lowercase hexadecimal, 64 digits: the form kept in a file name and in a log.
impl std::fmt::Display for Digest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// The text form read back: exactly 64 lowercase hexadecimal digits and nothing else, so that a digest
/// used as part of a file name can never be anything but that.
impl std::str::FromStr for Digest {
    type Err = Refused;

    fn from_str(text: &str) -> Result<Self, Refused> {
        if text.len() != 64
            || !text
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Refused::Mismatch);
        }
        let mut out = [0u8; 32];
        for (at, byte) in out.iter_mut().enumerate() {
            *byte =
                u8::from_str_radix(&text[at * 2..at * 2 + 2], 16).map_err(|_| Refused::Mismatch)?;
        }
        Ok(Self(out))
    }
}

/// A digest in a snapshot or a file is its text, and is read back through [`Digest`]'s one parser, so
/// what is stored or received is held to exactly what it prints as and to nothing looser.
impl Serialize for Digest {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse()
            .map_err(|refused: Refused| serde::de::Error::custom(refused.words()))
    }
}

/// The two things a digest is made over, so the two can never be taken for each other: a picture's
/// digest and the digest of having none start with different words.
const PICTURE_DOMAIN: &[u8] = b"peersus/profile-picture/1\0";
const NO_PICTURE_DOMAIN: &[u8] = b"peersus/profile-no-picture/1\0";

impl<const EDGE: u32> Image<EDGE> {
    /// This picture's digest: its width, its height and its pixels, each with its length fixed, under a
    /// word of its own. The shape is part of it because the same bytes in another shape are another
    /// picture.
    pub fn digest(&self) -> Digest {
        use sha2::{Digest as _, Sha256};
        let mut hash = Sha256::new();
        hash.update(PICTURE_DOMAIN);
        hash.update(self.width.to_be_bytes());
        hash.update(self.height.to_be_bytes());
        hash.update((self.rgba.len() as u64).to_be_bytes());
        hash.update(&self.rgba);
        Digest(hash.finalize().into())
    }
}

impl Picture {
    /// The digest that says "no picture": what a device announces when it has none. It is made under
    /// another word than any picture's, so it can never be one.
    pub fn digest_of_none() -> Digest {
        use sha2::{Digest as _, Sha256};
        Digest(Sha256::digest(NO_PICTURE_DOMAIN).into())
    }
}

/// The wire and file form of a picture: the three fields, read back through [`Image::new`], so a
/// stored or received picture is held to the same limits as one made here.
impl<'de, const EDGE: u32> Deserialize<'de> for Image<EDGE> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            width: u32,
            height: u32,
            rgba: String,
        }
        let raw = Raw::deserialize(deserializer)?;
        let most = u64::from(EDGE) * u64::from(EDGE) * 4;
        let rgba = bytes::decode(&raw.rgba, most).map_err(serde::de::Error::custom)?;
        Self::new(raw.width, raw.height, rgba)
            .map_err(|refused| serde::de::Error::custom(refused.words()))
    }
}

/// Pixels as one lowercase hexadecimal string. A list of numbers would be four times the size in
/// JSON and would let a length be wrong by one without any single number being wrong.
mod bytes {
    /// Said when the text is longer than any picture can be, before a byte of it is decoded.
    pub const TOO_LONG: &str = "That picture is larger than the profile picture may be.";
    const DAMAGED: &str = "That picture is damaged.";

    pub fn serialize<S: serde::Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        use std::fmt::Write as _;
        let mut text = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            let _ = write!(text, "{byte:02x}");
        }
        serializer.serialize_str(&text)
    }

    /// The pixels `text` spells, refused before anything is allocated for them when it is longer than `most`
    /// bytes could be.
    pub fn decode(text: &str, most: u64) -> Result<Vec<u8>, &'static str> {
        if text.len() as u64 > most * 2 {
            return Err(TOO_LONG);
        }
        if !text.len().is_multiple_of(2) {
            return Err(DAMAGED);
        }
        (0..text.len())
            .step_by(2)
            .map(|at| {
                text.get(at..at + 2)
                    .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                    .ok_or(DAMAGED)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixels(width: u32, height: u32) -> Vec<u8> {
        (0..width * height * 4).map(|n| (n % 251) as u8).collect()
    }

    #[test]
    fn pixels_that_agree_with_their_size_and_stay_within_the_limit_are_a_picture() {
        let picture = Picture::new(3, 2, pixels(3, 2)).unwrap();
        assert_eq!((picture.width(), picture.height()), (3, 2));
        assert_eq!(picture.rgba(), pixels(3, 2).as_slice());
        // The largest there can be.
        assert!(Picture::new(MAX_EDGE, MAX_EDGE, pixels(MAX_EDGE, MAX_EDGE)).is_ok());
        assert_eq!(pixels(MAX_EDGE, MAX_EDGE).len(), MAX_BYTES);
        // A line is a picture too.
        assert!(Picture::new(MAX_EDGE, 1, pixels(MAX_EDGE, 1)).is_ok());
    }

    #[test]
    fn what_is_empty_too_big_or_does_not_add_up_is_refused_with_its_own_reason() {
        assert_eq!(Picture::new(0, 4, Vec::new()), Err(Refused::Empty));
        assert_eq!(Picture::new(4, 0, Vec::new()), Err(Refused::Empty));
        assert_eq!(
            Picture::new(MAX_EDGE + 1, 1, pixels(MAX_EDGE + 1, 1)),
            Err(Refused::TooBig)
        );
        assert_eq!(
            Picture::new(1, MAX_EDGE + 1, pixels(1, MAX_EDGE + 1)),
            Err(Refused::TooBig)
        );
        // Sides that would overflow a multiplication are still just too big.
        assert_eq!(
            Picture::new(u32::MAX, u32::MAX, Vec::new()),
            Err(Refused::TooBig)
        );
        // One byte short and one byte over.
        let mut short = pixels(2, 2);
        short.pop();
        assert_eq!(Picture::new(2, 2, short), Err(Refused::Mismatch));
        let mut over = pixels(2, 2);
        over.push(0);
        assert_eq!(Picture::new(2, 2, over), Err(Refused::Mismatch));
        // Every refusal can be told to a person in words.
        for refused in [Refused::Empty, Refused::TooBig, Refused::Mismatch] {
            assert!(!refused.words().trim().is_empty());
        }
    }

    #[test]
    fn a_picture_round_trips_through_its_text_form_and_is_checked_again_on_the_way_in() {
        let picture = Picture::new(4, 3, pixels(4, 3)).unwrap();
        let json = serde_json::to_string(&picture).unwrap();
        assert!(json.contains("\"width\":4") && json.contains("\"height\":3"));
        let back: Picture = serde_json::from_str(&json).unwrap();
        assert_eq!(back, picture);

        // The numbers and the pixels disagree: refused when read, not trusted because it parsed.
        let lying = r#"{"width":4,"height":3,"rgba":"00ff"}"#;
        assert!(serde_json::from_str::<Picture>(lying).is_err());
        // Over the limit, even with the right number of bytes.
        let big = format!(
            r#"{{"width":{},"height":1,"rgba":"{}"}}"#,
            MAX_EDGE + 1,
            "00".repeat((MAX_EDGE as usize + 1) * 4)
        );
        assert!(serde_json::from_str::<Picture>(&big).is_err());
    }

    #[test]
    fn a_stored_or_received_picture_that_is_malformed_is_refused_whatever_way_it_is_malformed() {
        for bad in [
            r#"{"width":1,"height":1,"rgba":"zzzzzzzz"}"#, // not hexadecimal
            r#"{"width":1,"height":1,"rgba":"0011223"}"#,  // an odd number of digits
            r#"{"width":1,"height":1,"rgba":"00112233","x":1}"#, // a field nobody declared
            r#"{"width":1,"height":1}"#,                   // no pixels
            r#"{"width":-1,"height":1,"rgba":"00112233"}"#, // a negative side
            r#"{"width":1,"height":1,"rgba":"é0112233"}"#, // not ASCII
            r#"{"width":1,"height":1,"rgba":null}"#,
            r#"[1,2,3]"#,
        ] {
            assert!(
                serde_json::from_str::<Picture>(bad).is_err(),
                "{bad} was accepted"
            );
        }
    }

    #[test]
    fn text_longer_than_any_picture_can_be_is_refused_before_it_is_turned_into_bytes() {
        // Twice the largest picture, as digits: never decoded, just refused for its length.
        let huge = format!(
            r#"{{"width":1,"height":1,"rgba":"{}"}}"#,
            "0".repeat(MAX_BYTES * 2 + 2)
        );
        let error = serde_json::from_str::<Picture>(&huge)
            .unwrap_err()
            .to_string();
        // Told apart from a later mismatch: this is the refusal that comes BEFORE decoding.
        assert!(
            error.contains(bytes::TOO_LONG),
            "refused for another reason, after decoding: {error}"
        );
        // Exactly the longest a picture can be is not over the ceiling.
        let exact = format!(
            r#"{{"width":{MAX_EDGE},"height":{MAX_EDGE},"rgba":"{}"}}"#,
            "0".repeat(MAX_BYTES * 2)
        );
        assert!(serde_json::from_str::<Picture>(&exact).is_ok());
    }

    #[test]
    fn the_digest_is_the_same_for_the_same_picture_and_differs_for_any_other() {
        let a = Picture::new(4, 3, pixels(4, 3)).unwrap();
        let same = Picture::new(4, 3, pixels(4, 3)).unwrap();
        assert_eq!(
            a.digest(),
            same.digest(),
            "the same picture has one digest, however it was made"
        );
        assert_eq!(a.digest(), a.clone().digest());

        // One pixel changed, by one: a different picture, a different digest.
        let mut one = pixels(4, 3);
        one[17] = one[17].wrapping_add(1);
        assert_ne!(a.digest(), Picture::new(4, 3, one).unwrap().digest());
        // Only the opacity of one pixel changed.
        let mut alpha = pixels(4, 3);
        alpha[3] = alpha[3].wrapping_add(1);
        assert_ne!(a.digest(), Picture::new(4, 3, alpha).unwrap().digest());
    }

    #[test]
    fn the_digest_covers_the_shape_so_that_the_same_bytes_in_another_shape_are_another_picture() {
        // Twelve pixels as 4 by 3, as 3 by 4, as 6 by 2 and as 12 by 1: the very same bytes, several pictures.
        let bytes = pixels(4, 3);
        let shapes = [(4, 3), (3, 4), (6, 2), (12, 1), (2, 6), (1, 12)];
        let digests: Vec<_> = shapes
            .iter()
            .map(|&(w, h)| Picture::new(w, h, bytes.clone()).unwrap().digest())
            .collect();
        for (i, left) in digests.iter().enumerate() {
            for right in &digests[i + 1..] {
                assert_ne!(
                    left, right,
                    "two shapes of the same bytes were given one digest"
                );
            }
        }
    }

    #[test]
    fn there_is_a_digest_for_having_no_picture_and_it_is_not_the_digest_of_any_picture() {
        let none = Picture::digest_of_none();
        assert_eq!(none, Picture::digest_of_none());
        for (w, h) in [(1, 1), (2, 2), (4, 3)] {
            assert_ne!(none, Picture::new(w, h, pixels(w, h)).unwrap().digest());
        }
        // Even a picture of nothing but zeros is not "no picture".
        assert_ne!(none, Picture::new(1, 1, vec![0; 4]).unwrap().digest());
    }

    #[test]
    fn a_digest_written_as_text_reads_back_and_a_bad_one_is_refused() {
        let digest = Picture::new(2, 2, pixels(2, 2)).unwrap().digest();
        let text = digest.to_string();
        assert_eq!(text.len(), 64, "a digest is 32 bytes as 64 digits");
        assert!(
            text.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        assert_eq!(text.parse::<Digest>().unwrap(), digest);
        // 64 BYTES that are not 64 digits: multi-byte characters, which a slicing parser would split.
        let multibyte = "é".repeat(32);
        assert_eq!(multibyte.len(), 64);
        assert!(multibyte.parse::<Digest>().is_err());
        for bad in [
            "",
            "00",
            &"0".repeat(63),
            &"0".repeat(65),
            &"g".repeat(64),
            &"A".repeat(64),
            " 00",
        ] {
            assert!(
                bad.parse::<Digest>().is_err(),
                "{bad:?} was accepted as a digest"
            );
        }
    }

    #[test]
    fn a_digest_travels_as_its_text_and_a_malformed_one_is_refused_when_read() {
        let digest = Picture::new(2, 2, pixels(2, 2)).unwrap().digest();
        let json = serde_json::to_string(&digest).unwrap();
        assert_eq!(json, format!("\"{digest}\""));
        assert_eq!(serde_json::from_str::<Digest>(&json).unwrap(), digest);
        // Inside a larger value, as it is in a snapshot.
        let map: std::collections::BTreeMap<String, Digest> =
            serde_json::from_str(&format!(r#"{{"a":"{digest}"}}"#)).unwrap();
        assert_eq!(map["a"], digest);
        for bad in [
            r#""""#,
            r#""00""#,
            r#"1"#,
            r#"null"#,
            r#"[1,2,3]"#,
            &format!("\"{}\"", "G".repeat(64)),
            &format!("\"{}\"", "0".repeat(65)),
        ] {
            assert!(
                serde_json::from_str::<Digest>(bad).is_err(),
                "{bad} was read as a digest"
            );
        }
    }
}
