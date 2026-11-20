//! Back: the platform's way of going one step back (Android's Back button and gesture). What is open on top of the window
//! closes first, the newest first: a list, a menu, the Settings sheet, an extension's licence; with nothing open, the platform
//! does what its Back does (the activity goes behind the others). A platform without Back gives the window none of this.
use freya::prelude::*;
use std::sync::atomic::{AtomicU64, Ordering};

/// What is open on top of the window and can be closed by Back, the newest last. Given by the root when the platform has a
/// Back; read only when Back is pressed, never while drawing.
#[derive(Clone, Copy)]
pub(crate) struct BackStack(pub(crate) State<Vec<(u64, EventHandler<()>)>>);

impl BackStack {
    /// What Back does now: closes the newest open thing and says so, or says that nothing was open.
    pub(crate) fn close_top(mut self) -> bool {
        let top = self.0.write().pop();
        match top {
            Some((_, close)) => {
                close.call(());
                true
            }
            None => false,
        }
    }
}

/// Each use of [`use_back`] gets its own number: what it put on the stack is taken off by that number.
static NEXT: AtomicU64 = AtomicU64::new(1);

/// While `open`, Back calls `close` (the newest open thing first). When it closes some other way, or goes, it is taken off.
/// Does nothing where the window was given no stack: a platform without Back.
pub(crate) fn use_back(open: bool, close: EventHandler<()>) {
    let stack = try_consume_context::<BackStack>();
    let id = use_hook(|| NEXT.fetch_add(1, Ordering::Relaxed));
    let mut was_open = use_state(|| false);
    if let Some(BackStack(mut entries)) = stack
        && open != *was_open.peek()
    {
        was_open.set(open);
        let mut entries = entries.write();
        entries.retain(|(entry, _)| *entry != id);
        if open {
            entries.push((id, close));
        }
    }
    use_drop(move || {
        if let Some(BackStack(mut entries)) = stack {
            entries.write().retain(|(entry, _)| *entry != id);
        }
    });
}
