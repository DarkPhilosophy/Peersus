//! Input from a page's surface on its way to the plugin, at most one call at a time per surface.
//!
//! A finger moves sixty times a second and a plugin answers when it answers: sending a call for
//! each would queue motion behind motion and the pointer would trail the finger. Instead what
//! arrives while a call is out waits here, consecutive motion is added into one, and the next
//! call takes all of it the moment the last one is answered. Nothing a person pressed or typed
//! is merged or dropped: only how far the pointer went is summed, and of the places it was put
//! in a row only the last is kept.
use peersus_api::contributions::{MAX_POINTER_BATCH, PointerInput};
use peersus_core::{model::PeerId, plugins::PluginId};
use std::collections::{HashMap, VecDeque};

/// One surface a person is using: a page of a plugin, for one device.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SurfaceKey {
    pub plugin: PluginId,
    pub device: Option<PeerId>,
}

/// Whether a captured surface was stopped by a refusal or lost to a failure.
#[derive(Clone)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) struct SurfaceNotice {
    pub(crate) key: SurfaceKey,
    pub(crate) blocked: bool,
    pub(crate) visit: Option<u64>,
}

/// Most inputs held while a call is out. Motion is summed and a place replaced by the newer one, so
/// only presses and text fill it: a person cannot outpace this, a stuck plugin can, and is then
/// refused rather than buffered for ever.
const MOST_WAITING: usize = 4 * MAX_POINTER_BATCH;

#[derive(Default)]
struct Queue {
    /// The call out now, by its number.
    out: Option<u64>,
    waiting: VecDeque<Pending>,
}

/// A stream may go idle while its enclosing desktop visit is still active.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ending {
    Copy,
    Visit,
}

enum Pending {
    Begin,
    Input(PointerInput),
    End(Ending),
}

/// What to send now: a batch, and whether it is the last of this use of the surface.
#[derive(Debug, PartialEq)]
pub struct Batch {
    pub events: Vec<PointerInput>,
    pub begin: bool,
    pub(crate) end: Option<Ending>,
}

#[derive(Default)]
pub struct Surfaces {
    queues: HashMap<SurfaceKey, Queue>,
    /// Which surface each call out belongs to.
    calls: HashMap<u64, SurfaceKey>,
}

