//! The store as the owner keeps it: trust root, rollback floor, the last VERIFIED catalog, and what the
//! window is told. The service never fetches and never awaits: the caller downloads between a `begin_*`
//! and its `finish_*` (in production the separate `peersus-store-fetch` helper, off the event loop), so
//! the whole trust flow is a plain synchronous state machine, testable without a network.
//!
//! Only verified data is ever held: `catalog` is `None` until a signature has checked out, and the
//! window's listings are copied from it, never from anything a download said.
use crate::{
    archive::{self, Unpacked},
    catalog::{self, Catalog, Entry, Refusal},
    store,
    store_trust::{self, Floor},
};
use ed25519_dalek::VerifyingKey;
use peersus_core::{
    model::{StoreListing, StoreState, StoreStatus},
    plugins::PluginId,
};
use peersus_plugin_api::package::Package;
use std::path::Path;

/// A package that is exactly what the maintainer approved, unpacked, and bound to its catalog entry
/// through the manifest AND the program. Ready to go through the same install as a manual one.
pub struct Verified {
    pub unpacked: Unpacked,
    pub entry: Entry,
}

/// What to download for an install, decided only by the verified catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fetch {
    pub archive: String,
    pub limit: usize,
}

/// What operation is running, so a second one is refused instead of racing the first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Busy {
    Refresh,
    Install,
}

pub struct StoreService {
    key: Result<VerifyingKey, store_trust::Unavailable>,
    floor: Floor,
    catalog: Option<Catalog>,
    state: StoreState,
    busy: Option<Busy>,
}

impl StoreService {
    /// `candidate` is the compiled-in key (production passes `store_trust::MAINTAINER_PUBLIC_KEY`).
    pub fn new(candidate: Option<[u8; 32]>, data: &Path) -> Self {
        let key = store_trust::trust_root(candidate);
        let status = if key.is_ok() { StoreStatus::Idle } else { StoreStatus::NotConfigured };
        Self { key, floor: Floor::new(data), catalog: None, state: StoreState { status, ..StoreState::default() }, busy: None }
    }

    /// What the window is told. Copied into every snapshot, so it never blanks on an unrelated update.
    pub fn state(&self) -> &StoreState {
        &self.state
    }

    fn fail(&mut self, words: &'static str) -> Refusal {
        // A failed step keeps the last verified listing visible: it is still what was approved.
        self.busy = None;
        self.state.status = StoreStatus::Failed;
        self.state.error = Some(words.to_owned());
        Refusal(words)
    }

    fn claim(&mut self, what: Busy) -> Result<(), Refusal> {
        if let Err(unavailable) = &self.key {
            return Err(Refusal(unavailable.0));
        }
        if self.busy.is_some() {
            return Err(Refusal("The store is busy; wait for it to finish"));
        }
        self.busy = Some(what);
        Ok(())
    }

    /// Start a refresh: the caller fetches [`store::CATALOG_FILE`] and [`store::SIGNATURE_FILE`] and
    /// then calls [`Self::finish_refresh`]. Refused when not configured or already busy.
    pub fn begin_refresh(&mut self) -> Result<(), Refusal> {
        self.claim(Busy::Refresh)?;
        self.state.status = StoreStatus::Loading;
        self.state.error = None;
        Ok(())
    }

    /// Verify and adopt what was downloaded. The floor is raised only after the signature checked out.
    pub fn finish_refresh(&mut self, catalog_bytes: &[u8], signature: &[u8]) -> Result<(), Refusal> {
        let Ok(key) = self.key else { return Err(Refusal("The extension store is not set up in this build")) };
        let floor = self.floor.read().map_err(|unavailable| self.fail(unavailable.0))?;
        let catalog = catalog::verify(&key, catalog_bytes, signature, floor).map_err(|refusal| self.fail(refusal.0))?;
        self.floor.raise(catalog.sequence).map_err(|unavailable| self.fail(unavailable.0))?;
        self.state = StoreState {
            status: StoreStatus::Ready,
            listings: store::search(&catalog, "").into_iter().filter_map(listing).collect(),
            error: None,
        };
        self.catalog = Some(catalog);
        self.busy = None;
        Ok(())
    }

