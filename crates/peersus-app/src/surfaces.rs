//! Input from a page's surface on its way to the plugin, at most one call at a time per surface.
//!
//! A finger moves sixty times a second and a plugin answers when it answers: sending a call for
//! each would queue motion behind motion and the pointer would trail the finger. Instead what
//! arrives while a call is out waits here, consecutive motion is added into one, and the next
//! call takes all of it the moment the last one is answered. Nothing a person pressed or typed
//! is merged or dropped, only how far the pointer went is summed.
use peersus_api::contributions::{MAX_POINTER_BATCH, PointerInput};
use peersus_core::{model::PeerId, plugins::PluginId};
use std::collections::{HashMap, VecDeque};

/// One surface a person is using: a page of a plugin, for one device.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SurfaceKey {
    pub plugin: PluginId,
    pub device: Option<PeerId>,
}

/// Most inputs held while a call is out. Motion is summed, so only presses and text fill it: a
/// person cannot outpace this, a stuck plugin can, and is then refused rather than buffered for ever.
const MOST_WAITING: usize = 4 * MAX_POINTER_BATCH;

#[derive(Default)]
struct Queue {
    /// The call out now, by its number.
    out: Option<u64>,
    waiting: VecDeque<PointerInput>,
    /// The person stopped; said with the last of what waits.
    end: bool,
}

/// What to send now: a batch, and whether it is the last of this use of the surface.
#[derive(Debug, PartialEq)]
pub struct Batch {
    pub events: Vec<PointerInput>,
    pub end: bool,
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
    pub fn offer(
        &mut self,
        key: &SurfaceKey,
        events: Vec<PointerInput>,
        end: bool,
    ) -> Result<Option<Batch>, &'static str> {
        let queue = self.queues.entry(key.clone()).or_default();
        for event in events {
            let full = queue.waiting.len() >= MOST_WAITING;
            match (queue.waiting.back_mut(), &event) {
                (
                    Some(PointerInput::Move { dx, dy }),
                    PointerInput::Move {
                        dx: more_x,
                        dy: more_y,
                    },
                ) => {
                    *dx += more_x;
                    *dy += more_y;
                }
                _ if full => {
                    return Err("The extension is not keeping up with this input");
                }
                _ => queue.waiting.push_back(event),
            }
        }
        queue.end |= end;
        Ok(if queue.out.is_none() {
            Self::next(queue)
        } else {
            None
        })
    }

    /// The batch to send for a queue that has no call out, if it has anything to say.
    fn next(queue: &mut Queue) -> Option<Batch> {
        if queue.waiting.is_empty() && !queue.end {
            return None;
        }
        let take = queue.waiting.len().min(MAX_POINTER_BATCH);
        let events: Vec<_> = queue.waiting.drain(..take).collect();
        // The end goes with the last of what waits, never ahead of input still to come.
        let end = queue.end && queue.waiting.is_empty();
        if end {
            queue.end = false;
        }
        Some(Batch { events, end })
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

    /// Withdraw pending and in-flight input for a device before cancelling its calls.
    pub fn forget_device(&mut self, plugin: Option<&PluginId>, device: &PeerId) -> Vec<u64> {
        let matches = |key: &SurfaceKey| key.device.as_ref() == Some(device)
            && plugin.is_none_or(|plugin| &key.plugin == plugin);
        let ids = self.calls.iter().filter_map(|(id,key)| matches(key).then_some(*id)).collect();
        self.queues.retain(|key,_| !matches(key));
        self.calls.retain(|_,key| !matches(key));
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
        let first = surfaces.offer(&key(), vec![motion(1.)], false).unwrap();
        assert_eq!(
            first,
            Some(Batch {
                events: vec![motion(1.)],
                end: false
            })
        );
        surfaces.sent(&key(), 7);
        for event in [motion(2.), motion(3.), press(true), motion(4.)] {
            assert_eq!(surfaces.offer(&key(), vec![event], false).unwrap(), None);
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
                end: false
            }),
            "motion is summed up to a press, and nothing pressed is merged or dropped"
        );
    }

    #[test]
    fn the_end_goes_with_the_last_input_and_an_idle_surface_is_forgotten() {
        let mut surfaces = Surfaces::default();
        surfaces.offer(&key(), vec![motion(1.)], false).unwrap();
        surfaces.sent(&key(), 1);
        surfaces.offer(&key(), vec![press(false)], true).unwrap();
        let (_, next) = surfaces.answered(1).unwrap();
        assert_eq!(
            next,
            Some(Batch {
                events: vec![press(false)],
                end: true
            })
        );
        surfaces.sent(&key(), 2);
        let (_, after) = surfaces.answered(2).unwrap();
        assert_eq!(after, None);
        assert!(surfaces.queues.is_empty());
        // An end alone, with nothing else to say, still reaches the plugin so it closes what it holds.
        assert_eq!(
            surfaces.offer(&key(), vec![], true).unwrap(),
            Some(Batch {
                events: vec![],
                end: true
            })
        );
    }

    #[test]
    fn a_plugin_that_never_answers_fills_the_queue_and_more_is_refused_not_kept() {
        let mut surfaces = Surfaces::default();
        surfaces.offer(&key(), vec![press(true)], false).unwrap();
        surfaces.sent(&key(), 1);
        let mut refused = false;
        for i in 0..(MOST_WAITING + 1) {
            if surfaces
                .offer(&key(), vec![press(i % 2 == 0)], false)
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
        let mut surfaces=Surfaces::default();
        let first=key();let mut second=key();second.device=Some(PeerId("other".into()));
        for (key,id) in [(&first,7),(&second,8)] {
            surfaces.offer(key,vec![motion(1.)],false).unwrap();
            surfaces.sent(key,id);
            surfaces.offer(key,vec![press(true)],false).unwrap();
        }
        assert_eq!(surfaces.forget_device(Some(&first.plugin),first.device.as_ref().unwrap()),vec![7]);
        assert!(surfaces.answered(7).is_none());
        assert_eq!(surfaces.answered(8).unwrap().1.unwrap().events,vec![press(true)]);
    }
}
