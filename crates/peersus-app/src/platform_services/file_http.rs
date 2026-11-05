//! Shared private-file and origin-bound HTTP services. Platform UI is injected; provider protocols stay in guests.
use super::{
    Call, FILES_SERVICE, HTTP_SERVICE, MAX_FILE_BYTES, PlatformServices, Response, Snapshot,
};
use peersus_api::message::Outcome;
use peersus_core::plugins::PluginId;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::Semaphore;
use zeroize::Zeroizing;

/// Trusted UI boundary: only platform code sees credentials or opens OS dialogs.
pub type UiResponse<T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, &'static str>> + Send>>;
/// What a platform says when it cannot read a saved key without asking: the one answer that means "not supported" and not "no key".
pub const NO_EARLY_CREDENTIAL: &str = "This platform cannot check a saved API key";
pub trait Interaction: Send + Sync {
    fn choose(&self, caller: PluginId) -> UiResponse<(std::fs::File, String)>;
    fn configure(&self, caller: PluginId, origin: reqwest::Url) -> UiResponse<()>;
    fn authorize(
        &self,
        caller: PluginId,
        origin: reqwest::Url,
        name: String,
        bytes: u64,
    ) -> UiResponse<Zeroizing<Vec<u8>>>;
    fn present(&self, caller: PluginId, link: String) -> UiResponse<()>;
    /// The key already saved for this extension and origin, read without any prompt and without a file. For a check of the key
    /// itself: a platform that cannot do this says so, and nothing then pretends the key was checked.
    fn credential(
        &self,
        _caller: PluginId,
        _origin: reqwest::Url,
    ) -> UiResponse<Zeroizing<Vec<u8>>> {
        Box::pin(async { Err(NO_EARLY_CREDENTIAL) })
    }
}

