//! Updates of the application itself: what a signed release says, and what it proves.
//!
//! THE TRUST ROOT is its own Ed25519 public key, [`UPDATE_PUBLIC_KEY`], apart from the extension
//! store's. The two never stand in for each other: whoever can approve an extension cannot replace
//! the application, and the signed bytes begin with [`SIGNATURE_CONTEXT`], so a signature made for
//! one kind of document is not a signature of the other even if one key were ever used for both.
//! While the key is `None` updates are not set up and everything here refuses; there is no fallback.
//!
//! WHAT IS SIGNED is one small document per channel: the release's version and build number, and for
//! each artifact its target, kind, file name, size and SHA-256. Where the bytes came from is not
//! trusted and not signed: a release listing only says where to look.
//!
//! ROLLBACK. A release is offered only when it is newer than the running build. The newest release
//! ever verified is kept on disk with the digest of its document: an older one is refused, and so is
//! a different document for the same version and build (two releases under one identity).
//!
//! This module verifies and decides. It never fetches, installs or restarts: the caller downloads
//! between a `begin_*` and its `finish_*`, and a platform installer applies what was verified.
use crate::{catalog::Refusal, store_runtime::StoreFetcher};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use peersus_api::{name, package::Version};
use peersus_core::model::{UpdateState, UpdateStatus};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

/// The public key releases of the application are verified with, or `None` until one is chosen.
/// Set with the output of `cargo xtask update-keygen`; a public key is not a secret.
#[cfg(not(feature = "private-store"))]
pub const UPDATE_PUBLIC_KEY: Option<[u8; 32]> = None;
/// The development key: what private development builds verify the `test` channel with. Its seed
/// never signs anything a product build accepts, because a product build does not hold this key.
#[cfg(feature = "private-store")]
pub const UPDATE_PUBLIC_KEY: Option<[u8; 32]> = Some([
    78, 72, 236, 243, 236, 148, 162, 194, 34, 66, 99, 121, 175, 134, 25, 229, 198, 112, 33, 147,
    24, 145, 16, 45, 224, 247, 82, 209, 40, 170, 162, 210,
]);

/// The channel this build follows: releases are published under [`release_tag`] of it. A private
/// development build follows `test` and nothing else; the two never read each other's releases,
/// and the channel is inside what is signed.
pub const CHANNEL: &str = if cfg!(feature = "private-store") {
    "test"
} else {
    "stable"
};

/// What the signature covers before the document: the purpose of the signature, in the bytes.
pub const SIGNATURE_CONTEXT: &[u8] = b"peersus-app-update-v1\n";
pub const MANIFEST_FILE: &str = "update.json";
pub const SIGNATURE_FILE: &str = "update.sig";
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
/// Largest artifact a release may pin: what the fetcher will carry in one piece.
pub const MAX_ARTIFACT_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_ARTIFACTS: usize = 32;
/// Most assets a release listing may name before it is refused unread.
const MAX_ASSETS: usize = 64;
const MAX_NOTES_CHARS: usize = 2000;

const NOT_SET_UP: Refusal = Refusal("Application updates are not set up in this build");
const UNREADABLE: Refusal = Refusal("The update could not be checked");
const LISTING: Refusal = Refusal("The update listing could not be checked");

/// One build of the application: its version, then a number that rises with every build released
/// under that version. Ordered by version first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Build {
    pub version: Version,
    pub number: u64,
}

impl Build {
    /// The build this program is: the workspace version, and the number it was compiled with
    /// (`PEERSUS_BUILD_NUMBER`, 0 when none was given).
    pub fn running() -> Self {
        Self {
            version: Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or(Version(0, 0, 0)),
            number: decimal(option_env!("PEERSUS_BUILD_NUMBER")),
        }
    }
}

impl std::fmt::Display for Build {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.number == 0 {
            write!(formatter, "{}", self.version)
        } else {
            write!(formatter, "{} (build {})", self.version, self.number)
        }
    }
}

/// A build number given at compile time. Anything that is not a plain decimal number reads as 0,
/// which is never newer than a release: a mistyped value cannot make a build refuse its updates.
fn decimal(text: Option<&str>) -> u64 {
    text.filter(|text| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|text| text.parse().ok())
        .unwrap_or(0)
}

/// How an artifact is applied. A closed set: a kind this version does not know is refused whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// A compressed folder of the application's programs, replacing the folder it runs from.
    Archive,
    /// One self-contained file that is the application, replacing that file.
    Image,
    /// A package the operating system's installer applies.
    Package,
}

