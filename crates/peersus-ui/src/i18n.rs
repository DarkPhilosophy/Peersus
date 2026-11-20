//! The interface's words: English, written in the code, and the language the owner says the windows are shown in, read from
//! the language file it holds (see `peersus_app::languages`), whose text a window asks for (`Command::LanguageText`).
//! User-controlled names and paths never pass through here.
//!
//! A message is its English text. The template of every message is `languages/messages.pot`, the file translators start
//! from; a language file translates some or all of it, and whatever it leaves out, or translates with placeholders that are
//! not exactly the English text's, is shown in English. Each distinct file is read once and kept for the life of the
//! process: a window asks for `&str`s it holds while it draws them, and a person changes language or imports a file rarely.
use freya::prelude::{State, try_consume_context};
use peersus_api::language::{Catalog, PluralRule};
use peersus_core::model::LanguageState;
use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    sync::{Mutex, OnceLock, PoisonError, RwLock},
};

pub use peersus_core::config::Language;

/// The language the window draws in, and the owner's revision of its files: a window redraws when either changes, so a
/// file imported again over the one shown is shown at once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Shown {
    pub language: Language,
    pub revision: u64,
}

#[derive(Clone, Copy)]
pub(crate) struct Current(pub State<Shown>);

pub fn language() -> Language {
    try_consume_context::<Current>()
        .map(|current| current.0().language)
        .unwrap_or_default()
}

/// The template as written: what a person saves to start a translation from.
pub const TEMPLATE_TEXT: &str = include_str!("../languages/messages.pot");

/// The template: every message there is, in English.
fn template() -> &'static Catalog {
    static TEMPLATE: OnceLock<Catalog> = OnceLock::new();
    TEMPLATE.get_or_init(|| {
        // Checked by a test: the template always reads. An empty catalog is the answer to the impossible case.
        Catalog::parse(TEMPLATE_TEXT).unwrap_or_default()
    })
}

/// The catalog each language is read from now, by language.
fn catalogs() -> &'static RwLock<HashMap<Language, &'static Catalog>> {
    static CATALOGS: OnceLock<RwLock<HashMap<Language, &'static Catalog>>> = OnceLock::new();
    CATALOGS.get_or_init(RwLock::default)
}

fn catalog(language: Language) -> Option<&'static Catalog> {
    if language.is_english() {
        return None;
    }
    catalogs()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&language)
        .copied()
}

/// The most distinct language files one process reads. Each read is kept until the process ends (see the module's words),
/// in one of these places; past them a file is refused until the application starts again.
const MOST_FILES: usize = 32;

/// Reads `text` as the file of the language it names, keeps what fits the template, and makes it the one that language is
/// shown from. A text read before is not read again.
pub fn install(text: &str) -> Result<Language, &'static str> {
    static PLACES: [OnceLock<(u64, Language, Catalog)>; MOST_FILES] =
        [const { OnceLock::new() }; MOST_FILES];
    static FILLING: Mutex<()> = Mutex::new(());
    let digest = {
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        hasher.finish()
    };
    // One file is read at a time, so that two windows given the same text keep one copy.
    let _filling = FILLING.lock().unwrap_or_else(PoisonError::into_inner);
    let found = PLACES
        .iter()
        .map_while(OnceLock::get)
        .find(|(seen, ..)| *seen == digest);
    let (_, language, kept) = match found {
        Some(found) => found,
        None => {
            let catalog = Catalog::parse(text)
                .map_err(|_| "This is not a language file the application can read")?;
            let language = catalog
                .language()
                .filter(|language| !language.is_english())
                .ok_or("The language file does not say which language it is")?;
            let (kept, _) = catalog.without_problems(Some(template()));
            let place = PLACES.iter().find(|place| place.get().is_none()).ok_or(
                "Too many language files were read; start the application again to read another",
            )?;
            place.get_or_init(|| (digest, language, kept))
        }
    };
    let (language, kept) = (*language, kept);
    catalogs()
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(language, kept);
    Ok(language)
}

