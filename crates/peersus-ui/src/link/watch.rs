//! What the window waits on from outside its own tasks: the platform's usable area and theme, a clock, a delay, and what a picker,
//! a folder chooser or a camera hands over. Each runs on a plain thread of its own: the interface's tasks have no timer and no
//! runtime.
use super::*;

/// Where the platform says the application's usable area is, in physical pixels as
/// `(left, top, right, bottom)`. Asked again and again, because a platform has no answer until its window
/// has been laid out and the answer changes when its bars come and go. All zero means "not known yet".
pub type SafeAreaSource = Arc<dyn Fn() -> (i32, i32, i32, i32) + Send + Sync>;

/// Ask `source` for the usable area every `every` and publish it on the returned channel when it changes.
///
/// This runs on a plain thread of its own and not on the interface's tasks: those have no timer and no
/// runtime, and a timer made there panics, which is how an earlier version of this crashed on the phone the
/// moment it opened. The interface only waits on the channel, as it does for the owner's snapshots. The
/// thread ends when every receiver has been dropped, that is when the window has closed.
pub fn watch_safe_area(
    source: SafeAreaSource,
    every: std::time::Duration,
) -> tokio::sync::watch::Receiver<(i32, i32, i32, i32)> {
    poll_changes("safe-area", every, move || source())
}

/// Whether the system is in its dark mode now: `Some(true)` for dark, `Some(false)` for light, `None` when the platform cannot say.
/// Asked at once for the first frame and again and again for a change of the system's choice, as the usable area is.
pub type DarkSource = Arc<dyn Fn() -> Option<bool> + Send + Sync>;

/// Ask `source` whether the system is dark every `every` and publish it on the returned channel when it changes. The first answer is
/// there at once, so that the very first frame is drawn in the right theme and not in the toolkit's default one.
///
/// A plain thread of its own for the reason [`watch_safe_area`] is, ending when every receiver has been dropped.
pub fn watch_dark(
    source: DarkSource,
    every: std::time::Duration,
) -> tokio::sync::watch::Receiver<Option<bool>> {
    poll_changes("dark-mode", every, move || source())
}

/// The second now, as Unix seconds, from `source`, published on the returned channel each time it changes.
///
/// Every countdown on screen (the time left of an offer, in the bar over the window and in the circle of the device that sent
/// it) reads this one channel, so that none of them has a timer of its own and no thread is made per card. It is a plain thread
/// for the reason `watch_safe_area` is: the interface's tasks have no timer. `every` bounds how late a new second is seen, and
/// the thread ends when every receiver is gone. Only a CHANGED second is published, so a screen is woken once a second, not
/// as often as the thread looks.
pub fn clock(
    source: impl Fn() -> u64 + Send + 'static,
    every: std::time::Duration,
) -> tokio::sync::watch::Receiver<u64> {
    poll_changes("clock", every, source)
}

/// Read the initial value on the caller's thread. Later platform reads (including JNI) must
/// not hold the channel's write lock: the window can still borrow the previous value.
fn poll_changes<T: PartialEq + Send + Sync + 'static>(
    name: &'static str,
    every: std::time::Duration,
    source: impl Fn() -> T + Send + 'static,
) -> tokio::sync::watch::Receiver<T> {
    let (sender, receiver) = tokio::sync::watch::channel(source());
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            loop {
                std::thread::sleep(every);
                if sender.is_closed() {
                    break;
                }
                let now = source();
                sender.send_if_modified(|last| {
                    let changed = *last != now;
                    *last = now;
                    changed
                });
            }
        })
        .ok();
    receiver
}