/// What this installation can apply, as its platform said. No platform is named in this module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub platform: &'static str,
    pub architecture: &'static str,
    pub kind: Kind,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub platform: String,
    /// A processor architecture, or `any` for an artifact that carries every one it supports.
    pub architecture: String,
    pub kind: Kind,
    /// File name of the artifact where it is served. Never a path.
    pub file: String,
    /// Lower-case hex SHA-256 of the file.
    pub sha256: String,
    pub size: u64,
}

impl Artifact {
    fn valid(&self) -> bool {
        short_word(&self.platform)
            && short_word(&self.architecture)
            && !self.file.is_empty()
            && self.file.len() <= 128
            && !self.file.starts_with('.')
            && name::plain(&self.file)
            && self.sha256.len() == 64
            && self
                .sha256
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
            && (1..=MAX_ARTIFACT_BYTES).contains(&self.size)
    }
}

fn short_word(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn channel_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

#[derive(Deserialize)]
struct Schema {
    schema: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    #[allow(dead_code)]
    schema: u32,
    channel: String,
    version: String,
    build: u64,
    artifacts: Vec<Artifact>,
    #[serde(default)]
    notes: String,
}

/// A release whose signature checked out, and nothing of it that was not signed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub build: Build,
    pub notes: String,
    artifacts: Vec<Artifact>,
    /// SHA-256 of the signed document: what tells two documents for one build apart.
    digest: [u8; 32],
}

impl Release {
    /// The artifact for `target`: its exact architecture first, otherwise one for `any`,
    /// whatever order the release listed them in.
    pub fn artifact(&self, target: &Target) -> Option<&Artifact> {
        let fits = |architecture: &str| {
            self.artifacts.iter().find(|artifact| {
                artifact.platform == target.platform
                    && artifact.kind == target.kind
                    && artifact.architecture == architecture
            })
        };
        fits(target.architecture).or_else(|| fits("any"))
    }
}

/// The key to verify with, from a candidate (production passes [`UPDATE_PUBLIC_KEY`]).
pub fn trust_root(candidate: Option<[u8; 32]>) -> Result<VerifyingKey, Refusal> {
    VerifyingKey::from_bytes(&candidate.ok_or(NOT_SET_UP)?)
        .map_err(|_| Refusal("The application update key is not valid"))
}

/// Check the signature over [`SIGNATURE_CONTEXT`] and the document, then the document itself.
/// Nothing is parsed before the signature holds, except nothing: the signature comes first.
pub fn verify(
    key: &VerifyingKey,
    manifest: &[u8],
    signature: &[u8],
    channel: &str,
) -> Result<Release, Refusal> {
    if manifest.len() > MAX_MANIFEST_BYTES {
        return Err(UNREADABLE);
    }
    let signature = Signature::from_slice(signature)
        .map_err(|_| Refusal("The update is not signed by this application's publisher"))?;
    let mut signed = Vec::with_capacity(SIGNATURE_CONTEXT.len() + manifest.len());
    signed.extend_from_slice(SIGNATURE_CONTEXT);
    signed.extend_from_slice(manifest);
    key.verify(&signed, &signature)
        .map_err(|_| Refusal("The update is not signed by this application's publisher"))?;
    // Authentic from here on. A newer layout is said to be newer, not reported as damage.
    let schema: Schema = serde_json::from_slice(manifest).map_err(|_| UNREADABLE)?;
    if schema.schema != 1 {
        return Err(Refusal(
            "The update is described in a way this version does not read",
        ));
    }
    let document: Document = serde_json::from_slice(manifest).map_err(|_| UNREADABLE)?;
    if !channel_name(&document.channel) || document.channel != channel {
        return Err(Refusal("The update is for another release channel"));
    }
    let version = Version::parse(&document.version).ok_or(UNREADABLE)?;
    if document.artifacts.len() > MAX_ARTIFACTS
        || !(document.notes.is_empty()
            || document.notes.chars().count() <= MAX_NOTES_CHARS
                && !document
                    .notes
                    .chars()
                    .any(|character| character.is_control() && character != '\n'))
    {
        return Err(UNREADABLE);
    }
    let mut targets = BTreeSet::new();
    let mut files = BTreeSet::new();
    for artifact in &document.artifacts {
        if !artifact.valid()
            || !targets.insert((
                artifact.platform.as_str(),
                artifact.architecture.as_str(),
                artifact.kind,
            ))
            || !files.insert(artifact.file.as_str())
        {
            return Err(UNREADABLE);
        }
    }
    Ok(Release {
        build: Build {
            version,
            number: document.build,
        },
        notes: document.notes,
        artifacts: document.artifacts,
        digest: Sha256::digest(manifest).into(),
    })
}

