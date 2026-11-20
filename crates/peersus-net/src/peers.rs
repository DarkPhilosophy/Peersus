//! Durable authenticated peer identities; presence is never persisted as online.
use iroh::EndpointAddr;
use peersus_api::contributions::{Scope, Setting};
use peersus_core::{
    model::{Peer, PeerId},
    pairing::AuthorizedPair,
    plugins::{Grants, PluginId, Value},
};
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
    /// Where the person said this device can be found, kept as typed (a name stays a name, looked up at each connection). Absent until one
    /// is given, and absent from the file when there is none, so a file from before is read as it was. The device is only ever the one whose
    /// KEY answers: this says where to look, never who it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub typed: Option<peersus_core::address::TypedAddress>,
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
            remembered.peer.sleeping = false;
            remembered.peer.statuses.clear();
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

    /// The most direct (IP) addresses kept for one device. A phone that changes network often must not fill the file with the addresses
    /// it had: only the ones it is known by now are useful, so the bound is small.
    pub const MAX_DIRECT_ADDRESSES: usize = 4;

    /// The address a remembered device was just seen at, learnt from a connection that it made and that proved who it is. Only a device
    /// that is already remembered can be given an address, and only a direct (IP) one: nothing about who the device is changes, its relay and
    /// every other hint are kept, and an address that cannot be dialled (unspecified, multicast) is refused. Returns whether anything was
    /// stored; nothing is written when the address is already known.
    ///
    /// When there are more direct addresses than [`Self::MAX_DIRECT_ADDRESSES`] the new one stays and the older ones go: a device seen at a
    /// new address has left the old ones, which is the case this exists for (a phone that moved from one network to another).
    pub fn learn_address(&mut self, id: &PeerId, seen: std::net::SocketAddr) -> io::Result<bool> {
        if seen.ip().is_unspecified() || seen.ip().is_multicast() || seen.port() == 0 {
            return Err(invalid());
        }
        let remembered = self.document.peers.get(&id.0).ok_or_else(invalid)?;
        let known = |address: &EndpointAddr| address.addrs.contains(&iroh::TransportAddr::Ip(seen));
        if known(&remembered.address) {
            return Ok(false);
        }
        let mut candidate = self.document.clone();
        let entry = candidate.peers.get_mut(&id.0).ok_or_else(invalid)?;
        let direct = entry
            .address
            .addrs
            .iter()
            .filter(|addr| addr.is_ip())
            .count();
        if direct >= Self::MAX_DIRECT_ADDRESSES {
            // The device has moved: what it was known by before is not it any more, and only the relay and the new address are kept.
            entry.address.addrs.retain(|addr| !addr.is_ip());
        }
        entry.address.addrs.insert(iroh::TransportAddr::Ip(seen));
        self.publish(candidate)?;
        Ok(true)
    }

    /// The name a remembered device now gives itself. Only a device that is already remembered can have its name changed, and only its name:
    /// no identity is introduced and no grant, setting or address is touched. The name must pass the same check as at pairing. Returns
    /// whether the stored name changed; nothing is written when it did not.
    pub fn rename(&mut self, id: &PeerId, name: &str) -> io::Result<bool> {
        validate_name(name)?;
        let remembered = self.document.peers.get(&id.0).ok_or_else(invalid)?;
        if remembered.peer.name == name {
            return Ok(false);
        }
        let mut candidate = self.document.clone();
        candidate
            .peers
            .get_mut(&id.0)
            .ok_or_else(invalid)?
            .peer
            .name = name.to_owned();
        self.publish(candidate)?;
        Ok(true)
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
            sleeping: false,
            statuses: Vec::new(),
            grants: existing.map_or_else(Default::default, |entry| entry.peer.grants.clone()),
            plugin_values: existing
                .map_or_else(Default::default, |entry| entry.peer.plugin_values.clone()),
        };
        // What the person typed for this device is theirs and survives a new pairing, as its grants do.
        let typed = existing.and_then(|entry| entry.typed.clone());
        candidate.peers.insert(
            id,
            Remembered {
                peer,
                address,
                typed,
            },
        );
        self.publish(candidate)
    }

    /// Set where the person says a remembered device can be found, or clear it with `None`. Only a device that is already remembered, and only
    /// this: no identity, grant or other address is touched. Returns whether anything changed; nothing is written when it did not.
    pub fn set_typed(
        &mut self,
        id: &PeerId,
        typed: Option<peersus_core::address::TypedAddress>,
    ) -> io::Result<bool> {
        let remembered = self.document.peers.get(&id.0).ok_or_else(invalid)?;
        if remembered.typed == typed {
            return Ok(false);
        }
        let mut candidate = self.document.clone();
        candidate.peers.get_mut(&id.0).ok_or_else(invalid)?.typed = typed;
        self.publish(candidate)?;
        Ok(true)
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

    /// Keep, on every device, only those of ONE plugin's grants that are in `asked`: what a store update of it still asks for
    /// (`peersus_core::plugins::Installation::StoreUpdate`). One the new version no longer asks for goes, so that a later
    /// version asking for it again asks the person again. Returns how many permissions went; nothing is written when none did.
    pub fn narrow_plugin(
        &mut self,
        plugin: &PluginId,
        asked: &BTreeSet<peersus_api::permission::Permission>,
    ) -> io::Result<usize> {
        let mut candidate = self.document.clone();
        let mut removed = 0;
        for remembered in candidate.peers.values_mut() {
            let grants = &mut remembered.peer.grants;
            let Some(given) = grants.get(plugin) else {
                continue;
            };
            let kept: Vec<_> = given
                .iter()
                .filter(|p| asked.contains(p))
                .cloned()
                .collect();
            removed += given.iter().count() - kept.len();
            if kept.is_empty() {
                grants.remove(plugin);
            } else {
                grants.insert(plugin.clone(), Grants::new(kept).map_err(|_| invalid())?);
            }
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

/// Whether a device may call itself this: not blank, at most 128 bytes, no control characters. The one rule, at pairing and when a device
/// later says that it has been renamed, so a name that could not be given then cannot be given now either.
pub fn is_valid_name(name: &str) -> bool {
    !(name.trim().is_empty() || name.len() > 128 || name.chars().any(char::is_control))
}

fn validate_name(name: &str) -> io::Result<()> {
    if !is_valid_name(name) {
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
    fn perm(owner: &str, name: &str) -> peersus_api::permission::Permission {
        peersus_api::permission::Permission::new(owner, name).unwrap()
    }
    fn plugin(name: &str) -> PluginId {
        PluginId::new(name).unwrap()
    }
    fn toggle(id: &str, scope: Scope) -> Setting {
        use peersus_api::contributions::Kind;
        Setting {
            public_read: None,
            platforms: Vec::new(),
            id: id.into(),
            label: id.into(),
            help: None,
            scope,
            kind: Kind::Toggle { default: false },
        }
    }

    #[test]
    fn the_store_learns_an_address_by_adding_it_and_only_for_a_device_it_already_remembers() {
        let directory = tempfile::tempdir().unwrap();
        let (mut store, id, _, remote) = remembered(&directory.path().join("peers.json"), 100);
        let relay: iroh::RelayUrl = "https://relay.example.org/".parse().unwrap();
        store
            .update_address(
                EndpointAddr::new(remote)
                    .with_relay_url(relay)
                    .with_ip_addr("192.0.2.1:4000".parse().unwrap()),
            )
            .unwrap();
        let ips = |store: &Peers| -> Vec<std::net::SocketAddr> {
            store
                .get(&id)
                .unwrap()
                .address
                .ip_addrs()
                .copied()
                .collect()
        };
        let relays = |store: &Peers| store.get(&id).unwrap().address.relay_urls().count();

        // Added to what is held, and the relay is kept: an address seen on a connection must never make the device harder to reach.
        assert!(
            store
                .learn_address(&id, "198.51.100.7:5000".parse().unwrap())
                .unwrap()
        );
        assert_eq!(
            ips(&store).len(),
            2,
            "the address was not added to the one already held"
        );
        assert_eq!(relays(&store), 1, "learning an address lost the relay");
        // The same one again is nothing: nothing is written.
        assert!(
            !store
                .learn_address(&id, "198.51.100.7:5000".parse().unwrap())
                .unwrap()
        );
        // Only a device that is remembered: nobody is introduced by being seen.
        assert!(
            store
                .learn_address(
                    &PeerId("nobody".into()),
                    "198.51.100.8:5000".parse().unwrap()
                )
                .is_err()
        );
        // An address that cannot be dialled is refused, and nothing changed.
        for bad in ["0.0.0.0:5000", "224.0.0.1:5000", "198.51.100.9:0"] {
            assert!(
                store.learn_address(&id, bad.parse().unwrap()).is_err(),
                "{bad} was learnt"
            );
        }
        assert_eq!(ips(&store).len(), 2);
        // Past its bound the newest stays and the ones before it go, and the relay stays: a device that moved has left the places it was at.
        for port in 5001..5010 {
            store
                .learn_address(&id, format!("198.51.100.7:{port}").parse().unwrap())
                .unwrap();
        }
        assert!(
            ips(&store).len() <= Peers::MAX_DIRECT_ADDRESSES,
            "{:?}",
            ips(&store)
        );
        assert!(
            ips(&store).contains(&"198.51.100.7:5009".parse().unwrap()),
            "the newest address was not kept"
        );
        assert_eq!(relays(&store), 1, "making room lost the relay");
        // What was learnt is on disc: it is what the next start will use.
        let reopened = Peers::open(directory.path().join("peers.json")).unwrap();
        assert!(
            reopened
                .get(&id)
                .unwrap()
                .address
                .ip_addrs()
                .any(|a| a.to_string() == "198.51.100.7:5009")
        );
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
    fn narrowing_keeps_only_what_is_still_asked_for_on_every_device_and_on_disk() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("peers.json");
        let (mut store, id, _, _) = remembered(&path, 100);
        store
            .set_grants(
                &id,
                plugin("upd"),
                Grants::new([perm("host", "a"), perm("host", "b")]).unwrap(),
            )
            .unwrap();
        store
            .set_grants(
                &id,
                plugin("other"),
                Grants::new([perm("host", "b")]).unwrap(),
            )
            .unwrap();
        let asked: std::collections::BTreeSet<_> = [perm("host", "a"), perm("host", "c")].into();
        assert_eq!(store.narrow_plugin(&plugin("upd"), &asked).unwrap(), 1);
        let reopened = Peers::open(&path).unwrap();
        let peer = &reopened.get(&id).unwrap().peer;
        let kept = peer.grants.get(&plugin("upd")).unwrap();
        // Kept what is still asked for, dropped what is not, and never granted what is newly asked for.
        assert!(
            kept.allows(&perm("host", "a"))
                && !kept.allows(&perm("host", "b"))
                && !kept.allows(&perm("host", "c"))
        );
        // Another plugin's grants are not touched.
        assert!(
            peer.grants
                .get(&plugin("other"))
                .unwrap()
                .allows(&perm("host", "b"))
        );
        // Nothing still asked for: the entry goes, so default-deny is absence.
        assert_eq!(
            store
                .narrow_plugin(&plugin("upd"), &Default::default())
                .unwrap(),
            1
        );
        assert!(
            !Peers::open(&path)
                .unwrap()
                .get(&id)
                .unwrap()
                .peer
                .grants
                .contains_key(&plugin("upd"))
        );
        assert_eq!(store.narrow_plugin(&plugin("upd"), &asked).unwrap(), 0);
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
