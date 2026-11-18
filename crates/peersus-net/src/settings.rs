//! Validated atomic settings persistence shared by every application frontend.
use peersus_core::{Settings, SettingsError, config::SettingsPatch};
use std::{
    fs::File,
    io::{self, Read},
    path::PathBuf,
};

// Includes the bounded named-theme library; enough for 128 complete palettes.
const MAX_BYTES: u64 = 512 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("preferences could not be read or saved")]
    Io(#[source] io::Error),
    #[error("preferences are invalid; the existing file was preserved")]
    Decode(#[source] serde_json::Error),
    #[error(transparent)]
    Invalid(#[from] SettingsError),
    #[error("preferences exceed the supported size")]
    Oversized,
}
impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

/// One host owns this store; clients submit mutations to that host.
/// No private identity or plugin content is stored in this file.
pub struct SettingsStore {
    path: PathBuf,
    current: Settings,
    /// Where an unusable file was put when the store started on defaults instead, if one was.
    set_aside: Option<PathBuf>,
}

/// The names of the fields `T` reads, as its own `Deserialize` says them.
///
/// Asked of the type, and not worked out from what its default writes or kept as a list here: a field that is not
/// written while it holds its default is not in the default's serialization, and a list of such fields written by hand
/// was four fields behind, so that each of them was forgotten at every start. Empty for a type that does not read as
/// a plain struct, which its caller takes as "not known" and never as "no fields".
fn fields_of<T: for<'de> serde::Deserialize<'de>>() -> &'static [&'static str] {
    /// Refuses everything: only what a struct says of itself on its way to being refused is wanted.
    #[derive(Debug)]
    struct Nothing;
    impl std::fmt::Display for Nothing {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("nothing is read")
        }
    }
    impl std::error::Error for Nothing {}
    impl serde::de::Error for Nothing {
        fn custom<M: std::fmt::Display>(_: M) -> Self {
            Self
        }
    }
    struct Asked<'a>(&'a std::cell::Cell<&'static [&'static str]>);
    impl<'de> serde::Deserializer<'de> for Asked<'_> {
        type Error = Nothing;
        fn deserialize_any<V: serde::de::Visitor<'de>>(self, _: V) -> Result<V::Value, Nothing> {
            Err(Nothing)
        }
        fn deserialize_struct<V: serde::de::Visitor<'de>>(
            self,
            _: &'static str,
            fields: &'static [&'static str],
            _: V,
        ) -> Result<V::Value, Nothing> {
            self.0.set(fields);
            Err(Nothing)
        }
        serde::forward_to_deserialize_any! {
            bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf option unit
            unit_struct newtype_struct seq tuple tuple_struct map enum identifier ignored_any
        }
    }
    let fields = std::cell::Cell::new(&[][..]);
    let _ = T::deserialize(Asked(&fields));
    fields.get()
}

/// Keep only the keys that `Settings` has (and, one level down, that `NetworkSettings` has).
///
/// A file written by an older program can hold fields this one no longer has. They say nothing
/// that matters now, and the values beside them do (an address the person bound to, a relay they
/// turned off), so the unknown keys are dropped and the rest is kept. The schema itself still
/// refuses unknown keys, which is what lets a mistyped setting be caught where one is entered.
fn known_fields_only(mut value: serde_json::Value) -> serde_json::Value {
    // With no names to go by nothing is dropped: the schema then judges the file as it is.
    let keep = |found: &mut serde_json::Map<String, serde_json::Value>, known: &[&str]| {
        if !known.is_empty() {
            found.retain(|key, _| known.contains(&key.as_str()));
        }
    };
    if let Some(found) = value.as_object_mut() {
        keep(found, fields_of::<Settings>());
        if let Some(network) = found
            .get_mut("network")
            .and_then(|network| network.as_object_mut())
        {
            keep(network, fields_of::<peersus_core::NetworkSettings>());
        }
    }
    value
}

