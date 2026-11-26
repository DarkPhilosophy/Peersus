//! Updates of the application itself: what a signed release says, and what it proves.
//!
//! THE TRUST ROOT is its own Ed25519 public key, [`UPDATE_PUBLIC_KEY`], apart from the extension
//! store's. The two never stand in for each other: whoever can approve an extension cannot replace
//! the application, and the signed bytes begin with [`SIGNATURE_CONTEXT`], so a signature made for
//! one kind of document is not a signature of the other even if one key were ever used for both.
//! While the key is `None` updates are not set up and everything here refuses; there is no fallback.
//!
//! WHAT IS SIGNED is one list per release, [`SUMS_FILE`], in the form `sha256sum` reads: one
//! `<hex>  <name>` line for each artifact, named `peersus-<version>-<channel>.<build>-<platform>-
//! <architecture>.<extension>`, and one `<hex>  <archive stem>/<program>` line for each program an
//! archive holds. The version, channel and build exist only in those signed names; every artifact
//! of a release must name the same ones. Where the bytes came from is not trusted and not signed:
//! the release's tag and its listing only say where to look.
//!
//! ROLLBACK. A release is offered only when it is newer than the running build. The newest release
//! ever verified is kept on disk with the digest of its list: an older one is refused, and so is a
//! different list for the same version and build (two releases under one identity).
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
/// The development key: what private development builds verify the `dev` channel with. Its seed
/// never signs anything a product build accepts, because a product build does not hold this key.
#[cfg(all(feature = "private-store", not(feature = "prerelease-channel")))]
pub const UPDATE_PUBLIC_KEY: Option<[u8; 32]> = Some([
    78, 72, 236, 243, 236, 148, 162, 194, 34, 66, 99, 121, 175, 134, 25, 229, 198, 112, 33, 147,
    24, 145, 16, 45, 224, 247, 82, 209, 40, 170, 162, 210,
]);
/// The prerelease key: what prerelease builds verify the `pre` channel with, apart from the
/// development key, so a development release can never replace a prerelease build.
#[cfg(feature = "prerelease-channel")]
pub const UPDATE_PUBLIC_KEY: Option<[u8; 32]> = Some([
    20, 37, 202, 234, 80, 105, 234, 16, 67, 73, 128, 174, 179, 230, 214, 60, 199, 58, 99, 163, 148,
    232, 41, 87, 120, 15, 18, 201, 4, 131, 63, 114,
]);

/// The channel this build follows, fixed when it is built: a private development build follows
/// `dev`, a prerelease build `pre`, anything else `stable`. A release of another channel is
/// refused even when its signature holds, because the channel is in the signed names; moving to
/// another channel means installing that channel's build.
pub const CHANNEL: &str = if cfg!(feature = "prerelease-channel") {
    "pre"
} else if cfg!(feature = "private-store") {
    "dev"
} else {
    "stable"
};

/// What the signature covers before the list: the purpose of the signature, in the bytes.
pub const SIGNATURE_CONTEXT: &[u8] = b"peersus-sums-v1\n";
pub const SUMS_FILE: &str = "SHA256SUMS";
pub const SIGNATURE_FILE: &str = "SHA256SUMS.sig";
pub const MAX_SUMS_BYTES: usize = 64 * 1024;
/// Largest artifact a release may carry: what the fetcher will carry in one piece.
pub const MAX_ARTIFACT_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_ARTIFACTS: usize = 32;
/// Most programs one archive may list.
pub const MAX_PROGRAMS: usize = 64;
/// What every artifact's name begins with.
const PRODUCT: &str = "peersus-";
/// Most assets a release listing may name before it is refused unread.
const MAX_ASSETS: usize = 64;
/// Largest list of release tags read.
const MAX_TAGS_BYTES: usize = 1024 * 1024;

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

/// How an artifact is applied, said by its extension. A closed set: an extension this version does
/// not know makes the whole list unreadable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// A compressed folder of the application's programs (`.tar.gz`), replacing the folder it runs from.
    Archive,
    /// One self-contained file that is the application (`.AppImage`), replacing that file.
    Image,
    /// A package the operating system's installer applies (`.apk`).
    Package,
}

const EXTENSIONS: [(&str, Kind); 3] = [
    (".tar.gz", Kind::Archive),
    (".AppImage", Kind::Image),
    (".apk", Kind::Package),
];

/// What this installation can apply, as its platform said. No platform is named in this module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub platform: &'static str,
    pub architecture: &'static str,
    pub kind: Kind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Artifact {
    pub platform: String,
    /// A processor architecture, or `any` for an artifact that carries every one it supports.
    pub architecture: String,
    pub kind: Kind,
    /// File name of the artifact where it is served. Never a path.
    pub file: String,
    /// Lower-case hex SHA-256 of the file.
    pub sha256: String,
    /// For an archive: the lower-case hex SHA-256 of each program in it, by file name.
    pub programs: BTreeMap<String, String>,
}

