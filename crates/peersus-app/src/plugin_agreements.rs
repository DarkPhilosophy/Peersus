//! What THIS person accepted of each extension's licences and notice.
//!
//! One small file in the person's own data directory, `agreements.json`: extension id to the digest
//! ([`peersus_core::agreements::digest`]) of the agreement that was accepted, and when. It belongs to the
//! user whose directory it is in, so an agreement is never shared between the people who use one
//! computer, and it stays when an extension is updated: an update whose agreement is word for word the
//! one already accepted is not asked again, and one that changed a single word is.
//!
//! The extension itself learns only whether the agreement installed now is the one accepted, as a bare
//! boolean (`extension.tos.accepted`). It cannot write this file, choose what is recorded, or press
//! the buttons that record it.
//!
//! LOSING IT COSTS A QUESTION, NEVER A BLOCK. A file that is missing, unreadable, too large, not valid
//! or a link reads as "nothing accepted": the extension keeps running as installed, answers `false`,
//! and the next installation of it asks again.
use crate::private_fs;
use peersus_api::manifest::Manifest;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, io::Read, path::Path};

/// The file, in the data directory.
pub const FILE: &str = "agreements.json";
/// Most bytes of it that are read.
pub const MAX_BYTES: u64 = 64 * 1024;
/// Most extensions it may name: the most the application will hold.
pub const MAX_ENTRIES: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    /// Hex SHA-256 of the agreement as it was accepted.
    sha256: String,
    /// When, in Unix seconds. For the person to read; nothing decides by it.
    accepted_at: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    accepted: BTreeMap<String, Record>,
}

fn plausible(digest: &str) -> bool {
    digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit())
}

fn read_ledger(data: &Path) -> Ledger {
    let path = data.join(FILE);
    let Ok(metadata) = fs::symlink_metadata(&path) else {
        return Ledger::default();
    };
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return Ledger::default();
    }
    let Ok(file) = private_fs::read_no_follow(&path) else {
        return Ledger::default();
    };
    let mut text = Vec::new();
    if file.take(MAX_BYTES + 1).read_to_end(&mut text).is_err() || text.len() as u64 > MAX_BYTES {
        return Ledger::default();
    }
    match serde_json::from_slice::<Ledger>(&text) {
        Ok(ledger)
            if ledger.accepted.len() <= MAX_ENTRIES
                && ledger.accepted.iter().all(|(id, record)| {
                    peersus_api::name::valid(id) && plausible(&record.sha256)
                }) =>
        {
            ledger
        }
        _ => Ledger::default(),
    }
}

fn write_ledger(data: &Path, ledger: &Ledger) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(ledger).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    peersus_net::atomic::write_private(&data.join(FILE), &bytes)
}

/// Every acceptance recorded in `data`: extension id to the digest accepted.
#[must_use]
pub fn load(data: &Path) -> BTreeMap<String, String> {
    read_ledger(data)
        .accepted
        .into_iter()
        .map(|(id, record)| (id, record.sha256))
        .collect()
}

/// Record that the person accepted the agreement with `digest` for `id`, replacing what was recorded. Refuses a digest that
/// is not one, and a ledger already holding the most it may.
pub fn accept(data: &Path, id: &str, digest: &str, now: u64) -> std::io::Result<()> {
    if !peersus_api::name::valid(id) || !plausible(digest) {
        return Err(std::io::Error::other("not an extension or a digest"));
    }
    let mut ledger = read_ledger(data);
    if !ledger.accepted.contains_key(id) && ledger.accepted.len() >= MAX_ENTRIES {
        return Err(std::io::Error::other("too many agreements"));
    }
    ledger.accepted.insert(
        id.to_owned(),
        Record {
            sha256: digest.to_ascii_lowercase(),
            accepted_at: now,
        },
    );
    write_ledger(data, &ledger)
}