struct Selection {
    caller: PluginId,
    snapshot: Arc<Snapshot>,
    action: Option<String>,
    completed: Vec<(u64, u64)>,
}
struct Authorization {
    caller: PluginId,
    file: String,
    origin: reqwest::Url,
    credential: Zeroizing<Vec<u8>>,
    secrets: HashMap<String, Zeroizing<String>>,
}
struct InputContext {
    caller: PluginId,
    generation: u64,
    expires: std::time::Instant,
    input: crate::platform_services::SelectedInput,
    action: Option<String>,
}
struct State {
    next: u64,
    generations: HashMap<PluginId, u64>,
    files: HashMap<String, Selection>,
    authorizations: HashMap<String, Authorization>,
    contexts: HashMap<u64, InputContext>,
}
impl State {
    fn new() -> Self {
        Self {
            next: 0,
            generations: HashMap::new(),
            files: HashMap::new(),
            authorizations: HashMap::new(),
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
pub struct FileHttpServices {
    state: Arc<Mutex<State>>,
    workers: Arc<Semaphore>,
    scratch: PathBuf,
    ui: Arc<dyn Interaction>,
    progress: tokio::sync::watch::Sender<super::ActionProgress>,
    #[cfg(test)]
    test_root: Option<reqwest::Certificate>,
    #[cfg(test)]
    test_proxy: Option<reqwest::Url>,
}
impl FileHttpServices {
    pub fn new(scratch: PathBuf, ui: Arc<dyn Interaction>) -> Self {
        Self {
            state: Arc::new(Mutex::new(State::new())),
            workers: Arc::new(Semaphore::new(2)),
            scratch,
            ui,
            progress: tokio::sync::watch::channel(HashMap::new()).0,
            #[cfg(test)]
            test_root: None,
            #[cfg(test)]
            test_proxy: None,
        }
    }
    fn state(&self) -> Result<std::sync::MutexGuard<'_, State>, &'static str> {
        self.state
            .lock()
            .map_err(|_| "Platform services are unavailable")
    }
    async fn execute(&self, call: Call, generation: u64) -> Result<Value, &'static str> {
        match (call.service.as_str(), call.method.as_str()) {
            (FILES_SERVICE, "choose" | "selected") => {
                {
                    let state = self.state()?;
                    state.current(&call.caller, generation)?;
                    if state.files.len() >= 4
                        || state
                            .files
                            .values()
                            .filter(|file| file.caller == call.caller)
                            .count()
                            >= 2
                    {
                        return Err("Release a selected file before choosing another");
                    }
                }
                let limit = call
                    .args
                    .get("max_bytes")
                    .and_then(Value::as_u64)
                    .unwrap_or(MAX_FILE_BYTES);
                if limit == 0 || limit > MAX_FILE_BYTES {
                    return Err("Invalid file size limit");
                }
                let mut selected_action = None;
                let selected = if call.method == "selected" {
                    let context = call
                        .args
                        .get("context")
                        .and_then(Value::as_u64)
                        .ok_or("Missing selected input context")?;
                    let mut state = self.state()?;
                    state.current(&call.caller, generation)?;
                    state
                        .contexts
                        .get(&context)
                        .filter(|entry| {
                            entry.caller == call.caller
                                && entry.generation == generation
                                && entry.expires > std::time::Instant::now()
                                && !matches!(
                                    entry.input,
                                    crate::platform_services::SelectedInput::Text(_)
                                )
                        })
                        .ok_or("Selected input is unavailable")?;
                    let entry = state
                        .contexts
                        .remove(&context)
                        .ok_or("Selected input is unavailable")?;
                    selected_action = entry.action;
                    Some(entry.input)
                } else {
                    None
                };
                let picked = if selected.is_none() {
                    Some(self.ui.choose(call.caller.clone()).await?)
                } else {
                    None
                };
                // The permit lives on the blocking worker, including after cancellation.
                // Repeated cancellation cannot create unbounded detached copy workers.
                let permit = self
                    .workers
                    .clone()
                    .acquire_owned()
                    .await
                    .map_err(|_| "Selection is unavailable")?;
                let scratch = self.scratch.clone();
                let snapshot = Arc::new(
                    tokio::task::spawn_blocking(move || {
                        let _permit = permit;
                        let (source, name) = match (selected, picked) {
                            (Some(crate::platform_services::SelectedInput::Path(path)), None) => {
                                let name = path
                                    .file_name()
                                    .and_then(|name| name.to_str())
                                    .ok_or("Invalid selected filename")?
                                    .to_owned();
                                let source = std::fs::File::open(path)
                                    .map_err(|_| "Selected file is unavailable")?;
                                (source, name)
                            }
                            (
                                Some(crate::platform_services::SelectedInput::Held {
                                    registry,
                                    number,
                                    name,
                                }),
                                None,
                            ) => {
                                let copy = registry
                                    .reopen(number)
                                    .map_err(|_| "Selected file is unavailable")?;
                                let source =
                                    registry.take(copy).ok_or("Selected file is unavailable")?;
                                (source, name)
                            }
                            (None, Some(picked)) => picked,
                            _ => return Err("Invalid selected input"),
                        };
                        Snapshot::from_selected(source, name, limit, &scratch)
                    })
                    .await
                    .map_err(|_| "The selected file could not be copied")??,
                );
                let mut state = self.state()?;
                state.current(&call.caller, generation)?;
                if state.files.len() >= 4
                    || state
                        .files
                        .values()
                        .filter(|file| file.caller == call.caller)
                        .count()
                        >= 2
                {
                    return Err("Release a selected file before choosing another");
                }
                let handle = state.handle("file")?;
                let digest: String = snapshot
                    .sha256()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect();
                let result = json!({"file":handle,"name":snapshot.name(),"bytes":snapshot.bytes(),"sha256":digest});
                if let Some(action) = &selected_action {
                    self.progress.send_modify(|progress| {
                        progress
                            .insert((call.caller.clone(), action.clone()), (0, snapshot.bytes()));
                    });
                }
                state.files.insert(
                    handle,
                    Selection {
                        caller: call.caller,
                        snapshot,
                        action: selected_action,
                        completed: Vec::new(),
                    },
                );
                Ok(result)
            }
            (FILES_SERVICE, "release") => {
                let handle = argument(&call.args, "file")?;
                let mut state = self.state()?;
                state.current(&call.caller, generation)?;
                if state
                    .files
                    .get(handle)
                    .is_none_or(|file| file.caller != call.caller)
                {
                    return Err("Unknown selected file");
                }
                state
                    .authorizations
                    .retain(|_, auth| auth.caller != call.caller || auth.file != handle);
                if let Some(file) = state.files.remove(handle)
                    && let Some(action) = file.action
                {
                    self.progress.send_modify(|progress| {
                        progress.remove(&(call.caller.clone(), action));
                    });
                }
                Ok(json!({"released":true}))
            }
            (HTTP_SERVICE, "configure") => {
                let origin = approved_origin(argument(&call.args, "origin")?)?;
                // A configuration dialog has no file and cannot authorize a request.
                self.ui.configure(call.caller.clone(), origin).await?;
                self.state()?.current(&call.caller, generation)?;
                Ok(json!({"configured":true}))
            }
            (HTTP_SERVICE, "authorize") => {
                let handle = argument(&call.args, "file")?.to_owned();
                let origin = approved_origin(argument(&call.args, "origin")?)?;
                let snapshot = {
                    let state = self.state()?;
                    state.current(&call.caller, generation)?;
                    if state.authorizations.len() >= 4 {
                        return Err("Release an authorization before requesting another");
                    }
                    state
                        .files
                        .get(&handle)
                        .filter(|file| file.caller == call.caller)
                        .map(|file| file.snapshot.clone())
                        .ok_or("Unknown selected file")?
                };
                // The key must exist before the person is asked to allow anything: an approval that can only end in "no key" is a prompt
                // for nothing. A platform that cannot read the key ahead of time says so, and is asked as it always was.
                match self
                    .ui
                    .credential(call.caller.clone(), origin.clone())
                    .await
                {
                    Ok(_) => {}
                    Err(NO_EARLY_CREDENTIAL) => {}
                    Err(refused) => return Err(refused),
                }
                let credential = self
                    .ui
                    .authorize(
                        call.caller.clone(),
                        origin.clone(),
                        snapshot.name().to_owned(),
                        snapshot.bytes(),
                    )
                    .await?;
                let mut state = self.state()?;
                state.current(&call.caller, generation)?;
                if state.authorizations.len() >= 4 || !state.files.contains_key(&handle) {
                    return Err("The selection is no longer available");
                }
                let authorization = state.handle("authorization")?;
                state.authorizations.insert(
                    authorization.clone(),
                    Authorization {
                        caller: call.caller,
                        file: handle,
                        origin,
                        credential,
                        secrets: HashMap::new(),
                    },
                );
                Ok(json!({"authorization":authorization}))
            }
            (HTTP_SERVICE, "present") => {
                let handle = argument(&call.args, "authorization")?;
                let text = argument(&call.args, "link")?;
                let link = {
                    let state = self.state()?;
                    state.current(&call.caller, generation)?;
                    let auth = state
                        .authorizations
                        .get(handle)
                        .filter(|auth| auth.caller == call.caller)
                        .ok_or("Unknown authorization")?;
                    reject_secret_echo(&json!(text), &auth.credential, &auth.secrets)?;
                    let link = reqwest::Url::parse(text).map_err(|_| "Invalid result link")?;
                    if link.origin() != auth.origin.origin()
                        || !link.username().is_empty()
                        || link.password().is_some()
                        || link.fragment().is_some()
                    {
                        return Err("The result link must stay within the approved origin");
                    }
                    link.to_string()
                };
                self.ui.present(call.caller.clone(), link).await?;
                self.state()?.current(&call.caller, generation)?;
                Ok(json!({"presented":true}))
            }
            (HTTP_SERVICE, "release") => {
                let handle = argument(&call.args, "authorization")?;
                let mut state = self.state()?;
                state.current(&call.caller, generation)?;
                if state
                    .authorizations
                    .get(handle)
                    .is_none_or(|auth| auth.caller != call.caller)
                {
                    return Err("Unknown authorization");
                }
                state.authorizations.remove(handle);
                Ok(json!({"released":true}))
            }
            (HTTP_SERVICE, "request") => self.request(call, generation).await,
            (HTTP_SERVICE, "check") => self.check(call, generation).await,
            _ => Err("Unsupported platform operation"),
        }
    }
    /// The HTTPS client every request uses: HTTPS only, no proxy, no redirect, no retry, bounded in time.
    fn https_client(&self) -> Result<reqwest::Client, &'static str> {
        let builder = reqwest::Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(120));
        #[cfg(test)]
        let builder = match &self.test_root {
            Some(root) => builder.add_root_certificate(root.clone()),
            None => builder,
        };
        #[cfg(test)]
        let builder = if let Some(proxy) = &self.test_proxy {
            // This fixture terminates TLS locally and never opens an upstream
            // connection. A failed proxy cannot fall back to the public origin.
            if proxy.scheme() != "http"
                || proxy.host_str() != Some("127.0.0.1")
                || proxy.port().is_none()
                || !proxy.username().is_empty()
                || proxy.password().is_some()
                || self.test_root.is_none()
            {
                return Err("Invalid test transport");
            }
            builder
                .proxy(reqwest::Proxy::all(proxy.as_str()).map_err(|_| "Invalid test transport")?)
        } else {
            builder
        };
        builder.build().map_err(|_| "HTTPS is unavailable")
    }
    /// Whether the key the person saved for an origin is accepted there: one `GET` of a path the extension names, with the saved key as
    /// the bearer. Needs no selected file and asks nothing of the person. Only the status comes back: the body belongs to the provider
    /// and the key is never given to the extension, so neither can leak through this answer.
    async fn check(&self, call: Call, generation: u64) -> Result<Value, &'static str> {
        let origin = approved_origin(argument(&call.args, "origin")?)?;
        let url = within_origin(&origin, argument(&call.args, "path")?)?;
        self.state()?.current(&call.caller, generation)?;
        let credential = self
            .ui
            .credential(call.caller.clone(), origin.clone())
            .await?;
        self.state()?.current(&call.caller, generation)?;
        let mut header = reqwest::header::HeaderValue::from_bytes(
            &[b"Bearer ".as_slice(), credential.as_slice()].concat(),
        )
        .map_err(|_| "Invalid credential")?;
        header.set_sensitive(true);
        let response = self
            .https_client()?
            .get(url)
            .header(reqwest::header::AUTHORIZATION, header)
            .send()
            .await
            .map_err(|_| "HTTPS failed; no retry was attempted")?;
        Ok(json!({"status":response.status().as_u16()}))
    }
    async fn request(&self, call: Call, generation: u64) -> Result<Value, &'static str> {
        if call.args.get("present_link").is_some() {
            return Err("Validate the response before requesting result presentation");
        }
        let handle = argument(&call.args, "authorization")?.to_owned();
        let (origin, credential, snapshot, secrets, file_handle) = {
            let state = self.state()?;
            state.current(&call.caller, generation)?;
            let auth = state
                .authorizations
                .get(&handle)
                .filter(|auth| auth.caller == call.caller)
                .ok_or("Unknown authorization")?;
            let snapshot = state
                .files
                .get(&auth.file)
                .ok_or("Unknown selected file")?
                .snapshot
                .clone();
            (
                auth.origin.clone(),
                Zeroizing::new(auth.credential.to_vec()),
                snapshot,
                auth.secrets.clone(),
                auth.file.clone(),
            )
        };
        let path = argument(&call.args, "path")?;
        let url = within_origin(&origin, path)?;
        let method = argument(&call.args, "method")?;
        if !matches!(method, "GET" | "POST" | "PUT" | "DELETE") {
            return Err("Unsupported HTTPS method");
        }
        let client = self.https_client()?;
        let mut request = client.request(
            reqwest::Method::from_bytes(method.as_bytes()).map_err(|_| "Invalid method")?,
            url,
        );
        if call
            .args
            .get("bearer")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            let mut header = reqwest::header::HeaderValue::from_bytes(
                &[b"Bearer ".as_slice(), credential.as_slice()].concat(),
            )
            .map_err(|_| "Invalid credential")?;
            header.set_sensitive(true);
            request = request.header(reqwest::header::AUTHORIZATION, header);
        }
        if let Some(headers) = call.args.get("headers") {
            let headers = headers
                .as_object()
                .filter(|headers| headers.len() <= 8)
                .ok_or("Invalid headers")?;
            for (name, value) in headers {
                // No ambient cookies, host overrides or credential forwarding.
                if !matches!(name.as_str(), "content-type" | "x-upload-session") {
                    return Err("Unsupported request header");
                }
                let value = resolve_secret(value, &secrets)?;
                let mut value = reqwest::header::HeaderValue::from_str(
                    value.as_str().ok_or("Invalid header value")?,
                )
                .map_err(|_| "Invalid header value")?;
                value.set_sensitive(name != "content-type");
                request = request.header(name, value);
            }
        }
        let mut part_range = None;
        let body_count = ["json", "part", "multipart"]
            .iter()
            .filter(|name| call.args.get(**name).is_some())
            .count();
        if body_count > 1 {
            return Err("Choose exactly one request body");
        }
        if let Some(body) = call.args.get("json") {
            let body = resolve_secret(body, &secrets)?;
            let bytes = serde_json::to_vec(&body).map_err(|_| "Invalid JSON body")?;
            if bytes.len() > 64 * 1024 {
                return Err("JSON body exceeds its bound");
            }
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(bytes);
        } else if let Some(part) = call.args.get("part").or_else(|| call.args.get("multipart")) {
            let offset = part
                .get("offset")
                .and_then(Value::as_u64)
                .ok_or("Invalid part offset")?;
            let length = part
                .get("length")
                .and_then(Value::as_u64)
                .ok_or("Invalid part length")?;
            part_range = Some((offset, length));
            let permit = self
                .workers
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| "File reading is unavailable")?;
            let name = snapshot.name().to_owned();
            let bytes = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                snapshot.read_part(offset, length)
            })
            .await
            .map_err(|_| "File reading failed")??;
            if call.args.get("multipart").is_some() {
                let field = multipart_field(part)?;
                let mut form = reqwest::multipart::Form::new().part(
                    field,
                    reqwest::multipart::Part::bytes(bytes).file_name(name),
                );
                if let Some(fields) = part.get("fields") {
                    let fields = fields
                        .as_object()
                        .filter(|fields| fields.len() <= 8)
                        .ok_or("Invalid multipart fields")?;
                    for (key, value) in fields {
                        let value = value
                            .as_str()
                            .filter(|value| value.len() <= 1024)
                            .ok_or("Invalid multipart field")?;
                        if key.len() > 64 {
                            return Err("Invalid multipart field");
                        }
                        form = form.text(key.clone(), value.to_owned());
                    }
                }
                request = request.multipart(form);
            } else {
                request = request.body(bytes);
            }
        }
        let captures = call
            .args
            .get("capture_secrets")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if captures.len() > 4
            || captures.iter().any(|pointer| {
                pointer
                    .as_str()
                    .is_none_or(|pointer| pointer.len() > 256 || !pointer.starts_with('/'))
            })
        {
            return Err("Invalid response secret fields");
        }
        {
            // A part may have waited for an I/O worker. Releasing its authority
            // while it waits must prevent the request, not just hide its reply.
            let state = self.state()?;
            state.current(&call.caller, generation)?;
            let auth = state
                .authorizations
                .get(&handle)
                .filter(|auth| auth.caller == call.caller)
                .ok_or("Authorization was released")?;
            if !state.files.contains_key(&auth.file) {
                return Err("The selection is no longer available");
            }
        }
        let mut response = request
            .send()
            .await
            .map_err(|_| "HTTPS failed; no retry was attempted")?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Ok(json!({"status":status}));
        }
        if let Some((offset, length)) = part_range {
            let mut state = self.state()?;
            state.current(&call.caller, generation)?;
            let update = state.files.get_mut(&file_handle).and_then(|file| {
                let action = file.action.clone()?;
                let total = file.snapshot.bytes();
                let sent = completed_bytes(&mut file.completed, offset, length, total);
                Some((action, sent, total))
            });
            if let Some((action, sent, total)) = update {
                self.progress.send_modify(|progress| {
                    progress.insert((call.caller.clone(), action), (sent, total));
                });
            }
        }
        let mut bytes = Zeroizing::new(Vec::new());
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "HTTPS response was interrupted")?
        {
            if chunk.len() > (32 * 1024usize).saturating_sub(bytes.len()) {
                return Err("HTTPS response exceeds its bound");
            }
            bytes.extend_from_slice(&chunk);
        }
        let mut value: Value =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid JSON response")?;
        let mut state = self.state()?;
        state.current(&call.caller, generation)?;
        for pointer in captures {
            let field = value
                .pointer_mut(pointer.as_str().ok_or("Invalid secret field")?)
                .ok_or("Missing response secret field")?;
            let secret = match field.take() {
                Value::String(secret) => Zeroizing::new(secret),
                _ => return Err("Invalid response secret"),
            };
            if secret.is_empty()
                || secret.len() > 16384
                || !secret.bytes().all(|byte| (33..=126).contains(&byte))
            {
                return Err("Invalid response secret");
            }
            let secret_handle = state.handle("secret")?;
            let auth = state
                .authorizations
                .get_mut(&handle)
                .ok_or("Authorization was released")?;
            // 250 MiB with 3 MiB parts needs 84 receipts plus its session.
            if auth.secrets.len() >= 128 {
                return Err("Too many response secrets");
            }
            auth.secrets.insert(secret_handle.clone(), secret);
            *field = json!({"secret":secret_handle});
        }
        let auth = state
            .authorizations
            .get(&handle)
            .ok_or("Authorization was released")?;
        reject_secret_echo(&value, &auth.credential, &auth.secrets)?;
        if serde_json::to_vec(&value)
            .map_err(|_| "Invalid JSON response")?
            .len()
            > 32 * 1024
        {
            return Err("HTTPS response exceeds its bound");
        }
        drop(state);
        Ok(json!({"status":status,"json":value}))
    }
}
fn argument<'a>(args: &'a Value, name: &str) -> Result<&'a str, &'static str> {
    args.get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 2048)
        .ok_or("Invalid platform arguments")
}
fn approved_origin(text: &str) -> Result<reqwest::Url, &'static str> {
    let url = reqwest::Url::parse(text).map_err(|_| "Invalid HTTPS origin")?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("An HTTPS origin without a path or credentials is required");
    }
    Ok(url)
}
/// A request path resolved against the approved origin, or a refusal: never another origin, never credentials or a fragment in the address.
fn within_origin(origin: &reqwest::Url, path: &str) -> Result<reqwest::Url, &'static str> {
    let url = origin.join(path).map_err(|_| "Invalid request path")?;
    if url.origin() != origin.origin()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("The request must stay within the approved origin");
    }
    Ok(url)
}
fn resolve_secret(
    value: &Value,
    secrets: &HashMap<String, Zeroizing<String>>,
) -> Result<Value, &'static str> {
    match value {
        Value::Object(fields) if fields.len() == 1 && fields.contains_key("secret") => {
            let handle = fields["secret"].as_str().ok_or("Invalid secret handle")?;
            Ok(Value::String(
                secrets
                    .get(handle)
                    .ok_or("Unknown response secret")?
                    .to_string(),
            ))
        }
        Value::Object(fields) => fields
            .iter()
            .map(|(key, value)| Ok((key.clone(), resolve_secret(value, secrets)?)))
            .collect::<Result<serde_json::Map<_, _>, _>>()
            .map(Value::Object),
        Value::Array(values) => values
            .iter()
            .map(|value| resolve_secret(value, secrets))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        _ => Ok(value.clone()),
    }
}
fn multipart_field(part: &Value) -> Result<String, &'static str> {
    part.get("field")
        .and_then(Value::as_str)
        .filter(|field| {
            !field.is_empty()
                && field.len() <= 64
                && field
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        })
        .map(str::to_owned)
        .ok_or("Invalid multipart file field")
}
fn reject_secret_echo(
    value: &Value,
    credential: &[u8],
    secrets: &HashMap<String, Zeroizing<String>>,
) -> Result<(), &'static str> {
    match value {
        Value::String(text) => {
            let credential = std::str::from_utf8(credential).map_err(|_| "Invalid credential")?;
            if text.contains(credential)
                || secrets
                    .values()
                    .any(|secret| text.contains(secret.as_str()))
            {
                return Err("The response attempted to disclose a credential");
            }
        }
        Value::Object(fields) => {
            for (key, value) in fields {
                reject_secret_echo(&Value::String(key.clone()), credential, secrets)?;
                reject_secret_echo(value, credential, secrets)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                reject_secret_echo(value, credential, secrets)?;
            }
        }
        _ => {}
    }
    Ok(())
}
/// Count acknowledged file ranges once, including retries and out-of-order parts.
fn completed_bytes(ranges: &mut Vec<(u64, u64)>, offset: u64, length: u64, total: u64) -> u64 {
    let end = offset.saturating_add(length).min(total);
    if offset < end && ranges.len() < 4096 {
        ranges.push((offset, end));
        ranges.sort_unstable();
        let mut kept = 0;
        for index in 0..ranges.len() {
            let (start, end) = ranges[index];
            if kept > 0 && start <= ranges[kept - 1].1 {
                ranges[kept - 1].1 = ranges[kept - 1].1.max(end);
            } else {
                ranges[kept] = (start, end);
                kept += 1;
            }
        }
        ranges.truncate(kept);
    }
    ranges.iter().map(|(start, end)| end - start).sum()
}