fn short_word(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

/// A channel: lower-case letters and digits only, so that it never runs into the `-` and `.` that
/// separate it from the version and the build.
fn channel_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 16
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

fn digest_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// `<version>` and `<channel>.<build>`, each in its one spelling: no leading zeros, no sign,
/// a build number of at least 1.
fn identity<'a>(version: &str, release: &'a str) -> Option<(Build, &'a str)> {
    let parsed = Version::parse(version)?;
    let (channel, number) = release.rsplit_once('.')?;
    let canonical = parsed.to_string() == version
        && channel_name(channel)
        && (1..=20).contains(&number.len())
        && number.bytes().all(|byte| byte.is_ascii_digit())
        && !number.starts_with('0');
    canonical.then_some(())?;
    Some((
        Build {
            version: parsed,
            number: number.parse().ok()?,
        },
        channel,
    ))
}

/// What a release tag `v<version>-<channel>.<build>` says. Only a hint of where to look.
pub fn tag_identity(tag: &str) -> Option<(Build, &str)> {
    let (version, release) = tag.strip_prefix('v')?.split_once('-')?;
    identity(version, release)
}

/// The tag a release of `build` on `channel` is published under.
pub fn release_tag(build: Build, channel: &str) -> String {
    format!("v{}-{channel}.{}", build.version, build.number)
}

/// What an artifact's signed file name says.
struct Named<'a> {
    build: Build,
    channel: &'a str,
    platform: &'a str,
    architecture: &'a str,
    kind: Kind,
}

fn artifact_name(file: &str) -> Option<Named<'_>> {
    if file.len() > 128 || !name::plain(file) {
        return None;
    }
    let (stem, kind) = EXTENSIONS
        .into_iter()
        .find_map(|(extension, kind)| file.strip_suffix(extension).map(|stem| (stem, kind)))?;
    let mut parts = stem.strip_prefix(PRODUCT)?.split('-');
    let (Some(version), Some(release), Some(platform), Some(architecture), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return None;
    };
    let (build, channel) = identity(version, release)?;
    (short_word(platform) && short_word(architecture)).then_some(Named {
        build,
        channel,
        platform,
        architecture,
        kind,
    })
}

fn program_name(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.starts_with('.') && name::plain(value)
}

/// A release whose signature checked out, and nothing of it that was not signed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub build: Build,
    artifacts: Vec<Artifact>,
    /// SHA-256 of the signed list: what tells two lists for one build apart.
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

/// Check the signature over [`SIGNATURE_CONTEXT`] and the list, then the list itself: nothing is
/// read before the signature holds.
pub fn verify(
    key: &VerifyingKey,
    sums: &[u8],
    signature: &[u8],
    channel: &str,
) -> Result<Release, Refusal> {
    if sums.len() > MAX_SUMS_BYTES {
        return Err(UNREADABLE);
    }
    let signature = Signature::from_slice(signature)
        .map_err(|_| Refusal("The update is not signed by this application's publisher"))?;
    let mut signed = Vec::with_capacity(SIGNATURE_CONTEXT.len() + sums.len());
    signed.extend_from_slice(SIGNATURE_CONTEXT);
    signed.extend_from_slice(sums);
    key.verify(&signed, &signature)
        .map_err(|_| Refusal("The update is not signed by this application's publisher"))?;
    // Authentic from here on.
    let text = std::str::from_utf8(sums).map_err(|_| UNREADABLE)?;
    let body = text.strip_suffix('\n').ok_or(UNREADABLE)?;
    let mut released: Option<(Build, &str)> = None;
    let mut artifacts = Vec::new();
    let mut programs = Vec::new();
    let mut targets = BTreeSet::new();
    for line in body.split('\n') {
        let (sha256, file) = line.split_once("  ").ok_or(UNREADABLE)?;
        if !digest_hex(sha256) {
            return Err(UNREADABLE);
        }
        if let Some((stem, program)) = file.split_once('/') {
            if !program_name(program) {
                return Err(UNREADABLE);
            }
            programs.push((stem, program, sha256));
            continue;
        }
        let named = artifact_name(file).ok_or(UNREADABLE)?;
        let identity = (named.build, named.channel);
        if *released.get_or_insert(identity) != identity
            || artifacts.len() == MAX_ARTIFACTS
            || !targets.insert((named.platform, named.architecture, named.kind))
        {
            return Err(UNREADABLE);
        }
        artifacts.push(Artifact {
            platform: named.platform.to_owned(),
            architecture: named.architecture.to_owned(),
            kind: named.kind,
            file: file.to_owned(),
            sha256: sha256.to_owned(),
            programs: BTreeMap::new(),
        });
    }
    let (build, released_for) = released.ok_or(UNREADABLE)?;
    if released_for != channel {
        return Err(Refusal("The update is for another release channel"));
    }
    for (stem, program, sha256) in programs {
        let archive = artifacts
            .iter_mut()
            .find(|artifact| {
                artifact.kind == Kind::Archive
                    && artifact.file.strip_suffix(".tar.gz") == Some(stem)
            })
            .ok_or(UNREADABLE)?;
        if archive.programs.len() == MAX_PROGRAMS
            || archive
                .programs
                .insert(program.to_owned(), sha256.to_owned())
                .is_some()
        {
            return Err(UNREADABLE);
        }
    }
    Ok(Release {
        build,
        artifacts,
        digest: Sha256::digest(sums).into(),
    })
}

