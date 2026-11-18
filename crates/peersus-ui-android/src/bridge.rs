//! The bridge to the activity's document picker.
//!
//! This is the one place that crosses into Java, and it decides nothing. What happens to a file that the picker hands
//! over is `peersus_net::picked::Picked`, tried without a phone; the owner takes the file from `peersus_net::held::Held`
//! by number. This module only passes events in and starts the picker.
//!
//! WHAT CROSSES, AND WHO OWNS IT. Java opens the chosen file and gives up its own hold on the descriptor
//! (`detachFd`), so the number it passes is Rust's alone from then on: it is made into a `File` here, which closes it
//! when dropped. Were Java to keep it as well, it would be open twice and closed by neither.
//!
//! NO PANIC CROSSES. A panic that unwinds into the Java virtual machine ends the process without a message. Both
//! entry points run their work inside `catch_unwind`, and a failure is a file that is not kept and not a crash.
#![allow(unsafe_code)]

use jni::{
    JNIEnv, JavaVM,
    objects::{GlobalRef, JClass, JObject, JString},
    sys::{jint, jlong},
};
use peersus_net::{folder_picked::FolderPicked, held::Held, picked::Picked};
use std::{
    fs::File,
    os::fd::{FromRawFd, RawFd},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex, OnceLock},
};

/// What the picker has handed over, which knows the registry the files are kept in. One for the process, because there is
/// one activity, and because a native method of Java has no other way to find it.
static PICKED: OnceLock<Picked> = OnceLock::new();
/// How the folder picker's answers and deliveries stand. Separate from [`PICKED`] on purpose: see [`shared`].
static FOLDERS: OnceLock<FolderPicked> = OnceLock::new();
static ACTIVITY: Mutex<Option<Arc<(JavaVM, GlobalRef)>>> = Mutex::new(None);
static SERVICE: Mutex<Option<Arc<(JavaVM, GlobalRef)>>> = Mutex::new(None);
static PLATFORM_READY: OnceLock<tokio::sync::Notify> = OnceLock::new();

pub fn platform_ready() -> &'static tokio::sync::Notify {
    PLATFORM_READY.get_or_init(tokio::sync::Notify::new)
}

/// Make the picker's state, once, over `held`, and give it to the caller. A second call gives the same one.
///
/// This is the ONLY place that makes it, so it is always made over the owner's registry. It used to share one cell with
/// the folder picker's state, and whichever asked first made both: when the folder picker's was asked for first (another
/// thread, early), the picker's files were kept in a registry of its own, empty, and this call's `held` was thrown away
/// without a word. The interface then kept the chosen files in one registry and the owner looked for them in another.
pub fn shared(held: Held) -> Picked {
    PICKED.get_or_init(|| Picked::new(held)).clone()
}

/// The folder picker's state: the answers the person gave, and how the deliveries into the chosen folders went. The same one
/// for the process, for the same reason as [`shared`]. It owns nothing of the picker's files, so asking for it can never
/// decide which registry those are kept in: that is [`shared`]'s alone, made over the owner's.
pub fn folders() -> FolderPicked {
    FOLDERS.get_or_init(FolderPicked::default).clone()
}

/// Start the system's folder picker. Returns at once; the answer comes through `folderChosen`.
pub fn open_folder_picker() -> Result<(), String> {
    call_activity("chooseFolder", "()V", &[])
}

pub fn open_plugin_picker() -> Result<(), String> {
    call_activity("choosePlugin", "()V", &[])
}

pub fn open_language_picker() -> Result<(), String> {
    call_activity("chooseLanguage", "()V", &[])
}

/// Update process protection, independently of the activity's rendering lifecycle.
pub fn background_state(background: bool, active: bool) -> Result<(), String> {
    call_background(
        "setBackgroundState",
        "(ZZ)V",
        &[
            jni::objects::JValue::Bool(u8::from(background)),
            jni::objects::JValue::Bool(u8::from(active)),
        ],
    )
}

pub fn transfer_notifications(
    waiting: i32,
    completed: i32,
    newest: &str,
    files: &str,
) -> Result<(), String> {
    let known = background_object()?;
    let (vm, object) = known.as_ref();
    with_env(vm, |env| {
        let offer = JObject::from(env.new_string(newest).map_err(|error| error.to_string())?);
        let lines = JObject::from(env.new_string(files).map_err(|error| error.to_string())?);
        env.call_method(
            object.as_obj(),
            "setTransferNotifications",
            "(IILjava/lang/String;Ljava/lang/String;)V",
            &[
                jni::objects::JValue::Int(waiting),
                jni::objects::JValue::Int(completed),
                jni::objects::JValue::Object(&offer),
                jni::objects::JValue::Object(&lines),
            ],
        )
        .map_err(|error| error.to_string())?;
        Ok(())
    })
}

/// The service when there is one, the activity otherwise: the same choice as `call_background`, for a call that must make its
/// arguments itself, since a text for Java can only be made with the environment of the thread that calls.
fn background_object() -> Result<Arc<(JavaVM, GlobalRef)>, String> {
    let service = SERVICE
        .lock()
        .map_err(|_| "the service lock is poisoned")?
        .clone();
    match service {
        Some(service) => Ok(service),
        None => ACTIVITY
            .lock()
            .map_err(|_| "the activity lock is poisoned")?
            .clone()
            .ok_or_else(|| "the platform is not known yet".to_owned()),
    }
}

/// The notifications of the owner's list that reach outside the window, as the JSON cards Java shows while the window is not in
/// front (`PostedNotifications.java`). Called when they change.
pub fn posted_notifications(cards: &str) -> Result<(), String> {
    let known = background_object()?;
    let (vm, object) = known.as_ref();
    with_env(vm, |env| {
        let cards = JObject::from(env.new_string(cards).map_err(|error| error.to_string())?);
        env.call_method(
            object.as_obj(),
            "setPostedNotifications",
            "(Ljava/lang/String;)V",
            &[jni::objects::JValue::Object(&cards)],
        )
        .map_err(|error| error.to_string())?;
        Ok(())
    })
}

/// Answers given on a system notification of the owner's list, waiting for the loop that sends them: the number, and the button's id or
/// none when it was swiped away.
static NOTIFICATION_ANSWERS: Mutex<Vec<(u64, Option<String>)>> = Mutex::new(Vec::new());
static NOTIFICATION_ANSWERED: OnceLock<tokio::sync::Notify> = OnceLock::new();

/// Woken when a system notification of the owner's list was answered.
pub fn notification_answered() -> &'static tokio::sync::Notify {
    NOTIFICATION_ANSWERED.get_or_init(tokio::sync::Notify::new)
}

/// The answers given since the last time, in the order they were given, taken once.
pub fn take_notification_answers() -> Vec<(u64, Option<String>)> {
    NOTIFICATION_ANSWERS
        .lock()
        .map(|mut answers| std::mem::take(&mut *answers))
        .unwrap_or_default()
}

/// A system notification of the owner's list was answered. The number is the owner's and is only handed back; a button id is checked by
/// the owner against what the notification offered, so nothing Java sends is trusted here.
// SAFETY: no other symbol of this name exists in this library; Java's NotificationActions declares it with this signature.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_NotificationActions_notificationAnswered<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    notification: jlong,
    button: JString<'local>,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let Ok(id) = u64::try_from(notification) else {
            return;
        };
        let button: Option<String> = if button.is_null() {
            None
        } else {
            env.get_string(&button).map(Into::into).ok()
        };
        if let Ok(mut answers) = NOTIFICATION_ANSWERS.lock() {
            answers.push((id, button));
        }
        notification_answered().notify_one();
    }));
}

/// Give Java the colours the interface is drawn with now (surface, raised, border, text, muted, accent, bad, button, button text, each as
/// 0xRRGGBB), which it keeps for the notification of an offer. Called when the colours change.
pub fn offer_appearance(colours: &[u32; 9]) -> Result<(), String> {
    let known = background_object()?;
    let (vm, object) = known.as_ref();
    with_env(vm, |env| {
        let array = env.new_int_array(9).map_err(|error| error.to_string())?;
        let values: Vec<i32> = colours.iter().map(|colour| *colour as i32).collect();
        env.set_int_array_region(&array, 0, &values)
            .map_err(|error| error.to_string())?;
        let array = JObject::from(array);
        env.call_method(
            object.as_obj(),
            "setOfferAppearance",
            "([I)V",
            &[jni::objects::JValue::Object(&array)],
        )
        .map_err(|error| error.to_string())?;
        Ok(())
    })
}

