//! Desktop identity storage. No plaintext fallback and no credential error payloads.
//! Call off the UI thread, once at startup. Mobile supplies its native keystore.
use fs2::FileExt;
use iroh::SecretKey;
use keyring::{Entry, Error};
use std::{fs::OpenOptions, path::Path};
use zeroize::Zeroizing;

#[cfg(target_os = "linux")]
#[path = "identity_unlock.rs"]
mod unlock;

/// Serialize first-use creation across processes sharing the application directory.
/// The lock file contains no key material. Only the dedicated credential is accessed.
pub fn load(directory: &Path) -> Result<SecretKey, &'static str> {
    load_with(directory, prepare_for_window)
}

/// The launch agent may use an existing identity, never create or select one on a headless start.
/// Its executable disables keychain interaction before making any credential request.
#[cfg(any(target_os = "macos", windows))]
pub fn load_for_service(_directory: &Path) -> Result<SecretKey, &'static str> {
    let entry = Entry::new("Peersus", "transport-identity")
        .map_err(|_| "System credential store is unavailable")?;
    existing_service_key(&entry)
}

#[cfg(any(target_os = "macos", windows, test))]
fn existing_service_key(entry: &Entry) -> Result<SecretKey, &'static str> {
    let bytes = entry.get_secret().map_err(
        |_| "The service cannot read the existing identity; open the app in your session",
    )?;
    decode(&Zeroizing::new(bytes))
}

#[cfg(all(target_os = "linux", not(feature = "test-identity")))]
fn prepare_for_window() -> Result<(), &'static str> {
    unlock::prepare()
}

#[cfg(any(not(target_os = "linux"), feature = "test-identity"))]
fn prepare_for_window() -> Result<(), &'static str> {
    Ok(())
}

/// Why the Linux service could not read an existing credential without prompting.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub(crate) enum ServiceIdentityError {
    Locked,
    Missing,
    Unavailable,
    Invalid(&'static str),
}

#[cfg(target_os = "linux")]
pub(crate) async fn service_identity(directory: &Path) -> Result<SecretKey, ServiceIdentityError> {
    // The synthetic owner never connects to the system credential store.
    if cfg!(feature = "test-identity") {
        return Ok(SecretKey::generate());
    }
    // Do not ask D-Bus to start the provider before sign-in or read from a keyring that
    // reports locked collections (some providers hide their items until unlocked).
    match unlock::state_async().await {
        unlock::State::Locked => return Err(ServiceIdentityError::Locked),
        unlock::State::Unavailable => return Err(ServiceIdentityError::Unavailable),
        unlock::State::Ready => {}
    }
    let directory = directory.to_owned();
    tokio::task::spawn_blocking(move || read_service_identity(&directory))
        .await
        .unwrap_or(Err(ServiceIdentityError::Unavailable))
}

#[cfg(target_os = "linux")]
fn read_service_identity(directory: &Path) -> Result<SecretKey, ServiceIdentityError> {
    use dbus_secret_service::{EncryptionType, Error as DbusError, SecretService};
    use std::{
        collections::HashMap,
        time::{Duration, Instant},
    };

    fn access_error(error: DbusError) -> ServiceIdentityError {
        match error {
            DbusError::Locked => ServiceIdentityError::Locked,
            DbusError::Dbus(error)
                if error.name() == Some("org.freedesktop.Secret.Error.IsLocked") =>
            {
                ServiceIdentityError::Locked
            }
            _ => ServiceIdentityError::Unavailable,
        }
    }

    let deadline = Instant::now() + Duration::from_secs(5);
    let within_deadline = || {
        (Instant::now() < deadline)
            .then_some(())
            .ok_or(ServiceIdentityError::Unavailable)
    };
    let service = SecretService::connect(EncryptionType::Dh).map_err(access_error)?;
    within_deadline()?;
    let attributes = [("service", "Peersus"), ("username", "transport-identity")];
    let found = service
        .search_items(
            attributes
                .into_iter()
                .chain([("target", "default")])
                .collect(),
        )
        .map_err(access_error)?;
    if !found.locked.is_empty() {
        return Err(ServiceIdentityError::Locked);
    }
    let collection;
    let items = if found.unlocked.is_empty() {
        // Keep the interactive backend's lookup precedence for older records.
        within_deadline()?;
        collection = match service.get_default_collection() {
            Ok(collection) => collection,
            Err(DbusError::NoResult) => return Err(ServiceIdentityError::Missing),
            Err(error) => return Err(access_error(error)),
        };
        within_deadline()?;
        if collection.is_locked().map_err(access_error)? {
            return Err(ServiceIdentityError::Locked);
        }
        within_deadline()?;
        collection
            .search_items(HashMap::from(attributes))
            .map_err(access_error)?
    } else {
        found.unlocked
    };
    select_service_identity(
        directory,
        items.iter().map(|item| {
            within_deadline()?;
            // GetSecret itself refuses a locked item. Never call a wrapper that runs
            // Unlock or Prompt if the item relocks after SearchItems.
            item.get_secret().map(Zeroizing::new).map_err(access_error)
        }),
    )
}