    /// Start an install of `plugin`: what to download is the SIGNED entry's, never anything supplied by
    /// the caller. Refused before anything is fetched for a name the maintainer did not approve.
    pub fn begin_install(&mut self, plugin: &PluginId) -> Result<Fetch, Refusal> {
        let Some(entry) = self.catalog.as_ref().and_then(|catalog| catalog.find(plugin.as_str())) else {
            return Err(Refusal("The extension is not in the approved catalog"));
        };
        let fetch = Fetch {
            archive: entry.archive.clone(),
            limit: usize::try_from(entry.size).map_err(|_| Refusal("The package is too large"))?,
        };
        self.claim(Busy::Install)?;
        self.state.status = StoreStatus::Loading;
        self.state.error = None;
        Ok(fetch)
    }

    /// Check what was downloaded against the signed entry, unpack it, and bind catalog, manifest and
    /// program together. The result is ready for the ordinary install.
    pub fn finish_install(&mut self, plugin: &PluginId, bytes: Vec<u8>, scratch: &Path) -> Result<Verified, Refusal> {
        let Some(entry) = self.catalog.as_ref().and_then(|catalog| catalog.find(plugin.as_str())).cloned() else {
            return Err(self.fail("The extension is not in the approved catalog"));
        };
        if let Err(refusal) = catalog::archive_matches(&entry, &bytes) {
            return Err(self.fail(refusal.0));
        }
        // Kept under a name chosen HERE, never one from a URL: a flat archive takes its id from it.
        let file_name = format!("{}.tar.gz", entry.name);
        match unpack_and_bind(&entry, &file_name, bytes, scratch) {
            Ok(unpacked) => Ok(Verified { unpacked, entry }),
            Err(refusal) => Err(self.fail(refusal.0)),
        }
    }

    /// The install went through (or was given up): the store is ready again.
    pub fn done(&mut self) {
        self.busy = None;
        if self.catalog.is_some() {
            self.state.status = StoreStatus::Ready;
            self.state.error = None;
        }
    }

    /// An install the runtime started could not complete after verification (for example the real
    /// install refused it). Recorded in the store's words, and the store is usable again.
    pub fn failed(&mut self, words: &'static str) {
        self.fail(words);
    }

    /// Tell the store which version is installed under each name, as the owner read it from the installed
    /// folders. Only versions are taken, and only for names the catalog lists: it never adds a listing.
    pub fn set_installed(&mut self, installed: &std::collections::BTreeMap<PluginId, String>) {
        for listing in &mut self.state.listings {
            listing.installed_version = installed.get(&listing.plugin).cloned();
        }
    }
}

fn listing(item: store::Listing) -> Option<StoreListing> {
    Some(StoreListing {
        plugin: PluginId::new(item.name).ok()?,
        version: item.version,
        author: item.author,
        link: item.link,
        description: item.description,
        installed_version: None,
    })
}

/// Keep the verified bytes under `<name>.tar.gz`, unpack with the same hardened unpacker a manual
/// archive uses, and bind catalog, manifest and program together.
fn unpack_and_bind(entry: &Entry, file_name: &str, bytes: Vec<u8>, scratch: &Path) -> Result<Unpacked, Refusal> {
    let source = archive::keep_source(bytes.as_slice(), file_name, scratch).map_err(|r| Refusal(r.0))?;
    let unpacked = archive::unpack_in(source.path(), scratch).map_err(|r| Refusal(r.0))?;
    bind(entry, &unpacked)?;
    Ok(unpacked)
}