/// What a button of the offer's notification asked, for the offer it was made for. A button can be pressed with the window closed, so
/// the answer is kept here until what owns the offer takes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OfferAction {
    /// Refuse the offer.
    Cancel(String),
    /// Accept the offer: the person is shown the folder picker for it.
    Accept(String),
    /// Show the files of the offer: the notification, touched with the window closed, opens the list when the window comes.
    View(String),
}

static OFFER_ACTIONS: Mutex<Vec<OfferAction>> = Mutex::new(Vec::new());
static OFFER_ACTION_ARRIVED: OnceLock<tokio::sync::Notify> = OnceLock::new();

/// Woken when a button of a notification was pressed.
pub fn offer_action_arrived() -> &'static tokio::sync::Notify {
    OFFER_ACTION_ARRIVED.get_or_init(tokio::sync::Notify::new)
}

/// What the buttons asked since the last time, in the order they were pressed, taking only the ones `wanted` says are for the caller.
/// A press is never lost: what is not wanted stays for whoever wants it.
pub fn take_offer_actions(wanted: impl Fn(&OfferAction) -> bool) -> Vec<OfferAction> {
    let Ok(mut kept) = OFFER_ACTIONS.lock() else {
        return Vec::new();
    };
    let (taken, left): (Vec<_>, Vec<_>) = kept.drain(..).partition(|action| wanted(action));
    *kept = left;
    taken
}

/// Whether the notification was touched to see the files since the last time this was asked, taken once: the window opens its list of the
/// files on it. Only that kind is taken, so a Cancel or an Accept waiting in the queue stays for whoever answers it.
pub fn take_view_files() -> bool {
    !take_offer_actions(|action| matches!(action, OfferAction::View(_))).is_empty()
}

/// The offers whose Accept was pressed on the notification or the panel and was left for the window (see `protect_transfers`, which serves
/// the ones that need no window), by their transfer ids, taken once.
pub fn take_accepted_ids() -> Vec<String> {
    take_offer_actions(|action| matches!(action, OfferAction::Accept(_)))
        .into_iter()
        .filter_map(|action| {
            if let OfferAction::Accept(transfer) = action {
                Some(transfer)
            } else {
                None
            }
        })
        .collect()
}

/// Whether a press the caller wants is waiting, without taking it: what decides to ask for the window, which then takes it.
pub fn offer_actions_waiting(wanted: impl Fn(&OfferAction) -> bool) -> bool {
    OFFER_ACTIONS
        .lock()
        .is_ok_and(|kept| kept.iter().any(wanted))
}

fn push_offer_action(action: OfferAction) {
    if let Ok(mut kept) = OFFER_ACTIONS.lock() {
        kept.push(action);
    }
    offer_action_arrived().notify_one();
}

/// A button of the notification was pressed: `kind` is "cancel" or "accept", and `transfer` the offer the button was made for.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_NotificationActions_offerAnswered<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    kind: JString<'local>,
    transfer: JString<'local>,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let kind: String = env.get_string(&kind).map(Into::into).unwrap_or_default();
        let transfer: String = env
            .get_string(&transfer)
            .map(Into::into)
            .unwrap_or_default();
        if transfer.is_empty() {
            return;
        }
        // Only recognized action names, never the transfer id or arbitrary JNI text.
        let action = match kind.as_str() {
            "cancel" => "cancel",
            "accept" => "accept",
            "view" => "view",
            _ => "unknown",
        };
        tracing::debug!(action, "Notification action received");
        match kind.as_str() {
            "cancel" => push_offer_action(OfferAction::Cancel(transfer)),
            "accept" => push_offer_action(OfferAction::Accept(transfer)),
            "view" => push_offer_action(OfferAction::View(transfer)),
            _ => {}
        }
    }));
}
pub fn language(code: &str) -> Result<(), String> {
    let known = background_object()?;
    let (vm, object) = known.as_ref();
    with_env(vm, |env| {
        let code = JObject::from(env.new_string(code).map_err(|error| error.to_string())?);
        env.call_method(
            object.as_obj(),
            "setLanguage",
            "(Ljava/lang/String;)V",
            &[jni::objects::JValue::Object(&code)],
        )
        .map_err(|error| error.to_string())?;
        Ok(())
    })
}

/// Download untrusted store bytes in the platform's private worker process.
/// Called on a blocking pool; neither the native owner nor the UI thread waits.
pub fn store_download(name: &str, limit: usize, output: &std::path::Path) -> Result<(), String> {
    let known = background_object()?;
    let (vm, object) = known.as_ref();
    let output = output.to_str().ok_or("the download path is not valid")?;
    let limit = i64::try_from(limit).map_err(|_| "the download bound is not valid")?;
    let reached = with_env(vm, |env| {
        let name = JObject::from(env.new_string(name).map_err(|error| error.to_string())?);
        let output = JObject::from(env.new_string(output).map_err(|error| error.to_string())?);
        env.call_method(
            object.as_obj(),
            "downloadStore",
            "(Ljava/lang/String;JLjava/lang/String;)Z",
            &[
                jni::objects::JValue::Object(&name),
                jni::objects::JValue::Long(limit),
                jni::objects::JValue::Object(&output),
            ],
        )
        .and_then(|answer| answer.z())
        .map_err(|error| error.to_string())
    })?;
    if reached {
        Ok(())
    } else {
        Err("The extension store could not be reached".to_owned())
    }
}

/// Whether something else than the application keeps this installation up to date (the distribution's own answer); `None` when
/// neither the service nor the activity can be asked.
pub fn update_managed() -> Option<bool> {
    let known = service_or_activity()?;
    let (vm, object) = known.as_ref();
    with_env(vm, |env| {
        env.call_method(object.as_obj(), "updateManaged", "()Z", &[])
            .and_then(|answer| answer.z())
            .map_err(|error| error.to_string())
    })
    .ok()
}

/// A Java call's result, with a pending exception cleared: `None` for any failure.
fn answered<T>(env: &mut JNIEnv, result: jni::errors::Result<T>) -> Option<T> {
    result
        .map_err(|_| {
            let _ = env.exception_clear();
        })
        .ok()
}

/// The battery as the platform's `BatteryManager` says it: the files under `/sys/class/power_supply` are closed to an
/// application. `None` when there is nothing to ask; a property the phone does not give stays unknown.
pub fn battery() -> Option<peersus_app::system_readings::Battery> {
    use jni::objects::JValue;
    // `BatteryManager.BATTERY_PROPERTY_*`: the charge left (µAh), the current now (µA), the charge in percent.
    const CHARGE_COUNTER: i32 = 1;
    const CURRENT_NOW: i32 = 2;
    const CAPACITY: i32 = 4;
    let known = service_or_activity()?;
    let (vm, context) = known.as_ref();
    attached(vm, |env| {
        let name = JObject::from(env.new_string("batterymanager").ok()?);
        let found = env.call_method(
            context.as_obj(),
            "getSystemService",
            "(Ljava/lang/String;)Ljava/lang/Object;",
            &[JValue::Object(&name)],
        );
        let manager = answered(env, found.and_then(|value| value.l()))?;
        if manager.is_null() {
            return None;
        }
        // A property a phone does not give is `Integer.MIN_VALUE`.
        let mut property = |id: i32| {
            let said = env.call_method(&manager, "getIntProperty", "(I)I", &[JValue::Int(id)]);
            answered(env, said.and_then(|value| value.i())).filter(|n| *n != i32::MIN)
        };
        let (percent, left, current) = (
            property(CAPACITY),
            property(CHARGE_COUNTER),
            property(CURRENT_NOW),
        );
        let said = env.call_method(&manager, "isCharging", "()Z", &[]);
        let charging = answered(env, said.and_then(|value| value.z()));
        let said = env.call_method(&manager, "computeChargeTimeRemaining", "()J", &[]);
        let to_full = answered(env, said.and_then(|value| value.j()));
        Some(peersus_app::system_readings::battery_said(
            percent,
            charging,
            left.map(i64::from),
            current.map(i64::from),
            to_full,
        ))
    })
}

