//! Which calls the application has made to plugins and is still waiting on.
//!
//! Two jobs depend on knowing this. A user who presses the same button twice must not start
//! two runs of one action for one device, and withdrawing what a plugin may do for a device
//! must stop the work already started for that device, without touching the plugin process
//! or what it is doing for anyone else. The hub answers neither question, because it only
//! knows about individual calls, not about the plugin, action and device they belong to.
use peersus_core::{model::PeerId, plugins::PluginId};
use std::collections::HashMap;

/// A call the application is waiting on: whose action, which action and which device.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub plugin: PluginId,
    pub action: String,
    /// The device the action was run for, or `None` for an action that uses no device.
    pub device: Option<PeerId>,
}

/// At most one outstanding call per [`Key`], bounded overall.
#[derive(Debug, Default)]
pub struct InFlight {
    calls: HashMap<Key, u64>,
}

/// Why a call could not be registered.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// The same action for the same device is still running.
    Pending,
    /// Too many different operations are outstanding at once.
    Full,
}

/// Upper bound on outstanding calls, so a flood of distinct actions cannot grow this
/// without limit.
pub const MAX_IN_FLIGHT: usize = 64;

impl InFlight {
    /// Record that `id` is now outstanding for `key`, unless that would be a duplicate.
    pub fn start(&mut self, key: Key, id: u64) -> Result<(), Refused> {
        if self.calls.contains_key(&key) {
            return Err(Refused::Pending);
        }
        if self.calls.len() >= MAX_IN_FLIGHT {
            return Err(Refused::Full);
        }
        self.calls.insert(key, id);
        Ok(())
    }

    /// The call with this id has ended, whatever way. Returns whether it was known. The
    /// application itself always wants to know whose call it was ([`InFlight::finish_key`]);
    /// this shorter form is for the tests of the rules around it.
    #[cfg(test)]
    pub fn finish(&mut self, id: u64) -> bool {
        self.finish_key(id).is_some()
    }

    /// Like [`InFlight::finish`], and says whose call it was: the action and device it was
    /// for, so what became of it can be recorded against them.
    pub fn finish_key(&mut self, id: u64) -> Option<Key> {
        let key = self
            .calls
            .iter()
            .find(|(_, outstanding)| **outstanding == id)
            .map(|(key, _)| key.clone())?;
        self.calls.remove(&key);
        Some(key)
    }

    /// Take exactly the requested action, leaving every other operation intact.
    pub fn take_action(&mut self, key: &Key) -> Option<u64> {
        self.calls.remove(key)
    }

    /// Ids of every call outstanding for `plugin` on `device`, removed from the registry so
    /// the caller can withdraw them. A call for another device or plugin is never included.
    pub fn take_for_device(&mut self, plugin: &PluginId, device: &PeerId) -> Vec<u64> {
        self.take_where(|key| &key.plugin == plugin && key.device.as_ref() == Some(device))
    }

    /// Ids of every outstanding call for `device`, whichever plugin it was made to.
    pub fn take_all_for_device(&mut self, device: &PeerId) -> Vec<u64> {
        self.take_where(|key| key.device.as_ref() == Some(device))
    }

    /// Every outstanding call to `plugin`, with or without a device. A new process knows
    /// nothing of what the old one was doing, so none of it can ever be answered.
    pub fn take_for_plugin(&mut self, plugin: &PluginId) -> Vec<u64> {
        self.take_where(|key| &key.plugin == plugin)
    }

    fn take_where(&mut self, matches: impl Fn(&Key) -> bool) -> Vec<u64> {
        let mut taken = Vec::new();
        self.calls.retain(|key, id| {
            if matches(key) {
                taken.push(*id);
                false
            } else {
                true
            }
        });
        taken.sort_unstable();
        taken
    }