/// The owner's revision each language's file was installed for.
fn installed() -> &'static Mutex<HashMap<Language, u64>> {
    static INSTALLED: OnceLock<Mutex<HashMap<Language, u64>>> = OnceLock::new();
    INSTALLED.get_or_init(Mutex::default)
}

/// What a window shows for the owner's `state`: the language shown, once its file was installed for this revision (see
/// [`take_text`]); English until then, and when the owner shows English.
pub fn shown(state: &LanguageState) -> Shown {
    let ready = state.shown_file.is_some()
        && installed()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&state.shown)
            == Some(&state.revision);
    Shown {
        language: if ready {
            state.shown
        } else {
            Language::ENGLISH
        },
        revision: state.revision,
    }
}

/// Whether the window must ask for the text of the shown file before it can show `state`.
pub fn needs_text(state: &LanguageState) -> bool {
    state.shown_file.is_some() && shown(state).language != state.shown
}

/// Installs the text the owner gave for `state` (the answer to `LanguageText`), and says what the window shows now. A text
/// that is not the language the owner said leaves English.
pub fn take_text(state: &LanguageState, text: &str) -> Shown {
    if install(text) == Ok(state.shown) {
        installed()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(state.shown, state.revision);
    }
    shown(state)
}

/// What a window connected to the owner shows for `state`: asks the owner for the shown file's text first when this process
/// has not read it for this revision. Both the main window and the offer panel follow the owner through this.
pub async fn follow(client: &peersus_app::runtime::Client, state: &LanguageState) -> Shown {
    if needs_text(state)
        && let Ok(peersus_app::runtime::Reply::LanguageText(text)) = client
            .command(peersus_core::model::Command::LanguageText {})
            .await
    {
        return take_text(state, &text);
    }
    shown(state)
}

pub fn tr(english: &str) -> &str {
    translate(language(), english)
}

pub fn translate(language: Language, english: &str) -> &str {
    catalog(language)
        .and_then(|catalog| catalog.get(None, english))
        .unwrap_or(english)
}

/// Whether a text is a message of the template, which is the only way to tell one that is missing from one that reads the
/// same in every language.
pub fn has(english: &str) -> bool {
    template().contains(None, english)
}

/// How many messages there are: what a language file's count of translations is measured against.
pub fn messages() -> u32 {
    u32::try_from(template().counts().0).unwrap_or(u32::MAX)
}

/// Expand the translated template once. Inserted data is never parsed as another placeholder.
pub fn format(language: Language, english: &str, arguments: &[(&str, &str)]) -> String {
    expand(translate(language, english), arguments)
}

fn expand(mut remaining: &str, arguments: &[(&str, &str)]) -> String {
    let mut result = String::with_capacity(remaining.len());
    while let Some(start) = remaining.find('{') {
        result.push_str(&remaining[..start]);
        let tail = &remaining[start + 1..];
        let Some(end) = tail.find('}') else {
            result.push_str(&remaining[start..]);
            return result;
        };
        let key = &tail[..end];
        if let Some((_, value)) = arguments.iter().find(|(name, _)| *name == key) {
            result.push_str(value);
        } else {
            result.push_str(&remaining[start..start + end + 2]);
        }
        remaining = &tail[end + 1..];
    }
    result.push_str(remaining);
    result
}

/// The form of a counted message for `count`, `{count}` filled in: the language's own, else English's (`one` for 1, `other`
/// for the rest).
pub fn plural(
    language: Language,
    context: Option<&str>,
    one: &str,
    other: &str,
    count: u64,
) -> String {
    let text = catalog(language)
        .and_then(|catalog| catalog.plural(context, one, count))
        .unwrap_or_else(|| {
            if PluralRule::one_other().form(count) == 0 {
                one
            } else {
                other
            }
        });
    expand(text, &[("count", &count.to_string())])
}

pub fn files(language: Language, count: usize) -> String {
    plural(
        language,
        None,
        "{count} file",
        "{count} files",
        count as u64,
    )
}

#[derive(Clone, Copy)]
pub enum TimeUnit {
    Minute,
    Hour,
    Day,
    Month,
    Year,
}

