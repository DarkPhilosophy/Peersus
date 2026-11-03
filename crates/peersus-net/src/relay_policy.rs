//! Live home-relay priority. Never rebind or close a healthy direct connection.
use std::time::Duration;

use iroh::{Endpoint, Watcher};
use peersus_core::{
    NetworkSettings,
    relay::{RelayDiagnostics, RelayHealth, RelayPolicy},
};
use tokio::time::Instant;

use crate::transport::{self, RelayAccess};

pub(crate) struct Coordinator {
    policy: RelayPolicy,
    tier: usize,
    unavailable_since: Option<Instant>,
    primary_attempt: Instant,
    last_diagnostics: RelayDiagnostics,
}

impl Coordinator {
    pub(crate) fn new(settings: &NetworkSettings) -> Option<Self> {
        let policy = settings.effective_relay_policy();
        let tier = policy.enabled_targets().next()?.0;
        let now = Instant::now();
        Some(Self {
            policy,
            tier,
            unavailable_since: Some(now),
            primary_attempt: now,
            last_diagnostics: RelayDiagnostics::default(),
        })
    }

    pub(crate) fn diagnostics(&self, endpoint: &Endpoint) -> RelayDiagnostics {
        let current = transport::relay_map(&self.policy.targets[self.tier], None)
            .expect("validated relay policy");
        let statuses = endpoint.home_relay_status().get();
        let matching = || {
            statuses
                .iter()
                .filter(|status| current.contains(status.url()))
        };
        let health = if matching().any(|status| status.is_connected()) {
            RelayHealth::Connected
        } else if matching().any(|status| status.auth_denied_reason().is_some()) {
            RelayHealth::Refused
        } else if matching().any(|status| status.last_error().is_some()) {
            RelayHealth::Unavailable
        } else {
            RelayHealth::Connecting
        };
        RelayDiagnostics {
            active_target: Some(self.tier),
            health,
        }
    }

    /// Called by the host's existing supervised event loop, not by a detached task.
    pub(crate) async fn tick(
        &mut self,
        endpoint: &Endpoint,
        access: Option<&RelayAccess>,
        may_retry_primary: bool,
    ) -> Result<bool, transport::Error> {
        let current = transport::relay_map(&self.policy.targets[self.tier], access)?;
        let statuses = endpoint.home_relay_status().get();
        let healthy = statuses
            .iter()
            .any(|status| current.contains(status.url()) && status.is_connected());
        let refused = statuses
            .iter()
            .any(|status| current.contains(status.url()) && status.auth_denied_reason().is_some());
        let diagnostics = self.diagnostics(endpoint);
        let changed = diagnostics != self.last_diagnostics;
        self.last_diagnostics = diagnostics;
        let Some(next) = self.choose(Instant::now(), healthy, refused, may_retry_primary) else {
            return Ok(changed);
        };
        let replacement = transport::relay_map(&self.policy.targets[next], access)?;
        // One tier at a time: giving iroh the entire list would let latency override user order.
        // Insertion precedes removal so there is always a configured relay transport.
        for config in replacement.relays::<Vec<_>>() {
            endpoint.insert_relay(config.url.clone(), config).await;
        }
        for url in current.urls::<Vec<_>>() {
            if !replacement.contains(&url) {
                endpoint.remove_relay(&url).await;
            }
        }
        self.tier = next;
        self.unavailable_since = Some(Instant::now());
        if Some(next) == self.policy.enabled_targets().next().map(|(index, _)| index) {
            self.primary_attempt = Instant::now();
        }
        self.last_diagnostics = self.diagnostics(endpoint);
        Ok(true)
    }

