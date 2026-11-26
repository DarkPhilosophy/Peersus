//! Sandboxed provider protocol: the host owns all file bytes and credentials.
#![cfg(any(target_family = "wasm", feature = "native", test))]

mod host;
#[cfg(all(feature = "native", not(target_family = "wasm")))]
mod native;

use host::{Blocked, Readiness, Wire};
use serde_json::{Value, json};
use std::collections::VecDeque;
const PART: u64 = 3 * 1024 * 1024;
const MAX_FILE: u64 = 250 * 1024 * 1024;
const KEYLESS_MAX_FILE: u64 = 25 * 1024 * 1024;
/// The destination that uses an API key when saved, otherwise its native anonymous upload.
const ORIGIN: &str = "https://poo.wang";
/// The request header the provider's upload sessions are told by. Named to the application when asking to send.
const SESSION_HEADER: &str = "x-upload-session";
/// The provider's cookieless opt-in. This is only sent when admitting a keyless upload.
const NATIVE_HEADER: &str = "x-poo-upload-client";
const RETENTION: &str = "Burn after 24 hours";
/// A destination that needs no account and no key at all: one file, one request, a public link.
const CATBOX: &str = "https://catbox.moe";
/// Where that destination's links live. Named to the application with the consent and never requested: a request of this
/// extension goes to [`CATBOX`] only.
const CATBOX_LINKS: &str = "https://files.catbox.moe";
/// The largest file that destination takes from here: one multipart request, and a part is the most the application reads for
/// one. Anything larger would have to be split, which this destination does not accept, so it is refused before it is sent.
const CATBOX_MAX: u64 = 6 * 1024 * 1024;
/// The application's own words when the selection is larger than the limit it was asked for. This extension turns them into a
/// sentence that names the destination and its limit, since that is what the person chose.
const SIZE_REFUSED: &str = "Choose a regular file within the upload size limit";
const NEED_FILE: &str = "Choose one file on Send before uploading";
const POO_FILE_LIMIT: &str = "Choose a nonempty regular file of up to 250 MiB for poo.wang";
const POO_KEYLESS_FILE_LIMIT: &str =
    "Choose a nonempty regular file of up to 25 MiB for anonymous poo.wang upload";
