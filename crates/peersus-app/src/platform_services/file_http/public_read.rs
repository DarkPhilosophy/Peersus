//! Anonymous reads of public HTTPS resources. Separate from the selected-file upload authority.

use super::{Call, FileHttpServices, approved_origin, argument, within_origin};
use peersus_api::contributions::PublicRead;
use peersus_core::plugins::PluginId;
use serde_json::{Value, json};
use std::io::Read;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_DECODED_BYTES: usize = 4 * 1024 * 1024;
const CHUNK_BYTES: usize = 16 * 1024;
const MAX_AUTHORIZATIONS: usize = 4;
const TIMEOUT: Duration = Duration::from_secs(12);
/// Most bytes of public resources kept on this device for one extension; past it the oldest go first.
const CACHE_BYTES: u64 = 128 * 1024 * 1024;
/// Most resources kept for one extension, which also bounds the work of counting them.
const CACHE_FILES: usize = 16_384;

pub(super) struct Access {
    origin: reqwest::Url,
    root: String,
    id: String,
    body: Option<(String, Vec<u8>)>,
    last_fetch: Option<std::time::Instant>,
}

fn path(text: &str) -> Result<&str, &'static str> {
    if !PublicRead::valid_path(text) {
        return Err("Invalid public resource path");
    }
    Ok(text)
}

fn within_root(root: &str, candidate: &str) -> bool {
    candidate == root
        || candidate
            .strip_prefix(root)
            .is_some_and(|tail| tail.starts_with('/'))
}

fn public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_documentation()
                || a == 0
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 192 && (b == 0 && c == 0 || b == 88 && c == 99))
                || (a == 198 && matches!(b, 18 | 19)))
        }
        IpAddr::V6(ip) => {
            let [a, b, ..] = ip.segments();
            a & 0xe000 == 0x2000
                && !(a == 0x2001 && (b < 0x200 || b == 0x0db8))
                && a != 0x2002
                && !(a == 0x3fff && b < 0x1000)
        }
    }
}

fn origin(text: &str) -> Result<reqwest::Url, &'static str> {
    let url = approved_origin(text)?;
    if url.port_or_known_default() != Some(443) || url.domain().is_none() {
        return Err("A public HTTPS hostname is required");
    }
    Ok(url)
}

fn approval_path(data: &Path, caller: &peersus_core::plugins::PluginId) -> PathBuf {
    data.join("public-resource-approvals")
        .join(format!("{}.json", caller.as_str()))
}

/// Where one extension's downloaded public resources are kept on this device, one file per address.
fn cache_directory(data: &Path, caller: &PluginId) -> PathBuf {
    data.join("public-resource-cache").join(caller.as_str())
}

pub(super) fn cache_file(data: &Path, caller: &PluginId, url: &reqwest::Url) -> PathBuf {
    use sha2::Digest;
    cache_directory(data, caller).join(hex::encode(sha2::Sha256::digest(url.as_str().as_bytes())))
}

/// Everything the host keeps for one extension's public reads: its approval and its kept resources.
pub(super) fn forget(data: &Path, caller: &peersus_core::plugins::PluginId) {
    let _ = std::fs::remove_file(approval_path(data, caller));
    let _ = std::fs::remove_dir_all(cache_directory(data, caller));
}

/// The kept resources of one directory, oldest first: when each was kept, its size and where it is. Links and folders
/// are not resources and are left out.
fn kept(directory: &Path) -> Vec<(std::time::SystemTime, u64, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut kept: Vec<_> = entries
        .take(CACHE_FILES * 2)
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let metadata = entry.metadata().ok()?;
            if !metadata.is_file() || crate::private_fs::is_link(&metadata) {
                return None;
            }
            Some((metadata.modified().ok()?, metadata.len(), entry.path()))
        })
        .collect();
    kept.sort();
    kept
}

/// How many resources are kept for one extension and their bytes.
fn usage(directory: &Path) -> Value {
    let kept = kept(directory);
    json!({
        "resources": kept.len(),
        "bytes": kept.iter().map(|(_, size, _)| size).sum::<u64>(),
        "most_bytes": CACHE_BYTES,
    })
}

