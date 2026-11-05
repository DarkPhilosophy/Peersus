//! Ordered, user-controlled fallbacks. Direct IP connectivity is always preferred.
use serde::{Deserialize, Serialize};

/// The project relay is a configurable preference, not an assertion of server availability.
pub const PROJECT_RELAY_URL: &str = "https://peersus.axxa.dev";
/// How many relays a list may hold. Generous enough for any personal setup; still a bound, so a corrupted or hostile settings file
/// cannot make the network layer open an unbounded number of connections.
pub const MAX_RELAY_TARGETS: usize = 65_536;

/// One relay's own timings, replacing the list-wide ones for that relay only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayTimings {
    /// How long this relay is tried while unavailable before the next one is used.
    pub failover_wait_ms: u32,
    /// How long a fallback is used before this relay, if it is the first, is tried again.
    pub retry_primary_ms: u32,
}

impl RelayTimings {
    fn is_valid(&self) -> bool {
        (500..=60_000).contains(&self.failover_wait_ms)
            && (5_000..=3_600_000).contains(&self.retry_primary_ms)
    }
}

/// Sanitized live home-relay health. No server error text, URL or credential is exposed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RelayHealth {
    #[default]
    Disabled,
    Connecting,
    Connected,
    Unavailable,
    Refused,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayDiagnostics {
    /// Zero-based position in the configured fallback list; not a peer's traffic path.
    pub active_target: Option<usize>,
    pub health: RelayHealth,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "url",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum RelayTarget {
    /// A personal relay. Only this target may receive the locally stored access token.
    Custom(String),
    /// The project's relay; its address can be changed without rebuilding the application.
    Project(String),
    /// The public n0 relay pool, considered only at this position in the fallback list.
    Public,
}

impl RelayTarget {
    pub fn url(&self) -> Option<&str> {
        match self {
            Self::Custom(url) | Self::Project(url) => Some(url),
            Self::Public => None,
        }
    }

    /// The key a test of this relay is reported under: its address, or `n0` for the public pool. Stable when the list is reordered.
    pub fn probe_key(&self) -> String {
        self.url().map_or_else(
            || "n0".to_owned(),
            |url| url.trim_end_matches('/').to_owned(),
        )
    }

    /// Whether the person may delete this entry. The project relay and the public pool can be disabled or reordered, never removed.
    pub fn removable(&self) -> bool {
        matches!(self, Self::Custom(_))
    }

    /// The short name a person recognises: the public pool's operator, or the site an address belongs to (`https://peersus.axxa.dev`
    /// is `axxa.dev`). The full address stays in the target and is what is dialled.
    pub fn display_name(&self) -> String {
        match self.url() {
            None => "n0".to_owned(),
            Some(url) => site_of(url),
        }
    }
}

/// The registrable part of an address's host: its last two labels, or the host itself when it is an IP address or a single label.
fn site_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = host.rsplit_once('@').map_or(host, |(_, host)| host);
    let host = if host.starts_with('[') {
        host.split_inclusive(']').next().unwrap_or(host)
    } else {
        host.rsplit_once(':')
            .filter(|(_, port)| port.chars().all(|c| c.is_ascii_digit()))
            .map_or(host, |(name, _)| name)
    };
    if host.starts_with('[') || host.parse::<std::net::IpAddr>().is_ok() {
        return host.to_owned();
    }
    let labels: Vec<&str> = host.split('.').filter(|label| !label.is_empty()).collect();
    match labels.len() {
        0 => host.to_owned(),
        1 => labels[0].to_owned(),
        n => format!("{}.{}", labels[n - 2], labels[n - 1]),
    }
}

/// How the enabled relays are used when a direct path is not available.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RelayOrder {
    /// All enabled relays are offered together and the network picks the nearest that answers.
    Automatic,
    /// The list order is the preference: a fallback is used only while those above it are unavailable, and the first is tried
    /// again after a while.
    #[default]
    Priority,
    /// Down the list in order: a relay in use is kept until it fails, then the next one is taken.
    Sequential,
    /// The enabled relays are measured and the fastest that answers is used; they are measured again when it fails.
    Latency,
}

impl RelayOrder {
    pub const ALL: [RelayOrder; 4] = [
        Self::Automatic,
        Self::Priority,
        Self::Sequential,
        Self::Latency,
    ];
}

/// List order is priority, not a latency preference. Empty means direct-only.
/// Missing policy in an older settings file is migrated separately, without adding servers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RelayPolicy {
    pub targets: Vec<RelayTarget>,
    /// Disabled positions stay in the ordered preference list, retaining their address and rank.
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub disabled: std::collections::BTreeSet<usize>,
    /// Maximum initial opportunity for a direct path before application data may use a relay.
    pub direct_wait_ms: u32,
    /// How long an unavailable tier is tried before advancing to the next one.
    pub failover_wait_ms: u32,
    /// How long to wait before checking a preferred tier again, to avoid rapid oscillation.
    pub retry_primary_ms: u32,
    /// How the enabled relays are chosen among. Older settings without it keep the list-priority behaviour.
    #[serde(default)]
    pub order: RelayOrder,
    /// Timings set for single relays, by [`RelayTarget::probe_key`], so they follow the relay when the list is reordered. A relay
    /// without an entry uses the list-wide timings.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub timings: std::collections::BTreeMap<String, RelayTimings>,
}

