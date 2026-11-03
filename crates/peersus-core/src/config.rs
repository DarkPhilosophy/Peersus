//! Preferences shared by command-line and graphical clients.
//!
//! Application preferences are typed fields. Plugin preferences are an opaque map of
//! [`PluginId`] to saved values; this module never names a concrete plugin and never
//! validates a plugin value without the manifest that declared it.
use crate::plugins::{PluginId, Values};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, net::IpAddr};
use thiserror::Error;

/// Network routing policy. An explicit restriction never silently falls back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSettings {
    /// Local address selected by the user, or operating-system routing.
    pub bind: Option<IpAddr>,
    /// Allow encrypted relay fallback when direct connectivity fails.
    pub allow_relay: bool,
}

impl Default for NetworkSettings {
    fn default() -> Self {
        Self {
            bind: None,
            allow_relay: true,
        }
    }
}

/// Rendering preference, applied when the window is created.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Renderer {
    /// Prefer the GPU and fall back to the CPU rasterizer.
    #[default]
    Auto,
    /// Require GPU rendering; an unavailable adapter is reported, never replaced.
    Gpu,
    /// Require CPU rasterization.
    Cpu,
}

/// Theme selection independent of a particular GUI toolkit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Theme {
    /// Respect the platform preference.
    #[default]
    System,
    Dark,
    Light,
    /// The user's saved palette, independent from the system preference.
    Custom,
}

/// Brightness used for the toolkit's defaults before applying individual custom colours.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeBase {
    #[default]
    Dark,
    Light,
}

/// Semantic interface colours shared by every frontend, not toolkit-specific colour strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeColor {
    Background, Surface, Raised, Header, Sidebar, Border, Text, Muted,
    Portal, PortalText, Send, Receive, Good, Bad, Warning, WarningBackground,
    Button, ButtonText, Minimise, Maximise, Close,
    Icons, ActiveIcons, ScannerOutline,
}

/// Sparse overrides retain the selected base for unchanged elements. Leaving Custom never
/// discards this palette; an empty map explicitly resets all overrides.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CustomTheme {
    pub base: ThemeBase,
    pub colors: BTreeMap<ThemeColor, [u8; 3]>,
}

/// One palette registered under the name chosen by its author.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedTheme {
    pub name: String,
    pub palette: CustomTheme,
}

pub const MAX_SAVED_THEMES: usize = 128;
pub const MAX_THEME_NAME_CHARS: usize = 64;

pub fn valid_theme_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name == name.trim()
        && name.chars().count() <= MAX_THEME_NAME_CHARS
        && !name.chars().any(char::is_control)
}

/// The bar down the left side, which is how every place of the application (send, receive, peers,
/// settings) is reached, on a desktop and on a phone alike. It is always one bar with three states,
/// and the expanded one is reached from either of the others.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Navigation {
    /// Not shown: the window is the page alone, and a button brings the bar back.
    Hidden,
    /// Narrowed to the pictures alone.
    Icons,
    /// A picture and a name for each place. The state it opens in.
    ///
    /// `top` and `bar` are the names an earlier version wrote, when the places could also be across
    /// the top. That arrangement is gone, so a file that still says so is read as this and keeps
    /// loading, instead of a name this version does not know stopping the whole preferences file.
    #[default]
    #[serde(alias = "top", alias = "bar")]
    Expanded,
}

/// The interface language, shared by windows and platform notifications.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    #[default]
    #[serde(rename = "en")]
    English,
    #[serde(rename = "ro")]
    Romanian,
    #[serde(rename = "de")]
    German,
    #[serde(rename = "fr")]
    French,
    #[serde(rename = "es")]
    Spanish,
    #[serde(rename = "it")]
    Italian,
    #[serde(rename = "pt")]
    Portuguese,
    #[serde(rename = "pl")]
    Polish,
    #[serde(rename = "nl")]
    Dutch,
    #[serde(rename = "tr")]
    Turkish,
    #[serde(rename = "cs")]
    Czech,
    #[serde(rename = "sv")]
    Swedish,
}

impl Language {
    pub const ALL: [Self; 12] = [
        Self::English, Self::Romanian, Self::German, Self::French,
        Self::Spanish, Self::Italian, Self::Portuguese, Self::Polish,
        Self::Dutch, Self::Turkish, Self::Czech, Self::Swedish,
    ];

