//! What a plugin calls a device: an opaque name that reveals nothing about the device.
//!
//! A device's real identifier is its network public key. It never changes, so handing it to
//! every plugin that asks to list devices would let any of them recognise the same device
//! across plugins and across runs. A plugin is given an alias instead: derived from a secret
//! that exists only inside this running application, mixed with the plugin and the device.
//! The same plugin sees the same alias for a device for as long as the application runs, a
//! different plugin sees a different one for the same device, and nobody can turn an alias
//! back into the key or compare it with another plugin's.
//!
//! Nothing is stored. An alias is recomputed whenever it is needed, and resolving one is
//! recomputing it for every device the user has paired and comparing, so a device that was
//! forgotten simply stops resolving.
use hmac::{Hmac, Mac};
use peersus_core::{model::PeerId, plugins::PluginId};
use sha2::Sha256;

/// How many bytes of the digest an alias keeps. Sixteen bytes is far past what could collide
/// between the few devices a person pairs, and short enough to read in a log.
const ALIAS_BYTES: usize = 16;

/// The secret every alias of this run is derived from. Fresh at each start, never written
/// anywhere, never shown to a plugin.
#[derive(Clone)]
pub struct Aliases {
    secret: [u8; 32],
}

impl Aliases {
    /// A new secret, so aliases from an earlier run mean nothing to this one.
    pub fn new() -> Self {
        use rand::RngCore;
        let mut secret = [0u8; 32];
        rand::rng().fill_bytes(&mut secret);
        Self { secret }
    }

    /// What `plugin` calls `device`.
    pub fn alias(&self, plugin: &PluginId, device: &PeerId) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.secret).expect("any key length works");
        // Length-prefixed, so that no plugin and device can be chosen to read as another pair.
        for part in [plugin.as_str().as_bytes(), device.0.as_bytes()] {
            mac.update(&(part.len() as u64).to_le_bytes());
            mac.update(part);
        }
        hex::encode(&mac.finalize().into_bytes()[..ALIAS_BYTES])
    }

    /// The device `plugin` means by `alias`, among `devices`, if it is one of them.
    pub fn resolve<'a>(
        &self,
        plugin: &PluginId,
        alias: &str,
        devices: impl IntoIterator<Item = &'a PeerId>,
    ) -> Option<&'a PeerId> {
        devices
            .into_iter()
            .find(|device| self.alias(plugin, device) == alias)
    }
}

impl Default for Aliases {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(name: &str) -> PluginId {
        PluginId::new(name).unwrap()
    }
    fn device(key: &str) -> PeerId {
        PeerId(key.to_owned())
    }

    #[test]
    fn the_same_plugin_sees_the_same_alias_for_a_device_for_as_long_as_it_runs() {
        let aliases = Aliases::new();
        assert_eq!(
            aliases.alias(&plugin("clip"), &device("key-a")),
            aliases.alias(&plugin("clip"), &device("key-a"))
        );
    }

    #[test]
    fn two_plugins_cannot_recognise_the_same_device_by_its_alias() {
        let aliases = Aliases::new();
        assert_ne!(
            aliases.alias(&plugin("clip"), &device("key-a")),
            aliases.alias(&plugin("other"), &device("key-a"))
        );
    }

    #[test]
    fn two_devices_get_different_aliases_from_one_plugin() {
        let aliases = Aliases::new();
        assert_ne!(
            aliases.alias(&plugin("clip"), &device("key-a")),
            aliases.alias(&plugin("clip"), &device("key-b"))
        );
    }

    #[test]
    fn an_alias_from_one_run_means_nothing_to_another() {
        let first = Aliases::new();
        let second = Aliases::new();
        assert_ne!(
            first.alias(&plugin("clip"), &device("key-a")),
            second.alias(&plugin("clip"), &device("key-a"))
        );
    }

    #[test]
    fn an_alias_does_not_contain_the_key_it_stands_for() {
        let aliases = Aliases::new();
        let key = "abcdef0123456789abcdef0123456789";
        let alias = aliases.alias(&plugin("clip"), &device(key));
        assert!(!alias.contains("abcdef"), "{alias} leaks the key");
        assert_eq!(alias.len(), ALIAS_BYTES * 2);
    }

    #[test]
    fn a_plugin_and_device_cannot_be_chosen_to_read_as_another_pair() {
        // Without length prefixes, ("ab", "c") and ("a", "bc") would hash the same bytes.
        let aliases = Aliases::new();
        assert_ne!(
            aliases.alias(&plugin("ab"), &device("c")),
            aliases.alias(&plugin("a"), &device("bc"))
        );
    }

    #[test]
    fn an_alias_resolves_to_its_device_and_only_for_the_plugin_it_was_given_to() {
        let aliases = Aliases::new();
        let devices = [device("key-a"), device("key-b")];
        let given = aliases.alias(&plugin("clip"), &devices[1]);
        assert_eq!(
            aliases.resolve(&plugin("clip"), &given, &devices),
            Some(&devices[1])
        );
        // Another plugin that learned the alias cannot use it.
        assert_eq!(aliases.resolve(&plugin("other"), &given, &devices), None);
    }

    #[test]
    fn a_device_that_is_no_longer_paired_stops_resolving() {
        let aliases = Aliases::new();
        let before = [device("key-a"), device("key-b")];
        let given = aliases.alias(&plugin("clip"), &before[1]);
        let after = [device("key-a")];
        assert_eq!(aliases.resolve(&plugin("clip"), &given, &after), None);
    }

    #[test]
    fn something_that_was_never_an_alias_resolves_to_nothing() {
        let aliases = Aliases::new();
        let devices = [device("key-a")];
        for guess in ["", "key-a", "0000", "d1"] {
            assert_eq!(aliases.resolve(&plugin("clip"), guess, &devices), None);
        }
    }
}
