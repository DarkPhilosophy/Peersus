//! Sandboxed provider protocol: the host owns all file bytes and credentials.
#![cfg(any(target_family = "wasm", test))]
use serde_json::{Value, json};
#[cfg(target_family = "wasm")]
const LIMIT: usize = 48 * 1024;
const PART: u64 = 3 * 1024 * 1024;
const MAX_FILE: u64 = 250 * 1024 * 1024;
const ORIGIN: &str = "https://poo.wang";
const RETENTION: &str = "Burn after 24 hours";

#[cfg(target_family = "wasm")]
#[link(wasm_import_module = "peersus_v1")]
unsafe extern "C" {
    // Pointers and lengths are `usize`: 32 bits in a wasm32 module, 64 in a wasm64 one. The module's own memory decides.
    #[link_name = "receive"]
    fn receive_bytes(buffer: *mut u8, capacity: usize) -> i32;
    #[link_name = "send"]
    fn send_bytes(buffer: *const u8, length: usize) -> i32;
}
trait Wire {
    fn receive(&mut self) -> Result<Value, &'static str>;
    fn send(&mut self, value: Value) -> Result<(), &'static str>;
}
#[cfg(target_family = "wasm")]
struct Host;
#[cfg(target_family = "wasm")]
impl Wire for Host {
    fn receive(&mut self) -> Result<Value, &'static str> {
        let mut buffer = vec![0u8; LIMIT];
        let count = unsafe { receive_bytes(buffer.as_mut_ptr(), buffer.len()) };
        if count <= 0 || count as usize > LIMIT {
            return Err("The host disconnected");
        }
        serde_json::from_slice(&buffer[..count as usize]).map_err(|_| "Invalid host response")
    }
    fn send(&mut self, value: Value) -> Result<(), &'static str> {
        let bytes = serde_json::to_vec(&value).map_err(|_| "Invalid extension message")?;
        if bytes.len() > LIMIT {
            return Err("Extension message exceeds its bound");
        }
        if unsafe { send_bytes(bytes.as_ptr(), bytes.len()) } != 0 {
            return Err("The host disconnected");
        }
        Ok(())
    }
}
struct Job<'a, W> {
    wire: &'a mut W,
    ticket: u64,
    next: u64,
    file: Option<Value>,
    authorization: Option<Value>,
    cancelled: bool,
}
impl<W: Wire> Job<'_, W> {
    fn call(&mut self, service: &str, method: &str, args: Value) -> Result<Value, &'static str> {
        self.next = self
            .next
            .checked_add(1)
            .ok_or("Call identifiers exhausted")?;
        let id = self.next;
        self.wire.send(json!({"op":"call","id":id,"provider":"host","service":service,"version":1,"method":method,"args":args,"timeout_ms":600000}))?;
        loop {
            let message = self.wire.receive()?;
            match message["type"].as_str() {
                Some("result") if message["id"].as_u64() == Some(id) => {
                    if message["result"]["status"] != "ok" {
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
                Some("call") => self.wire.send(json!({"op":"reply","ticket":message["ticket"],"result":{"status":"error","code":"busy","message":"An upload is already active"}}))?,
                _ => {}
            }
        }
    }
    fn http(&mut self, mut args: Value) -> Result<Value, &'static str> {
        args["authorization"] = self.authorization.clone().ok_or("Missing authorization")?;
        let response = self.call("peersus.http", "request", args)?;
        if response["status"]
            .as_u64()
            .is_none_or(|status| !(200..300).contains(&status))
        {
            return Err("The server rejected the upload; no retry was attempted");
        }
        Ok(response["json"].clone())
    }
    /// Whether the key the person just saved is accepted by the provider. Only the status decides: a success is `valid`, the two answers
    /// that mean "not this key" are `rejected`, and anything else (a busy or failing provider, a redirect, no network) is `unverified`:
    /// a key is never called good without a real success, and never lost for a failure that says nothing about it.
    fn check_key(&mut self) -> Result<&'static str, &'static str> {
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
            _ => "unverified",
        })
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
        let authorization = self.call(
            "peersus.http",
            "authorize",
            json!({"file":self.file,"origin":ORIGIN}),
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
                let receipt = self.http(json!({"method":"PUT","path":format!("/api/upload/session/part?index={index}"),"headers":{"content-type":"application/octet-stream","x-upload-session":token},"part":{"offset":offset,"length":(size-offset).min(part_size)},"capture_secrets":["/receipt"]}))?;
                if receipt["success"] != true
                    || receipt["index"].as_u64() != Some(index)
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
#[cfg(target_family = "wasm")]
#[unsafe(no_mangle)]
pub extern "C" fn peersus_main() -> i32 {
    let mut host = Host;
    let mut next = 0;
    loop {
        let Ok(message) = host.receive() else {
            return 0;
        };
        if message["type"] != "call" {
            continue;
        }
        let Some(ticket) = message["ticket"].as_u64() else {
            return 1;
        };
        if message["service"] == "upload.file" && message["method"] == "configure" {
            let mut job = Job {
                wire: &mut host,
                ticket,
                next,
                file: None,
                authorization: None,
                cancelled: false,
            };
            let result = match job
                .call("peersus.http", "configure", json!({"origin":ORIGIN}))
                .and_then(|value| job.check_key().map(|key| configured(key, value)))
            {
                Ok(result) => result,
                Err(message) => failure(message, "configuration-failed"),
            };
            next = job.next;
            if !job.cancelled
                && job
                    .wire
                    .send(json!({"op":"reply","ticket":ticket,"result":result}))
                    .is_err()
            {
                return 1;
            }
            continue;
        }
        if message["service"] != "upload.file" || message["method"] != "choose" {
            if host.send(json!({"op":"reply","ticket":ticket,"result":{"status":"error","code":"invalid-call","message":"Unsupported upload action"}})).is_err() { return 1; }
            continue;
        }
        let mut job = Job {
            wire: &mut host,
            ticket,
            next,
            file: None,
            authorization: None,
            cancelled: false,
        };
        let result = message["args"]["context"]
            .as_u64()
            .ok_or("Choose one file on Send before uploading")
            .and_then(|context| job.run(context));
        job.release();
        next = job.next;
        if job.cancelled {
            continue;
        }
        let result = match result {
            Ok(url) => json!({"status":"ok","value":{"status":"uploaded","url":url}}),
            Err(message) => failure(message, "upload-failed"),
        };
        if job
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
        Job {
            wire,
            ticket: 1,
            next: 0,
            file: None,
            authorization: None,
            cancelled: false,
        }
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
                json!({"status":200,"json":{"success":true,"index":1,"receipt":{"secret":"last"}}}),
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
            wire.sent[5]["args"]["json"],
            json!({"token":{"secret":"session"},"receipts":[{"secret":"first"},{"secret":"last"}]})
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
            let mut job = Job {
                wire: &mut wire,
                ticket: 1,
                next: 0,
                file: None,
                authorization: None,
                cancelled: false,
            };
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
        let mut job = Job {
            wire: &mut wire,
            ticket: 1,
            next: 0,
            file: None,
            authorization: None,
            cancelled: false,
        };
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
}