/// Whether `bytes` are exactly the artifact the signed release pinned.
pub fn artifact_matches(artifact: &Artifact, bytes: &[u8]) -> Result<(), Refusal> {
    if hex::encode(Sha256::digest(bytes)) != artifact.sha256 {
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

#[derive(Deserialize)]
struct TagRef {
    #[serde(rename = "ref")]
    reference: String,
}

/// Where one file of a release can be asked for, as its listing said: a locator, never authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Located {
    pub id: u64,
    pub size: u64,
}

/// What the newest release of a channel led to: the signed list, its signature, and where each
/// file of the release can be asked for. All of it untrusted until [`UpdateService::finish_check`].
pub struct Found {
    pub sums: Vec<u8>,
    pub signature: Vec<u8>,
    pub assets: BTreeMap<String, Located>,
}

/// The name the fetcher is asked for the application's release tags by.
pub const TAGS_NAME: &str = "app-tags";

/// The name the fetcher is asked for the release listing of one tag by.
pub fn listing_name(tag: &str) -> String {
    format!("app-release-{tag}")
}

/// The newest tag of `channel` in a list of tag references. Other channels, extensions' tags and
/// whatever does not read as a release tag are passed over, not refused.
fn newest_tag(references: Vec<TagRef>, channel: &str) -> Option<String> {
    references
        .into_iter()
        .filter_map(|reference| {
            let tag = reference.reference.strip_prefix("refs/tags/")?.to_owned();
            let (build, released_for) = tag_identity(&tag)?;
            (released_for == channel).then_some((build, tag))
        })
        .max_by_key(|(build, _)| *build)
        .map(|(_, tag)| tag)
}

/// Find the channel's newest release and fetch its signed list. `None` when the channel has
/// published nothing: that is not a failure.
pub async fn discover(fetcher: &dyn StoreFetcher, channel: &str) -> Result<Option<Found>, Refusal> {
    if !channel_name(channel) {
        return Err(LISTING);
    }
    let bytes = fetcher.fetch(TAGS_NAME.to_owned(), MAX_TAGS_BYTES).await?;
    let references: Vec<TagRef> = serde_json::from_slice(&bytes).map_err(|_| LISTING)?;
    let Some(tag) = newest_tag(references, channel) else {
        return Ok(None);
    };
    let bytes = fetcher.fetch(listing_name(&tag), 1024 * 1024).await?;
    // `null`: the tag exists but its release is not published yet.
    let listing: Option<Listing> = serde_json::from_slice(&bytes).map_err(|_| LISTING)?;
    let Some(listing) = listing else {
        return Ok(None);
    };
    if listing.draft || listing.tag_name != tag || listing.assets.len() > MAX_ASSETS {
        return Err(LISTING);
    }
    let mut assets = BTreeMap::new();
    for asset in listing.assets {
        if asset.id == 0 || asset.state != "uploaded" {
            return Err(LISTING);
        }
        let located = Located {
            id: asset.id,
            size: asset.size,
        };
        if assets.insert(asset.name, located).is_some() {
            return Err(LISTING);
        }
    }
    let (Some(sums), Some(signature)) = (assets.get(SUMS_FILE), assets.get(SIGNATURE_FILE)) else {
        return Err(LISTING);
    };
    if sums.size > MAX_SUMS_BYTES as u64 || signature.size != 64 {
        return Err(LISTING);
    }
    let sums = fetcher
        .fetch(format!("store-asset-{}", sums.id), MAX_SUMS_BYTES)
        .await?;
    let signature = fetcher
        .fetch(format!("store-asset-{}", signature.id), 64)
        .await?;
    Ok(Some(Found {
        sums,
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

/// The signed list a release was verified from, as it was published: what an installation keeps
/// beside its programs so that they can be checked against it later.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedList {
    pub sums: Vec<u8>,
    pub signature: Vec<u8>,
}

/// An artifact that is exactly what the release pinned, ready for the platform's installer.
pub struct Verified {
    pub build: Build,
    pub kind: Kind,
    pub bytes: Vec<u8>,
    /// For an archive: what each program in it must be, by name, as the release signed them.
    pub programs: BTreeMap<String, String>,
    pub list: SignedList,
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
    /// What the listing said the file weighs: the download stops there, and the digest decides.
    limit: usize,
    list: SignedList,
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
    /// Installed programs differ from the signed list they were put in place from: the release of
    /// the running build is accepted again, to put them back.
    damaged: bool,
    /// A check is wanted now, not at the next scheduled one, because programs were found damaged.
    look_now: bool,
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
                error,
            },
            busy: None,
            damaged: false,
            look_now: false,
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
        self.look_now = false;
        self.state.status = UpdateStatus::Checking;
        self.state.error = None;
        Ok(())
    }

    /// What [`inspect`] checks an installation with, while updates are set up and it can apply one.
    pub fn inspector(&self) -> Option<(VerifyingKey, &'static str, Target)> {
        Some((*self.key.as_ref().ok()?, self.channel, self.target.ok()?))
    }

    /// Programs of this installation were found damaged: look for the running build's release now,
    /// and accept it, to put them back.
    pub fn damaged(&mut self) {
        self.damaged = true;
        self.look_now = true;
    }

    /// Whether a check is wanted before the next scheduled one.
    pub fn wants_check(&self) -> bool {
        self.look_now
    }

    /// Whether what is offered is the running build, to put damaged programs back: installed
    /// without waiting to be asked, because it changes nothing but what was damaged.
    pub fn repairing(&self) -> bool {
        self.available()
            && self
                .offer
                .as_ref()
                .is_some_and(|offer| offer.release.build == self.running)
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
        let Some(found) = found else {
            self.settle();
            return Ok(());
        };
        let release = verify(&key, &found.sums, &found.signature, self.channel)
            .map_err(|refusal| self.fail(refusal.0))?;
        self.floor
            .admit(&release)
            .map_err(|refusal| self.fail(refusal.0))?;
        // The running build itself only to put back what was damaged.
        if release.build < self.running || (release.build == self.running && !self.damaged) {
            self.settle();
            return Ok(());
        }
        let Some(artifact) = release.artifact(&target).cloned() else {
            // Authentic and newer, but nothing in it fits this installation: said, not hidden.
            return Err(
                self.fail("A newer version was published, but not for this kind of installation")
            );
        };
        let Some(asset) = found.assets.get(&artifact.file).copied() else {
            return Err(self.fail("The published update is missing its file"));
        };
        let Some(limit) = (1..=MAX_ARTIFACT_BYTES)
            .contains(&asset.size)
            .then(|| usize::try_from(asset.size).ok())
            .flatten()
        else {
            return Err(self.fail("The published update is too large"));
        };
        self.state.offered = Some(release.build.to_string());
        self.state.offered_bytes = asset.size;
        self.state.status = UpdateStatus::Available;
        self.state.error = None;
        self.busy = None;
        self.offer = Some(Offer {
            release,
            artifact,
            locator: format!("store-asset-{}", asset.id),
            limit,
            list: SignedList {
                sums: found.sums,
                signature: found.signature,
            },
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
            limit: offer.limit,
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
            programs: offer.artifact.programs.clone(),
            list: offer.list.clone(),
        })
    }

    /// The platform's installer put the update in place: it runs after a restart. Programs put
    /// back for the running build need no restart: what runs is already that build.
    pub fn installed(&mut self) {
        let repaired = self
            .offer
            .take()
            .is_some_and(|offer| offer.release.build == self.running);
        self.busy = None;
        self.state.error = None;
        if repaired {
            self.damaged = false;
            self.state.offered = None;
            self.state.offered_bytes = 0;
            self.state.status = UpdateStatus::Current;
        } else {
            self.state.status = UpdateStatus::Installed;
        }
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

    /// The signed list the installed programs were put in place from, where this installation
    /// keeps one. `None` where it keeps none (a build folder, a package the system manages).
    fn installed_list(&self) -> Option<SignedList> {
        None
    }

    /// The lower-case hex SHA-256 of one installed program, `None` when it is missing or cannot
    /// be read. Blocking.
    fn program_digest(&self, _program: &str) -> Option<String> {
        None
    }
}

/// The installed programs that are not what the signed list they were put in place from says:
/// missing, or other bytes. Empty when they all are, or when the installation keeps no list or
/// one this build cannot verify (then there is nothing to put them back from). Blocking: reads
/// every program.
pub fn inspect(
    key: &VerifyingKey,
    channel: &str,
    target: &Target,
    installer: &dyn Installer,
) -> Vec<String> {
    let Some(list) = installer.installed_list() else {
        return Vec::new();
    };
    let Ok(release) = verify(key, &list.sums, &list.signature, channel) else {
        return Vec::new();
    };
    let Some(artifact) = release.artifact(target) else {
        return Vec::new();
    };
    artifact
        .programs
        .iter()
        .filter(|(program, digest)| installer.program_digest(program).as_ref() != Some(*digest))
        .map(|(program, _)| program.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    const CHANNEL: &str = "dev";
    const TARGET: Target = Target {
        platform: "linux",
        architecture: "x86_64",
        kind: Kind::Archive,
    };

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[7; 32])
    }

    fn sign(key: &SigningKey, sums: &[u8]) -> Vec<u8> {
        let mut signed = SIGNATURE_CONTEXT.to_vec();
        signed.extend_from_slice(sums);
        key.sign(&signed).to_bytes().to_vec()
    }

    /// `peersus-<version>-dev.<build>-linux-<architecture><extension>`.
    fn file(version: &str, build: u64, architecture: &str, extension: &str) -> String {
        format!("peersus-{version}-{CHANNEL}.{build}-linux-{architecture}{extension}")
    }

    fn line(name: &str, bytes: &[u8]) -> String {
        format!("{}  {name}\n", hex::encode(Sha256::digest(bytes)))
    }

    fn sums(lines: &[(&str, &[u8])]) -> Vec<u8> {
        lines
            .iter()
            .map(|(name, bytes)| line(name, bytes))
            .collect::<String>()
            .into_bytes()
    }

    /// A list of one portable archive of `version` and `build`.
    fn release(version: &str, build: u64, bytes: &[u8]) -> (Vec<u8>, String) {
        let name = file(version, build, "any", ".tar.gz");
        (sums(&[(&name, bytes)]), name)
    }

    fn found(
        sums: Vec<u8>,
        key: &SigningKey,
        files: &[(&str, u64)],
    ) -> Result<Option<Found>, Refusal> {
        Ok(Some(Found {
            signature: sign(key, &sums),
            sums,
            assets: files
                .iter()
                .enumerate()
                .map(|(index, (file, size))| {
                    (
                        (*file).to_owned(),
                        Located {
                            id: index as u64 + 10,
                            size: *size,
                        },
                    )
                })
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
        let portable = file("0.1.0", 3, "any", ".tar.gz");
        let exact = file("0.1.0", 3, "x86_64", ".tar.gz");
        let image = file("0.1.0", 3, "x86_64", ".AppImage");
        let list = sums(&[
            (&portable, b"portable"),
            (&exact, &bytes),
            (&image, b"image"),
        ]);
        service.begin_check().unwrap();
        assert_eq!(
            service.begin_check().err(),
            Some(Refusal("An update is already being worked on"))
        );
        service
            .finish_check(found(
                list,
                &key(),
                &[(&portable, 8), (&exact, bytes.len() as u64), (&image, 5)],
            ))
            .unwrap();
        assert!(service.available());
        assert_eq!(service.state().offered.as_deref(), Some("0.1.0 (build 3)"));
        // What "Update now" downloads is the size the listing gave for the artifact this
        // installation takes; the exact architecture wins over the portable one listed before it.
        assert_eq!(service.state().offered_bytes, bytes.len() as u64);
        let fetch = service.begin_download().unwrap();
        assert_eq!(
            fetch,
            Fetch {
                name: "store-asset-11".into(),
                limit: bytes.len(),
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
            // Each directory keeps one floor: these are separate histories.
            let data = tempfile::tempdir().unwrap();
            service.floor = Floor {
                path: data.path().join("update-floor"),
            };
            let (list, name) = release(version, build, b"a");
            service.begin_check().unwrap();
            service
                .finish_check(found(list, &key(), &[(&name, 1)]))
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
        let (list, name) = release("0.2.0", 1, b"a");
        service.begin_check().unwrap();
        let other = SigningKey::from_bytes(&[8; 32]);
        assert!(
            service
                .finish_check(found(list.clone(), &other, &[(&name, 1)]))
                .is_err()
        );
        // The right key over the bare list, as something with another purpose would be signed.
        service.begin_check().unwrap();
        let bare = Found {
            signature: key().sign(&list).to_bytes().to_vec(),
            sums: list.clone(),
            assets: BTreeMap::new(),
        };
        assert!(service.finish_check(Ok(Some(bare))).is_err());
        // Signed by the right key, for the right purpose, but released on another channel.
        let elsewhere = String::from_utf8(list)
            .unwrap()
            .replace("-dev.1-", "-beta.1-");
        service.begin_check().unwrap();
        assert_eq!(
            service.finish_check(found(elsewhere.into_bytes(), &key(), &[(&name, 1)])),
            Err(Refusal("The update is for another release channel"))
        );
        assert!(!service.available());
        assert!(!data.path().join("update-floor").exists());
    }

    #[test]
    fn a_seen_release_cannot_be_rolled_back_or_swapped_even_after_a_restart() {
        let data = tempfile::tempdir().unwrap();
        let (newer, newer_name) = release("0.2.0", 5, b"a");
        {
            let mut service = service(data.path(), (0, 1, 0, 0));
            service.begin_check().unwrap();
            service
                .finish_check(found(newer.clone(), &key(), &[(&newer_name, 1)]))
                .unwrap();
        }
        let mut service = service(data.path(), (0, 1, 0, 0));
        // Older than what was seen, though newer than what runs.
        let (older, older_name) = release("0.2.0", 4, b"a");
        service.begin_check().unwrap();
        assert_eq!(
            service.finish_check(found(older, &key(), &[(&older_name, 1)])),
            Err(Refusal("The update offered is older than one already seen"))
        );
        // The same identity with other bytes.
        let (swapped, _) = release("0.2.0", 5, b"other");
        service.begin_check().unwrap();
        assert!(
            service
                .finish_check(found(swapped, &key(), &[(&newer_name, 1)]))
                .is_err()
        );
        // The very release that was seen is still accepted.
        service.begin_check().unwrap();
        service
            .finish_check(found(newer, &key(), &[(&newer_name, 1)]))
            .unwrap();
        assert!(service.available());

        std::fs::write(data.path().join("update-floor"), "not a floor").unwrap();
        let (later, later_name) = release("0.3.0", 1, b"a");
        service.begin_check().unwrap();
        assert_eq!(
            service.finish_check(found(later, &key(), &[(&later_name, 1)])),
            Err(Refusal("The saved update state is damaged"))
        );
    }

    #[test]
    fn lists_this_version_does_not_understand_are_refused_whole() {
        let key = key();
        let public = key.verifying_key();
        let check = |list: &str| {
            verify(
                &public,
                list.as_bytes(),
                &sign(&key, list.as_bytes()),
                CHANNEL,
            )
            .map(|_| ())
        };
        let good = file("1.0.0", 7, "x86_64", ".tar.gz");
        let hex = hex::encode(Sha256::digest(b"a"));
        assert_eq!(check(&format!("{hex}  {good}\n")), Ok(()));
        for list in [
            String::new(),
            "\n".to_owned(),
            // No final newline, a carriage return, one space, a capital, a short digest.
            format!("{hex}  {good}"),
            format!("{hex}  {good}\r\n"),
            format!("{hex} {good}\n"),
            format!("{}  {good}\n", hex.to_uppercase()),
            format!("{}  {good}\n", &hex[1..]),
            // Names that are not a release artifact's.
            format!("{hex}  {}\n", good.replace(".tar.gz", ".zip")),
            format!("{hex}  {}\n", good.replace("peersus-", "other-")),
            format!("{hex}  {}\n", good.replace("1.0.0", "1.0")),
            format!("{hex}  {}\n", good.replace("1.0.0", "1.00.0")),
            format!("{hex}  {}\n", good.replace(".7-", ".07-")),
            format!("{hex}  {}\n", good.replace(".7-", ".0-")),
            format!("{hex}  {}\n", good.replace("-dev.", "-Dev.")),
            format!("{hex}  {}\n", good.replace("-linux-", "-linux-extra-")),
            format!("{hex}  {}\n", good.replace("x86_64", "X86")),
            format!("{hex}  ../{good}\n"),
            // Two builds in one release, one target twice.
            format!(
                "{hex}  {good}\n{hex}  {}\n",
                file("1.0.0", 8, "aarch64", ".tar.gz")
            ),
            format!("{hex}  {good}\n{hex}  {good}\n"),
            // Programs of no archive, of an image, with names that are not plain.
            format!("{hex}  {good}\n{hex}  peersus-1.0.0-dev.7-linux-arm/peersus\n"),
            format!(
                "{hex}  {}\n{hex}  {}/peersus\n",
                file("1.0.0", 7, "x86_64", ".AppImage"),
                file("1.0.0", 7, "x86_64", "")
            ),
            format!(
                "{hex}  {good}\n{hex}  {}/.hidden\n",
                file("1.0.0", 7, "x86_64", "")
            ),
            format!(
                "{hex}  {good}\n{hex}  {}/a/b\n",
                file("1.0.0", 7, "x86_64", "")
            ),
            format!(
                "{hex}  {good}\n{hex}  {0}/peersus\n{hex}  {0}/peersus\n",
                file("1.0.0", 7, "x86_64", "")
            ),
        ] {
            assert_eq!(check(&list), Err(UNREADABLE), "{list:?}");
        }
    }

    #[test]
    fn the_programs_of_an_archive_are_read_with_it() {
        let key = key();
        let archive = file("1.0.0", 7, "x86_64", ".tar.gz");
        let stem = file("1.0.0", 7, "x86_64", "");
        let list = sums(&[
            (&format!("{stem}/peersus"), b"owner"),
            (&archive, b"archive"),
            (&format!("{stem}/peersus-ui"), b"window"),
        ]);
        let release = verify(&key.verifying_key(), &list, &sign(&key, &list), CHANNEL).unwrap();
        assert_eq!(
            release.build,
            Build {
                version: Version(1, 0, 0),
                number: 7
            }
        );
        let artifact = release.artifact(&TARGET).unwrap();
        assert_eq!(artifact.file, archive);
        assert_eq!(
            artifact.programs,
            BTreeMap::from([
                ("peersus".to_owned(), hex::encode(Sha256::digest(b"owner"))),
                (
                    "peersus-ui".to_owned(),
                    hex::encode(Sha256::digest(b"window"))
                ),
            ])
        );
    }

    #[test]
    fn a_newer_release_with_nothing_for_this_installation_is_said_not_hidden() {
        let data = tempfile::tempdir().unwrap();
        let mut service = service(data.path(), (0, 1, 0, 0));
        service.begin_check().unwrap();
        let image = file("0.2.0", 1, "x86_64", ".AppImage");
        assert!(
            service
                .finish_check(found(sums(&[(&image, b"a")]), &key(), &[(&image, 1)]))
                .is_err()
        );
        assert_eq!(service.state().status, UpdateStatus::Failed);
        assert!(!service.available());

        // Listed in the signed list but absent from the release, or weighing nothing or too much
        // there: nothing to ask for.
        let (list, name) = release("0.2.0", 1, b"a");
        for (files, words) in [
            (vec![], "The published update is missing its file"),
            (
                vec![(name.as_str(), 0)],
                "The published update is too large",
            ),
            (
                vec![(name.as_str(), MAX_ARTIFACT_BYTES + 1)],
                "The published update is too large",
            ),
        ] {
            let data = tempfile::tempdir().unwrap();
            let mut service = self::service(data.path(), (0, 1, 0, 0));
            service.begin_check().unwrap();
            assert_eq!(
                service.finish_check(found(list.clone(), &key(), &files)),
                Err(Refusal(words))
            );
        }
    }

    #[test]
    fn damaged_programs_take_the_running_build_again_and_need_no_restart() {
        let data = tempfile::tempdir().unwrap();
        let mut service = service(data.path(), (0, 2, 0, 5));
        let archive = file("0.2.0", 5, "x86_64", ".tar.gz");
        let stem = file("0.2.0", 5, "x86_64", "");
        let bytes = b"the same programs".to_vec();
        let list = sums(&[(&archive, &bytes), (&format!("{stem}/peersus"), b"owner")]);
        let release = || found(list.clone(), &key(), &[(&archive, bytes.len() as u64)]);
        // Whole: the running build is not an update.
        assert!(!service.wants_check());
        service.begin_check().unwrap();
        service.finish_check(release()).unwrap();
        assert_eq!(service.state().status, UpdateStatus::Current);
        // Damaged: looked for now, offered, and put back without asking.
        service.damaged();
        assert!(service.wants_check());
        service.begin_check().unwrap();
        assert!(!service.wants_check());
        service.finish_check(release()).unwrap();
        assert!(service.repairing());
        service.begin_download().unwrap();
        let verified = service.finish_download(Ok(bytes.clone())).unwrap();
        assert_eq!(
            verified.programs,
            BTreeMap::from([("peersus".to_owned(), hex::encode(Sha256::digest(b"owner")))])
        );
        assert_eq!(verified.list.sums, list);
        service.installed();
        assert_eq!(service.state().status, UpdateStatus::Current);
        assert_eq!(service.state().offered, None);
        // Put back: the running build is again not an update.
        service.begin_check().unwrap();
        service.finish_check(release()).unwrap();
        assert!(!service.available());
    }

    struct Kept {
        list: Option<SignedList>,
        programs: BTreeMap<String, String>,
    }

    impl Installer for Kept {
        fn target(&self) -> Result<Target, &'static str> {
            Ok(TARGET)
        }

        fn install(&self, _update: Verified, _reopen: bool) -> Result<(), &'static str> {
            Ok(())
        }

        fn installed_list(&self) -> Option<SignedList> {
            self.list.clone()
        }

        fn program_digest(&self, program: &str) -> Option<String> {
            self.programs.get(program).cloned()
        }
    }

    #[test]
    fn an_installation_is_inspected_against_the_signed_list_it_keeps() {
        let key = key();
        let archive = file("0.2.0", 5, "x86_64", ".tar.gz");
        let stem = file("0.2.0", 5, "x86_64", "");
        let list = sums(&[
            (&archive, b"archive"),
            (&format!("{stem}/peersus"), b"owner"),
            (&format!("{stem}/peersus-ui"), b"window"),
        ]);
        let kept = |signer: &SigningKey, programs: &[(&str, &[u8])]| Kept {
            list: Some(SignedList {
                signature: sign(signer, &list),
                sums: list.clone(),
            }),
            programs: programs
                .iter()
                .map(|(name, bytes)| ((*name).to_owned(), hex::encode(Sha256::digest(bytes))))
                .collect(),
        };
        let inspect = |installer: &Kept| inspect(&key.verifying_key(), CHANNEL, &TARGET, installer);
        assert!(
            inspect(&kept(
                &key,
                &[("peersus", b"owner"), ("peersus-ui", b"window")]
            ))
            .is_empty()
        );
        assert_eq!(
            inspect(&kept(
                &key,
                &[("peersus", b"owner"), ("peersus-ui", b"other")]
            )),
            ["peersus-ui"]
        );
        assert_eq!(
            inspect(&kept(&key, &[("peersus-ui", b"window")])),
            ["peersus"]
        );
        // Nothing to put back from: no list, or one this build cannot verify.
        let unlisted = Kept {
            list: None,
            programs: BTreeMap::new(),
        };
        assert!(inspect(&unlisted).is_empty());
        let other = SigningKey::from_bytes(&[8; 32]);
        assert!(inspect(&kept(&other, &[])).is_empty());
    }

    #[test]
    fn a_release_tag_says_one_build_of_one_channel_in_one_spelling() {
        let build = Build {
            version: Version(0, 1, 0),
            number: 12,
        };
        assert_eq!(release_tag(build, "dev"), "v0.1.0-dev.12");
        assert_eq!(tag_identity("v0.1.0-dev.12"), Some((build, "dev")));
        for tag in [
            "0.1.0-dev.12",
            "v0.1-dev.12",
            "v0.01.0-dev.12",
            "v0.1.0-dev.012",
            "v0.1.0-dev.0",
            "v0.1.0-dev",
            "v0.1.0-Dev.12",
            "v0.1.0-de-v.12",
            "v0.1.0-dev.12x",
            "extension/x/v0.1.0",
        ] {
            assert_eq!(tag_identity(tag), None, "{tag}");
        }
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
    async fn discovery_follows_the_newest_tag_of_the_channel_to_its_signed_list() {
        use crate::store_runtime::fake::MapFetcher;
        let tags = serde_json::to_vec(&serde_json::json!([
            {"ref": "refs/tags/extension/thing/v9.0.0", "object": {"sha": "x"}},
            {"ref": "refs/tags/v0.1.0-dev.3"},
            {"ref": "refs/tags/v0.1.0-dev.12"},
            {"ref": "refs/tags/v0.1.0-dev.9"},
            {"ref": "refs/tags/v9.0.0-beta.1"},
            {"ref": "refs/tags/v9.0.0-dev.01"},
            {"ref": "refs/heads/v9.0.0-dev.1"},
        ]))
        .unwrap();
        let listing = |tag: &str, draft: bool, sig_size: u64| {
            serde_json::to_vec(&serde_json::json!({
                "draft": draft, "prerelease": true, "tag_name": tag, "ignored": 1,
                "assets": [
                    {"id": 1, "name": SUMS_FILE, "size": 10, "state": "uploaded"},
                    {"id": 2, "name": SIGNATURE_FILE, "size": sig_size, "state": "uploaded"},
                    {"id": 3, "name": "a.tar.gz", "size": 5, "state": "uploaded"},
                ],
            }))
            .unwrap()
        };
        let files = MapFetcher::default();
        files.put(TAGS_NAME, tags);
        files.put("store-asset-1", b"sums".to_vec());
        files.put("store-asset-2", vec![0; 64]);
        let newest = "app-release-v0.1.0-dev.12";
        files.put(newest, listing("v0.1.0-dev.12", false, 64));
        let found = discover(&files, CHANNEL).await.unwrap().unwrap();
        assert_eq!(found.sums, b"sums");
        assert_eq!(
            found.assets.get("a.tar.gz"),
            Some(&Located { id: 3, size: 5 })
        );
        // Tagged, not published yet.
        files.put(newest, b"null".to_vec());
        assert!(discover(&files, CHANNEL).await.unwrap().is_none());
        for bad in [
            listing("v0.1.0-dev.9", false, 64),
            listing("v0.1.0-dev.12", true, 64),
            listing("v0.1.0-dev.12", false, 65),
            b"{}".to_vec(),
        ] {
            files.put(newest, bad);
            assert!(discover(&files, CHANNEL).await.is_err());
        }
        // Nothing of this channel was ever tagged.
        assert!(discover(&files, "alpha").await.unwrap().is_none());
        assert!(discover(&files, "Bad Channel").await.is_err());
        files.put(TAGS_NAME, b"{}".to_vec());
        assert!(discover(&files, CHANNEL).await.is_err());
    }

    #[test]
    fn each_private_channel_verifies_with_its_own_key() {
        // The one key of this build and the channel it follows are chosen together.
        let development = [
            78, 72, 236, 243, 236, 148, 162, 194, 34, 66, 99, 121, 175, 134, 25, 229, 198, 112, 33,
            147, 24, 145, 16, 45, 224, 247, 82, 209, 40, 170, 162, 210,
        ];
        match super::CHANNEL {
            "pre" => {
                let key = super::UPDATE_PUBLIC_KEY.unwrap();
                assert_ne!(key, development);
                assert!(VerifyingKey::from_bytes(&key).is_ok());
            }
            "dev" => assert_eq!(super::UPDATE_PUBLIC_KEY, Some(development)),
            "stable" => assert_eq!(super::UPDATE_PUBLIC_KEY, None),
            other => panic!("unexpected channel {other}"),
        }
    }
}
