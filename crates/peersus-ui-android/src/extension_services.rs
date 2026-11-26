//! Trusted Android adapter. Provider protocols remain in sandboxed extensions.
use crate::extension_clipboard::{self, AndroidBoard};
use peersus_api::message::Outcome;
use peersus_app::platform_services::{
    CLIPBOARD_SERVICE, Call, InputLease, PlatformServices, Response, SelectedInput, replay_input,
};
use peersus_core::plugins::PluginId;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use zeroize::Zeroizing;

struct InputContext {
    caller: PluginId,
    generation: u64,
    expires: std::time::Instant,
    input: peersus_app::platform_services::SelectedInput,
}
struct State {
    next: u64,
    generations: HashMap<PluginId, u64>,
    clipboard: peersus_app::clipboard_handles::Handles,
    contexts: HashMap<u64, InputContext>,
}
impl State {
    fn new() -> Self {
        Self {
            next: 0,
            generations: HashMap::new(),
            // The Android policy carries less than the application allows, so it says so here: a larger request is
            // refused when it begins and never after everything was sent.
            clipboard: peersus_app::clipboard_handles::Handles::bounded(
                extension_clipboard::MAX_BYTES,
            ),
            contexts: HashMap::new(),
        }
    }
    fn generation(&self, caller: &PluginId) -> u64 {
        self.generations.get(caller).copied().unwrap_or_default()
    }
    fn handle(&mut self, prefix: &str) -> Result<String, &'static str> {
        self.next = self
            .next
            .checked_add(1)
            .ok_or("Platform identifiers exhausted")?;
        Ok(format!("{prefix}-{}", self.next))
    }
    fn current(&self, caller: &PluginId, generation: u64) -> Result<(), &'static str> {
        if self.generation(caller) != generation {
            Err("The extension was stopped")
        } else {
            Ok(())
        }
    }
}

type Controller = (PluginId, Option<String>);

