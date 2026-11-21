//! Sandboxed provider protocol: the host owns all file bytes and credentials.
#![cfg(any(target_family = "wasm", feature = "native", test))]

mod host;
#[cfg(all(feature = "native", not(target_family = "wasm")))]
mod native;

use host::Wire;
use serde_json::{Value, json};
const PART: u64 = 3 * 1024 * 1024;
const MAX_FILE: u64 = 250 * 1024 * 1024;
/// The destination this extension was written for first: an account and its API key, files that burn after a day.
const ORIGIN: &str = "https://poo.wang";
/// The request header the provider's upload sessions are told by. Named to the application when asking to send.
const SESSION_HEADER: &str = "x-upload-session";
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
/// What this extension's settings say about the destination that needs no key, so that both destinations are visible where the
/// key of the first one is.
const CATBOX_WORDS: &str = "Catbox needs no key and no account: 'Upload selected file (Catbox, anonymous)' sends one file of up to 6 MiB to catbox.moe and shows the public link it returns on files.catbox.moe.";
/// How many calls are kept while the extension asks its first question; any past these are refused as busy.
const EARLY: usize = 4;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Destination {
    #[default]
    Both,
    Catbox,
    Account,
}

struct Job<'a, W> {
    destination: Destination,
    wire: &'a mut W,
    ticket: u64,
    next: u64,
    file: Option<Value>,
    authorization: Option<Value>,
    cancelled: bool,
    /// Calls that arrived while the extension was asking its first question, kept to be answered right after it. `None` once
    /// it has started: from then on a call that arrives in the middle of another is refused as busy.
    early: Option<Vec<Value>>,
    /// The settings line as it was known when this call began (nothing while the extension starts). A call that only asks for
    /// that line while this one waits, for a person or for the provider, is answered with it at once.
    shown: Value,
    /// The application's own words for its last refusal, for the settings line when the provider could not be asked.
    refused: Option<String>,
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
            early: None,
            shown: Value::Null,
            refused: None,
        }
    }
    /// The questions asked before any call is served. A press made in that moment is not lost and not called busy.
    fn starting(wire: &'a mut W) -> Self {
        Job {
            early: Some(Vec::new()),
            ..Job::new(wire, 0, 0)
        }
    }
}
impl<W: Wire> Job<'_, W> {
    /// A call that may wait for a person (a consent, the key's form, a large request): the longest deadline the application's
    /// file and HTTPS services allow.
    fn call(&mut self, service: &str, method: &str, args: Value) -> Result<Value, &'static str> {
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
    ) -> Result<Value, &'static str> {
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
                    if message["result"]["status"] != "ok" {
                        self.refused = message["result"]["message"].as_str().map(|said| {
                            said.chars().filter(|c| !c.is_control()).take(160).collect()
                        });
                        return Err(if message["result"]["code"] == "cancelled" {
                            "Upload cancelled"
                        } else if message["result"]["code"] == "denied" {
                            "Required platform permission has not been granted"
                        } else {
                            "The platform refused the operation"
                        });
                    }
                    return Ok(message["result"]["value"].clone());
                }
                Some("cancelled") if message["ticket"].as_u64() == Some(self.ticket) => {
                    self.cancelled = true;
                    self.wire.send(json!({"op":"cancel","id":id}))?;
                    return Err("Upload cancelled");
                }
                Some("call") => {
                    let line = !self.shown.is_null()
                        && message["service"] == "upload.file"
                        && message["method"] == "status";
                    match &mut self.early {
                        _ if line => self.wire.send(json!({"op":"reply","ticket":message["ticket"],"result":{"status":"ok","value":self.shown}}))?,
                        Some(early) if early.len() < EARLY => early.push(message),
                        _ => self.wire.send(json!({"op":"reply","ticket":message["ticket"],"result":{"status":"error","code":"busy","message":"This extension is busy with another action"}}))?,
                    }
                }
                _ => {}
            }
        }
    }
    /// One accepted request under the current authorization, whole: a destination that answers with text is read for text.
    fn accepted(&mut self, mut args: Value) -> Result<Value, &'static str> {
        args["authorization"] = self.authorization.clone().ok_or("Missing authorization")?;
        let response = self.call("peersus.http", "request", args)?;
        if response["status"]
            .as_u64()
            .is_none_or(|status| !(200..300).contains(&status))
        {
            return Err("The server rejected the upload; no retry was attempted");
        }
        Ok(response)
    }
    fn http(&mut self, args: Value) -> Result<Value, &'static str> {
        let response = self.accepted(args)?;
        Ok(response["json"].clone())
    }
    /// Whether the key the person just saved is accepted by the provider. Only the status decides: a success is `valid`, the two answers
    /// that mean "not this key" are `rejected`, and anything else (a busy or failing provider, a redirect, no network) is `unverified`:
    /// a key is never called good without a real success, and never lost for a failure that says nothing about it.
    fn check_key(&mut self) -> Result<&'static str, &'static str> {
        self.refused = None;
        let answer = match self.call(
            "peersus.http",
            "check",
            json!({"origin":ORIGIN,"path":"/api/me"}),
        ) {
            Ok(answer) => answer,
            Err("Upload cancelled") => return Err("Upload cancelled"),
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
    /// Ask the application whether it holds a usable key, and offer the upload accordingly: with no key it could only fail, so it is
    /// taken back until one is saved. When the application will not say (this extension is not allowed the network yet) the upload
    /// stays offered, and a press says what is missing. A verdict is of the key it was asked about: it goes when the key does.
    fn refresh(&mut self, key: &mut Key) {
        if self.destination == Destination::Catbox {
            self.withhold(&["upload", "configure"]);
            return;
        }
        let saved = self
            .call("peersus.http", "saved", json!({"origin":ORIGIN}))
            .ok()
            .and_then(|answer| answer["saved"].as_bool());
        if saved != key.saved {
            key.forget();
        }
        key.saved = saved;
        let withheld: &[&str] = match (self.destination, saved) {
            (Destination::Account, Some(false)) => &["upload", "upload_anonymous"],
            (Destination::Account, _) => &["upload_anonymous"],
            (_, Some(false)) => &["upload"],
            _ => &[],
        };
        self.withhold(withheld);
    }

    fn withhold(&mut self, withheld: &[&str]) {
        let _ = self.ask(
            "peersus.actions",
            "withhold",
            json!({"actions":withheld}),
            5_000,
        );
    }

    fn read_destination(&mut self) -> Destination {
        let config = self.ask("peersus.config", "get", json!({}), 5_000);
        match config.as_ref().ok().and_then(|c| c["settings"]["destination"].as_str()) {
            Some("Catbox") => Destination::Catbox,
            Some("poo.wang") => Destination::Account,
            _ => Destination::Both,
        }
    }

    fn settings_page(&self, key: &Key) -> Value {
        if self.destination == Destination::Catbox {
            json!({"k":"text","text":CATBOX_WORDS,"tone":"body"})
        } else {
            key.page()
        }
    }
    /// Ask the provider about the saved key, unless the application said there is none, and keep what it said for the settings line.
    fn verify(&mut self, key: &mut Key) -> Result<(), &'static str> {
        key.forget();
        if key.saved != Some(false) {
            let verdict = self.check_key()?;
            key.verdict = Some(verdict);
            // Why it could not be asked is the application's sentence or the provider's status, never anything of the key.
            key.why = self.refused.take().filter(|_| verdict == "unverified");
        }
        Ok(())
    }
    /// Save or remove the key through the application, then say what the provider thinks of what is saved now.
    fn configure(&mut self, key: &mut Key) -> Value {
        let value = match self.call("peersus.http", "configure", json!({"origin":ORIGIN})) {
            Ok(value) => value,
            Err(message) => return failure(message, "configuration-failed"),
        };
        self.refresh(key);
        // The key may be another one now, whatever was known of the one before.
        key.forget();
        match self.verify(key) {
            Ok(()) => configured(key.verdict.unwrap_or("none"), value),
            Err(message) => failure(message, "configuration-failed"),
        }
    }
    /// The settings line after what the person did there: opening it asks the application again, "Check now" also asks the provider,
    /// and anything else (its own clock) only draws what is known.
    fn status(&mut self, key: &mut Key, event: &Value) -> Value {
        if self.destination == Destination::Catbox {
            return json!({"status":"ok","value":self.settings_page(key)});
        }
        match (event["t"].as_str(), event["id"].as_str()) {
            (Some("open"), _) => self.refresh(key),
            (Some("press"), Some("check")) => {
                self.refresh(key);
                let _ = self.verify(key);
            }
            _ => {}
        }
        json!({"status":"ok","value":self.settings_page(key)})
    }
    /// One upload, beginning with the key: it is asked about first, so that a key the provider refuses is said before a file is
    /// prepared or the person is asked to allow anything.
    fn upload(&mut self, key: &mut Key, context: Option<u64>) -> Result<String, &'static str> {
        let context = context.ok_or("Choose one file on Send before uploading")?;
        self.refresh(key);
        if self.cancelled {
            return Err("Upload cancelled");
        }
        if key.saved == Some(false) {
            return Err("Save an API key in this extension's settings first");
        }
        self.verify(key)?;
        if key.verdict == Some("rejected") {
            return Err("The provider did not accept this API key");
        }
        self.run(context)
    }
    fn run(&mut self, context: u64) -> Result<String, &'static str> {
        if context == 0 {
            return Err("Choose one file on Send before uploading");
        }
        let selected = self.call(
            "peersus.files",
            "selected",
            json!({"context":context,"max_bytes":MAX_FILE}),
        )?;
        self.file = Some(selected["file"].clone());
        let size = selected["bytes"]
            .as_u64()
            .filter(|size| *size > 0 && *size <= MAX_FILE)
            .ok_or("Invalid selected size")?;
        let name = selected["name"].as_str().ok_or("Invalid selected name")?;
        // The header the provider's sessions use is named when asking, so the person is shown it; the application
        // sends no header it was not told of.
        let authorization = self.call(
            "peersus.http",
            "authorize",
            json!({"file":self.file,"origin":ORIGIN,"headers":[SESSION_HEADER]}),
        )?;
        self.authorization = Some(authorization["authorization"].clone());
        let result = if size <= PART {
            self.http(json!({"method":"POST","path":"/api/upload","bearer":true,"multipart":{"field":"files","offset":0,"length":size,"fields":{"burnMode":RETENTION,"showcase":"false"}}}))?
        } else {
            let session = self.http(json!({"method":"POST","path":"/api/upload/session","bearer":true,"json":{"name":name,"type":"","size":size,"burnMode":RETENTION,"showcase":false},"capture_secrets":["/token"]}))?;
            let part_size = session["partSize"]
                .as_u64()
                .filter(|size| (PART..=6 * 1024 * 1024).contains(size))
                .ok_or("Invalid server part size")?;
            let parts = session["parts"]
                .as_u64()
                .filter(|parts| *parts > 0 && *parts <= 45 && size.div_ceil(part_size) == *parts)
                .ok_or("Invalid server part count")?;
            let token = session["token"].clone();
            if token["secret"].as_str().is_none() {
                return Err("Missing opaque session authorization");
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
                    return Err("Invalid server part receipt");
                }
                receipts.push(receipt["receipt"].clone());
            }
            self.http(json!({"method":"POST","path":"/api/upload/session/complete","json":{"token":token,"receipts":receipts}}))?
        };
        let link = result_link(&result)?;
        let presented = self.call(
            "peersus.http",
            "present",
            json!({"authorization":self.authorization,"link":link}),
        )?;
        if presented["presented"] != true {
            return Err("The upload result could not be displayed");
        }
        Ok(link)
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
    /// One upload to the destination that needs no account: nothing of the person is saved, read or sent with it, so there is
    /// no key to ask about and no settings line to consult. The address and the address its link will be on are the two things
    /// the person is asked to allow, and the file is refused before that if it is larger than one request of it.
    fn upload_anonymous(&mut self, context: Option<u64>) -> Result<String, &'static str> {
        self.run_anonymous(context.ok_or("Choose one file on Send before uploading")?)
    }
    fn run_anonymous(&mut self, context: u64) -> Result<String, &'static str> {
        if context == 0 {
            return Err("Choose one file on Send before uploading");
        }
        // Asked for under this destination's own limit: a larger file is refused by the application while it is still only a
        // selection, before a snapshot, a question or a byte leaves. No part is ever sent as if it were the whole file.
        let selected = match self.call(
            "peersus.files",
            "selected",
            json!({"context":context,"max_bytes":CATBOX_MAX}),
        ) {
            Ok(selected) => selected,
            Err(message) => {
                return Err(if self.refused.as_deref() == Some(SIZE_REFUSED) {
                    "Catbox takes one file of up to 6 MiB; this application sends no more than that in one request"
                } else {
                    message
                });
            }
        };
        self.file = Some(selected["file"].clone());
        let size = selected["bytes"]
            .as_u64()
            .filter(|size| *size > 0 && *size <= CATBOX_MAX)
            .ok_or("Invalid selected size")?;
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
        let link = catbox_link(&answer)?;
        let presented = self.call(
            "peersus.http",
            "present",
            json!({"authorization":self.authorization,"link":link}),
        )?;
        if presented["presented"] != true {
            return Err("The upload result could not be displayed");
        }
        Ok(link)
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
    let mut host = host::Host;
    let mut key = Key::default();
    // Asked once before anything else, so that the upload is not offered while there is no key to send a file with.
    let (destination, mut next, early) = {
        let mut job = Job::starting(&mut host);
        job.destination = job.read_destination();
        job.refresh(&mut key);
        (job.destination, job.next, job.early.take().unwrap_or_default())
    };
    let mut early = early.into_iter();
    loop {
        let message = match early.next() {
            Some(message) => message,
            None => match host.receive() {
                Ok(message) => message,
                Err(_) => return 0,
            },
        };
        if message["type"] != "call" {
            continue;
        }
        let Some(ticket) = message["ticket"].as_u64() else {
            return 1;
        };
        let method = message["method"]
            .as_str()
            .filter(|_| message["service"] == "upload.file");
        let mut job = Job::new(&mut host, ticket, next);
        job.destination = destination;
        job.shown = job.settings_page(&key);
        let result = match method {
            Some("configure" | "choose") if destination == Destination::Catbox =>
                failure("Choose poo.wang in Upload destination before using this action", "destination-disabled"),
            Some("choose_anonymous") if destination == Destination::Account =>
                failure("Choose Catbox in Upload destination before using this action", "destination-disabled"),
            Some("configure") => job.configure(&mut key),
            Some("status") => job.status(&mut key, &message["args"]["event"]),
            Some("choose") => {
                let result = job.upload(&mut key, message["args"]["context"].as_u64());
                job.release();
                match result {
                    Ok(url) => json!({"status":"ok","value":{"status":"uploaded","url":url}}),
                    Err(message) => failure(message, "upload-failed"),
                }
            }
            Some("choose_anonymous") => {
                let result = job.upload_anonymous(message["args"]["context"].as_u64());
                job.release();
                match result {
                    Ok(url) => json!({"status":"ok","value":{"status":"uploaded","url":url}}),
                    Err(message) => failure(message, "upload-failed"),
                }
            }
            _ => {
                json!({"status":"error","code":"invalid-call","message":"Unsupported upload action"})
            }
        };
        next = job.next;
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

fn failure(message: &'static str, fallback: &str) -> Value {
    let code = match message {
        "Upload cancelled" => "cancelled",
        "Required platform permission has not been granted" => "denied",
        _ => fallback,
    };
    json!({"status":"error","code":code,"message":message})
}

/// The answer to saving a key, from the host's answer and the provider's verdict on it. A key the provider refuses is a failed action, so
/// that the person is told at once and not left believing it works; a provider that could not say is not a failure of the key. Nothing here
/// is the key or anything the provider sent: only the verdict, which is one of three fixed words.
fn configured(verdict: &'static str, mut value: Value) -> Value {
    if verdict == "rejected" {
        return json!({"status":"error","code":"key-rejected","message":"The provider did not accept this API key"});
    }
    value["key"] = json!(verdict);
    json!({"status":"ok","value":value})
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
    fn words(&self) -> String {
        match (self.saved, self.verdict) {
            (Some(false), _) => "No API key is saved. Save one to upload.",
            (None, _) => {
                "Allow this extension to use the network to see whether an API key is saved."
            }
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
        }
        children.push(json!({"k":"text","text":CATBOX_WORDS,"tone":"muted"}));
        json!({"k":"interval","id":"key","milliseconds":2000,"child":{"k":"column","children":children}})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

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
        assert_eq!(job.run(42).unwrap(), format!("{ORIGIN}/f/example"));
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
    fn session_upload_keeps_tokens_and_receipts_opaque_and_obeys_server_parts() {
        let mut wire = Script {
            replies: VecDeque::from([
                json!({"file":"chosen","name":"large.bin","bytes":PART+9}),
                json!({"authorization":"approved"}),
                json!({"status":200,"json":{"partSize":PART,"parts":2,"token":{"secret":"session"}}}),
                json!({"status":200,"json":{"success":true,"index":0,"receipt":{"secret":"first"}}}),
                json!({"status":200,"json":{"success":true,"index":1,"acceptedBytes":9,"receipt":{"secret":"last"}}}),
                json!({"status":200,"json":uploaded()}),
                json!({"presented":true}),
            ]),
            sent: Vec::new(),
        };
        assert!(job(&mut wire).run(42).is_ok());
        assert_eq!(
            wire.sent[3]["args"]["part"],
            json!({"offset":0,"length":PART})
        );
        assert_eq!(
            wire.sent[4]["args"]["part"],
            json!({"offset":PART,"length":9})
        );
        assert_eq!(
            wire.sent[3]["args"]["headers"]["x-upload-session"],
            json!({"secret":"session"})
        );
        assert_eq!(
            wire.sent[1]["args"],
            json!({"file":"chosen","origin":ORIGIN,"headers":["x-upload-session"]}),
            "the header is named when asking, so the person is shown it"
        );
        assert_eq!(
            wire.sent[5]["args"]["json"],
            json!({"token":{"secret":"session"},"receipts":[{"secret":"first"},{"secret":"last"}]})
        );
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
        assert_eq!(job(&mut wire).run(42), Err("Invalid server part receipt"));
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
        assert!(job(&mut wire).run(42).is_err());
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
            assert!(job(&mut wire).run(42).is_err());
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
        struct Refused {
            id: Value,
            code: &'static str,
        }
        impl Wire for Refused {
            fn send(&mut self, value: Value) -> Result<(), &'static str> {
                self.id = value["id"].clone();
                Ok(())
            }
            fn receive(&mut self) -> Result<Value, &'static str> {
                Ok(json!({"type":"result","id":self.id,"result":{
                    "status":"error","code":self.code,"message":"private host diagnostic"
                }}))
            }
        }
        for (code, expected) in [
            ("denied", "denied"),
            ("cancelled", "cancelled"),
            ("unavailable", "configuration-failed"),
        ] {
            let mut wire = Refused {
                id: Value::Null,
                code,
            };
            let mut job = Job::new(&mut wire, 1, 0);
            let message = job
                .call("peersus.http", "configure", json!({"origin":ORIGIN}))
                .expect_err("The refused operation must not succeed");
            let reply = failure(message, "configuration-failed");
            assert_eq!(reply["code"], expected);
            assert!(!reply.to_string().contains("private host diagnostic"));
            if code == "denied" {
                assert_eq!(failure(message, "upload-failed")["code"], "denied");
            }
        }
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
        let valid = configured("valid", json!({"configured":true}));
        assert_eq!(valid["status"], "ok");
        assert_eq!(valid["value"], json!({"configured":true,"key":"valid"}));
        let unverified = configured("unverified", json!({"configured":true}));
        assert_eq!(unverified["status"], "ok");
        assert_eq!(unverified["value"]["key"], "unverified");
        let rejected = configured("rejected", json!({"configured":true}));
        assert_eq!(rejected["status"], "error");
        assert_eq!(rejected["code"], "key-rejected");
    }
    /// Answers each call in turn: `Ok` with a value, or a refusal by its code.
    struct Told {
        answers: VecDeque<Result<Value, &'static str>>,
        sent: Vec<Value>,
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
                Err(code) => json!({"status":"error","code":code,"message":"private"}),
            };
            Ok(json!({"type":"result","id":id,"result":result}))
        }
    }
    fn told(answers: Vec<Result<Value, &'static str>>) -> Told {
        Told {
            answers: answers.into(),
            sent: Vec::new(),
        }
    }
    fn asked(wire: &Told) -> Vec<String> {
        wire.sent
            .iter()
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
    fn choosing_catbox_in_settings_hides_account_actions_without_reading_credentials() {
        let mut wire = told(vec![Ok(json!({"settings":{"destination":"Catbox"}})), Ok(json!({}))]);
        let mut job = Job::starting(&mut wire);
        job.destination = job.read_destination();
        let mut key = Key::default();
        job.refresh(&mut key);
        let page = job.status(&mut key, &json!({"t":"open"}));
        assert_eq!(page["value"]["text"], CATBOX_WORDS);
        assert_eq!(asked(&wire), ["peersus.config/get", "peersus.actions/withhold"]);
        assert_eq!(wire.sent[1]["args"]["actions"], json!(["upload", "configure"]));
    }

    #[test]
    fn the_upload_is_taken_back_only_while_the_application_says_no_key_is_saved() {
        for (answer, saved, withheld) in [
            (Ok(json!({"saved":false})), Some(false), json!(["upload"])),
            (Ok(json!({"saved":true})), Some(true), json!([])),
            // Not allowed the network yet: nothing is known, so the upload stays and a press says what is missing.
            (Err("denied"), None, json!([])),
        ] {
            let mut wire = told(vec![answer, Ok(json!({}))]);
            let mut key = Key {
                saved: Some(true),
                verdict: Some("valid"),
                why: None,
            };
            Job::new(&mut wire, 0, 0).refresh(&mut key);
            assert_eq!(key.saved, saved);
            // What the provider said was of the key that was there.
            assert_eq!(key.verdict, (saved == Some(true)).then_some("valid"));
            assert_eq!(
                asked(&wire),
                ["peersus.http/saved", "peersus.actions/withhold"]
            );
            assert_eq!(wire.sent[0]["args"], json!({"origin":ORIGIN}));
            assert_eq!(wire.sent[1]["args"], json!({"actions":withheld}));
            // Answered at once by the application, which has no raised limit for it: past 30 s the call would be refused.
            assert!(wire.sent[1]["timeout_ms"].as_u64().unwrap() <= 30_000);
        }
    }
    /// A wire on which a call from the application arrives before the answer to the extension's own first question.
    struct Arrives {
        call: Option<Value>,
        then: Told,
    }
    impl Wire for Arrives {
        fn send(&mut self, value: Value) -> Result<(), &'static str> {
            self.then.send(value)
        }
        fn receive(&mut self) -> Result<Value, &'static str> {
            match self.call.take() {
                Some(call) => Ok(call),
                None => self.then.receive(),
            }
        }
    }
    #[test]
    fn a_call_that_arrives_while_the_extension_starts_is_kept_and_one_in_the_middle_of_another_is_refused()
     {
        let call = json!({"type":"call","ticket":9,"service":"upload.file","method":"choose","args":{"context":42}});
        let mut wire = Arrives {
            call: Some(call.clone()),
            then: told(vec![Ok(json!({"saved":true})), Ok(json!({}))]),
        };
        let mut key = Key::default();
        let mut job = Job::starting(&mut wire);
        job.refresh(&mut key);
        // Kept whole for the loop that serves calls; nothing was said to the application about it.
        assert_eq!(job.early, Some(vec![call.clone()]));
        assert_eq!(key.saved, Some(true));
        assert!(wire.then.sent.iter().all(|sent| sent["op"] == "call"));

        // Once started, a second call in the middle of the first is told the extension is busy, by its own ticket.
        let mut wire = Arrives {
            call: Some(call),
            then: told(vec![Ok(json!({"saved":true})), Ok(json!({}))]),
        };
        let mut job = Job::new(&mut wire, 1, 0);
        job.refresh(&mut key);
        assert_eq!(job.early, None);
        let refusal = &wire.then.sent[1];
        assert_eq!(refusal["op"], "reply");
        assert_eq!(refusal["ticket"], 9);
        assert_eq!(refusal["result"]["code"], "busy");

        // The settings line draws itself every two seconds. While another call waits (for the person to write a key, or for
        // the provider), that is answered at once with the line as it was known, and is no error for the application to say.
        let line = json!({"type":"call","ticket":12,"service":"upload.file","method":"status","args":{"event":{"t":"tick","id":"key","at":1}}});
        let mut wire = Arrives {
            call: Some(line),
            then: told(vec![Ok(json!({"configured":true}))]),
        };
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
    fn saving_a_key_asks_the_provider_about_it_and_removing_one_asks_nobody() {
        let mut key = Key::default();
        let mut wire = told(vec![
            Ok(json!({"configured":true})),
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
        assert_eq!(refused["code"], "key-rejected");
        assert_eq!(key.verdict, Some("rejected"));
        assert_eq!(wire.sent[2]["args"], json!({"actions":[]}));

        let mut wire = told(vec![
            Ok(json!({"configured":false})),
            Ok(json!({"saved":false})),
            Ok(json!({})),
        ]);
        let removed = Job::new(&mut wire, 1, 0).configure(&mut key);
        assert_eq!(removed["value"], json!({"configured":false,"key":"none"}));
        assert_eq!(
            key,
            Key {
                saved: Some(false),
                verdict: None,
                why: None,
            }
        );
        assert_eq!(wire.sent[2]["args"], json!({"actions":["upload"]}));
        assert!(wire.answers.is_empty() && wire.sent.len() == 3);
    }
    #[test]
    fn an_upload_asks_about_the_key_before_it_touches_the_file() {
        // No key: said at once, and the upload is taken back.
        let mut key = Key::default();
        let mut wire = told(vec![Ok(json!({"saved":false})), Ok(json!({}))]);
        assert_eq!(
            Job::new(&mut wire, 1, 0).upload(&mut key, Some(42)),
            Err("Save an API key in this extension's settings first")
        );
        assert_eq!(wire.sent.len(), 2);
        // A key the provider refuses: said before the file is asked for.
        let mut wire = told(vec![
            Ok(json!({"saved":true})),
            Ok(json!({})),
            Ok(json!({"status":403})),
        ]);
        assert_eq!(
            Job::new(&mut wire, 1, 0).upload(&mut key, Some(42)),
            Err("The provider did not accept this API key")
        );
        assert_eq!(key.verdict, Some("rejected"));
        assert!(
            asked(&wire)
                .iter()
                .all(|call| !call.starts_with("peersus.files"))
        );
        // A provider that could not say does not stop an upload: the file is asked for next.
        let mut wire = told(vec![
            Ok(json!({"saved":true})),
            Ok(json!({})),
            Err("unavailable"),
            Err("unavailable"),
        ]);
        assert!(
            Job::new(&mut wire, 1, 0)
                .upload(&mut key, Some(42))
                .is_err()
        );
        assert_eq!(key.verdict, Some("unverified"));
        assert_eq!(asked(&wire).last().unwrap(), "peersus.files/selected");
        // No file chosen: nobody is asked anything.
        let mut wire = told(vec![]);
        assert!(Job::new(&mut wire, 1, 0).upload(&mut key, None).is_err());
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
        assert!(Key::default().words().contains("Allow this extension"));
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
    fn an_anonymous_upload_asks_for_no_key_and_sends_none() {
        let mut wire = Script {
            replies: VecDeque::from([
                json!({"file":"chosen","name":"small.png","bytes":7}),
                json!({"authorization":"approved"}),
                json!({"status":200,"text":CATBOX_LINK}),
                json!({"presented":true}),
                json!({"released":true}),
                json!({"released":true}),
            ]),
            sent: Vec::new(),
        };
        let mut job = job(&mut wire);
        assert_eq!(job.run_anonymous(42).unwrap(), CATBOX_LINK);
        job.release();
        // The size asked for is one request's worth: this destination takes no parts, so a larger file is refused by the
        // application before a snapshot or a question.
        assert_eq!(
            wire.sent[0]["args"],
            json!({"context":42,"max_bytes":CATBOX_MAX})
        );
        // Consent binds the file to the destination and names where its link will be, with no key and no header of its own.
        assert_eq!(
            wire.sent[1]["args"],
            json!({"file":"chosen","origin":CATBOX,"headers":[],"anonymous":true,"result_origin":CATBOX_LINKS})
        );
        // The file is one multipart part of the request, and no bearer is asked for anywhere: there is no key to send.
        assert_eq!(wire.sent[2]["method"], "request");
        assert_eq!(wire.sent[2]["args"]["path"], "/user/api.php");
        assert_eq!(wire.sent[2]["args"]["response"], "text");
        assert_eq!(
            wire.sent[2]["args"]["multipart"],
            json!({"field":"fileToUpload","offset":0,"length":7,"fields":{"reqtype":"fileupload"}})
        );
        assert!(wire.sent[2]["args"].get("bearer").is_none());
        assert_eq!(
            wire.sent[3]["args"],
            json!({"authorization":"approved","link":CATBOX_LINK})
        );
        assert!(
            wire.sent
                .iter()
                .all(|call| call["method"] != "check" && call["method"] != "saved"),
            "an anonymous destination is never asked about a key"
        );
        assert!(wire.replies.is_empty());
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
    fn a_file_larger_than_one_request_is_refused_before_anything_is_authorized_or_sent() {
        struct Sized;
        impl Wire for Sized {
            fn send(&mut self, _: Value) -> Result<(), &'static str> {
                Ok(())
            }
            fn receive(&mut self) -> Result<Value, &'static str> {
                Ok(json!({"type":"result","id":1,"result":{
                    "status":"error","code":"refused","message":SIZE_REFUSED
                }}))
            }
        }
        let mut wire = Sized;
        let mut job = Job::new(&mut wire, 1, 0);
        assert_eq!(
            job.run_anonymous(42),
            Err(
                "Catbox takes one file of up to 6 MiB; this application sends no more than that in one request"
            )
        );
        assert!(job.authorization.is_none());
        // A selection that failed for another reason is said as the application said it.
        struct Closed;
        impl Wire for Closed {
            fn send(&mut self, _: Value) -> Result<(), &'static str> {
                Ok(())
            }
            fn receive(&mut self) -> Result<Value, &'static str> {
                Ok(json!({"type":"result","id":1,"result":{
                    "status":"error","code":"refused","message":"Selected input is unavailable"
                }}))
            }
        }
        let mut wire = Closed;
        assert_eq!(
            Job::new(&mut wire, 1, 0).run_anonymous(42),
            Err("The platform refused the operation")
        );
        // No context means no file chosen, and nobody is asked anything.
        let mut wire = told(vec![]);
        assert!(Job::new(&mut wire, 1, 0).upload_anonymous(None).is_err());
        assert!(wire.sent.is_empty());
    }
}