/// The settings that can be made of `value`, each key kept if it makes a valid settings on its
/// own beside the defaults, and left at its default if not. The keys are tried one at a time
/// because a wrong value must reset itself and nothing else. Anything that is not an object
/// gives the defaults.
fn repaired(value: serde_json::Value) -> Settings {
    let defaults = serde_json::to_value(Settings::default()).unwrap_or_default();
    let (Some(found), Some(base)) = (value.as_object(), defaults.as_object()) else {
        return Settings::default();
    };
    let accepts = |candidate: &serde_json::Map<String, serde_json::Value>| {
        serde_json::from_value::<Settings>(serde_json::Value::Object(candidate.clone()))
            .is_ok_and(|settings| settings.validate().is_ok())
    };
    let mut kept = base.clone();
    for (key, wanted) in found {
        if key == "network" {
            // The two halves of the network policy are tried apart as well: a bind address that
            // cannot be used must not take with it a relay the person turned off.
            let (Some(wanted), Some(mut network)) = (
                wanted.as_object(),
                base.get("network").and_then(|n| n.as_object()).cloned(),
            ) else {
                continue;
            };
            for (field, value) in wanted {
                let mut trial = kept.clone();
                let mut with = network.clone();
                with.insert(field.clone(), value.clone());
                trial.insert("network".into(), serde_json::Value::Object(with.clone()));
                if accepts(&trial) {
                    network = with;
                }
            }
            kept.insert("network".into(), serde_json::Value::Object(network));
            continue;
        }
        let mut trial = kept.clone();
        trial.insert(key.clone(), wanted.clone());
        if accepts(&trial) {
            kept = trial;
        }
    }
    serde_json::from_value(serde_json::Value::Object(kept)).unwrap_or_default()
}
/// A name beside `path` for a file that is being put aside, that no file already has: the first
/// of `preferences.json.unusable-1`, `-2`, and so on that is free. Earlier ones are never touched.
fn set_aside_name(path: &std::path::Path) -> io::Result<PathBuf> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("preferences path has no file name"))?
        .to_string_lossy()
        .into_owned();
    (1..10_000)
        .map(|n| path.with_file_name(format!("{name}.unusable-{n}")))
        .find(|candidate| std::fs::symlink_metadata(candidate).is_err())
        .ok_or_else(|| io::Error::other("too many unusable preference files"))
}

