//! Desktop host clipboard service. The system clipboard is reached on a thread of its own
//! inside the application (the same owner thread the native extension uses), made the first
//! time an extension asks; bounded handles, ownership and UTF-8 chunking use the same rules as
//! Android.
use crate::{
    clipboard_handles::{Board, Handles, Method, Reading},
    platform_services::{CLIPBOARD_SERVICE, Call, PlatformServices, Response, SelectedInput},
};
use peersus_api::message::Outcome;
use peersus_clipboard_platform::{
    Clipboard,
    owner::{Owner, Retired},
};
use peersus_core::plugins::PluginId;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::watch;

/// What the system says about announcing copies. `started` is false until an extension first
/// used the clipboard: nothing is opened, listened to or polled before that.
#[derive(Clone, Copy, Default)]
struct Notice {
    started: bool,
    supported: bool,
    generation: u64,
    /// Which owner thread the notice is about: one that was let go must not speak for its successor.
    owner: u64,
}

/// How long one clipboard operation may take. One that takes longer leaves the clipboard
/// unanswered for this call; the owner thread is made again for the next one.
const DEADLINE: Duration = Duration::from_secs(8);
/// A thread stuck inside the system's clipboard cannot be stopped from outside. After this many
/// have hung, no new one is made: the clipboard stays unavailable until the application starts
/// again, rather than leaving a stuck thread behind for every later call.
/// The count is of threads still inside the clipboard: one that exits frees its place, and
/// nothing else ever does — not a later success elsewhere, not a timer.
const MOST_HUNG: usize = 2;

/// Opens an owner thread wired to a board's notices: first the copy-event health callback,
/// then the one-time readiness answer. A factory so tests can use a store that is no desktop.
type Open = Arc<
    dyn Fn(
            peersus_clipboard_platform::watch::Notifier,
            peersus_clipboard_platform::watch::Notifier,
        ) -> Owner
        + Send
        + Sync,
>;

/// Everything guarded while an operation runs on the owner thread.
struct Owners {
    current: Option<Owner>,
    /// Threads a timeout let go of. Each is counted only while it is still inside the system's
    /// clipboard: that live count, not the number of timeouts, is what [`MOST_HUNG`] bounds.
    retired: Vec<Retired>,
    /// How long one operation may take; a field so tests can exercise the timeout path quickly.
    deadline: Duration,
    open: Open,
}

struct DesktopBoard {
    owners: tokio::sync::Mutex<Owners>,
    notices: watch::Sender<Notice>,
}

impl DesktopBoard {
    /// The owner thread, made on first use together with its copy listener.
    fn owner<'a>(&self, owners: &'a mut Owners) -> &'a mut Owner {
        let open = owners.open.clone();
        owners.current.get_or_insert_with(|| {
            let notices = self.notices.clone();
            let mut current = 0;
            notices.send_modify(|notice| {
                notice.started = true;
                notice.supported = false;
                notice.generation = notice.generation.wrapping_add(1);
                notice.owner = notice.owner.wrapping_add(1);
                current = notice.owner;
            });
            // A thread that was let go may still speak later; only the current one is heard.
            let announced = notices.clone();
            let notifier: peersus_clipboard_platform::watch::Notifier = Arc::new(move |healthy| {
                announced.send_if_modified(|notice| {
                    if notice.owner != current {
                        return false;
                    }
                    notice.supported &= healthy;
                    notice.generation = notice.generation.wrapping_add(1);
                    true
                });
            });
            // Whether this system announces copies is answered by the owner thread itself once
            // its listener starts, so a clipboard blocked while opening leaves no extra thread
            // waiting forever for an answer that can never arrive.
            let ready: peersus_clipboard_platform::watch::Notifier = Arc::new(move |supported| {
                notices.send_if_modified(|notice| {
                    if notice.owner != current {
                        return false;
                    }
                    notice.supported = supported;
                    notice.generation = notice.generation.wrapping_add(1);
                    true
                });
            });
            open(notifier, ready)
        })
    }

    /// Run one operation on the owner thread within the deadline. An owner that did not answer
    /// in time is retired: gone for callers, but counted while its thread is still inside the
    /// system's clipboard, so a clipboard that hangs neither holds up a later call nor spawns
    /// owners without bound.
    async fn with_owner<T>(
        &self,
        operation: impl AsyncFnOnce(&mut Owner) -> T,
        unanswered: T,
    ) -> T {
        let mut owners = self.owners.lock().await;
        owners.retired.retain(Retired::alive);
        if owners.current.is_none() && owners.retired.len() >= MOST_HUNG {
            return unanswered;
        }
        let deadline = owners.deadline;
        let owner = self.owner(&mut owners);
        match tokio::time::timeout(deadline, operation(owner)).await {
            Ok(value) => value,
            Err(_) => {
                if let Some(retired) = owners.current.take().map(Owner::retire) {
                    owners.retired.push(retired);
                }
                self.notices.send_modify(|notice| {
                    notice.owner = notice.owner.wrapping_add(1);
                    notice.supported = false;
                    notice.generation = notice.generation.wrapping_add(1);
                });
                unanswered
            }
        }
    }

    /// Retired owner threads still inside the system's clipboard.
    #[cfg(test)]
    async fn stuck(&self) -> usize {
        self.owners
            .lock()
            .await
            .retired
            .iter()
            .filter(|retired| retired.alive())
            .count()
    }
}

