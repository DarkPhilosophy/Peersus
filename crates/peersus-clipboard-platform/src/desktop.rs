//! The native extension and the trusted helper use exactly the same OS adapter.
use crate::{
    Clipboard, Reading,
    owner::{Owner, Store},
    protocol::{self, Answer, Request},
};

struct Desktop {
    board: arboard::Clipboard,
    notifier: Option<crate::watch::Notifier>,
    ready: Option<std::sync::mpsc::SyncSender<bool>>,
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
            let _ = ready.send(supported);
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

/// Run before loading any application identity or network. EOF closes the helper.
/// The OS library's potentially unbounded read stays in this disposable process.
pub fn serve() -> std::io::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    // This binary mode owns stderr exclusively: bytes are metadata, never
    // diagnostics or clipboard contents. A disposable process contains the
    // system listeners' otherwise uninterruptible blocking OS waits.
    let (ready, readiness) = std::sync::mpsc::sync_channel(1);
    let (changed, changes) = std::sync::mpsc::sync_channel(1);
    let failed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let notifier: crate::watch::Notifier = {
        let failed = failed.clone();
        std::sync::Arc::new(move |healthy| {
            if !healthy {
                failed.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            let _ = changed.try_send(());
        })
    };
    std::thread::spawn(move || {
        use std::io::Write;
        let mut output = std::io::stderr().lock();
        let supported =
            readiness.recv().unwrap_or(false) && !failed.load(std::sync::atomic::Ordering::Relaxed);
        if output
            .write_all(&[u8::from(supported)])
            .and_then(|_| output.flush())
            .is_err()
        {
            return;
        }
        while changes.recv().is_ok() {
            let byte = if failed.load(std::sync::atomic::Ordering::Relaxed) {
                0
            } else {
                2
            };
            if output
                .write_all(&[byte])
                .and_then(|_| output.flush())
                .is_err()
            {
                return;
            }
        }
        let _ = output.write_all(&[0]).and_then(|_| output.flush());
    });
    let mut board = Owner::start(move || {
        arboard::Clipboard::new().ok().map(|board| Desktop {
            board,
            notifier: Some(notifier),
            ready: Some(ready),
        })
    });
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    while let Some(request) = protocol::read::<_, Request>(&mut input)? {
        let answer = runtime.block_on(async {
            match request {
                Request::Ready => Answer {
                    ok: true,
                    text: None,
                    changed: false,
                },
                Request::Read { observe } => {
                    let reading = if observe {
                        board.observe().await
                    } else {
                        board
                            .text()
                            .await
                            .map_or(Reading::Unavailable, Reading::Snapshot)
                    };
                    let (text, changed) = match reading {
                        Reading::Unavailable => (None, false),
                        Reading::Snapshot(text) => (Some(text), false),
                        Reading::Changed(text) => (Some(text), true),
                    };
                    Answer {
                        ok: true,
                        text,
                        changed,
                    }
                }
                Request::Write { text } => Answer {
                    ok: text.len() <= crate::MAX_TEXT && board.set_text(&text).await,
                    text: None,
                    changed: false,
                },
                Request::Reset => Answer {
                    ok: board.reset_observation().await,
                    text: None,
                    changed: false,
                },
            }
        });
        protocol::write(&mut output, &answer)?;
    }
    Ok(())
}