#[derive(Clone)]
pub struct AndroidServices {
    state: Arc<Mutex<State>>,
    controller: Arc<Mutex<Option<Controller>>>,
    files: peersus_app::platform_services::file_http::FileHttpServices,
    prompts: Arc<peersus_app::extension_prompts::ExtensionPrompts>,
    notifications: Arc<peersus_app::notifications::Notifications>,
}
impl AndroidServices {
    pub fn new(scratch: PathBuf) -> Self {
        let prompts = Arc::new(peersus_app::extension_prompts::ExtensionPrompts::default());
        let notifications = Arc::new(peersus_app::notifications::Notifications::default());
        Self {
            state: Arc::new(Mutex::new(State::new())),
            controller: Arc::default(),
            files: peersus_app::platform_services::file_http::FileHttpServices::new(
                scratch.clone(),
                Arc::new(AndroidInteraction {
                    prompts: prompts.clone(),
                }),
                notifications.clone(),
            )
            .with_public_read_approvals(scratch),
            prompts,
            notifications,
        }
    }
    fn state(&self) -> Result<std::sync::MutexGuard<'_, State>, &'static str> {
        self.state
            .lock()
            .map_err(|_| "Platform services are unavailable")
    }
    async fn execute(&self, call: Call, generation: u64) -> Result<Value, &'static str> {
        match (call.service.as_str(), call.method.as_str()) {
            (CLIPBOARD_SERVICE, "selected") => {
                let context = call
                    .args
                    .get("context")
                    .and_then(Value::as_u64)
                    .ok_or("Missing selected input context")?;
                let mut state = self.state()?;
                // Redeeming staged input shares its state with every other facility, so it stays here; holding the text
                // afterwards is the shared module's, under the same bounds as a read.
                state.current(&call.caller, generation)?;
                state.clipboard.may_capture(&call.caller)?;
                // Look before consuming: a context staged for a file must stay usable for a file.
                state
                    .contexts
                    .get(&context)
                    .filter(|entry| {
                        entry.caller == call.caller
                            && entry.generation == generation
                            && entry.expires > std::time::Instant::now()
                            && matches!(
                                entry.input,
                                peersus_app::platform_services::SelectedInput::Text(_)
                            )
                    })
                    .ok_or("Selected input is unavailable")?;
                let Some(peersus_app::platform_services::SelectedInput::Text(text)) =
                    state.contexts.remove(&context).map(|entry| entry.input)
                else {
                    return Err("Selected input is unavailable");
                };
                let handle = state.handle("clip")?;
                state.clipboard.hold_selected(&call.caller, handle, text)
            }
            (CLIPBOARD_SERVICE, method) => {
                self.clipboard(&call.caller, method, &call.args, generation)
                    .await
            }
            _ => Err("Unsupported platform operation"),
        }
    }
    /// Every clipboard method except redeeming staged input. The generation is checked under the same lock that touches
    /// a handle, so a caller stopped meanwhile can neither read nor take back what it held.
    async fn clipboard(
        &self,
        caller: &PluginId,
        method: &str,
        arguments: &Value,
        generation: u64,
    ) -> Result<Value, &'static str> {
        use peersus_app::clipboard_handles::{self as shared, Board, Method};
        let method = Method::parse(method).ok_or("Unsupported platform operation")?;
        match method {
            Method::Read | Method::Observe => {
                {
                    let state = self.state()?;
                    state.current(caller, generation)?;
                    state.clipboard.may_capture(caller)?;
                }
                let reading = AndroidBoard.read(matches!(method, Method::Observe)).await;
                let mut state = self.state()?;
                state.current(caller, generation)?;
                let handle = state.handle("clip")?;
                state.clipboard.held(caller, handle, reading)
            }
            Method::Chunk => {
                let mut state = self.state()?;
                state.current(caller, generation)?;
                state.clipboard.chunk(caller, arguments)
            }
            Method::Write => {
                let mut state = self.state()?;
                state.current(caller, generation)?;
                let handle = state.handle("put")?;
                state.clipboard.begin(caller, handle, arguments)
            }
            Method::Append => {
                let mut state = self.state()?;
                state.current(caller, generation)?;
                state.clipboard.append(caller, arguments)
            }
            Method::Commit => {
                let text = {
                    let mut state = self.state()?;
                    state.current(caller, generation)?;
                    state.clipboard.finish(caller, arguments)?
                };
                shared::commit(&AndroidBoard, text).await
            }
            Method::Abort => {
                let mut state = self.state()?;
                state.current(caller, generation)?;
                state.clipboard.abort(caller, arguments)
            }
            Method::Reset => {
                self.state()?.current(caller, generation)?;
                Ok(shared::reset(&AndroidBoard).await)
            }
            Method::Release => {
                let mut state = self.state()?;
                state.current(caller, generation)?;
                state.clipboard.release(caller);
                Ok(json!({"released":true}))
            }
        }
    }
}
/// A phone's battery and network through the platform's own interfaces: the files the application reads elsewhere are closed.
impl peersus_app::system_readings::PlatformReadings for AndroidServices {
    fn battery(&self) -> Option<peersus_app::system_readings::BatteryReading> {
        crate::bridge::battery()
    }
    fn network(&self) -> Option<peersus_app::system_readings::Network> {
        crate::bridge::network()
    }
    fn processor_stat(&self) -> Option<String> {
        crate::bridge::processor_stat()
    }
    fn processor_name(&self) -> Option<String> {
        crate::bridge::processor_name()
    }
}
impl PlatformServices for AndroidServices {
    fn public_read_directory(&self) -> Option<&std::path::Path> {
        self.files.public_read_directory()
    }
    fn extension_prompts(&self) -> Option<Arc<peersus_app::extension_prompts::ExtensionPrompts>> {
        Some(self.prompts.clone())
    }
    fn notifications(&self) -> Option<Arc<peersus_app::notifications::Notifications>> {
        Some(self.notifications.clone())
    }
    fn readings(&self) -> Option<&dyn peersus_app::system_readings::PlatformReadings> {
        Some(self)
    }
    fn action_progress(
        &self,
    ) -> Option<tokio::sync::watch::Receiver<peersus_app::platform_services::ActionProgress>> {
        self.files.action_progress()
    }
    fn select_for_action(
        &self,
        caller: &PluginId,
        input: SelectedInput,
        action: &str,
    ) -> Result<u64, &'static str> {
        self.files.select_for_action(caller, input, action)
    }
    fn extension_foreground(&self) -> Option<bool> {
        Some(crate::bridge::activity_is_foreground())
    }
    fn file_snapshot(
        &self,
        caller: &PluginId,
        file: &str,
    ) -> Option<Arc<peersus_app::platform_services::Snapshot>> {
        self.files.file_snapshot(caller, file)
    }
    fn save_extension_file(
        &self,
        _caller: PluginId,
        name: String,
    ) -> peersus_app::platform_services::file_http::UiResponse<std::fs::File> {
        Box::pin(async move {
            if !crate::bridge::activity_is_foreground() {
                return Err("Open the application to save this file");
            }
            crate::extension_files::save(name).await
        })
    }
    fn microphone_allowed(&self) -> peersus_app::platform_services::file_http::UiResponse<()> {
        Box::pin(async {
            tokio::task::spawn_blocking(crate::bridge::microphone_allowed)
                .await
                .unwrap_or(Err("The microphone permission could not be asked"))
        })
    }
    fn dispatch(&self, call: Call) -> Response {
        if call.service == peersus_app::platform_services::INPUT_SERVICE {
            return self.dispatch_input(call, None);
        }
        if call.service == peersus_app::platform_services::location::SERVICE {
            use peersus_app::platform_services::location;
            return Box::pin(async move {
                location::answer(&call.method, || async {
                    tokio::task::spawn_blocking(crate::bridge::location)
                        .await
                        .unwrap_or(Err(location::UNAVAILABLE))
                })
                .await
            });
        }
        if call.service != CLIPBOARD_SERVICE {
            return self.files.dispatch(call);
        }
        let service = self.clone();
        let generation = self.state().map(|state| state.generation(&call.caller));
        Box::pin(async move {
            let result = match generation {
                Ok(generation) => service.execute(call, generation).await,
                Err(error) => Err(error),
            };
            match result {
                Ok(value) => Outcome::Ok { value },
                Err(message) => Outcome::Error {
                    code: if matches!(
                        message,
                        "Selection was cancelled"
                            | "Selection was cancelled or the file could not be opened"
                            | "Authorization was cancelled"
                            | "Authorization was declined"
                    ) {
                        "cancelled"
                    } else {
                        "platform-refused"
                    }
                    .into(),
                    message: message.into(),
                },
            }
        })
    }
    fn dispatch_input(&self, call: Call, lease: Option<InputLease>) -> Response {
        let controller = self.controller.clone();
        Box::pin(async move {
            match tokio::task::spawn_blocking(move || {
                replay_input(lease.as_ref(), || remote_input(&call, &controller))
            })
            .await
            {
                Ok(Some(Ok(value))) => Outcome::ok(value),
                Ok(None) => Outcome::NotAccepted {},
                Ok(Some(Err(message))) => Outcome::error("platform-refused", &message),
                Err(_) => Outcome::error("platform-refused", "The input was not replayed"),
            }
        })
    }

    fn release_peer_input(&self, caller: &PluginId, owner: &str) {
        revoke_controller(&self.controller, |(plugin, alias)| {
            plugin == caller && alias.as_deref() == Some(owner)
        });
    }

    fn select(
        &self,
        caller: &PluginId,
        input: peersus_app::platform_services::SelectedInput,
    ) -> Result<u64, &'static str> {
        if !matches!(
            input,
            peersus_app::platform_services::SelectedInput::Text(_)
        ) {
            return self.files.select(caller, input);
        }
        let mut state = self.state()?;
        let now = std::time::Instant::now();
        state.contexts.retain(|_, entry| entry.expires > now);
        if state.contexts.len() >= 4 {
            return Err("Too many selected inputs are pending");
        }
        let context = peersus_app::platform_services::next_context()?;
        let generation = state.generation(caller);
        state.contexts.insert(
            context,
            InputContext {
                caller: caller.clone(),
                generation,
                expires: now + std::time::Duration::from_secs(60),
                input,
            },
        );
        Ok(context)
    }
    fn discard_selection(&self, caller: &PluginId, context: u64) {
        self.files.discard_selection(caller, context);
        if let Ok(mut state) = self.state()
            && state
                .contexts
                .get(&context)
                .is_some_and(|entry| &entry.caller == caller)
        {
            state.contexts.remove(&context);
        }
    }
    fn changes(&self, service: &str) -> Option<tokio::sync::watch::Receiver<u64>> {
        (service == CLIPBOARD_SERVICE).then(extension_clipboard::changes)
    }
    fn revoke(&self, caller: &PluginId) {
        revoke_controller(&self.controller, |(plugin, _)| plugin == caller);
        self.prompts.revoke(caller);
        self.files.revoke(caller);
        if let Ok(mut state) = self.state() {
            let generation = state.generation(caller).saturating_add(1);
            state.generations.insert(caller.clone(), generation);
            state.clipboard.release(caller);
            state.contexts.retain(|_, entry| &entry.caller != caller);
        }
    }
    fn revoke_permission(
        &self,
        caller: &PluginId,
        permission: &peersus_api::permission::Permission,
    ) {
        use peersus_app::platform_services::*;
        match permission.name.as_str() {
            INPUT_CAPABILITY => revoke_controller(&self.controller, |(plugin, _)| plugin == caller),
            CLIPBOARD_READ_CAPABILITY | CLIPBOARD_WRITE_CAPABILITY | CLIPBOARD_WATCH_CAPABILITY => {
                if let Ok(mut state) = self.state() {
                    let generation = state.generation(caller).saturating_add(1);
                    state.generations.insert(caller.clone(), generation);
                    state.clipboard.release(caller);
                    state.contexts.retain(|_, entry| &entry.caller != caller);
                }
            }
            FILES_CAPABILITY | HTTP_CAPABILITY | PUBLIC_HTTP_CAPABILITY => {
                self.prompts.revoke(caller);
                self.files.revoke(caller);
            }
            _ => {}
        }
    }
}

