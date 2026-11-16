//! One authenticated plugin session for desktop and isolated Android services.
//! Consent changes invalidate old observations; connectivity does not revoke a pending authorized copy.
use crate::{
    plugin::{self, Clipboard, Control},
    wire,
};
use peersus_api::message::{Delivery, Operation, Outcome};
use peersus_plugin::{
    frame::{self, CONTROL_LIMIT},
    link::Hello,
    supervisor::Bootstrap,
};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

const MAX_WAITING: usize = 32;
const FRESH_FOR: Duration = Duration::from_secs(3);
// Covers capture, the five-second claim, the bounded write and the separate
// fifteen-second delivery acknowledgement. Freshness still ends before writing.
const OPERATION_LIMIT: Duration = Duration::from_secs(40);

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
struct Device {
    device: String,
    online: bool,
    settings: DeviceSettings,
    #[serde(default)]
    permissions: Vec<peersus_api::permission::Permission>,
}
impl Device {
    fn allowed(&self, caller: &str) -> bool {
        self.permissions
            .iter()
            .any(|permission| permission.owner == caller && permission.name == "clipboard.use")
            && self
                .permissions
                .iter()
                .any(|permission| permission.owner == "host" && permission.name == "peers.send")
    }
}
#[derive(Default, Deserialize)]
struct Config {
    #[serde(default)]
    plugin: String,
    settings: Settings,
    peers: Vec<Device>,
}

/// State contains at most one bounded text; unavailable reads break continuity rather than
/// causing a later permission grant/focus gain to send the clipboard's old contents.
#[derive(Default)]
pub struct Observation {
    last: Option<String>,
    initialized: bool,
}
impl Observation {
    pub fn changed(&mut self, text: Option<String>) -> Option<String> {
        let text = text.filter(|text| !text.is_empty() && text.len() <= wire::MAX_TEXT as usize);
        let changed = self.initialized && self.last != text;
        self.initialized = text.is_some();
        self.last.clone_from(&text);
        if changed { text } else { None }
    }
    pub fn received(&mut self, text: &str) {
        self.last = Some(text.to_owned());
        self.initialized = true;
    }
    /// A platform event proves freshness even after unavailable/sensitive contents.
    pub fn fresh(&mut self, text: String) -> Option<String> {
        if text.is_empty() || text.len() > wire::MAX_TEXT as usize {
            return self.changed(None);
        }
        let duplicate = self.last.as_ref() == Some(&text);
        self.initialized = true;
        self.last = Some(text.clone());
        (!duplicate).then_some(text)
    }
}

struct Observed<C> {
    inner: C,
    observation: Observation,
}
impl<C: Clipboard> Clipboard for Observed<C> {
    async fn text(&mut self) -> Option<String> {
        self.inner.text().await
    }
    async fn selected_text(&mut self, context: u64) -> Option<String> {
        self.inner.selected_text(context).await
    }
    async fn selected_valid(&mut self, context: u64) -> bool {
        self.inner.selected_valid(context).await
    }
    async fn set_text_until(&mut self, text: &str, deadline: Instant) -> bool {
        if self.inner.set_text_until(text, deadline).await {
            self.observation.received(text);
            true
        } else {
            false
        }
    }
    async fn set_text(&mut self, text: &str) -> bool {
        if self.inner.set_text(text).await {
            self.observation.received(text);
            true
        } else {
            false
        }
    }
}
struct Snapshot(String);
impl Clipboard for Snapshot {
    async fn text(&mut self) -> Option<String> {
        Some(self.0.clone())
    }
    async fn set_text(&mut self, _: &str) -> bool {
        false
    }
}

fn eligible(config: &Config, online_only: bool) -> Vec<String> {
    let mut destinations: Vec<_> = config
        .peers
        .iter()
        .filter(|peer| {
            config.settings.automatic
                && (!online_only || peer.online)
                && peer.settings.sync
                && peer.allowed(&config.plugin)
        })
        .map(|peer| peer.device.clone())
        .collect();
    destinations.sort();
    destinations.dedup();
    destinations
}

