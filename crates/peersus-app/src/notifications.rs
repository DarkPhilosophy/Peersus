//! The notifications on show: one list for the application's own and for extensions', the same on every platform.
//!
//! A sender posts what the contract describes (`peersus_api::notification`); the list gives it a number, publishes it in the
//! snapshot, and tells the sender what the person answered. One that does not wait leaves by itself. Everything of an extension
//! is bounded and leaves with it. Where and how a notification is drawn is the window's; nothing here knows.
use peersus_api::{
    Error,
    message::Outcome,
    notification::{Answer, Notification},
};
use peersus_core::{
    model::{MissingGrant, NotificationSource, PeerId, PostedNotification},
    plugins::PluginId,
};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};
use tokio::{
    sync::{oneshot, watch},
    time::Instant,
};

/// How long one that does not wait is on show.
pub const PASSES_AFTER: Duration = Duration::from_secs(7);
/// The most one extension has on show at once: more would be a queue the person never asked for.
pub const MOST_OF_AN_EXTENSION: usize = 2;
/// The most the application itself has on show at once.
pub const MOST_OF_THE_APPLICATION: usize = 8;
/// Pending permission causes are bounded independently of incoming consent questions.
const MOST_MISSING_GRANTS: usize = 64;
const MOST_ABSENT_RECIPIENTS: usize = 16;
/// The most one extension may post within [`RATE_WINDOW`], shown or not.
pub const MOST_IN_WINDOW: usize = 6;
pub const RATE_WINDOW: Duration = Duration::from_secs(60);
/// The most words of tapped notifications kept for one extension until it takes them: more taps than it takes are old news.
const MOST_OPENED: usize = 8;

struct Shown {
    posted: PostedNotification,
    /// Present until the sender is told; a sender that stopped listening is simply not told.
    answer: Option<oneshot::Sender<Answer>>,
}

#[derive(Default)]
struct State {
    next: u64,
    shown: BTreeMap<u64, Shown>,
    /// Retained after dismissal, so the same unresolved cause cannot fill the window again.
    missing: BTreeMap<MissingGrant, u64>,
    absent: BTreeMap<(PeerId, String, u32), u64>,
    /// When each extension last posted, newest last, for the rate. Forgotten with the extension.
    posted_at: HashMap<PluginId, VecDeque<Instant>>,
    /// The words of an extension's tapped notifications that do not wait, oldest first, until it takes them.
    opened: HashMap<PluginId, VecDeque<String>>,
}

/// What a tap on a notification itself does: the extension whose page the window goes to (none for the application's own),
/// and whether that extension is to be told to take its word (one that waited was answered with it instead).
#[derive(Debug, PartialEq, Eq)]
pub struct Opening {
    pub page_of: Option<PluginId>,
    pub tell: bool,
}

pub struct Notifications {
    state: Mutex<State>,
    changed: watch::Sender<u64>,
}

impl Default for Notifications {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            changed: watch::channel(0).0,
        }
    }
}

/// A posted notification, as its sender holds it: its number, and the answer when there is one. The answer never comes for one
/// that left by itself, was taken back, or whose extension went: the receiver then ends with an error.
pub struct Posted {
    pub id: u64,
    pub answer: oneshot::Receiver<Answer>,
}