#[cfg(test)]
mod progress_tests {
    use super::completed_bytes;

    #[test]
    fn retries_and_overlapping_parts_are_counted_only_once() {
        let mut ranges = Vec::new();
        assert_eq!(completed_bytes(&mut ranges, 0, 40, 100), 40);
        assert_eq!(completed_bytes(&mut ranges, 0, 40, 100), 40);
        assert_eq!(completed_bytes(&mut ranges, 20, 50, 100), 70);
        assert_eq!(completed_bytes(&mut ranges, 70, 30, 100), 100);
        assert_eq!(ranges, [(0, 100)]);
    }

    #[test]
    fn out_of_order_parts_do_not_claim_unacknowledged_gaps() {
        let mut ranges = Vec::new();
        assert_eq!(completed_bytes(&mut ranges, 50, 50, 100), 50);
        assert_eq!(completed_bytes(&mut ranges, 0, 25, 100), 75);
        assert_eq!(completed_bytes(&mut ranges, 25, 25, 100), 100);
        assert_eq!(ranges, [(0, 100)]);
    }

    #[test]
    fn empty_or_out_of_range_parts_cannot_inflate_progress() {
        let mut ranges = Vec::new();
        assert_eq!(completed_bytes(&mut ranges, 100, 10, 100), 0);
        assert_eq!(completed_bytes(&mut ranges, 0, 0, 100), 0);
        assert_eq!(completed_bytes(&mut ranges, 90, u64::MAX, 100), 10);
    }
}

