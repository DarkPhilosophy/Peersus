//! System clipboard access shared by native extensions and the application's clipboard service.
#![forbid(unsafe_code)]
use std::future::Future;
#[cfg(not(target_os = "android"))]
pub mod desktop;
pub mod owner;
pub mod watch;
/// Maximum UTF-8 clipboard text carried by either execution format.
pub const MAX_TEXT: usize = 1024 * 1024;

/// Polling platforms expose snapshots; event-driven platforms can prove a fresh local
/// copy occurred after focus/permission was gained. Sensitive text is always Unavailable.
pub enum Reading {
    Unavailable,
    Snapshot(String),
    Changed(String),
}
/// The device's clipboard, as far as this plugin uses it.
///
/// Asynchronous because the real one is reached through a thread of its own: a caller waits
/// for the answer without blocking the connection to the application.
pub trait Clipboard: Send {
    /// None includes unavailable, unfocused or sensitive text; it is never cached as readable.
    fn text(&mut self) -> impl Future<Output = Option<String>> + Send;
    /// Consume an exact operation-scoped selection. Never fall back to the system clipboard.
    fn selected_text(&mut self, _context: u64) -> impl Future<Output = Option<String>> + Send {
        async { None }
    }
    /// Whether a captured selected input still belongs to a live, unexpired operation.
    fn selected_valid(&mut self, _context: u64) -> impl Future<Output = bool> + Send {
        async { false }
    }
    /// Recheck a capture immediately before its irreversible outgoing write.
    fn can_share(&mut self) -> impl Future<Output = bool> + Send {
        async { true }
    }
    /// Discard platform events from an earlier eligibility epoch and stop listening.
    /// False blocks automatic observation and sends until a later reset succeeds.
    fn reset_observation(&mut self) -> impl Future<Output = bool> + Send {
        async { true }
    }
    /// Put text on the clipboard. Android providers must mark received text sensitive.
    fn set_text(&mut self, text: &str) -> impl Future<Output = bool> + Send;
    /// The deadline belongs to the whole receive, not a new platform timeout. Providers
    /// that queue non-cancellable work must revoke its permit when this future is dropped.
    fn set_text_until(
        &mut self,
        text: &str,
        deadline: std::time::Instant,
    ) -> impl Future<Output = bool> + Send {
        async move {
            if std::time::Instant::now() >= deadline {
                return false;
            }
            self.set_text(text).await
        }
    }
    /// Override only when the platform can distinguish a new local copy from old contents.
    fn observe(&mut self) -> impl Future<Output = Reading> + Send {
        async {
            self.text()
                .await
                .map_or(Reading::Unavailable, Reading::Snapshot)
        }
    }
}