async fn rebaseline<C: Clipboard>(clipboard: &mut Observed<C>) -> bool {
    clipboard.observation = Observation::default();
    matches!(
        tokio::time::timeout(Duration::from_secs(3), clipboard.inner.reset_observation()).await,
        Ok(true)
    )
}

async fn configuration(
    stream: &mut peersus_plugin::transport::Stream,
    waiting: &mut VecDeque<Delivery>,
) -> Option<Config> {
    let request = Operation::Call {
        id: 2,
        provider: "host".into(),
        service: "peersus.config".into(),
        version: 1,
        method: "get".into(),
        args: json!({}),
        timeout_ms: 2_000,
    };
    frame::write(stream, &request, CONTROL_LIMIT).await.ok()?;
    loop {
        match frame::read::<Delivery>(stream, CONTROL_LIMIT).await.ok()? {
            Delivery::Result {
                id: 2,
                result: Outcome::Ok { value },
            } => {
                let config: Config = serde_json::from_value(value).ok()?;
                if config.peers.len() > 256
                    || config.peers.iter().any(|peer| {
                        !peersus_api::name::valid(&peer.device)
                            || peer.permissions.len() > 64
                            || peer
                                .permissions
                                .iter()
                                .any(|permission| !permission.is_valid())
                    })
                {
                    return None;
                }
                return Some(config);
            }
            // An older host or a restricted manifest may not offer configuration.
            // Automatic synchronization stays disabled; explicit share remains usable.
            Delivery::Result {
                id: 2,
                result: Outcome::Error { .. },
            } => return Some(Config::default()),
            delivery if waiting.len() < MAX_WAITING => waiting.push_back(delivery),
            _ => return None,
        }
    }
}

