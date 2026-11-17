//! What the user allowed a plugin that has nothing to do with any one device.
//!
//! Most permissions are about a device: "this plugin may send to that laptop". Some are not.
//! Letting a plugin restart other plugins concerns no device at all, and must be grantable on
//! a machine that has never been paired with anything. Those decisions live here, apart from
//! the preferences (which the user edits freely and a frontend replaces wholesale) and apart
//! from the devices (which come and go). Same discipline as the device store: bounded read,
//! unknown fields refused, a version that is refused rather than guessed at, every permission
//! checked on the way in and out, and a write that is either entirely old or entirely new.
use peersus_core::plugins::{Grants, PluginId};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{self, Read},
    path::PathBuf,
};

const MAX_PLUGINS: usize = 256;
const MAX_BYTES: u64 = 256 * 1024;
const VERSION: u32 = 1;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    grants: BTreeMap<PluginId, Grants>,
}

pub struct StandaloneGrants {
    path: PathBuf,
    document: Document,
}

impl StandaloneGrants {
    /// Open the saved decisions, or start with none when nothing was ever saved.
    ///
    /// Anything that is not exactly what this version writes is refused, and nothing is
    /// repaired: a decision the user did not make must not appear, and one they did make must
    /// not silently vanish.
    pub fn open(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let document = match File::open(&path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
                if bytes.len() as u64 > MAX_BYTES {
                    return Err(invalid());
                }
                let document: Document = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
                if document.version != VERSION
                    || document.grants.len() > MAX_PLUGINS
                    || document.grants.values().any(Grants::is_empty)
                {
                    return Err(invalid());
                }
                document
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Document {
                version: VERSION,
                grants: BTreeMap::new(),
            },
            Err(error) => return Err(error),
        };
        Ok(Self { path, document })
    }

    /// Everything granted, by plugin.
    pub fn all(&self) -> &BTreeMap<PluginId, Grants> {
        &self.document.grants
    }

    /// What one plugin was granted; nothing when it has no entry.
    pub fn get(&self, plugin: &PluginId) -> Option<&Grants> {
        self.document.grants.get(plugin)
    }

    /// Replace what one plugin is granted. Empty grants remove the entry, so that default-deny
    /// is represented by absence, never by an entry that says nothing.
    pub fn set(&mut self, plugin: PluginId, grants: Grants) -> io::Result<()> {
        let mut candidate = self.document.clone();
        if grants.is_empty() {
            candidate.grants.remove(&plugin);
        } else {
            if !candidate.grants.contains_key(&plugin) && candidate.grants.len() >= MAX_PLUGINS {
                return Err(io::Error::other("Plugin grant limit reached"));
            }
            candidate.grants.insert(plugin, grants);
        }
        self.publish(candidate)
    }

    /// Forget what was granted to every plugin that is not in `keep`. Returns how many plugins
    /// lost an entry; nothing is written when none did. See `Peers::retain_plugins` for why.
    pub fn retain_plugins(&mut self, keep: &BTreeSet<PluginId>) -> io::Result<usize> {
        let mut candidate = self.document.clone();
        let before = candidate.grants.len();
        candidate.grants.retain(|id, _| keep.contains(id));
        let removed = before - candidate.grants.len();
        if removed > 0 {
            self.publish(candidate)?;
        }
        Ok(removed)
    }

    /// Forget what was granted to ONE plugin outright. Returns how many went (0 or 1); nothing is
    /// written when none did. The same rule as [`StandaloneGrants::retain_plugins`], for one name.
    pub fn forget_plugin(&mut self, plugin: &PluginId) -> io::Result<usize> {
        let mut candidate = self.document.clone();
        let removed = usize::from(candidate.grants.remove(plugin).is_some());
        if removed > 0 {
            self.publish(candidate)?;
        }
        Ok(removed)
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

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "Plugin grants data is invalid")
}

#[cfg(test)]
mod tests {
    use super::*;
    use peersus_api::permission::Permission;
    use std::path::Path;

    fn plugin(id: &str) -> PluginId {
        PluginId::new(id).unwrap()
    }
    fn grants(owner: &str, name: &str) -> Grants {
        Grants::new([Permission::new(owner, name).unwrap()]).unwrap()
    }
    fn path(directory: &Path) -> PathBuf {
        directory.join("grants.json")
    }

    #[test]
    fn nothing_saved_means_nothing_granted() {
        let directory = tempfile::tempdir().unwrap();
        let store = StandaloneGrants::open(path(directory.path())).unwrap();
        assert!(store.all().is_empty());
        assert!(store.get(&plugin("watchdog")).is_none());
    }

    #[test]
    fn a_decision_survives_a_restart_with_its_owner() {
        let directory = tempfile::tempdir().unwrap();
        let file = path(directory.path());
        let mut store = StandaloneGrants::open(&file).unwrap();
        store
            .set(plugin("watchdog"), grants("host", "plugins.restart"))
            .unwrap();
        let reopened = StandaloneGrants::open(&file).unwrap();
        let held = reopened.get(&plugin("watchdog")).unwrap();
        assert!(held.allows(&Permission::new("host", "plugins.restart").unwrap()));
        // The owner is part of the permission: the same name owned by the plugin is not it.
        assert!(!held.allows(&Permission::new("watchdog", "plugins.restart").unwrap()));
    }

    #[test]
    fn granting_nothing_removes_the_entry_and_it_stays_removed() {
        let directory = tempfile::tempdir().unwrap();
        let file = path(directory.path());
        let mut store = StandaloneGrants::open(&file).unwrap();
        store
            .set(plugin("watchdog"), grants("host", "plugins.restart"))
            .unwrap();
        store.set(plugin("watchdog"), Grants::default()).unwrap();
        assert!(store.get(&plugin("watchdog")).is_none());
        assert!(
            StandaloneGrants::open(&file)
                .unwrap()
                .get(&plugin("watchdog"))
                .is_none()
        );
    }

