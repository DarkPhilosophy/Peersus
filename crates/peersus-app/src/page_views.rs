//! What extensions show on their own pages, and the events a person's actions there become.
//!
//! A page's events go to its extension one at a time: the next leaves only when the extension has
//! answered the last, so a slow or stuck extension holds back its own page and nothing else. Behind
//! the one out, a few wait; more than that, or more in one second than a person can do, is refused.
//! What the extension answers is checked before it is kept: a tree that is not one the frontends
//! draw is dropped, and the page keeps what it showed. Everything of an extension goes with it.
use peersus_api::elements::{Event, MAX_BYTES, Node, PointerPhase};
use peersus_core::{model::PageView, plugins::PluginId};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    time::{Duration, Instant},
};

/// Events that may wait behind the one out, for one page.
pub const MOST_WAITING: usize = 8;
/// Events one page may send in one second.
pub const MOST_PER_SECOND: usize = 30;
/// Continuous raw samples have their own budget; ordinary actions keep their existing limit.
pub const MOST_SAMPLES_PER_SECOND: usize = 240;
/// Continuous samples leave no closer together than this: a drag is drawn at the frame cadence,
/// not at the rate the extension can answer, which on a fast one is every sample a mouse makes.
/// A sample held back is superseded by the next, or sent when this has passed (see [`PageViews::due`]).
pub const PACE: Duration = Duration::from_millis(16);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PageKey {
    pub plugin: PluginId,
    pub page: String,
}

#[derive(Default)]
struct Queue {
    out: Option<u64>,
    waiting: VecDeque<Event>,
    recent: VecDeque<Instant>,
    samples: VecDeque<Instant>,
    /// When a continuous sample last left: see [`PACE`].
    paced_at: Option<Instant>,
}

#[derive(Default)]
pub struct PageViews {
    views: BTreeMap<PageKey, Node>,
    queues: HashMap<PageKey, Queue>,
    calls: HashMap<u64, PageKey>,
}