/// Forget what was accepted for `id`, so that installing it again asks again. Nothing to forget is not an error.
pub fn forget(data: &Path, id: &str) -> std::io::Result<()> {
    let mut ledger = read_ledger(data);
    if ledger.accepted.remove(id).is_none() {
        return Ok(());
    }
    write_ledger(data, &ledger)
}

/// What a person at a terminal reads before accepting by hand: every page with what it is, what changes, its links and its own content, in the
/// order of the window's overlay, in plain text. The digest the person gives back is that of the same agreement.
#[must_use]
pub fn as_text(agreements: &peersus_api::agreement::Agreements) -> String {
    use peersus_api::agreement::{Block, Kind};
    let mut text = String::new();
    let total = agreements.pages.len();
    for (index, page) in agreements.pages.iter().enumerate() {
        let kind = match page.kind {
            Kind::Licence => "LICENCE",
            Kind::Notice => "NOTICE",
            Kind::Terms => "TERMS",
        };
        text.push_str(&format!(
            "\n=== {kind} ({} of {total}): {} ===\n",
            index + 1,
            page.title
        ));
        if let Some(spdx) = &page.spdx {
            text.push_str(&format!("Licence: {spdx}\n"));
        }
        text.push_str(&format!("\nWHAT THIS IS\n{}\n", page.explains));
        text.push_str(&format!("\nWHAT CHANGES FOR YOU\n{}\n", page.changes));
        for link in &page.links {
            text.push_str(&format!(
                "\nRead the full text: {} <{}>\n",
                link.label, link.url
            ));
        }
        for block in &page.body {
            match block {
                Block::Heading { text: heading } => text.push_str(&format!("\n{heading}\n")),
                Block::Paragraph {
                    text: paragraph, ..
                } => text.push_str(&format!("\n{paragraph}\n")),
                Block::List { items, ordered } => {
                    text.push('\n');
                    for (number, item) in items.iter().enumerate() {
                        if *ordered {
                            text.push_str(&format!("  {}. {item}\n", number + 1));
                        } else {
                            text.push_str(&format!("  - {item}\n"));
                        }
                    }
                }
                Block::Callout { text: callout, .. } => text.push_str(&format!("\n! {callout}\n")),
                Block::Table { header, rows } => {
                    text.push_str(&format!("\n{}\n", header.join(" | ")));
                    for row in rows {
                        text.push_str(&format!("{}\n", row.join(" | ")));
                    }
                }
                Block::Link { label, url } => text.push_str(&format!("\n{label} <{url}>\n")),
                Block::Divider => text.push_str("\n----\n"),
            }
        }
    }
    text
}

/// The warning for a package that did not come through the store, in the words of the window's own (see `agreement_overlay` in the interface).
pub const OUTSIDE_STORE_WARNING: &str = "THIS EXTENSION DID NOT COME THROUGH THE STORE.\nNobody at Peersus has reviewed it, signed it or promised that it is safe. It runs with your access on this device and can read and send whatever you let it reach.\nThe Peersus project cannot take on the risk of an extension from outside its store, and is not responsible for what it does to you, your files or your devices. The risk is yours.\nPrefer the store: what is listed there was reviewed before it was offered. If you do not know who made this package, do not install it.";

