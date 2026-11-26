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
        if let Event::Visibility { id, .. } = &event {
            // In particular, a hide/focus loss supersedes an unsent positive observation.
            queue
                .waiting
                .retain(|sample| !matches!(sample, Event::Visibility { id: old, .. } if old == id));
        }
        // Only adjacent absolute samples may be replaced. Never merge across down/up/cancel:
        // those boundaries belong to the extension's gesture state machine.
        if let Some(last) = queue.waiting.back_mut() {
            // Relative wheel samples can be summed only with a common anchor and direction.
            // A moved anchor changes the transform, and a reversal may start at a zoom bound.
            if let (
                Event::Wheel {
                    id: a,
                    x,
                    y,
                    dx,
                    dy,
                    at,
                },
                Event::Wheel {
                    id: b,
                    x: nx,
                    y: ny,
                    dx: ndx,
                    dy: ndy,
                    at: nat,
                },
            ) = (&mut *last, &event)
                && a == b
                && x == nx
                && y == ny
                && dy.signum() == ndy.signum()
            {
                *at = *nat;
                *dx = dx.saturating_add(*ndx);
                *dy = dy.saturating_add(*ndy);
                return Ok(None);
            }
            let same_sample = match (&*last, &event) {
                (
                    Event::Pointer {
                        id: a,
                        phase: PointerPhase::Move,
                        ..
                    },
                    Event::Pointer {
                        id: b,
                        phase: PointerPhase::Move,
                        ..
                    },
                ) => a == b,
                (
                    Event::Pinch {
                        id: a,
                        begin: false,
                        ..
                    },
                    Event::Pinch {
                        id: b,
                        begin: false,
                        ..
                    },
                ) => a == b,
                (
                    Event::Rotate {
                        id: a,
                        begin: false,
                        ..
                    },
                    Event::Rotate {
                        id: b,
                        begin: false,
                        ..
                    },
                ) => a == b,
                (Event::Tick { id: a, .. }, Event::Tick { id: b, .. }) => a == b,
                (Event::Size { id: a, .. }, Event::Size { id: b, .. }) => a == b,
                (Event::CanvasSize { id: a, .. }, Event::CanvasSize { id: b, .. }) => a == b,
                _ => false,
            };
            if same_sample {
                *last = event;
                return Ok(None);
            }
        }
        let continuous = matches!(
            event,
            Event::Pointer {
                phase: PointerPhase::Move,
                ..
            } | Event::Tick { .. }
                | Event::Size { .. }
                | Event::CanvasSize { .. }
                | Event::Visibility { .. }
                | Event::Pinch { begin: false, .. }
                | Event::Rotate { begin: false, .. }
                | Event::Wheel { .. }
        );
        let (recent, limit) = if continuous {
            (&mut queue.samples, MOST_SAMPLES_PER_SECOND)
        } else {
            (&mut queue.recent, MOST_PER_SECOND)
        };
        while recent
            .front()
            .is_some_and(|at| now.duration_since(*at) >= Duration::from_secs(1))
        {
            recent.pop_front();
        }
        if recent.len() >= limit {
            // A sample is superseded by the next one: one too many is dropped, not reported.
            return if continuous { Ok(None) } else { Err(TOO_MUCH) };
        }
        let paced = continuous
            && queue
                .paced_at
                .is_some_and(|at| now.duration_since(at) < PACE);
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
        // A cached canvas page's Open retires its previous stream. Its next showing
        // is not a duplicate of an earlier Open waiting behind input or a frame.
        if event == Event::Open
            && queue.waiting.contains(&Event::Open)
            && !self
                .views
                .get(key)
                .is_some_and(|node| any_node(node, &mut |node| matches!(node, Node::Canvas { .. })))
        {
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
                queue.paced_at.map_or(Duration::ZERO, |at| {
                    PACE.saturating_sub(now.duration_since(at))
                })
            })
            .min()
    }

    /// The held-back events whose time has come, one per page, to be sent now.
    pub fn flush(&mut self, now: Instant) -> Vec<(PageKey, Event)> {
        let mut ready = Vec::new();
        for (key, queue) in &mut self.queues {
            if queue.out.is_none()
                && queue
                    .paced_at
                    .is_none_or(|at| now.duration_since(at) >= PACE)
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

    pub(crate) fn canvas_ids(&self, key: &PageKey) -> Vec<String> {
        self.views.get(key).map(canvas_ids).unwrap_or_default()
    }

    /// The extension answered call `id` with `value` (`None` when it failed): what it shows is kept if it
    /// is a tree the frontends draw. Returns the page and the next event waiting for it.
    pub(crate) fn answered(
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

    /// Resolve identity resources in the owner without exposing stable ids to the guest: an avatar's device and a chooser's chosen
    /// device become the owner's key, and a chosen device the extension may no longer name is no longer chosen.
    pub fn resolve_devices(&mut self, key: &PageKey, resolve: impl Fn(&str) -> Option<String>) {
        if let Some(node) = self.views.get_mut(key) {
            each_node(node, &mut |node| match node {
                Node::Avatar { device, .. } => {
                    if let Some(actual) = resolve(device) {
                        *device = actual;
                    }
                }
                Node::Devices { chosen, .. } => *chosen = chosen.as_deref().and_then(&resolve),
                _ => {}
            });
        }
    }

    /// Fill in the devices of every chooser of every page, of `plugin` only when given, with what `fill` says for that page's
    /// extension and that chooser's `pick` and `show`. Returns whether any page changed, so only then are the windows told.
    pub fn fill_devices(
        &mut self,
        plugin: Option<&PluginId>,
        mut fill: impl FnMut(
            &PluginId,
            &peersus_api::elements::Pick,
            &peersus_api::elements::Show,
        ) -> Vec<peersus_api::elements::DeviceEntry>,
    ) -> bool {
        let mut changed = false;
        for (key, node) in &mut self.views {
            if plugin.is_some_and(|plugin| *plugin != key.plugin) {
                continue;
            }
            each_node(node, &mut |node| {
                if let Node::Devices {
                    pick,
                    show,
                    entries,
                    ..
                } = node
                {
                    let filled = fill(&key.plugin, pick, show);
                    if *entries != filled {
                        *entries = filled;
                        changed = true;
                    }
                }
            });
        }
        changed
    }

    /// Write into every picture placed on the pages of `plugin` (of every extension with none) the digest of the picture that
    /// page's extension keeps under that id, or none. Returns whether any page changed, so only then are the windows told.
    pub fn fill_pictures(
        &mut self,
        plugin: Option<&PluginId>,
        digest: impl Fn(&PluginId, &str) -> Option<String>,
    ) -> bool {
        let mut changed = false;
        for (key, node) in &mut self.views {
            if plugin.is_some_and(|plugin| *plugin != key.plugin) {
                continue;
            }
            each_node(node, &mut |node| {
                if let Node::Picture {
                    id, digest: shown, ..
                } = node
                {
                    let now = digest(&key.plugin, id);
                    if *shown != now {
                        *shown = now;
                        changed = true;
                    }
                }
            });
        }
        changed
    }

    pub fn observes_visibility(&self, key: &PageKey, id: &str) -> bool {
        self.views.get(key).is_some_and(|root| {
            any_node(root, &mut |node| {
                matches!(node, Node::Box {id:Some(found), observe, ..}
                if found == id && observe.contains(&peersus_api::elements::Observe::Visibility))
            })
        })
    }

    /// Missing chooser means the window's view is stale; a chooser that still exists
    /// can explicitly refuse this device. The owner's device key never reaches the guest.
    pub fn choosable(&self, key: &PageKey, id: &str, device: Option<&str>) -> Option<bool> {
        let mut found = false;
        let allowed = any_node(self.views.get(key)?, &mut |node| {
            let Node::Devices {
                id: chooser,
                entries,
                this_device,
                ..
            } = node
            else {
                return false;
            };
            if chooser != id {
                return false;
            }
            found = true;
            match device {
                None => *this_device,
                Some(device) => entries
                    .iter()
                    .any(|entry| entry.device == device && entry.refused.is_none()),
            }
        });
        found.then_some(allowed)
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

/// Every element of `node`, itself first, each one changeable.
fn each_node(node: &mut Node, visit: &mut impl FnMut(&mut Node)) {
    visit(node);
    match node {
        Node::Box { children, .. } | Node::Column { children } | Node::Row { children } => {
            for child in children {
                each_node(child, visit);
            }
        }
        Node::Interval { child, .. } => each_node(child, visit),
        _ => {}
    }
}

/// Whether any element of `node`, itself included, is one `test` holds for.
fn any_node(node: &Node, test: &mut impl FnMut(&Node) -> bool) -> bool {
    test(node)
        || match node {
            Node::Box { children, .. }
            | Node::Column { children, .. }
            | Node::Row { children, .. } => children.iter().any(|child| any_node(child, test)),
            Node::Interval { child, .. } => any_node(child, test),
            _ => false,
        }
}

fn canvas_ids(node: &Node) -> Vec<String> {
    let mut ids = Vec::new();
    fn collect(node: &Node, ids: &mut Vec<String>) {
        match node {
            Node::Canvas { id, .. } => ids.push(id.clone()),
            Node::Box { children, .. } | Node::Column { children } | Node::Row { children } => {
                for child in children {
                    collect(child, ids);
                }
            }
            Node::Interval { child, .. } => collect(child, ids),
            _ => {}
        }
    }
    collect(node, &mut ids);
    ids
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
    fn hiding_an_observed_box_supersedes_a_pending_foreground_notice() {
        let mut pages = PageViews::default();
        let key = key();
        pages.sent(&key, 1);
        pages
            .answered(
                1,
                Some(&json!({"k":"box","id":"row","observe":["visibility"]})),
            )
            .unwrap();
        assert!(pages.observes_visibility(&key, "row"));
        assert!(!pages.observes_visibility(&key, "other"));
        pages.sent(&key, 2);
        let now = Instant::now();
        let visible = Event::Visibility {
            id: "row".into(),
            visible: true,
            foreground: true,
        };
        let hidden = Event::Visibility {
            id: "row".into(),
            visible: false,
            foreground: false,
        };
        assert_eq!(pages.offer(&key, visible, now), Ok(None));
        assert_eq!(pages.offer(&key, hidden.clone(), now), Ok(None));
        assert_eq!(pages.answered(2, None).unwrap().1, Some(hidden));
    }

    #[test]
    fn canvas_ids_follow_the_current_page_declaration() {
        let mut pages = PageViews::default();
        let key = key();
        let canvas = json!({"k":"canvas","id":"map"});
        pages.sent(&key, 1);
        assert!(pages.answered(1, Some(&canvas)).unwrap().2);
        assert_eq!(pages.canvas_ids(&key), ["map"]);
        pages.sent(&key, 2);
        let text = json!({"k":"text","text":"No canvas"});
        assert!(pages.answered(2, Some(&text)).unwrap().2);
        assert!(pages.canvas_ids(&key).is_empty());
    }

    #[test]
    fn canvas_reopens_are_not_coalesced_behind_a_pending_event() {
        let mut pages = PageViews::default();
        let key = key();
        let now = Instant::now();
        pages.sent(&key, 1);
        pages
            .answered(1, Some(&json!({"k":"canvas","id":"map"})))
            .unwrap();
        pages.sent(&key, 2);
        for _ in 0..2 {
            assert_eq!(pages.offer(&key, Event::Open, now), Ok(None));
        }
        assert_eq!(pages.answered(2, None).unwrap().1, Some(Event::Open));
        pages.sent(&key, 3);
        assert_eq!(pages.answered(3, None).unwrap().1, Some(Event::Open));
    }

    #[test]
    fn successive_canvas_sizes_are_paced_and_coalesced() {
        let mut pages = PageViews::default();
        let key = key();
        let now = Instant::now();
        let size = |width| Event::CanvasSize {
            id: "map".into(),
            x: 0,
            y: 0,
            width,
            height: 10_000,
            scale: 1500,
            palette: Default::default(),
        };
        pages.sent(&key, 1);
        for n in 1..=1_000 {
            assert_eq!(pages.offer(&key, size(n), now), Ok(None));
        }
        assert_eq!(pages.answered(1, None).unwrap().1, Some(size(1_000)));
    }

    #[test]
    fn withdrawing_one_pages_offer_cancels_its_pending_input_and_discards_late_answers() {
        let mut pages = PageViews::default();
        let now = Instant::now();
        let offered = key();
        let original = PageKey {
            plugin: offered.plugin.clone(),
            page: "original".into(),
        };
        let other = PageKey {
            plugin: PluginId::new("other").unwrap(),
            page: offered.page.clone(),
        };
        for (id, key) in [(1, &offered), (2, &original), (3, &other)] {
            pages.offer(key, Event::Open, now).unwrap();
            pages.sent(key, id);
            pages
                .answered(id, Some(&json!({"k":"text","text":"Page"})))
                .unwrap();
        }
        pages.offer(&offered, press("pending"), now).unwrap();
        pages.sent(&offered, 4);
        pages.offer(&offered, press("queued"), now).unwrap();
        assert_eq!(
            pages.forget_pages(&offered.plugin, std::slice::from_ref(&offered.page)),
            [4]
        );
        assert!(
            pages
                .answered(4, Some(&json!({"k":"text","text":"Too late"})))
                .is_none()
        );
        let remaining = pages.snapshot();
        assert_eq!(remaining.len(), 2);
        assert!(
            remaining
                .iter()
                .any(|view| view.plugin == original.plugin && view.page == original.page)
        );
        assert!(
            remaining
                .iter()
                .any(|view| view.plugin == other.plugin && view.page == other.page)
        );
        assert_eq!(
            pages.offer(&offered, Event::Open, now).unwrap(),
            Some(Event::Open)
        );
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
        let (_, next, changed) = pages
            .answered(2, Some(&json!({"k":"unknown-widget"})))
            .unwrap();
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
        let point = |phase, x| Event::Pointer {
            id: "area".into(),
            phase,
            x,
            y: 0,
            at: x as u64,
        };
        let down = point(PointerPhase::Down, 0);
        assert_eq!(pages.offer(&key(), down.clone(), now).unwrap(), Some(down));
        pages.sent(&key(), 1);
        for x in 1..=120 {
            assert_eq!(
                pages
                    .offer(&key(), point(PointerPhase::Move, x), now)
                    .unwrap(),
                None
            );
        }
        let up = point(PointerPhase::Up, 120);
        pages.offer(&key(), up.clone(), now).unwrap();
        let (_, next, changed) = pages.answered(1, Some(&serde_json::Value::Null)).unwrap();
        assert_eq!(next, Some(point(PointerPhase::Move, 120)));
        assert!(!changed);
        pages.sent(&key(), 2);
        assert_eq!(
            pages.answered(2, Some(&serde_json::Value::Null)).unwrap().1,
            Some(up)
        );
    }

    #[test]
    fn samples_leave_at_the_frame_cadence_and_a_press_never_passes_the_motion_before_it() {
        let mut pages = PageViews::default();
        let now = Instant::now();
        let point = |phase, x| Event::Pointer {
            id: "area".into(),
            phase,
            x,
            y: 0,
            at: x as u64,
        };
        assert!(
            pages
                .offer(&key(), point(PointerPhase::Move, 1), now)
                .unwrap()
                .is_some()
        );
        pages.sent(&key(), 1);
        pages.answered(1, Some(&serde_json::Value::Null));
        // Answered at once, as a fast page does: the next samples are still held to the cadence.
        for x in 2..=5 {
            let at = now + Duration::from_millis(x as u64 * 2);
            assert_eq!(
                pages
                    .offer(&key(), point(PointerPhase::Move, x), at)
                    .unwrap(),
                None
            );
        }
        let waited = pages.due(now + Duration::from_millis(10)).unwrap();
        assert!(waited <= Duration::from_millis(6), "{waited:?}");
        assert!(
            pages.flush(now + Duration::from_millis(10)).is_empty(),
            "not yet"
        );
        let later = now + PACE;
        assert_eq!(
            pages.flush(later),
            vec![(key(), point(PointerPhase::Move, 5))],
            "the newest sample"
        );
        pages.sent(&key(), 2);
        pages.answered(2, Some(&serde_json::Value::Null));
        // A sample within the cadence waits; the press that follows goes behind it, never before.
        assert_eq!(
            pages
                .offer(
                    &key(),
                    point(PointerPhase::Move, 6),
                    later + Duration::from_millis(1)
                )
                .unwrap(),
            None
        );
        let up = point(PointerPhase::Up, 6);
        assert_eq!(
            pages
                .offer(&key(), up.clone(), later + Duration::from_millis(2))
                .unwrap(),
            Some(point(PointerPhase::Move, 6))
        );
        pages.sent(&key(), 3);
        assert_eq!(
            pages.answered(3, Some(&serde_json::Value::Null)).unwrap().1,
            Some(up)
        );
        assert_eq!(pages.due(later + Duration::from_secs(1)), None);
    }

    #[test]
    fn pinch_samples_keep_the_latest_scale_and_time_without_crossing_a_new_gesture() {
        let mut pages = PageViews::default();
        let now = Instant::now();
        // The time of a sample is its scale here, so that a merged sample says which one it is.
        let pinch = |begin, scale| Event::Pinch {
            id: "map".into(),
            x: 20,
            y: 30,
            begin,
            scale,
            at: u64::from(scale),
        };
        assert_eq!(
            pages.offer(&key(), pinch(true, 10000), now),
            Ok(Some(pinch(true, 10000)))
        );
        pages.sent(&key(), 1);
        for scale in 10001..10200 {
            assert_eq!(pages.offer(&key(), pinch(false, scale), now), Ok(None));
        }
        pages.offer(&key(), pinch(true, 10000), now).unwrap();
        pages.offer(&key(), pinch(false, 8000), now).unwrap();
        for (ticket, expected) in [
            (1, pinch(false, 10199)),
            (2, pinch(true, 10000)),
            (3, pinch(false, 8000)),
        ] {
            assert_eq!(
                pages
                    .answered(ticket, Some(&serde_json::Value::Null))
                    .unwrap()
                    .1,
                Some(expected)
            );
            pages.sent(&key(), ticket + 1);
        }
    }

    #[test]
    fn a_touchpad_wheel_burst_while_the_page_answers_is_summed_not_refused() {
        let mut pages = PageViews::default();
        let now = Instant::now();
        let wheel = |dy, at| Event::Wheel {
            id: "map".into(),
            x: 5,
            y: 5,
            dx: 1,
            dy,
            at,
        };
        assert!(pages.offer(&key(), wheel(-3, 0), now).unwrap().is_some());
        pages.sent(&key(), 1);
        // Two hundred steps 8 ms apart while the first is still out: far past the queue's eight.
        for step in 1..=200u64 {
            let at = now + Duration::from_millis(step * 8);
            assert_eq!(
                pages.offer(&key(), wheel(-3, step), at),
                Ok(None),
                "step {step} refused"
            );
        }
        let (_, next, _) = pages.answered(1, Some(&serde_json::Value::Null)).unwrap();
        assert_eq!(
            next,
            Some(Event::Wheel {
                id: "map".into(),
                x: 5,
                y: 5,
                dx: 200,
                dy: -600,
                at: 200
            }),
            "every step at the common anchor is in the sum"
        );
        // A different element is not merged into it.
        pages.sent(&key(), 2);
        pages.offer(&key(), wheel(-1, 300), now).unwrap();
        let other = Event::Wheel {
            id: "list".into(),
            x: 0,
            y: 0,
            dx: 0,
            dy: 4,
            at: 301,
        };
        pages.offer(&key(), other.clone(), now).unwrap();
        let (_, next, _) = pages.answered(2, Some(&serde_json::Value::Null)).unwrap();
        assert_eq!(next, Some(wheel(-1, 300)));
    }

    #[test]
    fn moved_wheel_anchors_and_direction_reversals_keep_their_order() {
        let mut pages = PageViews::default();
        let now = Instant::now();
        let wheel = |x, dy, at| Event::Wheel {
            id: "map".into(),
            x,
            y: 0,
            dx: 0,
            dy,
            at,
        };
        let input = [
            wheel(10, -100, 1),
            wheel(10, -100, 2),
            wheel(20, -100, 3),
            wheel(20, 100, 4),
        ];
        assert_eq!(
            pages.offer(&key(), input[0].clone(), now),
            Ok(Some(input[0].clone()))
        );
        pages.sent(&key(), 1);
        for event in &input[1..] {
            assert_eq!(pages.offer(&key(), event.clone(), now), Ok(None));
        }
        for (id, event) in [2, 3, 4].into_iter().zip(&input[1..]) {
            assert_eq!(
                pages.answered(id - 1, None).unwrap().1.as_ref(),
                Some(event)
            );
            pages.sent(&key(), id);
        }
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

    #[test]
    fn a_chooser_is_filled_by_the_owner_and_only_an_offered_device_can_be_chosen_on_it() {
        use peersus_api::elements::{DeviceEntry, Presence, Refusal};
        let mut pages = PageViews::default();
        pages.offer(&key(), Event::Open, Instant::now()).unwrap();
        pages.sent(&key(), 1);
        let tree = json!({"k":"box","children":[{"k":"devices","id":"target","chosen":"scoped"},
            {"k":"devices","id":"gone","chosen":"forgotten","this_device":true}]});
        let (key, _, _) = pages.answered(1, Some(&tree)).unwrap();
        pages.resolve_devices(&key, |alias| (alias == "scoped").then(|| "owner-id".into()));
        let entry = |device: &str, refused| DeviceEntry {
            device: device.into(),
            name: device.into(),
            presence: Presence::Online,
            statuses: Vec::new(),
            refused,
        };
        let fill = |_: &PluginId, _: &_, _: &_| {
            vec![
                entry("owner-id", None),
                entry("asleep", Some(Refusal::Presence)),
            ]
        };
        assert!(pages.fill_devices(None, fill));
        assert!(
            !pages.fill_devices(None, fill),
            "unchanged: the windows are not told again"
        );
        let shown = serde_json::to_value(&pages.snapshot()[0].view).unwrap();
        assert_eq!(shown["children"][0]["chosen"], "owner-id");
        // A chosen device the extension may no longer name is no longer chosen.
        assert!(shown["children"][1].get("chosen").is_none());
        assert_eq!(shown["children"][0]["entries"][1]["refused"], "presence");
        assert_eq!(
            pages.choosable(&key, "target", Some("owner-id")),
            Some(true)
        );
        assert_eq!(
            pages.choosable(&key, "target", Some("asleep")),
            Some(false),
            "refused"
        );
        assert_eq!(
            pages.choosable(&key, "target", Some("elsewhere")),
            Some(false),
            "not offered"
        );
        assert_eq!(
            pages.choosable(&key, "other", Some("owner-id")),
            None,
            "no such chooser"
        );
        // This device only where it is offered.
        assert_eq!(pages.choosable(&key, "gone", None), Some(true));
        assert_eq!(pages.choosable(&key, "target", None), Some(false));
        // Another extension's pages are left alone.
        let other = PluginId::new("other").unwrap();
        assert!(!pages.fill_devices(Some(&other), |_, _, _| Vec::new()));
    }

    #[test]
    fn a_placed_picture_gets_the_digest_of_what_its_own_extension_keeps_and_an_extension_cannot_write_one()
     {
        let mut pages = PageViews::default();
        pages.offer(&key(), Event::Open, Instant::now()).unwrap();
        pages.sent(&key(), 1);
        // Written by the extension itself: refused as a tree, so nothing is shown.
        let forged = json!({"k":"picture","id":"photo/1","width":20,"height":20,"digest":"00"});
        assert!(Node::parse(&forged).is_err());
        let tree = json!({"k":"box","children":[{"k":"picture","id":"photo/1","width":20,"height":20,"radius":4},
            {"k":"picture","id":"photo/2","width":20,"height":20}]});
        let (key, _, _) = pages.answered(1, Some(&tree)).unwrap();
        let kept = |plugin: &PluginId, id: &str| {
            (*plugin == key.plugin && id == "photo/1").then(|| "ab".repeat(32))
        };
        assert!(pages.fill_pictures(None, kept));
        assert!(
            !pages.fill_pictures(None, kept),
            "unchanged: the windows are not told again"
        );
        let shown = serde_json::to_value(&pages.snapshot()[0].view).unwrap();
        assert_eq!(shown["children"][0]["digest"], "ab".repeat(32));
        assert!(shown["children"][1].get("digest").is_none(), "nothing kept");
        // Let go of: the digest goes, so no window draws it.
        assert!(pages.fill_pictures(Some(&key.plugin), |_, _| None));
        let shown = serde_json::to_value(&pages.snapshot()[0].view).unwrap();
        assert!(shown["children"][0].get("digest").is_none());
        // Another extension's pages are left alone.
        let other = PluginId::new("other").unwrap();
        assert!(!pages.fill_pictures(Some(&other), |_, _| Some("cd".repeat(32))));
    }
}