impl Notifications {
    fn state(&self) -> MutexGuard<'_, State> {
        // The list holds no invariant a panic elsewhere could break half-way: every change is one insert or one remove.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn told(&self) {
        self.changed.send_modify(|n| *n = n.wrapping_add(1));
    }

    /// Changes whenever the list does. Carries no contents.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    /// What is on show, oldest first.
    pub fn snapshot(&self) -> Vec<PostedNotification> {
        self.state()
            .shown
            .values()
            .map(|shown| shown.posted.clone())
            .collect()
    }

    /// Only the owner calls this, after checking a real, declared grant on this device.
    pub(crate) fn missing_permission(&self, cause: MissingGrant) {
        let mut state = self.state();
        if state.missing.len() >= MOST_MISSING_GRANTS || state.missing.contains_key(&cause) {
            return;
        }
        let Some(id) = state.next.checked_add(1) else {
            return;
        };
        state.next = id;
        state.missing.insert(cause.clone(), id);
        state.shown.insert(
            id,
            Shown {
                posted: PostedNotification {
                    id,
                    source: NotificationSource::MissingPermission(cause),
                    notification: Notification::saying(
                        "This extension is missing permission {permission}.",
                    ),
                },
                answer: None,
            },
        );
        drop(state);
        self.told();
    }

    pub(crate) fn unavailable_recipient(&self, peer: &PeerId, service: &str, version: u32) {
        let mut state = self.state();
        if state.absent.len() >= MOST_ABSENT_RECIPIENTS {
            return;
        }
        let cause = (peer.clone(), service.to_owned(), version);
        if state.absent.contains_key(&cause) {
            return;
        }
        let Some(id) = state.next.checked_add(1) else {
            return;
        };
        state.next = id;
        state.absent.insert(cause, id);
        state.shown.insert(
            id,
            Shown {
                posted: PostedNotification {
                    id,
                    source: NotificationSource::UnavailableRecipient,
                    notification: Notification::saying("No extension can receive this request."),
                },
                answer: None,
            },
        );
        drop(state);
        self.told();
    }

    pub(crate) fn retain_unavailable_recipients(
        &self,
        is_absent: impl Fn(&PeerId, &str, u32) -> bool,
    ) {
        let mut state = self.state();
        let State { absent, shown, .. } = &mut *state;
        let mut changed = false;
        absent.retain(|(peer, service, version), id| {
            if is_absent(peer, service, *version) {
                true
            } else {
                changed |= shown.remove(id).is_some();
                false
            }
        });
        drop(state);
        if changed {
            self.told();
        }
    }
    /// When the grant (or its scope) changes, remove both a visible warning and the record
    /// of its cause. A later, genuinely new refusal can then be shown once again.
    pub(crate) fn retain_missing_permissions(&self, is_missing: impl Fn(&MissingGrant) -> bool) {
        let mut state = self.state();
        let State { missing, shown, .. } = &mut *state;
        let mut changed = false;
        missing.retain(|cause, id| {
            if is_missing(cause) {
                true
            } else {
                changed |= shown.remove(id).is_some();
                false
            }
        });
        drop(state);
        if changed {
            self.told();
        }
    }

    /// Whether an extension may ask for one more now: at most [`MOST_IN_WINDOW`] requests within [`RATE_WINDOW`], shown or not.
    /// Taken by the service an extension posts through, before [`Self::post`]. What the application itself shows about an
    /// extension (a consent it must ask) is not the extension's request and does not pass here.
    pub fn admit(&self, plugin: &PluginId) -> Result<(), Error> {
        let now = Instant::now();
        let mut state = self.state();
        let recent = state.posted_at.entry(plugin.clone()).or_default();
        while recent
            .front()
            .is_some_and(|at| now.duration_since(*at) >= RATE_WINDOW)
        {
            recent.pop_front();
        }
        if recent.len() >= MOST_IN_WINDOW {
            return Err(Error::RateLimited);
        }
        recent.push_back(now);
        Ok(())
    }

    /// Put a notification on show. Refused, with nothing shown: one the contract does not allow (`InvalidPayload`) and one over
    /// what its sender may have on show (`QueueFull`).
    pub fn post(
        self: &Arc<Self>,
        source: NotificationSource,
        notification: Notification,
    ) -> Result<Posted, Error> {
        notification.validate()?;
        let (send, answer) = oneshot::channel();
        let remains = notification.remains();
        // One that opens and does not wait stands for a place on its sender's page: a newer one for the same place takes its
        // place, and when its sender has as many on show as it may, the oldest of those makes room. A question is never pushed out.
        let makes_room = notification.opens.is_some() && !notification.waits();
        let id = {
            let mut state = self.state();
            let most = match &source {
                NotificationSource::App => MOST_OF_THE_APPLICATION,
                NotificationSource::Plugin(_) => MOST_OF_AN_EXTENSION,
                NotificationSource::MissingPermission(_)
                | NotificationSource::UnavailableRecipient => {
                    return Err(Error::Denied);
                }
            };
            let replaced = makes_room.then(|| {
                state
                    .shown
                    .iter()
                    .filter(|(_, shown)| {
                        shown.posted.source == source
                            && shown.posted.notification.opens.is_some()
                            && !shown.posted.notification.waits()
                    })
                    .min_by_key(|(_, shown)| {
                        (
                            shown.posted.notification.opens != notification.opens,
                            shown.posted.id,
                        )
                    })
                    .map(|(id, shown)| (*id, shown.posted.notification.opens == notification.opens))
            });
            let on_show = state
                .shown
                .values()
                .filter(|shown| shown.posted.source == source)
                .count();
            match replaced.flatten() {
                Some((id, same)) if same || on_show >= most => {
                    state.shown.remove(&id);
                }
                _ if on_show >= most => return Err(Error::QueueFull),
                _ => {}
            }
            state.next = state.next.checked_add(1).ok_or(Error::Backend)?;
            let id = state.next;
            state.shown.insert(
                id,
                Shown {
                    posted: PostedNotification {
                        id,
                        source,
                        notification,
                    },
                    answer: Some(send),
                },
            );
            id
        };
        self.told();
        if !remains {
            let list = Arc::downgrade(self);
            tokio::spawn(async move {
                tokio::time::sleep(PASSES_AFTER).await;
                if let Some(list) = list.upgrade() {
                    list.remove(id);
                }
            });
        }
        Ok(Posted { id, answer })
    }

    /// Post one that waits and wait with it. `Cancelled` when it went without an answer: taken back, or its extension stopped.
    /// If the caller stops waiting (its own deadline passed), the notification leaves the screen with it.
    pub async fn ask(
        self: &Arc<Self>,
        source: NotificationSource,
        notification: Notification,
    ) -> Result<Answer, Error> {
        if !notification.waits() {
            return Err(Error::InvalidPayload);
        }
        let posted = self.post(source, notification)?;
        let _leaves_with_the_caller = Leaves {
            list: self.clone(),
            id: posted.id,
        };
        posted.answer.await.map_err(|_| Error::Cancelled)
    }

    /// The application's notification service, by method: `show` one, or take the words of the caller's tapped ones that did
    /// not wait (`opened`, `{}` → `{"opened": [word, …]}`, oldest first), which the caller is told to take by the event of
    /// the same name.
    pub async fn serve(
        self: Arc<Self>,
        caller: PluginId,
        method: &str,
        args: serde_json::Value,
    ) -> Outcome {
        match method {
            peersus_api::notification::OPENED => {
                if serde_json::from_value::<peersus_api::extension_data::Empty>(args).is_err() {
                    return Outcome::error("refused", "This method takes no arguments");
                }
                Outcome::ok(serde_json::json!({ "opened": self.take_opened(&caller) }))
            }
            _ => self.show(caller, args).await,
        }
    }

    /// What an extension asks through the application's service: `args` is the contract's notification, as it is. It counts
    /// against the extension's rate whether shown or not, is shown under the extension's name, and is answered at once (`{}`)
    /// when it passes by itself, or with what the person answered when it waits. Nothing is repaired on the way: what the
    /// contract refuses is refused whole. A caller that stops waiting takes its notification off the screen.
    pub async fn show(self: Arc<Self>, caller: PluginId, args: serde_json::Value) -> Outcome {
        let refused = |error| match error {
            Error::RateLimited => Outcome::error(
                "rate-limited",
                "This extension asked for too many notifications; it can ask again within a minute",
            ),
            Error::QueueFull => Outcome::error(
                "busy",
                "This extension already has as many notifications on show as it may",
            ),
            Error::Cancelled => {
                Outcome::error("cancelled", "The notification left without an answer")
            }
            _ => Outcome::error(
                "refused",
                "That is not a notification the application can show",
            ),
        };
        if let Err(error) = self.admit(&caller) {
            return refused(error);
        }
        let Ok(notification) = serde_json::from_value::<Notification>(args) else {
            return refused(Error::InvalidPayload);
        };
        let source = NotificationSource::Plugin(caller);
        if !notification.waits() {
            return match self.post(source, notification) {
                Ok(_) => Outcome::ok(serde_json::json!({})),
                Err(error) => refused(error),
            };
        }
        match self.ask(source, notification).await {
            Ok(answer) => {
                serde_json::to_value(answer).map_or_else(|_| refused(Error::Backend), Outcome::ok)
            }
            Err(error) => refused(error),
        }
    }

    /// The person's answer: the id of one of its buttons, or none, which closes it. The window's words for a refusal are fixed.
    pub fn answer(&self, id: u64, button: Option<String>) -> Result<(), &'static str> {
        let shown = {
            let mut state = self.state();
            let known = state
                .shown
                .get(&id)
                .ok_or("That notification is no longer shown")?;
            if let Some(button) = &button
                && !known
                    .posted
                    .notification
                    .buttons
                    .iter()
                    .any(|offered| &offered.id == button)
            {
                return Err("That notification has no such button");
            }
            state.shown.remove(&id)
        };
        self.told();
        if let Some(send) = shown.and_then(|shown| shown.answer) {
            // A sender that no longer listens is not an error of the person's.
            let _ = send.send(button.map_or(Answer::Dismissed, Answer::Pressed));
        }
        Ok(())
    }

    /// The person tapped the notification itself. Refused for one that is not on show and for one that opens nothing. It
    /// leaves; a sender that waits is answered with its word at once, and one that does not keeps the word until it takes it
    /// ([`Self::take_opened`]), told so by the caller.
    pub fn open(&self, id: u64) -> Result<Opening, &'static str> {
        let (shown, page_of, tell) = {
            let mut state = self.state();
            let known = state
                .shown
                .get(&id)
                .ok_or("That notification is no longer shown")?;
            let word = known
                .posted
                .notification
                .opens
                .clone()
                .ok_or("That notification opens nothing")?;
            let page_of = match &known.posted.source {
                NotificationSource::Plugin(plugin) => Some(plugin.clone()),
                _ => None,
            };
            let waits = known.posted.notification.waits();
            let mut shown = state.shown.remove(&id);
            let tell = match &page_of {
                Some(plugin) if !waits => {
                    let words = state.opened.entry(plugin.clone()).or_default();
                    if words.len() >= MOST_OPENED {
                        words.pop_front();
                    }
                    words.push_back(word);
                    true
                }
                _ => {
                    if let Some(send) = shown.as_mut().and_then(|shown| shown.answer.take()) {
                        let _ = send.send(Answer::Opened(word));
                    }
                    false
                }
            };
            (shown, page_of, tell)
        };
        drop(shown);
        self.told();
        Ok(Opening { page_of, tell })
    }

    /// The words of this extension's tapped notifications, oldest first, taken once.
    pub fn take_opened(&self, plugin: &PluginId) -> Vec<String> {
        self.state()
            .opened
            .remove(plugin)
            .map(Vec::from)
            .unwrap_or_default()
    }

    /// Take back one of the sender's own. Whether it was there: a number of another sender's is not.
    pub fn withdraw(&self, source: &NotificationSource, id: u64) -> bool {
        let mut state = self.state();
        let own = state
            .shown
            .get(&id)
            .is_some_and(|shown| &shown.posted.source == source);
        if own {
            state.shown.remove(&id);
            drop(state);
            self.told();
        }
        own
    }

    /// Everything of an extension leaves with it, and what it posted before is forgotten.
    pub fn revoke(&self, plugin: &PluginId) {
        let mut state = self.state();
        state.posted_at.remove(plugin);
        state.opened.remove(plugin);
        let before = state.shown.len();
        state.shown.retain(|_, shown| {
            !matches!(&shown.posted.source, NotificationSource::Plugin(of) if of == plugin)
        });
        let changed = before != state.shown.len();
        drop(state);
        if changed {
            self.told();
        }
    }

    fn remove(&self, id: u64) {
        if self.state().shown.remove(&id).is_some() {
            self.told();
        }
    }
}

