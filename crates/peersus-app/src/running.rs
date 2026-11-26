//! Transfers to plugins that are already carrying bytes.
//!
//! A transfer that has started is out of reach of everything that withdraws permission
//! from streams that are merely waiting: it is a task copying a peer's bytes into a plugin,
//! and it keeps doing that until the peer stops, however the user has meanwhile decided. So
//! each running transfer is recorded with who it belongs to and a way to stop it, and
//! withdrawing a permission or forgetting a device stops the matching ones.
//!
//! The registry knows nothing about tasks: what it holds is a handle it can call to stop
//! one, which keeps the rules (who is stopped, and who is left alone) testable on their own.
use crate::parked::Owner;
use peersus_core::{model::PeerId, plugins::PluginId};
use std::collections::HashMap;

/// A way to stop one running transfer. Calling it more than once is harmless.
pub trait Stop {
    fn stop(&self);
}

impl Stop for tokio::task::AbortHandle {
    fn stop(&self) {
        self.abort();
    }
}

/// Running transfers by the number of the call that announced them.
pub struct Running<S: Stop> {
    transfers: HashMap<u64, (Owner, S)>,
}

impl<S: Stop> Default for Running<S> {
    fn default() -> Self {
        Self {
            transfers: HashMap::new(),
        }
    }
}

impl<S: Stop> Running<S> {
    /// Record a transfer that has started.
    pub fn started(&mut self, call: u64, owner: Owner, handle: S) {
        self.transfers.insert(call, (owner, handle));
    }

    /// A transfer ended by itself, whatever way. Returns whether it was known.
    pub fn ended(&mut self, call: u64) -> bool {
        self.transfers.remove(&call).is_some()
    }

    /// The call that announced a transfer has ended (its deadline passed, or the plugin
    /// answered or went away): stop the transfer it announced, if one is running.
    ///
    /// Without this a peer that keeps sending a little at a time would keep the plugin inside
    /// one call for as long as it liked, because the copy itself only gives up after a
    /// stretch of silence, and with it everything queued behind that call.
    ///
    /// `call` is a CALL number, the one this registry is keyed by. A claim number belongs to
    /// a different registry and a different space of numbers, and must never be passed here,
    /// or an unrelated transfer could be stopped by a number that only happens to match.
    pub fn stop_call(&mut self, call: u64) -> bool {
        match self.transfers.remove(&call) {
            Some((_, handle)) => {
                handle.stop();
                true
            }
            None => false,
        }
    }

    /// Signal a live transfer to finish its authenticated cancellation on the wire.
    pub fn signal(
        &mut self,
        plugin: &PluginId,
        peer: Option<&PeerId>,
        mut send: impl FnMut(u64, &mut S) -> bool,
    ) -> Vec<(u64, Owner)> {
        self.transfers
            .iter_mut()
            .filter_map(|(&number, (owner, handle))| {
                if &owner.plugin != plugin || peer.is_some_and(|peer| &owner.peer != peer) {
                    return None;
                }
                if !send(number, handle) {
                    return None;
                }
                Some((number, owner.clone()))
            })
            .collect()
    }

    /// Stop what is running for `peer`, for one plugin or (with `None`) for every plugin,
    /// and return how many were stopped. A transfer of another device, or of the same device
    /// to another plugin, is never touched.
    pub fn stop_for(&mut self, peer: &PeerId, plugin: Option<&PluginId>) -> usize {
        self.stop_where(|owner| {
            &owner.peer == peer && plugin.is_none_or(|plugin| plugin == &owner.plugin)
        })
    }

    /// Whether something is running for `peer` and `plugin` now.
    pub fn any_for(&self, peer: &PeerId, plugin: &PluginId) -> bool {
        self.transfers
            .values()
            .any(|(owner, _)| &owner.peer == peer && &owner.plugin == plugin)
    }

    /// Stop everything running for `plugin`, from every device, as when it is restarted: the
    /// new process cannot continue what the old one was receiving.
    pub fn stop_for_plugin(&mut self, plugin: &PluginId) -> usize {
        self.stop_where(|owner| &owner.plugin == plugin)
    }

