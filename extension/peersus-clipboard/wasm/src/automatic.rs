//! Sending a new copy on its own, without being asked.
//!
//! The guest has no clock, so nothing here waits: the application says "the clipboard changed" and this wakes. What
//! happens then is the process variant's rules, one for one:
//!
//! * a copy is sent only to devices the person opted in AND that hold both permissions AND are online;
//! * the first reading after any change of consent or reachability is a BASELINE, never a copy, so an old clipboard is
//!   never sent as if it were just copied;
//! * consent is read again before EVERY destination, so a slow first device cannot keep a later, revoked one alive;
//! * a copy that waited longer than its freshness window is dropped, and the application enforces that window where it
//!   holds the time (the captured text expires there), so a stale one is refused rather than sent late.
use crate::config::Config;
use crate::observation::Observation;

/// What eligibility looked like at one moment: who consent covers, and who can be sent to right now.
#[derive(Default, PartialEq, Eq, Clone, Debug)]
pub struct Eligibility {
    pub consented: Vec<String>,
    pub reachable: Vec<String>,
}

impl Eligibility {
    pub fn of(config: &Config) -> Self {
        Self {
            consented: config.consented(),
            reachable: config.reachable(),
        }
    }
}

/// What one look at the configuration changes, decided before anything is read from the clipboard.
#[derive(Debug, PartialEq, Eq)]
pub enum Epoch {
    /// Consent changed in either direction, or no baseline was ever taken: take a new baseline and send nothing.
    Rebaseline,
    /// Only reachability changed: forget what was seen and send nothing, but the baseline is still good.
    Forget,
    /// Nothing changed that affects what may be sent.
    Same,
}

/// What the loop remembers between wakes.
#[derive(Default)]
pub struct Automatic {
    pub observation: Observation,
    seen: Eligibility,
    /// True until a baseline succeeds. While it is true, observation stays completely blocked.
    pub baseline_needed: bool,
    started: bool,
}

impl Automatic {
    pub fn new() -> Self {
        Self {
            baseline_needed: true,
            ..Self::default()
        }
    }

    /// Compare the configuration with what was seen, and say what that means.
    pub fn epoch(&mut self, now: &Eligibility) -> Epoch {
        let first = !self.started;
        self.started = true;
        if self.baseline_needed || first || now.consented != self.seen.consented {
            self.seen = now.clone();
            self.observation = Observation::default();
            return Epoch::Rebaseline;
        }
        if now.reachable != self.seen.reachable {
            self.seen = now.clone();
            self.observation = Observation::default();
            return Epoch::Forget;
        }
        Epoch::Same
    }

    /// Record how a baseline attempt went. A failed one keeps observation blocked rather than restarting anything.
    pub fn baseline_done(&mut self, succeeded: bool) {
        self.baseline_needed = !succeeded;
    }

    /// The devices that may be sent to now.
    pub fn destinations(&self) -> &[String] {
        &self.seen.reachable
    }

    /// Decide what a copy means for one reading. `Some` only for a genuine new copy.
    pub fn copy(&mut self, reading: Reading) -> Option<String> {
        match reading {
            Reading::Changed(text) => self.observation.fresh(text),
            Reading::Snapshot(text) => self.observation.changed(Some(text)),
            Reading::Unavailable => self.observation.changed(None),
        }
    }

    /// A text this device itself put on the clipboard: not a copy of its own, so it is never sent back.
    pub fn received(&mut self, text: &str) {
        self.observation.received(text);
    }

    /// Before each destination: has anything changed since the copy was taken? If so, stop sending it.
    pub fn still_valid(&mut self, latest: &Eligibility) -> bool {
        match self.epoch(latest) {
            Epoch::Same => true,
            Epoch::Rebaseline | Epoch::Forget => false,
        }
    }

    /// Whether a copy is worth sending at all right now. Sending itself asks the destinations, so only the tests ask this.
    #[cfg(test)]
    #[cfg(test)]
    pub fn can_send(&self) -> bool {
        !self.baseline_needed && !self.seen.reachable.is_empty()
    }

