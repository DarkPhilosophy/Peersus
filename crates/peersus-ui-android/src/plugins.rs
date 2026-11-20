//! Bundled plugins run in the private :plugins process, never on the owner's executor.
#![allow(unsafe_code)]
use jni::{
    JNIEnv, JavaVM,
    objects::{GlobalRef, JObject, JString},
    sys::{jboolean, jlong, jstring},
};
use peersus_core::{
    model::{Command, PeerId},
    plugins::PluginId,
};
use peersus_plugin::supervisor::{Bootstrap, Launcher, Lifecycle};
use std::{
    collections::BTreeMap,
    future::Future,
    io,
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::watch;

type Platform = Arc<(JavaVM, GlobalRef)>;
static PLATFORM: Mutex<Option<Platform>> = Mutex::new(None);
static GENERATIONS: Mutex<BTreeMap<u64, watch::Sender<bool>>> = Mutex::new(BTreeMap::new());
static SEQUENCE: AtomicU64 = AtomicU64::new(1);
/// One offered destination for shared text: which action, of which plugin, to which device.
struct Choice {
    id: u64,
    plugin: PluginId,
    action: String,
    peer: PeerId,
    offered: Instant,
}
static CHOICES: Mutex<Vec<Choice>> = Mutex::new(Vec::new());
const MAX_TEXT: usize = 64 * 1024;

pub fn packages(directory: &std::path::Path) -> peersus_app::plugin_manager::Manager {
    // Android owns the mode of files/ (commonly 0771). Do not chmod that system
    // directory or weaken desktop checks: the manager creates this child as 0700.
    peersus_app::plugin_manager::Manager::new(&directory.join("extensions"))
}

pub fn registry(directory: &std::path::Path) -> Result<peersus_app::discovery::Discovered, String> {
    // Nothing is built into the application: every plugin is installed by the person, from the store or by hand.
    Ok(packages(directory).load())
}

fn failure() -> io::Error {
    io::Error::other("The private plugin service is unavailable")
}

pub struct AndroidModules;
struct ModuleLauncher {
    module: std::path::PathBuf,
    native: bool,
}
impl peersus_plugin::module::ModuleFactory for AndroidModules {
    fn launcher(
        &self,
        manifest: &peersus_api::manifest::Manifest,
        module: &std::path::Path,
    ) -> io::Result<Arc<dyn Launcher>> {
        use peersus_api::manifest::{EXECUTION_ABI_VERSION, Execution};
        if !module.is_absolute() {
            return Err(failure());
        }
        let native = match manifest.execution {
            Execution::Wasm {
                abi: EXECUTION_ABI_VERSION,
            } => false,
            #[cfg(feature = "native-extensions")]
            Execution::NativeLibrary {
                abi: EXECUTION_ABI_VERSION,
            } => true,
            _ => return Err(failure()),
        };
        Ok(Arc::new(ModuleLauncher {
            module: module.to_owned(),
            native,
        }))
    }
}
impl Launcher for ModuleLauncher {
    fn launch(&self, bootstrap: Bootstrap) -> io::Result<Box<dyn Lifecycle>> {
        launch_android(bootstrap, Some(&self.module), self.native)
    }
}

fn launch_android(
    bootstrap: Bootstrap,
    module: Option<&std::path::Path>,
    native: bool,
) -> io::Result<Box<dyn Lifecycle>> {
    let platform = PLATFORM
        .lock()
        .map_err(|_| failure())?
        .clone()
        .ok_or_else(failure)?;
    let id = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let java_id = i64::try_from(id)
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(failure)?;
    let (ended, state) = watch::channel(false);
    GENERATIONS.lock().map_err(|_| failure())?.insert(id, ended);
    let result = (|| {
        let (vm, object) = platform.as_ref();
        let mut env = vm.attach_current_thread().map_err(|_| failure())?;
        let socket = JObject::from(
            env.new_string(bootstrap.socket.to_str().ok_or_else(failure)?)
                .map_err(|_| failure())?,
        );
        let data = JObject::from(
            env.new_string(bootstrap.data_socket.to_str().ok_or_else(failure)?)
                .map_err(|_| failure())?,
        );
        let token = JObject::from(env.new_string(&bootstrap.token).map_err(|_| failure())?);
        let module = module
            .map(|module| {
                env.new_string(module.to_str().ok_or_else(failure)?)
                    .map(JObject::from)
                    .map_err(|_| failure())
            })
            .transpose()?;
        let mut arguments = vec![jni::objects::JValue::Long(java_id)];
        if let Some(module) = module.as_ref() {
            arguments.push(jni::objects::JValue::Object(module));
        }
        arguments.extend([
            jni::objects::JValue::Object(&socket),
            jni::objects::JValue::Object(&data),
            jni::objects::JValue::Object(&token),
        ]);
        let (method, signature) = if module.is_some() {
            (
                if native { "launchNative" } else { "launchWasm" },
                "(JLjava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)Z",
            )
        } else {
            (
                "launch",
                "(JLjava/lang/String;Ljava/lang/String;Ljava/lang/String;)Z",
            )
        };
        match env
            .call_method(object.as_obj(), method, signature, &arguments)
            .and_then(|value| value.z())
        {
            Ok(true) => Ok(()),
            _ => {
                let _ = env.exception_clear();
                Err(failure())
            }
        }
    })();
    if result.is_err() {
        if let Ok(mut generations) = GENERATIONS.lock() {
            generations.remove(&id);
        }
        return Err(failure());
    }
    Ok(Box::new(Generation {
        id,
        platform,
        state,
    }))
}

struct Generation {
    id: u64,
    platform: Platform,
    state: watch::Receiver<bool>,
}
impl Generation {
    fn terminate(&self) {
        if *self.state.borrow() {
            return;
        }
        let (vm, object) = self.platform.as_ref();
        if let Ok(mut env) = vm.attach_current_thread()
            && env
                .call_method(
                    object.as_obj(),
                    "stop",
                    "(J)V",
                    &[jni::objects::JValue::Long(self.id as i64)],
                )
                .is_err()
        {
            let _ = env.exception_clear();
        }
    }
    async fn ended(&mut self) {
        while !*self.state.borrow_and_update() {
            if self.state.changed().await.is_err() {
                break;
            }
        }
    }
}
impl Lifecycle for Generation {
    fn wait(&mut self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(self.ended())
    }
    fn stop(&mut self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        self.terminate();
        Box::pin(self.ended())
    }
}
impl Drop for Generation {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn platform(env: &mut JNIEnv<'_>, object: &JObject<'_>) -> Option<Platform> {
    Some(Arc::new((
        env.get_java_vm().ok()?,
        env.new_global_ref(object).ok()?,
    )))
}

// SAFETY: Android calls these symbols with JNI references valid for the duration of each call.
// Every callback contains panics; owned cross-thread references are GlobalRef, never local JNI references.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_PluginBridge_nativeAttach(
    mut env: JNIEnv<'_>,
    _: JObject<'_>,
    object: JObject<'_>,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let (Some(platform), Ok(mut slot)) = (platform(&mut env, &object), PLATFORM.lock()) {
            *slot = Some(platform);
        }
    }));
}

