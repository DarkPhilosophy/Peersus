//! What a peer sent that is waiting for a plugin to come and collect it.
//!
//! When a peer opens a stream for a plugin the application does not hand it over at once:
//! the plugin is a separate process that has to be told, and that then connects back on its
//! own to fetch the bytes. Until it does, the stream is set aside here under the number the
//! application gave the call that announced it.
//!
//! Three things must hold, and each is a way a careless version leaks or hangs:
//!
//! * a parked stream is claimed once. A second claim for the same call gets nothing.
//! * it does not wait for ever. A plugin that never comes must not keep a peer's connection
//!   open indefinitely, so every entry has a deadline after which it is dropped, which also
//!   ends the peer's side.
//! * the number of parked streams is bounded, so a peer cannot grow this without limit by
//!   opening streams that no plugin ever collects.
use peersus_core::{model::PeerId, plugins::PluginId};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

/// Upper bound on streams waiting at once, across all peers.
pub const MAX_PARKED: usize = 32;

/// Longest a stream may wait for its plugin. A plugin that was started to receive it has to
/// connect within the time the call itself is allowed, so waiting longer than that serves
/// nobody.
pub const MAX_WAIT: Duration = Duration::from_secs(15);

/// How many candidate numbers [`Parked::park_fresh`] tries before giving up.
const MINT_TRIES: usize = 8;

/// Who a parked stream belongs to: the device that opened it and the plugin it is for.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Owner {
    pub peer: PeerId,
    pub plugin: PluginId,
}

struct Entry<T> {
    owner: Owner,
    deadline: Instant,
    item: T,
}

/// Streams set aside by call id, each with the peer it came from and a deadline.
pub struct Parked<T> {
    entries: HashMap<u64, Entry<T>>,
}

impl<T> Default for Parked<T> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }
}

/// Why a stream could not be set aside. In either case the stream is handed back, so the
/// caller closes it deliberately instead of it being dropped by accident.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused<T> {
    Full(T),
    /// That call id already has something parked. Ids are never reused, so this is a bug
    /// in the caller and must not silently replace what was there.
    Taken(T),
}

impl<T> Parked<T> {
    /// Change a claim's state without relinquishing its reservation or extending its lifetime.
    pub fn replace_for(&mut self, id: u64, owner: &Owner, item: T) {
        if let Some(entry) = self.entries.get_mut(&id)
            && &entry.owner == owner
        {
            entry.item = item;
        }
    }

    /// Set `item` aside for call `id`, to be collected before `now + wait`.
    pub fn park(
        &mut self,
        id: u64,
        owner: Owner,
        item: T,
        now: Instant,
        wait: Duration,
    ) -> Result<(), Refused<T>> {
        // Entries past their deadline are gone before the bound is checked, so a full
        // registry of stale entries cannot turn away a genuine new stream.
        self.expire(now);
        if self.entries.contains_key(&id) {
            return Err(Refused::Taken(item));
        }
        if self.entries.len() >= MAX_PARKED {
            return Err(Refused::Full(item));
        }
        self.entries.insert(
            id,
            Entry {
                owner,
                deadline: now + wait.min(MAX_WAIT),
                item,
            },
        );
        Ok(())
    }

    /// Set `item` aside under a number the table chooses, and return it.
    ///
    /// `mint` supplies candidate numbers. Zero is skipped, because zero means "no claim",
    /// and so is any number already in use, so one stream can never replace another. A
    /// source that keeps offering taken numbers is given up on after a few tries and the
    /// item is handed back, rather than looping.
    pub fn park_fresh(
        &mut self,
        owner: Owner,
        item: T,
        now: Instant,
        wait: Duration,
        mut mint: impl FnMut() -> u64,
    ) -> Result<u64, Refused<T>> {
        self.expire(now);
        if self.entries.len() >= MAX_PARKED {
            return Err(Refused::Full(item));
        }
        for _ in 0..MINT_TRIES {
            let number = mint();
            if number != 0 && !self.entries.contains_key(&number) {
                self.entries.insert(
                    number,
                    Entry {
                        owner,
                        deadline: now + wait.min(MAX_WAIT),
                        item,
                    },
                );
                return Ok(number);
            }
        }
        Err(Refused::Taken(item))
    }

