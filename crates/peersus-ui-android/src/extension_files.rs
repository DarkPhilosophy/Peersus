//! Descriptor ownership bridge for extension selections, independent of transfer pickers.
// Descriptor adoption and JNI exports are the only unsafe operations in this module.
#![allow(unsafe_code)]
use jni::{
    JNIEnv,
    objects::{JClass, JString},
    sys::{jint, jlong},
};
use std::{
    collections::HashMap,
    fs::File,
    os::fd::FromRawFd,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicI64, Ordering},
    },
};
use tokio::sync::oneshot;

type Selection = Option<(File, String)>;
type Waiters = HashMap<i64, oneshot::Sender<Selection>>;
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
            "cancelExtensionFile",
            "(J)V",
            &[jni::objects::JValue::Long(self.0)],
        );
    }
}

/// Cancellation removes the waiter before Java can return a late descriptor.
/// Unknown/cancelled results still close their descriptor in the native callback.
pub async fn choose(mime: &'static str) -> Result<(File, String), &'static str> {
    pick(Request::Open(mime)).await
}

pub async fn save(name: String) -> Result<File, &'static str> {
    pick(Request::Save(name)).await.map(|(file, _)| file)
}

enum Request {
    /// Open one file the person picks, offered by this MIME pattern.
    Open(&'static str),
    /// Create one new document with this suggested name.
    Save(String),
}

async fn pick(request: Request) -> Result<(File, String), &'static str> {
    let id = NEXT
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .map_err(|_| "Selection identifiers exhausted")?;
    let (send, receive) = oneshot::channel();
    {
        let mut waiters = waiters().lock().map_err(|_| "Selection is unavailable")?;
        if waiters.len() >= 4 {
            return Err("Too many pending selections");
        }
        waiters.insert(id, send);
    }
    let _pending = Pending(id);
    let (method, text) = match request {
        Request::Save(name) => ("saveExtensionFile", name),
        Request::Open(mime) => ("chooseExtensionFile", mime.to_owned()),
    };
    let issued = crate::bridge::with_activity(|env, activity| {
        let text = env.new_string(&text).map_err(|error| error.to_string())?;
        env.call_method(
            activity,
            method,
            "(JLjava/lang/String;)V",
            &[
                jni::objects::JValue::Long(id),
                jni::objects::JValue::Object(text.as_ref()),
            ],
        )
        .map_err(|error| {
            let _ = env.exception_clear();
            error.to_string()
        })?;
        Ok(())
    })
    .map(|_| ());
    issued.map_err(|_| "The file picker is unavailable")?;
    receive
        .await
        .map_err(|_| "Selection was cancelled")?
        .ok_or("Selection was cancelled or the file could not be opened")
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_ExtensionFiles_selected<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    request: jlong,
    descriptor: jint,
    name: JString<'local>,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let file = if descriptor >= 0 {
            // SAFETY: Java transferred exactly this descriptor with detachFd.
            Some(unsafe { File::from_raw_fd(descriptor) })
        } else {
            None
        };
        let selection =
            file.and_then(|file| env.get_string(&name).ok().map(|name| (file, name.into())));
        let sender = waiters()
            .lock()
            .ok()
            .and_then(|mut waiters| waiters.remove(&request));
        if let Some(sender) = sender {
            let _ = sender.send(selection);
        }
        // A failed send, missing waiter or invalid name drops the owned file here.
    }));
}