// SAFETY: The same-UID private Binder lifecycle supplies the generation; unknown/stale values are ignored.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_PluginBridge_nativeExited(
    _: JNIEnv<'_>,
    _: JObject<'_>,
    generation: jlong,
) {
    let _ = catch_unwind(|| {
        if let Ok(mut generations) = GENERATIONS.lock()
            && let Some(ended) = generations.remove(&(generation as u64))
        {
            ended.send_replace(true);
        }
    });
}

/// SAFETY: path belongs to the host bootstrap, never to a guest; ownership of the
/// connected descriptor transfers once to ParcelFileDescriptor.adoptFd in the owner.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_ModuleBridge_nativeConnect(
    mut env: JNIEnv<'_>,
    _: JObject<'_>,
    path: JString<'_>,
) -> jni::sys::jint {
    use std::os::fd::IntoRawFd;
    let connected = catch_unwind(AssertUnwindSafe(|| {
        let path: String = env.get_string(&path).ok()?.into();
        if !path.starts_with('/') || path.len() > 4096 {
            return None;
        }
        std::os::unix::net::UnixStream::connect(path)
            .ok()
            .map(IntoRawFd::into_raw_fd)
    }))
    .ok()
    .flatten();
    connected.unwrap_or_else(|| {
        let _ = env.throw_new(
            "java/io/IOException",
            "Plugin descriptor could not be connected",
        );
        -1
    })
}