impl SettingsStore {
    /// What was saved, or the defaults, and always something the program can start with.
    ///
    /// A missing file is a first run. A file with fields it no longer knows is read for what it
    /// still says. A file that cannot be used at all (not JSON, values out of range, too large)
    /// is moved to a name of its own, never deleted and never overwritten, and the program starts
    /// on the defaults: a program that stops over its settings serves nobody. Where the file was
    /// put is available from [`SettingsStore::set_aside`], so that it can be said.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, Error> {
        let path = path.into();
        let read = match File::open(&path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
                Some(bytes)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let Some(bytes) = read else {
            return Ok(Self {
                path,
                current: Settings::default(),
                set_aside: None,
            });
        };
        let parsed = (bytes.len() as u64 <= MAX_BYTES)
            .then(|| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .flatten()
            .map(known_fields_only);
        // Whole and valid: use it as it is.
        let whole = parsed
            .clone()
            .and_then(|value| serde_json::from_value::<Settings>(value).ok())
            .filter(|settings| settings.validate().is_ok());
        if let Some(current) = whole {
            return Ok(Self {
                path,
                current,
                set_aside: None,
            });
        }
        // Something in it is wrong. Keep every value that is right and reset only the ones that
        // are not, so that a wrong picture scale can never switch the relays back on. If it is
        // not even JSON, there is nothing to keep.
        let current = parsed.map(repaired).unwrap_or_default();
        // What was there is kept, never deleted and never overwritten, and the program uses a good
        // file from here on.
        let aside = set_aside_name(&path)?;
        std::fs::rename(&path, &aside)?;
        let mut store = Self {
            path,
            current: Settings::default(),
            set_aside: Some(aside),
        };
        store.replace(current)?;
        Ok(store)
    }

    /// Where an unusable preferences file was put when the store started on defaults instead.
    pub fn set_aside(&self) -> Option<&std::path::Path> {
        self.set_aside.as_deref()
    }

    pub fn current(&self) -> &Settings {
        &self.current
    }

    /// Save only a completely validated candidate. Memory changes after publication.
    pub fn update(&mut self, patch: &SettingsPatch) -> Result<&Settings, Error> {
        let candidate = patch.apply(&self.current)?;
        self.replace(candidate)
    }

    /// GUI and command handlers use the same complete-value transaction.
    pub fn replace(&mut self, candidate: Settings) -> Result<&Settings, Error> {
        candidate.validate()?;
        let mut bytes = serde_json::to_vec_pretty(&candidate).map_err(Error::Decode)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(Error::Oversized);
        }
        bytes.push(b'\n');
        crate::atomic::write(&self.path, &bytes)?;
        self.current = candidate;
        Ok(&self.current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peersus_core::config::BindChange;
    use std::fs;

    #[test]
    fn desktop_arrangement_survives_reopening() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        let placements = vec![peersus_core::layout::Placement {
            device: "paired-device".into(),
            x: 1920,
            y: -300,
        }];
        let mut store = SettingsStore::open(&path).unwrap();
        store
            .update(&SettingsPatch {
                layout: Some(Box::new(placements.clone())),
                ..Default::default()
            })
            .unwrap();
        drop(store);
        let reopened = SettingsStore::open(&path).unwrap();
        assert_eq!(reopened.current().layout, placements);
        assert!(reopened.set_aside().is_none());
    }
    #[test]
    fn the_known_fields_are_the_schemas_own_and_include_those_not_written_at_their_default() {
        let settings = fields_of::<Settings>();
        let written = serde_json::to_value(Settings::default()).unwrap();
        for key in written.as_object().unwrap().keys() {
            assert!(
                settings.contains(&key.as_str()),
                "{key} is written and not known"
            );
        }
        for unwritten in [
            "language",
            "custom_theme",
            "saved_themes",
            "layout",
            "auto_update",
            "nearby_send",
            "private_mode",
            "notice_place",
            "update_automatically",
            "restart_after_update",
        ] {
            assert!(settings.contains(&unwritten), "{unwritten} is not known");
        }
        let network = fields_of::<peersus_core::NetworkSettings>();
        for field in ["bind", "allow_relay", "relay_url", "relay_policy"] {
            assert!(network.contains(&field), "{field} is not known");
        }
        // A type that is not a plain struct says nothing, which is "not known" and drops nothing.
        assert!(fields_of::<u8>().is_empty());
    }

    #[test]
    fn a_choice_that_is_not_written_while_at_its_default_survives_reopening() {
        use peersus_core::config::NoticePlace;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        let mut store = SettingsStore::open(&path).unwrap();
        store
            .update(&SettingsPatch {
                nearby_send: Some(true),
                notice_place: Some(NoticePlace::Top),
                update_automatically: Some(true),
                restart_after_update: Some(true),
                ..SettingsPatch::default()
            })
            .unwrap();
        let chosen = store.current().clone();
        assert!(chosen.nearby_send && chosen.update_automatically && chosen.restart_after_update);
        assert_eq!(chosen.notice_place, NoticePlace::Top);
        drop(store);
        let reopened = SettingsStore::open(&path).unwrap();
        assert_eq!(reopened.current(), &chosen);
        assert!(reopened.set_aside().is_none());
        // And a key no program has is still dropped, with the rest kept.
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value.as_object_mut().unwrap().insert(
            "from_an_older_program".into(),
            serde_json::Value::Bool(true),
        );
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let reopened = SettingsStore::open(&path).unwrap();
        assert_eq!(reopened.current(), &chosen);
        assert!(reopened.set_aside().is_none());
    }
    #[test]
    fn every_language_choice_survives_reopening_without_losing_other_preferences() {
        use peersus_core::config::Language;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        let tags =
            ["en", "ro", "pt-br", "sr-latn-rs"].map(|tag| Some(Language::parse(tag).unwrap()));
        for language in tags.into_iter().chain([None]) {
            let mut store = SettingsStore::open(&path).unwrap();
            store
                .update(&SettingsPatch {
                    language: Some(language),
                    allow_relay: Some(false),
                    ..Default::default()
                })
                .unwrap();
            drop(store);
            let reopened = SettingsStore::open(&path).unwrap();
            assert_eq!(reopened.current().language, language);
            assert!(!reopened.current().network.allow_relay);
            assert!(
                reopened.set_aside().is_none(),
                "a supported language is not a damaged preference"
            );
        }
    }

    #[test]
    fn cli_patch_and_ui_replacement_survive_reopening() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        let mut cli = SettingsStore::open(&path).unwrap();
        cli.update(&SettingsPatch {
            bind: Some(BindChange::Address("127.0.0.1".parse().unwrap())),
            allow_relay: Some(false),
            background: Some(true),
            ..Default::default()
        })
        .unwrap();
        let mut ui = SettingsStore::open(&path).unwrap();
        assert!(ui.current().background);
        assert_eq!(
            ui.current().network.bind,
            Some("127.0.0.1".parse().unwrap())
        );
        let mut changed = ui.current().clone();
        changed.background = false;
        changed.network.bind = None;
        ui.replace(changed.clone()).unwrap();
        assert_eq!(SettingsStore::open(path).unwrap().current(), &changed);
    }