pub fn ago(language: Language, count: u64, unit: TimeUnit) -> String {
    let (one, other) = match unit {
        TimeUnit::Minute => ("{count} minute", "{count} minutes"),
        TimeUnit::Hour => ("{count} hour", "{count} hours"),
        TimeUnit::Day => ("{count} day", "{count} days"),
        TimeUnit::Month => ("{count} month", "{count} months"),
        TimeUnit::Year => ("{count} year", "{count} years"),
    };
    let duration = plural(language, Some("ago"), one, other, count);
    format(language, "{duration} ago", &[("duration", &duration)])
}

/// Language files for tests: a few messages in a few languages, the same in every test, installed once.
#[cfg(any(test, feature = "headless"))]
pub mod fixture {
    use super::*;

    /// Installs the test languages (once) and gives the one of `tag`.
    pub fn language(tag: &str) -> Language {
        static INSTALLED: OnceLock<()> = OnceLock::new();
        INSTALLED.get_or_init(|| {
            for text in [
                include_str!("i18n/fixtures/ro.po"),
                include_str!("i18n/fixtures/de.po"),
                include_str!("i18n/fixtures/pl.po"),
                include_str!("i18n/fixtures/cs.po"),
                include_str!("i18n/fixtures/sv.po"),
            ] {
                install(text).expect("a test language file reads");
            }
        });
        Language::parse(tag).expect("a test language tag")
    }
}