fn revoke_controller(
    controller: &Mutex<Option<Controller>>,
    matches: impl FnOnce(&Controller) -> bool,
) {
    if let Ok(mut active) = controller.lock()
        && active.as_ref().is_some_and(matches)
    {
        crate::bridge::release_remote_input(false);
        *active = None;
    }
}

/// What a paired device does, replayed on this phone through the system (see `RemoteInput.java`): the same checked events a computer
/// replays, and nothing a guest wrote reaches Java but a validated batch.
fn remote_input(call: &Call, controller: &Mutex<Option<Controller>>) -> Result<Value, String> {
    use peersus_api::contributions::{MAX_POINTER_BATCH, PointerInput};
    use peersus_app::platform_services::input_arg;
    let owner = || -> Result<Controller, String> {
        let alias = call
            .args
            .get(input_arg::OWNER)
            .map(|value| {
                value
                    .as_str()
                    .filter(|owner| !owner.is_empty())
                    .map(str::to_owned)
                    .ok_or("The input was not understood")
            })
            .transpose()?;
        Ok((call.caller.clone(), alias))
    };
    match call.method.as_str() {
        "inspect" if call.args.as_object().is_some_and(|args| args.is_empty()) => {
            serde_json::to_value(peersus_api::contributions::InputCapabilities {
                independent_pointer: false,
            })
            .map_err(|_| "Input capabilities are unavailable".to_owned())
        }
        "inject" => {
            let events: Vec<PointerInput> = call
                .args
                .get(input_arg::EVENTS)
                .cloned()
                .and_then(|events| serde_json::from_value(events).ok())
                .ok_or("The input was not understood")?;
            if events.is_empty()
                || events.len() > MAX_POINTER_BATCH
                || events.iter().any(|event| event.validate().is_err())
            {
                return Err("The input was not understood".to_owned());
            }
            let batch =
                serde_json::to_string(&events).map_err(|_| "The input was not understood")?;
            let owner = owner()?;
            let mut active = controller.lock().map_err(|_| "Input is unavailable")?;
            if active.as_ref().is_some_and(|current| current != &owner) {
                return Err("Another controller is using this device".into());
            }
            crate::bridge::inject_remote_input(&batch)?;
            *active = Some(owner);
            Ok(json!({"applied": events.len()}))
        }
        "release" => {
            let keep = match call.args.get("keep") {
                None => false,
                Some(Value::Bool(keep)) => *keep,
                Some(_) => return Err("The input was not understood".to_owned()),
            };
            let owner = owner()?;
            let mut active = controller.lock().map_err(|_| "Input is unavailable")?;
            if active.as_ref() == Some(&owner) {
                crate::bridge::release_remote_input(keep);
                *active = None;
            }
            Ok(json!({"released": true}))
        }
        _ => Err("Unknown input method".to_owned()),
    }
}

