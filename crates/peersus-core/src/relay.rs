//! Ordered, user-controlled fallbacks. Direct IP connectivity is always preferred.
use serde::{Deserialize, Serialize};

/// The project relay is a configurable preference, not an assertion of server availability.
pub const PROJECT_RELAY_URL: &str = "https://r.axxa.dev";
pub const MAX_RELAY_TARGETS: usize = 8;

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
        // One secure-storage credential belongs to one custom target, never to every fallback.
        self.targets
            .iter()
            .filter(|target| matches!(target, RelayTarget::Custom(_)))
            .count()
            <= 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn older_policies_enable_targets_and_old_project_default_is_migrated_only() {
        let old: RelayPolicy = serde_json::from_value(serde_json::json!({
            "targets": [{"kind": "project", "url": "https://axxa.dev"}, {"kind": "public"}]
        }))
        .unwrap();
        assert_eq!(old.enabled_targets().count(), 2);
        let mut settings = crate::config::NetworkSettings {
            relay_policy: Some(old),
            ..Default::default()
        };
        assert_eq!(
            settings.effective_relay_policy().targets[0],
            RelayTarget::Project(PROJECT_RELAY_URL.to_owned())
        );
        settings.relay_policy.as_mut().unwrap().targets[0] =
            RelayTarget::Project("https://configured.example".into());
        assert_eq!(
            settings.effective_relay_policy().targets[0],
            RelayTarget::Project("https://configured.example".into())
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
            vec![
                RelayTarget::Custom("https://one.example".into()),
                RelayTarget::Custom("https://two.example".into()),
            ],
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