    fn stop_where(&mut self, matches: impl Fn(&Owner) -> bool) -> usize {
        let mut stopped = 0;
        self.transfers.retain(|_, (owner, handle)| {
            let hit = matches(owner);
            if hit {
                handle.stop();
                stopped += 1;
            }
            !hit
        });
        stopped
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.transfers.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    /// A handle that counts how many times it was told to stop.
    #[derive(Clone, Default)]
    struct Counting(Arc<AtomicUsize>);
    impl Stop for Counting {
        fn stop(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    impl Counting {
        fn stopped(&self) -> usize {
            self.0.load(Ordering::SeqCst)
        }
    }

    fn owner(device: &str, plugin: &str) -> Owner {
        Owner {
            peer: PeerId(device.into()),
            plugin: PluginId::new(plugin).unwrap(),
        }
    }
    fn peer(name: &str) -> PeerId {
        PeerId(name.into())
    }

    #[test]
    fn forgetting_a_device_stops_every_transfer_it_has_running_and_only_those() {
        let (a, b, c) = (
            Counting::default(),
            Counting::default(),
            Counting::default(),
        );
        let mut running = Running::default();
        running.started(1, owner("gone", "x"), a.clone());
        running.started(2, owner("gone", "y"), b.clone());
        running.started(3, owner("stays", "x"), c.clone());
        assert_eq!(running.stop_for(&peer("gone"), None), 2);
        assert_eq!((a.stopped(), b.stopped(), c.stopped()), (1, 1, 0));
        assert_eq!(running.len(), 1);
    }

    #[test]
    fn withdrawing_one_plugin_for_a_device_leaves_its_other_plugins_and_other_devices_running() {
        let (to_x, to_y, other_device) = (
            Counting::default(),
            Counting::default(),
            Counting::default(),
        );
        let mut running = Running::default();
        running.started(1, owner("p", "x"), to_x.clone());
        running.started(2, owner("p", "y"), to_y.clone());
        running.started(3, owner("q", "x"), other_device.clone());
        let plugin_x = PluginId::new("x").unwrap();
        assert_eq!(running.stop_for(&peer("p"), Some(&plugin_x)), 1);
        assert_eq!(
            (to_x.stopped(), to_y.stopped(), other_device.stopped()),
            (1, 0, 0)
        );
    }

    #[test]
    fn restarting_a_plugin_stops_what_it_was_receiving_from_every_device() {
        let (from_p, from_q, elsewhere) = (
            Counting::default(),
            Counting::default(),
            Counting::default(),
        );
        let mut running = Running::default();
        running.started(1, owner("p", "x"), from_p.clone());
        running.started(2, owner("q", "x"), from_q.clone());
        running.started(3, owner("p", "y"), elsewhere.clone());
        assert_eq!(running.stop_for_plugin(&PluginId::new("x").unwrap()), 2);
        assert_eq!(
            (from_p.stopped(), from_q.stopped(), elsewhere.stopped()),
            (1, 1, 0)
        );
    }

    #[test]
    fn a_transfer_that_ended_by_itself_is_forgotten_and_is_not_stopped_later() {
        let handle = Counting::default();
        let mut running = Running::default();
        running.started(7, owner("p", "x"), handle.clone());
        assert!(running.ended(7));
        assert!(!running.ended(7), "ending twice is simply unknown");
        // Withdrawing afterwards finds nothing to stop, so nothing is stopped twice.
        assert_eq!(running.stop_for(&peer("p"), None), 0);
        assert_eq!(handle.stopped(), 0);
    }

    #[test]
    fn stopping_is_reported_as_a_count_and_never_stops_the_same_transfer_twice() {
        let handle = Counting::default();
        let mut running = Running::default();
        running.started(1, owner("p", "x"), handle.clone());
        assert_eq!(running.stop_for(&peer("p"), None), 1);
        assert_eq!(running.stop_for(&peer("p"), None), 0);
        assert_eq!(handle.stopped(), 1);
    }

    #[test]
    fn a_call_that_expired_stops_its_own_transfer_and_no_other() {
        let (this, other, elsewhere) = (
            Counting::default(),
            Counting::default(),
            Counting::default(),
        );
        let mut running = Running::default();
        running.started(10, owner("p", "x"), this.clone());
        // Same device and same plugin, a different call: not the one that expired.
        running.started(11, owner("p", "x"), other.clone());
        running.started(12, owner("q", "y"), elsewhere.clone());
        assert!(running.stop_call(10));
        assert_eq!(
            (this.stopped(), other.stopped(), elsewhere.stopped()),
            (1, 0, 0)
        );
        assert_eq!(running.len(), 2);
    }

    #[test]
    fn the_end_of_a_call_with_no_transfer_running_stops_nothing() {
        // Most calls never carried a stream at all (an action, or a receive that came to
        // nothing), so this is the common case and must not touch anything else.
        let handle = Counting::default();
        let mut running = Running::default();
        running.started(5, owner("p", "x"), handle.clone());
        assert!(!running.stop_call(6));
        assert_eq!(handle.stopped(), 0);
        assert_eq!(running.len(), 1);
    }

    #[test]
    fn a_call_that_ends_after_its_transfer_has_finished_stops_nothing_twice() {
        let handle = Counting::default();
        let mut running = Running::default();
        running.started(3, owner("p", "x"), handle.clone());
        assert!(running.ended(3));
        assert!(!running.stop_call(3));
        assert_eq!(handle.stopped(), 0);
    }
}
