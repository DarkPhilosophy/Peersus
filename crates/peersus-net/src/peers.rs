//! Durable authenticated peer identities; presence is never persisted as online.
use iroh::EndpointAddr;
use peersus_core::{
    model::{Peer, PeerId},
    pairing::AuthorizedPair,
    plugins::{Grants, PluginId, Value},
};
use peersus_plugin_api::contributions::{Scope, Setting};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{self, Read},
    path::PathBuf,
};

const MAX_PEERS: usize = 256;
const MAX_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Remembered {
    pub peer: Peer,
    pub address: EndpointAddr,
}
/// The version of the saved document. 2 stores grants as permissions with an owner; version
/// 1 stored bare names with the owner implied and is refused rather than guessed at.
const VERSION: u32 = 2;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    peers: BTreeMap<String, Remembered>,
}

pub struct Peers {
    path: PathBuf,
    document: Document,
}

impl Peers {
    pub fn open(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let mut document = match File::open(&path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
                if bytes.len() as u64 > MAX_BYTES {
                    return Err(invalid());
                }
                let document: Document = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
                if document.version != VERSION || document.peers.len() > MAX_PEERS {
                    return Err(invalid());
                }
                document
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Document {
                version: VERSION,
                peers: BTreeMap::new(),
            },
            Err(error) => return Err(error),
        };
        for (key, remembered) in &mut document.peers {
            if key != &remembered.peer.id.0 || key != &remembered.address.id.to_string() {
                return Err(invalid());
            }
            validate_name(&remembered.peer.name)?;
            remembered.peer.online = false;
        }
        Ok(Self { path, document })
    }

    pub fn entries(&self) -> impl Iterator<Item = &Remembered> {
        self.document.peers.values()
    }

    pub fn get(&self, id: &PeerId) -> Option<&Remembered> {
        self.document.peers.get(&id.0)
    }

    /// Refresh routing hints without introducing or changing an authenticated identity.
    pub fn update_address(&mut self, address: EndpointAddr) -> io::Result<()> {
        let mut candidate = self.document.clone();
        let remembered = candidate
            .peers
            .get_mut(&address.id.to_string())
            .ok_or_else(invalid)?;
        remembered.address = address;
        self.publish(candidate)
    }

    /// Only a completed consent ceremony can introduce a remembered identity.
    pub fn remember(
        &mut self,
        authorization: AuthorizedPair,
        address: EndpointAddr,
        name: String,
        now: u64,
    ) -> io::Result<()> {
        if authorization.peer_id() != address.id.as_bytes() {
            return Err(invalid());
        }
        validate_name(&name)?;
        let id = address.id.to_string();
        let mut candidate = self.document.clone();
        if !candidate.peers.contains_key(&id) && candidate.peers.len() >= MAX_PEERS {
            return Err(io::Error::other("Remembered device limit reached"));
        }
        let existing = candidate.peers.get(&id);
        let peer = Peer {
            id: PeerId(id.clone()),
            name,
            paired_at: existing.map_or(now, |entry| entry.peer.paired_at),
            last_seen: Some(now),
            online: false,
            grants: existing.map_or_else(Default::default, |entry| entry.peer.grants.clone()),
            plugin_values: existing
                .map_or_else(Default::default, |entry| entry.peer.plugin_values.clone()),
        };
        candidate.peers.insert(id, Remembered { peer, address });
        self.publish(candidate)
    }

    pub fn forget(&mut self, id: &PeerId) -> io::Result<()> {
        let mut candidate = self.document.clone();
        candidate.peers.remove(&id.0);
        self.publish(candidate)
    }

    /// Replace what one plugin may do for one remembered device. An empty set
    /// removes the entry so default-deny is represented by absence.
    pub fn set_grants(&mut self, id: &PeerId, plugin: PluginId, grants: Grants) -> io::Result<()> {
        let mut candidate = self.document.clone();
        let peer = candidate.peers.get_mut(&id.0).ok_or_else(invalid)?;
        if grants.is_empty() {
            peer.peer.grants.remove(&plugin);
        } else {
            peer.peer.grants.insert(plugin, grants);
        }
        self.publish(candidate)
    }