    fn choose(
        &mut self,
        now: Instant,
        healthy: bool,
        refused: bool,
        may_retry_primary: bool,
    ) -> Option<usize> {
        if self.policy.enabled_targets().count() < 2 {
            return None;
        }
        if healthy {
            self.unavailable_since = None;
            // A preferred-tier probe must never interrupt application traffic using a fallback.
            let primary = self.policy.enabled_targets().next()?.0;
            return (self.tier != primary
                && may_retry_primary
                && now.duration_since(self.primary_attempt)
                    >= Duration::from_millis(u64::from(self.policy.retry_primary_ms)))
            .then_some(primary);
        }
        let since = *self.unavailable_since.get_or_insert(now);
        if refused
            || now.duration_since(since)
                >= Duration::from_millis(u64::from(self.policy.failover_wait_ms))
        {
            self.policy
                .enabled_targets()
                .map(|(index, _)| index)
                .find(|index| *index > self.tier)
                .or_else(|| self.policy.enabled_targets().next().map(|(index, _)| index))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peersus_core::relay::RelayTarget;

    fn coordinator() -> Coordinator {
        Coordinator::new(&NetworkSettings {
            relay_policy: Some(RelayPolicy {
                targets: vec![
                    RelayTarget::Custom("https://first.example".into()),
                    RelayTarget::Project("https://second.example".into()),
                    RelayTarget::Public,
                ],
                ..RelayPolicy::default()
            }),
            ..NetworkSettings::default()
        })
        .unwrap()
    }
    #[test]
    fn disabled_relays_are_skipped_on_start_failover_and_primary_retry() {
        let mut settings = NetworkSettings {
            relay_policy: Some(coordinator().policy),
            ..Default::default()
        };
        settings.relay_policy.as_mut().unwrap().disabled.insert(0);
        let mut coordinator = Coordinator::new(&settings).unwrap();
        assert_eq!(coordinator.tier, 1);
        assert_eq!(
            coordinator.choose(coordinator.primary_attempt, false, true, true),
            Some(2)
        );
        coordinator.tier = 2;
        let later = coordinator.primary_attempt + Duration::from_secs(61);
        assert_eq!(coordinator.choose(later, true, false, true), Some(1));
        settings
            .relay_policy
            .as_mut()
            .unwrap()
            .disabled
            .extend([1, 2]);
        assert!(Coordinator::new(&settings).is_none());
    }

    #[test]
    fn a_healthy_preferred_tier_never_falls_back_just_because_time_passes() {
        let mut coordinator = coordinator();
        let later = coordinator.primary_attempt + Duration::from_secs(3600);
        assert_eq!(coordinator.choose(later, true, false, true), None);
    }

    #[test]
    fn failures_advance_in_user_order_and_a_new_failure_gets_its_own_deadline() {
        let mut coordinator = coordinator();
        let now = coordinator.primary_attempt;
        assert_eq!(
            coordinator.choose(now + Duration::from_secs(4), false, false, true),
            None
        );
        assert_eq!(
            coordinator.choose(now + Duration::from_secs(5), false, false, true),
            Some(1)
        );
        coordinator.tier = 1;
        coordinator.unavailable_since = Some(now + Duration::from_secs(5));
        assert_eq!(
            coordinator.choose(now + Duration::from_secs(9), false, false, true),
            None
        );
        assert_eq!(
            coordinator.choose(now + Duration::from_secs(10), false, false, true),
            Some(2)
        );
    }

    #[test]
    fn recovering_the_primary_never_preempts_an_active_relay_transfer() {
        let mut coordinator = coordinator();
        coordinator.tier = 2;
        let later = coordinator.primary_attempt + Duration::from_secs(61);
        assert_eq!(coordinator.choose(later, true, false, false), None);
        assert_eq!(coordinator.choose(later, true, false, true), Some(0));
    }

    #[test]
    fn auth_refusal_advances_only_to_explicitly_configured_fallbacks() {
        let mut coordinator = coordinator();
        assert_eq!(
            coordinator.choose(coordinator.primary_attempt, false, true, true),
            Some(1)
        );
        coordinator.policy.targets.truncate(1);
        assert_eq!(
            coordinator.choose(coordinator.primary_attempt, false, true, true),
            None
        );
    }

    #[derive(Debug)]
    struct LocalAccess {
        allow: std::sync::Arc<std::sync::atomic::AtomicBool>,
        saw_expected_token: std::sync::Arc<std::sync::atomic::AtomicBool>,
        wrong_token: std::sync::Arc<std::sync::atomic::AtomicBool>,
        expects_token: bool,
    }

    impl iroh_relay::server::AccessControl for LocalAccess {
        async fn on_connect(
            &self,
            request: &iroh_relay::server::ClientRequest,
        ) -> iroh_relay::server::Access {
            use std::sync::atomic::Ordering;
            let token = request.auth_token();
            let expected = if self.expects_token {
                Some("local-test-only")
            } else {
                None
            };
            self.saw_expected_token
                .store(token.as_deref() == expected, Ordering::SeqCst);
            if token.as_deref() != expected {
                self.wrong_token.store(true, Ordering::SeqCst);
            }
            if self.allow.load(Ordering::SeqCst) {
                iroh_relay::server::Access::Allow
            } else {
                iroh_relay::server::Access::Deny {
                    reason: Some("local test refusal".into()),
                }
            }
        }
    }

    async fn wait_for_status(endpoint: &Endpoint, url: &iroh::RelayUrl, connected: bool) {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if endpoint.home_relay_status().get().iter().any(|status| {
                    status.url() == url
                        && if connected {
                            status.is_connected()
                        } else {
                            status.auth_denied_reason().is_some()
                        }
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("local TLS relay did not reach the expected state");
    }

    #[tokio::test]
    async fn local_tls_relays_verify_refusal_failover_recovery_and_credential_scope() {
        use iroh::endpoint::{Builder, presets};
        use iroh_relay::{
            server::{CertConfig, RelayConfig, Server, ServerConfig, TlsConfig, testing},
            tls::CaTlsConfig,
        };
        use std::{
            net::Ipv4Addr,
            sync::{
                Arc,
                atomic::{AtomicBool, Ordering},
            },
        };
        let (certificates, tls) = testing::self_signed_tls_certs_and_config();
        let allow_primary = Arc::new(AtomicBool::new(false));
        let saw_primary = Arc::new(AtomicBool::new(false));
        let saw_fallback = Arc::new(AtomicBool::new(false));
        let wrong_token = Arc::new(AtomicBool::new(false));
        let spawn_relay = |allow, saw_expected_token, expects_token| {
            let mut relay = RelayConfig::new((Ipv4Addr::LOCALHOST, 0));
            relay.tls = Some(TlsConfig::new(
                (Ipv4Addr::LOCALHOST, 0),
                CertConfig::Manual {
                    server_config: tls.clone(),
                },
            ));
            relay.access = Arc::new(LocalAccess {
                allow,
                saw_expected_token,
                wrong_token: wrong_token.clone(),
                expects_token,
            });
            let mut config = ServerConfig::default();
            config.relay = Some(relay);
            Server::spawn(config)
        };
        let primary = spawn_relay(allow_primary.clone(), saw_primary.clone(), true)
            .await
            .unwrap();
        let fallback = spawn_relay(Arc::new(AtomicBool::new(true)), saw_fallback.clone(), false)
            .await
            .unwrap();
        let primary_url = primary.https_url().unwrap();
        let fallback_url = fallback.https_url().unwrap();
        let settings = NetworkSettings {
            relay_policy: Some(RelayPolicy {
                targets: vec![
                    RelayTarget::Custom(primary_url.to_string()),
                    RelayTarget::Project(fallback_url.to_string()),
                ],
                failover_wait_ms: 500,
                ..RelayPolicy::default()
            }),
            ..NetworkSettings::default()
        };
        assert!(settings.effective_relay_policy().is_valid());
        let access = RelayAccess::new("local-test-only");
        let endpoint = Builder::new(presets::Minimal)
            .secret_key(iroh::SecretKey::generate())
            .relay_mode(iroh::RelayMode::Custom(
                transport::relay_map(
                    &settings.effective_relay_policy().targets[0],
                    access.as_ref(),
                )
                .unwrap(),
            ))
            // Trust only this ephemeral local certificate; never disable verification.
            .ca_tls_config(CaTlsConfig::custom_roots(certificates))
            .bind()
            .await
            .unwrap();
        let mut coordinator = Coordinator::new(&settings).unwrap();
        wait_for_status(&endpoint, &primary_url, false).await;
        assert_eq!(
            coordinator.diagnostics(&endpoint),
            RelayDiagnostics {
                active_target: Some(0),
                health: RelayHealth::Refused,
            }
        );
        assert!(
            coordinator
                .tick(&endpoint, access.as_ref(), true)
                .await
                .unwrap()
        );
        assert_eq!(coordinator.tier, 1);
        wait_for_status(&endpoint, &fallback_url, true).await;
        assert_eq!(
            coordinator.diagnostics(&endpoint),
            RelayDiagnostics {
                active_target: Some(1),
                health: RelayHealth::Connected,
            }
        );
        // A health update on the same tier must publish a fresh snapshot once, not every tick.
        assert!(
            coordinator
                .tick(&endpoint, access.as_ref(), false)
                .await
                .unwrap()
        );
        assert!(
            !coordinator
                .tick(&endpoint, access.as_ref(), false)
                .await
                .unwrap()
        );
        assert!(saw_primary.load(Ordering::SeqCst));
        assert!(saw_fallback.load(Ordering::SeqCst));
        assert!(!wrong_token.load(Ordering::SeqCst));

        // Recovery is postponed while a fallback transfer is active.
        allow_primary.store(true, Ordering::SeqCst);
        coordinator.primary_attempt = Instant::now() - Duration::from_secs(61);
        assert!(
            !coordinator
                .tick(&endpoint, access.as_ref(), false)
                .await
                .unwrap()
        );
        assert_eq!(coordinator.tier, 1);
        assert!(
            coordinator
                .tick(&endpoint, access.as_ref(), true)
                .await
                .unwrap()
        );
        wait_for_status(&endpoint, &primary_url, true).await;
        assert_eq!(coordinator.tier, 0);

        primary.shutdown().await.unwrap();
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                coordinator
                    .tick(&endpoint, access.as_ref(), false)
                    .await
                    .unwrap();
                if coordinator.tier == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("stopped primary did not fail over");
        assert_eq!(coordinator.tier, 1);
        wait_for_status(&endpoint, &fallback_url, true).await;
        assert!(!wrong_token.load(Ordering::SeqCst));
        endpoint.close().await;
        fallback.shutdown().await.unwrap();
    }
}
