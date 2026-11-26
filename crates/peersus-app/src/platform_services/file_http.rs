//! Shared private-file and origin-bound HTTP services. Platform UI is injected; provider protocols stay in guests.
use super::{
    Call, FILES_SERVICE, HTTP_SERVICE, MAX_FILE_BYTES, PlatformServices, Response, Snapshot,
};
use crate::notifications::Notifications;
use bytes::Bytes;
use peersus_api::message::Outcome;
use peersus_api::notification::{Answer, Button, Notification, Reach};
use peersus_core::model::NotificationSource;
use peersus_core::plugins::PluginId;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::Semaphore;
use zeroize::Zeroizing;

mod public_read;

/// Trusted UI boundary: only platform code sees credentials or opens OS dialogs.
pub type UiResponse<T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, &'static str>> + Send>>;

/// `Ok(None)` means the store confirmed absence; an unreadable store or entry is `Err`.
pub type CredentialResponse = UiResponse<Option<Zeroizing<Vec<u8>>>>;

/// What only a platform can do for this service. What the person is asked and told (may this file go to that address, here is
/// the link) is not here: the service asks it itself, the same on every platform, through the application's notifications.
pub trait Interaction: Send + Sync {
    /// Ask the person for one file, in the picker the platform has for `kind`.
    fn choose(
        &self,
        caller: PluginId,
        kind: crate::platform_services::FileKind,
    ) -> UiResponse<(std::fs::File, String)>;
    fn configure(&self, caller: PluginId, origin: reqwest::Url) -> UiResponse<()>;
    /// Read the key for a check or a keyed authorization, without returning it to the guest.
    fn credential(&self, caller: PluginId, origin: reqwest::Url) -> CredentialResponse;
    /// Confirm whether a key is saved. Linux overrides this to search without requesting an unlock.
    fn credential_saved(&self, caller: PluginId, origin: reqwest::Url) -> UiResponse<bool> {
        let read = self.credential(caller, origin);
        Box::pin(async move { read.await.map(|credential| credential.is_some()) })
    }
    /// Put a result link on the clipboard, after the person asked for it.
    fn copy(&self, link: String) -> UiResponse<()>;
}

/// How long the person has to answer a question of this service before the call that asked is given up.
const ANSWER_WITHIN: std::time::Duration = std::time::Duration::from_secs(300);

const CREDENTIAL_UNAVAILABLE: &str = "The credential could not be read";
const CREDENTIAL_MISSING: &str = "Configure the API key in this extension's settings first";

/// The answers this service offers, as it is told them.
const ALLOW: &str = "allow";
const DECLINE: &str = "decline";
const COPY: &str = "copy";
/// The most characters of a file's name and of a link that are shown; the rest is left out of the middle.
const MOST_NAME_SHOWN: usize = 60;
const MOST_LINK_SHOWN: usize = 160;

/// `text` as it is when it has at most `most` characters; else its start and its end around `…`, `most` characters in all.
fn shortened(text: &str, most: usize) -> String {
    let count = text.chars().count();
    if count <= most {
        return text.to_owned();
    }
    let tail = most / 3;
    let head = most - tail - 1;
    text.chars()
        .take(head)
        .chain(['…'])
        .chain(text.chars().skip(count - tail))
        .collect()
}

/// What the person is asked before a file leaves for `origin`: which file, how large, where to, that it goes as it is, and the
/// request headers of its own the extension said it will send there, since the application does not know what a destination
/// reads in them. An anonymous authorization says that no key is sent either, and one that named a result origin says where the
/// link it returns will be, since that is another address the file's result will live on. The application's words, never the
/// extension's. The address is never shortened: one too long to show is refused by the contract's bound, and nothing is sent.
fn consent(
    file: &str,
    bytes: u64,
    origin: &reqwest::Url,
    headers: &[String],
    anonymous: bool,
    result: Option<&reqwest::Url>,
) -> Notification {
    // A name is the person's own text and may hold anything; what cannot be shown in one paragraph is shown as a space.
    let file: String = shortened(file, MOST_NAME_SHOWN)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut text = format!(
        "{file} ({bytes} bytes) → {origin} The file is sent to this address as it is, without end-to-end encryption."
    );
    if anonymous {
        text.push_str(" The application will not use a saved key.");
    }
    if !headers.is_empty() {
        text.push_str(" It also sends request headers of its own there: ");
        text.push_str(&headers.join(", "));
        text.push('.');
    }
    if let Some(result) = result {
        text.push_str(" The link it returns will be at ");
        text.push_str(result.as_str());
        text.push('.');
    }
    Notification {
        text,
        reach: Reach::Inside,
        stays: true,
        buttons: vec![
            Button {
                id: ALLOW.into(),
                label: "Allow".into(),
            },
            Button {
                id: DECLINE.into(),
                label: "Don't allow".into(),
            },
        ],
        opens: None,
    }
}

/// The link a destination answered with, shown until the person copies it or closes it.
fn result_link(link: &str) -> Notification {
    Notification {
        text: shortened(link, MOST_LINK_SHOWN),
        reach: Reach::Inside,
        stays: true,
        buttons: vec![Button {
            id: COPY.into(),
            label: "Copy link".into(),
        }],
        opens: None,
    }
}

