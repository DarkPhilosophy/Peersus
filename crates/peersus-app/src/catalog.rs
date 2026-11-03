//! The maintainer-approved extension catalog, and what it proves about a package.
//!
//! THE TRUST ROOT is one Ed25519 public key compiled into the application. The catalog is a JSON
//! document signed by the maintainer's offline key; this module only VERIFIES. It holds no secret
//! and cannot sign. A catalog entry pins, for one approved package: the plugin's name (which is the
//! package folder name and so the plugin id the host assigns), its version, the SHA-256 of the package
//! archive, and the author and link a person is shown.
//!
//! WHAT A VERIFIED ENTRY MEANS. That the maintainer approved exactly these archive bytes under this
//! name and version. It does NOT mean the plugin is harmless: approval is a human decision about
//! identity and provenance, and a plugin still starts with nothing allowed and is only as safe as
//! the process boundary it runs behind. Installing from the store feeds the verified bytes into the
//! SAME staging, limits, digest and confirmation path as a manual install, so no check is duplicated
//! and none is skipped.
//!
//! ROLLBACK. `sequence` only ever rises: a catalog older than the newest one seen is refused, and an
//! entry that is older than the installed version is never offered as an update.
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use peersus_plugin_api::{
    name,
    package::{Package, Version, public_link},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Largest catalog accepted, in bytes.
pub const MAX_CATALOG_BYTES: usize = 1024 * 1024;
/// Most entries one catalog may list.
pub const MAX_ENTRIES: usize = 512;
/// Largest archive a catalog may pin. Kept here, not borrowed from `plugin_manager`, because that
/// module only exists on unix and this one must compile everywhere; a unix test ties the two so
/// they cannot drift apart.
pub const MAX_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;

/// One approved package. Strict: an unknown field is refused, so nothing can be smuggled in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// The plugin's name: the package folder name, and so the id the host will assign.
    pub name: String,
    pub version: String,
    /// Lower-case hex SHA-256 of the archive file.
    pub sha256: String,
    pub size: u64,
    pub author: String,
    pub link: String,
    #[serde(default)]
    pub description: String,
    /// File name of the archive on the host that serves it. Never a path.
    pub archive: String,
}

/// The signed document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub schema: u32,
    /// Rises with every published catalog; protects against being served an older, valid one.
    pub sequence: u64,
    pub entries: Vec<Entry>,
}

/// Why a catalog or an archive was refused, in words this application chose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refusal(pub &'static str);

impl Entry {
    fn valid(&self) -> bool {
        name::valid(&self.name)
            && !self.name.starts_with('.')
            && Version::parse(&self.version).is_some()
            && self.sha256.len() == 64
            && self.sha256.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            && (1..=MAX_ARCHIVE_BYTES).contains(&self.size)
            && name::text(&self.author, peersus_plugin_api::package::MAX_NAME_CHARS)
            && public_link(&self.link)
            && (self.description.is_empty()
                || name::text(&self.description, peersus_plugin_api::package::MAX_DESCRIPTION_CHARS))
            && plain_file_name(&self.archive)
    }
}

fn plain_file_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.starts_with('.')
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