    /// BCP 47 tag used by platform resource selection.
    pub const fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Romanian => "ro",
            Self::German => "de",
            Self::French => "fr",
            Self::Spanish => "es",
            Self::Italian => "it",
            Self::Portuguese => "pt",
            Self::Polish => "pl",
            Self::Dutch => "nl",
            Self::Turkish => "tr",
            Self::Czech => "cs",
            Self::Swedish => "sv",
        }
    }

    /// Autonyms keep the language selector usable after an accidental selection.
    pub const fn name(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Romanian => "Română",
            Self::German => "Deutsch",
            Self::French => "Français",
            Self::Spanish => "Español",
            Self::Italian => "Italiano",
            Self::Portuguese => "Português",
            Self::Polish => "Polski",
            Self::Dutch => "Nederlands",
            Self::Turkish => "Türkçe",
            Self::Czech => "Čeština",
            Self::Swedish => "Svenska",
        }
    }
}

fn language_is_default(language: &Language) -> bool {
    *language == Language::English
}

/// User preferences; the CLI and every frontend submit this same validated value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Schema version, independent from executable releases.
    pub version: u32,
    /// Display name, not an authentication identifier.
    pub device_name: String,
    pub network: NetworkSettings,
    /// Start without opening a window.
    pub background: bool,
    pub renderer: Renderer,
    pub theme: Theme,
    /// English when absent, preserving preferences written before localization.
    #[serde(default, skip_serializing_if = "language_is_default")]
    pub language: Language,
    /// RGB accent.
    pub accent: [u8; 3],
    /// Retained even while System, Light or Dark is selected.
    #[serde(default, skip_serializing_if = "custom_theme_is_default")]
    pub custom_theme: CustomTheme,
    /// Named palettes remain registered after selecting another theme or restarting.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub saved_themes: BTreeMap<String, CustomTheme>,
    /// UI scale, bounded for usability.
    pub scale: f32,
    /// Disable decorative animation.
    pub reduced_motion: bool,
    /// The one option: whether the sidebar stays as its icons outside Settings, instead of not
    /// being there at all. Off by default. Whether the sidebar is expanded is where the person is
    /// (inside Settings it is), and is not saved.
    #[serde(default)]
    pub collapse_to_icons: bool,
    /// Android: whether a touch of two fingers opens the Settings overlay. ON by default, so that a file written before this option
    /// existed (which has no such field) loads with it on. It does nothing while the overlay is already open.
    #[serde(default = "default_two_finger_settings")]
    pub two_finger_settings: bool,
    /// Whether a file that is offered is saved without asking, into [`Settings::save_folder`]. OFF by default: a person who
    /// has just started is asked where each file goes, instead of finding out afterwards where it went. With it on, files
    /// are saved on their own.
    #[serde(default)]
    pub auto_save: bool,
    /// Whether touching the circle sends the chosen files at once, instead of holding it until it fills. OFF by default: holding is the
    /// moment in which a wrong choice of device is still free, and a send is not undone. A person who does not want that pause turns it on.
    #[serde(default)]
    pub send_instantly: bool,
    /// Where incoming files are saved on their own. `None` is the default folder (Peersus, in the person's downloads), which
    /// is worked out where the file is saved and is not written here, so that it follows the person's system. An absolute
    /// path only; one that stops existing is not an error here, and is dealt with when a file arrives.
    #[serde(default)]
    pub save_folder: Option<std::path::PathBuf>,
    /// How long a file that is offered to this device waits for an answer before it is refused on its own, in seconds. The
    /// application's own clock for it, so that a screen can show the time left and a plugin can read, and change, the same
    /// number. [`DEFAULT_OFFER_TIMEOUT`] unless the person chose another; bounded so that none can be made to never end.
    #[serde(default = "default_offer_timeout")]
    pub offer_timeout_seconds: u32,
    /// `navigation` and `sidebar_open`, which earlier versions wrote. They mean nothing now, but
    /// they may still be in the person's file, and a file with a field this version does not know
    /// would otherwise be refused whole. They are accepted and dropped on the spot, and never held
    /// or written: a value kept in memory and not written would make a `Settings` unequal to its
    /// own copy, and every save would be refused as "changed elsewhere".
    #[serde(default, rename = "navigation", skip_serializing, deserialize_with = "forget")]
    pub earlier_navigation: (),
    #[serde(default, rename = "sidebar_open", skip_serializing, deserialize_with = "forget")]
    pub earlier_sidebar_open: (),
    /// Saved plugin values by plugin id. Values are validated against each plugin's
    /// manifest by the host before they are stored.
    #[serde(default)]
    pub plugins: BTreeMap<PluginId, Values>,
}

/// Reads whatever a field holds and keeps none of it. For a field an earlier version wrote that
/// means nothing now, and that must not stop a file from loading.
fn forget<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<(), D::Error> {
    serde::de::IgnoredAny::deserialize(deserializer).map(|_| ())
}

fn custom_theme_is_default(theme: &CustomTheme) -> bool {
    theme == &CustomTheme::default()
}