/// A kept resource, read whole, and how many seconds ago it was kept; `None` when there is none or it cannot be read. Its
/// age does not refuse it: the caller is told and decides whether to ask for a fresh one (`refresh`).
fn cached(file: &Path) -> Option<(Vec<u8>, u64)> {
    let handle = crate::private_fs::read_no_follow(file).ok()?;
    let age = handle
        .metadata()
        .ok()?
        .modified()
        .ok()?
        .elapsed()
        .map_or(0, |age| age.as_secs());
    let mut bytes = Vec::new();
    handle
        .take((MAX_DECODED_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= MAX_DECODED_BYTES).then_some((bytes, age))
}

/// Keep a downloaded resource, then remove the oldest until the extension is back within its bounds. Keeping is a
/// convenience: a failure loses nothing but the next read's speed.
fn keep(file: &Path, bytes: &[u8]) {
    let Some(directory) = file.parent() else {
        return;
    };
    if crate::private_fs::private_folders()
        .create(directory)
        .is_err()
        || peersus_net::atomic::write_private(file, bytes).is_err()
    {
        return;
    }
    let kept = kept(directory);
    let mut total: u64 = kept.iter().map(|(_, size, _)| size).sum();
    let mut count = kept.len();
    for (_, size, path) in kept {
        if total <= CACHE_BYTES && count <= CACHE_FILES {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(size);
            count -= 1;
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Approval {
    agreement: String,
    origin: String,
    root: String,
    allowed: bool,
    #[serde(default)]
    from_settings: bool,
}

fn read_approval(
    file: &Path,
    agreement: &str,
    origin: &reqwest::Url,
    root: &str,
) -> Option<Approval> {
    let mut bytes = Vec::new();
    crate::private_fs::read_no_follow(file)
        .ok()?
        .take(8193)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 8192 {
        return None;
    }
    let saved: Approval = serde_json::from_slice(&bytes).ok()?;
    (saved.agreement == agreement && saved.origin == origin.as_str() && saved.root == root)
        .then_some(saved)
}

pub(super) fn approved(
    data: &Path,
    caller: &PluginId,
    agreement: &str,
    scope: &PublicRead,
    legacy_enabled: bool,
) -> bool {
    origin(&scope.origin)
        .ok()
        .and_then(|origin| {
            read_approval(
                &approval_path(data, caller),
                agreement,
                &origin,
                &scope.root,
            )
        })
        .is_some_and(|saved| saved.allowed && (saved.from_settings || legacy_enabled))
}

pub(super) fn decide(
    data: &Path,
    caller: &PluginId,
    agreement: &str,
    scope: &PublicRead,
    allowed: bool,
) -> Result<(), &'static str> {
    scope.url().ok_or("Invalid public resource address")?;
    let record = Approval {
        agreement: agreement.to_owned(),
        origin: origin(&scope.origin)?.as_str().to_owned(),
        root: scope.root.clone(),
        allowed,
        from_settings: true,
    };
    let bytes = serde_json::to_vec(&record).map_err(|_| "Cannot save public resource approval")?;
    peersus_net::atomic::write_private(&approval_path(data, caller), &bytes)
        .map_err(|_| "Cannot save public resource approval")
}

impl FileHttpServices {
    pub(super) async fn public_read(
        &self,
        call: Call,
        generation: u64,
    ) -> Result<Value, &'static str> {
        match call.method.as_str() {
            "authorize-read" | "restore-read" => {
                let origin = origin(argument(&call.args, "origin")?)?;
                let root = path(argument(&call.args, "root")?)?.to_owned();
                {
                    let state = self.state()?;
                    state.current(&call.caller, generation)?;
                    if let Some(access) = state.resources.get(&call.caller)
                        && access.origin == origin
                        && access.root == root
                    {
                        return Ok(json!({"authorization":access.id}));
                    }
                }
                let durable = call.args["_accepted_agreement"]
                    .as_str()
                    .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                    .zip(self.resource_approvals.as_ref())
                    .map(|(digest, data)| (digest, approval_path(data, &call.caller)));
                let saved = durable
                    .as_ref()
                    .and_then(|(digest, file)| read_approval(file, digest, &origin, &root));
                if !saved.is_some_and(|record| record.allowed) {
                    return Err("Enable this public resource in the extension's settings");
                }
                let mut state = self.state()?;
                state.current(&call.caller, generation)?;
                if state.resources.len() >= MAX_AUTHORIZATIONS
                    && !state.resources.contains_key(&call.caller)
                {
                    return Err("Too many public resource authorizations");
                }
                let id = state.handle("resource-access")?;
                state.resources.insert(
                    call.caller,
                    Access {
                        origin,
                        root,
                        id: id.clone(),
                        body: None,
                        last_fetch: None,
                    },
                );
                Ok(json!({"authorization":id}))
            }
            "get-resource" => {
                let auth = argument(&call.args, "authorization")?;
                let request = path(argument(&call.args, "path")?)?;
                let url = {
                    let mut state = self.state()?;
                    state.current(&call.caller, generation)?;
                    let access = state
                        .resources
                        .get_mut(&call.caller)
                        .filter(|a| a.id == auth)
                        .ok_or("Unknown public resource authorization")?;
                    if !within_root(&access.root, request) {
                        return Err("The resource is outside the approved path");
                    }
                    within_origin(&access.origin, request)?
                };
                // A copy kept on this device, by its whole address, answers without the network. A call that keeps
                // nothing (`"keep": false`) neither reads nor writes one.
                let file = self
                    .resource_approvals
                    .as_deref()
                    .filter(|_| call.args["keep"].as_bool() != Some(false))
                    .map(|data| cache_file(data, &call.caller, &url));
                // `refresh` skips the copy and downloads (keeping the new one); `kept_only` answers from the copy or not
                // at all, never asking the network.
                let refresh = call.args["refresh"].as_bool() == Some(true);
                let kept_only = call.args["kept_only"].as_bool() == Some(true);
                let copy = match file.clone().filter(|_| !refresh) {
                    Some(file) => tokio::task::spawn_blocking(move || cached(&file))
                        .await
                        .ok()
                        .flatten(),
                    None => None,
                };
                let (body, age) = match copy {
                    Some((body, age)) => (body, Some(age)),
                    None if kept_only => return Err("The resource is not kept on this device"),
                    None => {
                        let slot = self
                            .resource_workers
                            .clone()
                            .try_acquire_owned()
                            .map_err(|_| "Too many public resource requests")?;
                        {
                            let mut state = self.state()?;
                            state.current(&call.caller, generation)?;
                            let access = state
                                .resources
                                .get_mut(&call.caller)
                                .filter(|a| a.id == auth)
                                .ok_or("Public resource authorization was released")?;
                            let now = std::time::Instant::now();
                            if access.last_fetch.is_some_and(|last| {
                                now.duration_since(last) < Duration::from_millis(240)
                            }) {
                                return Err("The public resource request was rate limited");
                            }
                            access.last_fetch = Some(now);
                        }
                        let body = tokio::time::timeout(TIMEOUT, self.download_resource(url, slot))
                            .await
                            .map_err(|_| "The public resource request timed out")??;
                        if let Some(file) = file {
                            let copy = body.clone();
                            let _ = tokio::task::spawn_blocking(move || keep(&file, &copy)).await;
                        }
                        (body, None)
                    }
                };
                let mut state = self.state()?;
                state.current(&call.caller, generation)?;
                let key = state.handle("resource")?;
                let access = state
                    .resources
                    .get_mut(&call.caller)
                    .filter(|a| a.id == auth)
                    .ok_or("Public resource authorization was released")?;
                let size = body.len();
                access.body = Some((key.clone(), body));
                Ok(json!({"resource":key,"bytes":size,"age":age}))
            }
            "read-resource" => {
                let key = argument(&call.args, "resource")?;
                let offset = call.args["offset"]
                    .as_u64()
                    .and_then(|n| usize::try_from(n).ok())
                    .ok_or("Invalid resource offset")?;
                let mut state = self.state()?;
                state.current(&call.caller, generation)?;
                let access = state
                    .resources
                    .get_mut(&call.caller)
                    .ok_or("Unknown public resource authorization")?;
                let (_, body) = access
                    .body
                    .as_ref()
                    .filter(|(id, _)| id == key)
                    .ok_or("Unknown public resource")?;
                let end = offset.saturating_add(CHUNK_BYTES).min(body.len());
                let bytes = body.get(offset..end).ok_or("Invalid resource offset")?;
                let result = json!({"hex":hex::encode(bytes),"end":end==body.len()});
                if end == body.len() {
                    access.body = None;
                }
                Ok(result)
            }
            "release-read" => {
                let auth = argument(&call.args, "authorization")?;
                let mut state = self.state()?;
                state.current(&call.caller, generation)?;
                if state
                    .resources
                    .get(&call.caller)
                    .is_some_and(|a| a.id == auth)
                {
                    state.resources.remove(&call.caller);
                }
                Ok(json!({"released":true}))
            }
            // What is kept on this device for the caller, and its removal. The caller's own copies only; no authorization
            // is needed to look at or remove them, and nothing is sent anywhere.
            "resource-cache" | "clear-resource-cache" => {
                self.state()?.current(&call.caller, generation)?;
                let Some(data) = self.resource_approvals.clone() else {
                    return Err("Public resources cannot be kept on this device");
                };
                let directory = cache_directory(&data, &call.caller);
                let clear = call.method == "clear-resource-cache";
                tokio::task::spawn_blocking(move || {
                    if clear {
                        let _ = std::fs::remove_dir_all(&directory);
                    }
                    usage(&directory)
                })
                .await
                .map_err(|_| "Public resources kept on this device could not be read")
            }
            _ => Err("Unsupported public resource method"),
        }
    }

    async fn download_resource(
        &self,
        url: reqwest::Url,
        slot: tokio::sync::OwnedSemaphorePermit,
    ) -> Result<Vec<u8>, &'static str> {
        let host = url.host_str().ok_or("Missing resource hostname")?;
        let addresses = tokio::net::lookup_host((host, 443))
            .await
            .map_err(|_| "Resource hostname did not resolve")?
            .take(32)
            .filter(|addr| public_address(addr.ip()))
            .collect::<Vec<_>>();
        if addresses.is_empty() {
            return Err("Public resources cannot use non-public addresses");
        }
        let client = self
            .https_client_builder()?
            .resolve_to_addrs(host, &addresses)
            .timeout(TIMEOUT)
            .user_agent(concat!(
                "Peersus-PublicResources/",
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .map_err(|_| "HTTPS transport unavailable")?;
        let mut response = client
            .get(url)
            .header(reqwest::header::ACCEPT_ENCODING, "gzip")
            .send()
            .await
            .map_err(|_| "The public resource could not be fetched")?;
        if !response.status().is_success() {
            return Err("The public resource was not available");
        }
        let gzip = response
            .headers()
            .get(reqwest::header::CONTENT_ENCODING)
            .is_some_and(|encoding| encoding.as_bytes().eq_ignore_ascii_case(b"gzip"));
        if response
            .content_length()
            .is_some_and(|n| n > MAX_BYTES as u64)
        {
            return Err("Public resource too large");
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "Public resource transfer failed")?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_BYTES {
                return Err("Public resource too large");
            }
            bytes.extend_from_slice(&chunk);
        }
        if gzip {
            tokio::task::spawn_blocking(move || {
                let _slot = slot;
                inflate(&bytes)
            })
            .await
            .map_err(|_| "Public resource decompression failed")?
        } else {
            Ok(bytes)
        }
    }
}

fn inflate(bytes: &[u8]) -> Result<Vec<u8>, &'static str> {
    let mut output = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .take((MAX_DECODED_BYTES + 1) as u64)
        .read_to_end(&mut output)
        .map_err(|_| "Public resource decompression failed")?;
    if output.len() > MAX_DECODED_BYTES {
        return Err("Public resource too large after decompression");
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_reads_stop_at_the_decompressed_size_limit() {
        use std::io::Write;
        let pack = |size| {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder.write_all(&vec![0; size]).unwrap();
            encoder.finish().unwrap()
        };
        assert_eq!(
            inflate(&pack(MAX_DECODED_BYTES)).unwrap(),
            vec![0; MAX_DECODED_BYTES]
        );
        assert_eq!(
            inflate(&pack(MAX_DECODED_BYTES + 1)),
            Err("Public resource too large after decompression")
        );
        assert!(inflate(b"not gzip").is_err());
    }

    #[test]
    fn public_reads_stay_inside_a_canonical_path_and_use_public_addresses() {
        assert!(within_root("/tiles", path("/tiles/9/11/12.pbf").unwrap()));
        assert!(!within_root("/tiles", "/tiles-other/a"));
        for bad in [
            "/tiles/../data",
            "//tiles/a",
            "/tiles/%2e/a",
            "/tiles/a?q=b",
        ] {
            assert!(path(bad).is_err());
        }
        for address in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "::1",
            "::ffff:8.8.8.8",
            "fd00::1",
            "2001:db8::1",
        ] {
            assert!(!public_address(address.parse().unwrap()));
        }
        assert!(public_address("8.8.8.8".parse().unwrap()));
        assert!(public_address("2606:4700::1111".parse().unwrap()));
    }
}