/// Whether `bytes` are exactly the artifact the signed release pinned.
pub fn artifact_matches(artifact: &Artifact, bytes: &[u8]) -> Result<(), Refusal> {
    if bytes.len() as u64 != artifact.size || hex::encode(Sha256::digest(bytes)) != artifact.sha256
    {
        return Err(Refusal(
            "The downloaded update is not the one that was published",
        ));
    }
    Ok(())
}

/// The newest release ever verified, kept so that an older one is not accepted after a restart.
struct Floor {
    path: PathBuf,
}

impl Floor {
    const DAMAGED: Refusal = Refusal("The saved update state is damaged");

    /// Absent is a first run; anything unreadable is refused, never read as "no floor".
    fn read(&self) -> Result<Option<(Build, [u8; 32])>, Refusal> {
        let text = match crate::store_runtime::read_bounded(&self.path, 256) {
            Ok(bytes) => String::from_utf8(bytes).map_err(|_| Self::DAMAGED)?,
            Err(_) if !self.path.exists() => return Ok(None),
            Err(_) => return Err(Refusal("The saved update state could not be read")),
        };
        let mut parts = text.trim_end_matches('\n').split(' ');
        let (Some(version), Some(number), Some(digest), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(Self::DAMAGED);
        };
        let version = Version::parse(version).ok_or(Self::DAMAGED)?;
        if number.is_empty() || number.len() > 20 || !number.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Self::DAMAGED);
        }
        let number = number.parse().map_err(|_| Self::DAMAGED)?;
        let mut held = [0u8; 32];
        hex::decode_to_slice(digest, &mut held).map_err(|_| Self::DAMAGED)?;
        Ok(Some((Build { version, number }, held)))
    }

    /// Admit `release` against what is kept, and keep it when it is newer.
    fn admit(&self, release: &Release) -> Result<(), Refusal> {
        match self.read()? {
            Some((held, _)) if release.build < held => {
                return Err(Refusal("The update offered is older than one already seen"));
            }
            Some((held, digest)) if release.build == held => {
                return if digest == release.digest {
                    Ok(())
                } else {
                    Err(Refusal(
                        "The update offered is not the release already seen under that version",
                    ))
                };
            }
            _ => {}
        }
        let line = format!(
            "{} {} {}\n",
            release.build.version,
            release.build.number,
            hex::encode(release.digest)
        );
        peersus_net::atomic::write(&self.path, line.as_bytes())
            .map_err(|_| Refusal("The update state could not be saved"))
    }
}

#[derive(Deserialize)]
struct Listing {
    draft: bool,
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    id: u64,
    name: String,
    size: u64,
    state: String,
}

/// What a channel's release listing led to: the signed document, its signature, and where each
/// file of the release can be asked for. All of it untrusted until [`UpdateService::finish_check`].
pub struct Found {
    pub manifest: Vec<u8>,
    pub signature: Vec<u8>,
    /// Asset identifiers by file name: locators, never authority.
    pub assets: BTreeMap<String, u64>,
}

/// The name the fetcher is asked for a channel's release listing by.
pub fn listing_name(channel: &str) -> String {
    format!("app-release-{channel}")
}

/// The release tag a channel is published under.
pub fn release_tag(channel: &str) -> String {
    format!("app/{channel}")
}

/// Ask where the channel's release is and fetch its signed document. `None` when the channel has
/// published nothing: that is not a failure.
pub async fn discover(fetcher: &dyn StoreFetcher, channel: &str) -> Result<Option<Found>, Refusal> {
    if !channel_name(channel) {
        return Err(LISTING);
    }
    let bytes = fetcher.fetch(listing_name(channel), 1024 * 1024).await?;
    let listing: Option<Listing> = serde_json::from_slice(&bytes).map_err(|_| LISTING)?;
    let Some(listing) = listing else {
        return Ok(None);
    };
    if listing.draft
        || listing.tag_name != release_tag(channel)
        || listing.assets.len() > MAX_ASSETS
    {
        return Err(LISTING);
    }
    let mut assets = BTreeMap::new();
    let mut sizes = BTreeMap::new();
    for asset in listing.assets {
        if asset.id == 0 || asset.state != "uploaded" {
            return Err(LISTING);
        }
        sizes.insert(asset.name.clone(), asset.size);
        if assets.insert(asset.name, asset.id).is_some() {
            return Err(LISTING);
        }
    }
    let (Some(manifest), Some(signature)) = (assets.get(MANIFEST_FILE), assets.get(SIGNATURE_FILE))
    else {
        return Err(LISTING);
    };
    if sizes.get(MANIFEST_FILE).copied().unwrap_or(u64::MAX) > MAX_MANIFEST_BYTES as u64
        || sizes.get(SIGNATURE_FILE) != Some(&64)
    {
        return Err(LISTING);
    }
    let manifest = fetcher
        .fetch(format!("store-asset-{manifest}"), MAX_MANIFEST_BYTES)
        .await?;
    let signature = fetcher
        .fetch(format!("store-asset-{signature}"), 64)
        .await?;
    Ok(Some(Found {
        manifest,
        signature,
        assets,
    }))
}