const CATBOX_FILE_LIMIT: &str = "Choose a nonempty regular file of up to 6 MiB for Catbox";
/// What this extension's settings say about the destination that needs no key, so that both destinations are visible where the
/// key of the first one is.
const CATBOX_WORDS: &str = "Catbox needs no key and no account: 'Upload selected file (Catbox, anonymous)' sends one file of up to 6 MiB to catbox.moe and shows the public link it returns on files.catbox.moe.";
/// How many calls are kept while the extension asks its first question; any past these are refused as busy.
const EARLY: usize = 4;
const POO_ACTIONS: &[&str] = &["upload", "upload_keyless"];
const FILES: &str = "peersus.files";
const HTTP: &str = "peersus.http";
const SELECT: &str = "files.select";
const NETWORK: &str = "network.http";

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Destination {
    #[default]
    Both,
    Catbox,
    PooWang,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PooAuth {
    Keyed,
    Keyless,
}

#[derive(Debug, PartialEq, Eq)]
enum ActionError {
    Cancelled,
    Denied,
    Blocked(Blocked),
    Unavailable(&'static str),
    TooLarge(&'static str),
    RateLimited,
    Refused,
    CredentialRejected,
    NotShown,
    Failed(&'static str),
}
impl ActionError {
    fn outcome(&self, fallback: &str) -> Value {
        let (code, message) = match self {
            Self::Cancelled => ("cancelled", "Upload cancelled"),
            Self::Denied => (
                "denied",
                "Required platform permission has not been granted",
            ),
            Self::Blocked(Blocked::MissingPermission(permission)) => {
                return Blocked::MissingPermission(permission.clone()).value();
            }
            Self::Blocked(Blocked::NotAccepted) => return Blocked::NotAccepted.value(),
            Self::Unavailable(message) => ("unavailable", *message),
            Self::TooLarge(message) => ("too-large", *message),
            Self::RateLimited => (
                "rate-limited",
                "The server rate-limited the upload; no retry was attempted",
            ),
            Self::Refused => (
                "refused",
                "The server rejected the upload; no retry was attempted",
            ),
            Self::CredentialRejected => (
                "credential-rejected",
                "The provider did not accept this API key",
            ),
            Self::NotShown => (
                "not-shown",
                "The file was uploaded, but the link could not be shown",
            ),
            Self::Failed(message) => (fallback, *message),
        };
        json!({"status":"error","code":code,"message":message})
    }
}
impl From<&'static str> for ActionError {
    fn from(message: &'static str) -> Self {
        Self::Failed(message)
    }
}

struct Job<'a, W> {
    destination: Destination,
    wire: &'a mut W,
    ticket: u64,
    next: u64,
    file: Option<String>,
    authorization: Option<Value>,
    cancelled: bool,
    starting: bool,
    readiness: Readiness,
    refresh_pending: bool,
    /// Bounded startup calls, retained until dispatched or withdrawn, even during another queued call.
    early: VecDeque<Value>,
    /// The settings line as it was known when this call began (nothing while the extension starts). A call that only asks for
    /// that line while this one waits, for a person or for the provider, is answered with it at once.
    shown: Value,
    /// The application's own words for its last refusal, for the settings line when the provider could not be asked.
    refused: Option<String>,
    /// The last complete offer set the owner accepted; recreated from the manifest on startup.
    withheld: &'static [&'static str],
}
impl<'a, W: Wire> Job<'a, W> {
    fn new(wire: &'a mut W, ticket: u64, next: u64) -> Self {
        Job {
            destination: Destination::Both,
            wire,
            ticket,
            next,
            file: None,
            authorization: None,
            cancelled: false,
            starting: false,
            readiness: Readiness::default(),
            refresh_pending: false,
            early: VecDeque::new(),
            shown: Value::Null,
            refused: None,
            withheld: POO_ACTIONS,
        }
    }
    /// The questions asked before any call is served. A press made in that moment is not lost and not called busy.
    fn starting(wire: &'a mut W) -> Self {
        Job {
            starting: true,
            ..Job::new(wire, 0, 0)
        }
    }
}
impl<W: Wire> Job<'_, W> {
    /// A call that may wait for a person (a consent, the key's form, a large request): the longest deadline the application's
    /// file and HTTPS services allow.
    fn call(&mut self, service: &str, method: &str, args: Value) -> Result<Value, ActionError> {
        self.ask(service, method, args, 600_000)
    }
    /// A call with its own deadline. A method the application answers at once has no raised limit, and a deadline past the
    /// general one (30 s) is refused before the call is made.
    fn ask(
        &mut self,
        service: &str,
        method: &str,
        args: Value,
        timeout_ms: u32,
    ) -> Result<Value, ActionError> {
        let permission = match (service, method) {
            (FILES, "selected" | "release") => Some(SELECT),
            (
                HTTP,
                "configure" | "saved" | "authorize" | "request" | "check" | "present" | "release",
            ) => Some(NETWORK),
            _ => None,
        };
        if let Some(missing) =
            permission.and_then(|name| self.readiness.missing(None, "host", name))
        {
            return Err(ActionError::Blocked(Blocked::MissingPermission(missing)));
        }
        self.next = self
            .next
            .checked_add(1)
            .ok_or("Call identifiers exhausted")?;
        let id = self.next;
        self.wire.send(json!({"op":"call","id":id,"provider":"host","service":service,"version":1,"method":method,"args":args,"timeout_ms":timeout_ms}))?;
        loop {
            let message = self.wire.receive()?;
            match message["type"].as_str() {
                Some("result") if message["id"].as_u64() == Some(id) => {
                    if let Some(blocked) = Blocked::read(&message["result"]) {
                        self.refused = None;
                        if let Blocked::MissingPermission(missing) = &blocked
                            && permission.is_some_and(|name| missing.is("host", name))
                        {
                            self.readiness.deny(None, missing.clone());
                        }
                        return Err(ActionError::Blocked(blocked));
                    }
                    if message["result"]["status"] != "ok" {
                        self.refused = message["result"]["message"].as_str().map(|said| {
                            said.chars().filter(|c| !c.is_control()).take(160).collect()
                        });
                        return Err(match message["result"]["code"].as_str() {
                            Some("cancelled") => ActionError::Cancelled,
                            Some("denied") => ActionError::Denied,
                            Some("unavailable") => {
                                ActionError::Unavailable("The platform operation is unavailable")
                            }
                            _ => ActionError::Failed("The platform refused the operation"),
                        });
                    }
                    if service == host::CONFIG && method == "get" {
                        let _ = self.readiness.update(&message["result"]["value"]);
                    }
                    return Ok(message["result"]["value"].clone());
                }
                Some("cancelled") => {
                    let Some(ticket) = message["ticket"].as_u64() else {
                        continue;
                    };
                    self.early
                        .retain(|queued| queued["ticket"].as_u64() != Some(ticket));
                    if ticket != self.ticket {
                        continue;
                    }
                    self.cancelled = true;
                    let _ = self.wire.send(json!({"op":"cancel","id":id}));
                    return Err(ActionError::Cancelled);
                }
                Some("call") => {
                    let redraw = !self.shown.is_null()
                        && message["service"] == "upload.file"
                        && message["method"] == "status"
                        && message["args"]["event"]["t"] == "tick";
                    if redraw {
                        self.wire.send(json!({"op":"reply","ticket":message["ticket"],"result":{"status":"ok","value":self.shown}}))?;
                    } else if self.starting && self.early.len() < EARLY {
                        self.early.push_back(message);
                    } else {
                        self.wire.send(json!({"op":"reply","ticket":message["ticket"],"result":{"status":"error","code":"busy","message":"This extension is busy with another action"}}))?;
                    }
                }
                Some("event") if host::config_changed(&message) => self.refresh_pending = true,
                _ => {}
            }
        }
    }
    /// One accepted request under the current authorization, whole: a destination that answers with text is read for text.
    fn accepted(&mut self, mut args: Value) -> Result<Value, ActionError> {
        args["authorization"] = self.authorization.clone().ok_or("Missing authorization")?;
        let too_large = if args.get("multipart").is_some() {
            "The server rejected the file as too large; no retry was attempted"
        } else if args.get("part").is_some() {
            "The server rejected a file part as too large; no retry was attempted"
        } else if args["json"].get("receipts").is_some() {
            "The server rejected the upload completion request as too large; no retry was attempted"
        } else {
            "The server rejected the upload session request as too large; no retry was attempted"
        };
        let response = self.call("peersus.http", "request", args)?;
        match response["status"].as_u64() {
            Some(200..=299) => Ok(response),
            Some(413) => Err(ActionError::TooLarge(too_large)),
            Some(429) => Err(ActionError::RateLimited),
            Some(100..=599) => Err(ActionError::Refused),
            _ => Err(ActionError::Failed("Invalid HTTPS response status")),
        }
    }
    fn http(&mut self, args: Value) -> Result<Value, ActionError> {
        let response = self.accepted(args)?;
        Ok(response["json"].clone())
    }
    /// Whether the key the person just saved is accepted by the provider. Only the status decides: a success is `valid`, the two answers
    /// that mean "not this key" are `rejected`, and anything else (a busy or failing provider, a redirect, no network) is `unverified`:
    /// a key is never called good without a real success, and never lost for a failure that says nothing about it.
    fn check_key(&mut self) -> Result<&'static str, ActionError> {
        self.refused = None;
        let answer = match self.call(
            "peersus.http",
            "check",
            json!({"origin":ORIGIN,"path":"/api/me"}),
        ) {
            Ok(answer) => answer,
            Err(error @ (ActionError::Cancelled | ActionError::Blocked(_))) => return Err(error),
            Err(_) => return Ok("unverified"),
        };
        Ok(match answer["status"].as_u64() {
            Some(200..=299) => "valid",
            Some(401 | 403) => "rejected",
            status => {
                self.refused = status.map(|status| format!("it answered with status {status}"));
                "unverified"
            }
        })
    }
    /// Confirm key presence without asking the provider. An error is not evidence of absence.
    fn refresh(&mut self, key: &mut Key) -> Result<(), ActionError> {
        let saved = match self.call("peersus.http", "saved", json!({"origin":ORIGIN})) {
            Ok(answer) => answer["saved"].as_bool(),
            Err(error) if self.cancelled => return Err(error),
            Err(ActionError::Blocked(blocked)) => {
                key.saved = None;
                key.forget();
                key.remember_block(&blocked);
                return Err(ActionError::Blocked(blocked));
            }
            Err(_) => None,
        };
        if saved != key.saved || saved.is_none() {
            key.forget();
        }
        key.saved = saved;
        Ok(())
    }

    fn sync_offers(&mut self, key: &Key) -> Result<(), ActionError> {
        let withheld: &[&str] = match (self.destination, key.saved) {
            (Destination::Catbox, _) => &["upload", "upload_keyless", "configure"],
            (Destination::Both, Some(true)) => &["upload_keyless"],
            (Destination::Both, Some(false)) => &["upload"],
            (Destination::Both, None) => POO_ACTIONS,
            (Destination::PooWang, Some(true)) => &["upload_keyless", "upload_anonymous"],
            (Destination::PooWang, Some(false)) => &["upload", "upload_anonymous"],
            (Destination::PooWang, None) => &["upload", "upload_keyless", "upload_anonymous"],
        };
        if self.withheld != withheld {
            self.ask(
                "peersus.actions",
                "withhold",
                json!({"actions":withheld}),
                5_000,
            )?;
            self.withheld = withheld;
        }
        Ok(())
    }

    fn refresh_offers(&mut self, key: &mut Key) -> Result<(), ActionError> {
        let refreshed = self.refresh(key);
        if matches!(refreshed, Ok(()) | Err(ActionError::Blocked(_))) {
            self.sync_offers(key)?;
        }
        refreshed
    }

    fn read_destination(&mut self) -> Result<Destination, ActionError> {
        let config = self.ask(host::CONFIG, "get", json!({}), 5_000)?;
        match config["settings"]["destination"].as_str() {
            Some("Choose on Send") => Ok(Destination::Both),
            Some("Catbox") => Ok(Destination::Catbox),
            Some("poo.wang") => Ok(Destination::PooWang),
            _ => Err(ActionError::Unavailable(
                "The upload destination is unavailable",
            )),
        }
    }

    fn initialize(&mut self, key: &mut Key) -> Result<Destination, ActionError> {
        // The host supplies the effective destination, including its default.
        self.destination = self.read_destination()?;
        if self.destination != Destination::Catbox {
            match self.refresh(key) {
                Ok(()) | Err(ActionError::Blocked(_)) => {}
                Err(error) => return Err(error),
            }
        }
        self.sync_offers(key)?;
        Ok(self.destination)
    }

    fn settings_page(&self, key: &Key) -> Value {
        if self.destination == Destination::Catbox {
            json!({"k":"text","text":CATBOX_WORDS,"tone":"body"})
        } else {
            key.page()
        }
    }
    /// Ask the provider only about a key whose presence the application confirmed.
    fn verify(&mut self, key: &mut Key) -> Result<(), ActionError> {
        key.forget();
        if key.saved == Some(true) {
            let verdict = match self.check_key() {
                Err(ActionError::Blocked(blocked)) => {
                    key.remember_block(&blocked);
                    return Err(ActionError::Blocked(blocked));
                }
                answer => answer?,
            };
            key.verdict = Some(verdict);
            // Why it could not be asked is the application's sentence or the provider's status, never anything of the key.
            key.why = self.refused.take().filter(|_| verdict == "unverified");
        }
        Ok(())
    }
    /// Save or remove the key through the application, then say what the provider thinks of what is saved now.
    fn configure(&mut self, key: &mut Key) -> Value {
        let value = match self.call("peersus.http", "configure", json!({"origin":ORIGIN})) {
            Err(ActionError::Blocked(blocked)) => {
                key.forget();
                key.remember_block(&blocked);
                return ActionError::Blocked(blocked).outcome("configuration-failed");
            }
            Ok(value) => value,
            Err(error) => return error.outcome("configuration-failed"),
        };
        // The host may have saved or removed the key. Never carry the previous verdict over,
        // and never undo or recheck a successful save after the action is withdrawn.
        key.saved = None;
        key.forget();
        if value["configured"].as_bool() != Some(true) {
            let _ = self.sync_offers(key);
            return ActionError::Failed("Invalid credential configuration reply")
                .outcome("configuration-failed");
        }
        match self
            .sync_offers(key)
            .and_then(|()| self.refresh_offers(key))
            .and_then(|()| self.verify(key))
        {
            Ok(()) => configured(match key.saved {
                Some(true) => key.verdict.unwrap_or("unverified"),
                Some(false) => "none",
                None => "unknown",
            }),
            Err(error) => error.outcome("configuration-failed"),
        }
    }
    /// Active settings requests refresh offers. Only Check now asks the provider;
    /// the page's clock draws the last known state and does not ask either service.
    fn status(&mut self, key: &mut Key, event: &Value) -> Value {
        if self.destination == Destination::Catbox {
            return json!({"status":"ok","value":self.settings_page(key)});
        }
        let updated = match (event["t"].as_str(), event["id"].as_str()) {
            (Some("open"), _) | (Some("press"), Some("refresh")) => self.refresh_offers(key),
            (Some("press"), Some("check")) => {
                self.refresh_offers(key).and_then(|()| self.verify(key))
            }
            _ => Ok(()),
        };
        match updated {
            Err(ActionError::Blocked(_)) if key.why.is_some() => {}
            Err(error) => return error.outcome("configuration-failed"),
            Ok(()) => {}
        }
        json!({"status":"ok","value":self.settings_page(key)})
    }
    /// The chosen action fixes the mode. Never silently substitute another mode if
    /// a previously shown offer was stale by the time the person pressed it.
    fn upload(
        &mut self,
        key: &mut Key,
        context: Option<u64>,
        mode: PooAuth,
    ) -> Result<String, ActionError> {
        let context = context
            .filter(|&context| context != 0)
            .ok_or(ActionError::Unavailable(NEED_FILE))?;
        match self.refresh(key) {
            Err(error @ ActionError::Blocked(_)) => {
                let _ = self.sync_offers(key);
                return Err(error);
            }
            refreshed => refreshed?,
        }
        let expected = mode == PooAuth::Keyed;
        if key.saved != Some(expected) {
            // Refresh a stale row if we can, but the action still did not establish its
            // requested mode. A failed offer update must never allow the upload to proceed.
            let _ = self.sync_offers(key);
            return Err(ActionError::Unavailable(if key.saved.is_none() {
                "The application could not establish whether a poo.wang API key is saved; no upload was attempted"
            } else {
                "The saved API key state has changed; choose the upload destination again"
            }));
        }
        if mode == PooAuth::Keyed {
            self.verify(key)?;
            if key.verdict == Some("rejected") {
                return Err(ActionError::CredentialRejected);
            }
        }
        self.run(context, mode)
    }
    fn select_file(
        &mut self,
        context: u64,
        max_bytes: u64,
        limit_message: &'static str,
    ) -> Result<(Value, u64), ActionError> {
        // The host can reject before making a snapshot, and never authorizes or sends a truncated file.
        self.refused = None;
        let selected = self
            .call(
                "peersus.files",
                "selected",
                json!({"context":context,"max_bytes":max_bytes}),
            )
            .map_err(|error| match error {
                ActionError::Failed(_) if self.refused.as_deref() == Some(SIZE_REFUSED) => {
                    ActionError::Unavailable(limit_message)
                }
                _ => error,
            })?;
        let file = selected["file"]
            .as_str()
            .filter(|file| !file.is_empty())
            .ok_or("Invalid selected file handle")?;
        self.file = Some(file.to_owned());
        let size = selected["bytes"].as_u64().ok_or("Invalid selected size")?;
        if size > max_bytes {
            return Err(ActionError::TooLarge(limit_message));
        }
        if size == 0 {
            return Err(ActionError::Unavailable(limit_message));
        }
        Ok((selected, size))
    }

    fn run(&mut self, context: u64, mode: PooAuth) -> Result<String, ActionError> {
        let (max_bytes, limit_message) = match mode {
            PooAuth::Keyed => (MAX_FILE, POO_FILE_LIMIT),
            PooAuth::Keyless => (KEYLESS_MAX_FILE, POO_KEYLESS_FILE_LIMIT),
        };
        let (selected, size) = self.select_file(context, max_bytes, limit_message)?;
        let name = selected["name"].as_str().ok_or("Invalid selected name")?;
        // Declare every custom header at consent. An anonymous grant never loads a saved credential.
        let mut grant = json!({"file":self.file,"origin":ORIGIN,"headers":[SESSION_HEADER]});
        if mode == PooAuth::Keyless {
            grant["anonymous"] = json!(true);
            grant["headers"] = json!([SESSION_HEADER, NATIVE_HEADER]);
        }
        let authorization = self.call("peersus.http", "authorize", grant)?;
        self.authorization = Some(authorization["authorization"].clone());

        let mut start = if size <= PART {
            json!({"method":"POST","path":"/api/upload","multipart":{"field":"files","offset":0,"length":size,"fields":{"burnMode":RETENTION,"showcase":"false"}}})
        } else {
            json!({"method":"POST","path":"/api/upload/session","json":{"name":name,"type":"","size":size,"burnMode":RETENTION,"showcase":false},"capture_secrets":["/token"]})
        };
        match mode {
            PooAuth::Keyed => start["bearer"] = json!(true),
            PooAuth::Keyless => start["headers"] = json!({NATIVE_HEADER:"native"}),
        }
        let admission = self.http(start)?;
        let result = if size <= PART {
            admission
        } else {
            let session = admission;
            let part_size = session["partSize"]
                .as_u64()
                .filter(|size| (PART..=6 * 1024 * 1024).contains(size))
                .ok_or("Invalid server part size")?;
            // The validated file and part sizes bound this to at most ceil(250 / 3)
            // parts; do not impose a second, smaller limit on valid files.
            let parts = session["parts"]
                .as_u64()
                .filter(|parts| size.div_ceil(part_size) == *parts)
                .ok_or("Invalid server part count")?;
            let token = session["token"].clone();
            if token["secret"].as_str().is_none() {
                return Err(ActionError::Failed("Missing opaque session authorization"));
            }
            let mut receipts = Vec::new();
            for index in 0..parts {
                let offset = index * part_size;
                let length = (size - offset).min(part_size);
                let receipt = self.http(json!({"method":"PUT","path":format!("/api/upload/session/part?index={index}"),"headers":{"content-type":"application/octet-stream",SESSION_HEADER:token},"part":{"offset":offset,"length":length},"capture_secrets":["/receipt"]}))?;
                if receipt["success"] != true
                    || receipt["index"].as_u64() != Some(index)
                    // Older servers returned only the receipt. If the server reports a byte
                    // count, it must acknowledge this part, including a shorter final part.
                    || receipt.get("acceptedBytes").is_some_and(|bytes| bytes.as_u64() != Some(length))
                    || receipt["receipt"]["secret"].as_str().is_none()
                {
                    return Err(ActionError::Failed("Invalid server part receipt"));
                }
                receipts.push(receipt["receipt"].clone());
            }
            self.http(json!({"method":"POST","path":"/api/upload/session/complete","json":{"token":token,"receipts":receipts}}))?
        };
        if mode == PooAuth::Keyless && result["anonymous"] != true {
            return Err(ActionError::Failed("Invalid anonymous upload result"));
        }
        self.present(result_link(&result)?)
    }
    fn present(&mut self, link: String) -> Result<String, ActionError> {
        match self.call(
            "peersus.http",
            "present",
            json!({"authorization":self.authorization,"link":link}),
        ) {
            Ok(reply) if reply["presented"] == true => Ok(link),
            Err(ActionError::Cancelled) if self.cancelled => Err(ActionError::Cancelled),
            // The provider already accepted the file. Nothing here should cause another upload.
            _ => Err(ActionError::NotShown),
        }
    }
    fn release(&mut self) {
        if let Some(authorization) = self.authorization.take() {
            let _ = self.call(
                "peersus.http",
                "release",
                json!({"authorization":authorization}),
            );
        }
        if let Some(file) = self.file.take() {
            let _ = self.call("peersus.files", "release", json!({"file":file}));
        }
    }
    /// A Catbox upload has no credential at any stage, even when a poo.wang key is saved.
    fn upload_anonymous(&mut self, context: Option<u64>) -> Result<String, ActionError> {
        let context = context
            .filter(|&context| context != 0)
            .ok_or(ActionError::Unavailable(NEED_FILE))?;
        self.run_anonymous(context)
    }
    fn run_anonymous(&mut self, context: u64) -> Result<String, ActionError> {
        let (_, size) = self.select_file(context, CATBOX_MAX, CATBOX_FILE_LIMIT)?;
        // Anonymous: the application is asked to allow this file to this address with no key, and to allow a result link on
        // the address the destination puts it. It reads no saved key for either, and this extension never sees one.
        let authorization = self.call(
            "peersus.http",
            "authorize",
            json!({"file":self.file,"origin":CATBOX,"headers":[],"anonymous":true,"result_origin":CATBOX_LINKS}),
        )?;
        self.authorization = Some(authorization["authorization"].clone());
        let answer = self.accepted(json!({
            "method":"POST",
            "path":"/user/api.php",
            "response":"text",
            "multipart":{"field":"fileToUpload","offset":0,"length":size,"fields":{"reqtype":"fileupload"}}
        }))?;
        self.present(catbox_link(&answer)?)
    }

    fn action(&mut self, key: &mut Key, method: Option<&str>, args: &Value) -> Value {
        match method {
            Some("configure" | "choose" | "choose_keyless")
                if self.destination == Destination::Catbox =>
            {
                ActionError::Unavailable(
                    "Choose poo.wang in Upload destination before using this action",
                )
                .outcome("upload-failed")
            }
            Some("choose_anonymous") if self.destination == Destination::PooWang => {
                ActionError::Unavailable(
                    "Choose Catbox in Upload destination before using this action",
                )
                .outcome("upload-failed")
            }
            Some("configure") => self.configure(key),
            Some("status") => self.status(key, &args["event"]),
            Some(method @ ("choose" | "choose_keyless" | "choose_anonymous")) => {
                let context = args["context"].as_u64();
                let result = match method {
                    "choose" => self.upload(key, context, PooAuth::Keyed),
                    "choose_keyless" => self.upload(key, context, PooAuth::Keyless),
                    _ => self.upload_anonymous(context),
                };
                self.release();
                match result {
                    Ok(url) => json!({"status":"ok","value":{"status":"uploaded","url":url}}),
                    Err(error) => error.outcome("upload-failed"),
                }
            }
            _ => {
                json!({"status":"error","code":"invalid-call","message":"Unsupported upload action"})
            }
        }
    }
}
/// The link the anonymous destination answered with, which it writes as plain text: its own link origin, then one segment of
/// its own identifier characters and nothing else. Anything else is no link this extension will show: another origin, another
/// path, a query or a fragment, a second line, something that is not text.
fn catbox_link(answer: &Value) -> Result<String, &'static str> {
    let text = answer["text"]
        .as_str()
        .ok_or("Invalid upload result")?
        .trim_end_matches(['\r', '\n']);
    let name = text
        .strip_prefix(CATBOX_LINKS)
        .and_then(|rest| rest.strip_prefix('/'))
        .ok_or("Unsafe upload result link")?;
    if name.is_empty()
        || name.len() > 128
        || name == "."
        || name == ".."
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err("Unsafe upload result link");
    }
    Ok(format!("{CATBOX_LINKS}/{name}"))
}
fn result_link(result: &Value) -> Result<String, &'static str> {
    let files = result["files"]
        .as_array()
        .filter(|files| files.len() == 1)
        .ok_or("Invalid upload result")?;
    let id = files[0]["id"]
        .as_str()
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 128
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
        .ok_or("Invalid uploaded identifier")?;
    let expected = format!("{ORIGIN}/f/{id}");
    if result["success"] != true || files[0]["url"] != expected {
        return Err("Unsafe upload result link");
    }
    Ok(expected)
}
/// The entry the interpreter calls.
#[cfg(target_family = "wasm")]
#[unsafe(no_mangle)]
pub extern "C" fn peersus_main() -> i32 {
    run()
}