impl Default for RelayPolicy {
    fn default() -> Self {
        Self {
            targets: vec![
                RelayTarget::Project(PROJECT_RELAY_URL.into()),
                RelayTarget::Public,
            ],
            disabled: Default::default(),
            direct_wait_ms: 3000,
            failover_wait_ms: 5000,
            retry_primary_ms: 60_000,
            order: RelayOrder::default(),
            timings: Default::default(),
        }
    }
}

impl RelayPolicy {
    pub fn enabled_targets(&self) -> impl Iterator<Item = (usize, &RelayTarget)> {
        self.targets
            .iter()
            .enumerate()
            .filter(|(index, _)| !self.disabled.contains(index))
    }

    pub fn has_enabled_targets(&self) -> bool {
        self.enabled_targets().next().is_some()
    }

    /// The timings of the relay at `index`: its own if it has them, else the list-wide ones.
    pub fn timings_of(&self, index: usize) -> RelayTimings {
        self.targets
            .get(index)
            .and_then(|target| self.timings.get(&target.probe_key()))
            .copied()
            .unwrap_or(RelayTimings {
                failover_wait_ms: self.failover_wait_ms,
                retry_primary_ms: self.retry_primary_ms,
            })
    }

    /// Move the enabled state together with its target, never with the old position.
    pub fn swap_targets(&mut self, from: usize, to: usize) {
        if from >= self.targets.len() || to >= self.targets.len() {
            return;
        }
        self.targets.swap(from, to);
        let from_disabled = self.disabled.remove(&from);
        let to_disabled = self.disabled.remove(&to);
        if from_disabled {
            self.disabled.insert(to);
        }
        if to_disabled {
            self.disabled.insert(from);
        }
    }

    /// Refuse duplicate services and secret-bearing or unusable addresses before opening sockets.
    pub fn is_valid(&self) -> bool {
        if self.targets.len() > MAX_RELAY_TARGETS
            || self
                .disabled
                .iter()
                .any(|index| *index >= self.targets.len())
            || !(100..=30_000).contains(&self.direct_wait_ms)
            || !(500..=60_000).contains(&self.failover_wait_ms)
            || !(5_000..=3_600_000).contains(&self.retry_primary_ms)
            || self.timings.values().any(|timings| !timings.is_valid())
            || self
                .timings
                .keys()
                .any(|key| !self.targets.iter().any(|target| target.probe_key() == *key))
        {
            return false;
        }
        for (index, target) in self.targets.iter().enumerate() {
            if target
                .url()
                .is_some_and(|url| !crate::config::valid_relay_url(url))
            {
                return false;
            }
            if self.targets[..index]
                .iter()
                .any(|prior| match (prior.url(), target.url()) {
                    (Some(a), Some(b)) => a.trim_end_matches('/') == b.trim_end_matches('/'),
                    (None, None) => true,
                    _ => false,
                })
            {
                return false;
            }
        }
        true
    }

