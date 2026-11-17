//! The application's languages as the owner holds them: which files this device has, which the languages branch offers
//! for this release, and which one the windows are shown in.
//!
//! English is written in the code and needs nothing. Every other language is a gettext PO file (see
//! [`peersus_api::language`]) on the repository's `languages` branch, one directory per release:
//! `languages/<release>/<tag>.po`, beside `languages/<release>/languages.json`, the offer: which languages there are, the
//! name each gives itself and how many messages it translates (`cargo xtask languages-offer` writes it, CI checks it).
//! The files are fetched as the extension branch's are: the branch's listing, then each file by its blob id, through the
//! same fetcher as the store (the `peersus-store-fetch` helper on a desktop, the platform's worker on Android), so the
//! owner never opens a connection itself.
//!
//! A language file changes only text the windows show, never what anything does; it is NOT signed. What a downloaded or
//! imported file may carry is bounded and checked: its format, its size, that it is the language it says it is; and the
//! window uses a translation only where its placeholders are exactly the English text's (see `Catalog::problems`).
//!
//! On this device: `<data>/languages/releases/<release>/` holds what was downloaded with each release, and
//! `<data>/languages/imported/` what a person imported. An imported file is never replaced by a download or an update and
//! is shown before a downloaded one of the same language. After an update the previous release's file stays shown until the
//! new release's arrives, then it is let go; what the new release has that it does not is shown in English meanwhile.
use crate::{catalog::Refusal, store_branch, store_runtime::StoreFetcher};
use peersus_api::language::{self, Catalog, Language};
use peersus_core::model::{HeldLanguage, LanguageFile, LanguageState, LanguageStatus};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

/// The release this build is: the directory of the languages branch it reads.
pub const RELEASE: &str = env!("CARGO_PKG_VERSION");
/// The directory of the languages branch that holds one directory per release.
pub const BRANCH_DIRECTORY: &str = "languages";
/// The offer of one release, beside its language files.
pub const OFFER_FILE: &str = "languages.json";
/// Where a person shares a translation: an upload into this release's directory of the languages branch, which a maintainer
/// reviews before it is offered. The repository and branch must stay those `peersus-store-fetch` reads (`STORE_REPOSITORY`,
/// `LANGUAGE_BRANCH`).
pub const SHARE_URL: &str = concat!(
    "https://github.com/DarkPhilosophy/Peersus/upload/languages/languages/",
    env!("CARGO_PKG_VERSION")
);

/// The person's language: the one chosen, else the device's, else English. What the windows want to be shown in, and what
/// an extension is told (`peersus.language`).
pub fn person_language(chosen: Option<Language>, device: Option<Language>) -> Language {
    chosen.or(device).unwrap_or(Language::ENGLISH)
}

/// The largest offer read.
const MAX_OFFER_BYTES: usize = 256 * 1024;
/// The most languages one offer may list.
pub const MAX_OFFERED: usize = 500;
/// The most files of one directory read as held languages.
const MAX_HELD: usize = 200;

const UNREACHABLE: Refusal = Refusal("The languages could not be fetched");
const INVALID: Refusal = Refusal("What the languages branch sent could not be checked");
const NOT_OFFERED: Refusal = Refusal("No languages are offered for this release yet");

/// What one release offers: written beside its files, read before any of them is fetched.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Offer {
    pub release: String,
    /// How many messages the release's template has: what `translated` is counted against.
    pub messages: u32,
    pub languages: Vec<LanguageFile>,
}

impl Offer {
    /// An offer as the branch holds it, refused unless it is for `release`, lists each language once, names each with a short
    /// name and counts no more than the template has.
    pub fn parse(bytes: &[u8], release: &str) -> Result<Self, Refusal> {
        let offer: Self = serde_json::from_slice(bytes).map_err(|_| INVALID)?;
        let mut seen = BTreeSet::new();
        let fits = offer.release == release
            && offer.languages.len() <= MAX_OFFERED
            && offer.languages.iter().all(|file| {
                seen.insert(file.language)
                    && !file.language.is_english()
                    && file.translated <= offer.messages
                    && file.bytes <= language::MAX_FILE_BYTES as u64
                    && peersus_api::name::text(&file.name, language::MAX_NAME_CHARS)
            });
        if fits { Ok(offer) } else { Err(INVALID) }
    }