/// The bytes this phone received and sent since it started, as the platform's `TrafficStats` counts them: `/proc/net/dev` is
/// closed to an application. `None` when the platform cannot say.
pub fn network() -> Option<peersus_app::system_readings::Network> {
    let known = service_or_activity()?;
    let (vm, _) = known.as_ref();
    attached(vm, |env| {
        // `TrafficStats.UNSUPPORTED` is -1.
        let mut total = |method: &str| {
            let said = env.call_static_method("android/net/TrafficStats", method, "()J", &[]);
            answered(env, said.and_then(|value| value.j())).and_then(|n| u64::try_from(n).ok())
        };
        let (received, sent) = (total("getTotalRxBytes"), total("getTotalTxBytes"));
        (received.is_some() || sent.is_some())
            .then_some(peersus_app::system_readings::Network { received, sent })
    })
}

/// The first line of `/proc/stat`, which Android closes to an application, read through the shell provider the person already
/// allowed (Stellar/Shizuku, community distribution). `None` without one, or while it is being bound: it never waits for it.
pub fn processor_stat() -> Option<String> {
    let known = service_or_activity()?;
    let (vm, object) = known.as_ref();
    attached(vm, |env| {
        let said = env.call_method(
            object.as_obj(),
            "processorTime",
            "()Ljava/lang/String;",
            &[],
        );
        let line = answered(env, said.and_then(|value| value.l()))?;
        if line.is_null() {
            return None;
        }
        let line = JString::from(line);
        let text: String = env.get_string(&line).ok()?.into();
        (text.len() <= 256).then_some(text)
    })
}

/// The phone's chip as the platform names it (`Build.SOC_MANUFACTURER` and `Build.SOC_MODEL`, `QTI SM8650`): its
/// `/proc/cpuinfo` names none. `None` when the platform says it does not know.
pub fn processor_name() -> Option<String> {
    let known = service_or_activity()?;
    let (vm, _) = known.as_ref();
    attached(vm, |env| {
        let mut field = |name: &str| -> Option<String> {
            let said = env.get_static_field("android/os/Build", name, "Ljava/lang/String;");
            let value = answered(env, said.and_then(|value| value.l()))?;
            if value.is_null() {
                return None;
            }
            let value = JString::from(value);
            let text: String = env.get_string(&value).ok()?.into();
            // `Build.UNKNOWN` is the platform's word for not known.
            (!text.is_empty() && text != "unknown").then_some(text)
        };
        let (maker, model) = (field("SOC_MANUFACTURER"), field("SOC_MODEL"));
        match (maker, model) {
            (Some(maker), Some(model)) => Some(format!("{maker} {model}")),
            (None, Some(model)) => Some(model),
            _ => None,
        }
    })
}

/// Hand the verified update at `path` to the system. Blocking, on a blocking pool. A replacement that succeeds ends this process
/// before the call returns; what does return is the platform's fixed reason code, or `None` when it could not be asked at all.
pub fn install_update(path: &std::path::Path, reopen: bool) -> Option<i32> {
    let known = service_or_activity()?;
    let (vm, object) = known.as_ref();
    let path = path.to_str()?;
    with_env(vm, |env| {
        let path = JObject::from(env.new_string(path).map_err(|error| error.to_string())?);
        env.call_method(
            object.as_obj(),
            "installUpdate",
            "(Ljava/lang/String;Z)I",
            &[
                jni::objects::JValue::Object(&path),
                jni::objects::JValue::Bool(u8::from(reopen)),
            ],
        )
        .and_then(|answer| answer.i())
        .map_err(|error| error.to_string())
    })
    .ok()
}

/// Development configuration uses byte arrays, never method arguments containing
/// a URL/token string. Every exception is replaced with a fixed message.
#[cfg(feature = "private-store")]
pub fn private_store_access(input: Option<&[u8]>) -> Result<Option<Vec<u8>>, &'static str> {
    const FAILED: &str = "Secure private catalog storage is unavailable";
    let known = background_object().map_err(|_| FAILED)?;
    let (vm, object) = known.as_ref();
    with_env(vm, |env| {
        let result = (|| {
            if let Some(input) = input {
                if input.len() > 16384 {
                    return Err(FAILED);
                }
                let bytes = env.byte_array_from_slice(input).map_err(|_| FAILED)?;
                let object_bytes = JObject::from(bytes);
                let saved = env
                    .call_method(
                        object.as_obj(),
                        "savePrivateStore",
                        "([B)Z",
                        &[jni::objects::JValue::Object(&object_bytes)],
                    )
                    .map_err(|_| FAILED)?
                    .z()
                    .map_err(|_| FAILED)?;
                if !saved {
                    return Err(FAILED);
                }
                Ok(None)
            } else {
                let bytes = env
                    .call_method(object.as_obj(), "loadPrivateStore", "()[B", &[])
                    .map_err(|_| FAILED)?
                    .l()
                    .map_err(|_| FAILED)?;
                if bytes.is_null() {
                    return Err(FAILED);
                }
                let bytes = jni::objects::JByteArray::from(bytes);
                let length = env.get_array_length(&bytes).map_err(|_| FAILED)?;
                if !(0..=16384).contains(&length) {
                    return Err(FAILED);
                }
                let data = env.convert_byte_array(&bytes).map_err(|_| FAILED)?;
                env.set_byte_array_region(&bytes, 0, &vec![0; length as usize])
                    .map_err(|_| FAILED)?;
                Ok(Some(data))
            }
        })();
        result.map_err(str::to_owned)
    })
    .map_err(|_| FAILED)
}

fn call_background(
    method: &str,
    signature: &str,
    arguments: &[jni::objects::JValue],
) -> Result<(), String> {
    let service = SERVICE
        .lock()
        .map_err(|_| "the service lock is poisoned")?
        .clone();
    if let Some(service) = service {
        let (vm, object) = service.as_ref();
        with_env(vm, |env| {
            env.call_method(object.as_obj(), method, signature, arguments)
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
    } else {
        call_activity(method, signature, arguments)
    }
}

/// Bring the window of the program forward, for an Accept that only a window can serve (there is no folder to put the file in, so the system's
/// picker has to be shown). With the window open this does nothing.
pub fn bring_window_forward() -> Result<(), String> {
    call_background("bringWindowForward", "()V", &[])
}

/// Tell Android whether an approved extension may replay input on this phone (generic `host/input.inject`, no extension named).
pub fn remote_input_wanted(wanted: bool) -> Result<(), String> {
    call_background(
        "setRemoteInputWanted",
        "(Z)V",
        &[jni::objects::JValue::Bool(u8::from(wanted))],
    )
}

/// Tell Java whether an Accept of the notification is served by the owner with no window (saving on their own is on and a folder is chosen),
/// which decides what the Accept button of the notification is made of. Called whenever it changes.
pub fn accept_without_window(value: bool) -> Result<(), String> {
    call_background(
        "setAcceptWithoutWindow",
        "(Z)V",
        &[jni::objects::JValue::Bool(u8::from(value))],
    )
}

/// Tell Java at which edge of the screen the panel of an offer is drawn. A phone has a top and a bottom only: every other
/// place is the bottom, as the setting says. Called whenever it changes.
pub fn notice_place(place: peersus_core::config::NoticePlace) -> Result<(), String> {
    // The two numbers `OfferPanel` names TOP and BOTTOM.
    let edge = match place {
        peersus_core::config::NoticePlace::Top => 0,
        peersus_core::config::NoticePlace::Bottom
        | peersus_core::config::NoticePlace::Left
        | peersus_core::config::NoticePlace::Right => 1,
    };
    call_background("setNoticePlace", "(I)V", &[jni::objects::JValue::Int(edge)])
}

static IN_FRONT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the window is in front now, as the activity last said. Not in front until told: with only the service alive
/// there is no window.
pub fn window_in_front() -> bool {
    IN_FRONT.load(std::sync::atomic::Ordering::SeqCst)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_MainActivity_windowInFront(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    front: jni::sys::jboolean,
) {
    IN_FRONT.store(front != 0, std::sync::atomic::Ordering::SeqCst);
}

/// The most announcements kept between two looks of the owner; the oldest gives way.
const MOST_HEARD: usize = 256;
/// The longest data of a tag's announcement that is kept, and of an offer's. Java carries no more either; said again
/// here because what arrives from the air is nobody's word.
const MOST_TAG_DATA: i32 = 32;
const MOST_OFFER_DATA: i32 = peersus_core::nearby::OFFER_BYTES as i32;
static HEARD: Mutex<std::collections::VecDeque<peersus_app::nearby::Reading>> =
    Mutex::new(std::collections::VecDeque::new());

/// One announcement of one of the application's two nearness services that the phone heard: its data, of anyone's
/// making, the strength it was heard at, and which service's it is (0 the tag's, 1 an offer to pair). Called by
/// `NearbyRadio` on the system's thread.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_NearbyRadio_heard(
    env: JNIEnv<'_>,
    _class: JClass<'_>,
    data: jni::objects::JByteArray<'_>,
    strength: jint,
    kind: jint,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let (kind, most) = match kind {
            0 => (peersus_app::nearby::Kind::Tag, MOST_TAG_DATA),
            1 => (peersus_app::nearby::Kind::Offer, MOST_OFFER_DATA),
            _ => return,
        };
        if data.is_null()
            || !matches!(env.get_array_length(&data), Ok(length) if (0..=most).contains(&length))
        {
            return;
        }
        let (Ok(data), Ok(strength)) = (env.convert_byte_array(&data), i16::try_from(strength))
        else {
            return;
        };
        if let Ok(mut heard) = HEARD.lock() {
            if heard.len() >= MOST_HEARD {
                heard.pop_front();
            }
            heard.push_back(peersus_app::nearby::Reading {
                kind,
                data,
                strength,
            });
        }
    }));
}

