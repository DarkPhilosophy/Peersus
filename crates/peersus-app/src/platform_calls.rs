//! Deferred platform work belongs to the owner, never its dispatch thread.
//!
//! A broker cancellation drops the platform future. Implementations must retain their
//! picker/consent cancellation guards in that future, not detach unowned work.
use peersus_api::{message::Outcome, permission::Permission};
use peersus_core::plugins::PluginId;
use std::{collections::HashMap, future::Future};
use tokio::task::{AbortHandle, Id, JoinSet};

const MAX_CALLS: usize = 32;
const MAX_CALLS_PER_PLUGIN: usize = 4;

struct Pending {
    ticket: u64,
    caller: PluginId,
    guard: Option<Permission>,
    revoked: Option<Permission>,
    abort: AbortHandle,
    cancelled: bool,
    started: std::time::Instant,
}

/// A finished task, including panic recovery. Cancelled work produces no reply.
pub(crate) struct Completion {
    pub(crate) ticket: u64,
    pub(crate) result: Option<Outcome>,
}

#[derive(Default)]
pub(crate) struct PlatformCalls {
    tasks: JoinSet<Outcome>,
    pending: HashMap<Id, Pending>,
}

impl PlatformCalls {
    pub(crate) fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Build the future only after admitting the call. A refusal starts no work.
    /// Cancelled tasks still occupy a slot until joined, bounding abort storms too.
    pub(crate) fn start<F, Fut>(
        &mut self,
        ticket: u64,
        caller: PluginId,
        guard: Option<Permission>,
        make: F,
    ) -> Result<(), &'static str>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Outcome> + Send + 'static,
    {
        if self.pending.values().any(|call| call.ticket == ticket) {
            return Err("A platform call already owns this ticket");
        }
        if self.pending.len() >= MAX_CALLS
            || self
                .pending
                .values()
                .filter(|call| call.caller == caller)
                .count()
                >= MAX_CALLS_PER_PLUGIN
        {
            return Err("Too many platform requests are already running");
        }
        // Invoke inside the task: a platform adapter panic cannot unwind the owner.
        let abort = self.tasks.spawn(async move {
            peersus_diagnostics::dev_trace!(
                stage = "platform-start",
                ticket,
                "Platform operation started"
            );
            let result = make().await;
            peersus_diagnostics::dev_trace!(
                stage = "platform-result",
                ticket,
                outcome = peersus_diagnostics::outcome(&result),
                "Platform operation completed"
            );
            result
        });
        self.pending.insert(
            abort.id(),
            Pending {
                ticket,
                caller,
                guard,
                revoked: None,
                abort,
                cancelled: false,
                started: std::time::Instant::now(),
            },
        );
        Ok(())
    }

    pub(crate) fn cancel(&mut self, ticket: u64) {
        if let Some(call) = self.pending.values_mut().find(|call| call.ticket == ticket) {
            call.cancelled = true;
            peersus_diagnostics::dev_trace!(
                stage = "platform-cancel",
                ticket,
                "Platform operation cancelled"
            );
            call.abort.abort();
        }
    }

    pub(crate) fn revoke(&mut self, caller: &PluginId) {
        for call in self
            .pending
            .values_mut()
            .filter(|call| &call.caller == caller)
        {
            call.cancelled = true;
            call.abort.abort();
        }
    }

    pub(crate) fn revoke_permission(&mut self, caller: &PluginId, permission: &Permission) {
        for call in self.pending.values_mut().filter(|call| {
            &call.caller == caller
                && call.guard.as_ref().is_none_or(|guard| guard == permission)
                && !call.cancelled
        }) {
            call.cancelled = true;
            call.revoked = Some(permission.clone());
            call.abort.abort();
        }
    }

