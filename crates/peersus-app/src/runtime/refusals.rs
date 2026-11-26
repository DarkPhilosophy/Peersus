//! What paired devices answered with a plain no, and for how long that answer is believed.
//!
//! A no says only that the device would not take this: not why, and not whether it will a moment from now. It is believed while
//! nothing known of the route has changed (its acceptance, its visibility, its connection: the route's stamp), so that a
//! destination that said no is not opened again at every attempt. And a no told during a visit is that visit's: a visit is
//! the person's own crossing, so the next crossing asks again, and a no that arrives for a visit that is already over answers
//! nothing that is still being asked, so it is not believed at all. Without that, a refusal that only meant "not now" would
//! keep a person from the device until the application was restarted, and say nothing while it did.
use super::*;

struct Refusal<S> {
    destination: RemoteDestination,
    /// The route's stamp when the answer was asked for.
    stamp: S,
    refused: watch::Sender<bool>,
    /// The visit the no was told in, when it was told in one.
    told_in: Option<u64>,
}

pub(super) struct RemoteRefusals<S> {
    entries: Vec<Refusal<S>>,
}

impl<S> Default for RemoteRefusals<S> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

impl<S: Copy + PartialEq> RemoteRefusals<S> {
    /// Forget what no longer holds: an answer for a route that is not as it was, and a reservation that nobody holds and that
    /// holds no refusal.
    pub(super) fn prune(&mut self, stamp_of: impl Fn(&RemoteDestination) -> Option<S>) {
        self.entries.retain(|entry| {
            stamp_of(&entry.destination) == Some(entry.stamp)
                && (*entry.refused.borrow() || entry.refused.receiver_count() > 0)
        });
    }

