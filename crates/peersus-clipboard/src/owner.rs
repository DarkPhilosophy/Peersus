//! The one thread that owns the system clipboard, and the way the rest of the plugin reaches it.
//!
//! The operating system library blocks while it works and does not like being used from
//! several threads at once. So it lives on ONE thread, started once, and everything else sends
//! it a request and awaits the answer. Awaiting is the point: a caller on the application's
//! connection is never made to block, and nothing here calls `block_on`, which panics when it
//! is used from inside a running runtime.
//!
//! What the thread holds is a [`Store`], so this can be tested with one that is not a desktop.
use crate::{
    plugin::{Clipboard, Reading},
    watch::Signal,
    wire,
};
use std::{
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError},
    thread,
};
use tokio::sync::oneshot;

/// The system clipboard, as far as the owner thread uses it. Every call may block.
pub trait Store {
    fn get(&mut self) -> Option<String>;
    fn set(&mut self, text: &str) -> bool;
    /// Start listening for real selection changes, if this platform can tell a new copy from
    /// old contents. A signal says that something changed and whether it offers text, never
    /// what it is: nothing may read the clipboard until the session asks. Returns whether
    /// signals will arrive; the default is no, and the caller then only ever sees snapshots.
    fn listen(&mut self, _signals: SyncSender<Signal>) -> bool {
        false
    }
}

enum Request {
    Get(oneshot::Sender<Option<String>>),
    Set(String, oneshot::Sender<bool>),
    /// What the clipboard did since the last question.
    Observe(oneshot::Sender<Reading>),
    /// Forget everything from before now; the next copy seen is only a baseline.
    Reset(oneshot::Sender<bool>),
}

/// Most signals kept for one question; anything older than this was replaced anyway.
const SIGNAL_BACKLOG: usize = 16;

/// A handle to the clipboard thread. Cheap to use from asynchronous code.
pub struct Owner {
    requests: SyncSender<Request>,
}

/// What the owner thread knows about the clipboard between questions. It never leaves the thread.
struct Watching {
    signals: Option<Receiver<Signal>>,
    /// False until one signal has been taken as the starting point. That first one is whatever
    /// was already there, and must never be sent as if it were just copied.
    primed: bool,
    /// The text this plugin itself last put on the clipboard, so its own write is not a copy.
    written: Option<String>,
}

impl Watching {
    /// The newest thing announced since the last question, if anything.
    fn drain(&mut self) -> Option<Signal> {
        let signals = self.signals.as_ref()?;
        let mut newest = None;
        loop {
            match signals.try_recv() {
                Ok(signal) => newest = Some(signal),
                Err(TryRecvError::Empty) => return newest,
                Err(TryRecvError::Disconnected) => {
                    // The listener has gone (the compositor restarted, or it said no). Reporting
                    // "nothing new" forever would end automatic sending silently, which is worse
                    // than before there were signals at all: go back to reading snapshots.
                    self.signals = None;
                    return newest;
                }
            }
        }
    }

    fn read<S: Store>(store: &mut Option<S>) -> Option<String> {
        store
            .as_mut()
            .and_then(Store::get)
            .filter(|text| text.len() <= wire::MAX_TEXT as usize)
    }

    fn observe<S: Store>(&mut self, store: &mut Option<S>) -> Reading {
        // Nothing new is not "unreadable". The clipboard still holds what it held, and saying
        // so (as a snapshot) is what keeps the session's own comparison point current; saying
        // "unavailable" every tick would make it forget that point every tick.
        let snapshot = |text: Option<String>| text.map_or(Reading::Unavailable, Reading::Snapshot);
        if self.signals.is_none() {
            return snapshot(Self::read(store));
        }
        let Some(signal) = self.drain() else {
            return snapshot(Self::read(store));
        };
        let first = !std::mem::replace(&mut self.primed, true);
        match signal {
            Signal::Other => Reading::Unavailable,
            Signal::Text => match Self::read(store) {
                None => Reading::Unavailable,
                Some(text) if first || self.written.as_deref() == Some(text.as_str()) => {
                    Reading::Snapshot(text)
                }
                Some(text) => Reading::Changed(text),
            },
        }
    }
}

