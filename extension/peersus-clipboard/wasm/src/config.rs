//! Who automatic synchronization may send to, decided from what the application says of this extension.
//!
//! Pure on purpose: it reads one reply of `peersus.config` and answers a list of device aliases. A destination needs
//! the global opt-in AND its own, AND both permissions held for exactly the right owners, AND to be online. Anything
//! of an unexpected type is refused rather than guessed, so a malformed reply can only ever send LESS.
use serde::Deserialize;

/// The extension's own id, as the application reports it. Permissions are matched against it, never against a
/// string this guest wrote, so another extension's `clipboard.use` can never count.
#[derive(Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub plugin: String,
    #[serde(default)]
    settings: Settings,
    #[serde(default)]
    peers: Vec<Device>,
}

#[derive(Default, Deserialize)]
struct Settings {
    #[serde(default)]
    automatic: bool,
}

#[derive(Default, Deserialize)]
struct DeviceSettings {
    #[serde(default)]
    sync: bool,
}

#[derive(Deserialize)]
struct Permission {
    owner: String,
    name: String,
}

#[derive(Deserialize)]
struct Device {
    device: String,
    online: bool,
    #[serde(default)]
    settings: DeviceSettings,
    #[serde(default)]
    permissions: Vec<Permission>,
}

/// Most devices a reply may list, and permissions per device: a bound on what is held, not on what is allowed.
const MAX_DEVICES: usize = 256;
const MAX_PERMISSIONS: usize = 64;

/// The application's identifier grammar: ASCII alphanumerics plus `.`, `_` and `-`, non-empty, at most 128 bytes.
/// Written again here because this guest has one dependency only; the process variant uses the shared function, and
/// both must say the same, which `tests::the_identifier_grammar_matches_the_applications` states.
fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

impl Device {
    /// Both grants, for exactly their owners: the application's send permission and this extension's own.
    fn allowed(&self, plugin: &str) -> bool {
        let holds = |owner: &str, name: &str| {
            self.permissions.iter().any(|permission| permission.owner == owner && permission.name == name)
        };
        !plugin.is_empty() && holds(plugin, "clipboard.use") && holds("host", "peers.send")
    }
}

impl Config {
    /// Parse one reply. `None` for anything that is not the expected shape or exceeds its bounds.
    pub fn parse(value: serde_json::Value) -> Option<Self> {
        let config: Self = serde_json::from_value(value).ok()?;
        let valid = config.peers.len() <= MAX_DEVICES
            && (config.plugin.is_empty() || valid_name(&config.plugin))
            && config.peers.iter().all(|peer| {
                valid_name(&peer.device)
                    && peer.permissions.len() <= MAX_PERMISSIONS
                    && peer
                        .permissions
                        .iter()
                        .all(|permission| valid_name(&permission.owner) && valid_name(&permission.name))
            });
        valid.then_some(config)
    }

    /// Whether the person has switched automatic sending on at all. Production never asks this on its own: it asks who
    /// may be sent to, and that already requires this switch. Kept to state the setting is read, in the tests.
    #[cfg(test)]
    pub fn automatic(&self) -> bool {
        self.settings.automatic
    }

    /// The devices consent covers, online or not. A change here, in either direction, is a new eligibility epoch.
    pub fn consented(&self) -> Vec<String> {
        self.eligible(false)
    }

    /// The devices that can be sent to right now.
    pub fn reachable(&self) -> Vec<String> {
        self.eligible(true)
    }