/// What was heard since this was last asked, oldest first.
pub fn take_heard() -> Vec<peersus_app::nearby::Reading> {
    HEARD
        .lock()
        .map(|mut heard| heard.drain(..).collect())
        .unwrap_or_default()
}

/// Ask `NearbyRadio` through the service when there is one, else the activity. `None` when neither can be asked or Java
/// threw; otherwise the number it answered, which `NearbyRadio` names.
fn nearby_call(method: &str, signature: &str, argument: NearbyArgument<'_>) -> Option<i32> {
    let known = service_or_activity()?;
    let (vm, object) = known.as_ref();
    with_env(vm, |env| {
        let answer = match argument {
            NearbyArgument::Bytes(bytes) => {
                let bytes = match bytes {
                    Some(bytes) => JObject::from(
                        env.byte_array_from_slice(bytes)
                            .map_err(|error| error.to_string())?,
                    ),
                    None => JObject::null(),
                };
                env.call_method(
                    object.as_obj(),
                    method,
                    signature,
                    &[jni::objects::JValue::Object(&bytes)],
                )
            }
            NearbyArgument::On(on) => env.call_method(
                object.as_obj(),
                method,
                signature,
                &[jni::objects::JValue::Bool(u8::from(on))],
            ),
        };
        answer
            .and_then(|answer| answer.i())
            .map_err(|error| error.to_string())
    })
    .ok()
}

enum NearbyArgument<'a> {
    Bytes(Option<&'a [u8]>),
    On(bool),
}

/// Announce `tag` over Bluetooth Low Energy, or nothing. Blocking, for a blocking pool.
pub fn nearby_announce(tag: Option<&[u8]>) -> Option<i32> {
    nearby_call("announceNearby", "([B)I", NearbyArgument::Bytes(tag))
}

/// Announce `offer`, an offer to pair, beside the tag, or stop. Blocking, for a blocking pool.
pub fn nearby_offer(offer: Option<&[u8]>) -> Option<i32> {
    nearby_call("offerNearby", "([B)I", NearbyArgument::Bytes(offer))
}

/// Listen for announcements, or stop. Blocking, for a blocking pool.
pub fn nearby_listen(on: bool) -> Option<i32> {
    nearby_call("listenNearby", "(Z)I", NearbyArgument::On(on))
}

/// Have the activity ask the person to allow Bluetooth for devices nearby. Nothing happens with no window in front.
pub fn ask_nearby_permission() -> Result<(), String> {
    call_activity("askNearbyPermission", "()V", &[])
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_TransferService_startOwner(
    mut env: JNIEnv<'_>,
    service: JObject<'_>,
    directory: JString<'_>,
) {
    let result = catch_unwind(AssertUnwindSafe(|| -> Result<(), String> {
        let directory: String = env
            .get_string(&directory)
            .map_err(|error| error.to_string())?
            .into();
        let vm = env.get_java_vm().map_err(|error| error.to_string())?;
        let global = env
            .new_global_ref(service)
            .map_err(|error| error.to_string())?;
        *SERVICE.lock().map_err(|_| "the service lock is poisoned")? = Some(Arc::new((vm, global)));
        platform_ready().notify_one();
        let _ = crate::owner_at(std::path::Path::new(&directory))?;
        Ok(())
    }));
    if !matches!(result, Ok(Ok(()))) {
        let _ = env.exception_clear();
        tracing::error!(
            code = "background-owner-start-failed",
            "Android background initialization failed"
        );
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_TransferService_releaseService(
    env: JNIEnv<'_>,
    service: JObject<'_>,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Ok(mut current) = SERVICE.lock()
            && current.as_ref().is_some_and(|known| {
                env.is_same_object(known.1.as_obj(), &service)
                    .unwrap_or(false)
            })
        {
            *current = None;
        }
        platform_ready().notify_one();
    }));
}

static BACKGROUND_TIMEOUT: OnceLock<tokio::sync::Notify> = OnceLock::new();

pub fn background_timeout() -> &'static tokio::sync::Notify {
    BACKGROUND_TIMEOUT.get_or_init(tokio::sync::Notify::new)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_MainActivity_backgroundTimedOut(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| background_timeout().notify_one()));
}

static ASLEEP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static ASLEEP_CHANGED: OnceLock<tokio::sync::Notify> = OnceLock::new();

/// Woken when the system says the screen went off or on, or that it went idle or woke from it.
pub fn asleep_changed() -> &'static tokio::sync::Notify {
    ASLEEP_CHANGED.get_or_init(tokio::sync::Notify::new)
}

/// Whether this phone sleeps now, as the system last said: the screen is off, or it is in its idle mode (Doze). Awake until told.
pub fn asleep_now() -> bool {
    ASLEEP.load(std::sync::atomic::Ordering::SeqCst)
}

/// The system's word on whether the phone sleeps. Called by the service from the broadcasts of the screen and of the idle mode, on any
/// thread, with the window open or closed. Only a CHANGE wakes the owner: the same word twice changes nothing.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_TransferService_asleepChanged(
    _env: JNIEnv<'_>,
    _service: JObject<'_>,
    asleep: jni::sys::jboolean,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let now = asleep != 0;
        if ASLEEP.swap(now, std::sync::atomic::Ordering::SeqCst) != now {
            asleep_changed().notify_one();
        }
    }));
}

/// Put a file that arrived whole into the chosen folder: Java copies it and says how it went through `delivered`. The service does it when there
/// is one, so that it is done with the window closed, and the activity otherwise.
pub fn deliver(handoff: &peersus_net::folder_picked::Handoff) -> Result<(), String> {
    let service = SERVICE
        .lock()
        .map_err(|_| "the service lock is poisoned")?
        .clone();
    let known = match service {
        Some(service) => service,
        None => ACTIVITY
            .lock()
            .map_err(|_| "the activity lock is poisoned")?
            .clone()
            .ok_or("neither the service nor the activity is known yet")?,
    };
    let (vm, activity) = known.as_ref();
    let mut env = vm
        .attach_current_thread()
        .map_err(|error| error.to_string())?;
    let path = handoff.path.to_string_lossy();
    let strings = [
        handoff.transfer.as_str(),
        path.as_ref(),
        handoff.name.as_str(),
        handoff.tree.as_str(),
    ]
    .into_iter()
    .map(|text| env.new_string(text))
    .collect::<Result<Vec<_>, _>>()
    .map_err(|error| error.to_string())?;
    let folders = env
        .new_object_array(
            handoff.folders.len() as i32,
            "java/lang/String",
            JObject::null(),
        )
        .map_err(|error| error.to_string())?;
    for (index, folder) in handoff.folders.iter().enumerate() {
        let text = env.new_string(folder).map_err(|error| error.to_string())?;
        env.set_object_array_element(&folders, index as i32, text)
            .map_err(|error| error.to_string())?;
    }
    let mut arguments: Vec<jni::objects::JValue> =
        strings.iter().map(jni::objects::JValue::from).collect();
    let folders = JObject::from(folders);
    arguments.push(jni::objects::JValue::Object(&folders));
    env.call_method(
        activity.as_obj(),
        "deliver",
        "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;[Ljava/lang/String;)V",
        &arguments,
    )
    .map_err(|error| {
        let _ = env.exception_clear();
        error.to_string()
    })?;
    Ok(())
}