struct AndroidInteraction {
    prompts: Arc<peersus_app::extension_prompts::ExtensionPrompts>,
}
impl peersus_app::platform_services::file_http::Interaction for AndroidInteraction {
    fn choose(
        &self,
        _: PluginId,
        kind: peersus_app::platform_services::FileKind,
    ) -> peersus_app::platform_services::file_http::UiResponse<(std::fs::File, String)> {
        Box::pin(crate::extension_files::choose(kind.mime()))
    }
    fn configure(
        &self,
        caller: PluginId,
        origin: reqwest::Url,
    ) -> peersus_app::platform_services::file_http::UiResponse<()> {
        let prompts = self.prompts.clone();
        Box::pin(async move {
            let answer = prompts.ask(caller.clone(), origin.to_string()).await?;
            if !answer.accepted {
                return Err("Authorization was cancelled");
            }
            let (operation, secret) = if answer.remove {
                (2, Zeroizing::new(Vec::new()))
            } else {
                let key = answer.secret.ok_or("Enter an API key")?;
                (1, Zeroizing::new(key.expose().as_bytes().to_vec()))
            };
            crate::extension_authorization::access(caller, origin, operation, secret).await?;
            Ok(())
        })
    }
    fn credential(
        &self,
        caller: PluginId,
        origin: reqwest::Url,
    ) -> peersus_app::platform_services::file_http::CredentialResponse {
        Box::pin(async move {
            let credential = crate::extension_authorization::access(
                caller,
                origin,
                0,
                Zeroizing::new(Vec::new()),
            )
            .await?;
            // For reads the Java adapter returns empty only after confirming absence.
            Ok((!credential.is_empty()).then_some(credential))
        })
    }
    fn copy(&self, link: String) -> peersus_app::platform_services::file_http::UiResponse<()> {
        Box::pin(async move {
            if !crate::extension_clipboard::write(&link).await?.ok {
                return Err("The platform refused to copy the link");
            }
            Ok(())
        })
    }
}