impl Owner {
    /// Start the thread. `open` runs ON the thread, because the library ties its connection to
    /// the thread that made it; `None` means there is no clipboard here (no display, no seat),
    /// and every call then simply says there is nothing, instead of the plugin failing to start.
    pub fn start<S, F>(open: F) -> Self
    where
        S: Store,
        F: FnOnce() -> Option<S> + Send + 'static,
    {
        // Cancelled callers must not leave an unbounded backlog behind a blocked OS call.
        let (requests, incoming) = mpsc::sync_channel::<Request>(8);
        thread::spawn(move || {
            let mut store = open();
            let mut watching = Watching {
                signals: None,
                primed: false,
                written: None,
            };
            if let Some(store) = store.as_mut() {
                let (sender, signals) = mpsc::sync_channel(SIGNAL_BACKLOG);
                if store.listen(sender) {
                    watching.signals = Some(signals);
                }
            }
            while let Ok(request) = incoming.recv() {
                match request {
                    Request::Get(reply) => {
                        if !reply.is_closed() {
                            let text = store
                                .as_mut()
                                .and_then(Store::get)
                                .filter(|text| text.len() <= wire::MAX_TEXT as usize);
                            let _ = reply.send(text);
                        }
                    }
                    Request::Set(text, reply) => {
                        // A queued operation whose caller has gone must not change the clipboard.
                        if !reply.is_closed() {
                            let done = store.as_mut().is_some_and(|s| s.set(&text));
                            if done {
                                watching.written = Some(text);
                            }
                            let _ = reply.send(done);
                        }
                    }
                    Request::Observe(reply) => {
                        if !reply.is_closed() {
                            let _ = reply.send(watching.observe(&mut store));
                        }
                    }
                    Request::Reset(reply) => {
                        // Consent changed. This must never read the clipboard: while sending is
                        // off, nothing may look at what is on it. Only forget what was announced
                        // before now; the first announcement after this is a baseline, because
                        // whether it is old or new cannot be told without reading.
                        watching.drain();
                        watching.primed = false;
                        watching.written = None;
                        let _ = reply.send(true);
                    }
                }
            }
        });
        Self { requests }
    }
}

impl Clipboard for Owner {
    async fn text(&mut self) -> Option<String> {
        let (reply, answer) = oneshot::channel();
        self.requests.try_send(Request::Get(reply)).ok()?;
        answer.await.ok().flatten()
    }

    async fn set_text(&mut self, text: &str) -> bool {
        if text.len() > wire::MAX_TEXT as usize {
            return false;
        }
        let (reply, answer) = oneshot::channel();
        if self
            .requests
            .try_send(Request::Set(text.to_owned(), reply))
            .is_err()
        {
            return false;
        }
        answer.await.unwrap_or(false)
    }

    async fn observe(&mut self) -> Reading {
        let (reply, answer) = oneshot::channel();
        if self.requests.try_send(Request::Observe(reply)).is_err() {
            return Reading::Unavailable;
        }
        answer.await.unwrap_or(Reading::Unavailable)
    }