impl Board for DesktopBoard {
    async fn read(&self, observe: bool) -> Reading {
        let unavailable = Reading {
            text: None,
            changed: false,
        };
        self.with_owner(
            async |owner: &mut Owner| {
                let reading = if observe {
                    owner.observe().await
                } else {
                    owner
                        .text()
                        .await
                        .map_or(peersus_clipboard_platform::Reading::Unavailable, |text| {
                            peersus_clipboard_platform::Reading::Snapshot(text)
                        })
                };
                match reading {
                    peersus_clipboard_platform::Reading::Unavailable => Reading {
                        text: None,
                        changed: false,
                    },
                    peersus_clipboard_platform::Reading::Snapshot(text) => Reading {
                        text: Some(text),
                        changed: false,
                    },
                    peersus_clipboard_platform::Reading::Changed(text) => Reading {
                        text: Some(text),
                        changed: true,
                    },
                }
            },
            unavailable,
        )
        .await
    }
    async fn write(&self, text: String) -> bool {
        if text.len() > peersus_clipboard_platform::MAX_TEXT {
            return false;
        }
        self.with_owner(async |owner: &mut Owner| owner.set_text(&text).await, false)
            .await
    }
    async fn reset(&self) -> bool {
        self.with_owner(
            async |owner: &mut Owner| owner.reset_observation().await,
            false,
        )
        .await
    }
}

#[derive(Default)]
struct State {
    handles: Handles,
    next: u64,
    generations: HashMap<PluginId, u64>,
    selected: HashMap<u64, Selection>,
}

struct Selection {
    caller: PluginId,
    text: String,
    expires: Instant,
}

#[derive(Clone)]
pub struct DesktopClipboard {
    board: Arc<DesktopBoard>,
    state: Arc<Mutex<State>>,
}