/// Tell the interface when `after` has passed, once, on the returned channel.
///
/// The interface has no timer (see [`watch_safe_area`]), so the waiting is on a plain thread of its own. The thread sleeps
/// until the deadline and nothing wakes it before, except the channel itself the moment nobody waits on it any more: a code
/// that was replaced, a page that was left or a window that closed ends its thread at once, and does not leave it asleep
/// until a deadline nobody will hear of.
pub fn after(after: std::time::Duration) -> tokio::sync::oneshot::Receiver<()> {
    /// Wakes the sleeping thread when the channel says its receiver is gone.
    struct Unpark(std::thread::Thread);
    impl std::task::Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let (mut sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("deadline".to_owned())
        .spawn(move || {
            let waker = std::task::Waker::from(Arc::new(Unpark(std::thread::current())));
            let mut context = std::task::Context::from_waker(&waker);
            let until = std::time::Instant::now() + after;
            loop {
                // Asked before every sleep: the question is also what leaves the way to be woken with the channel.
                if sender.poll_closed(&mut context).is_ready() {
                    break;
                }
                let left = until.saturating_duration_since(std::time::Instant::now());
                if left.is_zero() {
                    let _ = sender.send(());
                    break;
                }
                // A sleep may end early for no reason: the loop asks again and sleeps what is left.
                std::thread::park_timeout(left);
            }
        })
        .ok();
    receiver
}

/// Wait on a plain thread of its own for what a platform's picker hands over, and publish each batch on the returned
/// channel, until the picker says it is done or the interface has gone.
///
/// The answers come on a thread that is not the interface's, at a moment nobody can predict, and the interface's
/// tasks have no timer to wait with, so the waiting is done here, as `watch_safe_area` does it, and the interface
/// only waits on the channel. Every batch is sent once; the last one is the one that says the picker is closed.
/// A picker that never says it is done, because its platform side vanished, is given up on after `patience`, so that
/// no thread waits for ever.
pub fn await_picked(
    picked: peersus_net::picked::Picked,
    every: std::time::Duration,
    patience: std::time::Duration,
) -> Batches {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let thread_picked = picked.clone();
    let generation = picked.generation();
    std::thread::Builder::new()
        .name("picker".to_owned())
        .spawn(move || {
            let picked = thread_picked;
            let started = std::time::Instant::now();
            let mut count_seen = None;
            loop {
                std::thread::sleep(every);
                let gave_up = started.elapsed() >= patience;
                if gave_up {
                    picked.finished_for(generation);
                }
                let taken = picked.take_for(generation);
                let count = picked.selected_count(generation);
                let declared = count != count_seen;
                count_seen = count;
                let finished = !taken.still_open;
                if (!taken.files.is_empty() || taken.refused > 0 || declared || finished || gave_up)
                    && let Err(undelivered) = sender.send(taken)
                {
                    // Nobody reads: what was taken is let go of here, the rest was the reader's, and is let go of with it.
                    picked.release(&undelivered.0.files);
                    break;
                }
                if finished || gave_up {
                    break;
                }
                if sender.is_closed() {
                    break;
                }
            }
        })
        .ok();
    Batches {
        receiver,
        picked,
        generation,
    }
}

/// Wait for the person to answer the platform's folder picker FOR `transfer`, and tell the interface once, on the returned channel.
///
/// Like [`await_picked`]: the answer comes on a thread that is not the interface's, at a moment nobody can predict, and the
/// interface has no timer, so the looking is done here and the interface only waits on the channel. Only the answer made for
/// `transfer` is taken (see `FolderPicked::take_for`). Looks every `every`; a picker whose platform side vanished is given up on
/// after `patience`, answered as backing out and its question let go of, so that no thread waits for ever and no later question is
/// blocked by it. The thread ends when nobody waits on the channel, and lets go of the question then too.
pub fn await_folder(
    picked: peersus_net::folder_picked::FolderPicked,
    transfer: String,
    every: std::time::Duration,
    patience: std::time::Duration,
) -> tokio::sync::oneshot::Receiver<Option<String>> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("folder-picker".to_owned())
        .spawn(move || {
            let mut sender = Some(sender);
            let started = std::time::Instant::now();
            loop {
                std::thread::sleep(every);
                let answer = match picked.take_for(&transfer) {
                    peersus_net::folder_picked::Taken::Answered { folder, .. } => Some(folder),
                    peersus_net::folder_picked::Taken::Waiting if started.elapsed() >= patience => {
                        picked.abandon(&transfer);
                        Some(None)
                    }
                    peersus_net::folder_picked::Taken::Waiting => None,
                };
                if let Some(answer) = answer {
                    if let Some(sender) = sender.take() {
                        let _ = sender.send(answer);
                    }
                    break;
                }
                if sender
                    .as_ref()
                    .is_some_and(tokio::sync::oneshot::Sender::is_closed)
                {
                    picked.abandon(&transfer);
                    break;
                }
            }
        })
        .ok();
    receiver
}