/// What to download for an update, decided only by the verified release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fetch {
    pub name: String,
    pub limit: usize,
}

/// An artifact that is exactly what the release pinned, ready for the platform's installer.
pub struct Verified {
    pub build: Build,
    pub kind: Kind,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Busy {
    Check,
    Download,
}

struct Offer {
    release: Release,
    artifact: Artifact,
    locator: String,
}

/// Application updates as the owner keeps them: trust root, floor, the offer that was verified and
/// what the window is told. Synchronous and without a network, like the store's service.
pub struct UpdateService {
    key: Result<VerifyingKey, Refusal>,
    channel: &'static str,
    running: Build,
    /// What this installation can apply, or the sentence that says why it cannot.
    target: Result<Target, &'static str>,
    floor: Floor,
    offer: Option<Offer>,
    state: UpdateState,
    busy: Option<Busy>,
}

impl UpdateService {
    pub fn new(
        candidate: Option<[u8; 32]>,
        channel: &'static str,
        running: Build,
        target: Result<Target, &'static str>,
        data: &Path,
    ) -> Self {
        let key = trust_root(candidate);
        let (status, error) = match (&key, &target) {
            (Err(_), _) => (UpdateStatus::NotConfigured, None),
            (Ok(_), Err(words)) => (UpdateStatus::Unsupported, Some((*words).to_owned())),
            (Ok(_), Ok(_)) => (UpdateStatus::Idle, None),
        };
        Self {
            key,
            channel,
            running,
            target,
            floor: Floor {
                path: data.join("update-floor"),
            },
            offer: None,
            state: UpdateState {
                status,
                current: running.to_string(),
                offered: None,
                offered_bytes: 0,
                notes: None,
                error,
            },
            busy: None,
        }
    }

    pub fn state(&self) -> &UpdateState {
        &self.state
    }

    pub fn channel(&self) -> &'static str {
        self.channel
    }

    /// Whether a verified, newer release is waiting to be downloaded.
    pub fn available(&self) -> bool {
        self.offer.is_some() && self.busy.is_none() && self.state.status == UpdateStatus::Available
    }

    fn fail(&mut self, words: &'static str) -> Refusal {
        self.busy = None;
        self.state.status = UpdateStatus::Failed;
        self.state.error = Some(words.to_owned());
        Refusal(words)
    }

    fn claim(&mut self, what: Busy) -> Result<(), Refusal> {
        if let Err(refusal) = &self.key {
            return Err(*refusal);
        }
        if let Err(words) = self.target {
            return Err(Refusal(words));
        }
        if self.busy.is_some() {
            return Err(Refusal("An update is already being worked on"));
        }
        if self.state.status == UpdateStatus::Installed {
            return Err(Refusal(
                "An update was installed; restart the application to use it",
            ));
        }
        self.busy = Some(what);
        Ok(())
    }

    /// Start a check: the caller runs [`discover`] and then [`Self::finish_check`].
    pub fn begin_check(&mut self) -> Result<(), Refusal> {
        self.claim(Busy::Check)?;
        self.state.status = UpdateStatus::Checking;
        self.state.error = None;
        Ok(())
    }

    /// Verify what the check found and decide whether it is an update for this installation.
    pub fn finish_check(&mut self, found: Result<Option<Found>, Refusal>) -> Result<(), Refusal> {
        let (Ok(key), Ok(target)) = (self.key, self.target) else {
            return Err(NOT_SET_UP);
        };
        let found = match found {
            Ok(found) => found,
            Err(refusal) => return Err(self.fail(refusal.0)),
        };
        self.offer = None;
        self.state.offered = None;
        self.state.offered_bytes = 0;
        self.state.notes = None;
        let Some(found) = found else {
            self.settle();
            return Ok(());
        };
        let release = verify(&key, &found.manifest, &found.signature, self.channel)
            .map_err(|refusal| self.fail(refusal.0))?;
        self.floor
            .admit(&release)
            .map_err(|refusal| self.fail(refusal.0))?;
        if release.build <= self.running {
            self.settle();
            return Ok(());
        }
        let Some(artifact) = release.artifact(&target).cloned() else {
            // Authentic and newer, but nothing in it fits this installation: said, not hidden.
            return Err(
                self.fail("A newer version was published, but not for this kind of installation")
            );
        };
        let Some(asset) = found.assets.get(&artifact.file) else {
            return Err(self.fail("The published update is missing its file"));
        };
        self.state.offered = Some(release.build.to_string());
        self.state.offered_bytes = artifact.size;
        self.state.notes = (!release.notes.is_empty()).then(|| release.notes.clone());
        self.state.status = UpdateStatus::Available;
        self.state.error = None;
        self.busy = None;
        self.offer = Some(Offer {
            release,
            artifact,
            locator: format!("store-asset-{asset}"),
        });
        Ok(())
    }

