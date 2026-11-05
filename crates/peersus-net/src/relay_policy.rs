//! Live home-relay choice. Never rebind or close a healthy direct connection.
use std::time::Duration;

use iroh::{Endpoint, Watcher};
use peersus_core::{
    NetworkSettings,
    relay::{RelayDiagnostics, RelayHealth, RelayOrder, RelayPolicy, RelayTarget},
};
use tokio::time::Instant;

use crate::transport::{self, RelayAccess};

pub(crate) struct Coordinator {
    policy: RelayPolicy,
    tier: usize,
    unavailable_since: Option<Instant>,
    primary_attempt: Instant,
    last_diagnostics: RelayDiagnostics,
    /// `Latency` mode: whether the enabled relays have been measured since the tier in use was taken.
    measured: bool,
    /// `Latency` mode: a measurement still going on, which names the fastest enabled relay when it ends.
    measuring: Option<tokio::task::JoinHandle<Option<usize>>>,
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
            measured: false,
            measuring: None,
        })
    }

    /// Apply a changed relay list to the endpoint in place, without a new transport: the relays the new list uses are added
    /// before the ones it no longer uses are removed, so connections and direct paths are not disturbed. Returns the coordinator
    /// for the new list (none when it enables no relay).
    pub(crate) async fn replace(
        old: Option<&Self>,
        endpoint: &Endpoint,
        settings: &NetworkSettings,
        access: Option<&RelayAccess>,
    ) -> Result<Option<Self>, transport::Error> {
        let before = old
            .map(|coordinator| coordinator.current_map(access))
            .transpose()?;
        let mut next = Self::new(settings);
        // The relay in use stays in use if the new list still enables it: editing the list is not a reason to switch.
        if let (Some(old), Some(next)) = (old, next.as_mut())
            && let Some(key) = old.policy.targets.get(old.tier).map(RelayTarget::probe_key)
            && let Some((index, _)) = next
                .policy
                .enabled_targets()
                .find(|(_, target)| target.probe_key() == key)
        {
            next.tier = index;
        }
        let after = next
            .as_ref()
            .map(|coordinator| coordinator.current_map(access))
            .transpose()?;
        if let Some(after) = &after {
            for config in after.relays::<Vec<_>>() {
                endpoint.insert_relay(config.url.clone(), config).await;
            }
        }
        if let Some(before) = &before {
            for url in before.urls::<Vec<_>>() {
                if !after.as_ref().is_some_and(|after| after.contains(&url)) {
                    endpoint.remove_relay(&url).await;
                }
            }
        }
        Ok(next)
    }

    /// The relays iroh holds for the tier in use: every enabled one in `Automatic`, else that tier alone.
    fn current_map(
        &self,
        access: Option<&RelayAccess>,
    ) -> Result<iroh::RelayMap, transport::Error> {
        if self.policy.order == RelayOrder::Automatic {
            transport::every_enabled_map(&self.policy, access)
        } else {
            transport::target_map(&self.policy, self.tier, access)
        }
    }

    pub(crate) fn diagnostics(&self, endpoint: &Endpoint) -> RelayDiagnostics {
        let current = self.current_map(None).expect("validated relay policy");
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
        let current = self.current_map(access)?;
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
        let next = if self.policy.order == RelayOrder::Latency {
            self.latency_choice(Instant::now(), healthy, refused)
        } else {
            self.choose(Instant::now(), healthy, refused, may_retry_primary)
        };
        let Some(next) = next else {
            return Ok(changed);
        };
        let replacement = transport::target_map(&self.policy, next, access)?;
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

    /// `Latency` mode. Measuring takes seconds and the host's event loop must not wait for it, so it runs as its own task and is
    /// picked up by a later tick: the first tick and every failure of the tier in use start one; a finished one names the fastest.
    fn latency_choice(&mut self, now: Instant, healthy: bool, refused: bool) -> Option<usize> {
        if let Some(measuring) = self.measuring.as_ref()
            && measuring.is_finished()
        {
            let measuring = self.measuring.take()?;
            let fastest = futures_util::FutureExt::now_or_never(measuring)
                .and_then(Result::ok)
                .flatten();
            return fastest.filter(|fastest| *fastest != self.tier);
        }
        let failed = !healthy && self.failed_long_enough(now, refused);
        if healthy {
            self.unavailable_since = None;
        }
        if self.measuring.is_none() && (!self.measured || failed) {
            self.measured = true;
            let enabled: Vec<_> = self
                .policy
                .enabled_targets()
                .map(|(index, target)| (index, target.clone()))
                .collect();
            self.measuring = Some(tokio::spawn(async move {
                let times = futures_util::future::join_all(
                    enabled
                        .iter()
                        .map(|(_, target)| crate::relay_probe::probe(target)),
                )
                .await;
                enabled
                    .iter()
                    .zip(times)
                    .filter_map(|((index, _), time)| Some((time?, *index)))
                    .min()
                    .map(|(_, index)| index)
            }));
        }
        None
    }

    /// Whether the tier in use has been unavailable long enough (or refused us) to leave it.
    fn failed_long_enough(&mut self, now: Instant, refused: bool) -> bool {
        let since = *self.unavailable_since.get_or_insert(now);
        refused
            || now.duration_since(since)
                >= Duration::from_millis(u64::from(
                    self.policy.timings_of(self.tier).failover_wait_ms,
                ))
    }

    fn choose(
        &mut self,
        now: Instant,
        healthy: bool,
        refused: bool,
        may_retry_primary: bool,
    ) -> Option<usize> {
        // `Automatic` hands every enabled relay to iroh, which chooses among them itself.
        if self.policy.order == RelayOrder::Automatic || self.policy.enabled_targets().count() < 2 {
            return None;
        }
        if healthy {
            self.unavailable_since = None;
            // Only `Priority` goes back to the first relay; `Sequential` keeps the one that works. A preferred-tier probe must never
            // interrupt application traffic using a fallback.
            if self.policy.order != RelayOrder::Priority {
                return None;
            }
            let primary = self.policy.enabled_targets().next()?.0;
            return (self.tier != primary
                && may_retry_primary
                && now.duration_since(self.primary_attempt)
                    >= Duration::from_millis(u64::from(
                        self.policy.timings_of(primary).retry_primary_ms,
                    )))
            .then_some(primary);
        }
        if self.failed_long_enough(now, refused) {
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
                    RelayTarget::Custom("https://second.example".into()),
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
    fn sequential_keeps_a_working_fallback_where_priority_goes_back_to_the_first() {
        let later =
            |coordinator: &Coordinator| coordinator.primary_attempt + Duration::from_secs(3600);
        let mut priority = coordinator();
        priority.tier = 1;
        assert_eq!(
            priority.choose(later(&priority), true, false, true),
            Some(0)
        );
        let mut sequential = coordinator();
        sequential.policy.order = RelayOrder::Sequential;
        sequential.tier = 1;
        assert_eq!(
            sequential.choose(later(&sequential), true, false, true),
            None
        );
        // Both still move down the list when the relay in use fails.
        assert_eq!(
            sequential.choose(sequential.primary_attempt, false, true, true),
            Some(2)
        );
    }

    #[test]
    fn automatic_leaves_the_choice_to_the_network_and_never_switches_tiers() {
        let mut automatic = coordinator();
        automatic.policy.order = RelayOrder::Automatic;
        assert_eq!(
            automatic.choose(automatic.primary_attempt, false, true, true),
            None
        );
        let map = automatic.current_map(None).unwrap();
        // Every enabled relay is offered: the two named ones and the public pool.
        assert!(map.contains(&"https://first.example".parse().unwrap()));
        assert!(map.contains(&"https://second.example".parse().unwrap()));
        assert!(map.len() > 2);
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

    /// Opt-in, needs the network: does the project relay accept a client that offers no credential? Run with `--ignored`.
    #[tokio::test]
    #[ignore = "dials the real project relay"]
    async fn the_project_relay_accepts_a_client_without_credentials() {
        use iroh::endpoint::{Builder, presets};
        let url: iroh::RelayUrl = peersus_core::relay::PROJECT_RELAY_URL.parse().unwrap();
        let endpoint = Builder::new(presets::Minimal)
            .secret_key(iroh::SecretKey::generate())
            .relay_mode(iroh::RelayMode::Custom(iroh::RelayMap::from(url.clone())))
            .bind()
            .await
            .unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let Some(status) = endpoint
                    .home_relay_status()
                    .get()
                    .iter()
                    .find(|status| status.url() == &url)
                {
                    if status.is_connected() {
                        return "connected".to_owned();
                    }
                    if let Some(reason) = status.auth_denied_reason() {
                        return format!("refused: {reason}");
                    }
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap_or_else(|_| "no answer in 20s".to_owned());
        println!("project relay without credentials: {outcome}");
        assert_eq!(outcome, "connected");
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
                    RelayTarget::Custom(fallback_url.to_string()),
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
                transport::target_map(&settings.effective_relay_policy(), 0, access.as_ref())
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