#[cfg(target_os = "linux")]
fn select_service_identity(
    directory: &Path,
    records: impl IntoIterator<Item = Result<Zeroizing<Vec<u8>>, ServiceIdentityError>>,
) -> Result<SecretKey, ServiceIdentityError> {
    let mut keys = Vec::new();
    let mut invalid = None;
    for record in records {
        match decode(&record?) {
            Ok(key) => keys.push(key),
            Err(reason) => invalid = Some(reason),
        }
    }
    if keys.is_empty() {
        return Err(invalid.map_or(ServiceIdentityError::Missing, ServiceIdentityError::Invalid));
    }
    keys.sort_by_key(|key| key.public().to_string());
    let chosen = pick(&keys, directory);
    Ok(keys.swap_remove(chosen))
}

fn load_with(
    directory: &Path,
    prepare: impl FnOnce() -> Result<(), &'static str>,
) -> Result<SecretKey, &'static str> {
    std::fs::create_dir_all(directory).map_err(|_| "Cannot create application directory")?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options
        .open(directory.join("identity.lock"))
        .map_err(|_| "Cannot open identity lock")?;
    lock.try_lock_exclusive()
        .map_err(|_| "Identity is being opened by another process")?;
    let entry = Entry::new("Peersus", "transport-identity")
        .map_err(|_| "System credential store is unavailable")?;
    load_from_directory_with(directory, &entry, prepare)
}

#[cfg(test)]
fn load_from_directory(directory: &Path, entry: &Entry) -> Result<SecretKey, &'static str> {
    load_from_directory_with(directory, entry, || Ok(()))
}

fn load_from_directory_with(
    directory: &Path,
    entry: &Entry,
    prepare: impl FnOnce() -> Result<(), &'static str>,
) -> Result<SecretKey, &'static str> {
    // Resolve native access before trusting lookup for an already-paired profile.
    // Cancelling the system prompt must never authorize a replacement identity.
    let has_pairings = peersus_net::peers::Peers::open(directory.join("peers.json"))
        .map(|peers| peers.entries().next().is_some())
        .map_err(|_| "Cannot verify existing pairings; identity was not replaced");
    if matches!(has_pairings, Ok(true)) {
        prepare()?;
    }
    load_entry_with(directory, entry, || {
        if has_pairings? {
            return Err(
                "The identity for existing pairings is unavailable; it was not replaced. If it is gone for good, run `peersus identity-reset --yes` to start with a new identity",
            );
        }
        Ok(())
    })
}

fn decode(bytes: &[u8]) -> Result<SecretKey, &'static str> {
    let bytes: &[u8; 32] = bytes
        .try_into()
        .map_err(|_| "Stored identity is invalid; it has not been replaced. Run `peersus identity-reset --yes` to start with a new identity")?;
    Ok(SecretKey::from_bytes(bytes))
}

#[cfg(test)]
fn load_entry(entry: &Entry) -> Result<SecretKey, &'static str> {
    load_entry_with(Path::new(""), entry, || Ok(()))
}