impl DesktopClipboard {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            board: Arc::new(DesktopBoard {
                owners: tokio::sync::Mutex::new(Owners {
                    current: None,
                    retired: Vec::new(),
                    deadline: DEADLINE,
                    open: Arc::new(peersus_clipboard_platform::desktop::open_watched),
                }),
                notices: watch::channel(Notice::default()).0,
            }),
            state: Arc::new(Mutex::new(State::default())),
        })
    }

    fn state(&self) -> Result<std::sync::MutexGuard<'_, State>, &'static str> {
        self.state
            .lock()
            .map_err(|_| "Clipboard service unavailable")
    }

    pub(crate) async fn copy_result(&self, text: String) -> Result<(), &'static str> {
        if self.board.write(text).await {
            Ok(())
        } else {
            Err("The result link could not be copied")
        }
    }

    async fn execute(&self, call: Call) -> Result<Value, &'static str> {
        if call.service != CLIPBOARD_SERVICE {
            return Err("This platform service is unavailable");
        }
        if call.method == "selected" {
            let context = call
                .args
                .get("context")
                .and_then(Value::as_u64)
                .ok_or("Missing selected input context")?;
            let mut state = self.state()?;
            state.handles.may_capture(&call.caller)?;
            let selection = state
                .selected
                .get(&context)
                .filter(|selection| {
                    selection.caller == call.caller && selection.expires > Instant::now()
                })
                .ok_or("Selected input is unavailable")?;
            let text = selection.text.clone();
            let handle = format!("selected-{context}");
            let result = state.handles.hold_selected(&call.caller, handle, text)?;
            state.selected.remove(&context);
            return Ok(result);
        }
        let method = Method::parse(&call.method).ok_or("Unknown clipboard method")?;
        match method {
            Method::Read | Method::Observe => {
                let generation = {
                    let state = self.state()?;
                    state.handles.may_capture(&call.caller)?;
                    *state.generations.get(&call.caller).unwrap_or(&0)
                };
                let reading = self.board.read(matches!(method, Method::Observe)).await;
                let mut state = self.state()?;
                if *state.generations.get(&call.caller).unwrap_or(&0) != generation {
                    return Err("Clipboard request was revoked");
                }
                state.next = state
                    .next
                    .checked_add(1)
                    .ok_or("Clipboard handle limit reached")?;
                let handle = format!("clip-{}", state.next);
                state.handles.held(&call.caller, handle, reading)
            }
            Method::Chunk => self.state()?.handles.chunk(&call.caller, &call.args),
            Method::Write => {
                let mut state = self.state()?;
                state.next = state
                    .next
                    .checked_add(1)
                    .ok_or("Clipboard handle limit reached")?;
                let handle = format!("put-{}", state.next);
                state.handles.begin(&call.caller, handle, &call.args)
            }
            Method::Append => self.state()?.handles.append(&call.caller, &call.args),
            Method::Commit => {
                let text = self.state()?.handles.finish(&call.caller, &call.args)?;
                crate::clipboard_handles::commit(self.board.as_ref(), text).await
            }
            Method::Abort => self.state()?.handles.abort(&call.caller, &call.args),
            Method::Reset => Ok(crate::clipboard_handles::reset(self.board.as_ref()).await),
            Method::Release => {
                self.state()?.handles.release(&call.caller);
                Ok(json!({"released":true}))
            }
        }
    }
}

impl PlatformServices for DesktopClipboard {
    fn dispatch(&self, call: Call) -> Response {
        let service = self.clone();
        Box::pin(async move {
            match service.execute(call).await {
                Ok(value) => Outcome::ok(value),
                Err(reason) => Outcome::error("platform", reason),
            }
        })
    }