/// Whether `accepted` (what the ledger holds for an extension, if anything) is the agreement `manifest` carries now. A
/// manifest without an agreement is never accepted.
#[must_use]
pub fn is_accepted(accepted: Option<&String>, manifest: &Manifest) -> bool {
    manifest.agreements.as_ref().is_some_and(|agreements| {
        accepted.is_some_and(|digest| *digest == peersus_core::agreements::digest(agreements))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use peersus_api::agreement::{Agreements, Kind, Link, Page};

    fn manifest(word: &str) -> Manifest {
        Manifest {
            agreements: Some(Agreements {
                pages: vec![
                    Page {
                        kind: Kind::Licence,
                        title: "MIT".into(),
                        explains: word.into(),
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
                        changes: "A page appears.".into(),
                        spdx: None,
                        links: vec![],
                        body: vec![],
                    },
                ],
            }),
            ..Manifest::default()
        }
    }

    fn digest_of(manifest: &Manifest) -> String {
        peersus_core::agreements::digest(manifest.agreements.as_ref().unwrap())
    }

    #[test]
    fn what_was_accepted_is_kept_per_extension_and_only_that_agreement_counts() {
        let directory = tempfile::tempdir().unwrap();
        let first = manifest("You may use it.");
        assert!(load(directory.path()).is_empty());
        accept(directory.path(), "echo", &digest_of(&first), 100).unwrap();
        let kept = load(directory.path());
        assert!(is_accepted(kept.get("echo"), &first));
        // Another extension has accepted nothing, and one word changed is another agreement.
        assert!(!is_accepted(kept.get("other"), &first));
        assert!(!is_accepted(kept.get("echo"), &manifest("You may use it!")));
        // Without an agreement nothing is accepted, whatever is recorded.
        assert!(!is_accepted(kept.get("echo"), &Manifest::default()));
        // Accepting again replaces; forgetting removes, and forgetting nothing is not an error.
        let changed = manifest("You may use it!");
        accept(directory.path(), "echo", &digest_of(&changed), 200).unwrap();
        assert!(is_accepted(load(directory.path()).get("echo"), &changed));
        forget(directory.path(), "echo").unwrap();
        forget(directory.path(), "echo").unwrap();
        assert!(load(directory.path()).is_empty());
    }

    #[test]
    fn a_damaged_or_hostile_file_reads_as_nothing_accepted() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(FILE);
        let good = digest_of(&manifest("x"));
        for text in [
            "not json".to_owned(),
            r#"{"accepted":{"echo":{"sha256":"short","accepted_at":1}}}"#.to_owned(),
            format!(r#"{{"accepted":{{"../escape":{{"sha256":"{good}","accepted_at":1}}}}}}"#),
            format!(r#"{{"accepted":{{"echo":{{"sha256":"{good}","accepted_at":1,"extra":1}}}}}}"#),
            "x".repeat(MAX_BYTES as usize + 1),
        ] {
            fs::write(&path, text).unwrap();
            assert!(load(directory.path()).is_empty());
        }
        // A record that is plainly a link is not read through it.
        #[cfg(unix)]
        {
            let elsewhere = directory.path().join("elsewhere");
            fs::write(
                &elsewhere,
                format!(r#"{{"accepted":{{"echo":{{"sha256":"{good}","accepted_at":1}}}}}}"#),
            )
            .unwrap();
            fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(&elsewhere, &path).unwrap();
            assert!(load(directory.path()).is_empty());
        }
    }

    #[test]
    fn a_name_that_is_not_one_and_a_digest_that_is_not_one_are_refused() {
        let directory = tempfile::tempdir().unwrap();
        let good = digest_of(&manifest("x"));
        assert!(accept(directory.path(), "../x", &good, 1).is_err());
        assert!(accept(directory.path(), "echo", "abc", 1).is_err());
        assert!(accept(directory.path(), "echo", &"g".repeat(64), 1).is_err());
        assert!(!directory.path().join(FILE).exists());
    }

    #[test]
    fn the_ledger_holds_no_more_than_the_application_will() {
        let directory = tempfile::tempdir().unwrap();
        let good = digest_of(&manifest("x"));
        for number in 0..MAX_ENTRIES {
            accept(directory.path(), &format!("ext{number}"), &good, 1).unwrap();
        }
        assert!(accept(directory.path(), "one-more", &good, 1).is_err());
        // One already there can still be replaced.
        accept(directory.path(), "ext0", &good, 2).unwrap();
        assert_eq!(load(directory.path()).len(), MAX_ENTRIES);
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_the_owners_alone() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        accept(directory.path(), "echo", &digest_of(&manifest("x")), 1).unwrap();
        let mode = fs::metadata(directory.path().join(FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
