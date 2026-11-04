//! Bundled plugins run in the private :plugins process, never on the owner's executor.
#![allow(unsafe_code)]
use jni::{
    JNIEnv, JavaVM,
    objects::{GlobalRef, JObject, JString},
    sys::{jboolean, jlong, jstring},
};
use peersus_clipboard::plugin::{Clipboard, Reading};
use peersus_core::{
    model::{Command, PeerId},
    plugins::PluginId,
};
use peersus_plugin_host::supervisor::{Bootstrap, Launcher, Lifecycle};
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
static CHOICES: Mutex<Vec<(u64, PeerId, Instant)>> = Mutex::new(Vec::new());
const MAX_TEXT: usize = 64 * 1024;

pub fn packages(directory: &std::path::Path) -> peersus_app::plugin_manager::Manager {
    // Android owns the mode of files/ (commonly 0771). Do not chmod that system
    // directory or weaken desktop checks: the manager creates this child as 0700.
    peersus_app::plugin_manager::Manager::new(&directory.join("extensions"))
}

pub fn registry(directory: &std::path::Path) -> Result<peersus_app::discovery::Discovered, String> {
    let manifest = serde_json::from_str(include_str!("../../peersus-clipboard/plugin.json"))
        .map_err(|_| "The bundled plugin declaration is invalid".to_owned())?;
    let mut registry = peersus_app::plugin_host::Registry::default();
    registry
        .register(peersus_app::plugin_host::PluginSpec {
            id: PluginId::new("clipboard").map_err(|_| "The bundled plugin identity is invalid")?,
            manifest,
            program: None,
            arguments: Vec::new(),
            restart_policy: None,
        })
        .map_err(|_| "The bundled plugin could not be registered".to_owned())?;
    let mut found = packages(directory).load();
    let external = std::mem::take(&mut found.registry);
    for spec in external.iter() {
        if registry.register(spec.clone()).is_err() {
            found.problems.push(peersus_app::discovery::Problem {
                plugin: spec.id.as_str().to_owned(),
                reason: "It clashes with a bundled plugin",
            });
            if registry.find(&spec.id).is_none() {
                found.held_back.push(peersus_app::discovery::HeldBack {
                    id: spec.id.clone(),
                    standing: peersus_core::model::Standing::Broken,
                    manifest: spec.manifest.clone(),
                    why: Some("It clashes with a bundled plugin"),
                    program: None,
                });
            }
        }
    }
    if let Some(installed) = &mut found.installed {
        installed.extend(
            registry
                .iter()
                .filter(|spec| spec.program.is_none())
                .map(|spec| spec.id.clone()),
        );
    }
    found.registry = registry;
    Ok(found)
}

pub struct AndroidLauncher;

fn failure() -> io::Error {
    io::Error::other("The private plugin service is unavailable")
}

impl Launcher for AndroidLauncher {
    fn launch(&self, bootstrap: Bootstrap) -> io::Result<Box<dyn Lifecycle>> {
        launch_android(bootstrap, None, false)
    }
}