    fn settle(&mut self) {
        self.busy = None;
        self.state.status = UpdateStatus::Current;
        self.state.error = None;
    }

    /// Start downloading the offered artifact. What is fetched is the verified release's.
    pub fn begin_download(&mut self) -> Result<Fetch, Refusal> {
        let Some(offer) = &self.offer else {
            return Err(Refusal("No update is available"));
        };
        let fetch = Fetch {
            name: offer.locator.clone(),
            limit: usize::try_from(offer.artifact.size)
                .map_err(|_| Refusal("The update is too large"))?,
        };
        self.claim(Busy::Download)?;
        self.state.status = UpdateStatus::Downloading;
        self.state.error = None;
        Ok(fetch)
    }

    /// Check the downloaded bytes against the signed release. The installer gets only these.
    pub fn finish_download(
        &mut self,
        bytes: Result<Vec<u8>, Refusal>,
    ) -> Result<Verified, Refusal> {
        let bytes = match bytes {
            Ok(bytes) => bytes,
            Err(refusal) => return Err(self.fail(refusal.0)),
        };
        let Some(offer) = &self.offer else {
            return Err(self.fail("No update is available"));
        };
        if let Err(refusal) = artifact_matches(&offer.artifact, &bytes) {
            return Err(self.fail(refusal.0));
        }
        self.state.status = UpdateStatus::Installing;
        Ok(Verified {
            build: offer.release.build,
            kind: offer.artifact.kind,
            bytes,
        })
    }

    /// The platform's installer put the update in place: it runs after a restart.
    pub fn installed(&mut self) {
        self.busy = None;
        self.offer = None;
        self.state.status = UpdateStatus::Installed;
        self.state.error = None;
    }

    /// The platform's installer could not apply the verified update. The offer stays.
    pub fn failed(&mut self, words: &'static str) {
        self.fail(words);
    }

    /// Whether an update is in place and waiting for the application to start again.
    pub fn waiting_for_restart(&self) -> bool {
        self.state.status == UpdateStatus::Installed
    }

    /// The application is ending so that the installed update runs. Refused unless one is in place.
    pub fn begin_restart(&mut self) -> Result<(), Refusal> {
        if !self.waiting_for_restart() {
            return Err(Refusal("No update is waiting for a restart"));
        }
        self.state.status = UpdateStatus::Restarting;
        Ok(())
    }
}

