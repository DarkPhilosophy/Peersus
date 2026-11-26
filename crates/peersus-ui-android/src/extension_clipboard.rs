//! Owner-process bridge to the platform clipboard policy. It knows no extension: it only
//! submits one bounded request to the activity and awaits exactly one answer.
// JNI exports are the only unsafe operations in this module.
#![allow(unsafe_code)]
use jni::{
    JNIEnv,
    objects::{JClass, JString},
    sys::{jboolean, jint, jlong},
};
use std::{
    collections::HashMap,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};
use tokio::sync::oneshot;

pub const READ: i32 = 1;
pub const OBSERVE: i32 = 2;
pub const RESET: i32 = 4;

/// Text bound of the Android clipboard policy, which is smaller than the API ceiling.
pub const MAX_BYTES: usize = 64 * 1024;
const MAX_WAIT: Duration = Duration::from_millis(2500);

/// What the platform answered. `text` exists only for reads; it is never logged.
pub struct Answer {
    pub ok: bool,
    pub text: Option<String>,
    pub changed: bool,
}
type Waiters = HashMap<i64, oneshot::Sender<Answer>>;

/// Raised on every change the platform policy notices. It carries no contents: only a read can say what is there.
static CHANGES: OnceLock<tokio::sync::watch::Sender<u64>> = OnceLock::new();
fn changes_sender() -> &'static tokio::sync::watch::Sender<u64> {
    CHANGES.get_or_init(|| tokio::sync::watch::channel(0).0)
}

/// Whoever wants to be told subscribes here; a subscriber made later still sees every later change.
pub fn changes() -> tokio::sync::watch::Receiver<u64> {
    changes_sender().subscribe()
}

/// Tell whoever watches that something may have changed. It carries no contents: only a read can say what is there.
pub fn announce_change() {
    changes_sender().send_modify(|count| *count = count.wrapping_add(1));
}
static WAITERS: OnceLock<Mutex<Waiters>> = OnceLock::new();
static NEXT: AtomicI64 = AtomicI64::new(1);
fn waiters() -> &'static Mutex<Waiters> {
    WAITERS.get_or_init(Mutex::default)
}

/// Dropping the request removes its waiter and tells Java, so a late answer is discarded.
struct Pending(i64);
impl Drop for Pending {
    fn drop(&mut self) {
        if let Ok(mut waiters) = waiters().lock() {
            waiters.remove(&self.0);
        }
        let _ = crate::bridge::call_activity(
            "cancelExtensionClipboard",
            "(J)V",
            &[jni::objects::JValue::Long(self.0)],
        );
    }
}

/// Submit one operation that carries no text and wait for its single answer.
/// Cancelling this future cancels the work.
pub async fn request(operation: i32) -> Result<Answer, &'static str> {
    if !matches!(operation, READ | OBSERVE | RESET) {
        return Err("This clipboard operation carries text");
    }
    submit(|id, remaining| {
        crate::bridge::call_activity(
            "runExtensionClipboard",
            "(JJI)V",
            &[
                jni::objects::JValue::Long(id),
                jni::objects::JValue::Long(remaining),
                jni::objects::JValue::Int(operation),
            ],
        )
        .map_err(|_| "Clipboard is unavailable")
    })
    .await
}

/// Ask the platform to replace the clipboard text. Only the platform policy decides whether it may,
/// and it checks again at the moment of the change; a cancelled caller never changes the clipboard.
pub async fn write(text: &str) -> Result<Answer, &'static str> {
    if text.len() > MAX_BYTES {
        return Err("Clipboard text exceeds the platform bound");
    }
    submit(|id, remaining| crate::bridge::write_extension_clipboard(id, remaining, text)).await
}

/// One admission, one waiter and one cancellation guard for every operation.
async fn submit(
    send_to_activity: impl FnOnce(i64, i64) -> Result<(), &'static str>,
) -> Result<Answer, &'static str> {
    let id = NEXT
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .map_err(|_| "Clipboard identifiers exhausted")?;
    let (send, receive) = oneshot::channel();
    {
        let mut waiters = waiters().lock().map_err(|_| "Clipboard is unavailable")?;
        if waiters.len() >= 8 {
            return Err("Too many pending clipboard requests");
        }
        waiters.insert(id, send);
    }
    let _pending = Pending(id);
    let remaining = i64::try_from(MAX_WAIT.as_millis()).map_err(|_| "Clipboard is unavailable")?;
    send_to_activity(id, remaining)?;
    tokio::time::timeout(MAX_WAIT, receive)
        .await
        .map_err(|_| "Clipboard did not answer in time")?
        .map_err(|_| "Clipboard request was cancelled")
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_ExtensionClipboard_answered<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    id: jlong,
    _operation: jint,
    ok: jboolean,
    text: JString<'local>,
    changed: jboolean,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let text = if text.is_null() {
            None
        } else {
            env.get_string(&text)
                .ok()
                .map(String::from)
                .filter(|text| text.len() <= MAX_BYTES)
        };
        let sender = waiters()
            .lock()
            .ok()
            .and_then(|mut waiters| waiters.remove(&id));
        if let Some(sender) = sender {
            let _ = sender.send(Answer {
                ok: ok != 0,
                text,
                changed: changed != 0,
            });
        }
    }));
}

/// The Android side of the shared clipboard rules: only what the activity can do. Every rule about handles, bounds,
/// expiry and ownership is in `peersus_app::clipboard_handles`, so a phone and a desktop cannot drift apart.
pub struct AndroidBoard;

impl peersus_app::clipboard_handles::Board for AndroidBoard {
    async fn read(&self, observe: bool) -> peersus_app::clipboard_handles::Reading {
        let operation = if observe { OBSERVE } else { READ };
        // A bridge that failed or timed out is "nothing readable", exactly as a refusal by the platform is: the caller
        // learns only that there is no text now, and an observer treats it as a break in what it has seen. It is SAID here,
        // with its reason, because from the extension's side nothing distinguishes it from an empty clipboard.
        match request(operation).await {
            Ok(answer) => {
                tracing::debug!(
                    target: "peersus_android::clipboard",
                    observe,
                    allowed = answer.ok,
                    has_text = answer.text.is_some(),
                    changed = answer.changed,
                    "The clipboard answered a read"
                );
                peersus_app::clipboard_handles::Reading {
                    text: answer.text.filter(|_| answer.ok),
                    changed: answer.changed,
                }
            }
            Err(reason) => {
                tracing::warn!(
                    observe,
                    reason,
                    "A clipboard read did not reach the platform"
                );
                peersus_app::clipboard_handles::Reading {
                    text: None,
                    changed: false,
                }
            }
        }
    }

    async fn write(&self, text: String) -> bool {
        let bytes = text.len();
        match write(&text).await {
            Ok(answer) => {
                tracing::debug!(target: "peersus_android::clipboard", bytes, allowed = answer.ok, "The clipboard answered a write");
                answer.ok
            }
            Err(reason) => {
                tracing::warn!(
                    bytes,
                    reason,
                    "A clipboard write did not reach the platform"
                );
                false
            }
        }
    }

    async fn reset(&self) -> bool {
        request(RESET).await.is_ok_and(|answer| answer.ok)
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_ExtensionClipboard_notifyChanged<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
) {
    let _ = catch_unwind(|| {
        announce_change();
        let sender = changes_sender();
        // Numbers only. Zero watchers means the owner never subscribed, so nothing downstream can possibly hear this.
        tracing::debug!(
            target: "peersus_android::clipboard",
            count = *sender.borrow(),
            watchers = sender.receiver_count(),
            "The platform noticed a clipboard change"
        );
    });
}