    fn select(&self, caller: &PluginId, input: SelectedInput) -> Result<u64, &'static str> {
        let SelectedInput::Text(text) = input else {
            return Err("This service accepts only selected text");
        };
        if text.is_empty() || text.len() > peersus_clipboard_platform::MAX_TEXT {
            return Err("Invalid selected text size");
        }
        let mut state = self.state()?;
        state
            .selected
            .retain(|_, selection| selection.expires > Instant::now());
        if state.selected.len() >= 4
            || state
                .selected
                .values()
                .any(|selection| selection.caller == *caller)
        {
            return Err("Release the selected input before selecting another");
        }
        let context = crate::platform_services::next_context()?;
        state.selected.insert(
            context,
            Selection {
                caller: caller.clone(),
                text,
                expires: Instant::now() + crate::clipboard_handles::LIFETIME,
            },
        );
        Ok(context)
    }

    fn discard_selection(&self, caller: &PluginId, context: u64) {
        if let Ok(mut state) = self.state.lock()
            && state
                .selected
                .get(&context)
                .is_some_and(|selection| selection.caller == *caller)
        {
            state.selected.remove(&context);
        }
    }

    fn changes(&self, service: &str) -> Option<watch::Receiver<u64>> {
        if service != CLIPBOARD_SERVICE {
            return None;
        }
        let (sender, receiver) = watch::channel(0_u64);
        let mut notices = self.board.notices.subscribe();
        tokio::spawn(async move {
            // Nothing is opened here: the clipboard starts the first time an extension uses it,
            // and until then this only waits. A system that cannot announce copies is looked at
            // twice a second once it is in use, so a watching extension still sees new copies.
            let mut ticks = tokio::time::interval(Duration::from_millis(500));
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                let polling = {
                    let notice = notices.borrow();
                    notice.started && !notice.supported
                };
                tokio::select! {
                    _ = sender.closed() => break,
                    result = notices.changed() => {
                        if result.is_err() { break; }
                        sender.send_modify(|counter| *counter = counter.wrapping_add(1));
                    }
                    _ = ticks.tick(), if polling => {
                        sender.send_modify(|counter| *counter = counter.wrapping_add(1));
                    }
                }
            }
        });
        Some(receiver)
    }

    fn revoke(&self, caller: &PluginId) {
        if let Ok(mut state) = self.state.lock() {
            state.handles.release(caller);
            state
                .selected
                .retain(|_, selection| selection.caller != *caller);
            let generation = state.generations.entry(caller.clone()).or_default();
            *generation = generation.wrapping_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peersus_clipboard_platform::{owner::Store, watch::Notifier};
    use std::sync::Condvar;

    #[tokio::test]
    async fn watching_without_any_use_opens_nothing_and_wakes_nobody() {
        let service = DesktopClipboard::new();
        let mut changes = service.changes(CLIPBOARD_SERVICE).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(1200), changes.changed())
                .await
                .is_err(),
            "nothing is announced, and nothing polls, before an extension uses the clipboard"
        );
        assert!(service.board.owners.lock().await.current.is_none());
        assert!(!service.board.notices.borrow().started);
    }

    /// A gate the test opens once: reads inside the store wait on it like a system's call that
    /// never answers, and it cannot re-close, like a call that finally returned. Waiting is a
    /// real `Condvar` block inside the store — never a lock held across `.await` and never a
    /// sleep assumed to be long enough.
    struct Gate {
        open: Mutex<bool>,
        ready: Condvar,
        /// How many reads have gone inside the store, whether or not they have come out.
        entered: std::sync::atomic::AtomicUsize,
    }
    impl Gate {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                open: Mutex::new(false),
                ready: Condvar::new(),
                entered: std::sync::atomic::AtomicUsize::new(0),
            })
        }
        fn entered(&self) -> usize {
            self.entered.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn wait(&self) {
            self.entered
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut open = self.open.lock().unwrap();
            while !*open {
                open = self.ready.wait(open).unwrap();
            }
        }
        fn free(&self) {
            *self.open.lock().unwrap() = true;
            self.ready.notify_all();
        }
    }

    /// A store whose reads wait on the gate.
    struct Blocked {
        gate: Arc<Gate>,
    }
    impl Store for Blocked {
        fn get(&mut self) -> Option<String> {
            self.gate.wait();
            Some("released".to_owned())
        }
        fn set(&mut self, _text: &str) -> bool {
            true
        }
    }

    /// The owner-thread factories a service has run, each with the `(notifier, ready)`
    /// callbacks it was given: capturing them lets a test answer late, the way a retired
    /// thread still inside the system would.
    type Callbacks = Arc<Mutex<Vec<(Notifier, Notifier)>>>;

    /// A service whose owner threads run a store the test makes, on a deadline short enough
    /// that the timeout path is exercised in milliseconds instead of the real 8 seconds.
    async fn service_with<S, F>(make: F, deadline: Duration) -> (Arc<DesktopClipboard>, Callbacks)
    where
        S: Store + 'static,
        F: Fn() -> Option<S> + Send + Sync + 'static,
    {
        let callbacks: Callbacks = Arc::new(Mutex::new(Vec::new()));
        let service = DesktopClipboard::new();
        let make = Arc::new(make);
        let recorded = callbacks.clone();
        let mut owners = service.board.owners.lock().await;
        owners.deadline = deadline;
        owners.open = Arc::new(move |notifier, ready| {
            recorded.lock().unwrap().push((notifier, ready));
            let make = make.clone();
            Owner::start(move || make())
        });
        drop(owners);
        (service, callbacks)
    }

    /// Wait until this many retired owner threads are still inside the system's clipboard. The
    /// loop is on real thread state (`is_finished`), and the timeout only turns "never happens"
    /// into a failure instead of a hung suite.
    async fn until_stuck(service: &Arc<DesktopClipboard>, expected: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while service.board.stuck().await != expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("expected {expected} retired thread(s) still inside"));
    }

    /// Read until one read has timed out with its owner thread inside the store, and say how
    /// many owners that took.
    ///
    /// A caller can give up before a slow-to-start owner thread has taken its request; that
    /// thread then touches nothing and exits, which is right and leaves nothing stuck. Which of
    /// the two happens is the scheduler's choice, so the test asks instead of assuming: after
    /// each timed-out read it waits for the thread either to be inside the store or to be
    /// gone, and reads again only in the second case.
    async fn time_out_inside(service: &Arc<DesktopClipboard>, gate: &Gate) -> usize {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut owners = 0;
            loop {
                let (inside, stuck) = (gate.entered(), service.board.stuck().await);
                assert!(service.board.read(false).await.text.is_none());
                owners += 1;
                loop {
                    if gate.entered() > inside {
                        return owners;
                    }
                    if service.board.stuck().await == stuck {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            }
        })
        .await
        .expect("an owner thread goes inside the store before its caller gives up")
    }

    /// Join every owner thread the test owns, retired or live: none is left behind.
    async fn join_owners(service: &Arc<DesktopClipboard>) {
        let retired: Vec<Retired> = {
            let mut owners = service.board.owners.lock().await;
            let mut retired: Vec<Retired> = owners.retired.drain(..).collect();
            retired.extend(owners.current.take().map(Owner::retire));
            retired
        };
        for retired in retired {
            retired.wait();
        }
    }

    /// A timeout frees the caller, not the thread: the place stays taken while the retired
    /// thread remains inside the system's clipboard, and only its exit makes a new owner
    /// possible again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_timed_out_owner_frees_its_place_only_when_the_thread_exits() {
        let gate = Gate::new();
        let (service, callbacks) = service_with(
            {
                let gate = gate.clone();
                move || Some(Blocked { gate: gate.clone() })
            },
            Duration::from_millis(20),
        )
        .await;
        let owners = time_out_inside(&service, &gate).await;
        assert_eq!(
            service.board.stuck().await,
            1,
            "the timed-out owner is retired but its thread is still inside the store"
        );
        gate.free();
        until_stuck(&service, 0).await;
        let reading = service.board.read(false).await;
        assert_eq!(reading.text.as_deref(), Some("released"));
        assert_eq!(
            callbacks.lock().unwrap().len(),
            owners + 1,
            "one owner per live thread"
        );
        join_owners(&service).await;
    }

    /// The bound is on threads still inside the clipboard, not on timeouts: two retired and
    /// still-blocked threads refuse a third owner without making one, and capacity returns
    /// only when those threads actually exit.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_still_blocked_threads_are_the_bound_not_two_timeouts() {
        let gate = Gate::new();
        let (service, callbacks) = service_with(
            {
                let gate = gate.clone();
                move || Some(Blocked { gate: gate.clone() })
            },
            Duration::from_millis(20),
        )
        .await;
        let owners =
            time_out_inside(&service, &gate).await + time_out_inside(&service, &gate).await;
        assert_eq!(service.board.stuck().await, MOST_HUNG);
        // Refused before any deadline: while two threads are held inside, no third is made.
        assert!(service.board.read(false).await.text.is_none());
        assert_eq!(
            callbacks.lock().unwrap().len(),
            owners,
            "a refused call must not start another owner thread"
        );
        gate.free();
        until_stuck(&service, 0).await;
        let reading = service.board.read(false).await;
        assert_eq!(reading.text.as_deref(), Some("released"));
        join_owners(&service).await;
    }

    /// A retired owner's readiness and health answers belong to an owner number that no longer
    /// exists: they can neither mark it supported nor unsupport its successor.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_retired_owners_late_answers_are_ignored_forever() {
        let gate = Gate::new();
        let (service, callbacks) = service_with(
            {
                let gate = gate.clone();
                move || Some(Blocked { gate: gate.clone() })
            },
            Duration::from_millis(20),
        )
        .await;
        assert!(service.board.read(false).await.text.is_none());
        let (old_notifier, old_ready) = callbacks.lock().unwrap()[0].clone();
        // The timeout bumped the owner number: answers arriving now are already too late.
        old_ready(true);
        old_notifier(true);
        assert!(!service.board.notices.borrow().supported);
        gate.free();
        until_stuck(&service, 0).await;
        assert!(service.board.read(false).await.text.is_some());
        let (_new_notifier, new_ready) = callbacks.lock().unwrap()[1].clone();
        new_ready(true);
        assert!(service.board.notices.borrow().supported);
        // And the retired thread still cannot speak over its successor, in either direction.
        old_ready(false);
        old_notifier(false);
        assert!(service.board.notices.borrow().supported);
        join_owners(&service).await;
    }

    fn caller(name: &str) -> PluginId {
        PluginId::new(name).unwrap()
    }
    fn selected(caller: &PluginId, context: u64) -> Call {
        Call {
            caller: caller.clone(),
            service: CLIPBOARD_SERVICE.into(),
            method: "selected".into(),
            args: json!({"context":context}),
        }
    }

    #[tokio::test]
    async fn abort_only_unblocks_the_callers_pending_write() {
        let service = DesktopClipboard::new();
        let (one, two) = (caller("one"), caller("two"));
        let call = |caller: &PluginId, method: &str, args| Call {
            caller: caller.clone(),
            service: CLIPBOARD_SERVICE.into(),
            method: method.into(),
            args,
        };
        let first = service
            .execute(call(&one, "write", json!({"bytes":2})))
            .await
            .unwrap();
        service
            .execute(call(&two, "write", json!({"bytes":2})))
            .await
            .unwrap();
        assert!(
            service
                .execute(call(
                    &one,
                    "append",
                    json!({"put":first["put"],"offset":1,"text":"x"})
                ))
                .await
                .is_err()
        );
        let abort = |caller| call(caller, "abort", json!({"put":first["put"]}));
        assert_eq!(
            service.execute(abort(&two)).await,
            Ok(json!({"aborted":false}))
        );
        assert!(
            service
                .execute(call(&one, "write", json!({"bytes":2})))
                .await
                .is_err()
        );
        assert_eq!(
            service.execute(abort(&one)).await,
            Ok(json!({"aborted":true}))
        );
        assert!(
            service
                .execute(call(&one, "write", json!({"bytes":2})))
                .await
                .is_ok()
        );
        assert_eq!(
            service.execute(abort(&one)).await,
            Ok(json!({"aborted":false}))
        );
        assert!(
            service
                .execute(call(&two, "write", json!({"bytes":2})))
                .await
                .is_err()
        );
        assert!(service.board.owners.lock().await.current.is_none());
    }

    #[tokio::test]
    async fn selected_text_is_caller_scoped_single_use_and_never_reads_the_system() {
        let service = DesktopClipboard::new();
        let (one, two) = (caller("one"), caller("two"));
        let context = service
            .select(&one, SelectedInput::Text("selected é🙂".into()))
            .unwrap();
        assert!(service.execute(selected(&two, context)).await.is_err());
        let capture = service.execute(selected(&one, context)).await.unwrap();
        assert!(service.execute(selected(&one, context)).await.is_err());
        let piece = service
            .execute(Call {
                caller: one,
                service: CLIPBOARD_SERVICE.into(),
                method: "chunk".into(),
                args: json!({"clip":capture["clip"],"offset":0}),
            })
            .await
            .unwrap();
        assert_eq!(piece["text"], "selected é🙂");
    }

    #[tokio::test]
    async fn revocation_and_expiry_make_selected_inputs_unusable() {
        let service = DesktopClipboard::new();
        let one = caller("one");
        let context = service
            .select(&one, SelectedInput::Text("first".into()))
            .unwrap();
        service.revoke(&one);
        assert!(service.execute(selected(&one, context)).await.is_err());
        let context = service
            .select(&one, SelectedInput::Text("second".into()))
            .unwrap();
        service
            .state()
            .unwrap()
            .selected
            .get_mut(&context)
            .unwrap()
            .expires = Instant::now();
        assert!(service.execute(selected(&one, context)).await.is_err());
        assert!(
            service.board.owners.lock().await.current.is_none(),
            "selected text never opens the OS clipboard"
        );
    }
}
