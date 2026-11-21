//! The ephemeral request for an extension's key, answered in its settings; shared by every platform.
use peersus_core::{
    model::{ExtensionPrompt, ExtensionSecret},
    plugins::PluginId,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{oneshot, watch};

pub struct Answer {
    pub accepted: bool,
    pub secret: Option<ExtensionSecret>,
    pub remove: bool,
}
struct Pending {
    prompt: ExtensionPrompt,
    answer: oneshot::Sender<Answer>,
}
/// The most prompts waiting for the owner at once, each from a different extension.
const MOST_PENDING: usize = 4;
struct State {
    next: u64,
    pending: BTreeMap<u64, Pending>,
}
pub struct ExtensionPrompts {
    state: Mutex<State>,
    changed: watch::Sender<u64>,
}
impl Default for ExtensionPrompts {
    fn default() -> Self {
        Self {
            state: Mutex::new(State {
                next: 0,
                pending: BTreeMap::new(),
            }),
            changed: watch::channel(0).0,
        }
    }
}
impl ExtensionPrompts {
    pub fn snapshot(&self) -> Vec<ExtensionPrompt> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .values()
            .map(|p| p.prompt.clone())
            .collect()
    }
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }
    pub fn revoke(&self, plugin: &PluginId) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let before = state.pending.len();
        state.pending.retain(|_, p| &p.prompt.plugin != plugin);
        if before != state.pending.len() {
            self.notify();
        }
    }
    fn notify(&self) {
        self.changed.send_modify(|n| *n = n.wrapping_add(1));
    }
    pub fn answer(&self, id: u64, answer: Answer) -> Result<(), &'static str> {
        let pending = self
            .state
            .lock()
            .map_err(|_| "Extension prompts unavailable")?
            .pending
            .remove(&id)
            .ok_or("Extension prompt is no longer active")?;
        self.notify();
        pending
            .answer
            .send(answer)
            .map_err(|_| "Extension prompt is no longer active")
    }
    pub async fn ask(
        self: &Arc<Self>,
        plugin: PluginId,
        origin: String,
    ) -> Result<Answer, &'static str> {
        let (send, receive) = oneshot::channel();
        let id = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "Extension prompts unavailable")?;
            // An extension asks for one thing at a time: without this, one extension could
            // hold every place and no other extension could ask its owner anything.
            if state.pending.values().any(|p| p.prompt.plugin == plugin) {
                return Err("This extension already has a prompt pending");
            }
            if state.pending.len() >= MOST_PENDING {
                return Err("Too many extension prompts are pending");
            }
            state.next = state
                .next
                .checked_add(1)
                .ok_or("Extension prompt identifiers exhausted")?;
            let id = state.next;
            state.pending.insert(
                id,
                Pending {
                    prompt: ExtensionPrompt { id, plugin, origin },
                    answer: send,
                },
            );
            id
        };
        let _guard = PendingGuard {
            registry: self.clone(),
            id,
        };
        self.notify();
        tokio::time::timeout(Duration::from_secs(300), receive)
            .await
            .ok()
            .and_then(Result::ok)
            .ok_or("Authorization was cancelled")
    }
}
struct PendingGuard {
    registry: Arc<ExtensionPrompts>,
    id: u64,
}
impl Drop for PendingGuard {
    fn drop(&mut self) {
        if self
            .registry
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .remove(&self.id)
            .is_some()
        {
            self.registry.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn kind() -> String {
        "https://example.invalid/".into()
    }
    #[tokio::test]
    async fn answers_are_single_use_and_snapshots_never_include_credentials() {
        let registry = Arc::new(ExtensionPrompts::default());
        let mut ask = Box::pin(registry.ask(PluginId::new("test").unwrap(), kind()));
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(ask.as_mut().poll(cx).is_pending()))
                .await
        );
        let prompt = registry.snapshot().pop().unwrap();
        let typed = "synthetic-test-key";
        registry
            .answer(
                prompt.id,
                Answer {
                    accepted: true,
                    secret: Some(typed.to_owned().try_into().unwrap()),
                    remove: false,
                },
            )
            .unwrap();
        let answer = ask.await.unwrap();
        assert_eq!(answer.secret.unwrap().expose(), typed);
        assert!(registry.snapshot().is_empty());
        assert!(
            registry
                .answer(
                    prompt.id,
                    Answer {
                        accepted: false,
                        secret: None,
                        remove: false,
                    }
                )
                .is_err()
        );
    }
    #[tokio::test]
    async fn dropping_pending_futures_removes_prompts_and_releases_bound() {
        let registry = Arc::new(ExtensionPrompts::default());
        let mut pending = Vec::new();
        for name in ["one", "two", "three", "four"] {
            let mut ask = Box::pin(registry.ask(PluginId::new(name).unwrap(), kind()));
            assert!(
                std::future::poll_fn(|cx| std::task::Poll::Ready(
                    ask.as_mut().poll(cx).is_pending()
                ))
                .await
            );
            pending.push(ask);
        }
        assert!(
            registry
                .ask(PluginId::new("five").unwrap(), kind())
                .await
                .is_err()
        );
        assert_eq!(registry.snapshot().len(), 4);
        pending.clear();
        assert!(registry.snapshot().is_empty());
    }
    type Asking<'a> = std::pin::Pin<Box<dyn Future<Output = Result<Answer, &'static str>> + 'a>>;
    fn waiting(ask: &mut Asking<'_>) -> bool {
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        ask.as_mut().poll(&mut cx).is_pending()
    }
    #[tokio::test]
    async fn one_extension_holds_one_prompt_and_the_others_are_still_asked() {
        let registry = Arc::new(ExtensionPrompts::default());
        let mut first: Asking<'_> =
            Box::pin(registry.ask(PluginId::new("greedy").unwrap(), kind()));
        assert!(waiting(&mut first));
        // A second one from the same extension is refused at once, however many are free.
        for _ in 0..4 {
            let mut more: Asking<'_> =
                Box::pin(registry.ask(PluginId::new("greedy").unwrap(), kind()));
            assert!(
                !waiting(&mut more),
                "a second prompt of one extension waits"
            );
        }
        assert_eq!(registry.snapshot().len(), 1);
        // Another extension is asked while the first one's prompt waits.
        let mut other: Asking<'_> = Box::pin(registry.ask(PluginId::new("other").unwrap(), kind()));
        assert!(waiting(&mut other));
        assert_eq!(registry.snapshot().len(), 2);
        // Once its prompt is gone the extension may ask again.
        drop(first);
        let mut again: Asking<'_> =
            Box::pin(registry.ask(PluginId::new("greedy").unwrap(), kind()));
        assert!(waiting(&mut again));
    }
}