    fn eligible(&self, online_only: bool) -> Vec<String> {
        let mut destinations: Vec<String> = self
            .peers
            .iter()
            .filter(|peer| {
                self.settings.automatic
                    && (!online_only || peer.online)
                    && peer.settings.sync
                    && peer.allowed(&self.plugin)
            })
            .map(|peer| peer.device.clone())
            .collect();
        destinations.sort();
        destinations.dedup();
        destinations
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config(automatic: bool, sync: bool, online: bool, permissions: serde_json::Value) -> Config {
        Config::parse(json!({
            "plugin":"clipboard",
            "settings":{"automatic":automatic},
            "peers":[{"device":"phone","online":online,"settings":{"sync":sync},"permissions":permissions}]
        }))
        .unwrap()
    }
    fn both() -> serde_json::Value {
        json!([{"owner":"host","name":"peers.send"},{"owner":"clipboard","name":"clipboard.use"}])
    }

    #[test]
    fn automatic_and_each_destination_require_independent_opt_in() {
        let none = Config::parse(json!({"settings":{},"peers":[{"device":"one","online":true,"settings":{}}]})).unwrap();
        assert!(!none.automatic());
        assert!(none.consented().is_empty(), "nothing is switched on by default");
        assert!(config(true, false, true, both()).consented().is_empty(), "the device itself is not opted in");
        assert!(config(false, true, true, both()).consented().is_empty(), "automatic sending is off");
        assert_eq!(config(true, true, true, both()).consented(), ["phone"]);
    }

    #[test]
    fn a_wrong_type_is_refused_not_guessed() {
        assert!(Config::parse(json!({"settings":{"automatic":"yes"},"peers":[]})).is_none());
        assert!(Config::parse(json!({"settings":{},"peers":[{"device":"d","online":"yes"}]})).is_none());
    }

    #[test]
    fn future_declared_settings_do_not_disable_existing_automatic_behavior() {
        let config = Config::parse(json!({
            "plugin":"clipboard",
            "settings":{"automatic":true,"future_setting":"ignored"},
            "peers":[{"device":"one","online":true,"settings":{"sync":true,"future":12},
                      "permissions":[{"owner":"host","name":"peers.send"},{"owner":"clipboard","name":"clipboard.use"}]}]
        }))
        .unwrap();
        assert!(config.automatic());
        assert_eq!(config.consented(), ["one"]);
    }

    #[test]
    fn automatic_requires_both_grants_of_exact_owners() {
        // Another extension's permission with the same name is a different permission.
        let other = json!([{"owner":"host","name":"peers.send"},{"owner":"other","name":"clipboard.use"}]);
        assert!(config(true, true, true, other).consented().is_empty());
        // Only one of the two.
        let only_send = json!([{"owner":"host","name":"peers.send"}]);
        assert!(config(true, true, true, only_send).consented().is_empty());
        let only_own = json!([{"owner":"clipboard","name":"clipboard.use"}]);
        assert!(config(true, true, true, only_own).consented().is_empty());
        // The send permission must be the application's, not something an extension named the same.
        let forged = json!([{"owner":"clipboard","name":"peers.send"},{"owner":"clipboard","name":"clipboard.use"}]);
        assert!(config(true, true, true, forged).consented().is_empty());
    }

    #[test]
    fn an_offline_device_is_consented_but_not_reachable() {
        let offline = config(true, true, false, both());
        assert_eq!(offline.consented(), ["phone"]);
        assert!(offline.reachable().is_empty());
    }

    #[test]
    fn a_reply_without_the_extensions_own_id_grants_nothing() {
        // The extension's id is missing, so no permission can be matched to it, even ones that look right.
        let anonymous = Config::parse(json!({
            "settings":{"automatic":true},
            "peers":[{"device":"phone","online":true,"settings":{"sync":true},"permissions":[
                {"owner":"clipboard","name":"clipboard.use"},{"owner":"host","name":"peers.send"}]}]
        }))
        .unwrap();
        assert!(anonymous.consented().is_empty());
    }

    #[test]
    fn an_empty_permission_owner_refuses_the_whole_reply_and_never_grants_by_default() {
        // An empty owner is outside the identifier grammar, so the reply is refused rather than partly believed.
        assert!(Config::parse(json!({
            "plugin":"clipboard","settings":{"automatic":true},
            "peers":[{"device":"phone","online":true,"settings":{"sync":true},"permissions":[
                {"owner":"","name":"clipboard.use"},{"owner":"host","name":"peers.send"}]}]
        }))
        .is_none());
    }

    #[test]
    fn oversized_replies_are_refused_whole() {
        let many: Vec<_> = (0..257).map(|n| json!({"device":format!("d{n}"),"online":true})).collect();
        assert!(Config::parse(json!({"peers":many})).is_none());
        let permissions: Vec<_> = (0..65).map(|n| json!({"owner":"host","name":format!("p{n}")})).collect();
        assert!(Config::parse(json!({"peers":[{"device":"d","online":true,"permissions":permissions}]})).is_none());
        assert!(Config::parse(json!({"peers":[{"device":"","online":true}]})).is_none());
    }

    #[test]
    fn destinations_are_sorted_and_not_repeated() {
        let config = Config::parse(json!({
            "plugin":"clipboard","settings":{"automatic":true},
            "peers":[
                {"device":"b","online":true,"settings":{"sync":true},"permissions":[{"owner":"host","name":"peers.send"},{"owner":"clipboard","name":"clipboard.use"}]},
                {"device":"a","online":true,"settings":{"sync":true},"permissions":[{"owner":"host","name":"peers.send"},{"owner":"clipboard","name":"clipboard.use"}]},
                {"device":"a","online":true,"settings":{"sync":true},"permissions":[{"owner":"host","name":"peers.send"},{"owner":"clipboard","name":"clipboard.use"}]}
            ]
        }))
        .unwrap();
        assert_eq!(config.consented(), ["a", "b"]);
    }

    /// The same cases the application's own `name::valid` states. Both implementations must give these answers.
    #[test]
    fn the_identifier_grammar_matches_the_applications() {
        for good in ["clipboard.read", "a-b_c.1", "x"] {
            assert!(valid_name(good), "{good:?}");
        }
        assert!(valid_name(&"x".repeat(128)));
        for bad in ["", "a b", "a/b", "é", "a\n", &"x".repeat(129)] {
            assert!(!valid_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn a_name_outside_the_grammar_anywhere_in_the_reply_refuses_the_whole_reply() {
        let permitted = json!([{"owner":"host","name":"peers.send"},{"owner":"clipboard","name":"clipboard.use"}]);
        for (device, owner, name, plugin) in [
            ("pho ne", "host", "peers.send", "clipboard"),
            ("phone", "ho/st", "peers.send", "clipboard"),
            ("phone", "host", "peers\nsend", "clipboard"),
            ("phone", "host", "peers.send", "clip board"),
            ("é", "host", "peers.send", "clipboard"),
        ] {
            let reply = json!({
                "plugin":plugin,"settings":{"automatic":true},
                "peers":[
                    {"device":"fine","online":true,"settings":{"sync":true},"permissions":permitted},
                    {"device":device,"online":true,"settings":{"sync":true},"permissions":[{"owner":owner,"name":name}]}
                ]
            });
            assert!(Config::parse(reply).is_none(), "{device:?} {owner:?} {name:?} {plugin:?}");
        }
    }
}
