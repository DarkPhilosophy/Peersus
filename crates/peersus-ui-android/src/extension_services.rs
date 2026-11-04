//! Trusted Android adapter. Provider protocols remain in sandboxed extensions.
use peersus_app::platform_services::{
    Call, FILES_SERVICE, HTTP_SERVICE, MAX_FILE_BYTES, PlatformServices, Response, Snapshot,
};
use peersus_core::plugins::PluginId;
use peersus_plugin_api::message::Outcome;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::Semaphore;
use zeroize::Zeroizing;

struct Selection {
    caller: PluginId,
    snapshot: Arc<Snapshot>,
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
    input: peersus_app::platform_services::SelectedInput,
}
#[derive(Default)]
struct State {
    next: u64,
    generations: HashMap<PluginId, u64>,
    files: HashMap<String, Selection>,
    authorizations: HashMap<String, Authorization>,
    contexts: HashMap<u64, InputContext>,
}
impl State {
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
    workers: Arc<Semaphore>,
    scratch: PathBuf,
}
impl AndroidServices {
    pub fn new(scratch: PathBuf) -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
            workers: Arc::new(Semaphore::new(2)),
            scratch,
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
                        })
                        .ok_or("Selected input is unavailable")?;
                    Some(
                        state
                            .contexts
                            .remove(&context)
                            .ok_or("Selected input is unavailable")?
                            .input,
                    )
                } else {
                    None
                };
                let picked = if selected.is_none() {
                    Some(crate::extension_files::choose().await?)
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
                            (
                                Some(peersus_app::platform_services::SelectedInput::Path(path)),
                                None,
                            ) => {
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
                                Some(peersus_app::platform_services::SelectedInput::Held {
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
                state.files.insert(
                    handle,
                    Selection {
                        caller: call.caller,
                        snapshot,
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
                state.files.remove(handle);
                Ok(json!({"released":true}))
            }
            (HTTP_SERVICE, "configure") => {
                let origin = approved_origin(argument(&call.args, "origin")?)?;
                // A configuration dialog has no file and cannot authorize a request.
                let _completion = crate::extension_authorization::request(
                    call.caller.as_str(),
                    origin.as_str(),
                    "",
                    0,
                )
                .await?;
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
                let credential = crate::extension_authorization::request(
                    call.caller.as_str(),
                    origin.as_str(),
                    snapshot.name(),
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
            _ => Err("Unsupported platform operation"),
        }
    }
    async fn request(&self, call: Call, generation: u64) -> Result<Value, &'static str> {
        let handle = argument(&call.args, "authorization")?.to_owned();
        let (origin, credential, snapshot, secrets) = {
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
            )
        };
        let path = argument(&call.args, "path")?;
        let url = origin.join(path).map_err(|_| "Invalid request path")?;
        if url.origin() != origin.origin()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err("The request must stay within the approved origin");
        }
        let method = argument(&call.args, "method")?;
        if !matches!(method, "GET" | "POST" | "PUT" | "DELETE") {
            return Err("Unsupported HTTPS method");
        }
        let client = reqwest::Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|_| "HTTPS is unavailable")?;
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
        let mut response = request
            .send()
            .await
            .map_err(|_| "HTTPS failed; no retry was attempted")?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Ok(json!({"status":status}));
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
            if auth.secrets.len() >= 64 {
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
        let link = call
            .args
            .get("present_link")
            .map(|pointer| {
                let pointer = pointer
                    .as_str()
                    .filter(|pointer| pointer.starts_with('/') && pointer.len() <= 256)
                    .ok_or("Invalid result link field")?;
                let text = value
                    .pointer(pointer)
                    .and_then(Value::as_str)
                    .filter(|text| text.len() <= 2048)
                    .ok_or("Missing result link")?;
                let link = reqwest::Url::parse(text).map_err(|_| "Invalid result link")?;
                if link.origin() != origin.origin()
                    || !link.username().is_empty()
                    || link.password().is_some()
                    || link.fragment().is_some()
                {
                    return Err("The result link must stay within the approved origin");
                }
                Ok(link.to_string())
            })
            .transpose()?;
        drop(state);
        if let Some(link) = link {
            crate::bridge::present_extension_result(call.caller.as_str(), &link)?;
        }
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
impl PlatformServices for AndroidServices {
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
        input: peersus_app::platform_services::SelectedInput,
    ) -> Result<u64, &'static str> {
        let mut state = self.state()?;
        let now = std::time::Instant::now();
        state.contexts.retain(|_, entry| entry.expires > now);
        if state.contexts.len() >= 4 {
            return Err("Too many selected inputs are pending");
        }
        let mut context = rand::random::<u64>();
        while context == 0 || state.contexts.contains_key(&context) {
            context = rand::random::<u64>();
        }
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn invoke(
        host: &AndroidServices,
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
        let host = AndroidServices::new(scratch.path().to_owned());
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
        use peersus_app::platform_services::SelectedInput;
        use std::io::Write;
        let scratch = tempfile::tempdir().unwrap();
        let host = AndroidServices::new(scratch.path().to_owned());
        let held = peersus_net::held::Held::default();
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
        use peersus_app::platform_services::SelectedInput;
        let scratch = tempfile::tempdir().unwrap();
        let host = AndroidServices::new(scratch.path().to_owned());
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
        let host = AndroidServices::new(scratch.path().to_owned());
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
        let host = AndroidServices::new(scratch.path().to_owned());
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