    #[test]
    fn rejected_update_preserves_disk_and_memory() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        let mut store = SettingsStore::open(&path).unwrap();
        store
            .update(&SettingsPatch {
                background: Some(true),
                ..Default::default()
            })
            .unwrap();
        let before = fs::read(&path).unwrap();
        let current = store.current().clone();
        assert!(
            store
                .update(&SettingsPatch {
                    scale: Some(10.0),
                    ..Default::default()
                })
                .is_err()
        );
        assert_eq!(fs::read(path).unwrap(), before);
        assert_eq!(store.current(), &current);
    }

    #[test]
    fn unusable_preferences_are_set_aside_not_lost_and_the_program_starts_on_the_defaults() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        fs::write(&path, b"not json").unwrap();
        let store = SettingsStore::open(&path).expect("a bad file must never stop the program");
        assert_eq!(store.current(), &Settings::default());
        // What was there is kept, under another name, exactly as it was.
        let kept = store.set_aside().expect("it must say it set a file aside");
        assert_eq!(fs::read(kept).unwrap(), b"not json");
        // And the file the program uses is a good one again.
        assert!(serde_json::from_slice::<Settings>(&fs::read(&path).unwrap()).is_ok());
    }

    #[test]
    fn a_file_from_an_older_program_with_fields_that_are_gone_keeps_what_is_still_known() {
        // The exact shape found on a real machine: two fields this program no longer has.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        fs::write(
            &path,
            br#"{"accent":[96,212,180],"background":true,"clipboard_enabled":false,
               "device_name":"Alex laptop","network":{"bind":"127.0.0.1","allow_relay":false,
               "local_only":false},"reduced_motion":true,"renderer":"auto","scale":1.0,
               "theme":"light","version":1}"#,
        )
        .unwrap();
        let store = SettingsStore::open(&path).expect("must open");
        let settings = store.current();
        assert_eq!(settings.device_name, "Alex laptop");
        assert!(settings.background && settings.reduced_motion);
        assert_eq!(settings.network.bind, Some("127.0.0.1".parse().unwrap()));
        assert!(!settings.network.allow_relay);
        assert_eq!(settings.theme, peersus_core::config::Theme::Light);
        // Nothing was lost, so nothing was set aside.
        assert!(store.set_aside().is_none());
    }

    #[test]
    fn a_bad_unrelated_value_resets_that_value_and_never_loosens_the_network_policy() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        let mut chosen = Settings::default();
        chosen.network.bind = Some("127.0.0.1".parse().unwrap());
        chosen.network.allow_relay = false;
        chosen.device_name = "Alex laptop".into();
        let mut bad = serde_json::to_value(&chosen).unwrap();
        bad["scale"] = serde_json::json!(50.0);
        fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
        let store = SettingsStore::open(&path).expect("must open");
        // The value that was wrong is back to its default...
        assert_eq!(store.current().scale, Settings::default().scale);
        // ...and what the person restricted is exactly as they chose it. A wrong picture scale
        // must never switch the relays back on.
        assert_eq!(
            store.current().network.bind,
            Some("127.0.0.1".parse().unwrap())
        );
        assert!(!store.current().network.allow_relay);
        assert_eq!(store.current().device_name, "Alex laptop");
        // The old file is kept, and the file in use now says the same as memory.
        assert!(store.set_aside().is_some());
        let reopened = SettingsStore::open(&path).unwrap();
        assert_eq!(reopened.current(), store.current());
        assert!(reopened.set_aside().is_none());
    }

    #[test]
    fn the_two_halves_of_the_network_policy_are_judged_apart_and_neither_loosens_the_other() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        let mut bad = serde_json::to_value(Settings::default()).unwrap();
        // An address the program refuses to bind to, beside a relay the person turned off.
        bad["network"]["bind"] = serde_json::json!("0.0.0.0");
        bad["network"]["allow_relay"] = serde_json::json!(false);
        fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
        let store = SettingsStore::open(&path).expect("must open");
        // The address cannot be used, so it is no longer restricted; but the relay that the
        // person turned off stays off: the unusable half takes nothing with it.
        assert_eq!(store.current().network.bind, None);
        assert!(!store.current().network.allow_relay);
        assert!(
            store.set_aside().is_some(),
            "the loss of the address must be said"
        );
    }

    #[test]
    fn a_restriction_survives_whichever_order_its_two_halves_are_written_in() {
        // A bind address is only valid beside a relay that is off, so judging them one at a
        // time can lose the address when the relay is judged second. Both orders must keep both.
        for text in [
            r#"{"version":1,"device_name":"A","network":{"bind":"127.0.0.1","allow_relay":false},
                "background":false,"renderer":"auto","theme":"system","accent":[1,2,3],
                "scale":50.0,"reduced_motion":false}"#,
            r#"{"version":1,"device_name":"A","network":{"allow_relay":false,"bind":"127.0.0.1"},
                "background":false,"renderer":"auto","theme":"system","accent":[1,2,3],
                "scale":50.0,"reduced_motion":false}"#,
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("preferences.json");
            fs::write(&path, text).unwrap();
            let store = SettingsStore::open(&path).expect("must open");
            assert_eq!(store.current().scale, Settings::default().scale, "{text}");
            assert_eq!(
                store.current().network.bind,
                Some("127.0.0.1".parse().unwrap()),
                "{text}"
            );
            assert!(!store.current().network.allow_relay, "{text}");
        }
    }

    #[test]
    fn a_file_that_is_not_json_at_all_has_nothing_to_keep_and_starts_on_the_defaults() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        fs::write(&path, b"{ this is not json").unwrap();
        let store = SettingsStore::open(&path).expect("must open");
        assert_eq!(store.current(), &Settings::default());
        assert!(store.set_aside().is_some());
    }

    #[test]
    fn setting_aside_never_overwrites_an_earlier_set_aside_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        fs::write(&path, b"first").unwrap();
        let first = SettingsStore::open(&path)
            .unwrap()
            .set_aside()
            .unwrap()
            .to_owned();
        fs::write(&path, b"second").unwrap();
        let second = SettingsStore::open(&path)
            .unwrap()
            .set_aside()
            .unwrap()
            .to_owned();
        assert_ne!(first, second);
        assert_eq!(fs::read(first).unwrap(), b"first");
        assert_eq!(fs::read(second).unwrap(), b"second");
    }

    #[test]
    fn a_file_too_large_is_set_aside_not_read_whole() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        fs::write(&path, vec![b' '; (MAX_BYTES + 10) as usize]).unwrap();
        let store = SettingsStore::open(&path).expect("must open");
        assert_eq!(store.current(), &Settings::default());
        assert!(store.set_aside().is_some());
    }

    #[test]
    fn write_failure_keeps_previous_memory() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        let mut store = SettingsStore::open(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(
            store
                .update(&SettingsPatch {
                    background: Some(true),
                    ..Default::default()
                })
                .is_err()
        );
        assert!(!store.current().background);
    }

    /// A field that is not written while it holds its default is invisible to anything that learns the fields of the settings from
    /// how the defaults are written. Each one is given a value that is not the default, saved, and read back from the file; the
    /// reopened settings must be what was saved, and nothing may be said to have been lost.
    #[test]
    fn every_field_that_is_not_written_at_its_default_survives_a_save_and_a_reopen() {
        let mut chosen = Settings::default();
        chosen.network.relay_url = Some("https://relay.example.org".into());
        chosen.language = Some(peersus_core::config::Language::parse("ro").unwrap());
        chosen
            .custom_theme
            .colors
            .insert(peersus_core::config::ThemeColor::Background, [1, 2, 3]);
        chosen
            .saved_themes
            .insert("Mine".into(), peersus_core::config::CustomTheme::default());
        chosen
            .validate()
            .expect("the settings under test are themselves valid");

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preferences.json");
        let mut store = SettingsStore::open(&path).unwrap();
        store.replace(chosen.clone()).unwrap();
        drop(store);

        let reopened = SettingsStore::open(&path).unwrap();
        assert_eq!(
            reopened.current().network.relay_url,
            chosen.network.relay_url,
            "the relay of one's own was dropped"
        );
        assert_eq!(reopened.current(), &chosen);
        assert!(
            reopened.set_aside().is_none(),
            "reading a valid file must not be said to have lost anything"
        );

        // The way the command line saves: an edit that says nothing about the relay must leave it where it was.
        let mut store = reopened;
        store
            .update(&SettingsPatch {
                reduced_motion: Some(true),
                ..Default::default()
            })
            .unwrap();
        drop(store);
        let after = SettingsStore::open(&path).unwrap();
        assert_eq!(
            after.current().network.relay_url,
            chosen.network.relay_url,
            "an edit of something else lost the relay"
        );
        assert!(after.current().reduced_motion);
    }
}