    /// Forget what was granted to every plugin that is not in `keep`, on every device. Returns
    /// how many grants went; nothing is written when none did.
    ///
    /// Consent belongs to a plugin that was there when it was given, and an id is only a name.
    /// Stored setting values are NOT touched: they are the user's preferences, not permission,
    /// and they are checked against whatever manifest declares the setting when they are used.
    pub fn retain_plugins(&mut self, keep: &BTreeSet<PluginId>) -> io::Result<usize> {
        let mut candidate = self.document.clone();
        let mut removed = 0;
        for remembered in candidate.peers.values_mut() {
            let grants = &mut remembered.peer.grants;
            let before = grants.len();
            grants.retain(|id, _| keep.contains(id));
            removed += before - grants.len();
        }
        if removed > 0 {
            self.publish(candidate)?;
        }
        Ok(removed)
    }

    /// Forget what was granted to ONE plugin, on every device. Returns how many grants went;
    /// nothing is written when none did. The same rule as [`Peers::retain_plugins`], for one name.
    pub fn forget_plugin(&mut self, plugin: &PluginId) -> io::Result<usize> {
        let mut candidate = self.document.clone();
        let mut removed = 0;
        for remembered in candidate.peers.values_mut() {
            removed += usize::from(remembered.peer.grants.remove(plugin).is_some());
        }
        if removed > 0 {
            self.publish(candidate)?;
        }
        Ok(removed)
    }

    /// Store one per-device plugin value after checking it against the setting the
    /// plugin declared. Stored state never contains a value no manifest declared.
    pub fn set_plugin_value(
        &mut self,
        id: &PeerId,
        plugin: PluginId,
        declared: &[Setting],
        setting: &str,
        value: Value,
    ) -> io::Result<()> {
        let mut candidate = self.document.clone();
        let peer = candidate.peers.get_mut(&id.0).ok_or_else(invalid)?;
        peer.peer
            .plugin_values
            .entry(plugin)
            .or_default()
            .set(declared, Scope::Device, setting, value)
            .map_err(|_| invalid())?;
        self.publish(candidate)
    }

    fn publish(&mut self, document: Document) -> io::Result<()> {
        let bytes = serde_json::to_vec(&document).map_err(|_| invalid())?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid());
        }
        crate::atomic::write(&self.path, &bytes)?;
        self.document = document;
        Ok(())
    }
}

