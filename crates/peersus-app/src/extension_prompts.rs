//! Ephemeral owner-mediated extension consent, shared by every platform.
use peersus_core::{
    model::{ExtensionPrompt, ExtensionPromptKind, ExtensionSecret},
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
        kind: ExtensionPromptKind,
    ) -> Result<Answer, &'static str> {
        let (send, receive) = oneshot::channel();
        let id = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "Extension prompts unavailable")?;
            if state.pending.len() >= 4 {
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
                    prompt: ExtensionPrompt { id, plugin, kind },
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
    fn kind() -> ExtensionPromptKind {
        ExtensionPromptKind::Configure {
            origin: "https://example.invalid/".into(),
        }
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
        let secret = "synthetic-test-key";
        registry
            .answer(
                prompt.id,
                Answer {
                    accepted: true,
                    secret: Some(secret.to_owned().try_into().unwrap()),
                    remove: false,
                },
            )
            .unwrap();
        let answer = ask.await.unwrap();
        assert_eq!(answer.secret.unwrap().expose(), secret);
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
        for _ in 0..4 {
            let mut ask = Box::pin(registry.ask(PluginId::new("test").unwrap(), kind()));
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
                .ask(PluginId::new("test").unwrap(), kind())
                .await
                .is_err()
        );
        assert_eq!(registry.snapshot().len(), 4);
        pending.clear();
        assert!(registry.snapshot().is_empty());
    }
}