/// Run only inside the plugin process. Bootstrap comes from the supervisor, never disk or
/// logs. Cancellation drops sockets and the platform-owned clipboard handle.
pub async fn run<C: Clipboard>(clipboard: C, bootstrap: Bootstrap) {
    let Ok(mut stream) = peersus_plugin::transport::connect(&bootstrap.socket).await else {
        return;
    };
    if frame::write(
        &mut stream,
        &Hello {
            token: bootstrap.token.clone(),
        },
        CONTROL_LIMIT,
    )
    .await
    .is_err()
    {
        return;
    }
    let mut clipboard = Observed {
        inner: clipboard,
        observation: Observation::default(),
    };
    let mut waiting = VecDeque::new();
    let mut timer = tokio::time::interval(Duration::from_millis(500));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut destinations = Vec::<String>::new();
    let mut consented = Vec::<String>::new();
    let mut baseline_needed = true;
    loop {
        if !plugin::drain(&mut stream, &mut waiting).await {
            return;
        }
        if waiting.len() > MAX_WAITING {
            return;
        }
        plugin::withdraw_cancelled(&mut waiting);
        if let Some(delivery) = waiting.pop_front() {
            let handled = tokio::time::timeout(
                OPERATION_LIMIT,
                plugin::handle(
                    &mut clipboard,
                    &mut stream,
                    &bootstrap.token,
                    &mut waiting,
                    delivery,
                    || peersus_plugin::transport::connect(&bootstrap.data_socket),
                ),
            )
            .await;
            if !matches!(handled, Ok(true)) {
                return;
            }
            continue;
        }
        // Read readiness is cancellation-safe. Never race a partial frame read with a timer.
        tokio::select! {
            ready = stream.readable() => {
                if ready.is_err() { return; }
            }
            _ = timer.tick() => {
                let config = tokio::time::timeout(Duration::from_secs(3), configuration(&mut stream, &mut waiting)).await;
                let Ok(Some(config)) = config else { return };
                let next_consent = eligible(&config, false);
                let next = eligible(&config, true);
                if baseline_needed || next_consent != consented {
                    consented = next_consent;
                    destinations = next;
                    // Transient platform jank must not restart the plugin. Until a
                    // baseline succeeds, keep automatic observation completely blocked.
                    baseline_needed = !rebaseline(&mut clipboard).await;
                    // Listener reset and eligibility acquisition are distinct from a copy.
                    continue;
                }
                if next != destinations {
                    destinations = next;
                    clipboard.observation = Observation::default();
                    continue;
                }
                if destinations.is_empty() { continue; }
                let observed_at = Instant::now();
                let reading = tokio::time::timeout(Duration::from_secs(1), clipboard.inner.observe()).await;
                let changed = match reading {
                    Ok(plugin::Reading::Changed(text)) => clipboard.observation.fresh(text),
                    Ok(plugin::Reading::Snapshot(text)) => clipboard.observation.changed(Some(text)),
                    Ok(plugin::Reading::Unavailable) | Err(_) => clipboard.observation.changed(None),
                };
                let Some(text) = changed else { continue };
                let mut snapshot = Snapshot(text);
                for device in destinations.clone() {
                    // Refresh on the same serial control stream before *every* destination.
                    // A slow first peer must not preserve a revoked later opt-in.
                    let latest = tokio::time::timeout(Duration::from_secs(3), configuration(&mut stream, &mut waiting)).await;
                    let Ok(Some(latest)) = latest else { return };
                    let next_consent = eligible(&latest, false);
                    let next = eligible(&latest, true);
                    if next_consent != consented {
                        consented = next_consent;
                        destinations = next;
                        baseline_needed = !rebaseline(&mut clipboard).await;
                        break;
                    }
                    if next != destinations {
                        destinations = next;
                        clipboard.observation = Observation::default();
                        break;
                    }
                    if observed_at.elapsed() > FRESH_FOR { break; }
                    let result = tokio::time::timeout(OPERATION_LIMIT, plugin::share_until(
                        &mut snapshot,
                        Control { stream: &mut stream, token: &bootstrap.token, waiting: &mut waiting },
                        &device,
                        || peersus_plugin::transport::connect(&bootstrap.data_socket),
                        Some(observed_at + FRESH_FOR),
                    )).await;
                    // A timed-out frame exchange cannot safely be reused.
                    if result.is_err() { return; }
                    if waiting.len() > MAX_WAITING { return; }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn initial_clipboard_and_reconnect_are_not_sent() {
        let mut state = Observation::default();
        assert_eq!(state.changed(Some("old".into())), None);
        assert_eq!(state.changed(None), None);
        assert_eq!(state.changed(Some("copied while unavailable".into())), None);
        assert_eq!(state.changed(Some("fresh".into())), Some("fresh".into()));
    }
    #[test]
    fn remote_write_and_duplicate_poll_do_not_echo() {
        let mut state = Observation::default();
        state.changed(Some("local".into()));
        state.received("remote");
        assert_eq!(state.changed(Some("remote".into())), None);
        assert_eq!(state.changed(Some("remote".into())), None);
        assert_eq!(
            state.changed(Some("new local".into())),
            Some("new local".into())
        );
    }
    #[test]
    fn empty_oversized_and_sensitive_unavailable_reads_break_continuity() {
        let mut state = Observation::default();
        state.changed(Some("before".into()));
        assert_eq!(
            state.changed(Some("x".repeat(wire::MAX_TEXT as usize + 1))),
            None
        );
        assert_eq!(state.changed(Some("after sensitive".into())), None);
        assert_eq!(state.changed(Some(String::new())), None);
    }
    #[test]
    fn automatic_and_each_destination_require_independent_opt_in() {
        let config: Config = serde_json::from_value(
            json!({"settings":{},"peers":[{"device":"one","online":true,"settings":{}}]}),
        )
        .unwrap();
        assert!(!config.settings.automatic);
        assert!(!config.peers[0].settings.sync);
        assert!(
            serde_json::from_value::<Config>(json!({"settings":{"automatic":"yes"},"peers":[]}))
                .is_err()
        );
    }
    #[test]
    fn a_proven_new_copy_after_sensitive_contents_is_not_mistaken_for_a_stale_snapshot() {
        let mut state = Observation::default();
        state.received("remote sensitive");
        assert_eq!(state.changed(None), None);
        assert_eq!(
            state.fresh("genuinely copied now".into()),
            Some("genuinely copied now".into())
        );
        assert_eq!(state.fresh("genuinely copied now".into()), None);
    }

    #[test]
    fn received_text_does_not_echo_after_destination_changes_rebaseline() {
        let mut state = Observation::default();
        state.received("remote");
        state = Observation::default();
        assert_eq!(state.changed(Some("remote".into())), None);
        assert_eq!(state.changed(Some("remote".into())), None);
    }

    #[test]
    fn future_declared_settings_do_not_disable_existing_automatic_behavior() {
        let config: Config = serde_json::from_value(json!({
            "settings":{"automatic":true,"future_setting":"ignored"},
            "peers":[{"device":"one","online":true,"settings":{"sync":true,"future":12}}]
        }))
        .unwrap();
        assert!(config.settings.automatic && config.peers[0].settings.sync);
    }
}

#[cfg(test)]
mod grant_tests {
    use super::*;
    #[test]
    fn automatic_requires_both_grants_of_exact_owners_and_rejects_stale_revocation() {
        let mut device: Device = serde_json::from_value(json!({
            "device":"alias","online":true,"settings":{"sync":true},
            "permissions":[{"owner":"host","name":"peers.send"},{"owner":"other","name":"clipboard.use"}]
        })).unwrap();
        assert!(!device.allowed("clipboard"));
        device
            .permissions
            .push(peersus_api::permission::Permission::new("clipboard", "clipboard.use").unwrap());
        assert!(device.allowed("clipboard"));
        device
            .permissions
            .retain(|permission| permission.owner != "host");
        assert!(!device.allowed("clipboard"));
    }
}

#[cfg(test)]
mod epoch_regressions {
    use super::*;
    use peersus_plugin::transport::Listener;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::io::AsyncReadExt;

    struct Events {
        pending: Option<String>,
        reads: usize,
        resets: Arc<AtomicUsize>,
    }
    impl Clipboard for Events {
        async fn text(&mut self) -> Option<String> {
            panic!("automatic path must use observation")
        }
        async fn set_text(&mut self, _: &str) -> bool {
            false
        }
        async fn reset_observation(&mut self) -> bool {
            self.pending = None;
            self.reads = 0;
            self.resets.fetch_add(1, Ordering::SeqCst);
            true
        }
        async fn observe(&mut self) -> plugin::Reading {
            self.reads += 1;
            if let Some(text) = self.pending.take() {
                return plugin::Reading::Changed(text);
            }
            if self.reads == 2 {
                plugin::Reading::Changed("fresh copy".into())
            } else {
                plugin::Reading::Snapshot("baseline only".into())
            }
        }
    }

    #[tokio::test]
    async fn eligibility_epoch_reset_discards_an_already_queued_platform_changed_event() {
        let resets = Arc::new(AtomicUsize::new(0));
        let mut clipboard = Observed {
            inner: Events {
                pending: Some("copied while disabled".into()),
                reads: 0,
                resets: resets.clone(),
            },
            observation: Observation::default(),
        };
        assert!(rebaseline(&mut clipboard).await);
        let plugin::Reading::Snapshot(text) = clipboard.inner.observe().await else {
            panic!("old event replayed")
        };
        assert_eq!(clipboard.observation.changed(Some(text)), None);
        assert_eq!(resets.load(Ordering::SeqCst), 1);
    }

    fn config(second_enabled: bool) -> serde_json::Value {
        json!({"plugin":"clipboard", "settings":{"automatic":true}, "peers":[
            {"device":"a", "online":true, "settings":{"sync":true}, "permissions":[
                {"owner":"clipboard","name":"clipboard.use"}, {"owner":"host","name":"peers.send"}]},
            {"device":"b", "online":true, "settings":{"sync":second_enabled}, "permissions":[
                {"owner":"clipboard","name":"clipboard.use"}, {"owner":"host","name":"peers.send"}]}
        ]})
    }

    async fn reply_config(
        stream: &mut peersus_plugin::transport::Stream,
        value: serde_json::Value,
    ) {
        let request = frame::read::<Operation>(stream, CONTROL_LIMIT)
            .await
            .unwrap();
        assert!(
            matches!(request, Operation::Call { id: 2, .. }),
            "expected eligibility refresh, got {request:?}"
        );
        frame::write(
            stream,
            &Delivery::Result {
                id: 2,
                result: Outcome::ok(value),
            },
            CONTROL_LIMIT,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn slow_first_destination_cannot_fan_out_to_second_after_its_sync_opt_in_is_revoked() {
        let directory = tempfile::tempdir().unwrap();
        let listener = Listener::bind(&directory.path().join("control")).unwrap();
        let data_listener = Listener::bind(&directory.path().join("data")).unwrap();
        let socket = listener.endpoint().to_owned();
        let data_socket = data_listener.endpoint().to_owned();
        let resets = Arc::new(AtomicUsize::new(0));
        let clipboard = Events {
            pending: None,
            reads: 0,
            resets: resets.clone(),
        };
        let session = run(
            clipboard,
            Bootstrap {
                socket,
                data_socket,
                token: "test".into(),
            },
        );
        let host = async {
            let mut stream = listener.accept().await.unwrap();
            let _: Hello = frame::read(&mut stream, CONTROL_LIMIT).await.unwrap();
            // Acquire destinations, establish a baseline, then observe a new copy.
            for _ in 0..4 {
                reply_config(&mut stream, config(true)).await;
            }
            let request = frame::read::<Operation>(&mut stream, CONTROL_LIMIT)
                .await
                .unwrap();
            assert!(
                matches!(&request, Operation::Call { id:1, args, .. } if args["device"] == "a")
            );
            // The setting changes while a's claim is delayed. b must be revalidated,
            // not sent from the eligibility snapshot fetched before a started.
            tokio::time::sleep(Duration::from_millis(50)).await;
            frame::write(
                &mut stream,
                &Delivery::Result {
                    id: 1,
                    result: Outcome::ok(json!({"claim":9})),
                },
                CONTROL_LIMIT,
            )
            .await
            .unwrap();
            let mut data = data_listener.accept().await.unwrap();
            let _: peersus_plugin::data::DataHello = frame::read(&mut data, 1024).await.unwrap();
            let mut body = Vec::new();
            data.read_to_end(&mut body).await.unwrap();
            frame::write(&mut data, &(body.len() as u64), 1024)
                .await
                .unwrap();
            assert_eq!(
                wire::decode(std::io::Cursor::new(body)).unwrap(),
                wire::Item::Text("fresh copy".into())
            );
            reply_config(&mut stream, config(false)).await;
            // The next frame must be a timer configuration poll, never a send to b.
            let next = frame::read::<Operation>(&mut stream, CONTROL_LIMIT)
                .await
                .unwrap();
            assert!(
                matches!(next, Operation::Call { id: 2, .. }),
                "revoked destination was sent to: {next:?}"
            );
            assert_eq!(resets.load(Ordering::SeqCst), 2);
            // Closing the control stream ends the supervised session, including its timer.
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(session, host);
        })
        .await
        .unwrap();
    }

    struct PendingCopy(Arc<std::sync::Mutex<Option<String>>>);
    impl Clipboard for PendingCopy {
        async fn text(&mut self) -> Option<String> {
            panic!("automatic path must use observation")
        }
        async fn set_text(&mut self, _: &str) -> bool {
            false
        }
        async fn reset_observation(&mut self) -> bool {
            self.0.lock().unwrap().take();
            true
        }
        async fn observe(&mut self) -> plugin::Reading {
            match self.0.lock().unwrap().take() {
                Some(text) => plugin::Reading::Changed(text),
                None => plugin::Reading::Unavailable,
            }
        }
    }

    #[tokio::test]
    async fn connectivity_change_does_not_revoke_a_copy_under_unchanged_consent() {
        let directory = tempfile::tempdir().unwrap();
        let listener = Listener::bind(&directory.path().join("control")).unwrap();
        let socket = listener.endpoint().to_owned();
        let pending = Arc::new(std::sync::Mutex::new(None));
        let session = run(
            PendingCopy(pending.clone()),
            Bootstrap {
                socket,
                data_socket: directory.path().join("unused-data"),
                token: "test".into(),
            },
        );
        let host = async {
            let mut stream = listener.accept().await.unwrap();
            let _: Hello = frame::read(&mut stream, CONTROL_LIMIT).await.unwrap();
            reply_config(&mut stream, config(false)).await;
            let mut offline = config(false);
            offline["peers"][0]["online"] = json!(false);
            reply_config(&mut stream, offline).await;
            let request = frame::read::<Operation>(&mut stream, CONTROL_LIMIT)
                .await
                .unwrap();
            assert!(matches!(request, Operation::Call { id: 2, .. }));
            *pending.lock().unwrap() = Some("authorized copy made before reconnect".into());
            frame::write(
                &mut stream,
                &Delivery::Result {
                    id: 2,
                    result: Outcome::ok(config(false)),
                },
                CONTROL_LIMIT,
            )
            .await
            .unwrap();
            let mut sending = false;
            for _ in 0..4 {
                let request = frame::read::<Operation>(&mut stream, CONTROL_LIMIT)
                    .await
                    .unwrap();
                if matches!(&request, Operation::Call { id:1, args, .. } if args["device"] == "a") {
                    sending = true;
                    break;
                }
                assert!(matches!(request, Operation::Call { id: 2, .. }));
                frame::write(
                    &mut stream,
                    &Delivery::Result {
                        id: 2,
                        result: Outcome::ok(config(false)),
                    },
                    CONTROL_LIMIT,
                )
                .await
                .unwrap();
            }
            assert!(
                sending,
                "reconnection discarded a new copy even though authorization never changed"
            );
        };
        tokio::time::timeout(Duration::from_secs(6), async {
            tokio::join!(session, host);
        })
        .await
        .unwrap();
    }

    struct RetryReset {
        resets: Arc<AtomicUsize>,
        observations: Arc<AtomicUsize>,
    }
    impl Clipboard for RetryReset {
        async fn text(&mut self) -> Option<String> {
            panic!("no ordinary read during baseline retry")
        }
        async fn set_text(&mut self, _: &str) -> bool {
            false
        }
        async fn reset_observation(&mut self) -> bool {
            self.resets.fetch_add(1, Ordering::SeqCst) > 0
        }
        async fn observe(&mut self) -> plugin::Reading {
            self.observations.fetch_add(1, Ordering::SeqCst);
            plugin::Reading::Changed("must not observe before baseline".into())
        }
    }

    #[tokio::test]
    async fn transient_baseline_failure_retries_without_exiting_or_observing() {
        let directory = tempfile::tempdir().unwrap();
        let listener = Listener::bind(&directory.path().join("control")).unwrap();
        let socket = listener.endpoint().to_owned();
        let resets = Arc::new(AtomicUsize::new(0));
        let observations = Arc::new(AtomicUsize::new(0));
        let clipboard = RetryReset {
            resets: resets.clone(),
            observations: observations.clone(),
        };
        let session = run(
            clipboard,
            Bootstrap {
                socket,
                data_socket: directory.path().join("unused-data"),
                token: "test".into(),
            },
        );
        let host = async {
            let mut stream = listener.accept().await.unwrap();
            let _: Hello = frame::read(&mut stream, CONTROL_LIMIT).await.unwrap();
            reply_config(&mut stream, config(true)).await;
            // A second config request proves failure did not terminate the session.
            let second = frame::read::<Operation>(&mut stream, CONTROL_LIMIT)
                .await
                .unwrap();
            assert!(matches!(second, Operation::Call { id: 2, .. }));
            assert_eq!(resets.load(Ordering::SeqCst), 1);
            assert_eq!(observations.load(Ordering::SeqCst), 0);
            frame::write(
                &mut stream,
                &Delivery::Result {
                    id: 2,
                    result: Outcome::ok(config(true)),
                },
                CONTROL_LIMIT,
            )
            .await
            .unwrap();
            let third = frame::read::<Operation>(&mut stream, CONTROL_LIMIT)
                .await
                .unwrap();
            assert!(matches!(third, Operation::Call { id: 2, .. }));
            assert_eq!(resets.load(Ordering::SeqCst), 2);
            assert_eq!(observations.load(Ordering::SeqCst), 0);
        };
        tokio::time::timeout(Duration::from_secs(4), async {
            tokio::join!(session, host);
        })
        .await
        .unwrap();
    }
}
