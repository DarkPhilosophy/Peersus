//! The native extension and the application's clipboard service use exactly the same OS adapter.
use crate::{
    owner::{Owner, Store},
    watch::Notifier,
};

struct Desktop {
    board: arboard::Clipboard,
    notifier: Option<crate::watch::Notifier>,
    /// Answered once, on this owner thread, with whether copy announcements will arrive. It is
    /// a call made from here, not a channel another thread waits on, so a clipboard that blocks
    /// while opening or listening cannot leave a second thread parked on the answer forever.
    ready: Option<crate::watch::Notifier>,
}
impl Store for Desktop {
    fn get(&mut self) -> Option<String> {
        self.board.get_text().ok()
    }
    fn set(&mut self, text: &str) -> bool {
        self.board.set_text(text).is_ok()
    }
    fn listen(&mut self, signals: std::sync::mpsc::SyncSender<crate::watch::Signal>) -> bool {
        let supported = crate::watch::start_with_notifier(signals, self.notifier.take());
        if let Some(ready) = self.ready.take() {
            ready(supported);
        }
        supported
    }
}

pub fn open() -> Owner {
    Owner::start(|| {
        arboard::Clipboard::new().ok().map(|board| Desktop {
            board,
            notifier: None,
            ready: None,
        })
    })
}

/// The clipboard as the application's own service uses it: on the owner's thread, inside the
/// application. `notifier` hears of every announced copy (and of the listener failing), and
/// `ready` once whether this system can announce copies at all.
pub fn open_watched(notifier: Notifier, ready: Notifier) -> Owner {
    Owner::start(move || {
        arboard::Clipboard::new().ok().map(|board| Desktop {
            board,
            notifier: Some(notifier),
            ready: Some(ready),
        })
    })
}