    async fn reset_observation(&mut self) -> bool {
        let (reply, answer) = oneshot::channel();
        if self.requests.try_send(Request::Reset(reply)).is_err() {
            return false;
        }
        answer.await.unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A clipboard that is a string, which also records the thread each call ran on.
    struct Fake {
        held: Arc<Mutex<Option<String>>>,
        threads: Arc<Mutex<Vec<thread::ThreadId>>>,
    }

    impl Store for Fake {
        fn get(&mut self) -> Option<String> {
            self.threads.lock().unwrap().push(thread::current().id());
            self.held.lock().unwrap().clone()
        }
        fn set(&mut self, text: &str) -> bool {
            self.threads.lock().unwrap().push(thread::current().id());
            *self.held.lock().unwrap() = Some(text.to_owned());
            true
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn it_is_used_from_inside_a_running_runtime_without_panicking() {
        // The way the plugin uses it: from asynchronous code on a runtime's worker. A caller
        // that blocked on a runtime handle here would panic with "Cannot start a runtime from
        // within a runtime".
        let held = Arc::new(Mutex::new(None));
        let threads = Arc::new(Mutex::new(Vec::new()));
        let mut owner = Owner::start({
            let (held, threads) = (held.clone(), threads.clone());
            move || Some(Fake { held, threads })
        });
        assert!(owner.set_text("hello").await);
        assert_eq!(owner.text().await.as_deref(), Some("hello"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn every_use_runs_on_the_one_thread_and_never_on_the_callers() {
        let held = Arc::new(Mutex::new(None));
        let threads = Arc::new(Mutex::new(Vec::new()));
        let mut owner = Owner::start({
            let (held, threads) = (held.clone(), threads.clone());
            move || Some(Fake { held, threads })
        });
        for n in 0..5 {
            assert!(owner.set_text(&format!("text {n}")).await);
            owner.text().await;
        }
        let threads = threads.lock().unwrap();
        assert_eq!(threads.len(), 10);
        assert!(
            threads.iter().all(|id| *id == threads[0]),
            "calls ran on more than one thread"
        );
        assert_ne!(threads[0], thread::current().id());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn with_no_clipboard_there_is_nothing_to_read_and_nothing_can_be_set() {
        let mut owner = Owner::start(|| None::<Fake>);
        assert_eq!(owner.text().await, None);
        assert!(!owner.set_text("lost").await);
    }

    #[tokio::test]
    async fn oversized_writes_never_reach_the_store() {
        let held = Arc::new(Mutex::new(Some("before".to_owned())));
        let threads = Arc::new(Mutex::new(Vec::new()));
        let mut owner = Owner::start({
            let (held, threads) = (held.clone(), threads.clone());
            move || Some(Fake { held, threads })
        });
        assert!(
            !owner
                .set_text(&"x".repeat(wire::MAX_TEXT as usize + 1))
                .await
        );
        assert_eq!(held.lock().unwrap().as_deref(), Some("before"));
        assert!(threads.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn cancelled_queued_writes_do_not_replace_the_clipboard() {
        let held = Arc::new(Mutex::new(Some("before".to_owned())));
        let threads = Arc::new(Mutex::new(Vec::new()));
        let (release, opened) = mpsc::sync_channel(1);
        let mut owner = Owner::start({
            let (held, threads) = (held.clone(), threads.clone());
            move || {
                opened.recv().unwrap();
                Some(Fake { held, threads })
            }
        });
        let (reply, answer) = oneshot::channel();
        assert!(
            owner
                .requests
                .try_send(Request::Set("stale".to_owned(), reply))
                .is_ok()
        );
        drop(answer);
        release.send(()).unwrap();
        // A subsequent read is an ordering barrier on the same owner thread.
        assert_eq!(owner.text().await.as_deref(), Some("before"));
        assert_eq!(held.lock().unwrap().as_deref(), Some("before"));
        assert_eq!(threads.lock().unwrap().len(), 1);
    }

    /// A clipboard that announces copies the way a compositor does: a copy both changes what is
    /// held and says that something changed, without saying what.
    struct Evented {
        held: Arc<Mutex<Option<String>>>,
        feed: Arc<Mutex<Option<SyncSender<Signal>>>>,
    }
    impl Store for Evented {
        fn get(&mut self) -> Option<String> {
            self.held.lock().unwrap().clone()
        }
        fn set(&mut self, text: &str) -> bool {
            *self.held.lock().unwrap() = Some(text.to_owned());
            true
        }
        fn listen(&mut self, signals: SyncSender<Signal>) -> bool {
            *self.feed.lock().unwrap() = Some(signals);
            true
        }
    }

    /// The test's hand on that clipboard: what another application does.
    struct Copier {
        held: Arc<Mutex<Option<String>>>,
        signals: SyncSender<Signal>,
    }
    impl Copier {
        fn copy(&self, text: &str) {
            *self.held.lock().unwrap() = Some(text.to_owned());
            self.signals.send(Signal::Text).unwrap();
        }
        /// A picture, a file list or a cleared selection: announced, but not text.
        fn copy_something_else(&self) {
            *self.held.lock().unwrap() = None;
            self.signals.send(Signal::Other).unwrap();
        }
    }

    async fn evented() -> (Owner, Copier) {
        let held = Arc::new(Mutex::new(None));
        let feed = Arc::new(Mutex::new(None));
        let owner = Owner::start({
            let (held, feed) = (held.clone(), feed.clone());
            move || Some(Evented { held, feed })
        });
        // Wait until the owner thread has started listening.
        for _ in 0..200 {
            if let Some(signals) = feed.lock().unwrap().clone() {
                return (owner, Copier { held, signals });
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("the owner thread never started listening");
    }

    fn is_changed(reading: &Reading, expected: &str) -> bool {
        matches!(reading, Reading::Changed(text) if text == expected)
    }
    fn is_snapshot(reading: &Reading, expected: &str) -> bool {
        matches!(reading, Reading::Snapshot(text) if text == expected)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn what_was_already_on_the_clipboard_is_a_baseline_and_never_sent_as_a_new_copy() {
        let (mut owner, copy) = evented().await;
        copy.copy("was already here");
        assert!(is_snapshot(&owner.observe().await, "was already here"));
        copy.copy("copied now");
        assert!(is_changed(&owner.observe().await, "copied now"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn text_copied_after_a_picture_is_a_new_copy_even_when_it_is_the_same_text_as_before() {
        // The report this exists for: copy text, copy a screenshot, copy the same text again.
        let (mut owner, copy) = evented().await;
        copy.copy("baseline");
        owner.observe().await;
        copy.copy("hello");
        assert!(is_changed(&owner.observe().await, "hello"));
        copy.copy_something_else();
        assert!(matches!(owner.observe().await, Reading::Unavailable));
        copy.copy("hello");
        assert!(is_changed(&owner.observe().await, "hello"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn with_nothing_new_it_reads_the_clipboard_as_a_snapshot_and_never_waits() {
        // Not "unavailable": the session compares snapshots to keep its starting point current,
        // and a tick that said "unavailable" would make it forget that point every time.
        let (mut owner, copy) = evented().await;
        copy.copy("baseline");
        owner.observe().await;
        let asked =
            tokio::time::timeout(std::time::Duration::from_millis(500), owner.observe()).await;
        assert!(matches!(asked, Ok(Reading::Snapshot(text)) if text == "baseline"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn only_the_newest_of_several_copies_between_two_questions_is_reported() {
        let (mut owner, copy) = evented().await;
        copy.copy("baseline");
        owner.observe().await;
        copy.copy("first");
        copy.copy("second");
        assert!(is_changed(&owner.observe().await, "second"));
        assert!(is_snapshot(&owner.observe().await, "second"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn text_this_plugin_wrote_is_not_reported_back_as_a_copy() {
        let (mut owner, copy) = evented().await;
        copy.copy("baseline");
        owner.observe().await;
        assert!(owner.set_text("from another device").await);
        copy.signals.send(Signal::Text).unwrap();
        assert!(is_snapshot(&owner.observe().await, "from another device"));
        copy.copy("typed by the person");
        assert!(is_changed(&owner.observe().await, "typed by the person"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_reset_forgets_what_was_announced_before_it_and_starts_over_from_a_baseline() {
        // Copies made while sending was off must not be sent the moment it is switched on.
        let (mut owner, copy) = evented().await;
        copy.copy("baseline");
        owner.observe().await;
        copy.copy("copied while sending was off");
        assert!(owner.reset_observation().await);
        // The announcement is forgotten; what is still on the clipboard is an ordinary snapshot.
        assert!(is_snapshot(
            &owner.observe().await,
            "copied while sending was off"
        ));
        // The first announcement after a reset is only a baseline, never a copy to send.
        copy.copy("the first copy after");
        assert!(is_snapshot(&owner.observe().await, "the first copy after"));
        copy.copy("and the one after that");
        assert!(is_changed(&owner.observe().await, "and the one after that"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn without_signals_the_clipboard_is_still_read_as_a_snapshot() {
        let held = Arc::new(Mutex::new(Some("plain".to_owned())));
        let threads = Arc::new(Mutex::new(Vec::new()));
        let mut owner = Owner::start({
            let (held, threads) = (held.clone(), threads.clone());
            move || Some(Fake { held, threads })
        });
        assert!(is_snapshot(&owner.observe().await, "plain"));
        *held.lock().unwrap() = None;
        assert!(matches!(owner.observe().await, Reading::Unavailable));
        assert!(owner.reset_observation().await);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn oversized_text_is_not_text_for_the_plugin() {
        let (mut owner, copy) = evented().await;
        copy.copy("baseline");
        owner.observe().await;
        copy.copy(&"x".repeat(wire::MAX_TEXT as usize + 1));
        assert!(matches!(owner.observe().await, Reading::Unavailable));
    }

    /// Counts every read of the clipboard, to prove what never looks at it.
    struct Counting {
        reads: Arc<std::sync::atomic::AtomicUsize>,
        feed: Arc<Mutex<Option<SyncSender<Signal>>>>,
    }
    impl Store for Counting {
        fn get(&mut self) -> Option<String> {
            self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Some("private".to_owned())
        }
        fn set(&mut self, _: &str) -> bool {
            true
        }
        fn listen(&mut self, signals: SyncSender<Signal>) -> bool {
            *self.feed.lock().unwrap() = Some(signals);
            true
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn copies_and_resets_never_read_the_clipboard_until_the_session_asks() {
        // With automatic sending off the session never asks. Whatever is copied meanwhile, and
        // however consent changes, nothing may look at what is on the clipboard.
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let feed = Arc::new(Mutex::new(None));
        let mut owner = Owner::start({
            let (reads, feed) = (reads.clone(), feed.clone());
            move || Some(Counting { reads, feed })
        });
        let signals = loop {
            if let Some(signals) = feed.lock().unwrap().clone() {
                break signals;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        };
        for _ in 0..5 {
            signals.send(Signal::Text).unwrap();
            assert!(owner.reset_observation().await);
        }
        assert_eq!(
            reads.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the clipboard was read without being asked"
        );
        owner.observe().await;
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn old_text_re_announced_after_a_reset_is_never_sent() {
        // A compositor may re-announce what was already there (a focus or seat change). That is
        // old content, and consent given a moment ago does not cover it.
        let (mut owner, copy) = evented().await;
        copy.copy("private, from before consent");
        owner.observe().await;
        assert!(owner.reset_observation().await);
        copy.signals.send(Signal::Text).unwrap();
        assert!(is_snapshot(
            &owner.observe().await,
            "private, from before consent"
        ));
        copy.copy("copied after consent");
        assert!(is_changed(&owner.observe().await, "copied after consent"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn when_the_listener_dies_the_plugin_goes_back_to_reading_snapshots() {
        let (mut owner, copy) = evented().await;
        copy.copy("baseline");
        owner.observe().await;
        let Copier { held, signals } = copy;
        // The listener is gone for good: the compositor restarted, or the connection broke.
        drop(signals);
        *held.lock().unwrap() = Some("still readable".to_owned());
        assert!(is_snapshot(&owner.observe().await, "still readable"));
        *held.lock().unwrap() = Some("and later copies too".to_owned());
        assert!(is_snapshot(&owner.observe().await, "and later copies too"));
    }

    #[tokio::test]
    async fn a_full_request_queue_refuses_instead_of_blocking_the_executor() {
        let (requests, _incoming) = mpsc::sync_channel(1);
        let mut owner = Owner { requests };
        let (reply, _answer) = oneshot::channel();
        assert!(owner.requests.try_send(Request::Get(reply)).is_ok());
        assert_eq!(owner.text().await, None);
        assert!(!owner.set_text("not queued").await);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_clipboard_that_cannot_be_opened_is_opened_on_the_owner_thread() {
        // The library ties its connection to the thread that made it, so `open` must run on
        // the owner's thread, not on whichever thread called `start`.
        let opened_on = Arc::new(Mutex::new(None));
        let _owner = Owner::start({
            let opened_on = opened_on.clone();
            move || {
                *opened_on.lock().unwrap() = Some(thread::current().id());
                None::<Fake>
            }
        });
        // Wait until the thread has run `open`.
        for _ in 0..200 {
            if opened_on.lock().unwrap().is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let opened = opened_on.lock().unwrap().expect("open never ran");
        assert_ne!(opened, thread::current().id());
    }
}