/// What an offer waits for an answer, in seconds, when nothing else was chosen: two minutes, as the application always waited,
/// now a value that is kept, read and changed like any other preference and not a number inside the transfer code.
pub const DEFAULT_OFFER_TIMEOUT: u32 = 120;
/// The shortest and the longest wait that can be chosen: long enough to read an offer, and never for ever.
pub const MIN_OFFER_TIMEOUT: u32 = 5;
pub const MAX_OFFER_TIMEOUT: u32 = 24 * 60 * 60;

fn default_offer_timeout() -> u32 {
    DEFAULT_OFFER_TIMEOUT
}

fn default_two_finger_settings() -> bool {
    true
}

pub const SCHEMA_VERSION: u32 = 1;
pub const MIN_SCALE: f32 = 0.75;
pub const MAX_SCALE: f32 = 2.0;
pub const MAX_NAME_CHARS: usize = 64;
/// The longest folder path kept, in bytes: far past any real path, and a bound on what a settings file can hold.
pub const MAX_FOLDER_BYTES: usize = 4096;


impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            device_name: "My device".into(),
            network: NetworkSettings::default(),
            background: false,
            renderer: Renderer::Auto,
            theme: Theme::System,
            language: Language::English,
            accent: [96, 212, 180],
            custom_theme: CustomTheme::default(),
            saved_themes: BTreeMap::new(),
            scale: 1.0,
            reduced_motion: false,
            collapse_to_icons: false,
            two_finger_settings: true,
            auto_save: false,
            send_instantly: false,
            save_folder: None,
            offer_timeout_seconds: DEFAULT_OFFER_TIMEOUT,
            earlier_navigation: (),
            earlier_sidebar_open: (),
            plugins: BTreeMap::new(),
        }
    }
}

/// Invalid preferences are rejected rather than partially applied.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SettingsError {
    #[error("unsupported settings version")]
    Version,
    #[error("device name must contain 1–64 non-control characters")]
    Name,
    #[error("interface scale must be between 0.75 and 2")]
    Scale,
    #[error("bind requires a concrete unicast address")]
    Bind,
    /// Relay sockets cannot honor an explicit interface restriction.
    #[error("disable internet relays before selecting a bind address")]
    BoundRelay,
    /// The folder for incoming files is not a full path, or is unreasonably long.
    #[error("the folder for incoming files must be a full path")]
    SaveFolder,
    /// The wait for an answer to an offer is shorter or longer than can be chosen.
    #[error("the wait for an answer must be between 5 seconds and 24 hours")]
    OfferTimeout,
    #[error("theme names must contain 1–64 non-control characters, without surrounding spaces")]
    ThemeName,
    #[error("the theme library can contain at most 128 saved themes")]
    ThemeLibraryFull,
}

impl SettingsError {
    /// The words of this refusal as text that lives for ever, for a caller that can only hold that (the owner's
    /// replies are static text). They are exactly the ones `Display` gives, and a test holds the two together, so
    /// that they cannot come apart: a person is told which preference was refused and why, and not "conflict".
    pub fn words(&self) -> &'static str {
        match self {
            Self::Version => "unsupported settings version",
            Self::Name => "device name must contain 1–64 non-control characters",
            Self::Scale => "interface scale must be between 0.75 and 2",
            Self::Bind => "bind requires a concrete unicast address",
            Self::BoundRelay => "disable internet relays before selecting a bind address",
            Self::SaveFolder => "the folder for incoming files must be a full path",
            Self::OfferTimeout => "the wait for an answer must be between 5 seconds and 24 hours",
            Self::ThemeName => "theme names must contain 1–64 non-control characters, without surrounding spaces",
            Self::ThemeLibraryFull => "the theme library can contain at most 128 saved themes",
        }
    }
}

impl Settings {
    /// Validate before applying or persisting any change.
    pub fn validate(&self) -> Result<(), SettingsError> {
        if self.version != SCHEMA_VERSION {
            return Err(SettingsError::Version);
        }
        let name = self.device_name.trim();
        if name.is_empty()
            || name.chars().count() > MAX_NAME_CHARS
            || name.chars().any(char::is_control)
        {
            return Err(SettingsError::Name);
        }
        if self.saved_themes.keys().any(|name| !valid_theme_name(name)) {
            return Err(SettingsError::ThemeName);
        }
        if self.saved_themes.len() > MAX_SAVED_THEMES {
            return Err(SettingsError::ThemeLibraryFull);
        }
        if self.save_folder.as_ref().is_some_and(|folder| !folder.is_absolute() || folder.as_os_str().len() > MAX_FOLDER_BYTES) {
            return Err(SettingsError::SaveFolder);
        }
        if !(MIN_OFFER_TIMEOUT..=MAX_OFFER_TIMEOUT).contains(&self.offer_timeout_seconds) {
            return Err(SettingsError::OfferTimeout);
        }
        if !self.scale.is_finite() || !(MIN_SCALE..=MAX_SCALE).contains(&self.scale) {
            return Err(SettingsError::Scale);
        }
        match self.network.bind {
            Some(address) if address.is_unspecified() || address.is_multicast() => {
                return Err(SettingsError::Bind);
            }
            Some(_) if self.network.allow_relay => return Err(SettingsError::BoundRelay),
            _ => {}
        }
        Ok(())
    }
}