    /// Room for one more send to `destination` in `visit` (`None` outside one), or the refusal it is already known to meet.
    pub(super) fn reserve(
        &mut self,
        destination: &RemoteDestination,
        stamp: S,
        visit: Option<u64>,
        stamp_of: impl Fn(&RemoteDestination) -> Option<S>,
    ) -> Result<watch::Receiver<bool>, SendRefusal> {
        self.prune(stamp_of);
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| &entry.destination == destination && entry.stamp == stamp)
        {
            if *entry.refused.borrow() {
                // The person who crosses again is asking again.
                let asked_again = entry
                    .told_in
                    .is_some_and(|told| visit.is_some_and(|now| now > told));
                if !asked_again {
                    return Err(SendRefusal::NotAccepted);
                }
                entry.told_in = None;
                entry.refused.send_replace(false);
            }
            return Ok(entry.refused.subscribe());
        }
        if self.entries.len() >= MOST_REMOTE_REFUSALS
            || self
                .entries
                .iter()
                .filter(|entry| entry.destination.owner.plugin == destination.owner.plugin)
                .count()
                >= MOST_REMOTE_REFUSALS_PER_EXTENSION
        {
            return Err("Too many extension destinations are already guarded".into());
        }
        let (refused, reader) = watch::channel(false);
        self.entries.push(Refusal {
            destination: destination.clone(),
            stamp,
            refused,
            told_in: None,
        });
        Ok(reader)
    }

    /// Believe a no, if it still answers something being asked: `false` when it is not believed, because it is for a visit that
    /// is over (`visit_now` is the person's latest crossing), or for a reservation that is no longer the route's.
    pub(super) fn record(
        &mut self,
        no: &RemoteNo,
        visit_now: u64,
        stamp_of: impl Fn(&RemoteDestination) -> Option<S>,
    ) -> bool {
        if no.visit.is_some_and(|told| told < visit_now) {
            return false;
        }
        let Some(entry) = self.entries.iter_mut().find(|entry| {
            entry.destination == no.destination
                && entry.refused.subscribe().same_channel(&no.reservation)
                && stamp_of(&entry.destination) == Some(entry.stamp)
        }) else {
            // Even an exact route can change and then return to its old stamp. A late answer from that retired reservation
            // cannot bind to a new one.
            return false;
        };
        entry.told_in = no.visit;
        entry.refused.send_replace(true);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(service: &str) -> RemoteDestination {
        RemoteDestination {
            owner: crate::parked::Owner {
                peer: PeerId("far".into()),
                plugin: PluginId::new("carrier").unwrap(),
            },
            service: service.into(),
            version: 1,
        }
    }

    /// A route whose stamp is whatever the test says it is now.
    fn stamped(now: u32) -> impl Fn(&RemoteDestination) -> Option<u32> {
        move |_| Some(now)
    }

    fn no(
        destination: &RemoteDestination,
        reservation: &watch::Receiver<bool>,
        visit: Option<u64>,
    ) -> RemoteNo {
        RemoteNo {
            destination: destination.clone(),
            reservation: reservation.clone(),
            visit,
        }
    }

    #[test]
    fn a_no_holds_for_the_visit_it_was_told_in_and_the_next_crossing_asks_again() {
        let (mut refusals, route) = (RemoteRefusals::default(), route("pointer"));
        let held = refusals.reserve(&route, 1, Some(1), stamped(1)).unwrap();
        assert!(refusals.record(&no(&route, &held, Some(1)), 1, stamped(1)));
        // The rest of that visit meets it without opening anything.
        assert_eq!(
            refusals
                .reserve(&route, 1, Some(1), stamped(1))
                .unwrap_err(),
            SendRefusal::NotAccepted
        );
        // The person crosses again: the answer is asked for again, on the same reservation.
        let again = refusals.reserve(&route, 1, Some(2), stamped(1)).unwrap();
        assert!(again.same_channel(&held));
        assert!(!*again.borrow());
        // And that visit's own no holds for it in turn, and lets go for the next.
        assert!(refusals.record(&no(&route, &again, Some(2)), 2, stamped(1)));
        assert!(refusals.reserve(&route, 1, Some(2), stamped(1)).is_err());
        assert!(refusals.reserve(&route, 1, Some(3), stamped(1)).is_ok());
    }

    #[test]
    fn a_no_that_arrives_for_a_visit_that_is_over_is_not_believed() {
        let (mut refusals, route) = (RemoteRefusals::default(), route("pointer"));
        let held = refusals.reserve(&route, 1, Some(1), stamped(1)).unwrap();
        // The person has crossed again by the time the old visit's last stream is answered.
        assert!(!refusals.record(&no(&route, &held, Some(1)), 2, stamped(1)));
        assert!(
            !*held.borrow(),
            "nothing was recorded against the new visit"
        );
        let next = refusals.reserve(&route, 1, Some(2), stamped(1)).unwrap();
        assert!(!*next.borrow());
    }

    #[test]
    fn a_no_told_outside_any_visit_is_believed_until_the_route_changes() {
        let (mut refusals, route) = (RemoteRefusals::default(), route("files"));
        let held = refusals.reserve(&route, 1, None, stamped(1)).unwrap();
        assert!(refusals.record(&no(&route, &held, None), 1, stamped(1)));
        for visit in [None, Some(1), Some(9)] {
            assert_eq!(
                refusals.reserve(&route, 1, visit, stamped(1)).unwrap_err(),
                SendRefusal::NotAccepted,
                "{visit:?}"
            );
        }
        // The device says it takes it now: that is a different route, and it is asked.
        let asked = refusals.reserve(&route, 2, None, stamped(2)).unwrap();
        assert!(!*asked.borrow());
    }

    #[test]
    fn a_no_told_in_a_visit_is_forgotten_with_the_route_it_was_told_for() {
        let (mut refusals, route) = (RemoteRefusals::default(), route("pointer"));
        let held = refusals.reserve(&route, 1, Some(1), stamped(1)).unwrap();
        assert!(refusals.record(&no(&route, &held, Some(1)), 1, stamped(1)));
        // The route changed and came back: the old reservation cannot answer for the new one.
        let fresh = refusals.reserve(&route, 2, Some(1), stamped(2)).unwrap();
        assert!(!fresh.same_channel(&held));
        assert!(!refusals.record(&no(&route, &held, Some(1)), 1, stamped(2)));
    }

    #[test]
    fn a_no_for_one_route_does_not_refuse_another() {
        let mut refusals = RemoteRefusals::default();
        let (pointer, clipboard) = (route("pointer"), route("clipboard"));
        let held = refusals.reserve(&pointer, 1, Some(1), stamped(1)).unwrap();
        let other = refusals
            .reserve(&clipboard, 1, Some(1), stamped(1))
            .unwrap();
        assert!(refusals.record(&no(&pointer, &held, Some(1)), 1, stamped(1)));
        assert!(!*other.borrow());
        assert!(
            !refusals.record(&no(&clipboard, &held, Some(1)), 1, stamped(1)),
            "an answer for one reservation is not recorded against another"
        );
        assert!(refusals.reserve(&pointer, 1, Some(1), stamped(1)).is_err());
        assert!(refusals.reserve(&clipboard, 1, Some(1), stamped(1)).is_ok());
    }

    #[test]
    fn what_nobody_holds_and_nothing_refused_is_not_kept() {
        let (mut refusals, route) = (RemoteRefusals::default(), route("pointer"));
        let held = refusals.reserve(&route, 1, Some(1), stamped(1)).unwrap();
        drop(held);
        refusals.prune(stamped(1));
        assert!(refusals.entries.is_empty());
        // A refusal is kept even when nobody holds it: it is what the next attempt is answered with.
        let held = refusals.reserve(&route, 1, Some(1), stamped(1)).unwrap();
        assert!(refusals.record(&no(&route, &held, Some(1)), 1, stamped(1)));
        drop(held);
        refusals.prune(stamped(1));
        assert_eq!(refusals.entries.len(), 1);
    }
}