struct Leaves {
    list: Arc<Notifications>,
    id: u64,
}

impl Drop for Leaves {
    fn drop(&mut self) {
        self.list.remove(self.id);
    }
}

/// What a person who agrees to everything was shown, oldest first.
#[cfg(test)]
pub(crate) type Seen = Arc<Mutex<Vec<PostedNotification>>>;

#[cfg(test)]
impl Notifications {
    /// A list on which a person agrees to everything: whatever waits with buttons is answered with its first one as soon as it
    /// is on show, and kept in what is returned beside the list. For tests of what asks through the list; needs a runtime.
    pub(crate) fn agreeing() -> (Arc<Self>, Seen) {
        let list = Arc::new(Self::default());
        let seen = Seen::default();
        let (watched, kept) = (Arc::downgrade(&list), seen.clone());
        let mut changes = list.subscribe();
        tokio::spawn(async move {
            loop {
                let Some(list) = watched.upgrade() else {
                    return;
                };
                for posted in list.snapshot() {
                    if let Some(first) = posted.notification.buttons.first()
                        && list.answer(posted.id, Some(first.id.clone())).is_ok()
                    {
                        kept.lock().unwrap().push(posted);
                    }
                }
                drop(list);
                if changes.changed().await.is_err() {
                    return;
                }
            }
        });
        (list, seen)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peersus_api::notification::{Button, Reach};

    fn plugin(id: &str) -> NotificationSource {
        NotificationSource::Plugin(PluginId::new(id).unwrap())
    }

    fn question() -> Notification {
        Notification {
            text: "Send the file?".into(),
            reach: Reach::Inside,
            stays: false,
            buttons: vec![
                Button {
                    id: "send".into(),
                    label: "Send it".into(),
                },
                Button {
                    id: "keep".into(),
                    label: "Keep it here".into(),
                },
            ],
            opens: None,
        }
    }

    /// Waits until this many are on show, by the list's own signal.
    async fn on_show(list: &Arc<Notifications>, count: usize) {
        let mut changes = list.subscribe();
        while list.snapshot().len() != count {
            tokio::time::timeout(Duration::from_secs(5), changes.changed())
                .await
                .expect("the list never came to that count")
                .unwrap();
        }
    }

    #[tokio::test]
    async fn the_sender_is_told_the_button_the_person_pressed_or_that_it_was_closed() {
        let list = Arc::new(Notifications::default());
        let asked = tokio::spawn({
            let list = list.clone();
            async move { list.ask(plugin("one"), question()).await }
        });
        on_show(&list, 1).await;
        let shown = list.snapshot();
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].source, plugin("one"));
        assert_eq!(shown[0].notification, question());
        // A button it does not have is refused and it stays; so is a number that is not there.
        assert!(list.answer(shown[0].id, Some("delete".into())).is_err());
        assert!(list.answer(shown[0].id + 1, None).is_err());
        assert_eq!(list.snapshot().len(), 1);
        list.answer(shown[0].id, Some("keep".into())).unwrap();
        assert_eq!(asked.await.unwrap(), Ok(Answer::Pressed("keep".into())));
        assert!(list.snapshot().is_empty());
        // Answered once: the same number again is refused.
        assert!(list.answer(shown[0].id, None).is_err());