    /// Take what was parked for call `id`, once. `None` if nothing is there, it was taken
    /// already, or it ran out of time.
    pub fn claim(&mut self, id: u64, now: Instant) -> Option<T> {
        self.expire(now);
        self.entries.remove(&id).map(|entry| entry.item)
    }

    /// Like [`Parked::claim`], and also says whose stream it was: the device that opened it
    /// and the plugin it was for. A transfer that starts from it has to be recorded under
    /// them, or a later withdrawal could not find it.
    pub fn claim_owned(&mut self, id: u64, now: Instant) -> Option<(Owner, T)> {
        self.expire(now);
        self.entries
            .remove(&id)
            .map(|entry| (entry.owner, entry.item))
    }

    /// Like [`Parked::claim_owned`], but only for the plugin the entry was issued to.
    ///
    /// The owner is checked BEFORE anything is taken. A plugin that names a number issued to
    /// another plugin gets nothing, and leaves the entry exactly where it was, so one plugin
    /// can neither collect what is not its own nor use up another's number by trying.
    pub fn claim_for(&mut self, id: u64, plugin: &PluginId, now: Instant) -> Option<(Owner, T)> {
        self.expire(now);
        if self.entries.get(&id)?.owner.plugin != *plugin {
            return None;
        }
        self.entries
            .remove(&id)
            .map(|entry| (entry.owner, entry.item))
    }

    /// Drop everything that waited too long and return how many were dropped.
    pub fn expire(&mut self, now: Instant) -> usize {
        let before = self.entries.len();
        self.entries.retain(|_, entry| entry.deadline > now);
        before - self.entries.len()
    }

    /// Take outstanding streams so permission withdrawal can answer them before closing.
    pub fn take_for(
        &mut self,
        plugin: &PluginId,
        peer: Option<&PeerId>,
        mut matches: impl FnMut(u64) -> bool,
    ) -> Vec<(u64, Owner, T)> {
        self.entries
            .extract_if(|id, entry| {
                &entry.owner.plugin == plugin
                    && peer.is_none_or(|peer| peer == &entry.owner.peer)
                    && matches(*id)
            })
            .map(|(id, entry)| (id, entry.owner, entry.item))
            .collect()
    }

    /// Drop what is parked for `peer`, for one plugin or (with `None`) for every plugin, as
    /// when a permission is withdrawn or the device is forgotten. Returns the numbers of the
    /// calls whose streams were dropped, in order, so that the caller can withdraw each of
    /// those calls at the hub as well: dropping the stream alone would leave the plugin
    /// waiting on a call that can never be served.
    pub fn drop_for(&mut self, peer: &PeerId, plugin: Option<&PluginId>) -> Vec<u64> {
        self.take_where(|owner| {
            &owner.peer == peer && plugin.is_none_or(|plugin| plugin == &owner.plugin)
        })
    }

    /// Drop everything parked for `plugin`, from every device, as when its process is
    /// restarted: a new process cannot collect what was announced to the old one. Returns
    /// the numbers of the calls whose streams were dropped, as [`Parked::drop_for`] does.
    pub fn drop_for_plugin(&mut self, plugin: &PluginId) -> Vec<u64> {
        self.take_where(|owner| &owner.plugin == plugin)
    }