/// The secret of every record the keyring listed as matching. A credential made from one of the records still matches ALL of them
/// when it looks itself up, so reading its secret by the usual call is ambiguous again and says nothing; the Secret Service
/// credential can read what every matching item holds, and does (unlocking one that is locked, as any read does). Another store, and
/// the mock the tests use, reads each credential on its own.
fn record_secrets(candidates: &[Box<keyring::credential::Credential>]) -> Vec<Zeroizing<Vec<u8>>> {
    #[cfg(target_os = "linux")]
    for candidate in candidates {
        if let Some(record) = candidate
            .as_any()
            .downcast_ref::<keyring::secret_service::SsCredential>()
        {
            return record
                .map_matching_items(keyring::secret_service::get_item_secret, false)
                .map(|all| all.into_iter().map(Zeroizing::new).collect())
                .unwrap_or_default();
        }
    }
    candidates
        .iter()
        .filter_map(|candidate| candidate.get_secret().ok())
        .map(Zeroizing::new)
        .collect()
}

/// Two records of the identity are in the keyring (the service migrated an older keyring while a newer record already stood in
/// its own collection, or a record was made while the older one was hidden by a lock). Refusing to start leaves the person with
/// nothing to do, and both are this person's own keys, so the application settles it and says what it did: the record that was
/// chosen before (`identity.id` in the application directory holds only its public key) if one still is there, else the first by
/// public key, which is the same on every start. Both records are left as they are; nothing is deleted or written to the keyring.
/// A device paired with the other identity does not know this one and must be paired again.
fn resolve_ambiguous(
    directory: &Path,
    candidates: Vec<Box<keyring::credential::Credential>>,
) -> Result<SecretKey, &'static str> {
    let mut keys = sorted_records(&candidates);
    if keys.is_empty() {
        return Err(
            "System keyring contains multiple matching identity records and none could be read; no identity was selected or changed. Run `peersus identity-list`, then `peersus identity-use <record>` to choose one, or `peersus identity-reset --yes` to start with a new identity",
        );
    }
    let chosen = pick(&keys, directory);
    let key = keys.swap_remove(chosen);
    if keys.is_empty() {
        // Several records held the very same key: nothing was in doubt.
        return Ok(key);
    }
    peersus_diagnostics::dev_trace!(
        stage = "identity",
        outcome = "ambiguous-settled",
        "Several identity records are in the keyring; one was chosen and none was changed"
    );
    let _ = std::fs::write(directory.join("identity.id"), key.public().to_string());
    Ok(key)
}

/// The keys the listed records hold, in memory only, one of each and in the order of their public keys, so that every start sees them the same.
fn sorted_records(candidates: &[Box<keyring::credential::Credential>]) -> Vec<SecretKey> {
    let mut keys: Vec<SecretKey> = record_secrets(candidates)
        .iter()
        .filter_map(|bytes| decode(bytes).ok())
        .collect();
    keys.sort_by_key(|key| key.public().to_string());
    keys.dedup_by_key(|key| key.public().to_string());
    keys
}

/// Which of `keys` (sorted) is the identity: the one chosen before (`identity.id`) if it is still there, else the first.
fn pick(keys: &[SecretKey], directory: &Path) -> usize {
    std::fs::read_to_string(directory.join("identity.id"))
        .ok()
        .and_then(|id| {
            keys.iter()
                .position(|key| key.public().to_string() == id.trim())
        })
        .unwrap_or(0)
}

/// Everything the keyring holds under the identity's name, as keys in memory: none, one, or several. A keyring that cannot be read
/// is an error and is never taken for an empty one.
fn records(entry: &Entry) -> Result<Vec<SecretKey>, &'static str> {
    match entry.get_secret() {
        Ok(bytes) => decode(&Zeroizing::new(bytes)).map(|key| vec![key]),
        Err(Error::NoEntry) => Ok(Vec::new()),
        Err(Error::Ambiguous(candidates)) => Ok(sorted_records(&candidates)),
        Err(_) => Err("Cannot read the system keyring; nothing was changed"),
    }
}

