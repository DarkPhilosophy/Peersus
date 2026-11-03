//! Portable upload core: pinned regular-file selection, private unlinked snapshot,
//! bounded HTTP replies and sequential session parts. No dialogs or credential store.
#![forbid(unsafe_code)]
use reqwest::{Client, Response, multipart::{Form, Part}, redirect::Policy};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{path::Path, time::Duration};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use url::Url;
use zeroize::Zeroizing;

pub const PART_SIZE: u64 = 3 * 1024 * 1024;
/// Operational ceiling in the server, not a promise of account entitlement.
pub const MAX_FILE: u64 = 250 * 1024 * 1024;
const MAX_RESPONSE: usize = 32 * 1024;
pub const RETENTION: &str = "Burn after 24 hours";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("choose a regular file between 1 byte and 250 MiB; your server account may have a lower limit")]
    Size,
    #[error("the selected file or private temporary snapshot could not be read or written")]
    File,
    #[error("the selected file changed; choose it again and review consent")]
    Changed,
    #[error("network request failed or timed out; upload may have completed; do not blindly retry")]
    Network,
    #[error("server rejected authentication; provision a valid non-owner machine credential")]
    Authentication,
    #[error("server account file-size limit reached (413)")]
    TooLarge,
    #[error("server rejected the file type (415); choose a supported, non-executable file")]
    FileType,
    #[error("server daily/rate limit reached (429); no retry was attempted")]
    RateLimited,
    #[error("server rejected upload (HTTP {0}); no retry was attempted")]
    Status(u16),
    #[error("server response exceeded the safety bound; upload may have completed")]
    ResponseSize,
    #[error("server returned an invalid result or unsafe link; upload may have completed")]
    Response,
    #[error("credential is empty, too long or contains invalid characters")]
    Credential,
}

/// Intentionally implements neither Debug nor Display.
pub struct Credential(Zeroizing<String>);
impl Credential {
    pub fn new(value: String) -> Result<Self, Error> {
        let value = Zeroizing::new(value);
        if value.is_empty() || value.len() > 8192 || !value.bytes().all(|b| (33..=126).contains(&b)) { return Err(Error::Credential); }
        Ok(Self(value))
    }
    #[cfg(feature = "desktop")]
    pub fn store(&self, entry: &keyring::Entry) -> Result<(), keyring::Error> { entry.set_password(&self.0) }
}

pub struct SelectedFile {
    source: tokio::fs::File,
    snapshot: tokio::fs::File,
    digest: [u8; 32],
    size: u64,
    name: String,
}
impl SelectedFile {
    pub async fn open(path: &Path) -> Result<Self, Error> {
        let mut options = tokio::fs::OpenOptions::new(); options.read(true);
        #[cfg(unix)]
        { options.custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32); }
        let mut source = options.open(path).await.map_err(|_| Error::File)?;
        let before = source.metadata().await.map_err(|_| Error::File)?;
        if !before.is_file() || before.len() == 0 || before.len() > MAX_FILE { return Err(Error::Size); }
        // tempfile() creates a private, unnamed/unlinked file, not a persistent plaintext copy.
        let temp = tokio::task::spawn_blocking(tempfile::tempfile).await.map_err(|_| Error::File)?.map_err(|_| Error::File)?;
        let mut snapshot = tokio::fs::File::from_std(temp);
        let mut hash = Sha256::new(); let mut total = 0u64; let mut buffer = vec![0; 64 * 1024];
        loop {
            let count = source.read(&mut buffer).await.map_err(|_| Error::File)?;
            if count == 0 { break; }
            total += count as u64; if total > MAX_FILE { return Err(Error::Changed); }
            hash.update(&buffer[..count]); snapshot.write_all(&buffer[..count]).await.map_err(|_| Error::File)?;
        }
        snapshot.flush().await.map_err(|_| Error::File)?;
        let after = source.metadata().await.map_err(|_| Error::File)?;
        if total != before.len() || before.len() != after.len() || before.modified().ok() != after.modified().ok() { return Err(Error::Changed); }
        let name = safe_name(path.file_name().and_then(|v| v.to_str()).unwrap_or("upload.bin"));
        let mut selected = Self { source, snapshot, digest: hash.finalize().into(), size: total, name };
        selected.check_unchanged().await?; Ok(selected)
    }
    pub fn name(&self) -> &str { &self.name }
    pub fn size(&self) -> u64 { self.size }
    async fn check_unchanged(&mut self) -> Result<(), Error> {
        self.source.seek(std::io::SeekFrom::Start(0)).await.map_err(|_| Error::File)?;
        let mut hash = Sha256::new(); let mut total = 0u64; let mut buffer = vec![0; 64 * 1024];
        loop {
            let count = self.source.read(&mut buffer).await.map_err(|_| Error::File)?;
            if count == 0 { break; }
            total += count as u64; if total > self.size { return Err(Error::Changed); } hash.update(&buffer[..count]);
        }
        if total != self.size || <[u8; 32]>::from(hash.finalize()) != self.digest { return Err(Error::Changed); } Ok(())
    }
    async fn part(&mut self, size: usize) -> Result<Vec<u8>, Error> {
        let mut part = vec![0; size]; self.snapshot.read_exact(&mut part).await.map_err(|_| Error::File)?; Ok(part)
    }
}
fn safe_name(name: &str) -> String {
    let cleaned: String = name.chars().take(120).map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' }).collect();
    if cleaned.is_empty() || cleaned.chars().all(|c| c == '.') { "upload.bin".into() } else { cleaned }
}