    /// Whether the platform should be kept observing. That follows CONSENT and not reachability: a guest that stopped
    /// observing while every consenting device was offline would depend on a notice to start again. Observing sends
    /// nothing: what is sent is decided by the reachable destinations, and the first reading after a device becomes
    /// reachable is a baseline, never a copy.
    pub fn can_observe(&self) -> bool {
        !self.baseline_needed && !self.seen.consented.is_empty()
    }
}

/// How the clipboard answered a look.
#[derive(Debug, PartialEq, Eq)]
pub enum Reading {
    /// A platform event proved this was copied since the baseline.
    Changed(String),
    /// A snapshot: this is on the clipboard, copied at some unknown time.
    Snapshot(String),
    /// Nothing readable: sensitive, empty, refused, or not allowed to be read now.
    Unavailable,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config(automatic: bool, devices: &[(&str, bool, bool)]) -> Config {
        let permissions = json!([{"owner":"host","name":"peers.send"},{"owner":"clipboard","name":"clipboard.use"}]);
        let peers: Vec<_> = devices
            .iter()
            .map(|(name, online, sync)| json!({"device":name,"online":online,"settings":{"sync":sync},"permissions":permissions}))
            .collect();
        Config::parse(
            json!({"plugin":"clipboard","settings":{"automatic":automatic},"peers":peers}),
        )
        .unwrap()
    }
    fn eligibility(config: &Config) -> Eligibility {
        Eligibility::of(config)
    }

    #[test]
    fn the_first_look_is_always_a_baseline_and_sends_nothing() {
        let mut state = Automatic::new();
        let now = eligibility(&config(true, &[("phone", true, true)]));
        assert_eq!(state.epoch(&now), Epoch::Rebaseline);
        assert!(!state.can_send(), "blocked until the baseline succeeds");
        state.baseline_done(true);
        assert!(state.can_send());
        assert_eq!(state.copy(Reading::Snapshot("already there".into())), None);
    }

    #[test]
    fn a_failed_baseline_keeps_everything_blocked_and_is_retried() {
        let mut state = Automatic::new();
        let now = eligibility(&config(true, &[("phone", true, true)]));
        state.epoch(&now);
        state.baseline_done(false);
        assert!(!state.can_send());
        assert_eq!(
            state.epoch(&now),
            Epoch::Rebaseline,
            "it is retried on the next look"
        );
        state.baseline_done(true);
        assert_eq!(state.epoch(&now), Epoch::Same);
        assert!(state.can_send());
    }

    #[test]
    fn a_new_copy_after_the_baseline_is_sent_and_a_repeat_is_not() {
        let mut state = Automatic::new();
        let now = eligibility(&config(true, &[("phone", true, true)]));
        state.epoch(&now);
        state.baseline_done(true);
        assert_eq!(state.copy(Reading::Snapshot("old".into())), None);
        assert_eq!(
            state.copy(Reading::Changed("new".into())),
            Some("new".into())
        );
        assert_eq!(
            state.copy(Reading::Changed("new".into())),
            None,
            "the same copy is not sent twice"
        );
    }

    #[test]
    fn a_change_of_consent_in_either_direction_starts_over_with_a_baseline() {
        let mut state = Automatic::new();
        let both = eligibility(&config(true, &[("a", true, true), ("b", true, true)]));
        state.epoch(&both);
        state.baseline_done(true);
        // A device opted out: fewer consented.
        let fewer = eligibility(&config(true, &[("a", true, true), ("b", true, false)]));
        assert_eq!(state.epoch(&fewer), Epoch::Rebaseline);
        state.baseline_done(true);
        // A device opted in again: more consented. What is on the clipboard now must not be sent to it.
        assert_eq!(state.epoch(&both), Epoch::Rebaseline);
        state.baseline_done(true);
        assert_eq!(
            state.copy(Reading::Snapshot("copied while it was opted out".into())),
            None
        );
    }