/// One identity record as a person is shown it: the start of its public key (never anything secret), and whether it is the one in use.
#[derive(Debug, PartialEq, Eq)]
pub struct Record {
    pub fingerprint: String,
    pub in_use: bool,
}

const FINGERPRINT: usize = 16;

fn list_with(directory: &Path, entry: &Entry) -> Result<Vec<Record>, &'static str> {
    let keys = records(entry)?;
    let chosen = pick(&keys, directory);
    Ok(keys
        .iter()
        .enumerate()
        .map(|(at, key)| Record {
            fingerprint: key.public().to_string().chars().take(FINGERPRINT).collect(),
            in_use: at == chosen,
        })
        .collect())
}

/// Make the record whose public key starts with `prefix` the identity, for good: its key is written to `identity.id` in the application
/// directory and nothing in the keyring is touched. A prefix that fits no record or several is refused and nothing is written.
fn choose_with(directory: &Path, entry: &Entry, prefix: &str) -> Result<String, &'static str> {
    let prefix = prefix.trim().to_lowercase();
    if prefix.len() < 4 {
        return Err(
            "Give at least the first four characters of the record, as `identity-list` shows it",
        );
    }
    let keys = records(entry)?;
    let mut fits = keys
        .iter()
        .filter(|key| key.public().to_string().to_lowercase().starts_with(&prefix));
    let (Some(key), None) = (fits.next(), fits.next()) else {
        return Err("That is not the start of exactly one record; nothing was changed");
    };
    std::fs::write(directory.join("identity.id"), key.public().to_string())
        .map_err(|_| "Cannot remember the choice; nothing was changed")?;
    Ok(key.public().to_string().chars().take(FINGERPRINT).collect())
}

/// Start over: every record of the identity is deleted from the keyring (irrecoverably), the choice is forgotten, and the list of
/// pairings is put aside as `peers.json.old-<time>` (kept, never deleted), since devices paired with the old identity do not know the
/// new one and a profile that still has pairings is not allowed a new identity. The next start makes one.
fn reset_with(
    directory: &Path,
    entry: &Entry,
    delete_all: impl FnOnce(&Entry) -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    // Read first, so that a keyring that cannot be read is not mistaken for one that was emptied.
    records(entry)?;
    delete_all(entry)?;
    let _ = std::fs::remove_file(directory.join("identity.id"));
    let peers = directory.join("peers.json");
    if peers.exists() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        std::fs::rename(&peers, directory.join(format!("peers.json.old-{stamp}")))
            .map_err(|_| "The identity was deleted but the pairings could not be put aside")?;
    }
    Ok(())
}

fn delete_records(entry: &Entry) -> Result<(), &'static str> {
    const REFUSED: &str = "The keyring refused to delete the identity records";
    #[cfg(target_os = "linux")]
    if let Some(record) = entry
        .get_credential()
        .downcast_ref::<keyring::secret_service::SsCredential>()
    {
        return record.delete_all_passwords().map_err(|_| REFUSED);
    }
    match entry.delete_credential() {
        Ok(()) | Err(Error::NoEntry) => Ok(()),
        Err(_) => Err(REFUSED),
    }
}

fn system_entry(directory: &Path) -> Result<Entry, &'static str> {
    std::fs::create_dir_all(directory).map_err(|_| "Cannot create application directory")?;
    #[cfg(all(target_os = "linux", not(feature = "test-identity")))]
    // Best effort: a keyring that was not unlocked here (cancelled, slow, a provider that fails) must not stop the person from listing, choosing or
    // starting over: reading and deleting ask the system to unlock what they touch, and say plainly if it still will not.
    let _ = unlock::prepare();
    Entry::new("Peersus", "transport-identity")
        .map_err(|_| "System credential store is unavailable")
}

/// The identity records in the system keyring and which one is in use (`peersus identity-list`).
pub fn list(directory: &Path) -> Result<Vec<Record>, &'static str> {
    list_with(directory, &system_entry(directory)?)
}

/// Make one record the identity (`peersus identity-use`).
pub fn choose(directory: &Path, prefix: &str) -> Result<String, &'static str> {
    choose_with(directory, &system_entry(directory)?, prefix)
}