pub struct AndroidModules;
struct ModuleLauncher {
    module: std::path::PathBuf,
    native: bool,
}
impl peersus_plugin_host::module::ModuleFactory for AndroidModules {
    fn launcher(
        &self,
        execution: peersus_plugin_api::manifest::Execution,
        module: &std::path::Path,
    ) -> io::Result<Arc<dyn Launcher>> {
        use peersus_plugin_api::manifest::{EXECUTION_ABI_VERSION, Execution};
        if !module.is_absolute() {
            return Err(failure());
        }
        let native = match execution {
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

/// Owns the Java-side permit before any non-cancellable blocking JNI work is queued.
/// Drop revokes it synchronously; an already queued Message cannot outlive its caller.
struct ClipboardCall {
    platform: Platform,
    id: i64,
}
impl ClipboardCall {
    fn begin(platform: &Platform, deadline: Instant) -> Option<Self> {
        let id = i64::try_from(SEQUENCE.fetch_add(1, Ordering::Relaxed))
            .ok()
            .filter(|id| *id > 0)?;
        let (vm, object) = platform.as_ref();
        let mut env = vm.attach_current_thread().ok()?;
        let remaining = deadline
            .checked_duration_since(Instant::now())?
            .as_millis()
            .min(2500);
        if remaining == 0 {
            return None;
        }
        let accepted = env
            .call_method(
                object.as_obj(),
                "beginClipboardRequest",
                "(JJ)Z",
                &[
                    jni::objects::JValue::Long(id),
                    jni::objects::JValue::Long(remaining as i64),
                ],
            )
            .and_then(|value| value.z());
        match accepted {
            Ok(true) => Some(Self {
                platform: Arc::clone(platform),
                id,
            }),
            _ => {
                let _ = env.exception_clear();
                None
            }
        }
    }
}
impl Drop for ClipboardCall {
    fn drop(&mut self) {
        let (vm, object) = self.platform.as_ref();
        if let Ok(mut env) = vm.attach_current_thread()
            && env
                .call_method(
                    object.as_obj(),
                    "cancelClipboardRequest",
                    "(J)V",
                    &[jni::objects::JValue::Long(self.id)],
                )
                .is_err()
        {
            let _ = env.exception_clear();
        }
    }
}

struct ActivityClipboard {
    platform: Platform,
}
impl ActivityClipboard {
    async fn read(&self, observe: bool, context: Option<u64>) -> Option<String> {
        let budget = if observe {
            Duration::from_secs(1)
        } else {
            Duration::from_millis(2500)
        };
        let call = ClipboardCall::begin(&self.platform, Instant::now() + budget)?;
        let platform = Arc::clone(&self.platform);
        let id = call.id;
        let result = tokio::task::spawn_blocking(move || {
            let (vm, object) = platform.as_ref();
            let mut env = vm.attach_current_thread().ok()?;
            let context = match context {
                Some(context) => Some(i64::try_from(context).ok()?),
                None => None,
            };
            let result = match context {
                Some(context) => env.call_method(
                    object.as_obj(),
                    "selectedClipboard",
                    "(JJ)Ljava/lang/String;",
                    &[
                        jni::objects::JValue::Long(id),
                        jni::objects::JValue::Long(context),
                    ],
                ),
                None => env.call_method(
                    object.as_obj(),
                    if observe {
                        "observeClipboard"
                    } else {
                        "readClipboard"
                    },
                    "(J)Ljava/lang/String;",
                    &[jni::objects::JValue::Long(id)],
                ),
            };
            let value = match result {
                Ok(value) => value.l().ok()?,
                Err(_) => {
                    let _ = env.exception_clear();
                    return None;
                }
            };
            if value.is_null() {
                return None;
            }
            let value = JString::from(value);
            let text: String = env.get_string(&value).ok()?.into();
            (text.len() <= MAX_TEXT + usize::from(observe)).then_some(text)
        })
        .await
        .ok()
        .flatten();
        drop(call);
        result
    }
}
impl Clipboard for ActivityClipboard {
    async fn text(&mut self) -> Option<String> {
        self.read(false, None).await
    }
    async fn selected_text(&mut self, context: u64) -> Option<String> {
        self.read(false, Some(context)).await
    }
    async fn selected_valid(&mut self, context: u64) -> bool {
        let Ok(context) = i64::try_from(context) else {
            return false;
        };
        let Some(call) =
            ClipboardCall::begin(&self.platform, Instant::now() + Duration::from_secs(1))
        else {
            return false;
        };
        let platform = Arc::clone(&self.platform);
        let id = call.id;
        let result = tokio::task::spawn_blocking(move || {
            let (vm, object) = platform.as_ref();
            let Ok(mut env) = vm.attach_current_thread() else {
                return false;
            };
            match env.call_method(
                object.as_obj(),
                "selectedClipboardValid",
                "(JJ)Z",
                &[
                    jni::objects::JValue::Long(id),
                    jni::objects::JValue::Long(context),
                ],
            ) {
                Ok(value) => value.z().unwrap_or(false),
                Err(_) => {
                    let _ = env.exception_clear();
                    false
                }
            }
        })
        .await
        .unwrap_or(false);
        drop(call);
        result
    }
    async fn reset_observation(&mut self) -> bool {
        let Some(call) =
            ClipboardCall::begin(&self.platform, Instant::now() + Duration::from_millis(2500))
        else {
            return false;
        };
        let platform = Arc::clone(&self.platform);
        let id = call.id;
        let result = tokio::task::spawn_blocking(move || {
            let (vm, object) = platform.as_ref();
            let Ok(mut env) = vm.attach_current_thread() else {
                return false;
            };
            match env.call_method(
                object.as_obj(),
                "resetClipboard",
                "(J)Z",
                &[jni::objects::JValue::Long(id)],
            ) {
                Ok(value) => value.z().unwrap_or(false),
                Err(_) => {
                    let _ = env.exception_clear();
                    false
                }
            }
        })
        .await
        .unwrap_or(false);
        drop(call);
        result
    }
    async fn observe(&mut self) -> Reading {
        match self.read(true, None).await {
            Some(text) if text.starts_with('c') => Reading::Changed(text[1..].to_owned()),
            Some(text) if text.starts_with('s') => Reading::Snapshot(text[1..].to_owned()),
            _ => Reading::Unavailable,
        }
    }
    async fn set_text(&mut self, text: &str) -> bool {
        self.set_text_until(text, Instant::now() + Duration::from_millis(2500))
            .await
    }
    async fn set_text_until(&mut self, text: &str, deadline: Instant) -> bool {
        if text.len() > MAX_TEXT {
            return false;
        }
        let Some(call) = ClipboardCall::begin(&self.platform, deadline) else {
            return false;
        };
        let platform = Arc::clone(&self.platform);
        let id = call.id;
        let text = text.to_owned();
        let result = tokio::task::spawn_blocking(move || {
            let (vm, object) = platform.as_ref();
            let Ok(mut env) = vm.attach_current_thread() else {
                return false;
            };
            let Ok(text) = env.new_string(&text) else {
                return false;
            };
            let text = JObject::from(text);
            match env.call_method(
                object.as_obj(),
                "writeClipboard",
                "(Ljava/lang/String;J)Z",
                &[
                    jni::objects::JValue::Object(&text),
                    jni::objects::JValue::Long(id),
                ],
            ) {
                Ok(value) => value.z().unwrap_or(false),
                Err(_) => {
                    let _ = env.exception_clear();
                    false
                }
            }
        })
        .await
        .unwrap_or(false);
        drop(call);
        result
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

// SAFETY: Only PluginService in :plugins invokes this native method; JNI strings are copied before asynchronous use.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_PluginService_nativeRun(
    mut env: JNIEnv<'_>,
    object: JObject<'_>,
    socket: JString<'_>,
    data: JString<'_>,
    token: JString<'_>,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let isolated = env
            .call_method(&object, "isPluginProcess", "()Z", &[])
            .and_then(|value| value.z());
        if !matches!(isolated, Ok(true)) {
            let _ = env.exception_clear();
            return;
        }
        let Some(platform) = platform(&mut env, &object) else {
            return;
        };
        let Ok(socket) = env.get_string(&socket).map(String::from) else {
            return;
        };
        let Ok(data) = env.get_string(&data).map(String::from) else {
            return;
        };
        let Ok(token) = env.get_string(&token).map(String::from) else {
            return;
        };
        if socket.len() > 4096 || data.len() > 4096 || token.len() > 256 {
            return;
        }
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        runtime.block_on(peersus_clipboard::session::run(
            ActivityClipboard { platform },
            Bootstrap {
                socket: socket.into(),
                data_socket: data.into(),
                token,
            },
        ));
    }));
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
        let plugin = PluginId::new("clipboard").ok()?;
        let send = peersus_plugin_api::permission::Permission::new("host", "peers.send")?;
        let clipboard =
            peersus_plugin_api::permission::Permission::new("clipboard", "clipboard.use")?;
        let mut choices = CHOICES.try_lock().ok()?;
        choices.clear();
        let mut public = Vec::new();
        for peer in &snapshot.peers {
            if public.len() >= 64 {
                break;
            }
            if !peer.online
                || !peer
                    .grants
                    .get(&plugin)
                    .is_some_and(|grants| grants.allows(&send) && grants.allows(&clipboard))
            {
                continue;
            }
            let choice = SEQUENCE.fetch_add(1, Ordering::Relaxed);
            if choice > i64::MAX as u64 {
                continue;
            }
            choices.push((choice, peer.id.clone(), Instant::now()));
            public.push(serde_json::json!({ "choice": choice, "name": peer.name }));
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
    _: JNIEnv<'_>,
    _: JObject<'_>,
    choice: jlong,
    context: jlong,
) -> jboolean {
    catch_unwind(|| {
        let context = u64::try_from(context).ok().filter(|context| *context > 0)?;
        let owner = super::OWNER.try_lock().ok()?.clone()?;
        let mut choices = CHOICES.try_lock().ok()?;
        let position = choices.iter().position(|(id, _, at)| {
            *id == choice as u64 && at.elapsed() < Duration::from_secs(30)
        })?;
        let (_, peer, _) = choices.swap_remove(position);
        choices.clear();
        drop(choices);
        let client = owner.client.clone();
        let platform = PLATFORM.try_lock().ok()?.clone()?;
        drop(owner._executor.spawn(async move {
            let result = tokio::time::timeout(
                Duration::from_secs(3),
                client.command(Command::InvokeActionWithContext {
                    context,
                    plugin: PluginId::new("clipboard").expect("static plugin identity"),
                    action: "share".into(),
                    peer: Some(peer),
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
    })
    .ok()
    .flatten()
    .unwrap_or(0)
}