/// The entry the native host calls, with its table of the same functions the interpreter offers.
///
/// # Safety
/// `host` is the native host's table, valid for the whole call.
#[cfg(all(feature = "native", not(target_family = "wasm")))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn peersus_plugin_main(host: *const native::HostV1) -> i32 {
    if !native::enter(host) {
        return 1;
    }
    let result = run();
    native::leave();
    result
}

#[cfg(any(target_family = "wasm", feature = "native"))]
fn run() -> i32 {
    serve(&mut host::Host)
}

fn serve<W: Wire>(wire: &mut W) -> i32 {
    if wire.send(host::subscribe_config()).is_err() {
        return 1;
    }
    let mut key = Key::default();
    let (mut initialized, mut next, mut early, mut withheld, mut readiness, mut refresh_pending) = {
        let mut job = Job::starting(wire);
        let initialized = job.initialize(&mut key);
        (
            initialized,
            job.next,
            job.early,
            job.withheld,
            job.readiness,
            job.refresh_pending,
        )
    };
    loop {
        if refresh_pending {
            // A signal received during another operation updates readiness afterward, not its outcome.
            let mut job = Job::new(wire, 0, next);
            job.starting = true;
            job.early = std::mem::take(&mut early);
            job.readiness = std::mem::take(&mut readiness);
            let _ = job.ask(host::CONFIG, "get", json!({}), 5_000);
            (next, early, readiness, refresh_pending) =
                (job.next, job.early, job.readiness, job.refresh_pending);
        }
        let message = match early.pop_front() {
            Some(message) => message,
            None => match wire.receive() {
                Ok(message) => message,
                Err(_) => return 0,
            },
        };
        if host::config_changed(&message) {
            refresh_pending = true;
            continue;
        }
        if message["type"] != "call" {
            continue;
        }
        let Some(ticket) = message["ticket"].as_u64() else {
            return 1;
        };
        let method = message["method"]
            .as_str()
            .filter(|_| message["service"] == "upload.file");
        let mut job = Job::new(wire, ticket, next);
        job.early = std::mem::take(&mut early);
        job.withheld = withheld;
        job.readiness = std::mem::take(&mut readiness);
        job.refresh_pending = refresh_pending;
        let args = &message["args"];
        // Retry only for a new action or an active settings request, never for a passive redraw.
        if initialized.is_err()
            && matches!(
                method,
                Some("choose" | "choose_keyless" | "choose_anonymous" | "configure" | "status")
            )
            && !(method == Some("status") && args["event"]["t"] == "tick")
        {
            initialized = job.initialize(&mut key);
        }
        let result = match &initialized {
            Ok(destination) => {
                job.destination = *destination;
                job.shown = job.settings_page(&key);
                job.action(&mut key, method, args)
            }
            Err(error) => error.outcome("configuration-failed"),
        };
        next = job.next;
        early = job.early;
        withheld = job.withheld;
        readiness = job.readiness;
        refresh_pending = job.refresh_pending;
        // A call that was withdrawn is not answered: the application no longer waits for it.
        if !job.cancelled
            && job
                .wire
                .send(json!({"op":"reply","ticket":ticket,"result":result}))
                .is_err()
        {
            return 1;
        }
    }
}

/// The answer to saving a key, from the host's answer and the provider's verdict on it. A key the provider refuses is a failed action, so
/// that the person is told at once and not left believing it works; a provider that could not say is not a failure of the key. Nothing here
/// is the key or anything the provider sent: only the verdict, which is one of three fixed words.
fn configured(verdict: &'static str) -> Value {
    if verdict == "rejected" {
        return ActionError::CredentialRejected.outcome("configuration-failed");
    }
    json!({"status":"ok","value":{"configured":true,"key":verdict}})
}