/// Delete every record and start with a new identity (`peersus identity-reset --yes`).
pub fn reset(directory: &Path) -> Result<(), &'static str> {
    reset_with(directory, &system_entry(directory)?, delete_records)
}

fn load_entry_with(
    directory: &Path,
    entry: &Entry,
    allow_creation: impl FnOnce() -> Result<(), &'static str>,
) -> Result<SecretKey, &'static str> {
    match entry.get_secret() {
        Ok(bytes) => decode(&Zeroizing::new(bytes)),
        Err(Error::NoEntry) => {
            allow_creation()?;
            let key = SecretKey::generate();
            let bytes = Zeroizing::new(key.to_bytes());
            entry
                .set_secret(bytes.as_ref())
                .map_err(|_| "Cannot save identity in system credential store")?;
            let saved = Zeroizing::new(
                entry
                    .get_secret()
                    .map_err(|_| "Cannot verify saved identity; retry reading on next startup")?,
            );
            let loaded = decode(&saved)?;
            if loaded.public() != key.public() {
                return Err("Stored identity changed during creation; startup refused");
            }
            Ok(loaded)
        }
        Err(Error::NoStorageAccess(_)) => Err(
            "System keyring access was denied or cancelled; existing identity and pairings were not changed",
        ),
        Err(Error::Ambiguous(candidates)) => resolve_ambiguous(directory, candidates),
        Err(_) => Err("Cannot read system identity; existing credentials were not changed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry() -> Entry {
        Entry::new_with_credential(Box::new(keyring::mock::MockCredential::default()))
    }
    #[test]
    fn identity_survives_repeated_load_without_rotation() {
        let entry = entry();
        let first = load_entry(&entry).unwrap().public();
        let second = load_entry(&entry).unwrap().public();
        assert_eq!(first, second);
    }
    #[test]
    fn a_headless_service_never_creates_or_repairs_an_identity() {
        let entry = entry();
        assert!(existing_service_key(&entry).is_err());
        assert!(matches!(entry.get_secret(), Err(Error::NoEntry)));
        let expected = load_entry(&entry).unwrap().public();
        assert_eq!(existing_service_key(&entry).unwrap().public(), expected);
        entry.set_secret(&[1, 2, 3]).unwrap();
        assert!(existing_service_key(&entry).is_err());
        assert_eq!(entry.get_secret().unwrap(), [1, 2, 3]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_service_waits_if_a_record_relocks_and_reuses_the_existing_key_when_ready() {
        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join("profile");
        let key = SecretKey::generate();
        let read = || Ok(Zeroizing::new(key.to_bytes().to_vec()));

        assert!(matches!(
            select_service_identity(&profile, [read(), Err(ServiceIdentityError::Locked)]),
            Err(ServiceIdentityError::Locked)
        ));
        assert!(matches!(
            select_service_identity(&profile, []),
            Err(ServiceIdentityError::Missing)
        ));
        let ready = select_service_identity(&profile, [read()]).unwrap();
        assert_eq!(ready.public(), key.public());
        assert!(
            !profile.exists(),
            "a service read must not initialize the profile"
        );
    }

    #[test]
    fn corrupt_identity_is_preserved_instead_of_silently_rotated() {
        let entry = entry();
        entry.set_secret(&[1, 2, 3]).unwrap();
        assert!(load_entry(&entry).is_err());
        assert_eq!(entry.get_secret().unwrap(), [1, 2, 3]);
    }

    #[test]
    fn access_denial_and_ambiguity_never_replace_a_saved_identity() {
        for error in [
            Error::NoStorageAccess(Box::new(std::io::Error::from(
                std::io::ErrorKind::PermissionDenied,
            ))),
            Error::Ambiguous(Vec::new()),
        ] {
            let entry = entry();
            let original = load_entry(&entry).unwrap().public();
            entry
                .get_credential()
                .downcast_ref::<keyring::mock::MockCredential>()
                .unwrap()
                .set_error(error);
            assert!(load_entry(&entry).is_err());
            assert_eq!(load_entry(&entry).unwrap().public(), original);
        }
    }

    fn two_records(a: &SecretKey, b: &SecretKey) -> Entry {
        use keyring::credential::CredentialApi;
        let record = |key: &SecretKey| -> Box<keyring::credential::Credential> {
            let credential = keyring::mock::MockCredential::default();
            credential.set_secret(&key.to_bytes()).unwrap();
            Box::new(credential)
        };
        let entry = entry();
        entry
            .get_credential()
            .downcast_ref::<keyring::mock::MockCredential>()
            .unwrap()
            .set_error(Error::Ambiguous(vec![record(a), record(b)]));
        entry
    }

    #[test]
    fn two_identity_records_are_settled_the_same_way_every_time_and_neither_is_changed() {
        let directory = tempfile::tempdir().unwrap();
        let (a, b) = (SecretKey::generate(), SecretKey::generate());
        let first = load_entry_with(directory.path(), &two_records(&a, &b), || Ok(())).unwrap();
        // The same two records, in the other order, give the same one: the choice does not depend on what the service listed first.
        let again = load_entry_with(directory.path(), &two_records(&b, &a), || Ok(())).unwrap();
        assert_eq!(first.public(), again.public());
        assert!([a.public(), b.public()].contains(&first.public()));
        // The choice is remembered by its public key alone.
        assert_eq!(
            std::fs::read_to_string(directory.path().join("identity.id")).unwrap(),
            first.public().to_string()
        );
    }

    #[test]
    fn the_record_chosen_before_is_the_one_that_stays_when_another_appears() {
        let directory = tempfile::tempdir().unwrap();
        let (a, b) = (SecretKey::generate(), SecretKey::generate());
        // Whichever sorts last would not be taken by the rule alone.
        let (kept, other) = if a.public().to_string() > b.public().to_string() {
            (a, b)
        } else {
            (b, a)
        };
        std::fs::write(
            directory.path().join("identity.id"),
            kept.public().to_string(),
        )
        .unwrap();
        let chosen =
            load_entry_with(directory.path(), &two_records(&kept, &other), || Ok(())).unwrap();
        assert_eq!(chosen.public(), kept.public());
    }

    #[test]
    fn records_that_cannot_be_read_are_not_guessed_at_and_equal_ones_are_no_doubt() {
        let directory = tempfile::tempdir().unwrap();
        let key = SecretKey::generate();
        // The same key twice is one identity, and nothing is written about a choice that was not made.
        let same = load_entry_with(directory.path(), &two_records(&key, &key), || Ok(())).unwrap();
        assert_eq!(same.public(), key.public());
        assert!(!directory.path().join("identity.id").exists());
    }

    #[test]
    fn a_missing_lookup_must_not_replace_an_identity_after_pairing() {
        let directory = tempfile::tempdir().unwrap();
        let remote = SecretKey::generate().public();
        let peers = serde_json::json!({
            "version": 2,
            "peers": { remote.to_string(): {
                "peer": {"id": remote, "name": "Paired device", "paired_at": 1,
                    "last_seen": null, "online": false},
                "address": iroh::EndpointAddr::new(remote)
            }}
        });
        let bytes = serde_json::to_vec(&peers).unwrap();
        std::fs::write(directory.path().join("peers.json"), &bytes).unwrap();
        let entry = entry();
        let original = load_entry(&entry).unwrap().public();
        entry
            .get_credential()
            .downcast_ref::<keyring::mock::MockCredential>()
            .unwrap()
            .set_error(Error::NoEntry);
        assert!(
            load_from_directory(directory.path(), &entry).is_err(),
            "a paired profile must not create another identity after a missing lookup"
        );
        // A service retry and a subsequent interactive start both recover the very same
        // device; neither requires the peer to approve a new pairing.
        assert_eq!(existing_service_key(&entry).unwrap().public(), original);
        assert_eq!(
            load_from_directory(directory.path(), &entry)
                .unwrap()
                .public(),
            original
        );
        assert_eq!(
            std::fs::read(directory.path().join("peers.json")).unwrap(),
            bytes
        );
    }

    #[test]
    fn first_use_and_empty_pairing_state_create_one_stable_identity() {
        for saved in [None, Some(br#"{"version":2,"peers":{}}"#.as_slice())] {
            let directory = tempfile::tempdir().unwrap();
            if let Some(bytes) = saved {
                std::fs::write(directory.path().join("peers.json"), bytes).unwrap();
            }
            let entry = entry();
            let first = load_from_directory(directory.path(), &entry)
                .unwrap()
                .public();
            assert_eq!(
                load_from_directory(directory.path(), &entry)
                    .unwrap()
                    .public(),
                first
            );
        }
    }

    #[test]
    fn existing_identity_is_reused_independently_of_profile_pairing_state() {
        let entry = entry();
        let original = load_entry(&entry).unwrap().public();
        for saved in [
            None,
            Some(br#"{"version":2,"peers":{}}"#.as_slice()),
            Some(b"{incomplete".as_slice()),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("peers.json");
            if let Some(bytes) = saved {
                std::fs::write(&path, bytes).unwrap();
            }
            assert_eq!(
                load_from_directory(directory.path(), &entry)
                    .unwrap()
                    .public(),
                original
            );
            assert_eq!(std::fs::read(path).ok().as_deref(), saved);
        }
    }

    #[test]
    fn unreadable_pairing_state_never_authorizes_identity_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("peers.json");
        std::fs::write(&path, b"{incomplete").unwrap();
        let entry = entry();
        assert!(load_from_directory(directory.path(), &entry).is_err());
        assert!(matches!(entry.get_secret(), Err(Error::NoEntry)));
        assert_eq!(std::fs::read(path).unwrap(), b"{incomplete");
    }

    #[test]
    fn native_access_is_resolved_before_reading_an_existing_paired_identity() {
        let directory = tempfile::tempdir().unwrap();
        let remote = SecretKey::generate().public();
        let bytes = serde_json::to_vec(&serde_json::json!({
            "version": 2,
            "peers": { remote.to_string(): {
                "peer": {"id": remote, "name": "Paired device", "paired_at": 1,
                    "last_seen": null, "online": false},
                "address": iroh::EndpointAddr::new(remote)
            }}
        }))
        .unwrap();
        std::fs::write(directory.path().join("peers.json"), &bytes).unwrap();
        let entry = entry();
        let original = load_entry(&entry).unwrap().public();
        let denied =
            "System keyring unlock was cancelled; existing identity and pairings were not changed";
        assert_eq!(
            load_from_directory_with(directory.path(), &entry, || Err(denied)).unwrap_err(),
            denied
        );
        assert_eq!(load_entry(&entry).unwrap().public(), original);
        assert_eq!(
            std::fs::read(directory.path().join("peers.json")).unwrap(),
            bytes
        );
        let opened = std::cell::Cell::new(false);
        assert_eq!(
            load_from_directory_with(directory.path(), &entry, || {
                opened.set(true);
                Ok(())
            },)
            .unwrap()
            .public(),
            original
        );
        assert!(opened.get());
    }

    #[test]
    fn first_use_does_not_request_unlock_of_unrelated_collections() {
        let directory = tempfile::tempdir().unwrap();
        let entry = entry();
        let key = load_from_directory_with(directory.path(), &entry, || {
            panic!("first use must not run paired identity recovery")
        })
        .unwrap();
        assert_eq!(load_entry(&entry).unwrap().public(), key.public());
    }
    #[test]
    #[ignore = "Requires explicit permission to access the dedicated system credential"]
    fn native_store_retains_identity() {
        let directory = tempfile::tempdir().unwrap();
        let first = load(directory.path()).unwrap().public();
        let second = load(directory.path()).unwrap().public();
        assert!(
            first == second,
            "Identity changed between native store reads"
        );
    }

    #[test]
    fn the_records_are_listed_by_the_start_of_their_public_key_and_the_one_in_use_is_marked() {
        let directory = tempfile::tempdir().unwrap();
        let (a, b) = (SecretKey::generate(), SecretKey::generate());
        let listed = list_with(directory.path(), &two_records(&a, &b)).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed.iter().filter(|record| record.in_use).count(), 1);
        let shown: Vec<_> = listed
            .iter()
            .map(|record| record.fingerprint.clone())
            .collect();
        for key in [&a, &b] {
            let start: String = key.public().to_string().chars().take(FINGERPRINT).collect();
            assert!(shown.contains(&start));
        }
        // A keyring with no record lists none.
        let entry = entry();
        let only = list_with(directory.path(), &entry).unwrap();
        assert!(only.is_empty(), "nothing was ever saved here");
    }

    #[test]
    fn choosing_a_record_is_remembered_and_a_vague_choice_changes_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let (a, b) = (SecretKey::generate(), SecretKey::generate());
        let wanted = if a.public().to_string() > b.public().to_string() {
            &a
        } else {
            &b
        };
        let start: String = wanted.public().to_string().chars().take(8).collect();
        let done = choose_with(directory.path(), &two_records(&a, &b), &start).unwrap();
        assert!(wanted.public().to_string().starts_with(&done));
        // The next start takes it, though another sorts first.
        let loaded = load_entry_with(directory.path(), &two_records(&a, &b), || Ok(())).unwrap();
        assert_eq!(loaded.public(), wanted.public());
        for vague in ["", "ab", "zzzzzzzz"] {
            assert!(choose_with(directory.path(), &two_records(&a, &b), vague).is_err());
        }
        assert_eq!(
            std::fs::read_to_string(directory.path().join("identity.id")).unwrap(),
            wanted.public().to_string()
        );
    }

    #[test]
    fn a_reset_deletes_the_records_puts_the_pairings_aside_and_lets_a_new_identity_be_made() {
        let directory = tempfile::tempdir().unwrap();
        let remote = SecretKey::generate().public();
        let peers = serde_json::to_vec(&serde_json::json!({
            "version": 2,
            "peers": { remote.to_string(): {
                "peer": {"id": remote, "name": "Paired device", "paired_at": 1,
                    "last_seen": null, "online": false},
                "address": iroh::EndpointAddr::new(remote)
            }}
        }))
        .unwrap();
        std::fs::write(directory.path().join("peers.json"), &peers).unwrap();
        std::fs::write(directory.path().join("identity.id"), "something").unwrap();
        let entry = entry();
        let old = load_entry(&entry).unwrap().public();
        reset_with(directory.path(), &entry, |entry| {
            entry.delete_credential().map_err(|_| "refused")
        })
        .unwrap();
        assert!(matches!(entry.get_secret(), Err(Error::NoEntry)));
        assert!(!directory.path().join("identity.id").exists());
        assert!(!directory.path().join("peers.json").exists());
        let kept: Vec<_> = std::fs::read_dir(directory.path())
            .unwrap()
            .filter_map(|file| file.ok())
            .filter(|file| {
                file.file_name()
                    .to_string_lossy()
                    .starts_with("peers.json.old-")
            })
            .collect();
        assert_eq!(kept.len(), 1);
        assert_eq!(std::fs::read(kept[0].path()).unwrap(), peers);
        // With no pairings left, a new identity may be made, and it is another one.
        let new = load_from_directory(directory.path(), &entry)
            .unwrap()
            .public();
        assert_ne!(new, old);
    }

    #[test]
    fn a_reset_does_not_delete_when_the_keyring_cannot_be_read() {
        let directory = tempfile::tempdir().unwrap();
        let entry = entry();
        let kept = load_entry(&entry).unwrap().public();
        entry
            .get_credential()
            .downcast_ref::<keyring::mock::MockCredential>()
            .unwrap()
            .set_error(Error::NoStorageAccess(Box::new(std::io::Error::from(
                std::io::ErrorKind::PermissionDenied,
            ))));
        let mut deleted = false;
        assert!(
            reset_with(directory.path(), &entry, |_| {
                deleted = true;
                Ok(())
            })
            .is_err()
        );
        assert!(!deleted);
        assert_eq!(load_entry(&entry).unwrap().public(), kept);
    }
}