/// What a picker handed over, as a reader takes it. A batch is the reader's from the moment it is read. What was sent and never
/// read is the receiver's, and is let go of when the receiver is dropped: that is the page being left, and it holds whether the
/// picker had finished or not and whether the thread had ended or not, which a thread watching the channel cannot promise.
pub struct Batches {
    receiver: tokio::sync::mpsc::UnboundedReceiver<peersus_net::picked::Taken>,
    pub(super) picked: peersus_net::picked::Picked,
    pub(super) generation: u64,
}

impl Batches {
    /// The next batch, or `None` when the picker is done and everything was read.
    pub async fn recv(&mut self) -> Option<peersus_net::picked::Taken> {
        self.receiver.recv().await
    }

    /// The next batch without waiting, for a thread that has no runtime.
    #[cfg(test)]
    pub fn blocking_recv(&mut self) -> Option<peersus_net::picked::Taken> {
        self.receiver.blocking_recv()
    }
}

impl Drop for Batches {
    fn drop(&mut self) {
        // Closing first, so that nothing more is sent behind the draining; then every batch that was never read.
        self.receiver.close();
        self.picked.finished_for(self.generation);
        let pending = self.picked.take_for(self.generation);
        self.picked.release(&pending.files);
        while let Ok(unread) = self.receiver.try_recv() {
            self.picked.release(&unread.files);
        }
    }
}

/// The files an Android picker handed over and that wait to be offered under a code: kept by number in the registry. They are
/// let go of when this ends, whatever ended it (a code made, a refusal, the page left, the wait given up on), so that a batch
/// the owner did not take never stays open. A number the owner already took is nothing to let go of.
pub struct Gathered {
    pub(super) picked: peersus_net::picked::Picked,
    pub(super) files: Vec<(u64, String)>,
}

impl Gathered {
    pub fn new(picked: peersus_net::picked::Picked) -> Self {
        Self {
            picked,
            files: Vec::new(),
        }
    }

    pub fn add(&mut self, files: Vec<(u64, String)>) {
        self.files.extend(files);
    }

    /// What is gathered, to be offered; `None` when nothing is.
    pub fn files(&self) -> Option<Vec<(u64, String)>> {
        (!self.files.is_empty()).then(|| self.files.clone())
    }

    /// The names the files go by, in the order they were gathered: what the page lists. Never where they were, and never their numbers.
    pub fn names(&self) -> Vec<String> {
        self.files.iter().map(|(_, name)| name.clone()).collect()
    }

    /// Move what is gathered out, leaving this EMPTY: what it held is then the caller's to hand over, and this lets go of nothing when it
    /// ends. Letting go twice of numbers the owner already took could revoke a send that is under way.
    pub fn take_files(&mut self) -> Vec<(u64, String)> {
        std::mem::take(&mut self.files)
    }

    /// The file at `index` is taken away and let go of in the registry, once. An index that is not there changes nothing: the list may have changed
    /// between the touch and the press. Returns whether a file was taken away.
    pub fn remove(&mut self, index: usize) -> bool {
        if index >= self.files.len() {
            return false;
        }
        let gone = self.files.remove(index);
        self.picked.release(std::slice::from_ref(&gone));
        true
    }

    /// Everything gathered is let go of in the registry, once, and the list is empty.
    pub fn clear(&mut self) {
        let everything = std::mem::take(&mut self.files);
        self.picked.release(&everything);
    }

    /// Take everything `other` holds into this guard, after what this one holds, and leave `other` EMPTY so that ending it lets go of nothing: the
    /// numbers are held once, by this guard. For a second selection that adds to the first instead of replacing it.
    pub fn absorb(&mut self, mut other: Gathered) {
        self.files.extend(other.take_files());
    }

    /// A second set of the same files, each from a handle of its own (see [`peersus_net::picked::Picked::reopened`]), in a guard of its own:
    /// what a page keeps while the first set waits behind a code, so that the files can be listed again if the code is taken back. `None`
    /// when the registry has no room for it or a file cannot be opened again; nothing is then kept.
    pub fn spare(&self) -> Option<Gathered> {
        let files = self.picked.reopened(&self.files).ok()?;
        Some(Self {
            picked: self.picked.clone(),
            files,
        })
    }
}

