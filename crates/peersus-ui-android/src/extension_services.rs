//! Trusted Android adapter. Provider protocols remain in sandboxed extensions.
use crate::extension_clipboard::{self, AndroidBoard};
use peersus_api::message::Outcome;
use peersus_app::platform_services::{
    CLIPBOARD_SERVICE, Call, PlatformServices, Response, SelectedInput,
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

#[derive(Clone)]
pub struct AndroidServices {
    state: Arc<Mutex<State>>,
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
            files: peersus_app::platform_services::file_http::FileHttpServices::new(
                scratch,
                Arc::new(AndroidInteraction {
                    prompts: prompts.clone(),
                }),
                notifications.clone(),
            ),
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
    fn battery(&self) -> Option<peersus_app::system_readings::Battery> {
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
    fn dispatch(&self, call: Call) -> Response {
        if call.service == peersus_app::platform_services::INPUT_SERVICE {
            return Box::pin(async move {
                let replayed = tokio::task::spawn_blocking(move || remote_input(&call))
                    .await
                    .unwrap_or(Err("The input was not replayed".to_owned()));
                match replayed {
                    Ok(value) => Outcome::Ok { value },
                    Err(message) => Outcome::Error {
                        code: "platform-refused".into(),
                        message,
                    },
                }
            });
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
        self.prompts.revoke(caller);
        self.files.revoke(caller);
        if let Ok(mut state) = self.state() {
            let generation = state.generation(caller).saturating_add(1);
            state.generations.insert(caller.clone(), generation);
            state.clipboard.release(caller);
            state.contexts.retain(|_, entry| &entry.caller != caller);
        }
    }
}

/// What a paired device does, replayed on this phone through the system (see `RemoteInput.java`): the same checked events a computer
/// replays, and nothing a guest wrote reaches Java but a validated batch.
fn remote_input(call: &Call) -> Result<Value, String> {
    use peersus_api::contributions::{MAX_POINTER_BATCH, PointerInput};
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
                .get("events")
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
            crate::bridge::inject_remote_input(&batch)?;
            Ok(json!({"applied": events.len()}))
        }
        "release" => {
            let keep = match call.args.get("keep") {
                None => false,
                Some(Value::Bool(keep)) => *keep,
                Some(_) => return Err("The input was not understood".to_owned()),
            };
            crate::bridge::release_remote_input(keep);
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
    ) -> peersus_app::platform_services::file_http::UiResponse<(std::fs::File, String)> {
        Box::pin(crate::extension_files::choose())
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
    ) -> peersus_app::platform_services::file_http::UiResponse<Zeroizing<Vec<u8>>> {
        // The Keystore read with nothing to approve and no file involved: what the person is asked is the service's, not this adapter's.
        Box::pin(crate::extension_authorization::access(
            caller,
            origin,
            0,
            Zeroizing::new(Vec::new()),
        ))
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