    /// The one personal relay the stored access token belongs to: the first personal relay in the list. Every other target,
    /// personal ones included, is dialled without it, so the credential never reaches a fallback it was not made for.
    pub fn token_target(&self) -> Option<usize> {
        self.targets
            .iter()
            .position(|target| matches!(target, RelayTarget::Custom(_)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_relay_is_named_by_its_site_and_only_personal_relays_can_be_removed() {
        assert_eq!(
            RelayTarget::Project(PROJECT_RELAY_URL.into()).display_name(),
            "axxa.dev"
        );
        assert_eq!(RelayTarget::Public.display_name(), "n0");
        for (url, name) in [
            ("https://relay.example.org", "example.org"),
            ("https://relay.example.org:8443/path?x=1", "example.org"),
            ("https://user@relay.example.org", "example.org"),
            ("https://localhost", "localhost"),
            ("https://192.0.2.7:443", "192.0.2.7"),
            ("https://[2001:db8::1]:443", "[2001:db8::1]"),
        ] {
            assert_eq!(
                RelayTarget::Custom(url.into()).display_name(),
                name,
                "{url}"
            );
        }
        assert!(RelayTarget::Custom("https://relay.example.org".into()).removable());
        assert!(!RelayTarget::Project(PROJECT_RELAY_URL.into()).removable());
        assert!(!RelayTarget::Public.removable());
    }

    #[test]
    fn several_personal_relays_are_valid_and_the_token_belongs_to_the_first_only() {
        let mut policy = RelayPolicy::default();
        policy
            .targets
            .push(RelayTarget::Custom("https://one.example.org".into()));
        policy
            .targets
            .push(RelayTarget::Custom("https://two.example.org".into()));
        assert!(policy.is_valid());
        assert_eq!(policy.token_target(), Some(2));
        assert_eq!(RelayPolicy::default().token_target(), None);
    }

    #[test]
    fn older_settings_without_an_order_keep_list_priority() {
        let old = serde_json::json!({
            "targets": [{"kind": "project", "url": PROJECT_RELAY_URL}, {"kind": "public"}],
            "direct_wait_ms": 3000, "failover_wait_ms": 5000, "retry_primary_ms": 60000
        });
        let policy: RelayPolicy = serde_json::from_value(old).unwrap();
        assert_eq!(policy.order, RelayOrder::Priority);
        let mut changed = policy.clone();
        changed.order = RelayOrder::Latency;
        let restored: RelayPolicy =
            serde_json::from_str(&serde_json::to_string(&changed).unwrap()).unwrap();
        assert_eq!(restored.order, RelayOrder::Latency);
    }

    #[test]
    fn disabling_and_reordering_preserve_targets_and_survive_serialization() {
        let mut policy = RelayPolicy::default();
        let original = policy.targets[0].clone();
        policy.disabled.insert(0);
        assert!(policy.is_valid());
        assert_eq!(policy.enabled_targets().next().unwrap().0, 1);
        policy.swap_targets(0, 1);
        assert_eq!(policy.targets[1], original);
        assert!(policy.disabled.contains(&1));
        assert!(!policy.disabled.contains(&0));
        let mut restored: RelayPolicy =
            serde_json::from_str(&serde_json::to_string(&policy).unwrap()).unwrap();
        assert_eq!(restored, policy);
        restored.disabled.remove(&1);
        assert_eq!(restored.enabled_targets().count(), 2);
        restored.disabled.extend(0..restored.targets.len());
        assert!(!restored.has_enabled_targets());
        assert!(restored.is_valid());
        restored.disabled.insert(restored.targets.len());
        assert!(!restored.is_valid());
    }

    #[test]
    fn older_policies_enable_targets_and_the_project_relay_keeps_its_fixed_address() {
        let old: RelayPolicy = serde_json::from_value(serde_json::json!({
            "targets": [{"kind": "project", "url": "https://configured.example"}, {"kind": "public"}]
        }))
        .unwrap();
        assert_eq!(old.enabled_targets().count(), 2);
        let mut settings = crate::config::NetworkSettings {
            relay_policy: Some(old),
            ..Default::default()
        };
        // Whatever address a settings file holds for the project relay, its one fixed address is what is dialled.
        assert_eq!(
            settings.effective_relay_policy().targets[0],
            RelayTarget::Project(PROJECT_RELAY_URL.to_owned())
        );
        settings.allow_relay = false;
        let disabled = settings.effective_relay_policy();
        assert!(!disabled.has_enabled_targets());
        assert_eq!(disabled.targets.len(), 2);
    }

    #[test]
    fn defaults_enable_both_fallbacks_in_project_then_public_order() {
        let policy = RelayPolicy::default();
        assert!(policy.is_valid());
        assert!(matches!(
            policy.targets.as_slice(),
            [RelayTarget::Project(_), RelayTarget::Public]
        ));
    }

    #[test]
    fn arbitrary_order_and_direct_only_round_trip_without_reordering() {
        for targets in [
            vec![],
            vec![
                RelayTarget::Public,
                RelayTarget::Custom("https://custom.example".into()),
                RelayTarget::Project("https://project.example".into()),
            ],
            vec![RelayTarget::Custom("https://custom.example".into())],
        ] {
            let policy = RelayPolicy {
                targets,
                ..RelayPolicy::default()
            };
            assert!(policy.is_valid());
            let restored: RelayPolicy =
                serde_json::from_str(&serde_json::to_string(&policy).unwrap()).unwrap();
            assert_eq!(restored, policy);
        }
    }

    #[test]
    fn duplicates_invalid_deadlines_and_credentials_in_urls_are_refused() {
        for targets in [
            vec![RelayTarget::Public, RelayTarget::Public],
            vec![
                RelayTarget::Custom("https://same.example".into()),
                RelayTarget::Project("https://same.example/".into()),
            ],
            vec![RelayTarget::Custom(
                "https://user:password@example.test".into(),
            )],
        ] {
            assert!(
                !RelayPolicy {
                    targets,
                    ..RelayPolicy::default()
                }
                .is_valid()
            );
        }
        assert!(
            !RelayPolicy {
                direct_wait_ms: 0,
                ..RelayPolicy::default()
            }
            .is_valid()
        );
        assert!(
            !RelayPolicy {
                failover_wait_ms: u32::MAX,
                ..RelayPolicy::default()
            }
            .is_valid()
        );
        assert!(
            !RelayPolicy {
                retry_primary_ms: 1,
                ..RelayPolicy::default()
            }
            .is_valid()
        );
    }
}