    /// How many calls are outstanding. Only the tests of this type need to ask.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.calls.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(plugin: &str, action: &str, device: Option<&str>) -> Key {
        Key {
            plugin: PluginId::new(plugin).unwrap(),
            action: action.into(),
            device: device.map(|d| PeerId(d.into())),
        }
    }

    #[test]
    fn the_same_action_for_the_same_device_cannot_run_twice_at_once() {
        let mut calls = InFlight::default();
        calls.start(key("a", "share", Some("p1")), 1).unwrap();
        assert_eq!(
            calls.start(key("a", "share", Some("p1")), 2),
            Err(Refused::Pending)
        );
        assert_eq!(calls.len(), 1, "a refused duplicate leaves nothing behind");
    }

    #[test]
    fn a_different_device_action_or_plugin_is_not_a_duplicate() {
        let mut calls = InFlight::default();
        calls.start(key("a", "share", Some("p1")), 1).unwrap();
        calls.start(key("a", "share", Some("p2")), 2).unwrap();
        calls.start(key("a", "other", Some("p1")), 3).unwrap();
        calls.start(key("b", "share", Some("p1")), 4).unwrap();
        calls.start(key("a", "share", None), 5).unwrap();
        assert_eq!(calls.len(), 5);
    }

    #[test]
    fn finishing_a_call_allows_the_same_action_again() {
        let mut calls = InFlight::default();
        calls.start(key("a", "share", Some("p1")), 7).unwrap();
        assert!(calls.finish(7));
        calls.start(key("a", "share", Some("p1")), 8).unwrap();
        // An id nobody is waiting on is simply unknown, never an error and never a removal
        // of something else.
        assert!(!calls.finish(999));
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn withdrawing_a_device_takes_only_that_devices_calls_for_that_plugin() {
        let mut calls = InFlight::default();
        calls.start(key("a", "share", Some("p1")), 1).unwrap();
        calls.start(key("a", "other", Some("p1")), 2).unwrap();
        calls.start(key("a", "share", Some("p2")), 3).unwrap();
        calls.start(key("b", "share", Some("p1")), 4).unwrap();
        calls.start(key("a", "global", None), 5).unwrap();
        let taken = calls.take_for_device(&PluginId::new("a").unwrap(), &PeerId("p1".into()));
        assert_eq!(taken, [1, 2]);
        // Everything else is still outstanding: another device, another plugin and a call
        // that uses no device at all.
        assert_eq!(calls.len(), 3);
        assert!(!calls.finish(1), "taken calls are no longer registered");
        assert!(calls.finish(3) && calls.finish(4) && calls.finish(5));
    }

    #[test]
    fn forgetting_a_device_takes_its_calls_across_every_plugin() {
        let mut calls = InFlight::default();
        calls.start(key("a", "share", Some("p1")), 1).unwrap();
        calls.start(key("b", "share", Some("p1")), 2).unwrap();
        calls.start(key("b", "share", Some("p2")), 3).unwrap();
        calls.start(key("a", "global", None), 4).unwrap();
        assert_eq!(calls.take_all_for_device(&PeerId("p1".into())), [1, 2]);
        assert_eq!(calls.len(), 2);
    }

    #[test]
    fn the_number_of_outstanding_calls_is_bounded() {
        let mut calls = InFlight::default();
        for n in 0..MAX_IN_FLIGHT as u64 {
            calls
                .start(key("a", &format!("act{n}"), None), n + 1)
                .unwrap();
        }
        assert_eq!(
            calls.start(key("a", "one-too-many", None), 1_000),
            Err(Refused::Full)
        );
        // Finishing one frees exactly one place.
        assert!(calls.finish(1));
        calls.start(key("a", "one-too-many", None), 1_000).unwrap();
    }

    #[test]
    fn restarting_a_plugin_forgets_all_its_calls_and_only_its_calls() {
        let mut calls = InFlight::default();
        calls.start(key("a", "share", Some("p1")), 1).unwrap();
        calls.start(key("a", "share", Some("p2")), 2).unwrap();
        calls.start(key("a", "global", None), 3).unwrap();
        calls.start(key("b", "share", Some("p1")), 4).unwrap();
        // A new process knows nothing of what the old one was doing, whether or not those
        // calls were for a device, but a different plugin is unaffected.
        assert_eq!(
            calls.take_for_plugin(&PluginId::new("a").unwrap()),
            [1, 2, 3]
        );
        assert_eq!(calls.len(), 1);
        assert!(calls.finish(4));
    }

    #[test]
    fn finishing_a_call_says_whose_it_was() {
        let mut calls = InFlight::default();
        calls.start(key("a", "share", Some("p1")), 1).unwrap();
        calls.start(key("a", "share", Some("p2")), 2).unwrap();
        assert_eq!(calls.finish_key(2), Some(key("a", "share", Some("p2"))));
        // The other one is untouched, and an id that is not outstanding names nobody.
        assert_eq!(calls.len(), 1);
        assert_eq!(calls.finish_key(2), None);
        assert_eq!(calls.finish_key(99), None);
        assert_eq!(calls.finish_key(1), Some(key("a", "share", Some("p1"))));
    }
}