    #[test]
    fn a_device_going_offline_or_online_forgets_what_was_seen_but_keeps_the_baseline() {
        let mut state = Automatic::new();
        let online = eligibility(&config(true, &[("phone", true, true)]));
        state.epoch(&online);
        state.baseline_done(true);
        state.copy(Reading::Snapshot("seen".into()));
        let offline = eligibility(&config(true, &[("phone", false, true)]));
        assert_eq!(state.epoch(&offline), Epoch::Forget);
        assert!(!state.baseline_needed, "reachability is not consent");
        assert!(!state.can_send(), "there is nobody to send to");
        assert_eq!(state.epoch(&online), Epoch::Forget);
        // Back online: the first thing seen is the new baseline and is not sent. A different text after it IS a copy, so a
        // real copy made right after reconnecting is not lost.
        assert_eq!(state.copy(Reading::Snapshot("seen".into())), None);
        assert_eq!(
            state.copy(Reading::Snapshot("copied after reconnecting".into())),
            Some("copied after reconnecting".into())
        );
    }

    #[test]
    fn turning_automatic_sending_off_stops_everything_and_turning_it_on_does_not_send_the_past() {
        let mut state = Automatic::new();
        let on = eligibility(&config(true, &[("phone", true, true)]));
        state.epoch(&on);
        state.baseline_done(true);
        let off = eligibility(&config(false, &[("phone", true, true)]));
        assert_eq!(state.epoch(&off), Epoch::Rebaseline);
        state.baseline_done(true);
        assert!(!state.can_send(), "off means nobody is eligible");
        assert_eq!(state.epoch(&on), Epoch::Rebaseline);
        state.baseline_done(true);
        assert_eq!(
            state.copy(Reading::Snapshot("copied while it was off".into())),
            None
        );
    }

    #[test]
    fn consent_is_checked_again_before_each_destination_and_a_change_stops_the_rest() {
        let mut state = Automatic::new();
        let both = eligibility(&config(true, &[("a", true, true), ("b", true, true)]));
        state.epoch(&both);
        state.baseline_done(true);
        assert!(state.still_valid(&both), "nothing changed: carry on");
        let revoked = eligibility(&config(true, &[("a", true, true), ("b", true, false)]));
        assert!(
            !state.still_valid(&revoked),
            "b was revoked while a was being sent to: stop"
        );
        assert_eq!(
            state.copy(Reading::Snapshot("old snapshot".into())),
            None,
            "a consent change invalidates the old observation before the next destination"
        );
        assert_eq!(
            state.destinations(),
            ["a"],
            "what is sent next is judged against the new state"
        );
    }

    #[test]
    fn a_reading_that_is_unavailable_breaks_the_chain_so_a_later_copy_is_not_mistaken_for_new() {
        let mut state = Automatic::new();
        let now = eligibility(&config(true, &[("phone", true, true)]));
        state.epoch(&now);
        state.baseline_done(true);
        state.copy(Reading::Snapshot("before".into()));
        assert_eq!(state.copy(Reading::Unavailable), None);
        assert_eq!(
            state.copy(Reading::Snapshot("after the gap".into())),
            None,
            "a gap makes the next one a baseline"
        );
        assert_eq!(
            state.copy(Reading::Snapshot("a real copy".into())),
            Some("a real copy".into())
        );
    }

    #[test]
    fn a_platform_event_proves_freshness_even_after_unavailable_contents() {
        let mut state = Automatic::new();
        let now = eligibility(&config(true, &[("phone", true, true)]));
        state.epoch(&now);
        state.baseline_done(true);
        state.copy(Reading::Unavailable);
        assert_eq!(
            state.copy(Reading::Changed("copied right after a password".into())),
            Some("copied right after a password".into())
        );
    }

    #[test]
    fn with_nobody_eligible_nothing_can_be_sent_whatever_the_clipboard_holds() {
        let mut state = Automatic::new();
        let nobody = eligibility(&config(true, &[("phone", true, false)]));
        state.epoch(&nobody);
        state.baseline_done(true);
        assert!(!state.can_send());
        assert!(state.destinations().is_empty());
    }
}
