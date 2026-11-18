//! What a device that keeps to itself was opened for, and for how long.
//!
//! In private mode nothing leaves this device for a paired one by itself. A permit is what lets something leave: it is made
//! only by what the person does for one device (a file sent to it, a device chosen on an extension's page, an action run for
//! it), never by an extension asking, and it belongs to who it was made for: the application's own sends, or one extension.
//! While at least one permit stands for a device, that device is shown how this one is; when the last one goes, it is told
//! nothing again.
//!
//! A permit stands while its holder is still at work for the person: a file of theirs still going, a stream an extension
//! still carries, or the person still on that extension's page. And for [`GRACE`] after that: someone who looks at another
//! device's numbers and then leaves is not hidden the same instant, and a second file sent a moment after the first needs
//! no new opening. What an extension sends, and what the other device asks of it, renews nothing: neither of them decides
//! how long this device is shown.
use peersus_core::{model::PeerId, plugins::PluginId};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

/// How long a permit stands after its holder was last at work for the device.
pub const GRACE: Duration = Duration::from_secs(30);

/// Who a permit is for: the application's own sends (`None`), or one extension.
pub type Holder = Option<PluginId>;

#[derive(Debug, Default)]
pub struct Permits {
    /// When each holder was last at work for each device.
    held: BTreeMap<(PeerId, Holder), Instant>,
}

impl Permits {
    /// The person started something for `peer` through `holder`: a permit is made, or the one that stands is renewed.
    pub fn grant(&mut self, peer: &PeerId, holder: Holder, now: Instant) {
        self.held.insert((peer.clone(), holder), now);
    }

    /// The person is on a page of `plugin` now: every device they opened through it stays open. Nothing is opened by it.
    pub fn renew_holder(&mut self, plugin: &PluginId, now: Instant) {
        for ((_, holder), last) in &mut self.held {
            if holder.as_ref() == Some(plugin) {
                *last = now;
            }
        }
    }

    /// Let go of every permit whose holder has not been at work for its device for [`GRACE`]; `busy` says which are at work
    /// this moment, and those are renewed. Returns the devices left with no permit at all, each once.
    pub fn expire(&mut self, now: Instant, busy: impl Fn(&PeerId, &Holder) -> bool) -> Vec<PeerId> {
        let mut lapsed = Vec::new();
        self.held.retain(|(peer, holder), last| {
            if busy(peer, holder) {
                *last = now;
            }
            let stands = now.saturating_duration_since(*last) < GRACE;
            if !stands && !lapsed.contains(peer) {
                lapsed.push(peer.clone());
            }
            stands
        });
        lapsed.retain(|peer| !self.held.keys().any(|(held, _)| held == peer));
        lapsed
    }

    /// Whether nothing is open.
    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    /// Every permit goes: private mode was turned on or off, and what stood under the other no longer means anything.
    pub fn clear(&mut self) {
        self.held.clear();
    }

    /// The permits of an extension that stopped or was removed go with it. Returns the devices left with no permit.
    pub fn forget_holder(&mut self, plugin: &PluginId) -> Vec<PeerId> {
        let mut lapsed = Vec::new();
        self.held.retain(|(peer, holder), _| {
            let goes = holder.as_ref() == Some(plugin);
            if goes && !lapsed.contains(peer) {
                lapsed.push(peer.clone());
            }
            !goes
        });
        lapsed.retain(|peer| !self.held.keys().any(|(held, _)| held == peer));
        lapsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(name: &str) -> PeerId {
        PeerId(name.to_owned())
    }
    fn plugin(name: &str) -> Holder {
        Some(PluginId::new(name).unwrap())
    }

    #[test]
    fn staying_on_an_extensions_page_keeps_only_what_was_opened_through_it() {
        let mut permits = Permits::default();
        let start = Instant::now();
        let monitor = PluginId::new("monitor").unwrap();
        let idle = |_: &PeerId, _: &Holder| false;
        // Nothing is opened by being on a page.
        permits.renew_holder(&monitor, start);
        assert!(permits.is_empty());
        permits.grant(&peer("a"), plugin("monitor"), start);
        permits.grant(&peer("a"), plugin("clipboard"), start);
        permits.grant(&peer("b"), None, start);
        permits.renew_holder(&monitor, start + GRACE);
        // The other extension's permit for A and the file's for B lapse; A stays open, through Monitor alone.
        assert_eq!(permits.expire(start + GRACE, idle), [peer("b")]);
        assert_eq!(permits.expire(start + GRACE + GRACE, idle), [peer("a")]);
    }

    #[test]
    fn a_permit_stands_for_the_grace_after_its_holder_was_last_at_work_and_then_the_device_is_closed()
     {
        let mut permits = Permits::default();
        let start = Instant::now();
        permits.grant(&peer("a"), None, start);
        let idle = |_: &PeerId, _: &Holder| false;
        assert!(
            permits
                .expire(start + GRACE - Duration::from_secs(1), idle)
                .is_empty()
        );
        // Still sending when the grace would have run out: it starts over from now.
        let later = start + GRACE + Duration::from_secs(5);
        assert!(permits.expire(later, |_, _| true).is_empty());
        assert!(
            permits
                .expire(later + GRACE - Duration::from_secs(1), idle)
                .is_empty()
        );
        assert_eq!(permits.expire(later + GRACE, idle), [peer("a")]);
        assert!(permits.is_empty());
    }

    #[test]
    fn a_device_is_closed_only_when_its_last_permit_goes() {
        let mut permits = Permits::default();
        let start = Instant::now();
        permits.grant(&peer("a"), None, start);
        permits.grant(
            &peer("a"),
            plugin("monitor"),
            start + Duration::from_secs(20),
        );
        permits.grant(&peer("b"), plugin("monitor"), start);
        let idle = |_: &PeerId, _: &Holder| false;
        // The file's permit and B's lapse; A is still open for the extension.
        assert_eq!(permits.expire(start + GRACE, idle), [peer("b")]);
        assert_eq!(
            permits.expire(start + Duration::from_secs(20) + GRACE, idle),
            [peer("a")]
        );
    }

    #[test]
    fn an_extension_that_goes_takes_its_permits_and_turning_the_mode_takes_them_all() {
        let mut permits = Permits::default();
        let start = Instant::now();
        permits.grant(&peer("a"), plugin("monitor"), start);
        permits.grant(&peer("b"), plugin("monitor"), start);
        permits.grant(&peer("b"), None, start);
        let monitor = PluginId::new("monitor").unwrap();
        assert_eq!(permits.forget_holder(&monitor), [peer("a")]);
        assert!(!permits.is_empty());
        permits.clear();
        assert!(permits.is_empty());
    }
}
