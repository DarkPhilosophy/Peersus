//! Back: the platform's way of going one step back (Android's Back button and gesture, Escape on a desktop). What is open on top
//! of the window closes first, the newest first: a list, a menu, the Settings sheet, an extension's licence. Then a page that takes
//! Back itself (an extension's box observing `back`) is told it. With nothing of either, the root goes back in the application.
use freya::prelude::*;
use std::sync::atomic::{AtomicU64, Ordering};

/// One thing Back can reach: by its number, what it does, and whether it stays after Back reached it.
type Entry = (u64, EventHandler<()>, bool);

/// What is open on top of the window and can be closed by Back, the newest last, with what a page takes Back for. Given by the
/// root; read only when Back is pressed, never while drawing.
#[derive(Clone, Copy)]
pub(crate) struct BackStack(pub(crate) State<Vec<Entry>>);

impl BackStack {
    /// What Back does now: closes the newest open thing, or else tells the newest page that takes Back, and says so; or says that
    /// nothing was there. What the application opened always comes before a page under it, whichever appeared first.
    pub(crate) fn close_top(mut self) -> bool {
        let reached = {
            let mut entries = self.0.write();
            match entries.iter().rposition(|(_, _, kept)| !kept) {
                Some(at) => Some(entries.remove(at).1),
                None => entries.last().map(|(_, reach, _)| reach.clone()),
            }
        };
        match reached {
            Some(reach) => {
                reach.call(());
                true
            }
            None => false,
        }
    }
}

/// Each use of [`use_back`] gets its own number: what it put on the stack is taken off by that number.
static NEXT: AtomicU64 = AtomicU64::new(1);

/// While `open`, Back calls `close` (the newest open thing first). When it closes some other way, or goes, it is taken off.
pub(crate) fn use_back(open: bool, close: EventHandler<()>) {
    use_entry(open, close, false);
}

/// While `taken`, Back reaches `reach` once nothing the application opened is left to close, and keeps reaching it at every
/// press until it is no longer taken: a page that goes back one level at a time (a conversation, then the list of them).
pub(crate) fn use_page_back(taken: bool, reach: EventHandler<()>) {
    use_entry(taken, reach, true);
}

fn use_entry(open: bool, close: EventHandler<()>, kept: bool) {
    let stack = try_consume_context::<BackStack>();
    let id = use_hook(|| NEXT.fetch_add(1, Ordering::Relaxed));
    let mut was_open = use_state(|| false);
    if let Some(BackStack(mut entries)) = stack
        && open != *was_open.peek()
    {
        was_open.set(open);
        let mut entries = entries.write();
        entries.retain(|(entry, _, _)| *entry != id);
        if open {
            entries.push((id, close, kept));
        }
    }
    use_drop(move || {
        if let Some(BackStack(mut entries)) = stack {
            entries.write().retain(|(entry, _, _)| *entry != id);
        }
    });
}