/// How one kind of installation replaces itself. The only place a platform is known: everything
/// before it (what is offered, what was signed, what the bytes are) is the same everywhere.
pub trait Installer: Send + Sync + 'static {
    /// What this installation can apply, or the sentence that says why it cannot replace itself
    /// (it is managed by something else, or it was not installed in a way that can be replaced).
    fn target(&self) -> Result<Target, &'static str>;

    /// Whether applying an update ends the running application, because the system replaces it
    /// whole. Where it does, nothing is left to restart afterwards: `reopen` says what the person
    /// wants to happen then.
    fn ends_application(&self) -> bool {
        false
    }

    /// Put the verified artifact in place. Blocking: called away from the event loop. `reopen` is
    /// whether the application should be started again where applying the update ends it.
    fn install(&self, update: Verified, reopen: bool) -> Result<(), &'static str>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    const CHANNEL: &str = "test";
    const TARGET: Target = Target {
        platform: "linux",
        architecture: "x86_64",
        kind: Kind::Archive,
    };

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[7; 32])
    }

    fn sign(key: &SigningKey, manifest: &[u8]) -> Vec<u8> {
        let mut signed = SIGNATURE_CONTEXT.to_vec();
        signed.extend_from_slice(manifest);
        key.sign(&signed).to_bytes().to_vec()
    }

    fn artifact(architecture: &str, kind: &str, file: &str, bytes: &[u8]) -> serde_json::Value {
        serde_json::json!({
            "platform": "linux", "architecture": architecture, "kind": kind, "file": file,
            "sha256": hex::encode(Sha256::digest(bytes)), "size": bytes.len(),
        })
    }

    fn manifest(version: &str, build: u64, artifacts: Vec<serde_json::Value>) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schema": 1, "channel": CHANNEL, "version": version, "build": build,
            "artifacts": artifacts, "notes": "What changed",
        }))
        .unwrap()
    }

    fn found(
        manifest: Vec<u8>,
        key: &SigningKey,
        files: &[&str],
    ) -> Result<Option<Found>, Refusal> {
        Ok(Some(Found {
            signature: sign(key, &manifest),
            manifest,
            assets: files
                .iter()
                .enumerate()
                .map(|(index, file)| ((*file).to_owned(), index as u64 + 10))
                .collect(),
        }))
    }

    fn service(data: &Path, running: (u64, u64, u64, u64)) -> UpdateService {
        UpdateService::new(
            Some(key().verifying_key().to_bytes()),
            CHANNEL,
            Build {
                version: Version(running.0, running.1, running.2),
                number: running.3,
            },
            Ok(TARGET),
            data,
        )
    }

    #[test]
    fn a_build_without_a_key_refuses_every_step_and_says_so() {
        let data = tempfile::tempdir().unwrap();
        let mut service =
            UpdateService::new(None, CHANNEL, Build::running(), Ok(TARGET), data.path());
        assert_eq!(service.state().status, UpdateStatus::NotConfigured);
        assert_eq!(service.begin_check(), Err(NOT_SET_UP));
        assert_eq!(
            service.begin_download().err(),
            Some(Refusal("No update is available"))
        );
        assert_eq!(service.state().status, UpdateStatus::NotConfigured);
    }

    #[test]
    fn an_installation_that_cannot_be_replaced_says_why_and_checks_nothing() {
        let data = tempfile::tempdir().unwrap();
        let mut service = UpdateService::new(
            Some(key().verifying_key().to_bytes()),
            CHANNEL,
            Build::running(),
            Err("This installation is updated by the system's own tool"),
            data.path(),
        );
        assert_eq!(service.state().status, UpdateStatus::Unsupported);
        assert_eq!(
            service.begin_check(),
            Err(Refusal(
                "This installation is updated by the system's own tool"
            ))
        );
    }

    #[test]
    fn a_newer_signed_release_is_offered_downloaded_and_checked_byte_for_byte() {
        let data = tempfile::tempdir().unwrap();
        let mut service = service(data.path(), (0, 1, 0, 0));
        let bytes = b"the new programs".to_vec();
        let document = manifest(
            "0.1.0",
            3,
            vec![
                artifact("any", "archive", "portable.tar.gz", b"portable"),
                artifact("x86_64", "archive", "exact.tar.gz", &bytes),
                artifact("x86_64", "image", "exact.image", b"image"),
            ],
        );
        service.begin_check().unwrap();
        assert_eq!(
            service.begin_check().err(),
            Some(Refusal("An update is already being worked on"))
        );
        service
            .finish_check(found(
                document,
                &key(),
                &["portable.tar.gz", "exact.tar.gz", "exact.image"],
            ))
            .unwrap();
        assert!(service.available());
        assert_eq!(service.state().offered.as_deref(), Some("0.1.0 (build 3)"));
        assert_eq!(service.state().notes.as_deref(), Some("What changed"));
        // What "Update now" downloads is the size the release pinned for the artifact this installation takes.
        assert_eq!(service.state().offered_bytes, bytes.len() as u64);
        // The exact architecture wins over the portable artifact listed before it.
        let fetch = service.begin_download().unwrap();
        assert_eq!(
            fetch,
            Fetch {
                name: "store-asset-11".into(),
                limit: bytes.len()
            }
        );

        let mut tampered = bytes.clone();
        tampered[0] ^= 1;
        assert!(service.finish_download(Ok(tampered)).is_err());
        assert_eq!(service.state().status, UpdateStatus::Failed);
        // The offer survives a bad download: the next attempt needs no new check.
        service.begin_download().unwrap();
        let verified = service.finish_download(Ok(bytes.clone())).unwrap();
        assert_eq!((verified.kind, verified.bytes), (Kind::Archive, bytes));
        service.installed();
        assert_eq!(service.state().status, UpdateStatus::Installed);
        assert!(service.begin_check().is_err());
    }

    #[test]
    fn the_same_or_an_older_build_is_not_an_update() {
        let data = tempfile::tempdir().unwrap();
        let mut service = service(data.path(), (0, 1, 0, 3));
        for (version, build) in [("0.1.0", 3), ("0.0.9", 99)] {
            // Each directory keeps one floor; these are separate histories.
            let data = tempfile::tempdir().unwrap();
            service.floor = Floor {
                path: data.path().join("update-floor"),
            };
            service.begin_check().unwrap();
            service
                .finish_check(found(
                    manifest(
                        version,
                        build,
                        vec![artifact("any", "archive", "a.tar.gz", b"a")],
                    ),
                    &key(),
                    &["a.tar.gz"],
                ))
                .unwrap();
            assert_eq!(service.state().status, UpdateStatus::Current);
            assert!(!service.available());
        }
        service.begin_check().unwrap();
        service.finish_check(Ok(None)).unwrap();
        assert_eq!(service.state().status, UpdateStatus::Current);
    }

    #[test]
    fn another_key_another_purpose_and_another_channel_are_refused() {
        let data = tempfile::tempdir().unwrap();
        let mut service = service(data.path(), (0, 1, 0, 0));
        let document = manifest(
            "0.2.0",
            0,
            vec![artifact("any", "archive", "a.tar.gz", b"a")],
        );

        service.begin_check().unwrap();
        let other = SigningKey::from_bytes(&[8; 32]);
        assert!(
            service
                .finish_check(found(document.clone(), &other, &["a.tar.gz"]))
                .is_err()
        );

        // The right key over the bare document, as an extension approval would be signed.
        service.begin_check().unwrap();
        let bare = Found {
            signature: key().sign(&document).to_bytes().to_vec(),
            manifest: document.clone(),
            assets: BTreeMap::new(),
        };
        assert!(service.finish_check(Ok(Some(bare))).is_err());

        let elsewhere = String::from_utf8(document.clone())
            .unwrap()
            .replace("\"test\"", "\"stable\"");
        service.begin_check().unwrap();
        assert_eq!(
            service.finish_check(found(elsewhere.into_bytes(), &key(), &["a.tar.gz"])),
            Err(Refusal("The update is for another release channel"))
        );
        assert!(!service.available());
        assert!(!data.path().join("update-floor").exists());
    }

    #[test]
    fn a_seen_release_cannot_be_rolled_back_or_swapped_even_after_a_restart() {
        let data = tempfile::tempdir().unwrap();
        let newer = manifest(
            "0.2.0",
            5,
            vec![artifact("any", "archive", "a.tar.gz", b"a")],
        );
        {
            let mut service = service(data.path(), (0, 1, 0, 0));
            service.begin_check().unwrap();
            service
                .finish_check(found(newer.clone(), &key(), &["a.tar.gz"]))
                .unwrap();
        }
        let mut service = service(data.path(), (0, 1, 0, 0));
        // Older than what was seen, though newer than what runs.
        service.begin_check().unwrap();
        assert_eq!(
            service.finish_check(found(
                manifest(
                    "0.2.0",
                    4,
                    vec![artifact("any", "archive", "a.tar.gz", b"a")]
                ),
                &key(),
                &["a.tar.gz"],
            )),
            Err(Refusal("The update offered is older than one already seen"))
        );
        // The same identity with other bytes.
        service.begin_check().unwrap();
        assert!(
            service
                .finish_check(found(
                    manifest(
                        "0.2.0",
                        5,
                        vec![artifact("any", "archive", "a.tar.gz", b"other")]
                    ),
                    &key(),
                    &["a.tar.gz"],
                ))
                .is_err()
        );
        // The very release that was seen is still accepted.
        service.begin_check().unwrap();
        service
            .finish_check(found(newer, &key(), &["a.tar.gz"]))
            .unwrap();
        assert!(service.available());

        std::fs::write(data.path().join("update-floor"), "not a floor").unwrap();
        service.begin_check().unwrap();
        assert_eq!(
            service.finish_check(found(
                manifest(
                    "0.3.0",
                    0,
                    vec![artifact("any", "archive", "a.tar.gz", b"a")]
                ),
                &key(),
                &["a.tar.gz"],
            )),
            Err(Refusal("The saved update state is damaged"))
        );
    }

    #[test]
    fn documents_this_version_does_not_understand_are_refused_in_words() {
        let key = key();
        let public = key.verifying_key();
        let check = |document: serde_json::Value| {
            let bytes = serde_json::to_vec(&document).unwrap();
            verify(&public, &bytes, &sign(&key, &bytes), CHANNEL).map(|_| ())
        };
        let base = || {
            serde_json::json!({
                "schema": 1, "channel": CHANNEL, "version": "1.0.0", "build": 0,
                "artifacts": [artifact("any", "archive", "a.tar.gz", b"a")],
            })
        };
        assert!(check(base()).is_ok());
        let mut newer = base();
        newer["schema"] = 2.into();
        newer["something"] = "new".into();
        assert_eq!(
            check(newer),
            Err(Refusal(
                "The update is described in a way this version does not read"
            ))
        );
        let mut unknown = base();
        unknown["extra"] = true.into();
        assert_eq!(check(unknown), Err(UNREADABLE));
        for (field, value) in [
            ("kind", serde_json::json!("script")),
            ("file", serde_json::json!("../a.tar.gz")),
            ("file", serde_json::json!(".hidden")),
            ("sha256", serde_json::json!("ABC")),
            ("size", serde_json::json!(0)),
            ("size", serde_json::json!(MAX_ARTIFACT_BYTES + 1)),
            ("platform", serde_json::json!("Linux x")),
        ] {
            let mut document = base();
            document["artifacts"][0][field] = value;
            assert_eq!(check(document), Err(UNREADABLE), "{field}");
        }
        let mut twice = base();
        let first = twice["artifacts"][0].clone();
        twice["artifacts"].as_array_mut().unwrap().push(first);
        assert_eq!(check(twice), Err(UNREADABLE));
        let mut version = base();
        version["version"] = "1.0".into();
        assert_eq!(check(version), Err(UNREADABLE));
    }

    #[test]
    fn a_newer_release_with_nothing_for_this_installation_is_said_not_hidden() {
        let data = tempfile::tempdir().unwrap();
        let mut service = service(data.path(), (0, 1, 0, 0));
        service.begin_check().unwrap();
        let document = manifest(
            "0.2.0",
            0,
            vec![artifact("x86_64", "image", "a.image", b"a")],
        );
        assert!(
            service
                .finish_check(found(document, &key(), &["a.image"]))
                .is_err()
        );
        assert_eq!(service.state().status, UpdateStatus::Failed);
        assert!(!service.available());

        // Listed in the signed document but absent from the release: no locator to ask for.
        let data = tempfile::tempdir().unwrap();
        let mut service = self::service(data.path(), (0, 1, 0, 0));
        service.begin_check().unwrap();
        let document = manifest(
            "0.2.0",
            0,
            vec![artifact("any", "archive", "a.tar.gz", b"a")],
        );
        assert_eq!(
            service.finish_check(found(document, &key(), &[])),
            Err(Refusal("The published update is missing its file"))
        );
    }

    #[test]
    fn a_build_number_that_is_not_a_number_reads_as_none() {
        assert_eq!(decimal(None), 0);
        assert_eq!(decimal(Some("")), 0);
        assert_eq!(decimal(Some("12")), 12);
        assert_eq!(decimal(Some("1.2")), 0);
        assert_eq!(decimal(Some("99999999999999999999999")), 0);
        let build = Build {
            version: Version(1, 2, 3),
            number: 0,
        };
        assert_eq!(build.to_string(), "1.2.3");
        assert!(
            Build {
                version: Version(1, 2, 3),
                number: 9
            } < Build {
                version: Version(1, 3, 0),
                number: 0
            }
        );
    }

    #[tokio::test]
    async fn discovery_takes_only_a_well_formed_listing_of_the_channel_s_own_release() {
        use crate::store_runtime::fake::MapFetcher;
        let listing = |tag: &str, draft: bool, sig_size: u64| {
            serde_json::to_vec(&serde_json::json!({
                "draft": draft, "prerelease": true, "tag_name": tag, "ignored": 1,
                "assets": [
                    {"id": 1, "name": MANIFEST_FILE, "size": 10, "state": "uploaded"},
                    {"id": 2, "name": SIGNATURE_FILE, "size": sig_size, "state": "uploaded"},
                    {"id": 3, "name": "a.tar.gz", "size": 5, "state": "uploaded"},
                ],
            }))
            .unwrap()
        };
        let files = MapFetcher::default();
        files.put("store-asset-1", b"manifest".to_vec());
        files.put("store-asset-2", vec![0; 64]);
        files.put("app-release-test", listing("app/test", false, 64));
        let found = discover(&files, CHANNEL).await.unwrap().unwrap();
        assert_eq!(found.manifest, b"manifest");
        assert_eq!(found.assets.get("a.tar.gz"), Some(&3));

        files.put("app-release-test", b"null".to_vec());
        assert!(discover(&files, CHANNEL).await.unwrap().is_none());
        for bad in [
            listing("app/stable", false, 64),
            listing("app/test", true, 64),
            listing("app/test", false, 65),
            b"{}".to_vec(),
        ] {
            files.put("app-release-test", bad);
            assert!(discover(&files, CHANNEL).await.is_err());
        }
        assert!(discover(&files, "Bad Channel").await.is_err());
    }
}