/// Explicitly reset routing or select one concrete local address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "address", rename_all = "kebab-case")]
pub enum BindChange {
    /// Restore operating-system routing.
    Automatic,
    /// Bind to this local address.
    Address(IpAddr),
}

/// Partial edits from any client. Missing fields preserve saved preferences.
///
/// Only the fields a patch changes are written, never the others as `null`. An owner keeps running
/// after the window that started it is closed, so a window can be newer than the owner it talks to;
/// that owner refuses fields it does not know (`deny_unknown_fields`), and a patch that carried
/// every field as `null` would be refused for the one it has never heard of, whatever it changed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind: Option<BindChange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_relay: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renderer: Option<Renderer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<Theme>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<Language>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent: Option<[u8; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_theme: Option<CustomTheme>,
    /// Register one palette without replacing themes another window may have saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save_theme: Option<NamedTheme>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reduced_motion: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collapse_to_icons: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub two_finger_settings: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_save: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_instantly: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save_folder: Option<SaveFolderChange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offer_timeout_seconds: Option<u32>,
}

/// Choose where incoming files are saved, or give the choice back to the default folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "path", rename_all = "kebab-case")]
pub enum SaveFolderChange {
    /// The default folder: Peersus inside the person's downloads.
    Default,
    /// This folder.
    Folder(std::path::PathBuf),
}

impl SettingsPatch {
    /// Produce a fully validated candidate without mutating live state.
    pub fn apply(&self, current: &Settings) -> Result<Settings, SettingsError> {
        let mut next = current.clone();
        if let Some(value) = &self.device_name {
            next.device_name.clone_from(value);
        }
        if let Some(value) = self.bind {
            next.network.bind = match value {
                BindChange::Automatic => None,
                BindChange::Address(address) => Some(address),
            };
        }
        if let Some(value) = self.allow_relay {
            next.network.allow_relay = value;
        }
        if let Some(value) = self.background {
            next.background = value;
        }
        if let Some(value) = self.renderer {
            next.renderer = value;
        }
        if let Some(value) = self.theme {
            next.theme = value;
        }
        if let Some(value) = self.language {
            next.language = value;
        }
        if let Some(value) = self.accent {
            next.accent = value;
        }
        if let Some(value) = &self.custom_theme {
            next.custom_theme.clone_from(value);
        }
        if let Some(theme) = &self.save_theme {
            if !valid_theme_name(&theme.name) {
                return Err(SettingsError::ThemeName);
            }
            next.saved_themes.insert(theme.name.clone(), theme.palette.clone());
        }
        if let Some(value) = self.scale {
            next.scale = value;
        }
        if let Some(value) = self.reduced_motion {
            next.reduced_motion = value;
        }
        if let Some(value) = self.collapse_to_icons {
            next.collapse_to_icons = value;
        }
        if let Some(value) = self.two_finger_settings {
            next.two_finger_settings = value;
        }
        if let Some(value) = self.auto_save {
            next.auto_save = value;
        }
        if let Some(value) = self.send_instantly {
            next.send_instantly = value;
        }
        if let Some(value) = &self.save_folder {
            next.save_folder = match value {
                SaveFolderChange::Default => None,
                SaveFolderChange::Folder(path) => Some(path.clone()),
            };
        }
        if let Some(value) = self.offer_timeout_seconds {
            next.offer_timeout_seconds = value;
        }
        next.validate()?;
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bound(address: [u8; 4]) -> Settings {
        let mut s = Settings::default();
        s.network.bind = Some(IpAddr::from(address));
        s
    }

    #[test]
    fn rejects_nonfinite_and_out_of_range_scale() {
        for scale in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.1, 3.0] {
            let s = Settings {
                scale,
                ..Settings::default()
            };
            assert_eq!(s.validate(), Err(SettingsError::Scale), "{scale}");
        }
        for scale in [MIN_SCALE, 1.0, MAX_SCALE] {
            assert!(
                Settings {
                    scale,
                    ..Settings::default()
                }
                .validate()
                .is_ok()
            );
        }
    }

