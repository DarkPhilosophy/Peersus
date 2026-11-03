//! What the real signer (`cargo xtask catalog-sign`) produced, checked by the application's verifier.
//! The fixture is public data signed with a throwaway test seed; it carries no project key.
use ed25519_dalek::SigningKey;
use peersus_app::{binary_metadata, catalog};

const CATALOG: &[u8] = include_bytes!("fixtures/catalog/catalog.json");
const SIGNATURE: &[u8] = include_bytes!("fixtures/catalog/catalog.sig");
const ARCHIVE: &[u8] = include_bytes!("fixtures/catalog/echo.tar.gz");

fn key() -> ed25519_dalek::VerifyingKey {
    SigningKey::from_bytes(&[7u8; 32]).verifying_key()
}

#[test]
fn the_signers_output_is_accepted_and_pins_the_archive() {
    let catalog = catalog::verify(&key(), CATALOG, SIGNATURE, 0).expect("signed by the test seed");
    let entry = catalog.find("echo").expect("the approved entry");
    assert_eq!(entry.version, "1.0.0");
    assert_eq!(entry.author, "Someone");
    catalog::archive_matches(entry, ARCHIVE).expect("the approved archive");
}

#[test]
fn a_changed_archive_or_catalog_or_key_is_refused() {
    let catalog = catalog::verify(&key(), CATALOG, SIGNATURE, 0).unwrap();
    let entry = catalog.find("echo").unwrap();
    let mut altered = ARCHIVE.to_vec();
    altered[40] ^= 1;
    assert!(catalog::archive_matches(entry, &altered).is_err(), "one flipped archive byte");
    let mut edited = CATALOG.to_vec();
    let at = edited.windows(7).position(|w| w == b"Someone").unwrap();
    edited[at] = b'X';
    assert!(catalog::verify(&key(), &edited, SIGNATURE, 0).is_err(), "an edited catalog");
    let other = SigningKey::from_bytes(&[9u8; 32]).verifying_key();
    assert!(catalog::verify(&other, CATALOG, SIGNATURE, 0).is_err(), "a different key");
    assert!(catalog::verify(&key(), CATALOG, SIGNATURE, 2).is_err(), "a rollback below sequence 2");
}

#[test]
fn the_approved_package_binds_to_its_catalog_entry_through_both_metadata_sources() {
    use std::io::Read;
    let mut files = std::collections::BTreeMap::new();
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(ARCHIVE));
    for member in archive.entries().unwrap() {
        let mut member = member.unwrap();
        let name = member.path().unwrap().to_string_lossy().into_owned();
        let mut bytes = Vec::new();
        member.read_to_end(&mut bytes).unwrap();
        files.insert(name, bytes);
    }
    let manifest = peersus_plugin_api::package::split(&files["echo/plugin.json"]).unwrap().package.unwrap();
    let embedded = binary_metadata::read(&files["echo/echo.so"]).expect("embedded record in the compiled .so");
    let catalog = catalog::verify(&key(), CATALOG, SIGNATURE, 0).unwrap();
    let entry = catalog.find("echo").unwrap();
    catalog::binds_package(entry, "echo", &manifest, &embedded).expect("catalog, manifest and binary agree");
    assert!(catalog::binds_package(entry, "other", &manifest, &embedded).is_err(), "a different install id");
}