/// Remember the activity, so that the picker can be started from any thread later.
///
/// # Safety
/// `vm` must be the JavaVM pointer of this process and `activity` a live `Activity` object, as `AndroidApp` gives them.
pub unsafe fn remember_activity(
    vm: *mut std::ffi::c_void,
    activity: *mut std::ffi::c_void,
) -> Result<(), String> {
    // SAFETY: the caller promises that `vm` is this process's JavaVM.
    let vm = unsafe { JavaVM::from_raw(vm.cast()) }.map_err(|error| error.to_string())?;
    let env = vm
        .attach_current_thread()
        .map_err(|error| error.to_string())?;
    // SAFETY: the caller promises that `activity` is a live object; a global reference keeps it alive past this call.
    let local = unsafe { JObject::from_raw(activity.cast()) };
    let global = env
        .new_global_ref(&local)
        .map_err(|error| error.to_string())?;
    drop(env);
    let mut current = ACTIVITY
        .lock()
        .map_err(|_| "the activity lock is poisoned")?;
    if current
        .as_ref()
        .is_some_and(|known| known.0.get_java_vm_pointer() != vm.get_java_vm_pointer())
    {
        return Err("the activity belongs to another virtual machine".to_owned());
    }
    *current = Some(Arc::new((vm, global)));
    drop(current);
    platform_ready().notify_one();
    Ok(())
}

/// Call a method of the activity that takes `arguments` and returns nothing, from any thread. Returns at once; what Java
/// does comes back through the native methods. A Java exception left pending would be thrown the next time Rust
/// returns to Java, so it is cleared here and said as an error.
pub fn call_activity(
    method: &str,
    signature: &str,
    arguments: &[jni::objects::JValue],
) -> Result<(), String> {
    let known = ACTIVITY
        .lock()
        .map_err(|_| "the activity lock is poisoned")?
        .clone()
        .ok_or("the activity is not known yet")?;
    let (vm, activity) = known.as_ref();
    with_env(vm, |env| {
        env.call_method(activity.as_obj(), method, signature, arguments)
            .map(|_| ())
            .map_err(|error| error.to_string())
    })
}

/// Call a method of the activity that takes no arguments and answers true or false, from any thread: what the notifications row of Settings asks
/// to show the real state. A Java exception left pending is cleared and said as an error, as in [`call_activity`].
pub fn call_activity_bool(method: &str) -> Result<bool, String> {
    let known = ACTIVITY
        .lock()
        .map_err(|_| "the activity lock is poisoned")?
        .clone()
        .ok_or("the activity is not known yet")?;
    let (vm, activity) = known.as_ref();
    with_env(vm, |env| {
        env.call_method(activity.as_obj(), method, "()Z", &[])
            .and_then(|answer| answer.z())
            .map_err(|error| error.to_string())
    })
}

/// Run `call` on this thread, attached to the Java machine for good, in a local frame of its own: every repeated call from
/// Rust into Java goes through here, whatever thread asks again and again (the usable area, the dark mode, the readings an
/// extension polls, Settings rows, nearby radio, notifications). Attaching and detaching for every call made the runtime create
/// a Java thread each time (seen as `Thread-N`, N climbing about 16 times a second on NX while nothing happened). The frame frees
/// the call's references when it returns; the thread is detached when it ends. A Java exception left pending is cleared, and
/// the call's error is said.
fn with_env<R>(
    vm: &JavaVM,
    call: impl FnOnce(&mut JNIEnv) -> Result<R, String>,
) -> Result<R, String> {
    let mut env = vm
        .attach_current_thread_permanently()
        .map_err(|error| error.to_string())?;
    let result = env
        .with_local_frame(16, |env| -> Result<Result<R, String>, jni::errors::Error> {
            Ok(call(env))
        })
        .map_err(|error| error.to_string())
        .and_then(|result| result);
    if result.is_err() {
        let _ = env.exception_clear();
    }
    result
}

/// [`with_env`] for a look that may find nothing: `None` when the thread cannot be attached or `ask` gives nothing.
fn attached<R>(vm: &JavaVM, ask: impl FnOnce(&mut JNIEnv) -> Option<R>) -> Option<R> {
    with_env(vm, |env| ask(env).ok_or_else(String::new)).ok()
}

/// Ask the activity something through [`attached`]. `None` when the activity is not known yet, is being torn down, or the call
/// failed (its exception cleared).
fn ask_activity<R>(ask: impl FnOnce(&mut JNIEnv, &JObject) -> Option<R>) -> Option<R> {
    let known = ACTIVITY.lock().ok()?.clone()?;
    let (vm, activity) = known.as_ref();
    attached(vm, |env| ask(env, activity.as_obj()))
}

/// Whether the system is in its dark mode now, from the activity's resources, on any thread: `Some(true)` dark, `Some(false)` light, `None` when the
/// activity cannot say (it is not known yet, or it is being torn down). Never read from the native application state (`AndroidApp::config`): that
/// takes a lock which is destroyed with the activity, and a thread that asks after it aborts the whole process, which is what a thread of its own
/// that asks every half second did on any change of activity.
pub fn dark_mode_now() -> Option<bool> {
    ask_activity(|env, activity| {
        let answer = env
            .call_method(activity, "darkModeNow", "()Z", &[])
            .map_err(|_| {
                let _ = env.exception_clear();
            })
            .ok()?;
        answer.z().ok()
    })
}

/// The usable area as a rectangle (left, top, right, bottom, in pixels), from the activity, on any thread; `None` when the activity cannot answer (it is
/// not known yet, or it is being torn down). Never from the native application state (`AndroidApp::content_rect`), which takes a lock destroyed with the
/// activity: a thread of its own that asked after it aborted the process.
pub fn usable_area() -> Option<(i32, i32, i32, i32)> {
    ask_activity(|env, activity| {
        let answer = env
            .call_method(activity, "usableAreaRectangle", "()[I", &[])
            .map_err(|_| {
                let _ = env.exception_clear();
            })
            .ok()?;
        let array = jni::objects::JIntArray::from(answer.l().ok()?);
        let mut four = [0_i32; 4];
        env.get_int_array_region(&array, 0, &mut four).ok()?;
        Some((four[0], four[1], four[2], four[3]))
    })
}

/// Open an explicitly pressed public HTTPS link; never invoked by startup or a network message.
pub fn open_external_url(url: &str) -> bool {
    with_activity(|env, activity| {
        let value = JObject::from(env.new_string(url).map_err(|error| error.to_string())?);
        env.call_method(
            activity,
            "openExternalUrl",
            "(Ljava/lang/String;)Z",
            &[jni::objects::JValue::Object(&value)],
        )
        .and_then(|answer| answer.z())
        .map_err(|error| error.to_string())
    })
    .unwrap_or(false)
}

/// Whether the notifications of this application can be shown now.
pub fn notifications_allowed() -> bool {
    call_activity_bool("notificationsAllowed").unwrap_or(false)
}

/// Whether the panel of an offer can be drawn over other applications now.
pub fn overlay_allowed() -> bool {
    call_activity_bool("overlayAllowed").unwrap_or(false)
}

/// Open the screen of the system where the person allows drawing over other applications.
pub fn open_overlay_permission() -> Result<(), String> {
    call_activity("openOverlayPermission", "()V", &[])
}

/// Whether the system leaves this program out of its battery optimisation now.
pub fn battery_unrestricted() -> bool {
    call_activity_bool("batteryUnrestricted").unwrap_or(false)
}

/// Open the system's list where the person chooses whether this program is left out of battery optimisation.
pub fn open_battery_settings() -> Result<(), String> {
    call_activity("openBatterySettings", "()V", &[])
}

/// What the notifications row does when pressed: ask for the permission, or open the settings where it can be given.
pub fn open_notifications() -> Result<(), String> {
    call_activity("openNotifications", "()V", &[])
}