    #[test]
    fn the_words_of_every_refusal_are_exactly_what_it_says_when_displayed() {
        // Every kind, by a match with no wildcard: a kind added later does not compile until it is listed here, and
        // so cannot be left with words that differ from, or are missing from, what `Display` says.
        fn kinds(error: &SettingsError) -> SettingsError {
            match error {
                SettingsError::Version => SettingsError::Version,
                SettingsError::Name => SettingsError::Name,
                SettingsError::Scale => SettingsError::Scale,
                SettingsError::Bind => SettingsError::Bind,
                SettingsError::BoundRelay => SettingsError::BoundRelay,
                SettingsError::SaveFolder => SettingsError::SaveFolder,
                SettingsError::OfferTimeout => SettingsError::OfferTimeout,
                SettingsError::ThemeName => SettingsError::ThemeName,
                SettingsError::ThemeLibraryFull => SettingsError::ThemeLibraryFull,
            }
        }
        for error in [
            SettingsError::Version,
            SettingsError::Name,
            SettingsError::Scale,
            SettingsError::Bind,
            SettingsError::BoundRelay,
            SettingsError::SaveFolder,
            SettingsError::OfferTimeout,
            SettingsError::ThemeName,
            SettingsError::ThemeLibraryFull,
        ] {
            assert_eq!(kinds(&error).words(), error.to_string(), "{error:?}");
            assert!(!error.words().is_empty());
        }
    }

    #[test]
    fn device_name_is_bounded_trimmed_and_single_line() {
        for name in ["", "   ", "a\nb", &"x".repeat(MAX_NAME_CHARS + 1)] {
            let s = Settings {
                device_name: name.into(),
                ..Settings::default()
            };
            assert_eq!(s.validate(), Err(SettingsError::Name), "{name:?}");
        }
        let s = Settings {
            device_name: "é".repeat(MAX_NAME_CHARS),
            ..Settings::default()
        };
        assert!(s.validate().is_ok());
    }

    #[test]
    fn bind_rejects_wildcard_and_multicast_and_is_incompatible_with_relay() {
        for address in [[0, 0, 0, 0], [224, 0, 0, 1]] {
            assert_eq!(bound(address).validate(), Err(SettingsError::Bind));
        }
        assert_eq!(
            bound([127, 0, 0, 1]).validate(),
            Err(SettingsError::BoundRelay)
        );
        let mut ok = bound([127, 0, 0, 1]);
        ok.network.allow_relay = false;
        assert_eq!(ok.validate(), Ok(()));
    }

    #[test]
    fn unsupported_schema_version_is_rejected() {
        let s = Settings {
            version: 2,
            ..Settings::default()
        };
        assert_eq!(s.validate(), Err(SettingsError::Version));
    }

    #[test]
    fn legacy_unknown_fields_fail_closed() {
        let json = serde_json::to_value(Settings::default()).unwrap();
        let mut with_extra = json.clone();
        with_extra["local_only"] = false.into();
        assert!(serde_json::from_value::<Settings>(with_extra).is_err());
        // Missing `plugins` (older saved file) still loads.
        let mut without = json;
        without.as_object_mut().unwrap().remove("plugins");
        assert!(
            serde_json::from_value::<Settings>(without)
                .unwrap()
                .plugins
                .is_empty()
        );
    }

    #[test]
    fn the_sidebar_keeps_no_icons_outside_settings_unless_the_person_asks() {
        // The default is no menu at all outside Settings: the one option is off.
        assert!(!Settings::default().collapse_to_icons);
    }

    #[test]
    fn only_the_one_option_is_saved_and_whether_the_sidebar_is_open_is_not_a_setting() {
        // Where the person is decides whether it is expanded; that is not stored anywhere.
        let written = serde_json::to_value(Settings::default()).unwrap();
        assert!(written.get("collapse_to_icons").is_some());
        assert!(written.get("sidebar_open").is_none());
        assert!(written.get("navigation").is_none());
    }

    #[test]
    fn a_file_saved_before_the_option_existed_still_loads_with_it_off() {
        let mut old = serde_json::to_value(Settings::default()).unwrap();
        old.as_object_mut().unwrap().remove("collapse_to_icons");
        let loaded = serde_json::from_value::<Settings>(old).expect("an older file still loads");
        assert!(!loaded.collapse_to_icons);
    }

    #[test]
    fn the_fields_earlier_versions_wrote_for_the_bar_are_read_and_do_not_stop_the_program() {
        // Earlier versions wrote `navigation` (top, bar, icons, hidden, expanded) and
        // `sidebar_open` into the person's file. They mean nothing now, but a file that still has
        // them must load, not be refused for them, and they are not written back.
        for old in ["top", "bar", "icons", "hidden", "expanded"] {
            let mut json = serde_json::to_value(Settings::default()).unwrap();
            json["navigation"] = old.into();
            json["sidebar_open"] = true.into();
            let loaded = serde_json::from_value::<Settings>(json).unwrap_or_else(|e| panic!("{old}: {e}"));
            let rewritten = serde_json::to_value(&loaded).unwrap();
            assert!(rewritten.get("navigation").is_none() && rewritten.get("sidebar_open").is_none(), "{old}");
        }
    }