impl PageViews {
    /// Take an event for a page. `Some` is the event to send now; `None`, that it waits behind the
    /// one out. Refused when the page has sent too much.
    pub fn offer(
        &mut self,
        key: &PageKey,
        event: Event,
        now: Instant,
    ) -> Result<Option<Event>, &'static str> {
        const TOO_MUCH: &str = "That page is being used too fast";
        let queue = self.queues.entry(key.clone()).or_default();
        // Only adjacent absolute samples may be replaced. Never merge across down/up/cancel:
        // those boundaries belong to the extension's gesture state machine.
        if let Some(last) = queue.waiting.back_mut() {
            // Wheel steps are relative: queued ones of the same element are summed, which loses
            // nothing, so a fast touchpad cannot fill the queue while the page is still answering.
            if let (
                Event::Wheel { id: a, x, y, dx, dy, at },
                Event::Wheel { id: b, x: nx, y: ny, dx: ndx, dy: ndy, at: nat },
            ) = (&mut *last, &event)
                && a == b
            {
                (*x, *y, *at) = (*nx, *ny, *nat);
                *dx = dx.saturating_add(*ndx);
                *dy = dy.saturating_add(*ndy);
                return Ok(None);
            }
            let same_sample = match (&*last, &event) {
                (Event::Pointer { id: a, phase: PointerPhase::Move, .. },
                 Event::Pointer { id: b, phase: PointerPhase::Move, .. }) => a == b,
                (Event::Tick { id: a, .. }, Event::Tick { id: b, .. }) => a == b,
                (Event::Size { id: a, .. }, Event::Size { id: b, .. }) => a == b,
                _ => false,
            };
            if same_sample {
                *last = event;
                return Ok(None);
            }
        }
        let continuous = matches!(event, Event::Pointer { phase: PointerPhase::Move, .. }
            | Event::Tick { .. } | Event::Size { .. } | Event::Wheel { .. });
        let (recent, limit) = if continuous {
            (&mut queue.samples, MOST_SAMPLES_PER_SECOND)
        } else {
            (&mut queue.recent, MOST_PER_SECOND)
        };
        while recent.front().is_some_and(|at| now.duration_since(*at) >= Duration::from_secs(1)) {
            recent.pop_front();
        }
        if recent.len() >= limit {
            // A sample is superseded by the next one: one too many is dropped, not reported.
            return if continuous { Ok(None) } else { Err(TOO_MUCH) };
        }
        let paced = continuous && queue.paced_at.is_some_and(|at| now.duration_since(at) < PACE);
        if queue.out.is_none() && !paced {
            recent.push_back(now);
            if continuous {
                queue.paced_at = Some(now);
            }
            // Nothing out, so nothing held back, unless a paced sample waits its turn: it leaves
            // first and this one waits, so that a press never passes the motion before it.
            return Ok(Some(match queue.waiting.pop_front() {
                Some(earlier) => {
                    queue.waiting.push_back(event);
                    earlier
                }
                None => event,
            }));
        }
        // Opening again while waiting says nothing new: the answer to what is out is the page as it is.
        if event == Event::Open && queue.waiting.contains(&Event::Open) {
            return Ok(None);
        }
        if queue.waiting.len() >= MOST_WAITING {
            return if continuous { Ok(None) } else { Err(TOO_MUCH) };
        }
        recent.push_back(now);
        queue.waiting.push_back(event);
        Ok(None)
    }

    pub fn sent(&mut self, key: &PageKey, id: u64) {
        self.queues.entry(key.clone()).or_default().out = Some(id);
        self.calls.insert(id, key.clone());
    }

    /// How long until a sample held back for pacing may leave, if one waits with nothing out.
    pub fn due(&self, now: Instant) -> Option<Duration> {
        self.queues
            .values()
            .filter(|queue| queue.out.is_none() && !queue.waiting.is_empty())
            .map(|queue| {
                queue.paced_at.map_or(Duration::ZERO, |at| PACE.saturating_sub(now.duration_since(at)))
            })
            .min()
    }

    /// The held-back events whose time has come, one per page, to be sent now.
    pub fn flush(&mut self, now: Instant) -> Vec<(PageKey, Event)> {
        let mut ready = Vec::new();
        for (key, queue) in &mut self.queues {
            if queue.out.is_none()
                && queue.paced_at.is_none_or(|at| now.duration_since(at) >= PACE)
                && let Some(event) = queue.waiting.pop_front()
            {
                queue.paced_at = Some(now);
                ready.push((key.clone(), event));
            }
        }
        ready
    }

    /// A send that never left: the page may send again.
    pub fn unsent(&mut self, key: &PageKey) {
        if let Some(queue) = self.queues.get_mut(key)
            && let Some(id) = queue.out.take()
        {
            self.calls.remove(&id);
        }
    }

    pub fn owns(&self, id: u64) -> bool {
        self.calls.contains_key(&id)
    }

    /// The extension answered call `id` with `value` (`None` when it failed): what it shows is kept if it
    /// is a tree the frontends draw. Returns the page and the next event waiting for it.
    pub fn answered(
        &mut self,
        id: u64,
        value: Option<&serde_json::Value>,
    ) -> Option<(PageKey, Option<Event>, bool)> {
        let key = self.calls.remove(&id)?;
        let mut changed = false;
        if let Some(value) = value
            && serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() <= MAX_BYTES)
            && let Ok(node) = Node::parse(value)
            && self.views.get(&key) != Some(&node)
        {
            self.views.insert(key.clone(), node);
            changed = true;
        }
        let queue = self.queues.get_mut(&key)?;
        queue.out = None;
        let next = queue.waiting.pop_front();
        Some((key, next, changed))
    }

    /// Resolve identity resources in the owner without exposing stable ids to the guest.
    pub fn resolve_devices(&mut self, key: &PageKey, resolve: impl Fn(&str) -> Option<String>) {
        fn walk(node: &mut Node, resolve: &impl Fn(&str) -> Option<String>) {
            match node {
                Node::Avatar { device, .. } => {
                    if let Some(actual) = resolve(device) { *device = actual; }
                }
                Node::Box { children, .. } | Node::Column { children } | Node::Row { children } => {
                    for child in children { walk(child, resolve); }
                }
                Node::Interval { child, .. } => walk(child, resolve),
                _ => {},
            }
        }
        if let Some(node) = self.views.get_mut(key) { walk(node, &resolve); }
    }

    /// Withdraw cached views, queued events and outstanding calls of removed additional pages.
    pub(crate) fn forget_pages(&mut self, plugin: &PluginId, pages: &[String]) -> Vec<u64> {
        let removed = |key: &PageKey| key.plugin == *plugin && pages.contains(&key.page);
        self.views.retain(|key, _| !removed(key));
        self.queues.retain(|key, _| !removed(key));
        let mut cancelled = Vec::new();
        self.calls.retain(|id, key| {
            if removed(key) {
                cancelled.push(*id);
                false
            } else {
                true
            }
        });
        cancelled
    }

    /// Everything of an extension that stopped or went away.
    pub fn forget(&mut self, plugin: &PluginId) {
        self.views.retain(|key, _| key.plugin != *plugin);
        self.queues.retain(|key, _| key.plugin != *plugin);
        self.calls.retain(|_, key| key.plugin != *plugin);
    }

    pub fn snapshot(&self) -> Vec<PageView> {
        self.views
            .iter()
            .map(|(key, view)| PageView {
                plugin: key.plugin.clone(),
                page: key.page.clone(),
                view: view.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn key() -> PageKey {
        PageKey {
            plugin: PluginId::new("test").unwrap(),
            page: "main".into(),
        }
    }

    fn press(id: &str) -> Event {
        Event::Press { id: id.into() }
    }

    #[test]
    fn withdrawing_one_pages_offer_cancels_its_pending_input_and_discards_late_answers() {
        let mut pages = PageViews::default();
        let now = Instant::now();
        let offered = key();
        let original = PageKey { plugin: offered.plugin.clone(), page: "original".into() };
        let other = PageKey { plugin: PluginId::new("other").unwrap(), page: offered.page.clone() };
        for (id, key) in [(1, &offered), (2, &original), (3, &other)] {
            pages.offer(key, Event::Open, now).unwrap();
            pages.sent(key, id);
            pages.answered(id, Some(&json!({"k":"text","text":"Page"}))).unwrap();
        }
        pages.offer(&offered, press("pending"), now).unwrap();
        pages.sent(&offered, 4);
        pages.offer(&offered, press("queued"), now).unwrap();
        assert_eq!(pages.forget_pages(&offered.plugin, &[offered.page.clone()]), [4]);
        assert!(pages.answered(4, Some(&json!({"k":"text","text":"Too late"}))).is_none());
        let remaining = pages.snapshot();
        assert_eq!(remaining.len(), 2);
        assert!(remaining.iter().any(|view| view.plugin == original.plugin && view.page == original.page));
        assert!(remaining.iter().any(|view| view.plugin == other.plugin && view.page == other.page));
        assert_eq!(pages.offer(&offered, Event::Open, now).unwrap(), Some(Event::Open));
    }

    #[test]
    fn one_event_is_out_at_a_time_and_the_rest_wait_in_order() {
        let mut pages = PageViews::default();
        let now = Instant::now();
        assert_eq!(
            pages.offer(&key(), Event::Open, now).unwrap(),
            Some(Event::Open)
        );
        pages.sent(&key(), 1);
        assert_eq!(pages.offer(&key(), press("a"), now).unwrap(), None);
        assert_eq!(pages.offer(&key(), press("b"), now).unwrap(), None);
        let tree = json!({"k":"text","text":"Hello"});
        let (_, next, changed) = pages.answered(1, Some(&tree)).unwrap();
        assert_eq!((next, changed), (Some(press("a")), true));
        assert_eq!(pages.snapshot()[0].view, Node::parse(&tree).unwrap());
        pages.sent(&key(), 2);
        // A failed call, or an answer no frontend draws, leaves what the page showed.
        let (_, next, changed) = pages.answered(2, Some(&json!({"k":"canvas"}))).unwrap();
        assert_eq!((next, changed), (Some(press("b")), false));
        assert_eq!(pages.snapshot().len(), 1);
    }

    #[test]
    fn a_page_used_too_fast_is_refused_and_everything_goes_with_its_extension() {
        let mut pages = PageViews::default();
        let now = Instant::now();
        pages.offer(&key(), Event::Open, now).unwrap();
        pages.sent(&key(), 1);
        for i in 0..MOST_WAITING {
            assert!(pages.offer(&key(), press(&format!("b{i}")), now).is_ok());
        }
        assert!(pages.offer(&key(), press("more"), now).is_err());
        // A second later the window of one second has passed, but the queue is still full.
        assert!(
            pages
                .offer(&key(), press("more"), now + Duration::from_secs(2))
                .is_err()
        );
        pages.forget(&key().plugin);
        assert!(!pages.owns(1));
        assert!(pages.snapshot().is_empty());
        assert_eq!(
            pages.offer(&key(), Event::Open, now).unwrap(),
            Some(Event::Open)
        );
    }

    #[test]
    fn more_than_a_person_can_do_in_a_second_is_refused() {
        let mut pages = PageViews::default();
        let now = Instant::now();
        for i in 0..MOST_PER_SECOND {
            let at = now + Duration::from_millis(i as u64);
            let event = pages.offer(&key(), press("go"), at).unwrap();
            assert!(event.is_some());
            pages.sent(&key(), i as u64 + 1);
            pages.answered(i as u64 + 1, None);
        }
        assert!(
            pages
                .offer(&key(), press("go"), now + Duration::from_millis(100))
                .is_err()
        );
        assert!(
            pages
                .offer(&key(), press("go"), now + Duration::from_millis(1_100))
                .is_ok()
        );
    }

    #[test]
    fn absolute_samples_coalesce_without_losing_gesture_boundaries() {
        let mut pages = PageViews::default();
        let now = Instant::now();
        let point = |phase, x| Event::Pointer { id: "area".into(), phase, x, y: 0, at: x as u64 };
        let down = point(PointerPhase::Down, 0);
        assert_eq!(pages.offer(&key(), down.clone(), now).unwrap(), Some(down));
        pages.sent(&key(), 1);
        for x in 1..=120 {
            assert_eq!(pages.offer(&key(), point(PointerPhase::Move, x), now).unwrap(), None);
        }
        let up = point(PointerPhase::Up, 120);
        pages.offer(&key(), up.clone(), now).unwrap();
        let (_, next, changed) = pages.answered(1, Some(&serde_json::Value::Null)).unwrap();
        assert_eq!(next, Some(point(PointerPhase::Move, 120)));
        assert!(!changed);
        pages.sent(&key(), 2);
        assert_eq!(pages.answered(2, Some(&serde_json::Value::Null)).unwrap().1, Some(up));
    }

    #[test]
    fn samples_leave_at_the_frame_cadence_and_a_press_never_passes_the_motion_before_it() {
        let mut pages = PageViews::default();
        let now = Instant::now();
        let point = |phase, x| Event::Pointer { id: "area".into(), phase, x, y: 0, at: x as u64 };
        assert!(pages.offer(&key(), point(PointerPhase::Move, 1), now).unwrap().is_some());
        pages.sent(&key(), 1);
        pages.answered(1, Some(&serde_json::Value::Null));
        // Answered at once, as a fast page does: the next samples are still held to the cadence.
        for x in 2..=5 {
            let at = now + Duration::from_millis(x as u64 * 2);
            assert_eq!(pages.offer(&key(), point(PointerPhase::Move, x), at).unwrap(), None);
        }
        let waited = pages.due(now + Duration::from_millis(10)).unwrap();
        assert!(waited <= Duration::from_millis(6), "{waited:?}");
        assert!(pages.flush(now + Duration::from_millis(10)).is_empty(), "not yet");
        let later = now + PACE;
        assert_eq!(pages.flush(later), vec![(key(), point(PointerPhase::Move, 5))], "the newest sample");
        pages.sent(&key(), 2);
        pages.answered(2, Some(&serde_json::Value::Null));
        // A sample within the cadence waits; the press that follows goes behind it, never before.
        assert_eq!(pages.offer(&key(), point(PointerPhase::Move, 6), later + Duration::from_millis(1)).unwrap(), None);
        let up = point(PointerPhase::Up, 6);
        assert_eq!(pages.offer(&key(), up.clone(), later + Duration::from_millis(2)).unwrap(), Some(point(PointerPhase::Move, 6)));
        pages.sent(&key(), 3);
        assert_eq!(pages.answered(3, Some(&serde_json::Value::Null)).unwrap().1, Some(up));
        assert_eq!(pages.due(later + Duration::from_secs(1)), None);
    }

    #[test]
    fn a_touchpad_wheel_burst_while_the_page_answers_is_summed_not_refused() {
        let mut pages = PageViews::default();
        let now = Instant::now();
        let wheel = |dy, at| Event::Wheel { id: "map".into(), x: 5, y: at as i32, dx: 1, dy, at };
        assert!(pages.offer(&key(), wheel(-3, 0), now).unwrap().is_some());
        pages.sent(&key(), 1);
        // Two hundred steps 8 ms apart while the first is still out: far past the queue's eight.
        for step in 1..=200u64 {
            let at = now + Duration::from_millis(step * 8);
            assert_eq!(pages.offer(&key(), wheel(-3, step), at), Ok(None), "step {step} refused");
        }
        let (_, next, _) = pages.answered(1, Some(&serde_json::Value::Null)).unwrap();
        assert_eq!(
            next,
            Some(Event::Wheel { id: "map".into(), x: 5, y: 200, dx: 200, dy: -600, at: 200 }),
            "every step is in the sum, at the latest position"
        );
        // A different element is not merged into it.
        pages.sent(&key(), 2);
        pages.offer(&key(), wheel(-1, 300), now).unwrap();
        let other = Event::Wheel { id: "list".into(), x: 0, y: 0, dx: 0, dy: 4, at: 301 };
        pages.offer(&key(), other.clone(), now).unwrap();
        let (_, next, _) = pages.answered(2, Some(&serde_json::Value::Null)).unwrap();
        assert_eq!(next, Some(wheel(-1, 300)));
    }

    #[test]
    fn scoped_avatar_resources_are_resolved_inside_nested_containers() {
        let mut pages = PageViews::default();
        pages.offer(&key(), Event::Open, Instant::now()).unwrap();
        pages.sent(&key(), 1);
        let tree = json!({"k":"interval","id":"clock","milliseconds":250,"child":
            {"k":"box","children":[{"k":"avatar","device":"scoped","name":"Computer","size":28}]}});
        let (key, _, _) = pages.answered(1, Some(&tree)).unwrap();
        pages.resolve_devices(&key, |alias| (alias == "scoped").then(|| "owner-id".into()));
        let shown = serde_json::to_value(&pages.snapshot()[0].view).unwrap();
        assert_eq!(shown["child"]["children"][0]["device"], "owner-id");
    }
}