pub fn background_clipboard_supported() -> bool {
    call_activity_bool("backgroundClipboardSupported").unwrap_or(false)
}

pub fn background_clipboard_status() -> String {
    activity_text("backgroundClipboardStatus")
}

pub fn open_background_clipboard() {
    let _ = call_activity("openBackgroundClipboard", "()V", &[]);
}

pub fn background_clipboard_on() -> bool {
    call_activity_bool("backgroundClipboardOn").unwrap_or(false)
}

pub fn set_background_clipboard(on: bool) {
    let _ = call_activity(
        "setBackgroundClipboard",
        "(Z)V",
        &[jni::objects::JValue::Bool(u8::from(on))],
    );
}

pub fn remote_input_status() -> String {
    activity_text("remoteInputStatus")
}

pub fn open_remote_input_settings() {
    let _ = call_activity("openRemoteInputSettings", "()V", &[]);
}

pub fn remote_input_recovery_on() -> bool {
    call_activity_bool("remoteInputRecoveryOn").unwrap_or(false)
}

pub fn set_remote_input_recovery(on: bool) {
    let _ = call_activity(
        "setRemoteInputRecovery",
        "(Z)V",
        &[jni::objects::JValue::Bool(u8::from(on))],
    );
}

/// What `object` says for `method`, a call with no argument that gives text: empty when it cannot say or has nothing to say. A Java
/// exception left pending is cleared.
fn text_from(known: &Arc<(JavaVM, GlobalRef)>, method: &str) -> String {
    let (vm, object) = known.as_ref();
    attached(vm, |env| {
        let value = env
            .call_method(object.as_obj(), method, "()Ljava/lang/String;", &[])
            .and_then(|answer| answer.l())
            .ok()?;
        if value.as_raw().is_null() {
            return None;
        }
        env.get_string(&JString::from(value)).map(Into::into).ok()
    })
    .unwrap_or_default()
}

/// The store link (`peersus://store/<id>`) the activity was started with, taken once: a window made again (turned, restored)
/// does not show it a second time. `None` when it was started some other way.
pub fn take_store_link() -> Option<String> {
    Some(activity_text("takeStoreLink")).filter(|link| !link.is_empty())
}

/// Store links that reached the running activity (`onNewIntent`), waiting for the loop that tells the owner.
static STORE_LINKS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static STORE_LINK_ARRIVED: OnceLock<tokio::sync::Notify> = OnceLock::new();

/// Woken when a store link reached the running activity.
pub fn store_link_arrived() -> &'static tokio::sync::Notify {
    STORE_LINK_ARRIVED.get_or_init(tokio::sync::Notify::new)
}

/// The store links that arrived since the last time, taken once.
pub fn take_store_links() -> Vec<String> {
    STORE_LINKS
        .lock()
        .map(|mut links| std::mem::take(&mut *links))
        .unwrap_or_default()
}

/// A store link reached the running activity. Only kept here: the owner reads which extension it names, and nothing else of it.
// SAFETY: no other symbol of this name exists in this library; Java's MainActivity declares it with this signature.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_MainActivity_storeLinkOpened<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    link: JString<'local>,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if link.is_null() {
            return;
        }
        let Ok(link) = env.get_string(&link).map(String::from) else {
            return;
        };
        // A few at most: a page cannot fill this by opening links faster than the loop takes them.
        if let Ok(mut links) = STORE_LINKS.lock()
            && links.len() < 8
        {
            links.push(link);
        }
        store_link_arrived().notify_one();
    }));
}

/// Presses of the system's Back that reached the activity, counted: the window closes what is open on top at each change.
static BACK_PRESSES: OnceLock<tokio::sync::watch::Sender<u64>> = OnceLock::new();

fn back_sender() -> &'static tokio::sync::watch::Sender<u64> {
    BACK_PRESSES.get_or_init(|| tokio::sync::watch::channel(0).0)
}

/// What the window watches for Back.
pub fn back_presses() -> tokio::sync::watch::Receiver<u64> {
    back_sender().subscribe()
}

/// Back with nothing open in the window: the activity goes behind the others, as the system's own Back would take it away.
pub fn to_background() {
    let _ = call_activity("toBackground", "()V", &[]);
}

/// The system's Back reached the activity (its back callback, or `onBackPressed` before Android 13).
// SAFETY: no other symbol of this name exists in this library; Java's MainActivity declares it with this signature.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_MainActivity_backPressed<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
) {
    let _ = catch_unwind(|| {
        back_sender().send_modify(|presses| *presses = presses.wrapping_add(1));
    });
}

/// What the activity says for `method`: empty when there is no activity, it cannot say, or it has nothing to say.
fn activity_text(method: &str) -> String {
    let Ok(known) = ACTIVITY.lock().map(|known| known.clone()) else {
        return String::new();
    };
    known.map_or_else(String::new, |known| text_from(&known, method))
}

/// The service when there is one, else the activity: what a call that must work with the window closed is made on.
fn service_or_activity() -> Option<Arc<(JavaVM, GlobalRef)>> {
    SERVICE
        .lock()
        .ok()
        .and_then(|known| known.clone())
        .or_else(|| ACTIVITY.lock().ok().and_then(|known| known.clone()))
}

/// Where this phone is now, as `DeviceLocation.find` says it: asked of the activity when the window is open (only it can show the
/// system's question for the permission), else of the service. Blocks up to the system's wait for a fresh fix: call it off the loop.
pub fn location() -> Result<peersus_app::platform_services::location::Position, &'static str> {
    use peersus_app::platform_services::location::{Position, REFUSED, UNAVAILABLE};
    let known = ACTIVITY
        .lock()
        .ok()
        .and_then(|known| known.clone())
        .or_else(service_or_activity)
        .ok_or(UNAVAILABLE)?;
    let said = text_from(&known, "location");
    // `DeviceLocation.REFUSED`: the person's setting refuses, told apart from no fix.
    if said == "refused" {
        return Err(REFUSED);
    }
    let mut words = said.split(' ');
    let mut number = || words.next().and_then(|word| word.parse::<f64>().ok());
    let (latitude, longitude) = (number().ok_or(UNAVAILABLE)?, number().ok_or(UNAVAILABLE)?);
    // `-` is the system not saying how sure it is.
    let accuracy = number();
    let found_ms: u64 = words
        .next()
        .and_then(|word| word.parse().ok())
        .ok_or(UNAVAILABLE)?;
    Position::new(latitude, longitude, accuracy, found_ms / 1000).ok_or(UNAVAILABLE)
}

/// Replay a batch of pointer and keyboard input on this phone (see `RemoteInput.java`). `Ok(())` when the system took it, `Err` with
/// the application's own words of why not (the accessibility service is off, there is no window and no service to ask).
pub fn inject_remote_input(batch: &str) -> Result<(), String> {
    let known = service_or_activity().ok_or("This phone cannot be controlled right now")?;
    let (vm, object) = known.as_ref();
    // Many batches a second while a device is controlled: one attachment for the thread, not one per batch.
    let refused = with_env(vm, |env| {
        let batch = env.new_string(batch).map_err(|error| error.to_string())?;
        let value = env
            .call_method(
                object.as_obj(),
                "injectRemoteInput",
                "(Ljava/lang/String;)Ljava/lang/String;",
                &[jni::objects::JValue::Object(&batch)],
            )
            .and_then(|answer| answer.l())
            .map_err(|error| error.to_string())?;
        if value.as_raw().is_null() {
            return Ok(None);
        }
        Ok(Some(
            env.get_string(&JString::from(value))
                .map(Into::into)
                .unwrap_or_else(|_| "The input was not replayed".to_owned()),
        ))
    })?;
    refused.map_or(Ok(()), Err)
}

