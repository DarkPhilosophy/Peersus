//! Explicit consent returns credential bytes to the trusted host, never guest JSON.
// JNI symbol export is the only unsafe boundary in this module.
#![allow(unsafe_code)]
use jni::{
    JNIEnv,
    objects::{JByteArray, JClass},
    sys::jlong,
};
use std::{
    collections::HashMap,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicI64, Ordering},
    },
};
use tokio::sync::oneshot;
use zeroize::Zeroizing;

type Secret = Zeroizing<Vec<u8>>;
type Waiters = HashMap<i64, oneshot::Sender<Option<Secret>>>;
static WAITERS: OnceLock<Mutex<Waiters>> = OnceLock::new();
static NEXT: AtomicI64 = AtomicI64::new(1);
fn waiters() -> &'static Mutex<Waiters> {
    WAITERS.get_or_init(Mutex::default)
}

struct Pending(i64);
impl Drop for Pending {
    fn drop(&mut self) {
        if let Ok(mut waiters) = waiters().lock() {
            waiters.remove(&self.0);
        }
        let _ = crate::bridge::call_activity(
            "cancelExtensionAuthorization",
            "(J)V",
            &[jni::objects::JValue::Long(self.0)],
        );
    }
}

pub async fn request(
    caller: &str,
    origin: &str,
    filename: &str,
    bytes: u64,
) -> Result<Secret, &'static str> {
    let id = NEXT
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .map_err(|_| "Authorization identifiers exhausted")?;
    let (send, receive) = oneshot::channel();
    {
        let mut waiters = waiters()
            .lock()
            .map_err(|_| "Authorization is unavailable")?;
        if waiters.len() >= 4 {
            return Err("Too many pending authorizations");
        }
        waiters.insert(id, send);
    }
    let _pending = Pending(id);
    let bytes = i64::try_from(bytes).map_err(|_| "Invalid selected size")?;
    crate::bridge::request_extension_authorization(id, caller, origin, filename, bytes)?;
    receive
        .await
        .map_err(|_| "Authorization was cancelled")?
        .ok_or("Authorization was declined")
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_ExtensionAuthorization_authorized<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
    request: jlong,
    credential: JByteArray<'local>,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let secret = if credential.is_null() {
            None
        } else {
            env.get_array_length(&credential)
                .ok()
                .filter(|length| (1..=8192).contains(length))
                .and_then(|_| env.convert_byte_array(&credential).ok())
                .map(Zeroizing::new)
                .filter(|bytes| bytes.iter().all(|byte| (33..=126).contains(byte)))
        };
        let sender = waiters()
            .lock()
            .ok()
            .and_then(|mut waiters| waiters.remove(&request));
        if let Some(sender) = sender {
            let _ = sender.send(secret);
        }
        // Missing/cancelled waiters and failed delivery zeroize the owned bytes.
    }));
}