    /// Remove every entry whose owner matches and return their call numbers, in order.
    fn take_where(&mut self, matches: impl Fn(&Owner) -> bool) -> Vec<u64> {
        let mut dropped = Vec::new();
        self.entries.retain(|id, entry| {
            let hit = matches(&entry.owner);
            if hit {
                dropped.push(*id);
            }
            !hit
        });
        dropped.sort_unstable();
        dropped
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(name: &str) -> PeerId {
        PeerId(name.into())
    }
    fn owner(device: &str, plugin: &str) -> Owner {
        Owner {
            peer: peer(device),
            plugin: PluginId::new(plugin).unwrap(),
        }
    }
    const WAIT: Duration = Duration::from_secs(10);

    #[test]
    fn a_parked_stream_is_claimed_once_and_only_by_its_call() {
        let now = Instant::now();
        let mut parked = Parked::default();
        parked
            .park(7, owner("p", "a"), "stream", now, WAIT)
            .unwrap();
        assert_eq!(parked.claim(8, now), None, "another call's id gets nothing");
        assert_eq!(parked.claim(7, now), Some("stream"));
        assert_eq!(parked.claim(7, now), None, "a second claim gets nothing");
    }

    #[test]
    fn claiming_a_stream_also_says_whose_it_was_so_it_can_be_stopped_later() {
        let now = Instant::now();
        let mut parked = Parked::default();
        parked
            .park(7, owner("p", "a"), "stream", now, WAIT)
            .unwrap();
        // A transfer that starts from this stream has to be findable by device and plugin,
        // or withdrawing a permission could not reach it once it is running.
        assert_eq!(
            parked.claim_owned(7, now),
            Some((owner("p", "a"), "stream"))
        );
        assert_eq!(parked.claim_owned(7, now), None, "still only once");
    }

    #[test]
    fn only_the_plugin_an_entry_was_issued_to_can_claim_it() {
        let now = Instant::now();
        let mut parked = Parked::default();
        parked
            .park(7, owner("p", "clip"), "mine", now, WAIT)
            .unwrap();
        let other = PluginId::new("other").unwrap();
        let clip = PluginId::new("clip").unwrap();
        assert_eq!(parked.claim_for(7, &other, now), None, "not its plugin");
        assert_eq!(
            parked.claim_for(7, &clip, now),
            Some((owner("p", "clip"), "mine"))
        );
    }

    #[test]
    fn a_wrong_plugin_asking_does_not_spend_the_entry_for_the_right_one() {
        let now = Instant::now();
        let mut parked = Parked::default();
        parked
            .park(7, owner("p", "clip"), "mine", now, WAIT)
            .unwrap();
        let other = PluginId::new("other").unwrap();
        for _ in 0..3 {
            assert_eq!(parked.claim_for(7, &other, now), None);
        }
        // Still there for its own plugin: nobody can burn another plugin's number.
        assert_eq!(parked.len(), 1);
        assert!(
            parked
                .claim_for(7, &PluginId::new("clip").unwrap(), now)
                .is_some()
        );
    }

    #[test]
    fn an_entry_is_claimed_once_by_its_own_plugin_too() {
        let now = Instant::now();
        let mut parked = Parked::default();
        parked
            .park(7, owner("p", "clip"), "mine", now, WAIT)
            .unwrap();
        let clip = PluginId::new("clip").unwrap();
        assert!(parked.claim_for(7, &clip, now).is_some());
        assert_eq!(parked.claim_for(7, &clip, now), None);
    }

    #[test]
    fn an_entry_past_its_deadline_cannot_be_claimed_even_by_its_own_plugin() {
        let now = Instant::now();
        let mut parked = Parked::default();
        parked
            .park(7, owner("p", "clip"), "mine", now, WAIT)
            .unwrap();
        let clip = PluginId::new("clip").unwrap();
        assert_eq!(
            parked.claim_for(7, &clip, now + WAIT + Duration::from_secs(1)),
            None
        );
    }

    /// A source of "random" numbers that plays back `values`, then repeats the last.
    fn playing(values: &[u64]) -> impl FnMut() -> u64 {
        let mut values = values.iter().copied();
        let mut last = 0;
        move || {
            if let Some(next) = values.next() {
                last = next;
            }
            last
        }
    }

    #[test]
    fn a_fresh_number_is_used_and_returned_so_the_item_can_be_claimed_by_it() {
        let now = Instant::now();
        let mut parked = Parked::default();
        let number = parked
            .park_fresh(owner("p", "a"), "stream", now, WAIT, playing(&[41]))
            .unwrap();
        assert_eq!(number, 41);
        assert_eq!(parked.claim(41, now), Some("stream"));
    }

    #[test]
    fn zero_is_never_used_because_it_means_no_claim() {
        let now = Instant::now();
        let mut parked = Parked::default();
        let number = parked
            .park_fresh(owner("p", "a"), "stream", now, WAIT, playing(&[0, 0, 9]))
            .unwrap();
        assert_eq!(number, 9, "a zero from the source must be skipped");
    }

    #[test]
    fn a_number_already_in_use_is_skipped_not_overwritten() {
        let now = Instant::now();
        let mut parked = Parked::default();
        parked.park(5, owner("p", "a"), "first", now, WAIT).unwrap();
        let number = parked
            .park_fresh(owner("q", "b"), "second", now, WAIT, playing(&[5, 5, 6]))
            .unwrap();
        assert_eq!(number, 6);
        // Neither stream replaced the other.
        assert_eq!(parked.claim(5, now), Some("first"));
        assert_eq!(parked.claim(6, now), Some("second"));
    }

    #[test]
    fn a_source_that_only_repeats_a_taken_number_gives_up_and_hands_the_item_back() {
        let now = Instant::now();
        let mut parked = Parked::default();
        parked.park(5, owner("p", "a"), "first", now, WAIT).unwrap();
        let refused = parked
            .park_fresh(owner("q", "b"), "second", now, WAIT, playing(&[5]))
            .unwrap_err();
        assert_eq!(refused, Refused::Taken("second"));
        assert_eq!(parked.claim(5, now), Some("first"), "what was there stays");
    }

    #[test]
    fn a_full_table_hands_the_item_back_instead_of_growing() {
        let now = Instant::now();
        let mut parked = Parked::default();
        for n in 1..=MAX_PARKED as u64 {
            parked.park(n, owner("p", "a"), "x", now, WAIT).unwrap();
        }
        let refused = parked
            .park_fresh(owner("p", "a"), "late", now, WAIT, playing(&[1000]))
            .unwrap_err();
        assert_eq!(refused, Refused::Full("late"));
    }

    #[test]
    fn a_stream_nobody_collects_is_dropped_at_its_deadline_not_before() {
        let now = Instant::now();
        let mut parked = Parked::default();
        parked.park(1, owner("p", "a"), "late", now, WAIT).unwrap();
        // One instant before the deadline it is still there; at the deadline it is gone.
        assert_eq!(
            parked.claim(1, now + WAIT - Duration::from_millis(1)),
            Some("late")
        );
        parked.park(2, owner("p", "a"), "later", now, WAIT).unwrap();
        assert_eq!(parked.claim(2, now + WAIT), None);
        assert_eq!(
            parked.len(),
            0,
            "an expired entry is removed, not just hidden"
        );
    }

    #[test]
    fn a_caller_cannot_ask_for_longer_than_the_maximum() {
        let now = Instant::now();
        let mut parked = Parked::default();
        parked
            .park(1, owner("p", "a"), "x", now, Duration::from_secs(3600))
            .unwrap();
        assert_eq!(parked.claim(1, now + MAX_WAIT), None);
    }

    #[test]
    fn the_number_waiting_is_bounded_and_the_refused_stream_comes_back() {
        let now = Instant::now();
        let mut parked = Parked::default();
        for id in 0..MAX_PARKED as u64 {
            parked.park(id, owner("p", "a"), id, now, WAIT).unwrap();
        }
        assert_eq!(
            parked.park(999, owner("p", "a"), 999, now, WAIT),
            Err(Refused::Full(999)),
            "the stream is returned so it can be closed on purpose"
        );
        // Collecting one frees exactly one place.
        assert!(parked.claim(0, now).is_some());
        parked.park(999, owner("p", "a"), 999, now, WAIT).unwrap();
    }

    #[test]
    fn stale_entries_never_turn_away_a_genuine_new_stream() {
        let now = Instant::now();
        let mut parked = Parked::default();
        for id in 0..MAX_PARKED as u64 {
            parked.park(id, owner("p", "a"), id, now, WAIT).unwrap();
        }
        // Long after, every old entry has expired. The registry looks full but is not.
        let later = now + WAIT + Duration::from_secs(1);
        parked.park(500, owner("q", "a"), 500, later, WAIT).unwrap();
        assert_eq!(parked.len(), 1);
    }

    #[test]
    fn a_call_id_that_already_has_a_stream_is_refused_not_replaced() {
        let now = Instant::now();
        let mut parked = Parked::default();
        parked.park(3, owner("p", "a"), "first", now, WAIT).unwrap();
        assert_eq!(
            parked.park(3, owner("p", "a"), "second", now, WAIT),
            Err(Refused::Taken("second"))
        );
        assert_eq!(parked.claim(3, now), Some("first"));
    }

    #[test]
    fn forgetting_a_device_drops_its_streams_for_every_plugin_and_reports_the_calls() {
        let now = Instant::now();
        let mut parked = Parked::default();
        parked.park(1, owner("gone", "a"), "a", now, WAIT).unwrap();
        parked.park(2, owner("gone", "b"), "b", now, WAIT).unwrap();
        parked.park(3, owner("stays", "a"), "c", now, WAIT).unwrap();
        // The call ids come back so the caller can withdraw each call at the hub too: a
        // stream that was dropped but whose call still stands would leave a plugin waiting.
        assert_eq!(parked.drop_for(&peer("gone"), None), [1, 2]);
        assert_eq!(parked.claim(1, now), None);
        assert_eq!(parked.claim(2, now), None, "the other plugin's stream too");
        assert_eq!(parked.claim(3, now), Some("c"));
    }

    #[test]
    fn withdrawing_one_plugin_for_a_device_leaves_that_devices_other_plugins_alone() {
        let now = Instant::now();
        let mut parked = Parked::default();
        parked.park(1, owner("p", "a"), "to-a", now, WAIT).unwrap();
        parked.park(2, owner("p", "b"), "to-b", now, WAIT).unwrap();
        parked
            .park(3, owner("q", "a"), "other-device", now, WAIT)
            .unwrap();
        let plugin_a = PluginId::new("a").unwrap();
        assert_eq!(parked.drop_for(&peer("p"), Some(&plugin_a)), [1]);
        assert_eq!(
            parked.claim(1, now),
            None,
            "the withdrawn stream must really be gone"
        );
        // Same device, different plugin, and same plugin, different device: both untouched.
        assert_eq!(parked.claim(2, now), Some("to-b"));
        assert_eq!(parked.claim(3, now), Some("other-device"));
    }

    #[test]
    fn restarting_a_plugin_drops_what_was_waiting_for_it_from_every_device_and_only_that() {
        let now = Instant::now();
        let mut parked = Parked::default();
        parked.park(1, owner("p", "a"), "x", now, WAIT).unwrap();
        parked.park(2, owner("q", "a"), "y", now, WAIT).unwrap();
        parked.park(3, owner("p", "b"), "z", now, WAIT).unwrap();
        // A new process cannot collect what was announced to the old one, and the calls that
        // announced them go with it, so the numbers come back to be withdrawn at the hub.
        let plugin_a = PluginId::new("a").unwrap();
        assert_eq!(parked.drop_for_plugin(&plugin_a), [1, 2]);
        // Reporting a stream as dropped is not enough: it must really be gone, or a process
        // that was supposed to have lost its work could still collect it.
        assert_eq!(parked.claim(1, now), None);
        assert_eq!(parked.claim(2, now), None);
        assert_eq!(parked.claim(3, now), Some("z"));
    }
}