impl Drop for Gathered {
    fn drop(&mut self) {
        self.picked.release(&self.files);
    }
}

/// Watch a scan on a plain thread of its own, and publish what each look at the camera makes of the state, until the scan
/// is over, the camera was refused, or `patience` has run out.
///
/// Like `await_picked`, it is on its own thread because the interface's tasks have no timer to wait with. What each
/// look means is [`crate::view::look`], which is tried without a phone: this only asks it, and passes on what it says.
/// Only a look that changes something is published (a code read, a refusal, the end), so that the interface is not woken
/// ten times a second to learn that nothing happened. A scan that is never answered is given up on after `patience`,
/// and closed, so that no camera stays open for ever with nobody looking at it.
pub fn await_scan(
    scanner: crate::scanner::Scanner,
    camera: CameraControl,
    every: std::time::Duration,
    patience: std::time::Duration,
) -> tokio::sync::mpsc::UnboundedReceiver<crate::view::Tick> {
    await_scan_shown(scanner, camera, every, patience, 0).0
}

/// [`await_scan`], and with it what the circle is to show of the camera: the picture of what it sees, with the codes it
/// has located marked on it, `side` pixels across, as a second channel.
///
/// The picture is made here, on the watcher's own thread, and never on the interface's: it costs a pass over the frame.
/// One is made only when there is a frame newer than the last made, so a camera that has stopped giving frames costs
/// nothing, and a picture is not made twice for one frame. Only the latest picture is kept for the reader.
pub fn await_scan_shown(
    scanner: crate::scanner::Scanner,
    camera: CameraControl,
    every: std::time::Duration,
    patience: std::time::Duration,
    side: u32,
) -> (
    tokio::sync::mpsc::UnboundedReceiver<crate::view::Tick>,
    tokio::sync::watch::Receiver<Option<Arc<crate::preview::Shown>>>,
) {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    // A watch and not a queue: only the latest picture matters, and a reader that is slow must never be handed a pile of
    // pictures a megabyte each to go through; it finds the newest one, and the ones between are gone.
    let (shown_sender, shown_receiver) = tokio::sync::watch::channel(None);
    std::thread::Builder::new()
        .name("scan".to_owned())
        .spawn(move || {
            let started = std::time::Instant::now();
            let mut looking = crate::view::Looking::Scanning;
            let mut last_shown = 0_u64;
            // The picture is made often, about thirty times a second, because that is what keeps what the person sees up with where the camera
            // is; reading for a code and looking at the state of the scan stay at `every`, which is all they need and what they cost.
            let picture_every = std::time::Duration::from_millis(33).min(every);
            let mut last_look = std::time::Instant::now();
            loop {
                std::thread::sleep(picture_every);
                if side > 0
                    && let Some(view) = scanner.view_after(last_shown)
                {
                    last_shown = view.serial;
                    if let Some(shown) = crate::preview::show(&view, side, (camera.turns)()) {
                        // A picture nobody is left to see is not a reason to stop: the ticks decide that.
                        shown_sender.send_replace(Some(Arc::new(shown)));
                    }
                }
                if last_look.elapsed() < every {
                    continue;
                }
                last_look = std::time::Instant::now();
                let tick = crate::view::look(looking, scanner.take(), (camera.refused)());
                let changed = tick.looking != looking || tick.join.is_some() || tick.close;
                looking = tick.looking;
                let over =
                    tick.close || tick.looking == crate::view::Looking::Shut || scanner.is_over();
                let gave_up = started.elapsed() >= patience;
                if changed && sender.send(tick).is_err() {
                    // The interface is gone: the camera is closed, since nobody is left to read what it sees.
                    (camera.close)();
                    break;
                }
                if over || gave_up {
                    // Every end closes the camera, here and once: a code read, a refusal, or a wait given up on. The
                    // page's own closing on leaving is the second line, for a page left before any of these.
                    (camera.close)();
                    if gave_up && !over {
                        let _ = sender.send(crate::view::Tick {
                            looking: crate::view::Looking::Shut,
                            join: None,
                            close: true,
                        });
                    }
                    break;
                }
            }
        })
        .ok();
    (receiver, shown_receiver)
}