struct Selection {
    caller: PluginId,
    snapshot: Arc<Snapshot>,
    progress: Option<Arc<FileProgress>>,
}
struct Authorization {
    caller: PluginId,
    file: String,
    origin: reqwest::Url,
    /// The key saved for `origin`, when this authorization sends one. `None` is a destination that needs none: nothing is read
    /// for it, nothing is sent with it, and nothing of it can be found in an answer.
    credential: Option<Zeroizing<Vec<u8>>>,
    /// Where a link this destination returns is allowed to live, when the extension named one and it is not `origin` itself:
    /// shown to the person with the consent, never requested. A request of this authorization always goes to `origin`.
    result_origin: Option<reqwest::Url>,
    /// The request headers of its own the extension named when it asked, as the person was shown them.
    headers: Vec<String>,
    secrets: HashMap<String, Zeroizing<String>>,
}
struct InputContext {
    caller: PluginId,
    generation: u64,
    expires: std::time::Instant,
    input: crate::platform_services::SelectedInput,
    track_progress: bool,
}
struct State {
    next: u64,
    generations: HashMap<PluginId, u64>,
    files: HashMap<String, Selection>,
    authorizations: HashMap<String, Authorization>,
    contexts: HashMap<u64, InputContext>,
    resources: HashMap<PluginId, public_read::Access>,
}
impl State {
    fn new() -> Self {
        Self {
            next: 0,
            generations: HashMap::new(),
            files: HashMap::new(),
            authorizations: HashMap::new(),
            contexts: HashMap::new(),
            resources: HashMap::new(),
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
    resource_workers: Arc<Semaphore>,
    scratch: PathBuf,
    resource_approvals: Option<PathBuf>,
    ui: Arc<dyn Interaction>,
    notifications: Arc<Notifications>,
    progress: tokio::sync::watch::Sender<super::ActionProgress>,
    #[cfg(test)]
    test_root: Option<reqwest::Certificate>,
    #[cfg(test)]
    test_proxy: Option<reqwest::Url>,
}
impl FileHttpServices {
    /// `notifications` is the application's own list: what this service asks is shown where everything else is.
    pub fn new(
        scratch: PathBuf,
        ui: Arc<dyn Interaction>,
        notifications: Arc<Notifications>,
    ) -> Self {
        Self {
            notifications,
            state: Arc::new(Mutex::new(State::new())),
            workers: Arc::new(Semaphore::new(2)),
            resource_workers: Arc::new(Semaphore::new(2)),
            scratch,
            resource_approvals: None,
            ui,
            progress: tokio::sync::watch::channel(HashMap::new()).0,
            #[cfg(test)]
            test_root: None,
            #[cfg(test)]
            test_proxy: None,
        }
    }
    /// The application's private persistent directory, not upload scratch.
    pub fn with_public_read_approvals(mut self, data: PathBuf) -> Self {
        self.resource_approvals = Some(data);
        self
    }
    /// A selection cannot be recovered by a different extension.
    pub fn file_snapshot(&self, caller: &PluginId, file: &str) -> Option<Arc<Snapshot>> {
        self.state()
            .ok()?
            .files
            .get(file)
            .filter(|entry| &entry.caller == caller)
            .map(|entry| entry.snapshot.clone())
    }

    pub fn public_read_directory(&self) -> Option<&std::path::Path> {
        self.resource_approvals.as_deref()
    }

    pub(crate) fn forget_public_read(data: &std::path::Path, caller: &PluginId) {
        public_read::forget(data, caller);
    }

    pub(crate) fn public_read_approved(
        data: &std::path::Path,
        caller: &PluginId,
        agreement: &str,
        scope: &peersus_api::contributions::PublicRead,
        legacy_enabled: bool,
    ) -> bool {
        public_read::approved(data, caller, agreement, scope, legacy_enabled)
    }

    /// Called by the owner following an exact, explicit Settings action, not by an extension.
    pub(crate) fn decide_public_read(
        data: &std::path::Path,
        caller: &PluginId,
        agreement: &str,
        scope: &peersus_api::contributions::PublicRead,
        enabled: bool,
    ) -> Result<(), &'static str> {
        public_read::decide(data, caller, agreement, scope, enabled)
    }
    fn state(&self) -> Result<std::sync::MutexGuard<'_, State>, &'static str> {
        self.state
            .lock()
            .map_err(|_| "Platform services are unavailable")
    }

    /// Checks and keyed authorization use the same read. Adapter errors never cross the trust
    /// boundary, and a revoked call cannot use the result.
    async fn read_credential(
        &self,
        caller: &PluginId,
        origin: reqwest::Url,
        generation: u64,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, &'static str> {
        self.state()?.current(caller, generation)?;
        let credential = self.ui.credential(caller.clone(), origin).await;
        self.state()?.current(caller, generation)?;
        credential.map_err(|_| CREDENTIAL_UNAVAILABLE)
    }

    /// Ask the person something for `caller`, under its name, and wait for the answer.
    async fn ask(&self, caller: &PluginId, asked: Notification) -> Result<Answer, &'static str> {
        let asking = self
            .notifications
            .ask(NotificationSource::Plugin(caller.clone()), asked);
        match tokio::time::timeout(ANSWER_WITHIN, asking).await {
            Ok(Ok(answer)) => Ok(answer),
            Ok(Err(peersus_api::Error::QueueFull)) => {
                Err("This extension already has questions waiting for an answer")
            }
            Ok(Err(peersus_api::Error::InvalidPayload)) => {
                Err("The request is too long to show for approval")
            }
            // Nobody answered in time, or the extension stopped while it was asked.
            _ => Err("Authorization was cancelled"),
        }
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
                let kind = crate::platform_services::FileKind::from_arg(call.args.get("kind"))?;
                if call.method == "selected" && call.args.get("kind").is_some() {
                    return Err("The selected file has no kind to choose");
                }
                let mut selected_context = None;
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
                    selected_context = entry.track_progress.then_some(context);
                    Some(entry.input)
                } else {
                    None
                };
                let picked = if selected.is_none() {
                    Some(self.ui.choose(call.caller.clone(), kind).await?)
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
                let progress = selected_context.map(|context| {
                    let meter = Arc::new(FileProgress::new(
                        context,
                        call.caller.clone(),
                        snapshot.bytes(),
                    ));
                    self.progress.send_modify(|progress| {
                        progress.insert(context, meter.clone());
                    });
                    meter
                });
                state.files.insert(
                    handle,
                    Selection {
                        caller: call.caller,
                        snapshot,
                        progress,
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
                    && let Some(meter) = file.progress
                {
                    self.progress
                        .send_if_modified(|progress| progress.remove(&meter.context).is_some());
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
                let headers = declared_headers(&call.args)?;
                // Anonymous: the extension says this destination needs no key, so none is read for it, saved or not. The
                // person is asked all the same, and the answer binds this file to this address exactly as a keyed one does.
                let anonymous = match call.args.get("anonymous") {
                    None => false,
                    Some(flag) => flag.as_bool().ok_or("Invalid anonymous flag")?,
                };
                // A separate address for the links this destination returns: named by the extension, shown with the consent,
                // and never requested. Equal to the origin, it is no separate address at all.
                let result_origin = match call.args.get("result_origin") {
                    None => None,
                    Some(text) => {
                        let named = approved_origin(text.as_str().ok_or("Invalid HTTPS origin")?)?;
                        (named.origin() != origin.origin()).then_some(named)
                    }
                };
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
                // The key must exist before the person is asked to allow anything: an approval that can only end in "no key" is a
                // question for nothing. A destination that needs no key is asked about none.
                let credential = if anonymous {
                    None
                } else {
                    Some(
                        self.read_credential(&call.caller, origin.clone(), generation)
                            .await?
                            .ok_or(CREDENTIAL_MISSING)?,
                    )
                };
                let asked = consent(
                    snapshot.name(),
                    snapshot.bytes(),
                    &origin,
                    &headers,
                    anonymous,
                    result_origin.as_ref(),
                );
                if self.ask(&call.caller, asked).await? != Answer::Pressed(ALLOW.into()) {
                    return Err("Authorization was declined");
                }
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
                        result_origin,
                        headers,
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
                    reject_secret_echo(
                        &json!(text),
                        auth.credential.as_deref().map(Vec::as_slice),
                        &auth.secrets,
                    )?;
                    // The address the person allowed for the result: the one the extension named, or the destination's own
                    // when it named none. A link anywhere else is not shown.
                    let allowed = auth.result_origin.as_ref().unwrap_or(&auth.origin);
                    let link = reqwest::Url::parse(text).map_err(|_| "Invalid result link")?;
                    if link.origin() != allowed.origin()
                        || !link.username().is_empty()
                        || link.password().is_some()
                        || link.fragment().is_some()
                    {
                        return Err("The result link must stay within the approved origin");
                    }
                    link.to_string()
                };
                if self.ask(&call.caller, result_link(&link)).await? == Answer::Pressed(COPY.into())
                {
                    self.ui.copy(link).await?;
                }
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
            (
                HTTP_SERVICE,
                "authorize-read"
                | "restore-read"
                | "get-resource"
                | "read-resource"
                | "release-read"
                | "resource-cache"
                | "clear-resource-cache",
            ) => self.public_read(call, generation).await,
            // No prompt, network or credential data in the reply. False means confirmed absence,
            // not that reading a possible credential failed.
            (HTTP_SERVICE, "saved") => {
                let origin = approved_origin(argument(&call.args, "origin")?)?;
                self.state()?.current(&call.caller, generation)?;
                let saved = self.ui.credential_saved(call.caller.clone(), origin).await;
                self.state()?.current(&call.caller, generation)?;
                Ok(json!({"saved": saved.map_err(|_| CREDENTIAL_UNAVAILABLE)?}))
            }
            _ => Err("Unsupported platform operation"),
        }
    }
    /// Build the HTTPS client for requests and credential checks.
    fn https_client(&self) -> Result<reqwest::Client, &'static str> {
        self.https_client_builder()?
            .build()
            .map_err(|_| "HTTPS is unavailable")
    }