impl Surfaces {
    /// Take `events` for `key`. Returns a batch to send at once when no call is out, otherwise
    /// keeps them for [`Surfaces::answered`].
    pub(crate) fn offer(
        &mut self,
        key: &SurfaceKey,
        events: Vec<PointerInput>,
        begin: bool,
        end: Option<Ending>,
    ) -> Result<Option<Batch>, &'static str> {
        let queue = self.queues.entry(key.clone()).or_default();
        if begin && !events.is_empty() {
            if queue.waiting.len() >= MOST_WAITING - 1 {
                return Err("The extension is not keeping up with this input");
            }
            queue.waiting.push_back(Pending::Begin);
        }
        for event in events {
            let full = queue.waiting.len() >= MOST_WAITING;
            match (queue.waiting.back_mut(), &event) {
                (
                    Some(Pending::Input(PointerInput::Move { dx, dy })),
                    PointerInput::Move {
                        dx: more_x,
                        dy: more_y,
                    },
                ) => {
                    *dx += more_x;
                    *dy += more_y;
                }
                // Where the pointer is, said again: the place that waits says nothing any more.
                (
                    Some(Pending::Input(PointerInput::Position { x, y })),
                    PointerInput::Position { x: to_x, y: to_y },
                ) => {
                    (*x, *y) = (*to_x, *to_y);
                }
                _ if full => {
                    return Err("The extension is not keeping up with this input");
                }
                _ => queue.waiting.push_back(Pending::Input(event)),
            }
        }
        if let Some(end) = end {
            // Keep one extra slot for a terminal marker even at the payload bound.
            if let Some(Pending::End(waiting)) = queue.waiting.back_mut() {
                if end == Ending::Visit {
                    *waiting = end;
                }
            } else {
                queue.waiting.push_back(Pending::End(end));
            }
        }
        Ok(if queue.out.is_none() {
            Self::next(queue)
        } else {
            None
        })
    }

    /// The batch to send for a queue that has no call out, if it has anything to say.
    fn next(queue: &mut Queue) -> Option<Batch> {
        let begin = matches!(queue.waiting.front(), Some(Pending::Begin));
        if begin {
            queue.waiting.pop_front();
        }
        let mut events = Vec::new();
        while events.len() < MAX_POINTER_BATCH
            && matches!(queue.waiting.front(), Some(Pending::Input(_)))
        {
            if let Some(Pending::Input(event)) = queue.waiting.pop_front() {
                events.push(event);
            }
        }
        // Never merge the preceding end or a later rest into the first batch of a new visit.
        let end = match queue.waiting.front() {
            Some(Pending::End(end)) if !begin => {
                let end = *end;
                queue.waiting.pop_front();
                Some(end)
            }
            _ => None,
        };
        (!events.is_empty() || end.is_some()).then_some(Batch { events, begin, end })
    }

    /// The call `id` was sent for `key`.
    pub fn sent(&mut self, key: &SurfaceKey, id: u64) {
        if let Some(queue) = self.queues.get_mut(key) {
            queue.out = Some(id);
            self.calls.insert(id, key.clone());
        }
    }

    /// A call that never left: nothing is out for its surface, and what it carried is gone.
    pub fn unsent(&mut self, key: &SurfaceKey) {
        if let Some(queue) = self.queues.get_mut(key) {
            queue.out = None;
        }
    }

    /// Whether call `id` is one of these.
    pub fn owns(&self, id: u64) -> bool {
        self.calls.contains_key(&id)
    }

    /// Call `id` was answered. Returns its surface and the next batch for it, if any waits. A
    /// surface with nothing waiting and nothing out is forgotten.
    pub fn answered(&mut self, id: u64) -> Option<(SurfaceKey, Option<Batch>)> {
        let key = self.calls.remove(&id)?;
        let queue = self.queues.get_mut(&key)?;
        queue.out = None;
        let next = Self::next(queue);
        if next.is_none() {
            self.queues.remove(&key);
        }
        Some((key, next))
    }

    /// Call `id` came back as a failure: what it carried did not arrive. What waits behind it is what the
    /// person did while nothing was arriving, and is dropped, since a click or a key replayed seconds late
    /// lands on whatever is under the pointer by then. Only an end still goes, so the plugin lets go of
    /// what it holds. Returns the surface and that end, if one waits.
    pub fn failed(&mut self, id: u64) -> Option<(SurfaceKey, Option<Batch>)> {
        if let Some(queue) = self.calls.get(&id).and_then(|key| self.queues.get_mut(key)) {
            let end = queue.waiting.iter().rev().find_map(|event| match event {
                Pending::End(end) => Some(*end),
                _ => None,
            });
            queue.waiting.clear();
            if let Some(end) = end {
                queue.waiting.push_back(Pending::End(end));
            }
        }
        self.answered(id)
    }

    /// Withdraw pending and in-flight input for a device before cancelling its calls.
    pub fn forget_device(&mut self, plugin: Option<&PluginId>, device: &PeerId) -> Vec<u64> {
        let matches = |key: &SurfaceKey| {
            key.device.as_ref() == Some(device) && plugin.is_none_or(|plugin| &key.plugin == plugin)
        };
        let ids = self
            .calls
            .iter()
            .filter_map(|(id, key)| matches(key).then_some(*id))
            .collect();
        self.queues.retain(|key, _| !matches(key));
        self.calls.retain(|_, key| !matches(key));
        ids
    }

    /// Everything of a plugin that is gone: nothing more is sent for it.
    pub fn forget(&mut self, plugin: &PluginId) {
        self.queues.retain(|key, _| &key.plugin != plugin);
        self.calls.retain(|_, key| &key.plugin != plugin);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peersus_api::contributions::Button;

    fn key() -> SurfaceKey {
        SurfaceKey {
            plugin: PluginId::new("control").unwrap(),
            device: Some(PeerId("pc".into())),
        }
    }
    fn motion(dx: f32) -> PointerInput {
        PointerInput::Move { dx, dy: 1. }
    }
    fn press(down: bool) -> PointerInput {
        PointerInput::Button {
            button: Button::Left,
            down,
        }
    }

    #[test]
    fn the_first_input_goes_at_once_and_what_comes_meanwhile_waits_with_motion_summed() {
        let mut surfaces = Surfaces::default();
        let first = surfaces
            .offer(&key(), vec![motion(1.)], false, None)
            .unwrap();
        assert_eq!(
            first,
            Some(Batch {
                events: vec![motion(1.)],
                begin: false,
                end: None
            })
        );
        surfaces.sent(&key(), 7);
        for event in [motion(2.), motion(3.), press(true), motion(4.)] {
            assert_eq!(
                surfaces.offer(&key(), vec![event], false, None).unwrap(),
                None
            );
        }
        assert!(surfaces.owns(7));
        let (answered, next) = surfaces.answered(7).unwrap();
        assert_eq!(answered, key());
        assert_eq!(
            next,
            Some(Batch {
                events: vec![
                    PointerInput::Move { dx: 5., dy: 2. },
                    press(true),
                    motion(4.)
                ],
                begin: false,
                end: None
            }),
            "motion is summed up to a press, and nothing pressed is merged or dropped"
        );
    }

    #[test]
    fn a_place_that_waits_is_replaced_by_the_newer_one_and_never_fills_the_queue() {
        let place = |x: f32| PointerInput::Position { x, y: 0.5 };
        let mut surfaces = Surfaces::default();
        surfaces
            .offer(&key(), vec![press(true)], false, None)
            .unwrap();
        surfaces.sent(&key(), 1);
        // A pointer carried across for as long as a call is out: far more places than the queue
        // holds, and each is where the pointer is, so only the last one says anything.
        let steps = u16::try_from(MOST_WAITING * 2).unwrap();
        for step in 0..steps {
            let event = place(f32::from(step) / f32::from(steps));
            assert_eq!(
                surfaces.offer(&key(), vec![event], false, None).unwrap(),
                None
            );
        }
        // A press is where the pointer was then: the place before it is kept, and the next one
        // comes after it.
        surfaces
            .offer(
                &key(),
                vec![press(false), place(0.25), place(0.75)],
                false,
                None,
            )
            .unwrap();
        let last = f32::from(steps - 1) / f32::from(steps);
        assert_eq!(
            surfaces.answered(1).unwrap().1,
            Some(Batch {
                events: vec![place(last), press(false), place(0.75)],
                begin: false,
                end: None
            })
        );
    }

    #[test]
    fn a_failed_call_drops_what_waited_behind_it_and_keeps_only_the_end() {
        let mut surfaces = Surfaces::default();
        surfaces
            .offer(&key(), vec![motion(1.)], false, None)
            .unwrap();
        surfaces.sent(&key(), 1);
        // Done while nothing was arriving: a click that must not land seconds later.
        surfaces
            .offer(&key(), vec![press(true), press(false)], false, None)
            .unwrap();
        assert_eq!(surfaces.failed(1), Some((key(), None)));
        assert!(surfaces.queues.is_empty());

        surfaces
            .offer(&key(), vec![press(true)], false, None)
            .unwrap();
        surfaces.sent(&key(), 2);
        surfaces
            .offer(&key(), vec![motion(3.)], false, Some(Ending::Copy))
            .unwrap();
        assert_eq!(
            surfaces.failed(2).unwrap().1,
            Some(Batch {
                events: vec![],
                begin: false,
                end: Some(Ending::Copy)
            }),
            "the end still reaches the plugin, so what it holds is let go"
        );
        assert!(surfaces.failed(9).is_none());
    }

    #[test]
    fn the_end_goes_with_the_last_input_and_an_idle_surface_is_forgotten() {
        let mut surfaces = Surfaces::default();
        surfaces
            .offer(&key(), vec![motion(1.)], false, None)
            .unwrap();
        surfaces.sent(&key(), 1);
        surfaces
            .offer(&key(), vec![press(false)], false, Some(Ending::Copy))
            .unwrap();
        let (_, next) = surfaces.answered(1).unwrap();
        assert_eq!(
            next,
            Some(Batch {
                events: vec![press(false)],
                begin: false,
                end: Some(Ending::Copy)
            })
        );
        surfaces.sent(&key(), 2);
        let (_, after) = surfaces.answered(2).unwrap();
        assert_eq!(after, None);
        assert!(surfaces.queues.is_empty());
        // An end alone, with nothing else to say, still reaches the plugin so it closes what it holds.
        assert_eq!(
            surfaces
                .offer(&key(), vec![], false, Some(Ending::Copy))
                .unwrap(),
            Some(Batch {
                events: vec![],
                begin: false,
                end: Some(Ending::Copy)
            })
        );
    }

    #[test]
    fn a_plugin_that_never_answers_fills_the_queue_and_more_is_refused_not_kept() {
        let mut surfaces = Surfaces::default();
        surfaces
            .offer(&key(), vec![press(true)], false, None)
            .unwrap();
        surfaces.sent(&key(), 1);
        let mut refused = false;
        for i in 0..(MOST_WAITING + 1) {
            if surfaces
                .offer(&key(), vec![press(i % 2 == 0)], false, None)
                .is_err()
            {
                refused = true;
            }
        }
        assert!(refused);
        surfaces.forget(&PluginId::new("control").unwrap());
        assert!(!surfaces.owns(1));
        assert!(surfaces.queues.is_empty());
    }

    #[test]
    fn withdrawal_discards_queued_input_without_touching_other_destinations() {
        let mut surfaces = Surfaces::default();
        let first = key();
        let mut second = key();
        second.device = Some(PeerId("other".into()));
        for (key, id) in [(&first, 7), (&second, 8)] {
            surfaces.offer(key, vec![motion(1.)], false, None).unwrap();
            surfaces.sent(key, id);
            surfaces.offer(key, vec![press(true)], false, None).unwrap();
        }
        assert_eq!(
            surfaces.forget_device(Some(&first.plugin), first.device.as_ref().unwrap()),
            vec![7]
        );
        assert!(surfaces.answered(7).is_none());
        assert_eq!(
            surfaces.answered(8).unwrap().1.unwrap().events,
            vec![press(true)]
        );
    }

    #[test]
    fn the_next_visit_begins_after_the_old_end_even_while_a_call_is_out() {
        let mut surfaces = Surfaces::default();
        let key = key();
        let first = surfaces
            .offer(&key, vec![motion(1.)], true, None)
            .unwrap()
            .unwrap();
        assert!(first.begin && first.end.is_none());
        surfaces.sent(&key, 1);
        surfaces
            .offer(&key, vec![press(false)], false, Some(Ending::Copy))
            .unwrap();
        surfaces.offer(&key, vec![motion(3.)], true, None).unwrap();
        surfaces
            .offer(&key, Vec::new(), false, Some(Ending::Copy))
            .unwrap();
        assert_eq!(
            surfaces.answered(1).unwrap().1,
            Some(Batch {
                events: vec![press(false)],
                begin: false,
                end: Some(Ending::Copy),
            })
        );
        surfaces.sent(&key, 2);
        assert_eq!(
            surfaces.answered(2).unwrap().1,
            Some(Batch {
                events: vec![motion(3.)],
                begin: true,
                end: None,
            })
        );
        surfaces.sent(&key, 3);
        assert_eq!(
            surfaces.answered(3).unwrap().1,
            Some(Batch {
                events: Vec::new(),
                begin: false,
                end: Some(Ending::Copy),
            })
        );
    }

    #[test]
    fn a_pending_copy_end_is_upgraded_to_a_visit_end() {
        let mut surfaces = Surfaces::default();
        let key = key();
        surfaces.offer(&key, vec![motion(1.)], true, None).unwrap();
        surfaces.sent(&key, 1);
        for end in [Ending::Copy, Ending::Visit, Ending::Copy] {
            assert!(
                surfaces
                    .offer(&key, Vec::new(), false, Some(end))
                    .unwrap()
                    .is_none()
            );
        }
        let (_, ending) = surfaces.answered(1).unwrap();
        assert_eq!(ending.unwrap().end, Some(Ending::Visit));
        surfaces.sent(&key, 2);
        assert_eq!(surfaces.answered(2).unwrap().1, None);
    }
}