    /// The offered language for `wanted`: the same tag, else its primary language (`ro` for `ro-ro`).
    pub fn find(&self, wanted: Language) -> Option<&LanguageFile> {
        self.languages
            .iter()
            .find(|file| file.language == wanted)
            .or_else(|| {
                self.languages
                    .iter()
                    .find(|file| file.language == wanted.primary())
            })
    }
}

/// Where a held file is, under the data directory: the one function the owner and a window both use, so that the two can
/// never look in different places.
pub fn held_path(data: &Path, held: &HeldLanguage) -> PathBuf {
    let root = data.join("languages");
    let name = format!("{}.po", held.file.language.tag());
    match &held.release {
        Some(release) => root.join("releases").join(release).join(name),
        None => root.join("imported").join(name),
    }
}
/// The language the environment names, as the C library reads it: the first of `LANGUAGE`, then `LC_ALL`, `LC_MESSAGES`,
/// `LANG`. What a desktop's own language is, where its frontend says nothing else.
pub fn environment_language() -> Option<Language> {
    let first = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    if let Some(list) = first("LANGUAGE")
        && let Some(language) = list.split(':').find_map(Language::from_locale)
    {
        return Some(language);
    }
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(first)
        .and_then(|value| Language::from_locale(&value))
}

/// Whether `text` is a release as the branch names its directories: three numbers.
fn is_release(text: &str) -> bool {
    let parts: Vec<&str> = text.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| (1..=20).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_digit()))
}

/// Reads a file of at most `limit` bytes, refusing a larger one without reading it all.
fn read_bounded(path: &Path, limit: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return None;
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= limit).then_some(bytes)
}