        let asked = tokio::spawn({
            let list = list.clone();
            async move { list.ask(NotificationSource::App, question()).await }
        });
        on_show(&list, 1).await;
        let id = list.snapshot()[0].id;
        assert!(id > shown[0].id, "a number is never used twice");
        list.answer(id, None).unwrap();
        assert_eq!(asked.await.unwrap(), Ok(Answer::Dismissed));
    }

    #[tokio::test(start_paused = true)]
    async fn a_tap_on_one_that_opens_tells_only_its_sender_its_word_and_one_per_word_is_on_show() {
        let list = Arc::new(Notifications::default());
        let one = PluginId::new("one").unwrap();
        let opens = |word: &str| serde_json::json!({"text": "New message", "reach": "outside", "stays": true, "opens": word});
        // Answered at once though it stays, and it does not go by itself.
        let answered = list
            .clone()
            .serve(one.clone(), "show", opens("room.a"))
            .await;
        assert_eq!(answered, Outcome::ok(serde_json::json!({})));
        tokio::time::sleep(PASSES_AFTER * 2).await;
        assert_eq!(list.snapshot().len(), 1);
        // A newer one for the same word takes its place; two words fill what the extension may have; a third word makes room.
        for word in ["room.a", "room.b", "room.c"] {
            let shown = list.clone().serve(one.clone(), "show", opens(word)).await;
            assert_eq!(shown, Outcome::ok(serde_json::json!({})), "{word}");
        }
        let words: Vec<_> = list
            .snapshot()
            .into_iter()
            .map(|posted| posted.notification.opens.unwrap())
            .collect();
        assert_eq!(words, ["room.b", "room.c"]);
        // One that opens nothing cannot be opened; another extension's words are its own.
        let plain = list
            .post(NotificationSource::App, Notification::saying("Saved"))
            .unwrap();
        assert!(list.open(plain.id).is_err());
        let tapped = list.snapshot()[0].id;
        assert_eq!(
            list.open(tapped),
            Ok(Opening {
                page_of: Some(one.clone()),
                tell: true
            })
        );
        assert!(list.open(tapped).is_err(), "it left with the tap");
        assert!(list.take_opened(&PluginId::new("two").unwrap()).is_empty());
        let taken = list
            .clone()
            .serve(
                one.clone(),
                peersus_api::notification::OPENED,
                serde_json::json!({}),
            )
            .await;
        assert_eq!(
            taken,
            Outcome::ok(serde_json::json!({"opened": ["room.b"]}))
        );
        assert!(list.take_opened(&one).is_empty(), "taken once");
    }

    #[tokio::test]
    async fn a_question_that_opens_is_answered_with_its_word_when_tapped() {
        let list = Arc::new(Notifications::default());
        let asked = tokio::spawn({
            let list = list.clone();
            async move {
                list.ask(
                    plugin("one"),
                    Notification {
                        opens: Some("details".into()),
                        ..question()
                    },
                )
                .await
            }
        });
        on_show(&list, 1).await;
        let opening = list.open(list.snapshot()[0].id).unwrap();
        assert!(!opening.tell, "its sender waits and is answered instead");
        assert_eq!(asked.await.unwrap(), Ok(Answer::Opened("details".into())));
        assert!(list.take_opened(&PluginId::new("one").unwrap()).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn one_that_does_not_wait_leaves_by_itself_and_one_that_waits_does_not() {
        let list = Arc::new(Notifications::default());
        let passing = list
            .post(NotificationSource::App, Notification::saying("Saved"))
            .unwrap();
        let staying = list
            .post(
                NotificationSource::App,
                Notification {
                    stays: true,
                    ..Notification::saying("Read this")
                },
            )
            .unwrap();
        assert_eq!(list.snapshot().len(), 2);
        tokio::time::sleep(PASSES_AFTER - Duration::from_millis(1)).await;
        assert_eq!(list.snapshot().len(), 2);
        tokio::time::sleep(Duration::from_millis(2)).await;
        tokio::task::yield_now().await;
        let left = list.snapshot();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, staying.id);
        // Nobody answered the one that left.
        assert!(passing.answer.await.is_err());
        tokio::time::sleep(PASSES_AFTER * 100).await;
        assert_eq!(list.snapshot().len(), 1);
        // One that only says something cannot be asked: there would be nothing to wait for.
        assert_eq!(
            list.ask(NotificationSource::App, Notification::saying("Saved"))
                .await,
            Err(Error::InvalidPayload)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_extension_is_bounded_in_what_it_shows_and_how_often_and_takes_nothing_from_another()
    {
        let list = Arc::new(Notifications::default());
        let mut held = Vec::new();
        for _ in 0..MOST_OF_AN_EXTENSION {
            held.push(list.post(plugin("loud"), question()).unwrap());
        }
        assert_eq!(
            list.post(plugin("loud"), question()).err(),
            Some(Error::QueueFull)
        );
        // Another extension and the application still have their places.
        let other = list.post(plugin("quiet"), question()).unwrap();
        list.post(NotificationSource::App, question()).unwrap();
        // It cannot take back another's, and its own it can.
        assert!(!list.withdraw(&plugin("loud"), other.id));
        assert!(list.withdraw(&plugin("loud"), held[0].id));
        assert!(!list.withdraw(&plugin("loud"), held[0].id));
        // Asking and taking back in a loop meets the rate, whatever is on show.
        let loud = PluginId::new("loud").unwrap();
        let mut asked = 0;
        let refused = loop {
            match list.admit(&loud) {
                Ok(()) => {
                    asked += 1;
                    let again = list.post(plugin("loud"), question()).unwrap();
                    assert!(list.withdraw(&plugin("loud"), again.id));
                }
                Err(error) => break error,
            }
        };
        assert_eq!(refused, Error::RateLimited);
        assert_eq!(asked, MOST_IN_WINDOW);
        assert_eq!(list.admit(&PluginId::new("quiet").unwrap()), Ok(()));
        // What the application shows about it is not its request.
        assert!(list.post(plugin("loud"), question()).is_ok());
        // After the window it may ask again, and stopping it forgets what it asked.
        tokio::time::sleep(RATE_WINDOW).await;
        assert_eq!(list.admit(&loud), Ok(()));
        for _ in 1..MOST_IN_WINDOW {
            list.admit(&loud).unwrap();
        }
        assert_eq!(list.admit(&loud), Err(Error::RateLimited));
        list.revoke(&loud);
        assert_eq!(list.admit(&loud), Ok(()));
        // What the contract refuses is never shown.
        assert_eq!(
            list.post(NotificationSource::App, Notification::saying(" "))
                .err(),
            Some(Error::InvalidPayload)
        );
    }

    #[tokio::test]
    async fn what_an_extension_showed_leaves_with_it_and_with_a_caller_that_stopped_waiting() {
        let list = Arc::new(Notifications::default());
        let asked = tokio::spawn({
            let list = list.clone();
            async move { list.ask(plugin("gone"), question()).await }
        });
        on_show(&list, 1).await;
        list.post(plugin("stays"), question()).unwrap();
        list.revoke(&PluginId::new("gone").unwrap());
        assert_eq!(asked.await.unwrap(), Err(Error::Cancelled));
        let left = list.snapshot();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].source, plugin("stays"));

        let waiting = tokio::spawn({
            let list = list.clone();
            async move { list.ask(NotificationSource::App, question()).await }
        });
        on_show(&list, 2).await;
        waiting.abort();
        let _ = waiting.await;
        assert_eq!(list.snapshot().len(), 1);
    }

    #[tokio::test]
    async fn an_extension_is_answered_at_once_for_what_passes_and_with_the_persons_answer_for_what_waits()
     {
        use serde_json::json;
        let list = Arc::new(Notifications::default());
        let me = || PluginId::new("asks").unwrap();
        let code = |outcome: Outcome| match outcome {
            Outcome::Error { code, .. } => code,
            Outcome::Ok { value } => panic!("answered {value}"),
            Outcome::MissingPermission { .. } | Outcome::NotAccepted {} => {
                panic!("notification storage does not decide grants")
            }
        };

        let said = list.clone().show(me(), json!({"text": "Saved"})).await;
        assert_eq!(said, Outcome::ok(json!({})));
        let shown = list.snapshot();
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].source, plugin("asks"));
        assert_eq!(shown[0].notification, Notification::saying("Saved"));
        // Taken off here so that what follows does not depend on its seven seconds.
        assert!(list.withdraw(&plugin("asks"), shown[0].id));

        let asked = tokio::spawn(list.clone().show(
            me(),
            json!({"text": "Send the file?", "reach": "outside", "buttons": [
                {"id": "send", "label": "Send it"}, {"id": "keep", "label": "Keep it here"}
            ]}),
        ));
        on_show(&list, 1).await;
        let question = list.snapshot().pop().unwrap();
        assert_eq!(question.notification.reach, Reach::Outside);
        list.answer(question.id, Some("keep".into())).unwrap();
        assert_eq!(
            asked.await.unwrap(),
            Outcome::ok(json!({"answer": "pressed", "button": "keep"}))
        );
        let stays = tokio::spawn(
            list.clone()
                .show(me(), json!({"text": "Read this", "stays": true})),
        );
        on_show(&list, 1).await;
        list.answer(list.snapshot().pop().unwrap().id, None)
            .unwrap();
        assert_eq!(
            stays.await.unwrap(),
            Outcome::ok(json!({"answer": "dismissed"}))
        );

        // Nothing is repaired: a field the contract does not have, two buttons of one id, what is no notification at all.
        for bad in [
            json!({"text": "Hello", "width": 900}),
            json!({"text": "Hello", "buttons": [{"id": "a", "label": "A"}, {"id": "a", "label": "B"}]}),
            json!("Hello"),
        ] {
            assert_eq!(code(list.clone().show(me(), bad).await), "refused");
        }
        assert!(list.snapshot().is_empty(), "nothing refused was shown");
        // Six requests so far, which is what a minute allows, and a refused one counted.
        assert_eq!(
            code(list.clone().show(me(), json!({"text": "Again"})).await),
            "rate-limited"
        );

        // As many on show as it may have: one more is told as busy and is not shown.
        let full = PluginId::new("full").unwrap();
        let mut waiting = Vec::new();
        for _ in 0..MOST_OF_AN_EXTENSION {
            waiting.push(tokio::spawn(
                list.clone()
                    .show(full.clone(), json!({"text": "Wait", "stays": true})),
            ));
        }
        on_show(&list, MOST_OF_AN_EXTENSION).await;
        assert_eq!(
            code(
                list.clone()
                    .show(full.clone(), json!({"text": "More"}))
                    .await
            ),
            "busy"
        );
        // It stops: what it asked ends as cancelled and leaves the screen.
        list.revoke(&full);
        for asked in waiting {
            assert_eq!(code(asked.await.unwrap()), "cancelled");
        }
        assert!(list.snapshot().is_empty());
    }
}