// SAFETY: Only names and expiring random-independent local choice numbers cross JNI, never device identity keys.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_PluginBridge_nativeTargets(
    env: JNIEnv<'_>,
    _: JObject<'_>,
) -> jstring {
    catch_unwind(AssertUnwindSafe(|| {
        let owner = super::OWNER.try_lock().ok()?.clone()?;
        let snapshots = owner.client.subscribe();
        let snapshot = snapshots.borrow();
        let send = peersus_api::permission::Permission::new("host", "peers.send")?;
        let mut choices = CHOICES.try_lock().ok()?;
        choices.clear();
        let mut public = Vec::new();
        // Every installed plugin that declares a shared-text action is offered, none is named here.
        for plugin in &snapshot.plugins {
            for action in plugin
                .manifest
                .contributions
                .actions
                .iter()
                .filter(|action| {
                    action.input == peersus_api::contributions::ActionInput::SharedText
                        && action.needs_device
                })
            {
                // What the action itself requires, and the application's permission to send to a device.
                let required = action.capability.as_deref().and_then(|name| {
                    peersus_api::permission::Permission::new(plugin.id.as_str(), name)
                });
                for peer in &snapshot.peers {
                    if public.len() >= 64 {
                        break;
                    }
                    if !peer.online
                        || !peer.grants.get(&plugin.id).is_some_and(|grants| {
                            grants.allows(&send)
                                && required
                                    .as_ref()
                                    .is_none_or(|required| grants.allows(required))
                        })
                    {
                        continue;
                    }
                    let choice = SEQUENCE.fetch_add(1, Ordering::Relaxed);
                    if choice > i64::MAX as u64 {
                        continue;
                    }
                    choices.push(Choice {
                        id: choice,
                        plugin: plugin.id.clone(),
                        action: action.id.clone(),
                        peer: peer.id.clone(),
                        offered: Instant::now(),
                    });
                    public.push(serde_json::json!({
                        "choice": choice,
                        "name": peer.name,
                    }));
                }
            }
        }
        env.new_string(serde_json::Value::Array(public).to_string())
            .ok()
            .map(JString::into_raw)
    }))
    .ok()
    .flatten()
    .unwrap_or(std::ptr::null_mut())
}

// SAFETY: A user-selected expiring choice is resolved in the owner process; InvokeAction rechecks authorization.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_PluginBridge_nativeShare(
    mut env: JNIEnv<'_>,
    _: JObject<'_>,
    choice: jlong,
    text: JString<'_>,
) -> jboolean {
    catch_unwind(AssertUnwindSafe(|| {
        let text: String = env.get_string(&text).ok()?.into();
        // The platform redeems at most this much, so a larger text is refused here, before it is held anywhere.
        if text.is_empty() || text.len() > MAX_TEXT {
            return None;
        }
        let owner = super::OWNER.try_lock().ok()?.clone()?;
        let mut choices = CHOICES.try_lock().ok()?;
        let position = choices.iter().position(|entry| {
            entry.id == choice as u64 && entry.offered.elapsed() < Duration::from_secs(30)
        })?;
        let Choice {
            plugin,
            action,
            peer,
            ..
        } = choices.swap_remove(position);
        choices.clear();
        drop(choices);
        let client = owner.client.clone();
        let platform = PLATFORM.try_lock().ok()?.clone()?;
        drop(owner._executor.spawn(async move {
            let result = tokio::time::timeout(
                Duration::from_secs(3),
                client.command(Command::InvokeSharedText {
                    plugin,
                    action,
                    peer,
                    text,
                }),
            )
            .await;
            if !matches!(result, Ok(Ok(_))) {
                let (vm, object) = platform.as_ref();
                if let Ok(mut env) = vm.attach_current_thread()
                    && env
                        .call_method(object.as_obj(), "shareFailed", "()V", &[])
                        .is_err()
                {
                    let _ = env.exception_clear();
                }
            }
        }));
        Some(1)
    }))
    .ok()
    .flatten()
    .unwrap_or(0)
}