impl PlatformServices for FileHttpServices {
    fn action_progress(&self) -> Option<tokio::sync::watch::Receiver<super::ActionProgress>> {
        Some(self.progress.subscribe())
    }
    fn select_for_action(
        &self,
        caller: &PluginId,
        input: super::SelectedInput,
        action: &str,
    ) -> Result<u64, &'static str> {
        let context = self.select(caller, input)?;
        self.state()?
            .contexts
            .get_mut(&context)
            .ok_or("Selected input is unavailable")?
            .action = Some(action.to_owned());
        Ok(context)
    }
    fn dispatch(&self, call: Call) -> Response {
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
        input: crate::platform_services::SelectedInput,
    ) -> Result<u64, &'static str> {
        if matches!(input, crate::platform_services::SelectedInput::Text(_)) {
            return Err("This service accepts only selected files");
        }
        let mut state = self.state()?;
        let now = std::time::Instant::now();
        state.contexts.retain(|_, entry| entry.expires > now);
        if state.contexts.len() >= 4 {
            return Err("Too many selected inputs are pending");
        }
        let context = super::next_context()?;
        let generation = state.generation(caller);
        state.contexts.insert(
            context,
            InputContext {
                caller: caller.clone(),
                generation,
                expires: now + std::time::Duration::from_secs(60),
                input,
                action: None,
            },
        );
        Ok(context)
    }
    fn discard_selection(&self, caller: &PluginId, context: u64) {
        if let Ok(mut state) = self.state()
            && state
                .contexts
                .get(&context)
                .is_some_and(|entry| &entry.caller == caller)
        {
            state.contexts.remove(&context);
        }
    }
    fn revoke(&self, caller: &PluginId) {
        if let Ok(mut state) = self.state() {
            let generation = state.generation(caller).saturating_add(1);
            state.generations.insert(caller.clone(), generation);
            state.contexts.retain(|_, entry| &entry.caller != caller);
            state.files.retain(|_, file| &file.caller != caller);
            state
                .authorizations
                .retain(|_, auth| &auth.caller != caller);
            self.progress
                .send_modify(|progress| progress.retain(|(plugin, _), _| plugin != caller));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct TestUi;
    impl Interaction for TestUi {
        fn choose(&self, _: PluginId) -> UiResponse<(std::fs::File, String)> {
            Box::pin(async {
                use std::io::{Seek, Write};
                let mut file = tempfile::tempfile().unwrap();
                file.write_all(b"synthetic file content").unwrap();
                file.rewind().unwrap();
                Ok((file, "synthetic.txt".into()))
            })
        }
        fn configure(&self, _: PluginId, _: reqwest::Url) -> UiResponse<()> {
            Box::pin(async { Ok(()) })
        }
        fn authorize(
            &self,
            _: PluginId,
            _: reqwest::Url,
            _: String,
            _: u64,
        ) -> UiResponse<Zeroizing<Vec<u8>>> {
            Box::pin(async { Ok(Zeroizing::new(b"synthetic-test-credential".to_vec())) })
        }
        fn present(&self, _: PluginId, _: String) -> UiResponse<()> {
            Box::pin(async { Ok(()) })
        }
    }
    async fn invoke(
        host: &FileHttpServices,
        caller: &str,
        service: &str,
        method: &str,
        args: Value,
    ) -> Result<Value, String> {
        match host
            .dispatch(Call {
                caller: PluginId::new(caller).unwrap(),
                service: service.into(),
                method: method.into(),
                args,
            })
            .await
        {
            Outcome::Ok { value } => Ok(value),
            Outcome::Error { message, .. } => Err(message),
        }
    }
    #[tokio::test]
    async fn configuring_a_key_does_not_select_files_or_authorize_uploads() {
        let scratch = tempfile::tempdir().unwrap();
        let host = FileHttpServices::new(scratch.path().to_owned(), Arc::new(TestUi));
        let result = invoke(
            &host,
            "first",
            HTTP_SERVICE,
            "configure",
            json!({"origin":"https://example.invalid"}),
        )
        .await
        .unwrap();
        assert_eq!(result, json!({"configured":true}));
        assert!(host.state().unwrap().files.is_empty());
        assert!(host.state().unwrap().authorizations.is_empty());
        assert!(
            invoke(
                &host,
                "first",
                HTTP_SERVICE,
                "configure",
                json!({"origin":"http://example.invalid"})
            )
            .await
            .is_err()
        );
        assert!(invoke(&host, "first", HTTP_SERVICE, "request",
            json!({"authorization":"configured","file":"configured","url":"https://example.invalid/"})).await.is_err());
    }

    #[tokio::test]
    async fn staged_context_is_single_use_caller_scoped_and_keeps_send_selection() {
        use crate::platform_services::SelectedInput;
        use std::io::Write;
        let scratch = tempfile::tempdir().unwrap();
        let host = FileHttpServices::new(scratch.path().to_owned(), Arc::new(TestUi));
        let held = peersus_net::held::Held::default();
        // Windows: an anonymous temporary file is opened for one handle only and cannot be opened
        // again, unlike a file a person picks; a named one opened like that can.
        #[cfg(windows)]
        let mut source = tempfile::NamedTempFile::new().unwrap().reopen().unwrap();
        #[cfg(unix)]
        let mut source = tempfile::tempfile().unwrap();
        source.write_all(b"selected bytes").unwrap();
        let number = held.keep(source).unwrap();
        let caller = PluginId::new("first").unwrap();
        let context = host
            .select(
                &caller,
                SelectedInput::Held {
                    registry: held.clone(),
                    number,
                    name: "synthetic.txt".into(),
                },
            )
            .unwrap();
        assert!(
            invoke(
                &host,
                "second",
                FILES_SERVICE,
                "selected",
                json!({"context":context})
            )
            .await
            .is_err()
        );
        let file = invoke(
            &host,
            "first",
            FILES_SERVICE,
            "selected",
            json!({"context":context}),
        )
        .await
        .unwrap();
        let handle = file["file"].as_str().unwrap();
        assert_eq!(
            host.state().unwrap().files[handle]
                .snapshot
                .read_part(0, 14)
                .unwrap(),
            b"selected bytes"
        );
        assert_eq!(held.len(), 1);
        assert!(
            invoke(
                &host,
                "first",
                FILES_SERVICE,
                "selected",
                json!({"context":context})
            )
            .await
            .is_err()
        );
        host.revoke(&caller);
        assert!(host.state().unwrap().files.is_empty());
        held.release(number);
        assert!(held.is_empty());
    }

    #[tokio::test]
    async fn staged_context_expiry_revocation_and_discard_deny_redemption() {
        use crate::platform_services::SelectedInput;
        let scratch = tempfile::tempdir().unwrap();
        let host = FileHttpServices::new(scratch.path().to_owned(), Arc::new(TestUi));
        let caller = PluginId::new("first").unwrap();
        let context = host
            .select(
                &caller,
                SelectedInput::Path(scratch.path().join("not-opened")),
            )
            .unwrap();
        host.state()
            .unwrap()
            .contexts
            .get_mut(&context)
            .unwrap()
            .expires = std::time::Instant::now();
        assert!(
            invoke(
                &host,
                "first",
                FILES_SERVICE,
                "selected",
                json!({"context":context})
            )
            .await
            .is_err()
        );
        let context = host
            .select(
                &caller,
                SelectedInput::Path(scratch.path().join("not-opened")),
            )
            .unwrap();
        host.discard_selection(&PluginId::new("second").unwrap(), context);
        assert!(host.state().unwrap().contexts.contains_key(&context));
        host.discard_selection(&caller, context);
        assert!(!host.state().unwrap().contexts.contains_key(&context));
        let context = host
            .select(
                &caller,
                SelectedInput::Path(scratch.path().join("not-opened")),
            )
            .unwrap();
        host.revoke(&caller);
        assert!(
            invoke(
                &host,
                "first",
                FILES_SERVICE,
                "selected",
                json!({"context":context})
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn selection_is_private_caller_scoped_and_revocation_invalidates_pending_calls() {
        let scratch = tempfile::tempdir().unwrap();
        let host = FileHttpServices::new(scratch.path().to_owned(), Arc::new(TestUi));
        let file = invoke(&host, "first", FILES_SERVICE, "choose", json!({}))
            .await
            .unwrap();
        assert_eq!(file["bytes"], 22);
        assert!(
            invoke(
                &host,
                "second",
                FILES_SERVICE,
                "release",
                json!({"file":file["file"]})
            )
            .await
            .is_err()
        );
        let pending = host.dispatch(Call {
            caller: PluginId::new("first").unwrap(),
            service: FILES_SERVICE.into(),
            method: "choose".into(),
            args: json!({}),
        });
        host.revoke(&PluginId::new("first").unwrap());
        assert!(matches!(pending.await, Outcome::Error { .. }));
        assert!(
            invoke(
                &host,
                "first",
                FILES_SERVICE,
                "release",
                json!({"file":file["file"]})
            )
            .await
            .is_err()
        );
        assert!(host.state().unwrap().files.is_empty());
        assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
    }
    #[tokio::test]
    async fn authorization_binds_snapshot_and_cannot_be_borrowed_or_change_origin() {
        let scratch = tempfile::tempdir().unwrap();
        let host = FileHttpServices::new(scratch.path().to_owned(), Arc::new(TestUi));
        let file = invoke(&host, "first", FILES_SERVICE, "choose", json!({}))
            .await
            .unwrap();
        assert!(
            invoke(
                &host,
                "second",
                HTTP_SERVICE,
                "authorize",
                json!({"file":file["file"],"origin":"https://example.invalid"})
            )
            .await
            .is_err()
        );
        let authorization = invoke(
            &host,
            "first",
            HTTP_SERVICE,
            "authorize",
            json!({"file":file["file"],"origin":"https://example.invalid"}),
        )
        .await
        .unwrap();
        let args = json!({"authorization":authorization["authorization"],"method":"POST","path":"https://elsewhere.invalid/upload"});
        assert!(
            invoke(&host, "first", HTTP_SERVICE, "request", args.clone())
                .await
                .is_err()
        );
        assert!(
            invoke(&host, "second", HTTP_SERVICE, "request", args)
                .await
                .is_err()
        );
        invoke(
            &host,
            "first",
            FILES_SERVICE,
            "release",
            json!({"file":file["file"]}),
        )
        .await
        .unwrap();
        assert!(host.state().unwrap().authorizations.is_empty());
    }
    #[tokio::test]
    async fn result_presentation_requires_current_caller_authorization_and_safe_origin() {
        let scratch = tempfile::tempdir().unwrap();
        let host = FileHttpServices::new(scratch.path().to_owned(), Arc::new(TestUi));
        let file = invoke(&host, "first", FILES_SERVICE, "choose", json!({}))
            .await
            .unwrap();
        let authorization = invoke(
            &host,
            "first",
            HTTP_SERVICE,
            "authorize",
            json!({"file":file["file"],"origin":"https://example.invalid"}),
        )
        .await
        .unwrap();
        let args = json!({
            "authorization":authorization["authorization"],
            "link":"https://example.invalid/f/result"
        });
        assert_eq!(
            invoke(&host, "first", HTTP_SERVICE, "present", args.clone())
                .await
                .unwrap(),
            json!({"presented":true})
        );
        assert!(
            invoke(&host, "second", HTTP_SERVICE, "present", args.clone())
                .await
                .is_err()
        );
        for link in [
            "https://elsewhere.invalid/f/result",
            "http://example.invalid/f/result",
            "https://user@example.invalid/f/result",
            "https://example.invalid/f/result#fragment",
            "https://example.invalid/f/synthetic-test-credential",
        ] {
            let mut invalid = args.clone();
            invalid["link"] = json!(link);
            assert!(
                invoke(&host, "first", HTTP_SERVICE, "present", invalid)
                    .await
                    .is_err()
            );
        }
        host.revoke(&PluginId::new("first").unwrap());
        assert!(
            invoke(&host, "first", HTTP_SERVICE, "present", args)
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn inline_result_presentation_is_refused_before_authorization_or_network_access() {
        let scratch = tempfile::tempdir().unwrap();
        let host = FileHttpServices::new(scratch.path().to_owned(), Arc::new(TestUi));
        let error = invoke(
            &host,
            "first",
            HTTP_SERVICE,
            "request",
            json!({"present_link":"/files/0/url"}),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error,
            "Validate the response before requesting result presentation"
        );
    }
    #[test]
    fn origins_reject_credentials_paths_downgrades_and_query_strings() {
        for origin in [
            "http://example.invalid",
            "https://user@example.invalid",
            "https://example.invalid/upload",
            "https://example.invalid/?a=1",
            "https://example.invalid/#fragment",
        ] {
            assert!(approved_origin(origin).is_err());
        }
        assert_eq!(
            approved_origin("https://example.invalid").unwrap().as_str(),
            "https://example.invalid/"
        );
    }
    #[test]
    fn provider_multipart_field_is_preserved_and_header_injection_is_rejected() {
        assert_eq!(multipart_field(&json!({"field":"files"})).unwrap(), "files");
        for value in [
            json!({}),
            json!({"field":""}),
            json!({"field":"files\r\ninjected"}),
            json!({"field":"a".repeat(65)}),
        ] {
            assert!(multipart_field(&value).is_err());
        }
    }
    #[test]
    fn opaque_secret_resolution_is_authorization_scoped_and_echoes_never_escape() {
        let secrets = HashMap::from([(
            "session".into(),
            Zeroizing::new("synthetic-session-token".into()),
        )]);
        assert_eq!(
            resolve_secret(&json!({"receipts":[{"secret":"session"}]}), &secrets).unwrap(),
            json!({"receipts":["synthetic-session-token"]})
        );
        assert!(resolve_secret(&json!({"secret":"other-authorization"}), &secrets).is_err());
        assert!(
            reject_secret_echo(
                &json!({"nested":["echo synthetic-session-token"]}),
                b"synthetic-credential",
                &secrets
            )
            .is_err()
        );
        assert!(
            reject_secret_echo(
                &json!({"synthetic-credential":"echo"}),
                b"synthetic-credential",
                &secrets
            )
            .is_err()
        );
        assert!(
            reject_secret_echo(
                &json!({"token":{"secret":"session"}}),
                b"synthetic-credential",
                &secrets
            )
            .is_ok()
        );
    }
}

#[cfg(test)]
mod transport_tests;