/// What is known of the key: whether the application holds a usable one (`None`: it would not say), and what the provider last
/// said of it (`None`: not asked since it was saved). Never the key itself, which this extension is not given.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Key {
    saved: Option<bool>,
    verdict: Option<&'static str>,
    /// Why the provider could not be asked, when that is known: the application's own sentence, or the status it answered with.
    why: Option<String>,
}
impl Key {
    /// What the provider said is of the key it was asked about: it goes when the key may be another.
    fn forget(&mut self) {
        self.verdict = None;
        self.why = None;
    }
    fn remember_block(&mut self, blocked: &Blocked) {
        self.why = Some(match blocked {
            Blocked::MissingPermission(permission) => {
                format!(
                    "poo.wang key: missing permission {} on this device.",
                    permission.name
                )
            }
            Blocked::NotAccepted => "poo.wang key: not accepted.".to_owned(),
        });
    }
    fn words(&self) -> String {
        if self.verdict.is_none()
            && let Some(blocked) = &self.why
        {
            return blocked.clone();
        }
        match (self.saved, self.verdict) {
            (Some(false), _) => {
                "No usable API key is saved. poo.wang uploads use its anonymous limits."
            }
            (None, _) => "The application could not check whether a poo.wang API key is saved.",
            (Some(true), Some("valid")) => "An API key is saved and the provider accepts it.",
            (Some(true), Some("rejected")) => {
                "An API key is saved, but the provider does not accept it."
            }
            (Some(true), Some(_)) => {
                return match &self.why {
                    Some(why) => format!(
                        "An API key is saved. The provider could not be asked about it: {why}"
                    ),
                    None => {
                        "An API key is saved. The provider could not be asked about it.".to_owned()
                    }
                };
            }
            (Some(true), None) => {
                "An API key is saved. The provider has not been asked about it yet."
            }
        }
        .to_owned()
    }
    /// The line in this extension's settings, and "Check now" while there is a key to ask about. It asks to be drawn again by
    /// itself, so that what a save or an upload found out is shown without the page being opened again; drawing asks nobody.
    /// The other destination is named here too, since it needs none of what this page is about.
    fn page(&self) -> Value {
        let mut children = vec![json!({"k":"text","text":self.words(),"tone":"muted"})];
        if self.saved == Some(true) {
            children.push(json!({"k":"button","id":"check","label":"Check now"}));
        } else {
            children.push(json!({"k":"button","id":"refresh","label":"Refresh key state"}));
        }
        children.push(json!({"k":"text","text":CATBOX_WORDS,"tone":"muted"}));
        json!({"k":"interval","id":"key","milliseconds":2000,"child":{"k":"column","children":children}})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Script {
        replies: VecDeque<Value>,
        sent: Vec<Value>,
    }
    impl Wire for Script {
        fn send(&mut self, value: Value) -> Result<(), &'static str> {
            self.sent.push(value);
            Ok(())
        }
        fn receive(&mut self) -> Result<Value, &'static str> {
            let value = self
                .replies
                .pop_front()
                .ok_or("Unexpected extra operation")?;
            let id = self.sent.last().unwrap()["id"].clone();
            Ok(json!({"type":"result","id":id,"result":{"status":"ok","value":value}}))
        }
    }
    fn uploaded() -> Value {
        json!({"success":true,"files":[{"id":"example","url":format!("{ORIGIN}/f/example")}]})
    }
    fn job(wire: &mut Script) -> Job<'_, Script> {
        Job::new(wire, 1, 0)
    }
    #[test]
    fn small_upload_uses_host_snapshot_and_releases_both_authorities() {
        let mut wire = Script {
            replies: VecDeque::from([
                json!({"file":"chosen","name":"small.txt","bytes":7}),
                json!({"authorization":"approved"}),
                json!({"status":200,"json":uploaded()}),
                json!({"presented":true}),
                json!({"released":true}),
                json!({"released":true}),
            ]),
            sent: Vec::new(),
        };
        let mut job = job(&mut wire);
        assert_eq!(
            job.run(42, PooAuth::Keyed).unwrap(),
            format!("{ORIGIN}/f/example")
        );
        job.release();
        assert_eq!(
            wire.sent[2]["args"]["multipart"],
            json!({"field":"files","offset":0,"length":7,"fields":{"burnMode":RETENTION,"showcase":"false"}})
        );
        assert_eq!(wire.sent[2]["args"]["authorization"], "approved");
        assert_eq!(wire.sent[3]["method"], "present");
        assert_eq!(
            wire.sent[3]["args"],
            json!({"authorization":"approved","link":format!("{ORIGIN}/f/example")})
        );
        assert_eq!(wire.sent[4]["args"], json!({"authorization":"approved"}));
        assert_eq!(wire.sent[5]["args"], json!({"file":"chosen"}));
        assert!(wire.replies.is_empty());
    }
    #[test]
    fn selection_without_an_opaque_handle_never_requests_authorization() {
        for file in [Value::Null, json!(""), json!(17)] {
            let mut wire = Script {
                replies: VecDeque::from([json!({"file":file,"name":"small.txt","bytes":7})]),
                sent: Vec::new(),
            };
            let mut upload = job(&mut wire);
            assert_eq!(
                upload.run(42, PooAuth::Keyed),
                Err(ActionError::Failed("Invalid selected file handle"))
            );
            upload.release();
            assert_eq!(wire.sent.len(), 1);
            assert_eq!(wire.sent[0]["method"], "selected");
        }
    }

    #[test]
    fn maximum_keyed_file_completes_with_the_minimum_server_part_size() {
        let parts = MAX_FILE.div_ceil(PART);
        let mut wire = Script {
            replies: VecDeque::from([
                json!({"file":"chosen","name":"large.bin","bytes":MAX_FILE}),
                json!({"authorization":"approved"}),
                json!({"status":200,"json":{"partSize":PART,"parts":parts,"token":{"secret":"session"}}}),
            ]),
            sent: Vec::new(),
        };
        for index in 0..parts {
            wire.replies.push_back(json!({"status":200,"json":{
                "success":true,"index":index,"acceptedBytes":(MAX_FILE-index*PART).min(PART),
                "receipt":{"secret":format!("part-{index}")}
            }}));
        }
        wire.replies.extend([
            json!({"status":200,"json":uploaded()}),
            json!({"presented":true}),
        ]);
        assert_eq!(
            job(&mut wire).run(42, PooAuth::Keyed).unwrap(),
            format!("{ORIGIN}/f/example")
        );
        let final_part = &wire.sent[2 + usize::try_from(parts).unwrap()]["args"]["part"];
        assert_eq!(final_part["offset"], (parts - 1) * PART);
        assert_eq!(final_part["length"], MAX_FILE - (parts - 1) * PART);
        let complete = &wire.sent[3 + usize::try_from(parts).unwrap()]["args"];
        assert_eq!(complete["path"], "/api/upload/session/complete");
        assert_eq!(
            complete["json"]["receipts"].as_array().unwrap().len() as u64,
            parts
        );
    }

    #[test]
    fn payload_and_session_size_refusals_keep_the_request_context() {
        for (request, expected) in [
            (
                json!({"multipart":{}}),
                "The server rejected the file as too large; no retry was attempted",
            ),
            (
                json!({"part":{}}),
                "The server rejected a file part as too large; no retry was attempted",
            ),
            (
                json!({"json":{"size":7}}),
                "The server rejected the upload session request as too large; no retry was attempted",
            ),
            (
                json!({"json":{"receipts":[]}}),
                "The server rejected the upload completion request as too large; no retry was attempted",
            ),
        ] {
            let mut wire = Script {
                replies: VecDeque::from([json!({"status":413})]),
                sent: Vec::new(),
            };
            let mut upload = job(&mut wire);
            upload.authorization = Some(json!("approved"));
            assert_eq!(
                upload.accepted(request),
                Err(ActionError::TooLarge(expected))
            );
            assert_eq!(wire.sent.len(), 1);
        }
    }

    #[test]
    fn session_upload_keeps_tokens_and_receipts_opaque_and_obeys_server_parts() {
        for mode in [PooAuth::Keyed, PooAuth::Keyless] {
            let mut result = uploaded();
            if mode == PooAuth::Keyless {
                result["anonymous"] = json!(true);
            }
            let mut wire = Script {
                replies: VecDeque::from([
                    json!({"file":"chosen","name":"large.bin","bytes":PART+9}),
                    json!({"authorization":"approved"}),
                    json!({"status":200,"json":{"partSize":PART,"parts":2,"token":{"secret":"session"}}}),
                    json!({"status":200,"json":{"success":true,"index":0,"receipt":{"secret":"first"}}}),
                    json!({"status":200,"json":{"success":true,"index":1,"acceptedBytes":9,"receipt":{"secret":"last"}}}),
                    json!({"status":200,"json":result}),
                    json!({"presented":true}),
                ]),
                sent: Vec::new(),
            };
            assert_eq!(
                job(&mut wire).run(42, mode).unwrap(),
                format!("{ORIGIN}/f/example")
            );
            let grant = &wire.sent[1]["args"];
            let start = &wire.sent[2]["args"];
            assert_eq!(start["path"], "/api/upload/session");
            assert_eq!(
                start["json"],
                json!({"name":"large.bin","type":"","size":PART+9,"burnMode":"Burn after 24 hours","showcase":false})
            );
            assert_eq!(start["capture_secrets"], json!(["/token"]));
            if mode == PooAuth::Keyless {
                assert_eq!(
                    grant,
                    &json!({"file":"chosen","origin":ORIGIN,"headers":["x-upload-session","x-poo-upload-client"],"anonymous":true})
                );
                assert_eq!(start["headers"], json!({"x-poo-upload-client":"native"}));
                assert!(start.get("bearer").is_none());
            } else {
                assert_eq!(
                    grant,
                    &json!({"file":"chosen","origin":ORIGIN,"headers":["x-upload-session"]})
                );
                assert_eq!(start["bearer"], true);
                assert!(start.get("headers").is_none());
            }
            assert_eq!(
                wire.sent[3]["args"]["part"],
                json!({"offset":0,"length":PART})
            );
            assert_eq!(
                wire.sent[4]["args"]["part"],
                json!({"offset":PART,"length":9})
            );
            for call in &wire.sent[3..5] {
                assert_eq!(
                    call["args"]["headers"],
                    json!({"content-type":"application/octet-stream","x-upload-session":{"secret":"session"}})
                );
                assert!(call["args"].get("bearer").is_none());
            }
            assert_eq!(
                wire.sent[5]["args"],
                json!({
                    "authorization":"approved","method":"POST","path":"/api/upload/session/complete",
                    "json":{"token":{"secret":"session"},"receipts":[{"secret":"first"},{"secret":"last"}]}
                })
            );
        }
    }
    #[test]
    fn a_receipt_for_a_different_byte_count_never_finalizes_the_file() {
        let mut wire = Script {
            replies: VecDeque::from([
                json!({"file":"chosen","name":"large.bin","bytes":PART+9}),
                json!({"authorization":"approved"}),
                json!({"status":200,"json":{"partSize":PART,"parts":2,"token":{"secret":"session"}}}),
                json!({"status":200,"json":{"success":true,"index":0,"acceptedBytes":PART-1,"receipt":{"secret":"first"}}}),
            ]),
            sent: Vec::new(),
        };
        assert_eq!(
            job(&mut wire).run(42, PooAuth::Keyed),
            Err(ActionError::Failed("Invalid server part receipt"))
        );
        assert_eq!(
            wire.sent.last().unwrap()["args"]["path"],
            "/api/upload/session/part?index=0"
        );
    }

    #[test]
    fn inconsistent_session_is_rejected_before_any_part_or_completion() {
        let mut wire = Script {
            replies: VecDeque::from([
                json!({"file":"chosen","name":"large.bin","bytes":PART+9}),
                json!({"authorization":"approved"}),
                json!({"status":200,"json":{"partSize":PART,"parts":1,"token":{"secret":"session"}}}),
            ]),
            sent: Vec::new(),
        };
        assert!(job(&mut wire).run(42, PooAuth::Keyed).is_err());
        assert_eq!(wire.sent.len(), 3);
    }
    #[test]
    fn rejected_upload_results_never_request_presentation() {
        let mut invalid = vec![
            json!({"success":false,"files":[{"id":"example","url":format!("{ORIGIN}/f/example")}]}),
            json!({"success":true,"files":[]}),
            json!({"success":true,"files":[{"id":"../example","url":format!("{ORIGIN}/f/example")}]}),
            json!({"success":true,"files":[{"id":"example","url":format!("{ORIGIN}/other")}]}),
        ];
        let mut multiple = uploaded();
        multiple["files"]
            .as_array_mut()
            .unwrap()
            .push(uploaded()["files"][0].clone());
        invalid.push(multiple);
        for response in invalid {
            let mut wire = Script {
                replies: VecDeque::from([
                    json!({"file":"chosen","name":"small.txt","bytes":7}),
                    json!({"authorization":"approved"}),
                    json!({"status":200,"json":response}),
                ]),
                sent: Vec::new(),
            };
            assert!(job(&mut wire).run(42, PooAuth::Keyed).is_err());
            assert!(
                wire.sent.iter().all(|call| {
                    call["method"] != "present" && call["args"].get("present_link").is_none()
                }),
                "A rejected upload asked the host to show a result"
            );
        }
    }
    #[test]
    fn returned_link_cannot_change_origin_path_or_identifier() {
        let mut result = uploaded();
        result["files"][0]["url"] = json!("https://example.invalid/f/example");
        assert!(result_link(&result).is_err());
        result["files"][0]["url"] = json!(format!("{ORIGIN}/f/example"));
        result["files"][0]["id"] = json!("../example");
        assert!(result_link(&result).is_err());
    }
    #[test]
    fn host_denial_remains_a_permission_failure_without_exposing_host_messages() {
        let mut wire = told(vec![Err("denied")]);
        wire.refusal_message = "private host diagnostic";
        let reply = Job::new(&mut wire, 1, 0).configure(&mut Key::default());
        assert_eq!(reply["code"], "denied");
        assert!(!reply.to_string().contains("private host diagnostic"));
    }
    #[test]
    fn a_saved_key_is_checked_against_the_provider_and_only_the_status_decides() {
        for (status, expected) in [
            (200, "valid"),
            (204, "valid"),
            (401, "rejected"),
            (403, "rejected"),
            (429, "unverified"),
            (500, "unverified"),
            (302, "unverified"),
        ] {
            let mut wire = Script {
                replies: VecDeque::from([json!({"status":status})]),
                sent: Vec::new(),
            };
            let verdict = job(&mut wire).check_key().unwrap();
            assert_eq!(verdict, expected, "status {status}");
            // The provider's identity route lives here and nowhere in the host; no file, no authorization, no key in the call.
            assert_eq!(wire.sent.len(), 1);
            assert_eq!(wire.sent[0]["service"], "peersus.http");
            assert_eq!(wire.sent[0]["method"], "check");
            assert_eq!(
                wire.sent[0]["args"],
                json!({"origin":ORIGIN,"path":"/api/me"})
            );
        }
    }
    #[test]
    fn a_key_check_that_cannot_be_asked_is_unverified_and_never_valid() {
        struct Down;
        impl Wire for Down {
            fn send(&mut self, _: Value) -> Result<(), &'static str> {
                Ok(())
            }
            fn receive(&mut self) -> Result<Value, &'static str> {
                Ok(
                    json!({"type":"result","id":1,"result":{"status":"error","code":"unavailable","message":"no network"}}),
                )
            }
        }
        let mut wire = Down;
        let mut job = Job::new(&mut wire, 1, 0);
        // A refusal is an answer of "could not tell", not a reason to call the key good or to lose it.
        assert_eq!(job.check_key().unwrap(), "unverified");
    }
    #[test]
    fn only_a_rejected_key_fails_the_action_and_nothing_in_the_answer_is_the_key_or_the_body() {
        let valid = configured("valid");
        assert_eq!(valid["status"], "ok");
        assert_eq!(valid["value"], json!({"configured":true,"key":"valid"}));
        let unverified = configured("unverified");
        assert_eq!(unverified["status"], "ok");
        assert_eq!(unverified["value"]["key"], "unverified");
        let rejected = configured("rejected");
        assert_eq!(rejected["status"], "error");
        assert_eq!(rejected["code"], "credential-rejected");
    }
    /// Answers each call in turn: `Ok` with a value, or a refusal by its code.
    struct Told {
        answers: VecDeque<Result<Value, &'static str>>,
        sent: Vec<Value>,
        refusal_message: &'static str,
    }
    impl Wire for Told {
        fn send(&mut self, value: Value) -> Result<(), &'static str> {
            self.sent.push(value);
            Ok(())
        }
        fn receive(&mut self) -> Result<Value, &'static str> {
            // The answer is to the last call made, whatever else (a refusal of someone else's call) was sent after it.
            let id = self
                .sent
                .iter()
                .rev()
                .find(|sent| sent["op"] == "call")
                .unwrap()["id"]
                .clone();
            let result = match self
                .answers
                .pop_front()
                .ok_or("Unexpected extra operation")?
            {
                Ok(value) => json!({"status":"ok","value":value}),
                Err(code) => json!({"status":"error","code":code,"message":self.refusal_message}),
            };
            Ok(json!({"type":"result","id":id,"result":result}))
        }
    }
    fn told(answers: Vec<Result<Value, &'static str>>) -> Told {
        Told {
            answers: answers.into(),
            sent: Vec::new(),
            refusal_message: "private",
        }
    }
    fn asked(wire: &Told) -> Vec<String> {
        wire.sent
            .iter()
            .filter(|call| call["op"] == "call")
            .map(|call| {
                format!(
                    "{}/{}",
                    call["service"].as_str().unwrap(),
                    call["method"].as_str().unwrap()
                )
            })
            .collect()
    }

    #[test]
    fn poo_wang_sends_the_file_and_bearer_only_under_its_approved_authorization() {
        let mut wire = told(vec![
            Ok(json!({"saved":true})),
            Ok(json!({"status":200})),
            Ok(json!({"file":"chosen","name":"small.txt","bytes":7})),
            Ok(json!({"authorization":"approved"})),
            Ok(json!({"status":200,"json":uploaded()})),
            Ok(json!({"presented":true})),
            Ok(json!({"released":true})),
            Ok(json!({"released":true})),
        ]);
        let reply = Job::new(&mut wire, 1, 0).action(
            &mut Key::default(),
            Some("choose"),
            &json!({"context":42}),
        );
        assert_eq!(
            reply,
            json!({"status":"ok","value":{"status":"uploaded","url":"https://poo.wang/f/example"}})
        );
        assert_eq!(
            asked(&wire),
            [
                "peersus.http/saved",
                "peersus.http/check",
                "peersus.files/selected",
                "peersus.http/authorize",
                "peersus.http/request",
                "peersus.http/present",
                "peersus.http/release",
                "peersus.files/release",
            ]
        );
        assert_eq!(
            wire.sent[1]["args"],
            json!({"origin":"https://poo.wang","path":"/api/me"})
        );
        assert_eq!(
            wire.sent[3]["args"],
            json!({"file":"chosen","origin":"https://poo.wang","headers":["x-upload-session"]})
        );
        assert_eq!(
            wire.sent[4]["args"],
            json!({"authorization":"approved","method":"POST","path":"/api/upload","bearer":true,
                "multipart":{"field":"files","offset":0,"length":7,
                    "fields":{"burnMode":"Burn after 24 hours","showcase":"false"}}})
        );
    }

    #[test]
    fn poo_wang_without_a_saved_key_uses_only_the_anonymous_grant_and_marker() {
        let mut result = uploaded();
        result["anonymous"] = json!(true);
        let mut wire = told(vec![
            Ok(json!({"saved":false})),
            Ok(json!({"file":"chosen","name":"small.txt","bytes":7})),
            Ok(json!({"authorization":"approved"})),
            Ok(json!({"status":200,"json":result})),
            Ok(json!({"presented":true})),
            Ok(json!({"released":true})),
            Ok(json!({"released":true})),
        ]);
        let reply = Job::new(&mut wire, 1, 0).action(
            &mut Key::default(),
            Some("choose_keyless"),
            &json!({"context":42}),
        );
        assert_eq!(reply["value"]["url"], format!("{ORIGIN}/f/example"));
        assert_eq!(
            asked(&wire),
            [
                "peersus.http/saved",
                "peersus.files/selected",
                "peersus.http/authorize",
                "peersus.http/request",
                "peersus.http/present",
                "peersus.http/release",
                "peersus.files/release",
            ]
        );
        assert_eq!(
            wire.sent[1]["args"],
            json!({"context":42,"max_bytes":25 * 1024 * 1024})
        );
        assert_eq!(
            wire.sent[2]["args"],
            json!({
                "file":"chosen","origin":ORIGIN,"anonymous":true,
                "headers":["x-upload-session","x-poo-upload-client"]
            })
        );
        // The exact body and headers contain the chosen file and the fixed opt-in, no account or device identifier.
        assert_eq!(
            wire.sent[3]["args"],
            json!({
                "authorization":"approved","method":"POST","path":"/api/upload",
                "headers":{"x-poo-upload-client":"native"},
                "multipart":{"field":"files","offset":0,"length":7,"fields":{"burnMode":"Burn after 24 hours","showcase":"false"}}
            })
        );
    }

    #[test]
    fn an_unknown_saved_key_state_is_not_permission_to_upload_anonymously() {
        for answer in [
            Err("denied"),
            Err("cancelled"),
            Err("unavailable"),
            Err("credential-store-failed"),
            Ok(json!({"saved":"false"})),
        ] {
            for previous in [false, true] {
                let mut wire = told(vec![answer.clone()]);
                let mut key = Key {
                    saved: Some(previous),
                    verdict: previous.then_some("valid"),
                    why: None,
                };
                let reply = Job::new(&mut wire, 1, 0).action(
                    &mut key,
                    Some("choose"),
                    &json!({"context":42}),
                );
                assert_eq!(
                    reply,
                    json!({
                        "status":"error", "code":"unavailable",
                        "message":"The application could not establish whether a poo.wang API key is saved; no upload was attempted"
                    })
                );
                assert_eq!(key, Key::default());
                assert_eq!(asked(&wire), ["peersus.http/saved"]);
            }
        }
    }

    #[test]
    fn withdrawing_an_upload_during_key_lookup_cancels_the_pending_call() {
        let mut wire = arrives([(1, json!({"type":"cancelled","ticket":1}))], vec![]);
        let mut job = Job::new(&mut wire, 1, 0);
        let reply = job.action(&mut Key::default(), Some("choose"), &json!({"context":42}));
        assert!(job.cancelled);
        assert_eq!(reply["code"], "cancelled");
        assert_eq!(asked(&wire.then), ["peersus.http/saved"]);
        assert_eq!(wire.then.sent[1], json!({"op":"cancel","id":1}));
    }

    #[test]
    fn a_keyless_upload_requires_an_anonymous_result_before_presentation() {
        for result in [
            uploaded(),
            json!({"anonymous":true,"success":true,"files":[]}),
        ] {
            let mut wire = told(vec![
                Ok(json!({"saved":false})),
                Ok(json!({"file":"chosen","name":"small.txt","bytes":7})),
                Ok(json!({"authorization":"approved"})),
                Ok(json!({"status":200,"json":result})),
                Ok(json!({"released":true})),
                Ok(json!({"released":true})),
            ]);
            let reply = Job::new(&mut wire, 1, 0).action(
                &mut Key::default(),
                Some("choose_keyless"),
                &json!({"context":42}),
            );
            assert_eq!(reply["code"], "upload-failed");
            assert_eq!(
                asked(&wire),
                [
                    "peersus.http/saved",
                    "peersus.files/selected",
                    "peersus.http/authorize",
                    "peersus.http/request",
                    "peersus.http/release",
                    "peersus.files/release",
                ]
            );
        }
    }

    #[test]
    fn a_valid_upload_with_failed_presentation_is_reported_without_uploading_again() {
        for (method, answer) in [
            ("choose", json!({"status":200,"json":uploaded()})),
            ("choose_anonymous", json!({"status":200,"text":CATBOX_LINK})),
        ] {
            for (presentation, blocked) in [
                (Some(Ok(json!({"presented":false}))), None),
                (Some(Err("denied")), None),
                (
                    None,
                    Some(
                        json!({"status":"missing-permission","permission":{"owner":"host","name":"network.http"}}),
                    ),
                ),
                (None, Some(json!({"status":"not-accepted"}))),
            ] {
                let mut answers = Vec::new();
                if method == "choose" {
                    answers.extend([Ok(json!({"saved":true})), Ok(json!({"status":200}))]);
                }
                let prefix = answers.len();
                answers.extend([
                    Ok(json!({"file":"chosen","name":"small.txt","bytes":7})),
                    Ok(json!({"authorization":"approved"})),
                    Ok(answer.clone()),
                ]);
                answers.extend(presentation);
                let mut expected = vec![
                    "peersus.files/selected",
                    "peersus.http/authorize",
                    "peersus.http/request",
                    "peersus.http/present",
                ];
                if blocked
                    .as_ref()
                    .is_none_or(|blocked| blocked["status"] != "missing-permission")
                {
                    expected.push("peersus.http/release");
                    answers.push(Ok(json!({"released":true})));
                }
                expected.push("peersus.files/release");
                answers.push(Ok(json!({"released":true})));
                let id = prefix + 4;
                let mut wire = arrives(
                    blocked.map(|result| (id, json!({"type":"result","id":id,"result":result}))),
                    answers,
                );
                let reply = Job::new(&mut wire, 1, 0).action(
                    &mut Key::default(),
                    Some(method),
                    &json!({"context":42}),
                );
                assert_eq!(reply["code"], "not-shown");
                assert_eq!(asked(&wire.then)[prefix..], expected);
            }
        }
    }

    #[test]
    fn an_upload_only_reports_not_allowed_for_a_host_permission_denial() {
        for (host_code, expected) in [
            ("denied", "denied"),
            ("cancelled", "cancelled"),
            ("unavailable", "unavailable"),
            ("platform-refused", "upload-failed"),
            ("refused", "upload-failed"),
        ] {
            let mut wire = told(vec![Err(host_code)]);
            let reply = Job::new(&mut wire, 1, 0).action(
                &mut Key::default(),
                Some("choose_anonymous"),
                &json!({"context":42}),
            );
            assert_eq!(reply["status"], "error");
            assert_eq!(reply["code"], expected);
            assert_eq!(asked(&wire), ["peersus.files/selected"]);
        }
    }

    #[test]
    fn checked_upload_refusals_are_not_failures_and_never_resend_the_request() {
        for refusal in [
            json!({"status":"missing-permission","permission":{"owner":"host","name":"network.http"}}),
            json!({"status":"not-accepted"}),
        ] {
            let mut wire = arrives(
                [(3, json!({"type":"result","id":3,"result":refusal}))],
                vec![
                    Ok(json!({"file":"chosen","name":"small.txt","bytes":7})),
                    Ok(json!({"authorization":"approved"})),
                    Ok(json!({"released":true})),
                    Ok(json!({"released":true})),
                ],
            );
            let mut job = Job::new(&mut wire, 1, 0);
            let reply = job.action(
                &mut Key::default(),
                Some("choose_anonymous"),
                &json!({"context":42}),
            );
            assert_eq!(reply, refusal);
            let _ = job.status(&mut Key::default(), &json!({"t":"tick","id":"key"}));
            let mut expected = vec![
                "peersus.files/selected",
                "peersus.http/authorize",
                "peersus.http/request",
            ];
            if refusal["status"] == "not-accepted" {
                expected.push("peersus.http/release");
            }
            expected.push("peersus.files/release");
            assert_eq!(asked(&wire.then), expected);
        }
    }

    #[test]
    fn denied_key_check_is_named_instead_of_assuming_an_unverified_key_can_upload() {
        let refusal = json!({"status":"missing-permission","permission":{"owner":"host","name":"network.http"}});
        let mut wire = arrives(
            [(2, json!({"type":"result","id":2,"result":refusal}))],
            vec![Ok(json!({"saved":true}))],
        );
        let mut key = Key::default();
        let mut job = Job::new(&mut wire, 1, 0);
        let reply = job.action(&mut key, Some("choose"), &json!({"context":42}));
        assert_eq!(reply, refusal);
        assert!(
            key.words()
                .contains("missing permission network.http on this device")
        );
        assert_eq!(
            asked(&wire.then),
            ["peersus.http/saved", "peersus.http/check"]
        );
    }

    #[test]
    fn a_grant_change_reads_only_readiness_until_an_explicit_new_action() {
        let blocked = json!({"owner":"host","name":"files.select"});
        let config = |missing: Value| json!({"settings":{"destination":"Choose on Send"},"missing_permissions":missing,"peers":[]});
        let invoke = |ticket| json!({"type":"call","ticket":ticket,"service":"upload.file","method":"choose_anonymous","args":{"context":42}});
        let signal = json!({"type":"event","provider":"host","service":"peersus.config","version":1,"event":"changed","data":{}});
        let mut wire = arrives(
            [
                (
                    2,
                    json!({"type":"result","id":2,"result":{"status":"ok","value":{"saved":"unknown"}}}),
                ),
                (2, invoke(10)),
                (2, signal),
                (
                    3,
                    json!({"type":"result","id":3,"result":{"status":"ok","value":config(json!([]))}}),
                ),
                (3, invoke(11)),
            ],
            vec![
                Ok(config(json!([blocked]))),
                Ok(json!({"file":"chosen","name":"small.txt","bytes":7})),
                Ok(json!({"authorization":"approved"})),
                Ok(json!({"status":200,"text":CATBOX_LINK})),
                Ok(json!({"presented":true})),
                Ok(json!({"released":true})),
                Ok(json!({"released":true})),
            ],
        );
        assert_eq!(serve(&mut wire), 0);
        assert_eq!(wire.then.sent[0], host::subscribe_config());
        assert_eq!(
            wire.then.sent[3]["result"],
            json!({"status":"missing-permission","permission":blocked})
        );
        assert_eq!(
            wire.then.sent.last().unwrap()["result"]["value"]["status"],
            "uploaded"
        );
        assert_eq!(
            asked(&wire.then),
            [
                "peersus.config/get",
                "peersus.http/saved",
                "peersus.config/get",
                "peersus.files/selected",
                "peersus.http/authorize",
                "peersus.http/request",
                "peersus.http/present",
                "peersus.http/release",
                "peersus.files/release",
            ]
        );
    }

    #[test]
    fn provider_refusal_or_malformed_reply_never_presents_a_link_or_retries() {
        for (answer, expected) in [
            (json!({"status":403}), "refused"),
            (json!({"status":413}), "too-large"),
            (json!({"status":429}), "rate-limited"),
            (json!({"status":"200"}), "upload-failed"),
        ] {
            for (method, saved, size) in [
                ("choose", Some(true), 7),
                ("choose_keyless", Some(false), 7),
                ("choose_keyless", Some(false), PART + 9),
                ("choose_anonymous", None, 7),
            ] {
                let mut answers = Vec::new();
                if let Some(saved) = saved {
                    answers.push(Ok(json!({"saved":saved})));
                    if saved {
                        answers.push(Ok(json!({"status":200})));
                    }
                }
                let prefix = answers.len();
                answers.extend([
                    Ok(json!({"file":"chosen","name":"small.txt","bytes":size})),
                    Ok(json!({"authorization":"approved"})),
                    Ok(answer.clone()),
                    Ok(json!({"released":true})),
                    Ok(json!({"released":true})),
                ]);
                let mut wire = told(answers);
                let reply = Job::new(&mut wire, 1, 0).action(
                    &mut Key::default(),
                    Some(method),
                    &json!({"context":42}),
                );
                assert_eq!(reply["status"], "error");
                assert_eq!(reply["code"], expected, "{method}: {answer}");
                assert_eq!(
                    asked(&wire)[prefix..],
                    [
                        "peersus.files/selected",
                        "peersus.http/authorize",
                        "peersus.http/request",
                        "peersus.http/release",
                        "peersus.files/release",
                    ]
                );
            }
        }
    }

    #[test]
    fn disabled_destinations_and_missing_input_are_unavailable_before_work_starts() {
        for (destination, method, context) in [
            (Destination::Catbox, "choose", json!(42)),
            (Destination::Catbox, "choose_keyless", json!(42)),
            (Destination::PooWang, "choose_anonymous", json!(42)),
            (Destination::Both, "choose", Value::Null),
            (Destination::Both, "choose", json!(0)),
            (Destination::Both, "choose_keyless", json!(0)),
            (Destination::Both, "choose_anonymous", json!(0)),
        ] {
            let mut wire = told(vec![]);
            let mut job = Job::new(&mut wire, 1, 0);
            job.destination = destination;
            let reply = job.action(
                &mut Key::default(),
                Some(method),
                &json!({"context":context}),
            );
            assert_eq!(reply["status"], "error");
            assert_eq!(reply["code"], "unavailable");
            assert!(wire.sent.is_empty());
        }
    }
    #[test]
    fn startup_offers_only_the_selected_destination_and_confirmed_mode() {
        for (setting, saved, hidden) in [
            ("Choose on Send", Some(true), &["upload_keyless"][..]),
            ("Choose on Send", Some(false), &["upload"][..]),
            ("Choose on Send", None, POO_ACTIONS),
            (
                "poo.wang",
                Some(true),
                &["upload_keyless", "upload_anonymous"][..],
            ),
            ("poo.wang", Some(false), &["upload", "upload_anonymous"][..]),
            (
                "poo.wang",
                None,
                &["upload", "upload_keyless", "upload_anonymous"][..],
            ),
            (
                "Catbox",
                None,
                &["upload", "upload_keyless", "configure"][..],
            ),
        ] {
            let mut answers = vec![Ok(json!({"settings":{"destination":setting}}))];
            let mut expected = vec!["peersus.config/get"];
            if setting != "Catbox" {
                answers.push(saved.map_or(Err("unavailable"), |saved| Ok(json!({"saved":saved}))));
                expected.push("peersus.http/saved");
            }
            if hidden != POO_ACTIONS {
                answers.push(Ok(json!({})));
                expected.push("peersus.actions/withhold");
            }
            let mut wire = told(answers);
            let mut job = Job::starting(&mut wire);
            let mut key = Key::default();
            assert!(job.initialize(&mut key).is_ok());
            assert_eq!(job.withheld, hidden);
            if setting == "Catbox" {
                assert_eq!(
                    job.status(&mut key, &json!({"t":"open"}))["value"]["text"],
                    CATBOX_WORDS
                );
            }
            assert_eq!(asked(&wire), expected);
            if hidden != POO_ACTIONS {
                assert_eq!(wire.sent.last().unwrap()["args"]["actions"], json!(hidden));
            }
        }
    }

    #[test]
    fn a_stale_poo_offer_updates_the_rows_but_never_substitutes_the_mode() {
        for (method, old, actual, hidden) in [
            (
                "choose",
                &["upload_keyless"][..],
                Ok(json!({"saved":false})),
                &["upload"][..],
            ),
            (
                "choose_keyless",
                &["upload"][..],
                Ok(json!({"saved":true})),
                &["upload_keyless"][..],
            ),
            (
                "choose_keyless",
                &["upload"][..],
                Err("denied"),
                POO_ACTIONS,
            ),
        ] {
            let mut wire = told(vec![actual, Ok(json!({}))]);
            let mut job = Job::new(&mut wire, 1, 0);
            job.withheld = old;
            let reply = job.action(&mut Key::default(), Some(method), &json!({"context":42}));
            assert_eq!(reply["code"], "unavailable");
            assert_eq!(
                asked(&wire),
                ["peersus.http/saved", "peersus.actions/withhold"]
            );
            assert_eq!(wire.sent[1]["args"]["actions"], json!(hidden));
        }
    }

    #[test]
    fn unknown_key_state_is_visible_in_settings_and_manual_refresh_can_restore_an_offer() {
        let mut wire = told(vec![
            Err("denied"),
            Ok(json!({})),
            Ok(json!({"saved":false})),
            Ok(json!({})),
        ]);
        let mut job = Job::new(&mut wire, 1, 0);
        let mut key = Key {
            saved: Some(true),
            ..Key::default()
        };
        job.withheld = &["upload_keyless"];
        let unknown = job.status(&mut key, &json!({"t":"open"}));
        assert_eq!(
            unknown["value"]["child"]["children"][0]["text"],
            "The application could not check whether a poo.wang API key is saved."
        );
        assert_eq!(unknown["value"]["child"]["children"][1]["id"], "refresh");
        assert_eq!(job.withheld, POO_ACTIONS);
        let refreshed = job.status(&mut key, &json!({"t":"press","id":"refresh"}));
        assert_eq!(refreshed["status"], "ok");
        assert_eq!(job.withheld, ["upload"]);
        assert_eq!(
            asked(&wire),
            [
                "peersus.http/saved",
                "peersus.actions/withhold",
                "peersus.http/saved",
                "peersus.actions/withhold"
            ]
        );
    }

    #[test]
    fn refreshing_saved_key_state_discards_a_verdict_for_a_missing_or_unknown_key() {
        for (answer, saved) in [
            (Ok(json!({"saved":false})), Some(false)),
            (Ok(json!({"saved":true})), Some(true)),
            (Err("unavailable"), None),
        ] {
            let mut wire = told(vec![answer]);
            let mut key = Key {
                saved: Some(true),
                verdict: Some("valid"),
                why: None,
            };
            Job::new(&mut wire, 0, 0).refresh(&mut key).unwrap();
            assert_eq!(key.saved, saved);
            // What the provider said was of the key that was there.
            assert_eq!(key.verdict, (saved == Some(true)).then_some("valid"));
            assert_eq!(asked(&wire), ["peersus.http/saved"]);
            assert_eq!(wire.sent[0]["args"], json!({"origin":ORIGIN}));
        }
    }
    /// Inject inbound messages once a given number of host requests have been sent.
    struct Arrives {
        events: VecDeque<(usize, Value)>,
        then: Told,
    }
    impl Wire for Arrives {
        fn send(&mut self, value: Value) -> Result<(), &'static str> {
            self.then.send(value)
        }
        fn receive(&mut self) -> Result<Value, &'static str> {
            let sent = self
                .then
                .sent
                .iter()
                .filter(|value| value["op"] == "call")
                .count();
            if self.events.front().is_some_and(|(after, _)| *after <= sent)
                && let Some((_, event)) = self.events.pop_front()
            {
                return Ok(event);
            }
            self.then.receive()
        }
    }
    fn arrives(
        events: impl IntoIterator<Item = (usize, Value)>,
        answers: Vec<Result<Value, &'static str>>,
    ) -> Arrives {
        Arrives {
            events: events.into_iter().collect(),
            then: told(answers),
        }
    }
    #[test]
    fn a_call_that_arrives_while_the_extension_starts_is_kept_and_one_in_the_middle_of_another_is_refused()
     {
        let call = json!({"type":"call","ticket":9,"service":"upload.file","method":"choose","args":{"context":42}});
        let mut wire = arrives(
            [(1, call.clone())],
            vec![
                Ok(json!({"settings":{"destination":"Choose on Send"}})),
                Ok(json!({"saved":true})),
                Ok(json!({})),
            ],
        );
        let mut key = Key::default();
        let mut job = Job::starting(&mut wire);
        assert!(matches!(job.initialize(&mut key), Ok(Destination::Both)));
        // Kept whole for the loop that serves calls; nothing was said to the application about it.
        assert_eq!(job.early, VecDeque::from([call.clone()]));
        assert!(wire.then.sent.iter().all(|sent| sent["op"] == "call"));

        // Once started, a second call in the middle of the first is told the extension is busy, by its own ticket.
        let mut wire = arrives([(1, call)], vec![Ok(json!({"saved":true}))]);
        let mut job = Job::new(&mut wire, 1, 0);
        job.refresh(&mut key).unwrap();
        assert!(job.early.is_empty());
        let refusal = &wire.then.sent[1];
        assert_eq!(refusal["op"], "reply");
        assert_eq!(refusal["ticket"], 9);
        assert_eq!(refusal["result"]["code"], "busy");

        // The settings line draws itself every two seconds. While another call waits (for the person to write a key, or for
        // the provider), that is answered at once with the line as it was known, and is no error for the application to say.
        let line = json!({"type":"call","ticket":12,"service":"upload.file","method":"status","args":{"event":{"t":"tick","id":"key","at":1}}});
        let mut wire = arrives([(1, line)], vec![Ok(json!({"configured":true}))]);
        let known = key.page();
        let mut job = Job::new(&mut wire, 1, 0);
        job.shown = known.clone();
        job.call("peersus.http", "configure", json!({"origin":ORIGIN}))
            .unwrap();
        assert_eq!(
            wire.then.sent[1],
            json!({"op":"reply","ticket":12,"result":{"status":"ok","value":known}})
        );
    }

    #[test]
    fn cancelled_startup_uploads_are_not_dispatched() {
        let upload = json!({"type":"call","ticket":9,"service":"upload.file","method":"choose_anonymous","args":{"context":42}});
        let cancelled = json!({"type":"cancelled","ticket":9});
        let mut wire = arrives(
            [(1, upload), (1, cancelled)],
            vec![
                Ok(json!({"settings":{"destination":"Catbox"}})),
                Ok(json!({})),
            ],
        );
        assert_eq!(serve(&mut wire), 0);
        assert_eq!(
            asked(&wire.then),
            ["peersus.config/get", "peersus.actions/withhold"]
        );
        assert!(
            wire.then.sent[1..]
                .iter()
                .all(|message| message["op"] == "call")
        );
    }

    #[test]
    fn a_later_startup_job_can_be_withdrawn_while_the_first_one_waits() {
        let upload = |ticket| json!({"type":"call","ticket":ticket,"service":"upload.file","method":"choose_anonymous","args":{"context":42}});
        let mut wire = arrives(
            [
                (1, upload(9)),
                (1, upload(10)),
                (3, json!({"type":"cancelled","ticket":10})),
            ],
            vec![
                Ok(json!({"settings":{"destination":"Catbox"}})),
                Ok(json!({})),
                Err("unavailable"),
            ],
        );
        assert_eq!(serve(&mut wire), 0);
        assert_eq!(
            asked(&wire.then),
            [
                "peersus.config/get",
                "peersus.actions/withhold",
                "peersus.files/selected"
            ]
        );
        let replied: Vec<_> = wire
            .then
            .sent
            .iter()
            .filter(|message| message["op"] == "reply")
            .filter_map(|message| message["ticket"].as_u64())
            .collect();
        assert_eq!(replied, [9]);
    }

    #[test]
    fn settings_open_and_check_now_do_not_succeed_from_a_cached_page() {
        for event in [json!({"t":"open"}), json!({"t":"press","id":"check"})] {
            let call = json!({"type":"call","ticket":12,"service":"upload.file","method":"status","args":{"event":event}});
            let mut wire = arrives([(1, call)], vec![Ok(json!({"configured":true}))]);
            let mut job = Job::new(&mut wire, 1, 0);
            job.shown = Key::default().page();
            job.call("peersus.http", "configure", json!({"origin":ORIGIN}))
                .unwrap();
            assert_eq!(wire.then.sent[1]["ticket"], 12);
            assert_eq!(wire.then.sent[1]["result"]["code"], "busy");
            assert_eq!(asked(&wire.then), ["peersus.http/configure"]);
        }
    }

    #[test]
    fn unreadable_destination_never_checks_a_key_or_dispatches_an_upload() {
        let upload = json!({"type":"call","ticket":9,"service":"upload.file","method":"choose","args":{"context":42}});
        for config in [
            Err("unavailable"),
            Ok(json!({"settings":{}})),
            Ok(json!({"settings":{"destination":true}})),
            Ok(json!({"settings":{"destination":"unknown"}})),
        ] {
            let mut wire = arrives([(1, upload.clone())], vec![config.clone(), config]);
            assert_eq!(serve(&mut wire), 0);
            assert_eq!(
                asked(&wire.then),
                ["peersus.config/get", "peersus.config/get"]
            );
            let reply = wire.then.sent.last().unwrap();
            assert_eq!(reply["ticket"], 9);
            assert_eq!(reply["result"]["code"], "unavailable");
        }
    }

    #[test]
    fn refused_action_offers_stop_startup_and_are_reported_to_the_caller() {
        let upload = json!({"type":"call","ticket":9,"service":"upload.file","method":"choose_anonymous","args":{"context":42}});
        let mut wire = arrives(
            [(1, upload)],
            vec![
                Ok(json!({"settings":{"destination":"Catbox"}})),
                Err("unavailable"),
                Ok(json!({"settings":{"destination":"Catbox"}})),
                Err("unavailable"),
            ],
        );
        assert_eq!(serve(&mut wire), 0);
        assert_eq!(
            asked(&wire.then),
            [
                "peersus.config/get",
                "peersus.actions/withhold",
                "peersus.config/get",
                "peersus.actions/withhold",
            ]
        );
        assert_eq!(
            wire.then
                .sent
                .iter()
                .find(|message| message["method"] == "withhold")
                .unwrap()["args"]["actions"],
            json!(["upload", "upload_keyless", "configure"])
        );
        assert_eq!(
            wire.then.sent.last().unwrap()["result"]["code"],
            "unavailable"
        );
    }

    #[test]
    fn startup_can_recover_on_a_later_request_without_a_restart() {
        let request = |ticket, method, args| json!({"type":"call","ticket":ticket,"service":"upload.file","method":method,"args":args});
        let catbox = || json!({"settings":{"destination":"Catbox"}});
        for (mut answers, mut expected) in [
            (vec![Err("unavailable")], vec!["peersus.config/get"]),
            (
                vec![Ok(catbox()), Err("unavailable")],
                vec!["peersus.config/get", "peersus.actions/withhold"],
            ),
        ] {
            answers.extend([
                Err("unavailable"),
                Ok(catbox()),
                Ok(json!({})),
                Err("unavailable"),
            ]);
            expected.extend([
                "peersus.config/get",
                "peersus.config/get",
                "peersus.actions/withhold",
                "peersus.files/selected",
            ]);
            let mut wire = arrives(
                [
                    (1, request(9, "choose", json!({"context":42}))),
                    (1, request(10, "status", json!({"event":{"t":"tick"}}))),
                    (1, request(11, "status", json!({"event":{"t":"open"}}))),
                    (1, request(12, "choose_anonymous", json!({"context":42}))),
                ],
                answers,
            );
            assert_eq!(serve(&mut wire), 0);
            assert_eq!(asked(&wire.then), expected);
            let replies: Vec<_> = wire
                .then
                .sent
                .iter()
                .filter(|m| m["op"] == "reply")
                .collect();
            assert_eq!(replies.len(), 4);
            assert_eq!(replies[0]["ticket"], 9);
            assert_eq!(replies[0]["result"]["code"], "unavailable");
            assert_eq!(replies[1]["ticket"], 10);
            assert_eq!(replies[1]["result"]["code"], "unavailable");
            assert_eq!(replies[2]["ticket"], 11);
            assert_eq!(replies[2]["result"]["status"], "ok");
            assert_eq!(replies[3]["ticket"], 12);
        }
    }

    #[test]
    fn failed_startup_does_not_retry_while_idle() {
        let mut wire = told(vec![Err("unavailable")]);
        assert_eq!(serve(&mut wire), 0);
        assert_eq!(asked(&wire), ["peersus.config/get"]);
    }

    #[test]
    fn withdrawing_configuration_during_refresh_stops_follow_up_after_the_save() {
        for (after, answers, expected) in [
            (
                3,
                vec![Ok(json!({"configured":true})), Ok(json!({}))],
                vec![
                    "peersus.http/configure",
                    "peersus.actions/withhold",
                    "peersus.http/saved",
                ],
            ),
            (
                5,
                vec![
                    Ok(json!({"configured":true})),
                    Ok(json!({})),
                    Ok(json!({"saved":true})),
                    Ok(json!({})),
                ],
                vec![
                    "peersus.http/configure",
                    "peersus.actions/withhold",
                    "peersus.http/saved",
                    "peersus.actions/withhold",
                    "peersus.http/check",
                ],
            ),
        ] {
            let mut wire = arrives([(after, json!({"type":"cancelled","ticket":1}))], answers);
            let mut key = Key {
                saved: Some(true),
                verdict: Some("valid"),
                why: None,
            };
            let mut job = Job::new(&mut wire, 1, 0);
            job.withheld = &["upload_keyless"];
            let reply = job.configure(&mut key);
            assert!(job.cancelled);
            assert_eq!(reply["code"], "cancelled");
            assert_eq!(key.verdict, None);
            assert_eq!(asked(&wire.then), expected);
            assert_eq!(
                wire.then.sent.last().unwrap(),
                &json!({"op":"cancel","id":after})
            );
        }
    }

    #[test]
    fn malformed_configure_acknowledgement_stops_before_the_key_is_checked() {
        for value in [
            json!(true),
            json!([]),
            json!({}),
            json!(null),
            json!({"configured":false}),
            json!({"configured":"true"}),
        ] {
            let mut wire = told(vec![Ok(value)]);
            let reply = Job::new(&mut wire, 1, 0).configure(&mut Key::default());
            assert_eq!(reply["status"], "error");
            assert_eq!(reply["code"], "configuration-failed");
            assert_eq!(asked(&wire), ["peersus.http/configure"]);
        }
        let mut wire = told(vec![Ok(json!({"configured":false})), Ok(json!({}))]);
        let mut key = Key {
            saved: Some(true),
            verdict: Some("valid"),
            why: None,
        };
        let mut job = Job::new(&mut wire, 1, 0);
        job.withheld = &["upload_keyless"];
        assert_eq!(job.configure(&mut key)["code"], "configuration-failed");
        assert_eq!(key, Key::default());
        assert_eq!(
            asked(&wire),
            ["peersus.http/configure", "peersus.actions/withhold"]
        );
        assert_eq!(
            wire.sent.last().unwrap()["args"],
            json!({"actions":POO_ACTIONS})
        );
    }
    #[test]
    fn saving_a_key_asks_the_provider_about_it_and_removing_one_asks_nobody() {
        let mut key = Key::default();
        let mut wire = told(vec![
            Ok(json!({"configured":true,"unrelated":"synthetic host field"})),
            Ok(json!({"saved":true})),
            Ok(json!({})),
            Ok(json!({"status":200})),
        ]);
        let saved = Job::new(&mut wire, 1, 0).configure(&mut key);
        assert_eq!(saved["value"], json!({"configured":true,"key":"valid"}));
        assert_eq!(
            key,
            Key {
                saved: Some(true),
                verdict: Some("valid"),
                why: None,
            }
        );
        assert_eq!(asked(&wire).last().unwrap(), "peersus.http/check");

        // Another key saved over it, which the provider refuses: the action fails and the upload stays offered, since a key is there.
        let mut wire = told(vec![
            Ok(json!({"configured":true})),
            Ok(json!({"saved":true})),
            Ok(json!({})),
            Ok(json!({"status":401})),
        ]);
        let refused = Job::new(&mut wire, 1, 0).configure(&mut key);
        assert_eq!(refused["code"], "credential-rejected");
        assert_eq!(key.verdict, Some("rejected"));
        assert_eq!(
            asked(&wire),
            [
                "peersus.http/configure",
                "peersus.http/saved",
                "peersus.actions/withhold",
                "peersus.http/check"
            ]
        );

        let mut wire = told(vec![
            Ok(json!({"configured":true})),
            Ok(json!({})),
            Ok(json!({"saved":false})),
            Ok(json!({})),
        ]);
        let mut job = Job::new(&mut wire, 1, 0);
        job.withheld = &["upload_keyless"];
        let removed = job.configure(&mut key);
        assert_eq!(removed["value"], json!({"configured":true,"key":"none"}));
        assert_eq!(
            key,
            Key {
                saved: Some(false),
                verdict: None,
                why: None,
            }
        );
        assert_eq!(
            asked(&wire),
            [
                "peersus.http/configure",
                "peersus.actions/withhold",
                "peersus.http/saved",
                "peersus.actions/withhold"
            ]
        );
        assert_eq!(wire.sent[1]["args"]["actions"], json!(POO_ACTIONS));
        assert_eq!(wire.sent[3]["args"]["actions"], json!(["upload"]));
    }
    #[test]
    fn an_upload_asks_about_the_key_before_it_touches_the_file() {
        let mut key = Key::default();
        // A key the provider refuses: said before the file is asked for.
        let mut wire = told(vec![Ok(json!({"saved":true})), Ok(json!({"status":403}))]);
        let reply =
            Job::new(&mut wire, 1, 0).action(&mut key, Some("choose"), &json!({"context":42}));
        assert_eq!(reply["status"], "error");
        assert_eq!(reply["code"], "credential-rejected");
        assert_eq!(key.verdict, Some("rejected"));
        assert_eq!(asked(&wire), ["peersus.http/saved", "peersus.http/check"]);
        // A provider that could not say does not stop an upload: the file is asked for next.
        let mut wire = told(vec![
            Ok(json!({"saved":true})),
            Err("unavailable"),
            Err("unavailable"),
        ]);
        assert!(
            Job::new(&mut wire, 1, 0)
                .upload(&mut key, Some(42), PooAuth::Keyed)
                .is_err()
        );
        assert_eq!(key.verdict, Some("unverified"));
        assert_eq!(asked(&wire).last().unwrap(), "peersus.files/selected");
        // No file chosen: nobody is asked anything.
        let mut wire = told(vec![]);
        assert!(
            Job::new(&mut wire, 1, 0)
                .upload(&mut key, None, PooAuth::Keyed)
                .is_err()
        );
        assert!(wire.sent.is_empty());
    }
    #[test]
    fn the_settings_line_says_what_is_known_and_checks_only_when_asked() {
        let line = |page: &Value| {
            page["value"]["child"]["children"][0]["text"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        let button = |page: &Value| page["value"]["child"]["children"][1]["id"] == "check";
        let mut key = Key::default();
        // Opened: the application is asked, the provider is not.
        let mut wire = told(vec![Ok(json!({"saved":true})), Ok(json!({}))]);
        let page = Job::new(&mut wire, 1, 0).status(&mut key, &json!({"t":"open"}));
        assert_eq!(page["value"]["k"], "interval");
        assert!(line(&page).contains("has not been asked") && button(&page));
        assert_eq!(
            asked(&wire),
            ["peersus.http/saved", "peersus.actions/withhold"]
        );
        // Its own clock draws what is known and asks nobody.
        let mut wire = told(vec![]);
        let again =
            Job::new(&mut wire, 1, 0).status(&mut key, &json!({"t":"tick","id":"key","at":1}));
        assert_eq!(again, page);
        assert!(wire.sent.is_empty());
        // "Check now" asks the provider.
        let mut wire = told(vec![
            Ok(json!({"saved":true})),
            Ok(json!({})),
            Ok(json!({"status":200})),
        ]);
        let checked =
            Job::new(&mut wire, 1, 0).status(&mut key, &json!({"t":"press","id":"check"}));
        assert!(line(&checked).contains("accepts it"));
        // A provider that could not be asked is not "not asked yet": the line says so, with the application's own sentence for
        // why, or the status the provider answered with. Nothing of it stays once the provider answers.
        for (answer, said) in [
            (
                Err("platform-refused"),
                "An API key is saved. The provider could not be asked about it: private",
            ),
            (
                Ok(json!({"status":503})),
                "An API key is saved. The provider could not be asked about it: it answered with status 503",
            ),
            (
                Ok(json!({})),
                "An API key is saved. The provider could not be asked about it.",
            ),
            (
                Ok(json!({"status":401})),
                "An API key is saved, but the provider does not accept it.",
            ),
        ] {
            let mut wire = told(vec![Ok(json!({"saved":true})), Ok(json!({})), answer]);
            let page =
                Job::new(&mut wire, 1, 0).status(&mut key, &json!({"t":"press","id":"check"}));
            assert_eq!(line(&page), said);
        }
        assert_eq!(key.why, None);
        // With no key there is nothing to check, and with nothing known the line says what to allow.
        let none = Key {
            saved: Some(false),
            verdict: None,
            why: None,
        }
        .page();
        assert!(
            none["child"]["children"]
                .as_array()
                .unwrap()
                .iter()
                .all(|child| child["id"] != "check"),
            "nothing to check without a key"
        );
        // The destination that needs no key is named in the same settings line, so both are visible where one is configured.
        assert!(
            none["child"]["children"]
                .as_array()
                .unwrap()
                .iter()
                .any(|child| child["text"] == CATBOX_WORDS)
        );
        assert!(
            Key {
                saved: Some(true),
                verdict: Some("rejected"),
                why: None,
            }
            .words()
            .contains("does not accept")
        );
    }
    const CATBOX_LINK: &str = "https://files.catbox.moe/a1b2c3.png";
    #[test]
    fn catbox_sends_no_key_and_does_not_look_for_one_even_if_poo_wang_has_one() {
        let size = 6 * 1024 * 1024;
        for saved in [true, false] {
            let mut key = Key {
                saved: Some(saved),
                ..Key::default()
            };
            let mut wire = told(vec![
                Ok(json!({"file":"chosen","name":"file.png","bytes":size})),
                Ok(json!({"authorization":"approved"})),
                Ok(json!({"status":200,"text":CATBOX_LINK})),
                Ok(json!({"presented":true})),
                Ok(json!({"released":true})),
                Ok(json!({"released":true})),
            ]);
            let reply = Job::new(&mut wire, 1, 0).action(
                &mut key,
                Some("choose_anonymous"),
                &json!({"context":42}),
            );
            assert_eq!(
                reply,
                json!({"status":"ok","value":{"status":"uploaded","url":CATBOX_LINK}})
            );
            assert_eq!(
                asked(&wire),
                [
                    "peersus.files/selected",
                    "peersus.http/authorize",
                    "peersus.http/request",
                    "peersus.http/present",
                    "peersus.http/release",
                    "peersus.files/release",
                ]
            );
            assert_eq!(wire.sent[0]["args"], json!({"context":42,"max_bytes":size}));
            assert_eq!(
                wire.sent[1]["args"],
                json!({"file":"chosen","origin":"https://catbox.moe","headers":[],"anonymous":true,
                    "result_origin":"https://files.catbox.moe"})
            );
            assert_eq!(
                wire.sent[2]["args"],
                json!({"authorization":"approved","method":"POST","path":"/user/api.php","response":"text",
                    "multipart":{"field":"fileToUpload","offset":0,"length":size,"fields":{"reqtype":"fileupload"}}})
            );
            assert_eq!(
                wire.sent[3]["args"],
                json!({"authorization":"approved","link":CATBOX_LINK})
            );
        }
    }
    #[test]
    fn a_link_the_destination_did_not_write_is_never_presented() {
        for answer in [
            json!({"status":200,"text":"https://catbox.moe/a1b2c3.png"}),
            json!({"status":200,"text":"https://files.catbox.moe/a1/b2"}),
            json!({"status":200,"text":"https://files.catbox.moe/"}),
            json!({"status":200,"text":"https://files.catbox.moe/a1b2c3.png?x=1"}),
            json!({"status":200,"text":"https://files.catbox.moe/a1b2c3.png#top"}),
            json!({"status":200,"text":"http://files.catbox.moe/a1b2c3.png"}),
            json!({"status":200,"text":"File too large"}),
            json!({"status":200,"text":"https://files.catbox.moe/a1b2c3.png\nhttps://files.catbox.moe/other.png"}),
            json!({"status":200,"text":"https://files.catbox.moe/a1b2c3/../x"}),
            json!({"status":200,"text":"https://files.catbox.moe/.."}),
            json!({"status":200,"text":"https://files.catbox.moe/."}),
            json!({"status":200}),
            json!({"status":200,"text":"https://files.catbox.moe/ünïcode"}),
        ] {
            let mut wire = Script {
                replies: VecDeque::from([
                    json!({"file":"chosen","name":"small.png","bytes":7}),
                    json!({"authorization":"approved"}),
                    answer,
                ]),
                sent: Vec::new(),
            };
            assert!(
                job(&mut wire).run_anonymous(42).is_err(),
                "a link this extension did not validate was shown"
            );
            assert_eq!(
                wire.sent.last().unwrap()["method"],
                "request",
                "an unvalidated link reached presentation"
            );
        }
        // What it does write is taken whole, with the line endings a text answer may carry.
        for text in [
            CATBOX_LINK,
            "https://files.catbox.moe/a1b2c3.png\n",
            "https://files.catbox.moe/a1b2c3.png\r\n",
        ] {
            assert_eq!(catbox_link(&json!({"text":text})).unwrap(), CATBOX_LINK);
        }
    }
    #[test]
    fn a_selection_outside_the_destination_limit_is_refused_before_authorization() {
        for (method, saved, limit) in [
            ("choose", Some(true), 250 * 1024 * 1024),
            ("choose_keyless", Some(false), 25 * 1024 * 1024),
            ("choose_anonymous", None, 6 * 1024 * 1024),
        ] {
            for (answer, code, release) in [
                (Err("platform-refused"), "unavailable", false),
                (
                    Ok(json!({"file":"chosen","name":"empty.txt","bytes":0})),
                    "unavailable",
                    true,
                ),
                (
                    Ok(json!({"file":"chosen","name":"large.txt","bytes":limit+1})),
                    "too-large",
                    true,
                ),
            ] {
                let mut answers = Vec::new();
                if let Some(saved) = saved {
                    answers.push(Ok(json!({"saved":saved})));
                    if saved {
                        answers.push(Ok(json!({"status":200})));
                    }
                }
                let prefix = answers.len();
                answers.push(answer);
                if release {
                    answers.push(Ok(json!({"released":true})));
                }
                let mut wire = told(answers);
                wire.refusal_message = SIZE_REFUSED;
                let reply = Job::new(&mut wire, 1, 0).action(
                    &mut Key::default(),
                    Some(method),
                    &json!({"context":42}),
                );
                assert_eq!(reply["code"], code);
                let expected: &[&str] = if release {
                    &["peersus.files/selected", "peersus.files/release"]
                } else {
                    &["peersus.files/selected"]
                };
                assert_eq!(asked(&wire)[prefix..], *expected);
                assert_eq!(
                    wire.sent[prefix]["args"],
                    json!({"context":42,"max_bytes":limit})
                );
            }
        }
    }
}