/// Release held input, preserving the pointer only between gestures of the same controller.
pub fn release_remote_input(keep: bool) {
    if let Some(known) = service_or_activity() {
        let (vm, object) = known.as_ref();
        let _ = with_env(vm, |env| {
            env.call_method(
                object.as_obj(),
                "releaseRemoteInput",
                "(Z)V",
                &[jni::objects::JValue::Bool(u8::from(keep))],
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
        });
    }
}

/// The screen of this phone, in pixels, as `(width, height)`; `None` when neither the service nor the activity can say.
pub fn screen_size() -> Option<(u32, u32)> {
    let text = text_from(&service_or_activity()?, "screenSize");
    let (width, height) = text.split_once(' ')?;
    Some((width.parse().ok()?, height.parse().ok()?))
}

/// The device's own first preferred language, as the system says it now; `None` when neither the service nor the activity
/// can say, or the system names none (or names it in a way that is no language tag).
pub fn device_language() -> Option<peersus_core::config::Language> {
    let text = text_from(&service_or_activity()?, "deviceLanguage");
    peersus_core::config::Language::from_locale(&text)
}

/// Run `body` on a thread attached to the Java machine, with the activity. A Java exception left pending is cleared and said as an error:
/// unlike [`activity_text`], a failure is never turned into an empty answer, for a caller that must tell "nothing there" from "could not ask".
pub fn with_activity<T>(
    body: impl FnOnce(&mut JNIEnv<'_>, &JObject<'_>) -> Result<T, String>,
) -> Result<T, String> {
    let known = ACTIVITY
        .lock()
        .map_err(|_| "the activity lock is poisoned")?
        .clone()
        .ok_or("the activity is not known yet")?;
    let (vm, activity) = known.as_ref();
    with_env(vm, |env| body(env, activity.as_obj()))
}

/// What the activity answers for `method`, a call with no argument that gives text or nothing: `Ok(None)` is nothing (null), `Err` is that it
/// could not be asked (no activity, or Java threw).
pub fn call_activity_string(method: &str) -> Result<Option<String>, String> {
    with_activity(|env, activity| {
        let value = env
            .call_method(activity, method, "()Ljava/lang/String;", &[])
            .map_err(|error| {
                let _ = env.exception_clear();
                error.to_string()
            })?
            .l()
            .map_err(|error| error.to_string())?;
        if value.as_raw().is_null() {
            return Ok(None);
        }
        env.get_string(&JString::from(value))
            .map(|text| Some(text.into()))
            .map_err(|error| error.to_string())
    })
}

/// What the service says for `method` when there is one, else the activity: for what either can answer, so that it is answered with the
/// window closed (only the service is alive then) as well as with it open.
fn service_or_activity_text(method: &str) -> String {
    let service = SERVICE.lock().map(|known| known.clone()).ok().flatten();
    match service {
        Some(service) => text_from(&service, method),
        None => activity_text(method),
    }
}

/// The back cameras of this phone as the activity describes them, one line each (id, focal length in tenths of a millimetre, nearest focus
/// in centimetres, 1 if it is the one chosen), tab separated. Empty when the activity cannot say.
pub fn back_cameras() -> String {
    activity_text("backCameras")
}

/// The folder the person chose for the files that come in on their own, as the tree address the system gave, or `None` when none is chosen
/// (or the system no longer lets the program write in it). Text the program never reads as a path.
pub fn saved_folder() -> Option<String> {
    Some(service_or_activity_text("savedFolder")).filter(|tree| !tree.is_empty())
}

/// Start the system's folder picker for the folder of the files that come in on their own. Returns at once; the answer comes through
/// `folderChosen`, and the activity keeps it as the saved folder.
pub fn open_saved_folder_picker() -> Result<(), String> {
    call_activity("chooseSavedFolder", "()V", &[])
}

/// Let go of the saved folder: files that come in ask again where they go.
pub fn forget_saved_folder() -> Result<(), String> {
    call_activity("forgetSavedFolder", "()V", &[])
}

/// Choose the camera by its id; an empty id gives the choice back to the program. The camera in use is started again on the new one.
pub fn choose_camera(id: &str) -> Result<(), String> {
    with_activity(|env, activity| {
        let text = JObject::from(env.new_string(id).map_err(|error| error.to_string())?);
        env.call_method(
            activity,
            "chooseCamera",
            "(Ljava/lang/String;)V",
            &[jni::objects::JValue::Object(&text)],
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
    })
}

/// Start the system's document picker. Returns at once; the files come back through the native methods below.
pub fn open_picker(several: bool) -> Result<(), String> {
    call_activity(
        "chooseDocuments",
        "(Z)V",
        &[jni::objects::JValue::Bool(u8::from(several))],
    )
}

/// Files shared from another application: a selection is opened for them, as the window opens one when the person asks for the picker.
/// Returns its generation; 0 when a picker is already open or an earlier share has not been taken (Java then refuses the share); and -1 when
/// the program has not made its state yet, which is what a share that STARTS the application finds: Java then looks again a moment later.
/// The window learns of the files from the same `Picked` it already holds (see `Picked::take_external`), so nothing else is kept here.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_MainActivity_openSharedSelection<'local>(
    _env: JNIEnv<'local>,
    _activity: JObject<'local>,
) -> jlong {
    catch_unwind(AssertUnwindSafe(|| match PICKED.get() {
        None => -1,
        Some(picked) => picked.opened_externally().unwrap_or(0) as jlong,
    }))
    .unwrap_or(0)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_MainActivity_pickerGeneration<'local>(
    _env: JNIEnv<'local>,
    _activity: JObject<'local>,
) -> jlong {
    catch_unwind(AssertUnwindSafe(|| {
        PICKED
            .get()
            .map_or(0, |picked| picked.generation() as jlong)
    }))
    .unwrap_or(0)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_MainActivity_pickerState<'local>(
    _env: JNIEnv<'local>,
    _activity: JObject<'local>,
    generation: jlong,
) -> jint {
    catch_unwind(AssertUnwindSafe(|| {
        PICKED
            .get()
            .map_or(-1, |picked| picked.producer_state(generation as u64))
    }))
    .unwrap_or(-1)
}

/// The complete selected count is sent before the worker opens any descriptor.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_MainActivity_pickStarted<'local>(
    _env: JNIEnv<'local>,
    _activity: JObject<'local>,
    generation: jlong,
    count: jint,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if count >= 0
            && let Some(picked) = PICKED.get()
        {
            picked.selected_for(generation as u64, count as u32);
        }
    }));
}

/// One chosen file: an open descriptor that now belongs to Rust, and the name the provider gave it.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_MainActivity_pickedFile<'local>(
    mut env: JNIEnv<'local>,
    _activity: JObject<'local>,
    generation: jlong,
    descriptor: jint,
    name: JString<'local>,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let name: String = env.get_string(&name).map(Into::into).unwrap_or_default();
        // SAFETY: Java called `detachFd`, which gave up its ownership, so this descriptor is open, is ours, and nothing
        // else will close it. `File` takes it over and closes it when it is dropped.
        let file = unsafe { File::from_raw_fd(descriptor as RawFd) };
        if let Some(picked) = PICKED.get() {
            picked.file_for(generation as u64, file, name);
        }
        // Before the owner has made the picker's state there is nobody to take the file: it is dropped here, which closes it.
    }));
}

/// The picker is done, with files or without them.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_MainActivity_pickFinished<'local>(
    _env: JNIEnv<'local>,
    _activity: JClass<'local>,
    generation: jlong,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Some(picked) = PICKED.get() {
            picked.finished_for(generation as u64);
        }
    }));
}

/// The folder the person chose, as the tree address Java gives, or null if they backed out.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_MainActivity_folderChosen<'local>(
    mut env: JNIEnv<'local>,
    _activity: JObject<'local>,
    tree: JString<'local>,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let tree: Option<String> = if tree.as_raw().is_null() {
            None
        } else {
            env.get_string(&tree).ok().map(Into::into)
        };
        folders().answered(tree);
    }));
}

/// A file was put in the chosen folder, or could not be.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_FolderDelivery_delivered<'local>(
    mut env: JNIEnv<'local>,
    _class: jni::objects::JClass<'local>,
    transfer: JString<'local>,
    done: jni::sys::jboolean,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let transfer: String = env
            .get_string(&transfer)
            .map(Into::into)
            .unwrap_or_default();
        folders().delivered(transfer, done != 0);
    }));
}