#[cfg(test)]
#[path = "i18n/coverage.rs"]
mod coverage;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_forms_follow_the_selected_language() {
        for (count, romanian, polish, czech) in [
            (1, "1 fișier", "1 plik", "1 soubor"),
            (2, "2 fișiere", "2 pliki", "2 soubory"),
            (12, "12 fișiere", "12 plików", "12 souborů"),
            (20, "20 de fișiere", "20 plików", "20 souborů"),
            (22, "22 de fișiere", "22 pliki", "22 souborů"),
            (101, "101 fișiere", "101 plików", "101 souborů"),
        ] {
            assert_eq!(files(fixture::language("ro"), count), romanian);
            assert_eq!(files(fixture::language("pl"), count), polish);
            assert_eq!(files(fixture::language("cs"), count), czech);
        }
    }

    #[test]
    fn translated_sentences_do_not_reinterpret_device_names() {
        assert_eq!(
            format(
                fixture::language("ro"),
                "{name} wants to send you a file",
                &[("name", "Send {name} {files}")]
            ),
            "Send {name} {files} vrea să îți trimită un fișier"
        );
        assert_eq!(
            format(
                fixture::language("de"),
                "{name} wants to send you a file",
                &[("name", "{name}")]
            ),
            "{name} möchte dir eine Datei senden"
        );
    }

    #[test]
    fn elapsed_time_uses_language_specific_order_and_inflection() {
        assert_eq!(
            ago(fixture::language("ro"), 20, TimeUnit::Minute),
            "acum 20 de minute"
        );
        assert_eq!(
            ago(fixture::language("de"), 2, TimeUnit::Day),
            "vor 2 Tagen"
        );
        assert_eq!(
            ago(fixture::language("pl"), 12, TimeUnit::Month),
            "12 miesięcy temu"
        );
        assert_eq!(
            ago(fixture::language("cs"), 3, TimeUnit::Hour),
            "před 3 hodinami"
        );
        assert_eq!(
            ago(fixture::language("sv"), 1, TimeUnit::Year),
            "för 1 år sedan"
        );
    }

    /// The Updates section picks its sentence and its button by rule, not as a literal the sentence scan (`coverage.rs`) can
    /// find: every one of them, for every place an update can be, must be a message of the catalog. And
    /// the rules themselves: nothing at all in a build without updates, never a button while a step
    /// runs, and no switches where the installation does not replace itself.
    #[test]
    fn the_updates_section_is_worded_for_every_state_and_offers_one_thing_at_a_time() {
        use crate::view::{UpdateAction, update_panel};
        use peersus_core::model::{UpdateState, UpdateStatus};
        let at = |status, offered: Option<&str>| UpdateState {
            status,
            current: "0.1.0".into(),
            offered: offered.map(str::to_owned),
            offered_bytes: offered.map_or(0, |_| 25 * 1024 * 1024),
            notes: Some("What changed".into()),
            error: Some("Why".into()),
        };
        assert_eq!(update_panel(&at(UpdateStatus::NotConfigured, None)), None);
        for (status, offered, action, said, switches) in [
            (UpdateStatus::Unsupported, None, None, true, false),
            (
                UpdateStatus::Idle,
                None,
                Some(UpdateAction::Check),
                false,
                true,
            ),
            (UpdateStatus::Checking, None, None, false, true),
            (
                UpdateStatus::Current,
                None,
                Some(UpdateAction::Check),
                false,
                true,
            ),
            (
                UpdateStatus::Available,
                Some("0.2.0"),
                Some(UpdateAction::Install),
                false,
                true,
            ),
            (UpdateStatus::Downloading, Some("0.2.0"), None, false, true),
            (UpdateStatus::Installing, Some("0.2.0"), None, false, true),
            (
                UpdateStatus::Installed,
                None,
                Some(UpdateAction::Restart),
                false,
                true,
            ),
            (UpdateStatus::Restarting, None, None, false, true),
            (
                UpdateStatus::Failed,
                Some("0.2.0"),
                Some(UpdateAction::Install),
                true,
                true,
            ),
            (
                UpdateStatus::Failed,
                None,
                Some(UpdateAction::Check),
                true,
                true,
            ),
        ] {
            let panel = update_panel(&at(status, offered)).expect("a section");
            assert!(has(panel.line), "{}", panel.line);
            assert_eq!(panel.action, action, "{status:?}");
            assert_eq!(panel.said.is_some(), said, "{status:?}");
            assert_eq!(panel.switches, switches, "{status:?}");
            // The publisher's words belong to the offer, and are gone with it.
            assert_eq!(panel.notes.is_some(), status == UpdateStatus::Available);
            // What the update downloads is said exactly while "Update now" can be pressed.
            assert_eq!(
                panel.bytes > 0,
                action == Some(UpdateAction::Install),
                "{status:?}"
            );
            if let Some(action) = panel.action {
                assert!(has(action.label()), "{}", action.label());
            }
        }
        for text in [
            "UPDATES",
            "Update automatically",
            "Restart after an update",
            "On: a verified update of Peersus is downloaded and put in place without asking.",
            "On: Peersus starts again by itself once an update is in place, when nothing is being sent or received.",
        ] {
            assert!(has(text), "{text}");
        }
        assert_eq!(
            format(
                fixture::language("ro"),
                "Version {version} is available.",
                &[("version", "0.2.0 (build 3)")]
            ),
            "Versiunea 0.2.0 (build 3) este disponibilă."
        );
    }

    #[test]
    fn the_overview_says_where_updates_stand_in_every_build_and_every_state() {
        use crate::view::{running_version, update_word};
        use peersus_core::model::{UpdateState, UpdateStatus};
        let all = [
            UpdateStatus::NotConfigured,
            UpdateStatus::Unsupported,
            UpdateStatus::Idle,
            UpdateStatus::Checking,
            UpdateStatus::Current,
            UpdateStatus::Available,
            UpdateStatus::Downloading,
            UpdateStatus::Installing,
            UpdateStatus::Installed,
            UpdateStatus::Restarting,
            UpdateStatus::Failed,
        ];
        for status in all {
            let word = update_word(status);
            assert!(has(word), "{word}");
        }
        // A build without updates says so, where the Updates section draws nothing.
        assert_eq!(
            update_word(UpdateStatus::NotConfigured),
            "not in this build"
        );
        assert_eq!(
            translate(fixture::language("ro"), update_word(UpdateStatus::Current)),
            "la zi"
        );
        for text in ["Version", "Updates"] {
            assert!(has(text), "{text}");
        }
        let mut state = UpdateState::default();
        assert_eq!(running_version(&state), env!("CARGO_PKG_VERSION"));
        state.current = "0.1.0 (build 3)".to_owned();
        assert_eq!(running_version(&state), "0.1.0 (build 3)");
    }
}