    #[test]
    fn a_patch_changes_the_one_option_and_nothing_else() {
        let current = Settings::default();
        let on = SettingsPatch { collapse_to_icons: Some(true), ..SettingsPatch::default() }.apply(&current).unwrap();
        assert!(on.collapse_to_icons);
        assert_eq!(Settings { collapse_to_icons: false, ..on.clone() }, current);
        // A patch that does not mention it leaves it alone.
        let kept = SettingsPatch { reduced_motion: Some(true), ..SettingsPatch::default() }.apply(&on).unwrap();
        assert!(kept.collapse_to_icons);
    }

    #[test]
    fn two_fingers_open_settings_unless_the_person_turns_it_off() {
        assert!(Settings::default().two_finger_settings);
    }

    #[test]
    fn a_file_saved_before_the_two_finger_option_existed_loads_with_it_on() {
        // The default of a field a file does not have is ON, not the `false` that `serde(default)` alone would give: a person who updates
        // is not left without the gesture only because their file is older than it.
        let mut old = serde_json::to_value(Settings::default()).unwrap();
        old.as_object_mut().unwrap().remove("two_finger_settings");
        let loaded = serde_json::from_value::<Settings>(old).expect("an older file still loads");
        assert!(loaded.two_finger_settings);
    }

    #[test]
    fn a_patch_turns_the_two_finger_option_off_and_changes_nothing_else() {
        let current = Settings::default();
        let off = SettingsPatch { two_finger_settings: Some(false), ..SettingsPatch::default() }.apply(&current).unwrap();
        assert!(!off.two_finger_settings);
        assert_eq!(Settings { two_finger_settings: true, ..off.clone() }, current);
        // A patch that does not mention it leaves it alone, off as well as on.
        let kept = SettingsPatch { reduced_motion: Some(true), ..SettingsPatch::default() }.apply(&off).unwrap();
        assert!(!kept.two_finger_settings);
        let kept_on = SettingsPatch { reduced_motion: Some(true), ..SettingsPatch::default() }.apply(&current).unwrap();
        assert!(kept_on.two_finger_settings);
    }

    #[test]
    fn the_two_finger_option_is_written_as_one_key_by_its_patch() {
        let one = SettingsPatch { two_finger_settings: Some(false), ..SettingsPatch::default() };
        assert_eq!(serde_json::to_value(one).unwrap(), serde_json::json!({ "two_finger_settings": false }));
    }

    #[test]
    fn a_patch_is_written_with_only_the_fields_it_changes() {
        // A patch with nothing in it is `{}`, and one change is one key: an owner that was started
        // before a field existed, and refuses fields it does not know, then still understands every
        // patch that does not mention it. Writing the unused fields as `null` would make a window
        // newer than its owner fail on every single change.
        assert_eq!(serde_json::to_value(SettingsPatch::default()).unwrap(), serde_json::json!({}));
        let one = SettingsPatch { reduced_motion: Some(true), ..SettingsPatch::default() };
        assert_eq!(serde_json::to_value(one).unwrap(), serde_json::json!({ "reduced_motion": true }));
        let icons = SettingsPatch { collapse_to_icons: Some(true), ..SettingsPatch::default() };
        assert_eq!(serde_json::to_value(icons).unwrap(), serde_json::json!({ "collapse_to_icons": true }));
    }

    #[test]
    fn a_patch_without_the_newer_fields_is_still_read() {
        // What a window older than the owner sends: nothing about the bar at all.
        let read: SettingsPatch = serde_json::from_value(serde_json::json!({ "theme": "dark" })).unwrap();
        assert_eq!(read, SettingsPatch { theme: Some(Theme::Dark), ..SettingsPatch::default() });
    }

    #[test]
    fn settings_read_from_a_file_with_the_earlier_field_equal_themselves_after_a_round_trip() {
        // The owner compares what it holds with what a window sends back as `expected`, and a
        // window gets the settings written out. If something read from the file were kept in
        // memory and not written, the two could never be equal again and every save would be
        // refused as "changed elsewhere" until the owner was restarted.
        let mut json = serde_json::to_value(Settings::default()).unwrap();
        json["navigation"] = "top".into();
        let held: Settings = serde_json::from_value(json).expect("a file with the earlier field loads");
        let sent_back: Settings = serde_json::from_value(serde_json::to_value(&held).unwrap()).unwrap();
        assert_eq!(held, sent_back);
    }