/// Writes a file whole or not at all: a reader never sees half of it.
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let directory = path
        .parent()
        .ok_or_else(|| std::io::Error::other("no directory"))?;
    fs::create_dir_all(directory)?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    std::io::Write::write_all(&mut temporary, bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// What a language file says about itself, read as the window will read it; `None` when it is not one, or is not the
/// language its name says.
fn describe(bytes: &[u8], expected: Option<Language>) -> Option<(Language, LanguageFile)> {
    let text = std::str::from_utf8(bytes).ok()?;
    let catalog = Catalog::parse(text).ok()?;
    let language = catalog.language()?;
    if expected.is_some_and(|expected| expected != language) || language.is_english() {
        return None;
    }
    let (_, translated) = catalog.counts();
    let name = catalog.name().unwrap_or(language.tag()).to_owned();
    Some((
        language,
        LanguageFile {
            language,
            name,
            translated: u32::try_from(translated).unwrap_or(u32::MAX),
            bytes: bytes.len() as u64,
            machine: catalog.machine_translated(),
        },
    ))
}

/// What one fetch brought: the offer, and the file of the language wanted when it is offered.
pub struct Fetched {
    pub offer_bytes: Vec<u8>,
    pub offer: Offer,
    pub file: Option<(Language, Vec<u8>)>,
}

/// The offer of `release` and, if `wanted` (or its primary language) is offered, its file. Only UNTRUSTED bytes come back
/// from the fetcher; each is bounded and checked here before it is kept.
pub async fn fetch(
    fetcher: &dyn StoreFetcher,
    release: &str,
    wanted: Option<Language>,
) -> Result<Fetched, Refusal> {
    let listing = fetcher
        .fetch("languages-tree".into(), store_branch::MAX_INDEX_BYTES)
        .await
        .map_err(|_| UNREACHABLE)?;
    let tree: store_branch::Tree = serde_json::from_slice(&listing).map_err(|_| INVALID)?;
    if tree.truncated {
        return Err(INVALID);
    }
    let directory = format!("{BRANCH_DIRECTORY}/{release}/");
    let blob_of = |name: &str| {
        tree.tree
            .iter()
            .find(|blob| blob.is_file() && blob.path.strip_prefix(&directory) == Some(name))
            .map(|blob| format!("store-blob-{}", blob.sha))
    };
    let offer_name = blob_of(OFFER_FILE).ok_or(NOT_OFFERED)?;
    let offer_bytes = fetcher
        .fetch(offer_name, MAX_OFFER_BYTES)
        .await
        .map_err(|_| UNREACHABLE)?;
    let offer = Offer::parse(&offer_bytes, release)?;
    let mut file = None;
    if let Some(offered) = wanted
        .filter(|wanted| !wanted.is_english())
        .and_then(|wanted| offer.find(wanted))
    {
        let language = offered.language;
        let name = blob_of(&format!("{}.po", language.tag())).ok_or(INVALID)?;
        let bytes = fetcher
            .fetch(name, language::MAX_FILE_BYTES)
            .await
            .map_err(|_| UNREACHABLE)?;
        // The offer says how large the file is, as the person was told before choosing it; another size is not that file.
        if offered.bytes != 0 && offered.bytes != bytes.len() as u64 {
            return Err(INVALID);
        }
        describe(&bytes, Some(language)).ok_or(INVALID)?;
        file = Some((language, bytes));
    }
    Ok(Fetched {
        offer_bytes,
        offer,
        file,
    })
}

/// How old a held offer may be before it is fetched again without being asked (with the shown language's file): the offer
/// of a release does not change often, but its files are improved within a release. Measured by the offer file's time, so
/// it holds across restarts.
const REFETCH_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// The owner's languages.
pub struct Languages {
    data: PathBuf,
    release: String,
    device: Option<Language>,
    held: Vec<HeldLanguage>,
    offer: Option<Offer>,
    /// When the held offer of this release was saved, read from its file.
    offer_saved: Option<std::time::SystemTime>,
    /// A person asked to look again: the next fetch is made whatever is held.
    asked: bool,
    status: LanguageStatus,
    error: Option<&'static str>,
    revision: u64,
    schedule: crate::store_runtime::RefreshSchedule,
}

impl Languages {
    /// What `data` holds for `release`. Reading is all this does.
    pub fn new(data: &Path, release: &str) -> Self {
        let mut languages = Self {
            data: data.to_owned(),
            release: release.to_owned(),
            device: None,
            held: Vec::new(),
            offer: None,
            offer_saved: None,
            asked: false,
            status: LanguageStatus::Idle,
            error: None,
            revision: 0,
            schedule: crate::store_runtime::RefreshSchedule::every(REFETCH_EVERY),
        };
        languages.rescan();
        languages
    }

    fn root(&self) -> PathBuf {
        self.data.join("languages")
    }

    /// Reads again which files are held and the offer of this release.
    fn rescan(&mut self) {
        let root = self.root();
        let mut held = Vec::new();
        let mut directories = vec![(root.join("imported"), None)];
        if let Ok(releases) = fs::read_dir(root.join("releases")) {
            for entry in releases.flatten().take(MAX_HELD) {
                if let Some(release) = entry.file_name().to_str().filter(|name| is_release(name)) {
                    directories.push((entry.path(), Some(release.to_owned())));
                }
            }
        }
        for (directory, release) in directories {
            let Ok(entries) = fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten().take(MAX_HELD) {
                let path = entry.path();
                let Some(tag) = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| name.strip_suffix(".po"))
                else {
                    continue;
                };
                let Ok(expected) = Language::parse(tag) else {
                    continue;
                };
                // A file that is not what its name says is passed over, never shown: it stays for a person to look at.
                let Some((_, file)) = read_bounded(&path, language::MAX_FILE_BYTES)
                    .and_then(|bytes| describe(&bytes, Some(expected)))
                else {
                    continue;
                };
                held.push(HeldLanguage {
                    file,
                    release: release.clone(),
                });
            }
        }
        held.sort_by(|left, right| {
            (left.file.language.tag(), &left.release)
                .cmp(&(right.file.language.tag(), &right.release))
        });
        self.held = held;
        let offer = root.join("releases").join(&self.release).join(OFFER_FILE);
        self.offer = read_bounded(&offer, MAX_OFFER_BYTES)
            .and_then(|bytes| Offer::parse(&bytes, &self.release).ok());
        self.offer_saved = self
            .offer
            .as_ref()
            .and_then(|_| fs::metadata(&offer).and_then(|meta| meta.modified()).ok());
        self.revision = self.revision.wrapping_add(1);
    }

    /// The language wanted: the chosen one, else the device's, else English.
    pub fn wanted(&self, chosen: Option<Language>) -> Language {
        person_language(chosen, self.device)
    }

    /// The held file of `wanted`, by precedence: imported, then this release's, then an earlier release's; the same tag
    /// before its primary language. `None` for English, and when nothing of it is held.
    pub fn shown_file(&self, wanted: Language) -> Option<&HeldLanguage> {
        if wanted.is_english() {
            return None;
        }
        for language in [wanted, wanted.primary()] {
            let of = |release: Option<&str>| {
                self.held.iter().find(|held| {
                    held.file.language == language
                        && match release {
                            None => held.release.is_none(),
                            Some(release) => held.release.as_deref() == Some(release),
                        }
                })
            };
            let earlier = || {
                self.held
                    .iter()
                    .filter(|held| held.file.language == language && held.release.is_some())
                    .max_by_key(|held| {
                        held.release.as_deref().map(|release| {
                            release
                                .split('.')
                                .map(|part| part.parse::<u64>().unwrap_or(0))
                                .collect::<Vec<_>>()
                        })
                    })
            };
            if let Some(found) = of(None)
                .or_else(|| of(Some(&self.release)))
                .or_else(earlier)
            {
                return Some(found);
            }
        }
        None
    }

    /// The text of the held file the windows are shown in; refused while that is English.
    pub fn shown_text(&self, chosen: Option<Language>) -> Result<String, &'static str> {
        let held = self
            .shown_file(self.wanted(chosen))
            .ok_or("The windows are shown in English, which needs no file")?;
        read_bounded(&held_path(&self.data, held), language::MAX_FILE_BYTES)
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .ok_or("The language file could not be read")
    }

    /// The state a window is told.
    pub fn state(&self, chosen: Option<Language>) -> LanguageState {
        let shown_file = self.shown_file(self.wanted(chosen)).cloned();
        LanguageState {
            device: self.device,
            shown: shown_file
                .as_ref()
                .map_or(Language::ENGLISH, |held| held.file.language),
            shown_file,
            held: self.held.clone(),
            offered: self
                .offer
                .as_ref()
                .map(|offer| offer.languages.clone())
                .unwrap_or_default(),
            status: self.status,
            error: self.error.map(str::to_owned),
            revision: self.revision,
        }
    }

    /// The device's language, as its platform says it now. Whether that changed anything.
    pub fn set_device(&mut self, language: Option<Language>) -> bool {
        if self.device == language {
            return false;
        }
        self.device = language;
        // A new language wanted is looked for now, not after the wait a failure set.
        self.schedule = crate::store_runtime::RefreshSchedule::every(REFETCH_EVERY);
        true
    }

    /// Asks for a fetch of the offer and of the shown language's file at the next chance, whatever is held and whatever the
    /// wait: what "Check again" does.
    pub fn ask(&mut self) {
        self.asked = true;
        self.schedule = crate::store_runtime::RefreshSchedule::every(REFETCH_EVERY);
    }

    /// What to fetch now, if anything: the offer when this release's is not held or is older than [`REFETCH_EVERY`], and the
    /// file of the wanted language when it is offered and this release's file of it is not held; both when a person asked
    /// ([`Self::ask`]). `Some(wanted)` starts a fetch (see [`fetch`]); the caller runs it and hands the result to
    /// [`Self::finish`].
    pub fn due(
        &mut self,
        chosen: Option<Language>,
        asked: Option<Language>,
        now: tokio::time::Instant,
    ) -> Option<Option<Language>> {
        if self.status == LanguageStatus::Fetching {
            return None;
        }
        let wanted = asked.unwrap_or_else(|| self.wanted(chosen));
        let missing = !wanted.is_english()
            && self
                .offer
                .as_ref()
                .is_some_and(|offer| offer.find(wanted).is_some())
            && !self.held.iter().any(|held| {
                held.release.as_deref() == Some(self.release.as_str())
                    && (held.file.language == wanted || held.file.language == wanted.primary())
            });
        let stale = self
            .offer_saved
            .is_some_and(|saved| saved.elapsed().is_ok_and(|age| age >= REFETCH_EVERY));
        let requested = asked.is_some() || self.asked;
        let needed = requested || self.offer.is_none() || missing || stale;
        if !needed || (!requested && !self.schedule.due(now)) {
            return None;
        }
        self.asked = false;
        self.schedule.started(now);
        self.status = LanguageStatus::Fetching;
        self.error = None;
        Some((!wanted.is_english()).then_some(wanted))
    }

    /// Keeps what a fetch brought, and lets go of an earlier release's file of the same language once this release's is held.
    pub fn finish(&mut self, result: Result<Fetched, Refusal>, now: tokio::time::Instant) {
        let kept = result.and_then(|fetched| {
            let directory = self.root().join("releases").join(&self.release);
            write_atomically(&directory.join(OFFER_FILE), &fetched.offer_bytes)
                .map_err(|_| Refusal("The languages could not be saved"))?;
            if let Some((language, bytes)) = &fetched.file {
                let held = HeldLanguage {
                    file: LanguageFile {
                        language: *language,
                        name: String::new(),
                        translated: 0,
                        bytes: 0,
                        machine: false,
                    },
                    release: Some(self.release.clone()),
                };
                write_atomically(&held_path(&self.data, &held), bytes)
                    .map_err(|_| Refusal("The languages could not be saved"))?;
                for earlier in self.held.iter().filter(|other| {
                    other.file.language == *language
                        && other
                            .release
                            .as_deref()
                            .is_some_and(|release| release != self.release)
                }) {
                    let path = held_path(&self.data, earlier);
                    let _ = fs::remove_file(&path);
                    if let Some(directory) = path.parent() {
                        // Only when nothing else is left in it: the offer of that release goes with its last file.
                        let empty = fs::read_dir(directory)
                            .map(|entries| {
                                entries
                                    .flatten()
                                    .all(|entry| entry.file_name() == OFFER_FILE)
                            })
                            .unwrap_or(false);
                        if empty {
                            let _ = fs::remove_file(directory.join(OFFER_FILE));
                            let _ = fs::remove_dir(directory);
                        }
                    }
                }
            }
            Ok(())
        });
        self.schedule.finished(kept.is_ok(), now);
        match kept {
            Ok(()) => {
                self.status = LanguageStatus::Idle;
                self.error = None;
            }
            Err(refusal) => {
                self.status = LanguageStatus::Failed;
                self.error = Some(refusal.0);
            }
        }
        self.rescan();
    }

    /// Holds a file a person imported. The language it names is the answer.
    pub fn import(&mut self, text: &str) -> Result<Language, &'static str> {
        if text.len() > language::MAX_FILE_BYTES {
            return Err("The language file is too large");
        }
        let catalog = Catalog::parse(text)
            .map_err(|_| "This is not a language file the application can read")?;
        let language = catalog.language().ok_or(
            "The language file does not say which language it is (its header has no Language)",
        )?;
        if language.is_english() {
            return Err("English is the application's own language and needs no file");
        }
        let held = HeldLanguage {
            file: LanguageFile {
                language,
                name: String::new(),
                translated: 0,
                bytes: 0,
                machine: false,
            },
            release: None,
        };
        write_atomically(&held_path(&self.data, &held), text.as_bytes())
            .map_err(|_| "The language file could not be saved")?;
        self.rescan();
        Ok(language)
    }

    /// Lets go of a held file.
    pub fn remove(&mut self, language: Language, imported: bool) -> Result<(), &'static str> {
        let found: Vec<HeldLanguage> = self
            .held
            .iter()
            .filter(|held| held.file.language == language && held.release.is_none() == imported)
            .cloned()
            .collect();
        if found.is_empty() {
            return Err("That language file is not held");
        }
        for held in &found {
            fs::remove_file(held_path(&self.data, held))
                .map_err(|_| "The language file could not be removed")?;
        }
        self.rescan();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store_runtime::fake::MapFetcher;

    fn ro() -> Language {
        Language::parse("ro").unwrap()
    }

    const ROMANIAN: &str = "msgid \"\"\nmsgstr \"\"\n\"Language: ro\\n\"\n\"X-Language-Name: Română\\n\"\n\nmsgid \"Send\"\nmsgstr \"Trimite\"\n\nmsgid \"Cancel\"\nmsgstr \"\"\n";

    fn offer(release: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "release": release, "messages": 2,
            "languages": [{"language": "ro", "name": "Română", "translated": 1}]
        }))
        .unwrap()
    }

    fn branch(release: &str, file: &str) -> MapFetcher {
        let fetcher = MapFetcher::default();
        let (offer_sha, file_sha) = ("a".repeat(40), "b".repeat(40));
        fetcher.put(
            "languages-tree",
            serde_json::to_vec(&serde_json::json!({"truncated": false, "tree": [
                {"path": format!("languages/{release}/languages.json"), "type": "blob", "sha": offer_sha},
                {"path": format!("languages/{release}/ro.po"), "type": "blob", "sha": file_sha},
            ]}))
            .unwrap(),
        );
        fetcher.put(&format!("store-blob-{offer_sha}"), offer(release));
        fetcher.put(&format!("store-blob-{file_sha}"), file.as_bytes().to_vec());
        fetcher
    }

    #[test]
    fn an_offer_is_for_its_release_and_lists_each_language_once_with_a_short_name() {
        assert!(Offer::parse(&offer("0.1.0"), "0.1.0").is_ok());
        assert_eq!(Offer::parse(&offer("0.1.0"), "0.2.0"), Err(INVALID));
        for bad in [
            serde_json::json!({"release": "0.1.0", "messages": 2, "languages": [
                {"language": "ro", "name": "A", "translated": 1}, {"language": "ro", "name": "B", "translated": 1}]}),
            serde_json::json!({"release": "0.1.0", "messages": 2, "languages": [{"language": "ro", "name": "", "translated": 1}]}),
            serde_json::json!({"release": "0.1.0", "messages": 2, "languages": [{"language": "ro", "name": "A", "translated": 3}]}),
            serde_json::json!({"release": "0.1.0", "messages": 2, "languages": [{"language": "en", "name": "English", "translated": 0}]}),
            serde_json::json!({"release": "0.1.0", "messages": 2, "languages": [], "extra": 1}),
        ] {
            assert!(
                Offer::parse(&serde_json::to_vec(&bad).unwrap(), "0.1.0").is_err(),
                "{bad}"
            );
        }
        let offer = Offer::parse(&offer("0.1.0"), "0.1.0").unwrap();
        assert_eq!(
            offer
                .find(Language::parse("ro-RO").unwrap())
                .unwrap()
                .language,
            ro()
        );
        assert!(offer.find(Language::parse("de").unwrap()).is_none());
    }

    #[tokio::test]
    async fn the_wanted_language_is_fetched_kept_and_shown_and_english_needs_nothing() {
        let data = tempfile::tempdir().unwrap();
        let mut languages = Languages::new(data.path(), "0.1.0");
        // A moment after the schedules made below begin: what a tick sees.
        let now = tokio::time::Instant::now() + Duration::from_secs(1);
        assert_eq!(languages.state(None).shown, Language::ENGLISH);
        assert!(languages.set_device(Some(Language::parse("ro-RO").unwrap())));
        let wanted = languages
            .due(None, None, now)
            .expect("the offer is not held yet");
        assert_eq!(wanted, Some(Language::parse("ro-ro").unwrap()));
        assert_eq!(languages.due(None, None, now), None, "one fetch at a time");
        let fetched = fetch(&branch("0.1.0", ROMANIAN), "0.1.0", wanted).await;
        languages.finish(fetched, now);
        let state = languages.state(None);
        assert_eq!((state.status, state.shown), (LanguageStatus::Idle, ro()));
        let shown = state.shown_file.unwrap();
        assert_eq!(
            (shown.file.name.as_str(), shown.file.translated),
            ("Română", 1)
        );
        assert_eq!(
            std::fs::read_to_string(held_path(data.path(), &shown)).unwrap(),
            ROMANIAN
        );
        assert_eq!(state.offered.len(), 1);
        assert_eq!(languages.due(None, None, now), None, "nothing is missing");
        // Chosen English is English, with the file still held.
        assert_eq!(
            languages.state(Some(Language::ENGLISH)).shown,
            Language::ENGLISH
        );
        // Read again from the disk, as at the next start.
        assert_eq!(
            Languages::new(data.path(), "0.1.0").state(None).held.len(),
            1
        );
    }

    #[tokio::test]
    async fn after_an_update_the_earlier_file_is_shown_until_the_new_one_arrives_then_let_go() {
        let data = tempfile::tempdir().unwrap();
        // A moment after the schedules made below begin: what a tick sees.
        let now = tokio::time::Instant::now() + Duration::from_secs(1);
        let mut old = Languages::new(data.path(), "0.1.0");
        old.finish(
            fetch(&branch("0.1.0", ROMANIAN), "0.1.0", Some(ro())).await,
            now,
        );
        let mut new = Languages::new(data.path(), "0.2.0");
        // A moment after `new`'s schedule began, however long the setup above took on a busy machine.
        let now = tokio::time::Instant::now() + Duration::from_secs(1);
        let state = new.state(Some(ro()));
        assert_eq!(state.shown, ro());
        assert_eq!(state.shown_file.unwrap().release.as_deref(), Some("0.1.0"));
        assert_eq!(
            new.due(Some(ro()), None, now),
            Some(Some(ro())),
            "the new release's offer is fetched"
        );
        new.finish(
            fetch(&branch("0.2.0", ROMANIAN), "0.2.0", Some(ro())).await,
            now,
        );
        let state = new.state(Some(ro()));
        assert_eq!(state.shown_file.unwrap().release.as_deref(), Some("0.2.0"));
        assert_eq!(state.held.len(), 1, "the earlier release's file was let go");
        assert!(!data.path().join("languages/releases/0.1.0").exists());
    }

    #[tokio::test]
    async fn a_failed_or_wrong_fetch_keeps_english_and_tries_again_later() {
        let data = tempfile::tempdir().unwrap();
        let mut languages = Languages::new(data.path(), "0.1.0");
        // A moment after the schedule made above began: what a tick sees.
        let now = tokio::time::Instant::now() + Duration::from_secs(1);
        languages.set_device(Some(ro()));
        assert!(languages.due(None, None, now).is_some());
        languages.finish(
            fetch(&MapFetcher::default(), "0.1.0", Some(ro())).await,
            now,
        );
        let state = languages.state(None);
        assert_eq!(
            (state.status, state.shown),
            (LanguageStatus::Failed, Language::ENGLISH)
        );
        assert_eq!(languages.due(None, None, now), None, "not again at once");
        assert!(
            languages
                .due(None, None, now + Duration::from_secs(61))
                .is_some(),
            "again after a wait"
        );
        // A file that says it is another language is not kept.
        let lying = ROMANIAN.replace("Language: ro", "Language: de");
        languages.finish(
            fetch(&branch("0.1.0", &lying), "0.1.0", Some(ro())).await,
            now,
        );
        assert_eq!(languages.state(None).status, LanguageStatus::Failed);
        assert!(languages.state(None).held.is_empty());
    }

    #[tokio::test]
    async fn check_again_fetches_the_offer_and_the_shown_file_even_when_both_are_held() {
        let data = tempfile::tempdir().unwrap();
        let mut languages = Languages::new(data.path(), "0.1.0");
        let now = tokio::time::Instant::now() + Duration::from_secs(1);
        languages.set_device(Some(ro()));
        let wanted = languages.due(None, None, now).unwrap();
        languages.finish(
            fetch(&branch("0.1.0", ROMANIAN), "0.1.0", wanted).await,
            now,
        );
        assert_eq!(languages.due(None, None, now), None, "nothing is missing");
        languages.ask();
        let wanted = languages.due(None, None, now);
        assert_eq!(wanted, Some(Some(ro())), "asked: fetched although held");
        // The branch's file was improved within the release; the held one is replaced by it.
        let improved = ROMANIAN.replace(
            "msgid \"Cancel\"\nmsgstr \"\"",
            "msgid \"Cancel\"\nmsgstr \"Anulează\"",
        );
        languages.finish(
            fetch(&branch("0.1.0", &improved), "0.1.0", wanted.unwrap()).await,
            now,
        );
        assert_eq!(languages.shown_text(None).unwrap(), improved);
        assert_eq!(languages.due(None, None, now), None, "one ask, one fetch");
    }

    #[tokio::test]
    async fn an_offer_older_than_a_day_is_fetched_again_at_the_next_start() {
        let data = tempfile::tempdir().unwrap();
        let mut languages = Languages::new(data.path(), "0.1.0");
        let now = tokio::time::Instant::now() + Duration::from_secs(1);
        languages.set_device(Some(ro()));
        let wanted = languages.due(None, None, now).unwrap();
        languages.finish(
            fetch(&branch("0.1.0", ROMANIAN), "0.1.0", wanted).await,
            now,
        );
        let mut fresh = Languages::new(data.path(), "0.1.0");
        // Ticks are taken after each new start, so that only the offer's age decides.
        let now = tokio::time::Instant::now() + Duration::from_secs(1);
        fresh.set_device(Some(ro()));
        assert_eq!(
            fresh.due(None, None, now),
            None,
            "a fresh offer is not fetched again"
        );
        let offer = data
            .path()
            .join("languages/releases/0.1.0")
            .join(OFFER_FILE);
        std::fs::File::options()
            .write(true)
            .open(&offer)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - REFETCH_EVERY - Duration::from_secs(60))
            .unwrap();
        let mut old = Languages::new(data.path(), "0.1.0");
        let now = tokio::time::Instant::now() + Duration::from_secs(1);
        old.set_device(Some(ro()));
        assert_eq!(
            old.due(None, None, now),
            Some(Some(ro())),
            "an old offer is"
        );
    }

    #[test]
    fn an_imported_file_is_shown_before_a_download_and_can_be_removed() {
        let data = tempfile::tempdir().unwrap();
        let mut languages = Languages::new(data.path(), "0.1.0");
        assert!(languages.import("not a po file \"").is_err());
        assert!(
            languages.import("msgid \"a\"\nmsgstr \"b\"\n").is_err(),
            "no Language in its header"
        );
        assert_eq!(languages.import(ROMANIAN), Ok(ro()));
        let state = languages.state(Some(ro()));
        assert_eq!(state.shown_file.unwrap().release, None);
        assert_eq!(
            languages.remove(ro(), false),
            Err("That language file is not held")
        );
        assert_eq!(languages.remove(ro(), true), Ok(()));
        assert_eq!(languages.state(Some(ro())).shown, Language::ENGLISH);
    }
}