static PLUGIN_IMPORT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A short message of the application's own, shown by the platform over the window in the language the window shows. Called
/// outside any window component, so the language is read from the owner's state, never from the window's context.
fn toast(words: &'static str) {
    // `try_lock`: a caller may still hold the owner's lock (a `let … else` keeps its temporaries), and waiting for it here
    // would never end. Not knowing the language says the message in English.
    let language = super::OWNER
        .try_lock()
        .ok()
        .and_then(|owner| owner.clone())
        .map(|owner| owner.client.snapshot().languages.shown)
        .unwrap_or_default();
    let known = ACTIVITY.lock().ok().and_then(|activity| activity.clone());
    if let Some(known) = known {
        let (vm, activity) = known.as_ref();
        if let Ok(mut env) = vm.attach_current_thread()
            && let Ok(words) = env.new_string(peersus_ui::i18n::translate(language, words))
        {
            let words = JObject::from(words);
            if env
                .call_method(
                    activity.as_obj(),
                    "pluginInstallFailed",
                    "(Ljava/lang/String;)V",
                    &[jni::objects::JValue::Object(&words)],
                )
                .is_err()
            {
                let _ = env.exception_clear();
            }
        }
    }
}

static LANGUAGE_IMPORT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// SAFETY: the Activity detached this descriptor exactly once. Rust owns it from entry, including rejected selections; the
/// file is read to at most a language file's size and handed to the owner as text, which checks it.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_MainActivity_languageChosen(
    _env: JNIEnv<'_>,
    _: JClass<'_>,
    descriptor: jint,
) {
    // SAFETY: a descriptor at or above zero was transferred by ParcelFileDescriptor.detachFd, never borrowed; the codes
    // below zero are not descriptors.
    let file = (descriptor >= 0).then(|| unsafe { File::from_raw_fd(descriptor) });
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // The Activity's codes for what went wrong before a file could be handed over (MainActivity.LANGUAGE_*).
        let Some(file) = file else {
            match descriptor {
                -1 => toast("The language file could not be read."),
                -2 => toast("The file picker could not be opened"),
                -3 => toast("Another language file is being imported"),
                _ => {}
            }
            return;
        };
        let Some(owner) = super::OWNER.lock().ok().and_then(|owner| owner.clone()) else {
            toast("The application is not ready to import a language file");
            return;
        };
        if LANGUAGE_IMPORT.swap(true, std::sync::atomic::Ordering::AcqRel) {
            toast("Another language file is being imported");
            return;
        }
        struct ImportGuard;
        impl Drop for ImportGuard {
            fn drop(&mut self) {
                LANGUAGE_IMPORT.store(false, std::sync::atomic::Ordering::Release);
            }
        }
        let guard = ImportGuard;
        let client = owner.client.clone();
        drop(owner._executor.spawn(async move {
            let _guard = guard;
            let text = tokio::task::spawn_blocking(move || {
                use std::io::Read;
                let mut text = String::new();
                file.take(peersus_api::language::MAX_FILE_BYTES as u64 + 1)
                    .read_to_string(&mut text)
                    .ok()
                    .map(|_| text)
            })
            .await;
            let Ok(Some(text)) = text else {
                toast("The language file could not be read.");
                return;
            };
            match client
                .command(peersus_core::model::Command::LanguageImport { text })
                .await
            {
                Ok(_) => toast("The language file was imported."),
                Err(words) => toast(words),
            }
        }));
    }));
}

/// SAFETY: the Activity detached this descriptor exactly once. Rust owns it from entry,
/// including rejected selections; untrusted provider names never become filesystem paths.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_MainActivity_pluginChosen(
    mut env: JNIEnv<'_>,
    _: JClass<'_>,
    descriptor: jint,
    name: JString<'_>,
) {
    if descriptor < 0 {
        return;
    }
    // SAFETY: ownership was transferred by ParcelFileDescriptor.detachFd, never borrowed.
    let file = unsafe { File::from_raw_fd(descriptor) };
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let name: String = env
            .get_string(&name)
            .ok()
            .map(Into::into)
            .unwrap_or_default();
        let Some(owner) = super::OWNER.lock().ok().and_then(|owner| owner.clone()) else {
            toast("The application is not ready to open plugins");
            return;
        };
        if PLUGIN_IMPORT.swap(true, std::sync::atomic::Ordering::AcqRel) {
            toast("Another plugin archive is being opened");
            return;
        }
        struct ImportGuard;
        impl Drop for ImportGuard {
            fn drop(&mut self) {
                PLUGIN_IMPORT.store(false, std::sync::atomic::Ordering::Release);
            }
        }
        let guard = ImportGuard;
        let scratch = owner.socket.parent().map(std::path::Path::to_path_buf);
        let client = owner.client.clone();
        drop(owner._executor.spawn(async move {
            let _guard = guard;
            let source = tokio::task::spawn_blocking(move || {
                peersus_app::archive::keep_source(
                    file,
                    &name,
                    &scratch.ok_or(peersus_app::archive::Refused(
                        "The package store is unavailable",
                    ))?,
                )
            })
            .await;
            match source {
                Ok(Ok(source)) => {
                    let Some(path) = peersus_core::model::SourceDir::new(source.path()) else {
                        toast("The selected archive could not be staged");
                        return;
                    };
                    // StageInstall retains its own validated unpacked package. The raw
                    // source below drops after staging, not after a future confirmation.
                    if let Err(words) = client
                        .command(peersus_core::model::Command::StageInstall { source: path })
                        .await
                    {
                        toast(words);
                    }
                    drop(source);
                }
                Ok(Err(refusal)) => toast(refusal.0),
                Err(_) => toast("The selected archive could not be read"),
            }
        }));
    }));
}

/// Access encrypted platform storage; all consent is rendered by the common Freya UI.
pub fn extension_credential_access(
    caller: &str,
    origin: &str,
    operation: i32,
    supplied: &[u8],
) -> Result<zeroize::Zeroizing<Vec<u8>>, &'static str> {
    let known = ACTIVITY
        .lock()
        .map_err(|_| "The activity is unavailable")?
        .clone()
        .ok_or("The activity is unavailable")?;
    let (vm, activity) = known.as_ref();
    let mut env = vm
        .attach_current_thread()
        .map_err(|_| "The activity is unavailable")?;
    let result = (|| {
        let caller = JObject::from(env.new_string(caller).map_err(|_| ())?);
        let origin = JObject::from(env.new_string(origin).map_err(|_| ())?);
        let supplied = env.byte_array_from_slice(supplied).map_err(|_| ())?;
        let supplied_object = JObject::from(supplied);
        let result = env
            .call_method(
                activity.as_obj(),
                "extensionCredentialAccess",
                "(Ljava/lang/String;Ljava/lang/String;I[B)[B",
                &[
                    jni::objects::JValue::Object(&caller),
                    jni::objects::JValue::Object(&origin),
                    jni::objects::JValue::Int(operation),
                    jni::objects::JValue::Object(&supplied_object),
                ],
            )
            .and_then(|value| value.l())
            .map_err(|_| ())?;
        let result = jni::objects::JByteArray::from(result);
        let length = env.get_array_length(&result).map_err(|_| ())?;
        if !(0..=8192).contains(&length) {
            return Err(());
        }
        let bytes = env
            .convert_byte_array(&result)
            .map(zeroize::Zeroizing::new)
            .map_err(|_| ())?;
        env.set_byte_array_region(&result, 0, &vec![0; length as usize])
            .map_err(|_| ())?;
        Ok(bytes)
    })();
    result.map_err(|()| {
        let _ = env.exception_clear();
        "The encrypted credential store is unavailable; configure the API key first"
    })
}

/// Ask the activity to replace the clipboard text. The platform policy decides whether it may; the answer comes back
/// through the native callback of the clipboard facility, never as a return value.
pub fn write_extension_clipboard(
    request: i64,
    remaining: i64,
    text: &str,
) -> Result<(), &'static str> {
    let known = ACTIVITY
        .lock()
        .map_err(|_| "The activity is unavailable")?
        .clone()
        .ok_or("The activity is unavailable")?;
    let (vm, activity) = known.as_ref();
    let mut env = vm
        .attach_current_thread()
        .map_err(|_| "The activity is unavailable")?;
    let text = JObject::from(env.new_string(text).map_err(|_| {
        let _ = env.exception_clear();
        "Clipboard is unavailable"
    })?);
    env.call_method(
        activity.as_obj(),
        "writeExtensionClipboard",
        "(JJLjava/lang/String;)V",
        &[
            jni::objects::JValue::Long(request),
            jni::objects::JValue::Long(remaining),
            jni::objects::JValue::Object(&text),
        ],
    )
    .map_err(|_| {
        let _ = env.exception_clear();
        "Clipboard is unavailable"
    })?;
    Ok(())
}