/// The package folder name is the id the host assigns; the manifest's package block and the program's
/// embedded record must both equal what the signed entry says.
fn bind(entry: &Entry, unpacked: &Unpacked) -> Result<(), Refusal> {
    const BAD: Refusal = Refusal("The package could not be checked");
    let root = unpacked.root();
    let id = root.file_name().and_then(|n| n.to_str()).ok_or(BAD)?;
    let (manifest, package) = crate::discovery::read_package_and_info(root).map_err(|_| BAD)?;
    let package: Package = package.ok_or(Refusal("The package does not say who made it"))?;
    // The store distributes only formats that run in the separate, isolated module helper. A legacy
    // `process` package runs directly with the user's own rights; it can still be installed by hand for
    // development, with its warning, but it is never what the maintainer's store offers.
    if matches!(manifest.execution, peersus_plugin_api::manifest::Execution::Process) {
        return Err(Refusal("The store only distributes extensions that run in the isolated module host"));
    }
    let program = std::fs::read(root.join(manifest.program_file())).map_err(|_| BAD)?;
    let embedded = crate::binary_metadata::read(&program).map_err(|absent| Refusal(absent.words()))?;
    catalog::binds_package(entry, id, &package, &embedded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    // Produced by the real `cargo xtask catalog-sign` from a package validated by the CI validator.
    // Signed with the throwaway test seed [7; 32]; public data, no project key.
    const CATALOG: &[u8] = include_bytes!("../tests/fixtures/catalog/catalog.json");
    const SIGNATURE: &[u8] = include_bytes!("../tests/fixtures/catalog/catalog.sig");
    const ARCHIVE: &[u8] = include_bytes!("../tests/fixtures/catalog/echo.tar.gz");

    fn test_key() -> [u8; 32] {
        SigningKey::from_bytes(&[7u8; 32]).verifying_key().to_bytes()
    }

    fn echo() -> PluginId {
        PluginId::new("echo").unwrap()
    }

    fn ready(dir: &Path) -> StoreService {
        let mut service = StoreService::new(Some(test_key()), dir);
        service.begin_refresh().unwrap();
        service.finish_refresh(CATALOG, SIGNATURE).unwrap();
        service
    }

    #[test]
    fn without_a_key_nothing_can_start_and_the_state_stays_not_configured() {
        let dir = tempfile::tempdir().unwrap();
        let mut service = StoreService::new(None, dir.path());
        assert_eq!(service.state().status, StoreStatus::NotConfigured);
        assert!(service.begin_refresh().is_err());
        assert!(service.begin_install(&echo()).is_err());
        assert_eq!(service.state().status, StoreStatus::NotConfigured, "a refusal must not read as 'failed' and invite a retry");
        assert!(service.finish_refresh(CATALOG, SIGNATURE).is_err(), "even a validly signed catalog is not trusted without a key");
    }

    #[test]
    fn a_signed_catalog_becomes_the_listing_and_a_second_operation_is_refused_while_busy() {
        let dir = tempfile::tempdir().unwrap();
        let mut service = StoreService::new(Some(test_key()), dir.path());
        service.begin_refresh().unwrap();
        assert_eq!(service.state().status, StoreStatus::Loading);
        assert!(service.begin_refresh().is_err(), "two fetches would race the floor");
        service.finish_refresh(CATALOG, SIGNATURE).unwrap();
        let state = service.state();
        assert_eq!(state.status, StoreStatus::Ready);
        assert_eq!(state.listings.len(), 1);
        assert_eq!(state.listings[0].plugin.as_str(), "echo");
        assert_eq!((state.listings[0].author.as_str(), state.listings[0].link.as_str()), ("Someone", "https://example.org/echo"));
        service.begin_refresh().expect("free again after a finished refresh");
    }

    #[test]
    fn a_catalog_that_does_not_verify_is_never_adopted_and_does_not_raise_the_floor() {
        let dir = tempfile::tempdir().unwrap();
        let mut service = StoreService::new(Some(test_key()), dir.path());
        service.begin_refresh().unwrap();
        let mut edited = CATALOG.to_vec();
        edited[20] ^= 1;
        assert!(service.finish_refresh(&edited, SIGNATURE).is_err());
        assert_eq!(service.state().status, StoreStatus::Failed);
        assert!(service.state().listings.is_empty());
        assert_eq!(Floor::new(dir.path()).read(), Ok(0), "nothing unverified may move the floor");
        service.begin_refresh().expect("a failure releases the store so the person can retry");
    }

    #[test]
    fn an_older_catalog_is_refused_after_a_restart_because_the_floor_was_saved() {
        let dir = tempfile::tempdir().unwrap();
        ready(dir.path());
        Floor::new(dir.path()).raise(9).unwrap();
        let mut restarted = StoreService::new(Some(test_key()), dir.path());
        restarted.begin_refresh().unwrap();
        assert!(restarted.finish_refresh(CATALOG, SIGNATURE).is_err(), "sequence 1 < the saved 9");
    }

    #[test]
    fn only_an_approved_name_can_start_an_install_and_nothing_else_is_ever_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let mut service = ready(dir.path());
        for stranger in ["unlisted", "../../etc/passwd", "ECHO"] {
            if let Ok(id) = PluginId::new(stranger) {
                assert!(service.begin_install(&id).is_err(), "{stranger}");
            }
        }
        let fetch = service.begin_install(&echo()).unwrap();
        assert_eq!(fetch.archive, "echo.tar.gz", "the file name is the signed entry's");
        assert_eq!(fetch.limit, ARCHIVE.len());
        assert!(service.begin_install(&echo()).is_err(), "a second install of the same name while one runs");
    }

    #[test]
    fn the_approved_package_unpacks_and_binds_through_the_real_manifest_and_elf_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let mut service = ready(dir.path());
        service.begin_install(&echo()).unwrap();
        let verified = service.finish_install(&echo(), ARCHIVE.to_vec(), dir.path()).expect("approved bytes");
        assert_eq!(verified.entry.name, "echo");
        assert!(verified.unpacked.root().join("plugin.json").is_file());
        service.done();
        assert_eq!(service.state().status, StoreStatus::Ready);
    }

    #[test]
    fn changed_bytes_are_refused_before_anything_is_unpacked() {
        let dir = tempfile::tempdir().unwrap();
        let mut service = ready(dir.path());
        service.begin_install(&echo()).unwrap();
        let mut altered = ARCHIVE.to_vec();
        altered[40] ^= 1;
        let before = std::fs::read_dir(dir.path()).unwrap().count();
        assert!(service.finish_install(&echo(), altered, dir.path()).is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), before, "nothing was unpacked or kept");
        assert_eq!(service.state().status, StoreStatus::Failed);
        service.begin_install(&echo()).expect("released after the refusal");
    }

    /// The SHA-256 pin on its own. A corrupted archive is refused by the unpacker and a longer one by the size
    /// cap, so neither shows that the digest is checked. This one is the approved package plus trailing zero
    /// bytes, which a gzip reader ignores: well-formed, unpacks to the very same package, and of EXACTLY the
    /// size the signed entry pins. Only a different digest can refuse it.
    #[test]
    fn a_well_formed_archive_of_the_right_size_is_refused_by_its_digest_alone() {
        use ed25519_dalek::Signer;
        use sha2::{Digest, Sha256};
        let dir = tempfile::tempdir().unwrap();
        let mut padded = ARCHIVE.to_vec();
        padded.extend([0u8; 64]);
        // Sign a catalog that pins the REAL archive's digest but the padded archive's size.
        let entry = |c: &Catalog| c.entries[0].clone();
        let real: Catalog = serde_json::from_slice(CATALOG).unwrap();
        let mut pinned = entry(&real);
        pinned.size = padded.len() as u64;
        assert_eq!(pinned.sha256, hex::encode(Sha256::digest(ARCHIVE)), "the fixture pins the real archive");
        let catalog = Catalog { schema: 1, sequence: 1, entries: vec![pinned] };
        let body = serde_json::to_vec(&catalog).unwrap();
        let signature = SigningKey::from_bytes(&[7u8; 32]).sign(&body).to_bytes();
        let mut service = StoreService::new(Some(test_key()), dir.path());
        service.begin_refresh().unwrap();
        service.finish_refresh(&body, &signature).unwrap();
        service.begin_install(&echo()).unwrap();
        // Control: it really is accepted by everything but the digest, so this cannot pass for another reason.
        let source = archive::keep_source(padded.as_slice(), "echo.tar.gz", dir.path()).unwrap();
        archive::unpack_in(source.path(), dir.path()).expect("the padded archive unpacks");
        let before = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(
            service.finish_install(&echo(), padded, dir.path()).err(),
            Some(Refusal("The package is not the one that was approved"))
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), before, "nothing was unpacked or kept");
    }

    /// The same isolation by the simplest means: the gzip header's MTIME (bytes 4..8) is ignored by readers and
    /// outside any header CRC, so flipping it gives the SAME length and the SAME unpacked package with a
    /// different digest. Nothing but the SHA-256 pin can refuse it.
    #[test]
    fn an_archive_that_differs_only_in_its_gzip_timestamp_is_refused_by_its_digest_alone() {
        let dir = tempfile::tempdir().unwrap();
        let mut service = ready(dir.path());
        service.begin_install(&echo()).unwrap();
        let mut altered = ARCHIVE.to_vec();
        altered[4] ^= 1;
        assert_ne!(altered, ARCHIVE);
        assert_eq!(altered.len(), ARCHIVE.len());
        // Control: it unpacks, so no layer but the digest can be what refuses it.
        let source = archive::keep_source(altered.as_slice(), "echo.tar.gz", dir.path()).unwrap();
        archive::unpack_in(source.path(), dir.path()).expect("only the timestamp differs, so it unpacks");
        assert_eq!(
            service.finish_install(&echo(), altered, dir.path()).err(),
            Some(Refusal("The package is not the one that was approved"))
        );
    }

    /// The binding on its own. The archive is EXACTLY the approved bytes (so the digest and size pass), but the
    /// signed entry names a different author or version than the package's manifest and compiled-in record say. A
    /// maintainer-signed catalog that disagrees with the package must not install it: only `binds_package` can refuse.
    #[test]
    fn a_package_that_says_something_other_than_its_signed_entry_is_refused_by_the_binding_alone() {
        use ed25519_dalek::Signer;
        let real: Catalog = serde_json::from_slice(CATALOG).unwrap();
        type Change = fn(&mut Entry);
        let mutate: [(&str, Change); 3] = [
            ("author", |e| e.author = "Someone else".into()),
            ("link", |e| e.link = "https://example.org/elsewhere".into()),
            ("version", |e| e.version = "1.0.1".into()),
        ];
        for (what, change) in mutate {
            let dir = tempfile::tempdir().unwrap();
            let mut entry = real.entries[0].clone();
            change(&mut entry);
            let body = serde_json::to_vec(&Catalog { schema: 1, sequence: 1, entries: vec![entry] }).unwrap();
            let signature = SigningKey::from_bytes(&[7u8; 32]).sign(&body).to_bytes();
            let mut service = StoreService::new(Some(test_key()), dir.path());
            service.begin_refresh().unwrap();
            service.finish_refresh(&body, &signature).unwrap();
            service.begin_install(&echo()).unwrap();
            let refused = service.finish_install(&echo(), ARCHIVE.to_vec(), dir.path()).err();
            assert_eq!(
                refused,
                Some(Refusal("The package does not say what the catalog says about it")),
                "a signed {what} that the package does not carry was accepted"
            );
        }
    }

    /// An archive built from the approved one with `plugin.json` rewritten by `edit`, the compiled program untouched,
    /// and a catalog signed over THAT archive's real digest and size, naming `signed_author`. So the archive, its
    /// digest and (when `edit` makes it say the same) its manifest all agree with the signed entry, and the ONLY thing
    /// that can still disagree is the program's compiled-in record.
    fn service_over_edited_manifest(
        dir: &Path,
        signed_author: &str,
        edit: impl Fn(&mut serde_json::Value),
    ) -> (StoreService, Vec<u8>) {
        use ed25519_dalek::Signer;
        use sha2::{Digest, Sha256};
        use std::io::Read;
        let mut members: Vec<(String, Vec<u8>, u32)> = Vec::new();
        let mut reader = tar::Archive::new(flate2::read::GzDecoder::new(ARCHIVE));
        for member in reader.entries().unwrap() {
            let mut member = member.unwrap();
            if member.header().entry_type().is_dir() {
                continue;
            }
            let path = member.path().unwrap().to_string_lossy().into_owned();
            let mode = member.header().mode().unwrap();
            let mut bytes = Vec::new();
            member.read_to_end(&mut bytes).unwrap();
            if path == "echo/plugin.json" {
                let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                edit(&mut value);
                bytes = serde_json::to_vec(&value).unwrap();
            }
            members.push((path, bytes, mode));
        }
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
        for (path, bytes, mode) in members {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(mode);
            header.set_cksum();
            builder.append_data(&mut header, path, bytes.as_slice()).unwrap();
        }
        let archive = builder.into_inner().unwrap().finish().unwrap();
        let real: Catalog = serde_json::from_slice(CATALOG).unwrap();
        let mut entry = real.entries[0].clone();
        entry.sha256 = hex::encode(Sha256::digest(&archive));
        entry.size = archive.len() as u64;
        entry.author = signed_author.to_owned();
        let body = serde_json::to_vec(&Catalog { schema: 1, sequence: 1, entries: vec![entry] }).unwrap();
        let signature = SigningKey::from_bytes(&[7u8; 32]).sign(&body).to_bytes();
        let mut service = StoreService::new(Some(test_key()), dir);
        service.begin_refresh().unwrap();
        service.finish_refresh(&body, &signature).unwrap();
        service.begin_install(&echo()).unwrap();
        (service, archive)
    }

    /// The user's requirement made concrete: text in the manifest is easy to edit, so an edited manifest author
    /// must not pass just because someone also made the signed entry agree with the edit. The program's compiled-in
    /// record still says the original author, and that disagreement alone must refuse the package.
    #[test]
    fn an_edited_manifest_author_is_refused_when_the_compiled_in_record_says_otherwise() {
        let dir = tempfile::tempdir().unwrap();
        let (mut service, archive) = service_over_edited_manifest(dir.path(), "Impostor", |manifest| {
            manifest["package"]["author"] = "Impostor".into();
        });
        let refused = service.finish_install(&echo(), archive, dir.path()).err();
        assert_eq!(
            refused,
            Some(Refusal("The package does not say what the catalog says about it")),
            "the manifest and the signed entry agreed on an author the program does not carry"
        );
    }

    /// The store distributes only formats that run in the isolated module host. A package whose manifest has no
    /// `execution` is a legacy process that would run with the user's own rights, and is refused even when the
    /// catalog, the digest and the embedded record all agree.
    #[test]
    fn a_legacy_process_package_is_refused_by_the_isolation_gate_alone() {
        let dir = tempfile::tempdir().unwrap();
        let (mut service, archive) = service_over_edited_manifest(dir.path(), "Someone", |manifest| {
            manifest.as_object_mut().unwrap().remove("execution");
        });
        let refused = service.finish_install(&echo(), archive, dir.path()).err();
        assert_eq!(refused, Some(Refusal("The store only distributes extensions that run in the isolated module host")));
    }
}