pub struct Uploader { client: Client, endpoint: Url }
impl Uploader {
    pub fn production() -> Result<Self, Error> {
        Self::with_endpoint(Url::parse("https://poo.wang/api/upload").map_err(|_| Error::Response)?, true)
    }
    fn with_endpoint(endpoint: Url, https_only: bool) -> Result<Self, Error> {
        let client = Client::builder().https_only(https_only).redirect(Policy::none())
            .retry(reqwest::retry::never()).no_proxy().connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(120)).build().map_err(|_| Error::Network)?;
        Ok(Self { client, endpoint })
    }
    fn route(&self, suffix: &str) -> Result<Url, Error> {
        Url::parse(&format!("{}{suffix}", self.endpoint.as_str())).map_err(|_| Error::Response)
    }
    /// Each write is attempted once; dropping cancels local I/O, not committed server data.
    pub async fn upload(&self, mut file: SelectedFile, credential: Credential) -> Result<String, Error> {
        file.check_unchanged().await?;
        file.snapshot.seek(std::io::SeekFrom::Start(0)).await.map_err(|_| Error::File)?;
        if file.size <= PART_SIZE {
            let bytes = file.part(file.size as usize).await?;
            // Unspecified MIME avoids a false extension/MIME mismatch in the server.
            let part = Part::bytes(bytes).file_name(file.name);
            let form = Form::new().part("files", part).text("burnMode", RETENTION).text("showcase", "false");
            let response = self.client.post(self.endpoint.clone()).bearer_auth(&*credential.0).multipart(form).send().await.map_err(|_| Error::Network)?;
            return parse_result(&bounded(response).await?);
        }
        let response = self.client.post(self.route("/session")?).bearer_auth(&*credential.0)
            .json(&serde_json::json!({"name":file.name,"type":"","size":file.size,"burnMode":RETENTION,"showcase":false}))
            .send().await.map_err(|_| Error::Network)?;
        let session: Session = serde_json::from_slice(&bounded(response).await?).map_err(|_| Error::Response)?;
        // Server increases partSize for large files to stay within its 45-part limit.
        // Bound every allocation even if an authenticated response is hostile.
        if session.part_size < PART_SIZE || session.part_size > 6 * 1024 * 1024 || session.parts == 0 || session.parts > 45
            || file.size.div_ceil(session.part_size) != session.parts || session.token.is_empty() || session.token.len() > 16384
            || !session.token.bytes().all(|b| (33..=126).contains(&b)) { return Err(Error::Response); }
        let session_secret = Zeroizing::new(session.token);
        let mut receipts = Vec::with_capacity(session.parts as usize);
        let mut remaining = file.size;
        for index in 0..session.parts {
            let count = remaining.min(session.part_size);
            let bytes = file.part(count as usize).await?;
            let mut route = self.route("/session/part")?; route.query_pairs_mut().append_pair("index", &index.to_string());
            let response = self.client.put(route).header("x-upload-session", &*session_secret)
                .header("content-type", "application/octet-stream").body(bytes).send().await.map_err(|_| Error::Network)?;
            let part: PartReceipt = serde_json::from_slice(&bounded(response).await?).map_err(|_| Error::Response)?;
            if !part.success || part.index != index || part.receipt.is_empty() || part.receipt.len() > 4096 { return Err(Error::Response); }
            receipts.push(Zeroizing::new(part.receipt)); remaining -= count;
        }
        // Recheck before completion so a changed selection never creates a published record.
        file.check_unchanged().await?;
        let receipt_refs: Vec<&str> = receipts.iter().map(|r| r.as_str()).collect();
        let response = self.client.post(self.route("/session/complete")?)
            .json(&serde_json::json!({"token":session_secret.as_str(),"receipts":receipt_refs}))
            .send().await.map_err(|_| Error::Network)?;
        parse_result(&bounded(response).await?)
    }
}
#[derive(Deserialize)]
struct Session { token: String, #[serde(rename="partSize")] part_size: u64, parts: u64 }
#[derive(Deserialize)]
struct PartReceipt { success: bool, index: u64, receipt: String }
async fn bounded(mut response: Response) -> Result<Vec<u8>, Error> {
    let status = response.status();
    if !status.is_success() { return Err(match status.as_u16() {
        401 | 403 => Error::Authentication, 413 => Error::TooLarge, 415 => Error::FileType,
        429 => Error::RateLimited, code => Error::Status(code),
    }); }
    if response.content_length().is_some_and(|n| n > MAX_RESPONSE as u64) { return Err(Error::ResponseSize); }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| Error::Network)? {
        if chunk.len() > MAX_RESPONSE.saturating_sub(bytes.len()) { return Err(Error::ResponseSize); }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
#[derive(Deserialize)]
struct UploadResponse { success: bool, files: Vec<Uploaded> }
#[derive(Deserialize)]
struct Uploaded { id: String, url: String }
fn parse_result(bytes: &[u8]) -> Result<String, Error> {
    let result: UploadResponse = serde_json::from_slice(bytes).map_err(|_| Error::Response)?;
    if !result.success || result.files.len() != 1 { return Err(Error::Response); }
    let file = &result.files[0];
    if file.id.is_empty() || file.id.len() > 128 || !file.id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')) { return Err(Error::Response); }
    let expected = format!("https://poo.wang/f/{}", file.id);
    if file.url != expected { return Err(Error::Response); } Ok(expected)
}
#[cfg(all(feature = "desktop", any(target_os = "linux", target_os = "macos")))]
pub mod desktop;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod session_tests;