impl Catalog {
    /// Check the structure of an already-authenticated catalog.
    fn validate(&self) -> Result<(), Refusal> {
        if self.schema != 1 {
            return Err(Refusal("The catalog is a version this application does not read"));
        }
        if self.entries.len() > MAX_ENTRIES {
            return Err(Refusal("The catalog lists too many extensions"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for entry in &self.entries {
            if !entry.valid() {
                return Err(Refusal("The catalog has an entry that is not valid"));
            }
            if !seen.insert(entry.name.as_str()) {
                return Err(Refusal("The catalog lists a name twice"));
            }
        }
        Ok(())
    }

    /// The entry for `name`, if the maintainer approved one.
    pub fn find(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.name == name)
    }
}

/// Verify `signature` over exactly `catalog_bytes` with `key`, then read it. The signature is checked
/// BEFORE anything is parsed, so an unauthenticated byte never reaches the JSON reader.
///
/// `newest_seen` is the highest `sequence` this installation has accepted; a catalog that does not
/// beat it is refused (an equal one is a harmless repeat and is accepted).
pub fn verify(
    key: &VerifyingKey,
    catalog_bytes: &[u8],
    signature: &[u8],
    newest_seen: u64,
) -> Result<Catalog, Refusal> {
    if catalog_bytes.len() > MAX_CATALOG_BYTES {
        return Err(Refusal("The catalog is too large"));
    }
    let signature: [u8; 64] = signature.try_into().map_err(|_| Refusal("The catalog signature is not valid"))?;
    key.verify(catalog_bytes, &Signature::from_bytes(&signature))
        .map_err(|_| Refusal("The catalog is not signed by the maintainer"))?;
    let catalog: Catalog =
        serde_json::from_slice(catalog_bytes).map_err(|_| Refusal("The catalog could not be read"))?;
    catalog.validate()?;
    if catalog.sequence < newest_seen {
        return Err(Refusal("The catalog is older than one already seen"));
    }
    Ok(catalog)
}

/// Whether `archive` is exactly the package `entry` approved: size and SHA-256, compared in
/// constant length. Checked before the archive is unpacked or looked into at all.
pub fn archive_matches(entry: &Entry, archive: &[u8]) -> Result<(), Refusal> {
    if archive.len() as u64 != entry.size {
        return Err(Refusal("The package is not the one that was approved"));
    }
    let digest = hex::encode(Sha256::digest(archive));
    if digest == entry.sha256 {
        Ok(())
    } else {
        Err(Refusal("The package is not the one that was approved"))
    }
}

/// What the catalog entry says must also be what the package says about itself, in the manifest AND
/// in the program. `plugin_id` is the folder name the host will assign; it must equal the entry's name
/// so a catalog entry cannot install under an id other than the one that was reviewed.
pub fn binds_package(
    entry: &Entry,
    plugin_id: &str,
    manifest: &Package,
    embedded: &Package,
) -> Result<(), Refusal> {
    if plugin_id != entry.name {
        return Err(Refusal("The package name is not the approved name"));
    }
    for claimed in [manifest, embedded] {
        if claimed.version != entry.version || claimed.author != entry.author || claimed.link != entry.link {
            return Err(Refusal("The package does not say what the catalog says about it"));
        }
    }
    if !crate::binary_metadata::agrees(embedded, manifest) {
        return Err(Refusal("The program and its manifest disagree about who made it"));
    }
    Ok(())
}

/// Whether `entry` is an update for something installed at `installed_version`. Only strictly newer.
pub fn is_update(entry: &Entry, installed_version: &str) -> bool {
    matches!(
        (Version::parse(&entry.version), Version::parse(installed_version)),
        (Some(offered), Some(have)) if offered > have
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    // A throwaway key made in the test. The test needs `rand_core`-free construction, so a fixed
    // seed is used; it is not, and never becomes, a project key.
    fn key() -> (SigningKey, VerifyingKey) {
        let signing = SigningKey::from_bytes(&[7u8; 32]);
        let verifying = signing.verifying_key();
        (signing, verifying)
    }

    fn entry(archive: &[u8]) -> Entry {
        Entry {
            name: "echo".into(),
            version: "1.2.0".into(),
            sha256: hex::encode(Sha256::digest(archive)),
            size: archive.len() as u64,
            author: "Someone".into(),
            link: "https://example.org/echo".into(),
            description: String::new(),
            archive: "echo-1.2.0.tar.gz".into(),
        }
    }

    fn catalog(sequence: u64, entries: Vec<Entry>) -> Vec<u8> {
        serde_json::to_vec(&Catalog { schema: 1, sequence, entries }).unwrap()
    }

    fn package(version: &str, author: &str, link: &str) -> Package {
        Package {
            name: "Echo".into(),
            version: version.into(),
            description: String::new(),
            changelog: String::new(),
            author: author.into(),
            link: link.into(),
        }
    }

    #[test]
    fn a_catalog_signed_by_the_maintainer_is_accepted() {
        let (signing, verifying) = key();
        let bytes = catalog(5, vec![entry(b"archive")]);
        let signature = signing.sign(&bytes).to_bytes();
        let read = verify(&verifying, &bytes, &signature, 5).unwrap();
        assert_eq!(read.find("echo").unwrap().version, "1.2.0");
    }

    #[test]
    fn a_catalog_that_was_changed_after_signing_is_refused_before_it_is_parsed() {
        let (signing, verifying) = key();
        let bytes = catalog(5, vec![entry(b"archive")]);
        let signature = signing.sign(&bytes).to_bytes();
        let mut tampered = bytes.clone();
        let at = tampered.windows(7).position(|w| w == b"Someone").unwrap();
        tampered[at] = b'X';
        assert_eq!(
            verify(&verifying, &tampered, &signature, 0),
            Err(Refusal("The catalog is not signed by the maintainer"))
        );
        // Garbage that is not even JSON is refused for the signature, not parsed.
        assert_eq!(
            verify(&verifying, b"not json", &signature, 0),
            Err(Refusal("The catalog is not signed by the maintainer"))
        );
    }

    #[test]
    fn a_catalog_signed_by_someone_else_is_refused() {
        let (_, verifying) = key();
        let other = SigningKey::from_bytes(&[9u8; 32]);
        let bytes = catalog(1, vec![entry(b"a")]);
        let signature = other.sign(&bytes).to_bytes();
        assert!(verify(&verifying, &bytes, &signature, 0).is_err());
        assert!(verify(&verifying, &bytes, &[0u8; 10], 0).is_err(), "a short signature");
    }

    #[test]
    fn an_older_catalog_is_a_rollback_and_is_refused() {
        let (signing, verifying) = key();
        let bytes = catalog(4, vec![entry(b"a")]);
        let signature = signing.sign(&bytes).to_bytes();
        assert_eq!(
            verify(&verifying, &bytes, &signature, 5),
            Err(Refusal("The catalog is older than one already seen"))
        );
        assert!(verify(&verifying, &bytes, &signature, 4).is_ok());
    }

    #[test]
    fn a_signed_but_malformed_catalog_is_still_refused() {
        let (signing, verifying) = key();
        let mut bad = entry(b"a");
        bad.link = "http://example.org".into();
        let mut dotted = entry(b"a");
        dotted.name = ".hidden".into();
        let mut path = entry(b"a");
        path.archive = "../escape.tar.gz".into();
        let mut uppercase = entry(b"a");
        uppercase.sha256 = uppercase.sha256.to_uppercase();
        for (what, entries) in [
            ("insecure link", vec![bad]),
            ("dot-leading name", vec![dotted]),
            ("path in archive name", vec![path]),
            ("non-canonical digest", vec![uppercase]),
            ("duplicate name", vec![entry(b"a"), entry(b"a")]),
        ] {
            let bytes = catalog(1, entries);
            let signature = signing.sign(&bytes).to_bytes();
            assert!(verify(&verifying, &bytes, &signature, 0).is_err(), "{what} was accepted");
        }
        let unknown = br#"{"schema":1,"sequence":1,"entries":[],"extra":true}"#;
        assert!(verify(&verifying, unknown, &signing.sign(unknown).to_bytes(), 0).is_err());
    }

    #[test]
    fn only_the_exact_approved_archive_passes() {
        let approved = entry(b"the real archive");
        assert!(archive_matches(&approved, b"the real archive").is_ok());
        assert!(archive_matches(&approved, b"the real archivE").is_err(), "one byte differs");
        assert!(archive_matches(&approved, b"the real archive!").is_err(), "longer");
        assert!(archive_matches(&approved, b"").is_err(), "empty");
    }

    #[test]
    fn the_package_must_say_what_the_catalog_says_in_both_places() {
        let approved = entry(b"a");
        let ok = package("1.2.0", "Someone", "https://example.org/echo");
        assert!(binds_package(&approved, "echo", &ok, &ok).is_ok());
        // Installing under a different id than the one reviewed.
        assert!(binds_package(&approved, "other", &ok, &ok).is_err());
        // Either claim diverging from the catalog is enough to refuse.
        let author = package("1.2.0", "Impostor", "https://example.org/echo");
        assert!(binds_package(&approved, "echo", &author, &ok).is_err(), "manifest author");
        assert!(binds_package(&approved, "echo", &ok, &author).is_err(), "embedded author");
        let version = package("1.2.1", "Someone", "https://example.org/echo");
        assert!(binds_package(&approved, "echo", &version, &version).is_err(), "version");
        let link = package("1.2.0", "Someone", "https://example.org/else");
        assert!(binds_package(&approved, "echo", &ok, &link).is_err(), "embedded link");
    }

    #[cfg(unix)]
    #[test]
    fn the_catalog_bound_is_the_package_bound() {
        assert_eq!(MAX_ARCHIVE_BYTES, crate::plugin_manager::MAX_BYTES);
    }

    #[test]
    fn only_a_strictly_newer_entry_is_an_update() {
        let approved = entry(b"a");
        assert!(is_update(&approved, "1.1.9"));
        assert!(!is_update(&approved, "1.2.0"), "the same version");
        assert!(!is_update(&approved, "1.3.0"), "a downgrade");
        assert!(!is_update(&approved, "garbage"), "an installed version that is not one");
    }
}