    /// A patch as it was before `navigation` existed, refusing fields it does not know, which is
    /// what an owner started before that change still is: it keeps running after its window closes.
    #[derive(Debug, PartialEq, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct PatchBeforeNavigation {
        device_name: Option<String>,
        allow_relay: Option<bool>,
        background: Option<bool>,
        theme: Option<Theme>,
        reduced_motion: Option<bool>,
    }

    #[test]
    fn an_owner_that_predates_navigation_still_accepts_every_patch_that_does_not_use_it() {
        // The changes the Settings screen makes, written as the window writes them.
        for patch in [
            SettingsPatch { reduced_motion: Some(true), ..SettingsPatch::default() },
            SettingsPatch { allow_relay: Some(false), ..SettingsPatch::default() },
            SettingsPatch { background: Some(true), ..SettingsPatch::default() },
            SettingsPatch { theme: Some(Theme::Dark), ..SettingsPatch::default() },
            SettingsPatch { device_name: Some("Laptop".into()), ..SettingsPatch::default() },
        ] {
            let written = serde_json::to_value(&patch).unwrap();
            let read = serde_json::from_value::<PatchBeforeNavigation>(written.clone());
            assert!(read.is_ok(), "the older owner refused {written}: {read:?}");
        }
        // And the one change it cannot know is refused by it, which is right: it is not applied
        // half, and the window says so.
        let new = serde_json::to_value(SettingsPatch { collapse_to_icons: Some(true), ..SettingsPatch::default() }).unwrap();
        assert!(serde_json::from_value::<PatchBeforeNavigation>(new).is_err());
    }

    #[test]
    fn incoming_files_are_saved_on_their_own_into_the_default_folder_until_the_person_says_otherwise() {
        let defaults = Settings::default();
        assert!(!defaults.auto_save, "a new person is asked where each file goes, not told after the fact where it went");
        assert_eq!(defaults.save_folder, None, "the default folder is worked out where a file is saved, not written");
        assert!(defaults.validate().is_ok());
    }

    #[test]
    fn a_settings_file_from_before_these_settings_loads_with_them_at_their_defaults() {
        // Written by a version that did not know them: no auto_save, no save_folder.
        let mut value = serde_json::to_value(Settings::default()).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("auto_save");
        object.remove("save_folder");
        let read: Settings = serde_json::from_value(value).expect("an older file was refused");
        assert!(!read.auto_save, "an older file turned saving on its own on, which nobody asked for");
        assert_eq!(read.save_folder, None);
    }

    #[test]
    fn sending_instantly_is_off_for_a_new_person_and_for_a_file_from_before_it_existed() {
        assert!(!Settings::default().send_instantly, "a new person was sent files without the pause that protects a wrong choice");
        let mut value = serde_json::to_value(Settings::default()).unwrap();
        value.as_object_mut().unwrap().remove("send_instantly");
        let read: Settings = serde_json::from_value(value).expect("an older file was refused");
        assert!(!read.send_instantly, "an older file turned sending instantly on, which nobody asked for");
    }

    #[test]
    fn sending_instantly_changes_alone_and_is_written_as_one_key() {
        let current = Settings { auto_save: true, reduced_motion: true, ..Settings::default() };
        let on = SettingsPatch { send_instantly: Some(true), ..SettingsPatch::default() }.apply(&current).unwrap();
        assert!(on.send_instantly);
        assert_eq!(Settings { send_instantly: false, ..on.clone() }, current, "turning it on changed something else");
        let off = SettingsPatch { send_instantly: Some(false), ..SettingsPatch::default() }.apply(&on).unwrap();
        assert_eq!(off, current);
        // A patch that does not mention it leaves it alone, off as well as on.
        assert!(SettingsPatch { auto_save: Some(false), ..SettingsPatch::default() }.apply(&on).unwrap().send_instantly);
        assert!(!SettingsPatch { auto_save: Some(false), ..SettingsPatch::default() }.apply(&off).unwrap().send_instantly);
        let one = SettingsPatch { send_instantly: Some(true), ..SettingsPatch::default() };
        assert_eq!(serde_json::to_value(one).unwrap(), serde_json::json!({ "send_instantly": true }));
    }

    #[test]
    fn the_folder_for_incoming_files_must_be_a_full_path_of_reasonable_length() {
        for bad in [std::path::PathBuf::from("relative/dir"), std::path::PathBuf::from("~/Downloads"), std::path::PathBuf::from("")] {
            let s = Settings { save_folder: Some(bad.clone()), ..Settings::default() };
            assert_eq!(s.validate(), Err(SettingsError::SaveFolder), "{bad:?} was accepted");
        }
        let too_long = std::path::PathBuf::from(format!("/{}", "a".repeat(MAX_FOLDER_BYTES)));
        assert_eq!(Settings { save_folder: Some(too_long), ..Settings::default() }.validate(), Err(SettingsError::SaveFolder));
        let good = Settings { save_folder: Some(std::path::PathBuf::from("/home/me/Received")), ..Settings::default() };
        assert!(good.validate().is_ok());
    }

    #[test]
    fn the_save_settings_are_changed_by_a_patch_and_the_folder_can_be_given_back_to_the_default() {
        let current = Settings::default();
        let on = SettingsPatch { auto_save: Some(true), ..SettingsPatch::default() }.apply(&current).unwrap();
        assert!(on.auto_save);
        let off = SettingsPatch { auto_save: Some(false), ..SettingsPatch::default() }.apply(&on).unwrap();
        assert!(!off.auto_save);
        // Independence, from BOTH states of saving on its own: choosing a folder must leave it as it was, whether it was on or
        // off (from only one state a change that sets it to that same value could not be seen).
        for starting in [on.clone(), off.clone()] {
            let chosen = SettingsPatch {
                save_folder: Some(SaveFolderChange::Folder(std::path::PathBuf::from("/srv/in"))),
                ..SettingsPatch::default()
            }
            .apply(&starting)
            .unwrap();
            assert_eq!(chosen.save_folder, Some(std::path::PathBuf::from("/srv/in")));
            assert_eq!(chosen.auto_save, starting.auto_save, "choosing a folder changed whether saving on its own is on");
            // And the other way: turning it on or off leaves the chosen folder where it is.
            for value in [true, false] {
                let flipped = SettingsPatch { auto_save: Some(value), ..SettingsPatch::default() }.apply(&chosen).unwrap();
                assert_eq!(flipped.save_folder, chosen.save_folder, "switching saving on its own to {value} lost the folder");
            }
        }
        let chosen = SettingsPatch {
            save_folder: Some(SaveFolderChange::Folder(std::path::PathBuf::from("/srv/in"))),
            ..SettingsPatch::default()
        }
        .apply(&current)
        .unwrap();
        let back = SettingsPatch { save_folder: Some(SaveFolderChange::Default), ..SettingsPatch::default() }.apply(&chosen).unwrap();
        assert_eq!(back.save_folder, None);
        // A folder that is not a full path is refused by the patch, and nothing is applied.
        let bad = SettingsPatch { save_folder: Some(SaveFolderChange::Folder("nope".into())), ..SettingsPatch::default() };
        assert_eq!(bad.apply(&current), Err(SettingsError::SaveFolder));
        // A patch that does not mention them leaves them as they were.
        let untouched = SettingsPatch { theme: Some(Theme::Dark), ..SettingsPatch::default() }.apply(&chosen).unwrap();
        assert_eq!(untouched.save_folder, chosen.save_folder);
        assert_eq!(untouched.auto_save, chosen.auto_save);
    }

    #[test]
    fn the_wait_for_an_answer_defaults_to_two_minutes_and_an_older_file_loads_with_it() {
        assert_eq!(Settings::default().offer_timeout_seconds, 120);
        let mut value = serde_json::to_value(Settings::default()).unwrap();
        value.as_object_mut().unwrap().remove("offer_timeout_seconds");
        let read: Settings = serde_json::from_value(value).expect("an older file was refused");
        assert_eq!(read.offer_timeout_seconds, DEFAULT_OFFER_TIMEOUT, "an older file got another wait than the default");
    }

    #[test]
    fn the_wait_for_an_answer_is_bounded_at_both_ends_and_the_bounds_themselves_are_allowed() {
        for (seconds, allowed) in [
            (0, false),
            (MIN_OFFER_TIMEOUT - 1, false),
            (MIN_OFFER_TIMEOUT, true),
            (3600, true),
            (MAX_OFFER_TIMEOUT, true),
            (MAX_OFFER_TIMEOUT + 1, false),
            (u32::MAX, false),
        ] {
            let patch = SettingsPatch { offer_timeout_seconds: Some(seconds), ..SettingsPatch::default() };
            let outcome = patch.apply(&Settings::default());
            assert_eq!(outcome.is_ok(), allowed, "{seconds} s: {outcome:?}");
            if !allowed {
                assert_eq!(outcome, Err(SettingsError::OfferTimeout));
            }
        }
    }

    #[test]
    fn changing_the_wait_changes_nothing_else_and_a_patch_that_does_not_mention_it_keeps_it() {
        let current = Settings::default();
        let hour = SettingsPatch { offer_timeout_seconds: Some(3600), ..SettingsPatch::default() }.apply(&current).unwrap();
        assert_eq!(hour.offer_timeout_seconds, 3600);
        assert_eq!(Settings { offer_timeout_seconds: current.offer_timeout_seconds, ..hour.clone() }, current, "it changed something else");
        let untouched = SettingsPatch { theme: Some(Theme::Dark), ..SettingsPatch::default() }.apply(&hour).unwrap();
        assert_eq!(untouched.offer_timeout_seconds, 3600);
        // The words a person is told are the ones the error carries.
        assert_eq!(SettingsError::OfferTimeout.words(), SettingsError::OfferTimeout.to_string());
    }
}