    pub(crate) async fn next(&mut self) -> Option<Completion> {
        let joined = self.tasks.join_next_with_id().await?;
        let (id, outcome) = match joined {
            Ok((id, outcome)) => (id, Some(outcome)),
            Err(error) => (
                error.id(),
                (!error.is_cancelled()).then(|| {
                    Outcome::error(
                        "unavailable",
                        "The platform request failed; no automatic retry was attempted",
                    )
                }),
            ),
        };
        let call = self.pending.remove(&id)?;
        peersus_diagnostics::dev_trace!(
            stage = "platform-end",
            ticket = call.ticket,
            cancelled = call.cancelled,
            elapsed_ms = call.started.elapsed().as_millis() as u64,
            "Platform task joined"
        );
        Some(Completion {
            ticket: call.ticket,
            result: if call.cancelled {
                call.revoked
                    .map(|permission| Outcome::MissingPermission { permission })
            } else {
                outcome
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use tokio::sync::oneshot;

    fn plugin(name: &str) -> PluginId {
        PluginId::new(name).unwrap()
    }

    #[tokio::test]
    async fn pending_platform_work_does_not_block_another_completion() {
        let mut calls = PlatformCalls::default();
        let (_release, wait) = oneshot::channel::<()>();
        calls
            .start(1, plugin("first"), None, || async move {
                let _ = wait.await;
                Outcome::ok(serde_json::json!({}))
            })
            .unwrap();
        calls
            .start(2, plugin("second"), None, || async {
                Outcome::ok(serde_json::json!({"done": true}))
            })
            .unwrap();
        let done = tokio::time::timeout(std::time::Duration::from_secs(1), calls.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(done.ticket, 2);
        assert!(done.result.is_some());
        calls.cancel(1);
        assert!(calls.next().await.unwrap().result.is_none());
        assert!(calls.is_empty());
    }

    #[tokio::test]
    async fn cancellation_drops_live_work_and_never_reports_a_late_success() {
        struct Guard(Arc<AtomicBool>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let guard = Guard(Arc::clone(&dropped));
        let (started, ready) = oneshot::channel();
        let mut calls = PlatformCalls::default();
        calls
            .start(7, plugin("upload"), None, || async move {
                let _guard = guard;
                let _ = started.send(());
                std::future::pending::<Outcome>().await
            })
            .unwrap();
        ready.await.unwrap();
        calls.cancel(7);
        let done = calls.next().await.unwrap();
        assert_eq!(done.ticket, 7);
        assert!(done.result.is_none());
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn revocation_is_scoped_to_the_callers_work() {
        let mut calls = PlatformCalls::default();
        calls
            .start(1, plugin("revoked"), None, std::future::pending)
            .unwrap();
        calls
            .start(2, plugin("retained"), None, || async {
                Outcome::ok(serde_json::json!({}))
            })
            .unwrap();
        calls.revoke(&plugin("revoked"));
        let mut answers = Vec::new();
        while let Some(done) = calls.next().await {
            answers.push((done.ticket, done.result.is_some()));
        }
        answers.sort();
        assert_eq!(answers, [(1, false), (2, true)]);
    }

    #[tokio::test]
    async fn panic_produces_one_failure_and_releases_its_slot() {
        let mut calls = PlatformCalls::default();
        calls
            .start(4, plugin("broken"), None, || async {
                panic!("test adapter failed")
            })
            .unwrap();
        let done = calls.next().await.unwrap();
        assert_eq!(done.ticket, 4);
        assert!(matches!(done.result, Some(Outcome::Error { .. })));
        assert!(calls.is_empty());
        assert!(calls.pending.is_empty());
    }

    #[tokio::test]
    async fn refusal_never_constructs_platform_work_or_reuses_a_ticket() {
        let mut calls = PlatformCalls::default();
        for ticket in 1..=MAX_CALLS_PER_PLUGIN as u64 {
            calls
                .start(ticket, plugin("busy"), None, std::future::pending)
                .unwrap();
        }
        assert!(
            calls
                .start(
                    10,
                    plugin("busy"),
                    None,
                    || -> std::future::Ready<Outcome> {
                        panic!("must not start a refused request")
                    }
                )
                .is_err()
        );
        assert!(
            calls
                .start(
                    1,
                    plugin("other"),
                    None,
                    || -> std::future::Ready<Outcome> {
                        panic!("must not reuse an existing ticket")
                    }
                )
                .is_err()
        );
        calls.revoke(&plugin("busy"));
        while calls.next().await.is_some() {}
    }
}