    #[test]
    fn a_decision_for_one_plugin_leaves_another_alone() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = StandaloneGrants::open(path(directory.path())).unwrap();
        store.set(plugin("a"), grants("host", "x")).unwrap();
        store.set(plugin("b"), grants("host", "y")).unwrap();
        store.set(plugin("a"), Grants::default()).unwrap();
        assert!(store.get(&plugin("a")).is_none());
        assert!(
            store
                .get(&plugin("b"))
                .unwrap()
                .allows(&Permission::new("host", "y").unwrap())
        );
    }

    #[test]
    fn a_version_this_build_does_not_write_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let file = path(directory.path());
        std::fs::write(&file, br#"{"version":2,"grants":{}}"#).unwrap();
        assert!(StandaloneGrants::open(&file).is_err());
        std::fs::write(&file, br#"{"version":0,"grants":{}}"#).unwrap();
        assert!(StandaloneGrants::open(&file).is_err());
    }

    #[test]
    fn a_field_nobody_wrote_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let file = path(directory.path());
        std::fs::write(&file, br#"{"version":1,"grants":{},"admin":true}"#).unwrap();
        assert!(StandaloneGrants::open(&file).is_err());
    }

    #[test]
    fn damaged_or_hostile_content_is_refused_and_left_on_disk() {
        let directory = tempfile::tempdir().unwrap();
        let file = path(directory.path());
        for content in [
            &b"not json"[..],
            br#"{"version":1,"grants":{"watchdog":["plugins.restart"]}}"#, // a bare name: no owner
            br#"{"version":1,"grants":{"Bad Id":[{"owner":"host","name":"x"}]}}"#,
            br#"{"version":1,"grants":{"watchdog":[]}}"#, // an entry that says nothing
        ] {
            std::fs::write(&file, content).unwrap();
            assert!(
                StandaloneGrants::open(&file).is_err(),
                "accepted: {}",
                String::from_utf8_lossy(content)
            );
            assert_eq!(
                std::fs::read(&file).unwrap(),
                content,
                "the file was touched"
            );
        }
    }

    #[test]
    fn a_file_larger_than_the_limit_is_refused_without_being_read_whole() {
        let directory = tempfile::tempdir().unwrap();
        let file = path(directory.path());
        std::fs::write(&file, vec![b' '; MAX_BYTES as usize + 1]).unwrap();
        assert!(StandaloneGrants::open(&file).is_err());
    }

    #[test]
    fn the_number_of_plugins_is_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = StandaloneGrants::open(path(directory.path())).unwrap();
        for index in 0..MAX_PLUGINS {
            store
                .set(plugin(&format!("p{index}")), grants("host", "x"))
                .unwrap();
        }
        assert!(
            store
                .set(plugin("one-too-many"), grants("host", "x"))
                .is_err()
        );
        // An existing plugin can still change its grants at the limit.
        store.set(plugin("p0"), grants("host", "y")).unwrap();
        // And withdrawing makes room again.
        store.set(plugin("p1"), Grants::default()).unwrap();
        store
            .set(plugin("one-too-many"), grants("host", "x"))
            .unwrap();
    }

    #[test]
    fn a_failed_save_changes_neither_the_disk_nor_the_memory() {
        let directory = tempfile::tempdir().unwrap();
        let file = path(directory.path());
        let mut store = StandaloneGrants::open(&file).unwrap();
        store.set(plugin("a"), grants("host", "x")).unwrap();
        // Make the replacement impossible: a directory now sits where the file is.
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        assert!(store.set(plugin("b"), grants("host", "y")).is_err());
        assert!(
            store.get(&plugin("b")).is_none(),
            "memory ran ahead of the disk"
        );
        assert!(store.get(&plugin("a")).is_some());
    }

    #[test]
    fn only_what_belongs_to_a_kept_plugin_survives_a_prune() {
        let directory = tempfile::tempdir().unwrap();
        let file = path(directory.path());
        let mut store = StandaloneGrants::open(&file).unwrap();
        store.set(plugin("kept"), grants("host", "x")).unwrap();
        store.set(plugin("gone"), grants("host", "y")).unwrap();
        let keep: std::collections::BTreeSet<PluginId> = [plugin("kept")].into();
        assert_eq!(store.retain_plugins(&keep).unwrap(), 1);
        assert!(store.get(&plugin("gone")).is_none());
        assert!(store.get(&plugin("kept")).is_some());
        // And on disk, so a restart does not bring it back.
        let reopened = StandaloneGrants::open(&file).unwrap();
        assert!(reopened.get(&plugin("gone")).is_none());
        assert!(reopened.get(&plugin("kept")).is_some());
    }

    #[test]
    fn a_prune_that_removes_nothing_writes_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let file = path(directory.path());
        let mut store = StandaloneGrants::open(&file).unwrap();
        let keep: std::collections::BTreeSet<PluginId> = [plugin("kept")].into();
        // No file yet: nothing to prune must not create one.
        assert_eq!(store.retain_plugins(&keep).unwrap(), 0);
        assert!(!file.exists(), "a prune of nothing created a file");
        store.set(plugin("kept"), grants("host", "x")).unwrap();
        let before = std::fs::metadata(&file).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(store.retain_plugins(&keep).unwrap(), 0);
        assert_eq!(
            std::fs::metadata(&file).unwrap().modified().unwrap(),
            before,
            "a prune that removed nothing rewrote the file"
        );
    }
}
