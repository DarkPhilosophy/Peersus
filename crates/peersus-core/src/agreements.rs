//! The identity of an agreement: what the person saw, what the owner checks, and what is recorded.
//!
//! The digest covers every word, link, block and page in order, so a change to a single character of
//! a licence or a notice is a different agreement and has to be accepted again. The window sends the
//! digest of what it showed when the person pressed `Accept`; the owner compares it with the
//! agreement of the package it is holding, and installs only if they are the same.
use peersus_api::agreement::Agreements;
use sha2::{Digest, Sha256};
use std::fmt::Write;

/// Hex SHA-256 of the agreement as canonical JSON (fields in their declared order, no maps).
#[must_use]
pub fn digest(agreements: &Agreements) -> String {
    let bytes = serde_json::to_vec(agreements).unwrap_or_default();
    let mut hash = Sha256::new();
    hash.update(b"peersus-agreement-v1\0");
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(&bytes);
    hash.finalize()
        .iter()
        .fold(String::with_capacity(64), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use peersus_api::agreement::{Block, Kind, Link, Page};

    fn agreement() -> Agreements {
        Agreements {
            pages: vec![
                Page {
                    kind: Kind::Licence,
                    title: "MIT".into(),
                    explains: "You may use it.".into(),
                    changes: "Nothing changes.".into(),
                    spdx: Some("MIT".into()),
                    links: vec![Link {
                        label: "Text".into(),
                        url: "https://opensource.org/license/mit".into(),
                    }],
                    body: vec![],
                },
                Page {
                    kind: Kind::Notice,
                    title: "Notice".into(),
                    explains: "It does a thing.".into(),
                    changes: "A page is added.".into(),
                    spdx: None,
                    links: vec![],
                    body: vec![Block::Divider],
                },
            ],
        }
    }

    #[test]
    fn the_same_agreement_has_the_same_digest_and_any_change_makes_another() {
        let one = digest(&agreement());
        assert_eq!(one.len(), 64);
        assert!(one.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(one, digest(&agreement()));
        let mut word = agreement();
        word.pages[0].explains = "You may use it!".into();
        assert_ne!(one, digest(&word));
        let mut order = agreement();
        order.pages.reverse();
        assert_ne!(one, digest(&order));
        let mut block = agreement();
        block.pages[1].body.push(Block::Divider);
        assert_ne!(one, digest(&block));
        let mut link = agreement();
        link.pages[0].links[0].url = "https://opensource.org/license/mit/".into();
        assert_ne!(one, digest(&link));
    }
}