fn validate_name(name: &str) -> io::Result<()> {
    if name.trim().is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
        return Err(invalid());
    }
    Ok(())
}
fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Remembered device data is invalid",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;
    use peersus_core::pairing::{PairingSession, Role};
    use std::path::Path;

    fn authorize(local: [u8; 32], remote: [u8; 32]) -> AuthorizedPair {
        let mut a =
            PairingSession::new([1; 32], local, remote, [2; 32], Role::Initiator, 0, 60).unwrap();
        let mut b =
            PairingSession::new([1; 32], local, remote, [2; 32], Role::Responder, 0, 60).unwrap();
        let choice = a.options()[0];
        let ca = a.choose(choice, [3; 32], 1).unwrap();
        let cb = b.choose(choice, [4; 32], 1).unwrap();
        a.receive_commitment(cb, 1).unwrap();
        b.receive_commitment(ca, 1).unwrap();
        let ra = a.reveal(1).unwrap();
        let rb = b.reveal(1).unwrap();
        let ready_a = a.receive_reveal(rb, 1).unwrap();
        let ready_b = b.receive_reveal(ra, 1).unwrap();
        a.receive_ready(ready_b, 1).unwrap();
        b.receive_ready(ready_a, 1).unwrap();
        a.finish(1).unwrap()
    }

    fn remembered(path: &Path, now: u64) -> (Peers, PeerId, iroh::PublicKey, iroh::PublicKey) {
        let local = SecretKey::generate().public();
        let remote = SecretKey::generate().public();
        let mut store = Peers::open(path).unwrap();
        store
            .remember(
                authorize(*local.as_bytes(), *remote.as_bytes()),
                EndpointAddr::new(remote),
                "Laptop".into(),
                now,
            )
            .unwrap();
        (store, PeerId(remote.to_string()), local, remote)
    }
    fn perm(owner: &str, name: &str) -> peersus_plugin_api::permission::Permission {
        peersus_plugin_api::permission::Permission::new(owner, name).unwrap()
    }
    fn plugin(name: &str) -> PluginId {
        PluginId::new(name).unwrap()
    }
    fn toggle(id: &str, scope: Scope) -> Setting {
        use peersus_plugin_api::contributions::Kind;
        Setting {
            id: id.into(),
            label: id.into(),
            help: None,
            scope,
            kind: Kind::Toggle { default: false },
        }
    }

    #[test]
    fn authorized_peer_survives_restart_with_default_deny_and_no_fabricated_presence() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("peers.json");
        let (_, id, _, _) = remembered(&path, 100);
        let mut reopened = Peers::open(&path).unwrap();
        let peer = &reopened.get(&id).unwrap().peer;
        assert!(!peer.online);
        assert!(peer.grants.is_empty() && peer.plugin_values.is_empty());
        assert_eq!(peer.paired_at, 100);
        reopened
            .set_grants(
                &id,
                plugin("any"),
                Grants::new([perm("any", "any.read")]).unwrap(),
            )
            .unwrap();
        let saved = Peers::open(&path).unwrap();
        assert!(
            saved.get(&id).unwrap().peer.grants[&plugin("any")].allows(&perm("any", "any.read"))
        );
        // Revoking everything removes the entry: default-deny is absence.
        reopened
            .set_grants(&id, plugin("any"), Grants::default())
            .unwrap();
        assert!(
            Peers::open(&path)
                .unwrap()
                .get(&id)
                .unwrap()
                .peer
                .grants
                .is_empty()
        );
        reopened.forget(&id).unwrap();
        assert!(Peers::open(path).unwrap().get(&id).is_none());
    }

    #[test]
    fn the_document_is_version_two_and_older_ones_are_refused_not_guessed_at() {
        // Version 1 stored grants as bare names with the owner implied. Turning those into
        // owned permissions would mean guessing whose each one was, which is exactly the
        // ambiguity the owner exists to remove. Nothing was released in that format, so it is
        // refused and the user pairs again.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("peers.json");
        let (_, _, _, _) = remembered(&path, 100);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("\"version\": 2") || text.contains("\"version\":2"),
            "{text}"
        );
        let older = text
            .replace("\"version\": 2", "\"version\": 1")
            .replace("\"version\":2", "\"version\":1");
        assert_ne!(older, text, "the version was not rewritten");
        std::fs::write(&path, older).unwrap();
        assert!(
            Peers::open(&path).is_err(),
            "a version 1 document was accepted"
        );
    }

    #[test]
    fn grants_are_stored_with_their_owner_and_come_back_with_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("peers.json");
        let (mut store, id, _, _) = remembered(&path, 100);
        store
            .set_grants(
                &id,
                plugin("p"),
                Grants::new([perm("host", "peers.send"), perm("q", "go")]).unwrap(),
            )
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("\"owner\""),
            "the owner was not written: {text}"
        );
        let reopened = Peers::open(&path).unwrap();
        let granted = &reopened.get(&id).unwrap().peer.grants[&plugin("p")];
        assert!(granted.allows(&perm("host", "peers.send")));
        assert!(granted.allows(&perm("q", "go")));
        // The same names under the other owner were never granted.
        assert!(!granted.allows(&perm("q", "peers.send")));
        assert!(!granted.allows(&perm("host", "go")));
    }

    #[test]
    fn a_stored_grant_without_an_owner_is_refused_when_the_file_is_read() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("peers.json");
        let (mut store, id, _, _) = remembered(&path, 100);
        store
            .set_grants(
                &id,
                plugin("p"),
                Grants::new([perm("host", "peers.send")]).unwrap(),
            )
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        // Rewrite one stored permission as a bare name, as an old or hand-edited file would.
        let bare = text
            .replace(
                "{\n              \"owner\": \"host\",\n              \"name\": \"peers.send\"\n            }",
                "\"peers.send\"",
            )
            .replace("{\"owner\":\"host\",\"name\":\"peers.send\"}", "\"peers.send\"");
        assert_ne!(
            bare, text,
            "the stored shape was not what the test rewrites"
        );
        std::fs::write(&path, bare).unwrap();
        assert!(
            Peers::open(&path).is_err(),
            "a grant with no owner was accepted"
        );
    }

    #[test]
    fn grants_and_values_are_inherited_when_the_same_device_pairs_again() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("peers.json");
        let (mut store, id, local, remote) = remembered(&path, 100);
        let declared = [toggle("send", Scope::Device)];
        store
            .set_grants(&id, plugin("p"), Grants::new([perm("p", "p.use")]).unwrap())
            .unwrap();
        store
            .set_plugin_value(&id, plugin("p"), &declared, "send", Value::Toggle(true))
            .unwrap();
        store
            .remember(
                authorize(*local.as_bytes(), *remote.as_bytes()),
                EndpointAddr::new(remote),
                "Renamed".into(),
                200,
            )
            .unwrap();
        let reopened = Peers::open(&path).unwrap();
        let peer = &reopened.get(&id).unwrap().peer;
        assert_eq!(peer.name, "Renamed");
        assert_eq!(peer.paired_at, 100, "original pairing time is kept");
        assert!(peer.grants[&plugin("p")].allows(&perm("p", "p.use")));
        assert_eq!(
            peer.plugin_values[&plugin("p")].get("send"),
            Some(&Value::Toggle(true))
        );
    }

    #[test]
    fn an_undeclared_wrong_scope_or_mistyped_value_persists_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("peers.json");
        let (mut store, id, _, _) = remembered(&path, 100);
        let declared = [
            toggle("send", Scope::Device),
            toggle("global", Scope::Global),
        ];
        let before = std::fs::read(&path).unwrap();
        for (setting, value) in [
            ("missing", Value::Toggle(true)),
            ("global", Value::Toggle(true)),
            ("send", Value::Number(1)),
        ] {
            assert!(
                store
                    .set_plugin_value(&id, plugin("p"), &declared, setting, value)
                    .is_err(),
                "{setting}"
            );
        }
        assert_eq!(std::fs::read(&path).unwrap(), before, "file untouched");
        assert!(
            store.get(&id).unwrap().peer.plugin_values.is_empty(),
            "no empty entry leaked"
        );
        // An unknown device is rejected rather than silently created.
        let stranger = PeerId("nobody".into());
        assert!(
            store
                .set_grants(
                    &stranger,
                    plugin("p"),
                    Grants::new([perm("p", "p.use")]).unwrap()
                )
                .is_err()
        );
    }

    #[test]
    fn a_removed_plugins_grants_leave_every_device_and_its_settings_stay() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("peers.json");
        let (mut store, id, _, _) = remembered(&path, 100);
        let declared = [toggle("send", Scope::Device)];
        for name in ["kept", "gone"] {
            store
                .set_grants(
                    &id,
                    plugin(name),
                    Grants::new([perm(name, &format!("{name}.use"))]).unwrap(),
                )
                .unwrap();
            store
                .set_plugin_value(&id, plugin(name), &declared, "send", Value::Toggle(true))
                .unwrap();
        }
        let keep: std::collections::BTreeSet<PluginId> = [plugin("kept")].into();
        assert_eq!(store.retain_plugins(&keep).unwrap(), 1);
        let reopened = Peers::open(&path).unwrap();
        let peer = &reopened.get(&id).unwrap().peer;
        assert!(!peer.grants.contains_key(&plugin("gone")));
        // Preferences are not consent: they stay, and are checked against the manifest that
        // declares the setting whenever they are used.
        assert!(peer.plugin_values.contains_key(&plugin("gone")));
        assert!(peer.grants.contains_key(&plugin("kept")));
        assert!(peer.plugin_values.contains_key(&plugin("kept")));
    }

    #[test]
    fn pruning_devices_with_nothing_to_remove_writes_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("peers.json");
        let (mut store, _, _, _) = remembered(&path, 100);
        let modified = || std::fs::metadata(&path).unwrap().modified().unwrap();
        let before = modified();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let keep: std::collections::BTreeSet<PluginId> = [plugin("kept")].into();
        assert_eq!(store.retain_plugins(&keep).unwrap(), 0);
        assert_eq!(
            modified(),
            before,
            "a prune that removed nothing rewrote the file"
        );
    }

    #[test]
    fn authorization_cannot_be_reused_for_another_identity() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = Peers::open(directory.path().join("peers.json")).unwrap();
        let local = SecretKey::generate().public();
        let remote = SecretKey::generate().public();
        let wrong = EndpointAddr::new(SecretKey::generate().public());
        assert!(
            store
                .remember(
                    authorize(*local.as_bytes(), *remote.as_bytes()),
                    wrong,
                    "Wrong".into(),
                    100
                )
                .is_err()
        );
        assert_eq!(store.entries().count(), 0);
    }
}