    /// Shared HTTPS defaults, including those for public resources: no proxy, redirect or retry.
    fn https_client_builder(&self) -> Result<reqwest::ClientBuilder, &'static str> {
        let builder = reqwest::Client::builder()
            .https_only(true)
            .user_agent(concat!("Peersus/", env!("CARGO_PKG_VERSION")))
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
        Ok(builder)
    }
    /// Which way an HTTPS request failed, in a fixed sentence: nothing of the address, the key or any answer is in it, so it can be
    /// told to the extension and shown to the person. Nothing was retried in any case.
    fn https_failed(error: &reqwest::Error) -> &'static str {
        if error.is_timeout() {
            return "HTTPS failed: the address did not answer in time; no retry was attempted";
        }
        // `io::Error` hides what it wraps from `source()`, and the TLS error sits two of them deep (seen: Other → InvalidData →
        // rustls), so each wrapped error is entered as well as each source.
        let mut cause: Option<&(dyn std::error::Error + 'static)> =
            std::error::Error::source(error);
        while let Some(now) = cause {
            if let Some(tls) = now.downcast_ref::<rustls::Error>() {
                use rustls::CertificateError as Certificate;
                return match tls {
                    rustls::Error::InvalidCertificate(Certificate::UnknownIssuer) => {
                        "HTTPS failed: the address's certificate is not from an issuer this device trusts; no retry was attempted"
                    }
                    rustls::Error::InvalidCertificate(
                        Certificate::Expired
                        | Certificate::ExpiredContext { .. }
                        | Certificate::NotValidYet
                        | Certificate::NotValidYetContext { .. },
                    ) => {
                        "HTTPS failed: the address's certificate is not valid at this device's date (is its clock right?); no retry was attempted"
                    }
                    rustls::Error::InvalidCertificate(
                        Certificate::NotValidForName | Certificate::NotValidForNameContext { .. },
                    ) => {
                        "HTTPS failed: the address's certificate is for another name; no retry was attempted"
                    }
                    rustls::Error::InvalidCertificate(_) => {
                        "HTTPS failed: the address's certificate was not accepted; no retry was attempted"
                    }
                    // What a certificate verifier says when it could not do its work at all, as opposed to having refused the
                    // certificate. The two sentences of the Android verifier (rustls-platform-verifier) are told apart: the
                    // system answered that it has no trust store, or the system's checker could not be called at all.
                    rustls::Error::General(said) if said.starts_with("No system trust stores") => {
                        "HTTPS failed: this device's system gave no certificate trust store to the application; no retry was attempted"
                    }
                    rustls::Error::General(said)
                        if said.starts_with("failed to call native verifier") =>
                    {
                        "HTTPS failed: the system's certificate checker could not be called; no retry was attempted"
                    }
                    rustls::Error::General(_) | rustls::Error::Other(_) => {
                        "HTTPS failed: this device could not check the address's certificate; no retry was attempted"
                    }
                    _ => {
                        "HTTPS failed: the secure connection was refused (its certificate or its TLS); no retry was attempted"
                    }
                };
            }
            cause = match now
                .downcast_ref::<std::io::Error>()
                .and_then(std::io::Error::get_ref)
            {
                Some(inner) => Some(inner as &(dyn std::error::Error + 'static)),
                None => now.source(),
            };
        }
        if error.is_connect() {
            "HTTPS failed: the address could not be reached; no retry was attempted"
        } else {
            "HTTPS failed; no retry was attempted"
        }
    }
    /// Whether the key the person saved for an origin is accepted there: one `GET` of a path the extension names, with the saved key as
    /// the bearer. Needs no selected file and asks nothing of the person. Only the status comes back: the body belongs to the provider
    /// and the key is never given to the extension, so neither can leak through this answer.
    async fn check(&self, call: Call, generation: u64) -> Result<Value, &'static str> {
        let origin = approved_origin(argument(&call.args, "origin")?)?;
        let url = within_origin(&origin, argument(&call.args, "path")?)?;
        let credential = self
            .read_credential(&call.caller, origin.clone(), generation)
            .await?
            .ok_or(CREDENTIAL_MISSING)?;
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
            .map_err(|error| Self::https_failed(&error))?;
        Ok(json!({"status":response.status().as_u16()}))
    }
    async fn request(&self, call: Call, generation: u64) -> Result<Value, &'static str> {
        if call.args.get("present_link").is_some() {
            return Err("Validate the response before requesting result presentation");
        }
        let handle = argument(&call.args, "authorization")?.to_owned();
        let (origin, credential, snapshot, secrets, progress, declared) = {
            let state = self.state()?;
            state.current(&call.caller, generation)?;
            let auth = state
                .authorizations
                .get(&handle)
                .filter(|auth| auth.caller == call.caller)
                .ok_or("Unknown authorization")?;
            let file = state.files.get(&auth.file).ok_or("Unknown selected file")?;
            (
                auth.origin.clone(),
                auth.credential
                    .as_ref()
                    .map(|credential| Zeroizing::new(credential.to_vec())),
                file.snapshot.clone(),
                auth.secrets.clone(),
                file.progress.clone(),
                auth.headers.clone(),
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
            // An anonymous destination has no key, and no empty `Bearer` is ever sent: the refusal stands here, in the host,
            // whatever the extension believes it asked for.
            let credential = credential
                .as_deref()
                .map(Vec::as_slice)
                .ok_or("No credential is saved for this destination")?;
            let mut header = reqwest::header::HeaderValue::from_bytes(
                &[b"Bearer ".as_slice(), credential].concat(),
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
                // No ambient cookies, host overrides or credential forwarding: only what the person was shown when asked.
                if !header_allowed(name, &declared) {
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
        // A destination may answer with plain text: a link, or its reason for refusing. It is read as text under its own small
        // bound and returned as it came, so the extension can decide whether it is what the destination promises; nothing is
        // parsed and no secret field can be captured from it.
        let text_mode = match call.args.get("response") {
            None => false,
            Some(kind) => match kind.as_str() {
                Some("json") => false,
                Some("text") => true,
                _ => return Err("Invalid response kind"),
            },
        };
        if text_mode && call.args.get("capture_secrets").is_some() {
            return Err("A text response carries no secret fields");
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
            let body = if let Some(progress) = progress {
                self.track_body(progress, offset, bytes)
            } else {
                reqwest::Body::from(bytes)
            };
            if call.args.get("multipart").is_some() {
                let field = multipart_field(part)?;
                let mut form = reqwest::multipart::Form::new().part(
                    field,
                    reqwest::multipart::Part::stream_with_length(body, length).file_name(name),
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
                request = request
                    .header(reqwest::header::CONTENT_LENGTH, length)
                    .body(body);
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
            .map_err(|error| Self::https_failed(&error))?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Ok(json!({"status":status}));
        }
        let mut bytes = Zeroizing::new(Vec::new());
        let bound = if text_mode { MOST_TEXT } else { 32 * 1024 };
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "HTTPS response was interrupted")?
        {
            if chunk.len() > bound.saturating_sub(bytes.len()) {
                return Err("HTTPS response exceeds its bound");
            }
            bytes.extend_from_slice(&chunk);
        }
        let mut value = if text_mode {
            let text = std::str::from_utf8(&bytes).map_err(|_| "Invalid text response")?;
            if text
                .chars()
                .any(|c| !c.is_ascii_graphic() && !matches!(c, ' ' | '\r' | '\n' | '\t'))
            {
                return Err("Invalid text response");
            }
            Value::String(text.to_owned())
        } else {
            serde_json::from_slice(&bytes).map_err(|_| "Invalid JSON response")?
        };
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
            if auth.secrets.len() >= MOST_SECRETS {
                return Err("Too many response secrets");
            }
            auth.secrets.insert(secret_handle.clone(), secret);
            *field = json!({"secret":secret_handle});
        }
        let auth = state
            .authorizations
            .get(&handle)
            .ok_or("Authorization was released")?;
        reject_secret_echo(
            &value,
            auth.credential.as_deref().map(Vec::as_slice),
            &auth.secrets,
        )?;
        if serde_json::to_vec(&value)
            .map_err(|_| "Invalid JSON response")?
            .len()
            > 32 * 1024
        {
            return Err("HTTPS response exceeds its bound");
        }
        // Response validation does not affect transport progress or mark the action done.
        drop(state);
        Ok(if text_mode {
            json!({"status":status,"text":value})
        } else {
            json!({"status":status,"json":value})
        })
    }

    fn track_body(
        &self,
        progress: Arc<FileProgress>,
        offset: u64,
        bytes: Vec<u8>,
    ) -> reqwest::Body {
        const CHUNK: usize = 64 * 1024;
        let watch = self.progress.clone();
        let bytes = Bytes::from(bytes);
        reqwest::Body::wrap_stream(futures_util::stream::iter(
            (0..bytes.len()).step_by(CHUNK).map(move |start| {
                let chunk = bytes.slice(start..(start + CHUNK).min(bytes.len()));
                if progress.consume(offset + start as u64, chunk.len() as u64) {
                    // Later chunks only change the pending sample. The owner sees one
                    // wake until it samples; a detached stream cannot wake a new action.
                    watch.send_if_modified(|meters| meters.contains_key(&progress.context));
                }
                Ok::<_, std::convert::Infallible>(chunk)
            }),
        ))
    }
}
/// The most values one authorization keeps from responses. A file sent in parts has a receipt for each part and something
/// for the whole: the largest file the service takes, in parts of a few MiB, stays under a hundred.
const MOST_SECRETS: usize = 128;
/// The most request headers of its own an extension names for one authorization.
const MOST_HEADERS: usize = 8;
/// The most bytes of a text response. A destination answers with a link or with the reason it refused; a body that needs more
/// than this is not that, and is refused rather than shown.
const MOST_TEXT: usize = 4096;

/// Whether an extension may send request header `name` with an authorization for which it named `declared`: what the
/// body is, and what the person was shown when asked. No header is known here by a destination's name for it.
fn header_allowed(name: &str, declared: &[String]) -> bool {
    name == "content-type" || declared.iter().any(|allowed| allowed == name)
}

/// Request headers an extension never sends, named or not: those the transport or the application writes (the host, the
/// lengths and encodings, the bearer), the one that carries what a browser holds for a site, and those a server or a
/// proxy in front of it reads as where a request came from or what it really asks. They are the names a browser forbids a
/// page to set (the Fetch standard's forbidden request headers), with `authorization` and the forwarding ones.
fn header_forbidden(name: &str) -> bool {
    const NAMES: &[&str] = &[
        "accept-charset",
        "accept-encoding",
        "access-control-request-headers",
        "access-control-request-method",
        "authorization",
        "connection",
        "content-length",
        "cookie",
        "cookie2",
        "date",
        "dnt",
        "expect",
        "forwarded",
        "host",
        "keep-alive",
        "origin",
        "referer",
        "set-cookie",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
        "user-agent",
        "via",
        "x-host",
        "x-http-method",
        "x-http-method-override",
        "x-method-override",
        "x-real-ip",
    ];
    const PREFIXES: &[&str] = &[
        "proxy-",
        "sec-",
        "x-forwarded-",
        "x-original-",
        "x-rewrite-",
    ];
    NAMES.contains(&name) || PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}

/// The request headers of its own an extension says it will send, from `authorize`'s `headers`: lower-case names, each
/// once. `content-type` needs no naming. A name that is not a header name, or one of [`header_forbidden`], refuses the call
/// before the person is asked anything.
fn declared_headers(args: &Value) -> Result<Vec<String>, &'static str> {
    const INVALID: &str = "Invalid request header names";
    let Some(names) = args.get("headers") else {
        return Ok(Vec::new());
    };
    let names = names
        .as_array()
        .filter(|names| names.len() <= MOST_HEADERS)
        .ok_or(INVALID)?;
    let mut declared: Vec<String> = Vec::new();
    for name in names {
        let name = name.as_str().ok_or(INVALID)?;
        let shaped = !name.is_empty()
            && name.len() <= 64
            && name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        if !shaped {
            return Err(INVALID);
        }
        if header_forbidden(name) {
            return Err("An extension cannot send that request header");
        }
        if name != "content-type" && !declared.iter().any(|known| known == name) {
            declared.push(name.to_owned());
        }
    }
    Ok(declared)
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
/// Nothing a destination answers may carry the key it was sent or a secret this authorization captured. An anonymous
/// authorization has no key, so only the captured secrets can be found.
fn reject_secret_echo(
    value: &Value,
    credential: Option<&[u8]>,
    secrets: &HashMap<String, Zeroizing<String>>,
) -> Result<(), &'static str> {
    match value {
        Value::String(text) => {
            if let Some(credential) = credential {
                let credential =
                    std::str::from_utf8(credential).map_err(|_| "Invalid credential")?;
                if text.contains(credential) {
                    return Err("The response attempted to disclose a credential");
                }
            }
            if secrets
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
/// One snapshot's payload ranges consumed by the HTTP client, not provider acknowledgement.
pub struct FileProgress {
    context: u64,
    caller: PluginId,
    total: u64,
    pending: Mutex<PayloadRanges>,
}

#[derive(Default)]
struct PayloadRanges {
    ranges: Vec<(u64, u64)>,
    sent: u64,
    dirty: bool,
}

impl FileProgress {
    pub(crate) fn new(context: u64, caller: PluginId, total: u64) -> Self {
        Self {
            context,
            caller,
            total,
            pending: Mutex::new(PayloadRanges::default()),
        }
    }

    /// The owner is the single sampler. Clearing the dirtiness allows the next
    /// changed range to wake it without listing every chunk on the watch.
    pub(crate) fn sample(&self) -> Option<(u64, u64)> {
        let mut pending = self.pending.lock().ok()?;
        pending.dirty = false;
        Some((pending.sent, self.total))
    }

    pub(crate) fn consume(&self, offset: u64, length: u64) -> bool {
        let Ok(mut pending) = self.pending.lock() else {
            return false;
        };
        let end = offset.saturating_add(length).min(self.total);
        if end <= offset || !pending.add(offset, end) {
            return false;
        }
        !std::mem::replace(&mut pending.dirty, true)
    }
}

impl PayloadRanges {
    /// The intervals are sorted, with gaps between them. A duplicate or overlap
    /// counts only its new bytes. At the cap, an isolated interval can be left
    /// unreported, but an overlapping/bridging interval can still be merged.
    fn add(&mut self, start: u64, end: u64) -> bool {
        let left = self
            .ranges
            .partition_point(|&(_, known_end)| known_end < start);
        let right =
            left + self.ranges[left..].partition_point(|&(known_start, _)| known_start <= end);
        let previous = &self.ranges[left..right];
        if previous.is_empty() && self.ranges.len() >= 4096 {
            return false;
        }
        let start = previous.first().map_or(start, |range| start.min(range.0));
        let end = previous.last().map_or(end, |range| end.max(range.1));
        let old: u64 = previous.iter().map(|&(start, end)| end - start).sum();
        let delta = end - start - old;
        if delta == 0 {
            return false;
        }
        self.ranges.splice(left..right, [(start, end)]);
        self.sent += delta;
        true
    }
}

#[cfg(test)]
mod progress_tests {
    use super::{FileProgress, PayloadRanges, PluginId};

    #[test]
    fn retries_and_overlapping_parts_are_counted_only_once() {
        let mut sent = PayloadRanges::default();
        assert!(sent.add(0, 40));
        assert!(!sent.add(0, 40));
        assert!(sent.add(20, 70));
        assert_eq!(sent.sent, 70);
        assert!(sent.add(70, 100));
        assert_eq!(sent.sent, 100);
        assert_eq!(sent.ranges, [(0, 100)]);
    }

    #[test]
    fn out_of_order_parts_do_not_claim_the_gap_and_wake_only_until_sampled() {
        let meter = FileProgress::new(1, PluginId::new("fixture").unwrap(), 100);
        assert!(meter.consume(50, 50));
        assert!(!meter.consume(0, 25), "an unseen update already exists");
        assert_eq!(meter.sample(), Some((75, 100)));
        assert!(meter.consume(25, 25));
        assert_eq!(meter.sample(), Some((100, 100)));
    }

    #[test]
    fn empty_or_out_of_range_parts_cannot_inflate_progress() {
        let meter = FileProgress::new(1, PluginId::new("fixture").unwrap(), 100);
        assert!(!meter.consume(100, 10));
        assert!(!meter.consume(0, 0));
        assert!(meter.consume(90, u64::MAX));
        assert_eq!(meter.sample(), Some((10, 100)));
    }
}

impl PlatformServices for FileHttpServices {
    fn public_read_directory(&self) -> Option<&std::path::Path> {
        FileHttpServices::public_read_directory(self)
    }
    fn action_progress(&self) -> Option<tokio::sync::watch::Receiver<super::ActionProgress>> {
        Some(self.progress.subscribe())
    }
    fn select_for_action(
        &self,
        caller: &PluginId,
        input: super::SelectedInput,
        _action: &str,
    ) -> Result<u64, &'static str> {
        let context = self.select(caller, input)?;
        self.state()?
            .contexts
            .get_mut(&context)
            .ok_or("Selected input is unavailable")?
            .track_progress = true;
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
                    code: match message {
                        "Selection was cancelled"
                        | "Selection was cancelled or the file could not be opened"
                        | "Authorization was cancelled"
                        | "Authorization was declined" => "cancelled",
                        CREDENTIAL_UNAVAILABLE => "unavailable",
                        crate::platform_services::OUTSIDE_LIMIT => "too-large",
                        _ => "platform-refused",
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
                track_progress: false,
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
            state.resources.remove(caller);
            state
                .authorizations
                .retain(|_, auth| &auth.caller != caller);
            self.progress
                .send_modify(|progress| progress.retain(|_, meter| &meter.caller != caller));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct TestUi;
    impl Interaction for TestUi {
        fn choose(
            &self,
            _: PluginId,
            _: crate::platform_services::FileKind,
        ) -> UiResponse<(std::fs::File, String)> {
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
        fn credential(&self, _: PluginId, _: reqwest::Url) -> CredentialResponse {
            Box::pin(async { Ok(Some(Zeroizing::new(b"synthetic-test-credential".to_vec()))) })
        }
        fn copy(&self, _: String) -> UiResponse<()> {
            Box::pin(async { Ok(()) })
        }
    }
    impl FileHttpServices {
        /// The service as a person who agrees to everything meets it, and what that person was shown.
        pub(super) fn agreed(
            scratch: PathBuf,
            ui: Arc<dyn Interaction>,
        ) -> (Self, crate::notifications::Seen) {
            let (notifications, seen) = Notifications::agreeing();
            (Self::new(scratch, ui, notifications), seen)
        }
    }
    /// What the tests below build: the service with [`TestUi`], every question agreed to.
    fn service(scratch: &tempfile::TempDir) -> FileHttpServices {
        FileHttpServices::agreed(scratch.path().to_owned(), Arc::new(TestUi)).0
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
            Outcome::MissingPermission { .. } | Outcome::NotAccepted {} => {
                panic!("platform adapter is below permission admission")
            }
        }
    }
    #[tokio::test]
    async fn a_chosen_file_larger_than_the_callers_limit_is_refused_as_too_large() {
        let scratch = tempfile::tempdir().unwrap();
        let host = service(&scratch);
        let call = |max_bytes: u64| Call {
            caller: PluginId::new("chat").unwrap(),
            service: FILES_SERVICE.into(),
            method: "choose".into(),
            args: json!({"max_bytes": max_bytes, "kind": "audio"}),
        };
        // The synthetic file is 22 bytes.
        match host.dispatch(call(8)).await {
            Outcome::Error { code, .. } => assert_eq!(code, "too-large"),
            other => panic!("a file past the limit was {other:?}"),
        }
        assert!(matches!(host.dispatch(call(22)).await, Outcome::Ok { .. }));
    }
    #[tokio::test]
    async fn public_reads_remember_the_answer_across_host_restart_for_the_exact_agreement() {
        let scratch = tempfile::tempdir().unwrap();
        let restart = || {
            let (host, shown) =
                FileHttpServices::agreed(scratch.path().to_owned(), Arc::new(TestUi));
            (
                host.with_public_read_approvals(scratch.path().to_owned()),
                shown,
            )
        };
        let args = |agreement: &str| {
            json!({
                "origin":"https://example.test","root":"/maps","_accepted_agreement":agreement
            })
        };
        let agreement = "a".repeat(64);
        let (host, shown) = restart();
        assert!(
            invoke(&host, "map", HTTP_SERVICE, "restore-read", args(&agreement))
                .await
                .is_err()
        );
        assert!(shown.lock().unwrap().is_empty());
        assert!(
            invoke(
                &host,
                "map",
                HTTP_SERVICE,
                "authorize-read",
                args(&agreement)
            )
            .await
            .is_err()
        );
        assert!(shown.lock().unwrap().is_empty());
        let caller = PluginId::new("map").unwrap();
        let scope = peersus_api::contributions::PublicRead {
            origin: "https://example.test".into(),
            root: "/maps".into(),
        };
        FileHttpServices::decide_public_read(scratch.path(), &caller, &agreement, &scope, true)
            .unwrap();
        drop(host);

        let (host, shown) = restart();
        assert!(
            invoke(
                &host,
                "map",
                HTTP_SERVICE,
                "restore-read",
                args(&"b".repeat(64))
            )
            .await
            .is_err()
        );
        invoke(&host, "map", HTTP_SERVICE, "restore-read", args(&agreement))
            .await
            .unwrap();
        invoke(
            &host,
            "map",
            HTTP_SERVICE,
            "authorize-read",
            args(&agreement),
        )
        .await
        .unwrap();
        assert!(shown.lock().unwrap().is_empty());
        host.revoke(&caller);
        FileHttpServices::decide_public_read(scratch.path(), &caller, &agreement, &scope, false)
            .unwrap();
        assert!(
            invoke(&host, "map", HTTP_SERVICE, "restore-read", args(&agreement))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn public_reads_require_their_own_authorization_and_stay_with_its_caller() {
        let scratch = tempfile::tempdir().unwrap();
        let (host, shown) = FileHttpServices::agreed(scratch.path().to_owned(), Arc::new(TestUi));
        let host = host.with_public_read_approvals(scratch.path().to_owned());
        let caller = PluginId::new("map").unwrap();
        let agreement = "a".repeat(64);
        let scope = peersus_api::contributions::PublicRead {
            origin: "https://example.test".into(),
            root: "/maps".into(),
        };
        FileHttpServices::decide_public_read(scratch.path(), &caller, &agreement, &scope, true)
            .unwrap();
        let guessed = json!({"authorization":"resource-access-1","path":"/maps/a.pbf"});
        assert!(
            invoke(&host, "map", HTTP_SERVICE, "get-resource", guessed)
                .await
                .is_err()
        );
        let granted = invoke(
            &host,
            "map",
            HTTP_SERVICE,
            "restore-read",
            json!({"origin":scope.origin,"root":scope.root,"_accepted_agreement":agreement}),
        )
        .await
        .unwrap();
        let id = &granted["authorization"];
        let wrong_caller = json!({"authorization":id,"path":"/maps/a.pbf"});
        assert!(
            invoke(&host, "other", HTTP_SERVICE, "get-resource", wrong_caller)
                .await
                .is_err()
        );
        let wrong_root = json!({"authorization":id,"path":"/other/a.pbf"});
        assert!(
            invoke(&host, "map", HTTP_SERVICE, "get-resource", wrong_root)
                .await
                .is_err()
        );
        // The test fills only the resource pool: ordinary file operations have their own slots.
        let resource_pool = host.resource_workers.acquire_many(2).await.unwrap();
        assert!(
            invoke(
                &host,
                "map",
                HTTP_SERVICE,
                "get-resource",
                json!({
                    "authorization":id,"path":"/maps/a.pbf"
                })
            )
            .await
            .is_err()
        );
        assert!(
            invoke(&host, "file", FILES_SERVICE, "choose", json!({}))
                .await
                .is_ok()
        );
        drop(resource_pool);
        host.revoke(&PluginId::new("map").unwrap());
        assert!(
            invoke(
                &host,
                "map",
                HTTP_SERVICE,
                "get-resource",
                json!({
                    "authorization":id,"path":"/maps/a.pbf"
                })
            )
            .await
            .is_err()
        );
        assert!(shown.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_kept_public_resource_answers_without_the_network_and_is_counted_cleared_and_forgotten()
     {
        let scratch = tempfile::tempdir().unwrap();
        let (host, _) = FileHttpServices::agreed(scratch.path().to_owned(), Arc::new(TestUi));
        let host = host.with_public_read_approvals(scratch.path().to_owned());
        let caller = PluginId::new("map").unwrap();
        let agreement = "a".repeat(64);
        let scope = peersus_api::contributions::PublicRead {
            origin: "https://example.test".into(),
            root: "/maps".into(),
        };
        FileHttpServices::decide_public_read(scratch.path(), &caller, &agreement, &scope, true)
            .unwrap();
        let granted = invoke(
            &host,
            "map",
            HTTP_SERVICE,
            "restore-read",
            json!({"origin":scope.origin,"root":scope.root,"_accepted_agreement":agreement}),
        )
        .await
        .unwrap();
        let id = &granted["authorization"];
        // `example.test` is reserved and resolves nowhere: only a copy kept on this device can answer for it.
        let file = public_read::cache_file(
            scratch.path(),
            &caller,
            &reqwest::Url::parse("https://example.test/maps/a.pbf").unwrap(),
        );
        let keep = || {
            crate::private_fs::private_folders()
                .create(file.parent().unwrap())
                .unwrap();
            peersus_net::atomic::write_private(&file, b"kept bytes").unwrap();
        };
        keep();
        let got = invoke(
            &host,
            "map",
            HTTP_SERVICE,
            "get-resource",
            json!({"authorization":id,"path":"/maps/a.pbf"}),
        )
        .await
        .unwrap();
        assert_eq!(got["bytes"], 10);
        assert!(
            got["age"].as_u64().is_some(),
            "a kept copy says how old it is: {got}"
        );
        let read = invoke(
            &host,
            "map",
            HTTP_SERVICE,
            "read-resource",
            json!({"resource":got["resource"],"offset":0}),
        )
        .await
        .unwrap();
        assert_eq!(read["hex"], hex::encode(b"kept bytes"));
        // Only what is kept, never the network: an address not kept is refused at once.
        let asked = |path: &str, flag: &str| {
            let host = &host;
            let args = json!({"authorization":id,"path":path, flag:true});
            async move { invoke(host, "map", HTTP_SERVICE, "get-resource", args).await }
        };
        assert!(asked("/maps/a.pbf", "kept_only").await.is_ok());
        assert!(asked("/maps/b.pbf", "kept_only").await.is_err());
        // A refresh goes past the copy to the network, which this reserved address cannot reach.
        assert!(asked("/maps/a.pbf", "refresh").await.is_err());
        let usage = |caller: &'static str, method: &'static str| {
            let host = &host;
            async move {
                invoke(host, caller, HTTP_SERVICE, method, json!({}))
                    .await
                    .unwrap()
            }
        };
        let counted = usage("map", "resource-cache").await;
        assert_eq!(
            (counted["resources"].as_u64(), counted["bytes"].as_u64()),
            (Some(1), Some(10))
        );
        // Another extension keeps nothing of it and cannot remove it.
        assert_eq!(usage("other", "clear-resource-cache").await["resources"], 0);
        assert!(file.exists());
        let cleared = usage("map", "clear-resource-cache").await;
        assert_eq!(
            (cleared["resources"].as_u64(), cleared["bytes"].as_u64()),
            (Some(0), Some(0))
        );
        assert!(!file.exists());
        // Removing the extension forgets its copies with its approval.
        keep();
        FileHttpServices::forget_public_read(scratch.path(), &caller);
        assert!(!file.exists());
    }

    #[tokio::test]
    async fn a_failed_https_request_says_which_way_it_failed_and_nothing_of_the_address() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let scratch = tempfile::tempdir().unwrap();
        let client = service(&scratch).https_client().unwrap();
        // A port nobody listens on: freed at once after being taken.
        let closed = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let error = client
            .get(format!("https://{closed}/"))
            .send()
            .await
            .unwrap_err();
        assert_eq!(
            FileHttpServices::https_failed(&error),
            "HTTPS failed: the address could not be reached; no retry was attempted"
        );
        // Something that answers, but not with TLS: the secure connection is what failed.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            // The client speaks first; it is answered in plain text, and the line stays open until it gives up.
            let mut hello = [0u8; 512];
            let _ = socket.read(&mut hello).await;
            let _ = socket.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
            let _ = socket.read(&mut hello).await;
        });
        let error = client
            .get(format!("https://{address}/"))
            .send()
            .await
            .unwrap_err();
        let said = FileHttpServices::https_failed(&error);
        assert_eq!(
            said,
            "HTTPS failed: the secure connection was refused (its certificate or its TLS); no retry was attempted",
            "{error:?}"
        );
        assert!(!said.contains("127.0.0.1"));
        server.await.unwrap();
    }
    #[tokio::test]
    async fn configuring_a_key_does_not_select_files_or_authorize_uploads() {
        let scratch = tempfile::tempdir().unwrap();
        let host = service(&scratch);
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
        let host = service(&scratch);
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
        let host = service(&scratch);
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
        let host = service(&scratch);
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
        let host = service(&scratch);
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
    fn an_extension_sends_the_headers_it_named_when_it_asked_and_never_the_forbidden_ones() {
        // Named or not, the body's type; nothing else unless it was named.
        assert!(header_allowed("content-type", &[]));
        assert!(!header_allowed("x-example-session", &[]));
        assert!(header_allowed(
            "x-example-session",
            &["x-example-session".to_owned()]
        ));
        // What is named: lower-case header names, each once, at most eight.
        assert_eq!(declared_headers(&json!({})).unwrap(), Vec::<String>::new());
        assert_eq!(
            declared_headers(
                &json!({"headers":["x-example-session","content-type","x-example-session","if-match"]})
            )
            .unwrap(),
            ["x-example-session", "if-match"]
        );
        for refused in [
            json!({"headers":"x-example"}),
            json!({"headers":[7]}),
            json!({"headers":[""]}),
            json!({"headers":["X-Example"]}),
            json!({"headers":["x example"]}),
            json!({"headers":["x-example\r\nhost"]}),
            json!({"headers":["a","b","c","d","e","f","g","h","i"]}),
            // The transport's, the application's, a browser's for the site, and a proxy's.
            json!({"headers":["host"]}),
            json!({"headers":["authorization"]}),
            json!({"headers":["cookie"]}),
            json!({"headers":["content-length"]}),
            json!({"headers":["transfer-encoding"]}),
            json!({"headers":["user-agent"]}),
            json!({"headers":["proxy-authorization"]}),
            json!({"headers":["x-forwarded-host"]}),
            json!({"headers":["x-http-method-override"]}),
            json!({"headers":["sec-fetch-site"]}),
        ] {
            assert!(declared_headers(&refused).is_err(), "{refused} was taken");
        }
    }

    #[tokio::test]
    async fn the_headers_named_at_authorization_are_kept_with_it_and_a_forbidden_one_asks_nobody() {
        let scratch = tempfile::tempdir().unwrap();
        let host = service(&scratch);
        let file = invoke(&host, "first", FILES_SERVICE, "choose", json!({}))
            .await
            .unwrap();
        assert_eq!(
            invoke(
                &host,
                "first",
                HTTP_SERVICE,
                "authorize",
                json!({"file":file["file"],"origin":"https://example.invalid","headers":["cookie"]}),
            )
            .await
            .unwrap_err(),
            "An extension cannot send that request header"
        );
        assert!(host.state().unwrap().authorizations.is_empty());
        let authorization = invoke(
            &host,
            "first",
            HTTP_SERVICE,
            "authorize",
            json!({"file":file["file"],"origin":"https://example.invalid","headers":["x-example-session"]}),
        )
        .await
        .unwrap();
        let handle = authorization["authorization"].as_str().unwrap().to_owned();
        assert_eq!(
            host.state().unwrap().authorizations[&handle].headers,
            ["x-example-session"]
        );
        // One it did not name is refused before anything leaves this device.
        assert_eq!(
            invoke(
                &host,
                "first",
                HTTP_SERVICE,
                "request",
                json!({"authorization":handle,"method":"POST","path":"/upload","headers":{"x-other":"1"}}),
            )
            .await
            .unwrap_err(),
            "Unsupported request header"
        );
    }
    #[tokio::test]
    async fn result_presentation_requires_current_caller_authorization_and_safe_origin() {
        let scratch = tempfile::tempdir().unwrap();
        let host = service(&scratch);
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
    #[test]
    fn the_consent_says_the_file_the_address_and_the_headers_named_in_the_applications_words() {
        let origin = reqwest::Url::parse("https://example.invalid").unwrap();
        let plain = consent("notes.txt", 12, &origin, &[], false, None);
        assert_eq!(
            plain.text,
            "notes.txt (12 bytes) → https://example.invalid/ The file is sent to this address as it is, without end-to-end encryption."
        );
        assert!(plain.waits());
        assert_eq!(
            plain
                .buttons
                .iter()
                .map(|button| (button.id.as_str(), button.label.as_str()))
                .collect::<Vec<_>>(),
            [("allow", "Allow"), ("decline", "Don't allow")]
        );
        let named = consent(
            "notes.txt",
            12,
            &origin,
            &["x-example-session".to_owned(), "if-match".to_owned()],
            false,
            None,
        );
        assert!(named.text.starts_with(&plain.text));
        assert!(named.text.ends_with(
            " It also sends request headers of its own there: x-example-session, if-match."
        ));
        // An anonymous destination says so where the headers are said, and names the address the result will live on: the
        // person sees everything the upload touches before allowing it.
        let links = reqwest::Url::parse("https://links.example").unwrap();
        let anonymous = consent("notes.txt", 12, &origin, &[], true, Some(&links));
        assert_eq!(
            anonymous.text,
            "notes.txt (12 bytes) → https://example.invalid/ The file is sent to this address as it is, without end-to-end encryption. The application will not use a saved key. The link it returns will be at https://links.example/."
        );
        assert_eq!(anonymous.validate(), Ok(()));
        // The most the service lets an extension name, a long name with what a name may hold, and a long address still fit
        // what a notification may be: nothing here can make a consent that cannot be shown.
        let most: Vec<String> = (0..MOST_HEADERS)
            .map(|n| format!("{n}{}", "x".repeat(63)))
            .collect();
        let long_name = format!("{}\n\t{}.bin", "ă".repeat(200), "z".repeat(100));
        let shown = consent(&long_name, u64::MAX, &origin, &[], false, None);
        assert_eq!(shown.validate(), Ok(()));
        assert!(shown.text.starts_with(&"ă".repeat(39)));
        assert!(shown.text.contains("…"));
        assert!(shown.text.contains("zzzz.bin ("));
        // Every header at its longest does not fit beside it: such a request is refused, never shown cut.
        assert!(
            consent("notes.txt", 12, &origin, &most, false, None)
                .validate()
                .is_err()
        );
        assert_eq!(
            consent("notes.txt", 12, &origin, &most[..4], false, None).validate(),
            Ok(())
        );

        assert_eq!(shortened("short", 5), "short");
        assert_eq!(shortened("abcdefghij", 7), "abcd…ij");
        let link = format!("https://example.invalid/f/{}", "k".repeat(2000));
        let shown = result_link(&link);
        assert_eq!(shown.validate(), Ok(()));
        assert_eq!(shown.text.chars().count(), MOST_LINK_SHOWN);
        assert!(shown.text.starts_with("https://example.invalid/f/kk"));
        assert_eq!(shown.buttons[0].label, "Copy link");
    }
    #[tokio::test(start_paused = true)]
    async fn a_consent_is_a_notification_of_the_extension_and_only_allow_authorizes() {
        let scratch = tempfile::tempdir().unwrap();
        let list = Arc::new(Notifications::default());
        let host = FileHttpServices::new(scratch.path().to_owned(), Arc::new(TestUi), list.clone());
        let file = invoke(&host, "first", FILES_SERVICE, "choose", json!({}))
            .await
            .unwrap();
        let args = json!({"file":file["file"],"origin":"https://example.invalid","headers":["x-example-session"]});
        // What the person does with the question, and what the extension is told.
        for (answer, told) in [
            (Some(Some("decline")), Err("Authorization was declined")),
            (Some(None), Err("Authorization was declined")),
            (None, Err("Authorization was cancelled")),
            (Some(Some("allow")), Ok(())),
        ] {
            let asking = tokio::spawn({
                let (host, args) = (host.clone(), args.clone());
                async move { invoke(&host, "first", HTTP_SERVICE, "authorize", args).await }
            });
            let mut changes = list.subscribe();
            while list.snapshot().is_empty() {
                changes.changed().await.unwrap();
            }
            let shown = list.snapshot();
            assert_eq!(shown.len(), 1);
            assert_eq!(
                shown[0].source,
                NotificationSource::Plugin(PluginId::new("first").unwrap())
            );
            assert_eq!(
                shown[0].notification,
                consent(
                    "synthetic.txt",
                    22,
                    &reqwest::Url::parse("https://example.invalid").unwrap(),
                    &["x-example-session".to_owned()],
                    false,
                    None
                )
            );
            match answer {
                Some(button) => list.answer(shown[0].id, button.map(str::to_owned)).unwrap(),
                // Nobody answers: the question is given up after its time, and leaves the screen.
                None => tokio::time::sleep(ANSWER_WITHIN + std::time::Duration::from_secs(1)).await,
            }
            let result = asking.await.unwrap();
            assert!(list.snapshot().is_empty());
            match told {
                Ok(()) => {
                    let handle = result.unwrap()["authorization"]
                        .as_str()
                        .unwrap()
                        .to_owned();
                    assert!(host.state().unwrap().authorizations.contains_key(&handle));
                }
                Err(message) => {
                    assert_eq!(result.unwrap_err(), message);
                    assert!(host.state().unwrap().authorizations.is_empty());
                }
            }
        }
        // The result link: closed without copying is still presented, and it is the extension's own notification too.
        let handle = host
            .state()
            .unwrap()
            .authorizations
            .keys()
            .next()
            .unwrap()
            .clone();
        let presenting = tokio::spawn({
            let host = host.clone();
            async move {
                invoke(
                    &host,
                    "first",
                    HTTP_SERVICE,
                    "present",
                    json!({"authorization":handle,"link":"https://example.invalid/f/result"}),
                )
                .await
            }
        });
        let mut changes = list.subscribe();
        while list.snapshot().is_empty() {
            changes.changed().await.unwrap();
        }
        let shown = list.snapshot();
        assert_eq!(
            shown[0].notification,
            result_link("https://example.invalid/f/result")
        );
        list.answer(shown[0].id, None).unwrap();
        assert_eq!(
            presenting.await.unwrap().unwrap(),
            json!({"presented":true})
        );
        // Stopping the extension takes its question off the screen and ends the call.
        let asking = tokio::spawn({
            let (host, args) = (host.clone(), args.clone());
            async move { invoke(&host, "first", HTTP_SERVICE, "authorize", args).await }
        });
        while list.snapshot().is_empty() {
            changes.changed().await.unwrap();
        }
        list.revoke(&PluginId::new("first").unwrap());
        assert_eq!(
            asking.await.unwrap().unwrap_err(),
            "Authorization was cancelled"
        );
    }
    #[tokio::test]
    async fn inline_result_presentation_is_refused_before_authorization_or_network_access() {
        let scratch = tempfile::tempdir().unwrap();
        let host = service(&scratch);
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
                Some(b"synthetic-credential"),
                &secrets
            )
            .is_err()
        );
        assert!(
            reject_secret_echo(
                &json!({"synthetic-credential":"echo"}),
                Some(b"synthetic-credential"),
                &secrets
            )
            .is_err()
        );
        assert!(
            reject_secret_echo(
                &json!({"token":{"secret":"session"}}),
                Some(b"synthetic-credential"),
                &secrets
            )
            .is_ok()
        );
        // With no key there is nothing of one to find: a destination's own text is not refused for the empty string that a
        // missing key would otherwise match.
        let nothing = HashMap::new();
        assert_eq!(
            reject_secret_echo(&json!("https://links.example/fixture"), None, &nothing),
            Ok(())
        );
    }
}

#[cfg(test)]
mod transport_tests;

#[cfg(all(test, unix))]
pub(crate) use transport_tests::upload_wasm_pipeline::{PausedUpload, artifact as upload_artifact};
