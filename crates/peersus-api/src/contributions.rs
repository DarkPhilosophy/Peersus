//! Declarative, toolkit-neutral extension points.
//!
//! A plugin describes *what* it adds with stable identifiers and plain data. It never
//! names a widget, renderer, window or toolkit; any frontend renders the same
//! description. Contributor identity is assigned by the host, not claimed here.
use crate::{Error, Result, chord::Chord, name};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const MAX_PAGES: usize = 8;
pub const MAX_ACTIONS: usize = 32;
pub const MAX_SETTINGS: usize = 64;
pub const MAX_CHOICES: usize = 32;
pub const MAX_LABEL: usize = 80;
pub const MAX_HELP: usize = 280;
pub const MAX_TEXT_SETTING: usize = 4096;

/// Where a page is offered. A frontend may ignore placements it does not have.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    /// A top-level destination in primary navigation. Only a copy placed through `peersus.pages` under the extension's own
    /// option, turned on by the person, is there; a page a manifest declares `primary` is drawn in the extension's settings.
    Primary,
    /// A section inside application settings.
    Settings,
    /// A compact summary area.
    Status,
}

/// What a setting or grant applies to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// One value for the whole application.
    #[default]
    Global,
    /// One value per paired device, chosen independently for each device.
    Device,
}

/// A page whose content is produced by an exported method, never by the frontend.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Page {
    pub id: String,
    pub label: String,
    pub placement: Placement,
    /// Frontend-neutral icon name; unknown names fall back to a generic glyph.
    #[serde(default)]
    pub icon: Option<String>,
    /// Exported `(service, method)` returning the page's view data, or, for a page with a
    /// [`Surface`], receiving what the person does on it.
    #[serde(default)]
    pub source: Option<Source>,
    /// Generic elements supplied by the extension and rendered by the frontend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<Surface>,
    /// Every input names a paired device the person chose on the page.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub needs_device: bool,
    /// Must equal the capability the exporter requires for the source method.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<String>,
    /// Systems the page is offered on, by the names of `platforms`; empty is every system the
    /// plugin runs on. A controller page belongs on a phone and not on the computer it controls.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub platforms: Vec<crate::platform::OperatingSystem>,
}

/// A frontend-neutral page renderer. Page-specific widgets and gestures belong to the extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Surface {
    /// The extension's own content: a tree of [`crate::elements::Node`] it answers each
    /// [`crate::elements::Event`] with, sent to `source` as `{"event": ..}`. The frontend draws it
    /// inside the page and nowhere else.
    Elements,
}

/// Input modes implemented by the receiving platform adapter. This is not a permission
/// grant or a promise that the operating system will accept a later injection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputCapabilities {
    pub independent_pointer: bool,
}

/// Largest batch of pointer input one call carries.
pub const MAX_POINTER_BATCH: usize = 64;
/// Longest text one typed input carries, in characters.
pub const MAX_TYPED_TEXT: usize = 256;

/// A mouse button, by its role rather than by a number of some system.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Button {
    Left,
    Right,
    Middle,
}

/// A key that is not text, by its role. Text is sent as text, so no layout is assumed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Key {
    Enter,
    Backspace,
    Tab,
    Escape,
    Left,
    Right,
    Up,
    Down,
}

/// Portable pointer or keyboard input, already interpreted by its sender.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case", deny_unknown_fields)]
pub enum PointerInput {
    /// Relative pointer motion, in logical pixels already scaled by the person's sensitivity.
    Move {
        dx: f32,
        dy: f32,
    },
    Button {
        button: Button,
        down: bool,
    },
    /// Wheel motion in whole notches; positive `dy` scrolls down, positive `dx` right.
    Scroll {
        dx: i32,
        dy: i32,
    },
    /// One key pressed and released.
    Key {
        key: Key,
    },
    /// Text typed, character by character, whatever the keyboard layout.
    Text {
        text: String,
    },
    /// The pointer at a point of the receiving desktop, as a share of its width and height (0 to 1).
    /// What the layout of devices sends, so both ends agree exactly where the pointer is.
    Position {
        x: f32,
        y: f32,
    },
    /// A physical key pressed or released, by its Linux key code (`input-event-codes.h`): what a
    /// keyboard passed through from another computer sends, whatever its layout.
    KeyCode {
        code: u16,
        down: bool,
    },
}

impl PointerInput {
    /// Within the bounds every replaying device holds input to: finite, modest motion, a
    /// handful of notches, and short text without control characters.
    pub fn validate(&self) -> Result<()> {
        const MOST_MOTION: f32 = 10_000.;
        match self {
            Self::Move { dx, dy } => ok(dx.is_finite()
                && dy.is_finite()
                && dx.abs() <= MOST_MOTION
                && dy.abs() <= MOST_MOTION),
            Self::Button { .. } | Self::Key { .. } => Ok(()),
            Self::Position { x, y } => ok((0.0..=1.0).contains(x) && (0.0..=1.0).contains(y)),
            // Keys and buttons a keyboard has; never the range of tools, switches or the system's own codes.
            Self::KeyCode { code, .. } => {
                ok((1..=0x2ff).contains(code) && !(0x100..0x160).contains(code))
            }
            Self::Scroll { dx, dy } => ok(dx.abs() <= 100 && dy.abs() <= 100),
            Self::Text { text } => ok(!text.is_empty()
                && text.chars().count() <= MAX_TYPED_TEXT
                && !text.chars().any(char::is_control)),
        }
    }
}

/// An exported service method addressed by data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub service: String,
    pub method: String,
}

/// The trusted frontend input required by an action, independent of its renderer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionInput {
    /// Ordinary global or device action, offered in settings or device controls.
    #[default]
    None,
    /// One file already staged on Send. Never opens a second picker.
    SelectedFile,
    /// One text already staged on Send or shared into the application, with a device chosen by the person.
    /// Never reads the clipboard: the text arrives as a single-use, caller-bound handle.
    SharedText,
    /// A masked inline field. Its value stays in the trusted frontend and answers
    /// the action's host credential request; it is never included in guest arguments.
    SecretText,
}
impl ActionInput {
    fn is_none(&self) -> bool {
        *self == Self::None
    }
}

/// A user-invocable command backed by one exported method.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub id: String,
    pub label: String,
    /// Optional concise wording for constrained inline controls; `label` remains the full name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_label: Option<String>,
    /// Selected-file actions belong on Send rather than in global settings.
    #[serde(default, skip_serializing_if = "ActionInput::is_none")]
    pub input: ActionInput,
    pub target: Source,
    /// Must equal the capability the exporter requires for that method.
    #[serde(default)]
    pub capability: Option<String>,
    /// The action needs a chosen paired device as its argument.
    #[serde(default)]
    pub needs_device: bool,
    /// Destructive actions require frontend confirmation.
    #[serde(default)]
    pub destructive: bool,
    /// A user action may wait for a native chooser or transfer; ordinary broker calls
    /// keep their short deadline. The host never exceeds this declared bounded duration.
    #[serde(default = "default_action_timeout")]
    pub timeout_ms: u32,
    /// The chord its extension suggests for it. The application binds it when the extension is newly installed, if it is free
    /// (for an action that needs no device); a key something else holds stays with it. The person changes or removes it like
    /// any binding. Only for an action a key can run as it is: nothing to type or stage first, and nothing destructive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chord: Option<Chord>,
    /// What it does, in a sentence or two, shown under its label where there is room (the Binds page): extra information,
    /// never needed to recognise it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
}

impl Action {
    /// Whether a key can run it as it is: nothing to type or stage first, and nothing destructive.
    /// The only actions that may suggest a chord, and the only ones the application binds one to.
    pub fn bindable(&self) -> bool {
        self.input == ActionInput::None && !self.destructive
    }
}

pub const MAX_ACTION_TIMEOUT_MS: u32 = 600_000;
pub const fn default_action_timeout() -> u32 {
    10_000
}

/// Typed value description. The host persists values; the plugin only declares them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Kind {
    Toggle {
        default: bool,
    },
    Number {
        min: i64,
        max: i64,
        default: i64,
    },
    Choice {
        choices: Vec<String>,
        default: String,
    },
    Text {
        max_len: usize,
        default: String,
    },
}

/// One anonymous HTTPS origin and path subtree that the person can enable in Settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicRead {
    pub origin: String,
    pub root: String,
}

impl PublicRead {
    pub fn url(&self) -> Option<url::Url> {
        if self.origin.len() > 256 || self.root.len() > 512 || !Self::valid_path(&self.root) {
            return None;
        }
        let origin = url::Url::parse(&self.origin).ok()?;
        (origin.scheme() == "https"
            && origin.domain().is_some()
            && origin.port_or_known_default() == Some(443)
            && origin.username().is_empty()
            && origin.password().is_none()
            && origin.path() == "/"
            && origin.query().is_none()
            && origin.fragment().is_none())
        .then(|| origin.join(&self.root).ok())
        .flatten()
    }

    pub fn valid_path(path: &str) -> bool {
        path.strip_prefix('/').is_some_and(|path| {
            path.split('/').all(|part| {
                !matches!(part, "" | "." | "..")
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
            })
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Setting {
    pub id: String,
    /// Systems on which this setting applies. Empty means every supported system.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub platforms: Vec<crate::platform::OperatingSystem>,
    pub label: String,
    #[serde(default)]
    pub help: Option<String>,
    #[serde(default)]
    pub scope: Scope,
    pub kind: Kind,
    /// The toggle itself records consent for this exact resource. It starts off without an approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_read: Option<PublicRead>,
}

/// Upper protocol bound for a captured pointer's edge pressure, in relative motion units.
pub const MAX_EDGE_PRESSURE: u16 = 400;

/// Whether the host measures a push at an arranged edge before forwarding or returning input.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EdgePressure {
    /// Cross the first eligible edge without measuring an additional push.
    // A struct variant also rejects extra fields on the internally tagged value.
    Off {},
    /// Use a global number declared by this carrier. Zero is the same as [`Self::Off`].
    Setting { id: String },
}

/// An extension that transports platform pointer input to a paired desktop. This is independent
/// of pages: changing or removing its editor cannot change forwarding or its per-device guard.
///
/// `source` is called with `{peer, events, end, at}`: `at` is milliseconds on one monotonic clock of
/// the application, so a transport can pace a long stream without a clock of its own; only the
/// difference between two values means anything.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub source: Source,
    pub capability: String,
    pub edge_pressure: EdgePressure,
}

impl Input {
    /// Resolve this carrier's pressure setting declaration, or refuse an invalid binding.
    pub fn pressure_setting<'a>(&self, settings: &'a [Setting]) -> Result<Option<&'a Setting>> {
        let EdgePressure::Setting { id } = &self.edge_pressure else {
            return Ok(None);
        };
        let setting = settings
            .iter()
            .find(|setting| &setting.id == id)
            .ok_or(Error::InvalidManifest)?;
        let Kind::Number {
            min: 0,
            max,
            default,
        } = setting.kind
        else {
            return Err(Error::InvalidManifest);
        };
        ok(setting.scope == Scope::Global
            && setting.platforms.as_slice() == [crate::platform::OperatingSystem::Linux]
            && (1..=i64::from(MAX_EDGE_PRESSURE)).contains(&max)
            && (0..=max).contains(&default))?;
        Ok(Some(setting))
    }
}

/// Everything one plugin adds to the product surface.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contributions {
    #[serde(default)]
    pub pages: Vec<Page>,
    #[serde(default)]
    pub actions: Vec<Action>,
    /// Declared actions to leave out until this generation publishes its availability.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub initially_withheld_actions: BTreeSet<String>,
    #[serde(default)]
    pub settings: Vec<Setting>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Input>,
}

fn ok(valid: bool) -> Result<()> {
    valid.then_some(()).ok_or(Error::InvalidManifest)
}

fn action_label(label: &str) -> bool {
    name::text(label, MAX_LABEL) && !label.contains(['\u{2028}', '\u{2029}'])
}

impl Source {
    fn validate(&self) -> Result<()> {
        ok(name::valid(&self.service) && name::valid(&self.method))
    }
}

impl Kind {
    /// A value satisfies the same constraints its default is held to.
    pub fn validate(&self) -> Result<()> {
        match self {
            Kind::Toggle { .. } => Ok(()),
            Kind::Number { min, max, default } => ok(min < max && min <= default && default <= max),
            Kind::Choice { choices, default } => {
                let unique: BTreeSet<_> = choices.iter().collect();
                ok(!choices.is_empty()
                    && choices.len() <= MAX_CHOICES
                    && unique.len() == choices.len()
                    && choices.iter().all(|c| name::text(c, MAX_LABEL))
                    && choices.contains(default))
            }
            Kind::Text { max_len, default } => ok(*max_len > 0
                && *max_len <= MAX_TEXT_SETTING
                && default.chars().count() <= *max_len
                && !default.chars().any(char::is_control)),
        }
    }
}

impl Setting {
    pub fn available_on(&self, os: Option<crate::platform::OperatingSystem>) -> bool {
        self.platforms.is_empty() || os.is_some_and(|os| self.platforms.contains(&os))
    }
    fn validate(&self) -> Result<()> {
        ok(name::valid(&self.id)
            && name::text(&self.label, MAX_LABEL)
            && self.help.as_deref().is_none_or(|h| name::text(h, MAX_HELP)))?;
        if let Some(read) = &self.public_read {
            ok(self.scope == Scope::Global
                && self.kind == (Kind::Toggle { default: false })
                && read.url().is_some())?;
        }
        self.kind.validate()
    }
}

impl Contributions {
    /// Structural validation. The manifest additionally checks that every action and
    /// page source addresses a method this plugin really exports.
    pub fn validate(&self) -> Result<()> {
        ok(self.pages.len() <= MAX_PAGES
            && self.actions.len() <= MAX_ACTIONS
            && self.settings.len() <= MAX_SETTINGS)?;
        if let Some(input) = &self.input {
            input.source.validate()?;
            ok(name::valid(&input.capability))?;
        }
        let mut seen = BTreeSet::new();
        let mut suggested = BTreeSet::new();
        for p in &self.pages {
            ok(name::valid(&p.id)
                && name::text(&p.label, MAX_LABEL)
                && p.icon.as_deref().is_none_or(name::valid)
                && p.capability.as_deref().is_none_or(name::valid)
                // A surface sends its input somewhere: without a method to receive it, it does nothing.
                && (p.surface.is_none() || p.source.is_some())
                // What an extension shows is about no one device: its own content picks one if it needs to.
                && !(p.surface == Some(Surface::Elements) && p.needs_device)
                // A device is chosen only on a page that has input to send to it.
                && (!p.needs_device || (p.surface.is_some() && p.source.is_some()))
                && seen.insert(("page", p.id.as_str())))?;
            if let Some(source) = &p.source {
                source.validate()?;
            }
        }
        for a in &self.actions {
            ok(name::valid(&a.id)
                && action_label(&a.label)
                && a.short_label.as_deref().is_none_or(action_label)
                && a.help.as_deref().is_none_or(|help| name::text(help, MAX_HELP))
                && a.capability.as_deref().is_none_or(name::valid)
                && !(a.input == ActionInput::SelectedFile && a.needs_device)
                // Shared text is sent to a device the person chose, so it needs one declared.
                && (a.input != ActionInput::SharedText || a.needs_device)
                && (a.input != ActionInput::SecretText || (!a.needs_device && !a.destructive))
                && a.chord
                    .is_none_or(|chord| a.bindable() && suggested.insert(chord))
                && a.timeout_ms > 0
                && a.timeout_ms <= MAX_ACTION_TIMEOUT_MS
                && seen.insert(("action", a.id.as_str())))?;
            a.target.validate()?;
        }
        ok(self
            .initially_withheld_actions
            .iter()
            .all(|id| seen.contains(&("action", id.as_str()))))?;
        // The host holds one public-resource authorization per extension.
        ok(self
            .settings
            .iter()
            .filter(|s| s.public_read.is_some())
            .count()
            <= 1)?;
        for s in &self.settings {
            s.validate()?;
            ok(seen.insert(("setting", s.id.as_str())))?;
        }
        Ok(())
    }

    /// The words a person reads, each looked up by [`Contributions::translate`]: the labels and help of pages, actions and
    /// settings, in that order. What an extension's language file translates (`languages/<tag>.po` in its package).
    pub fn words(&self) -> impl Iterator<Item = &str> {
        let pages = self.pages.iter().map(|page| page.label.as_str());
        let actions = self.actions.iter().flat_map(|action| {
            std::iter::once(action.label.as_str())
                .chain(action.short_label.as_deref())
                .chain(action.help.as_deref())
        });
        let settings = self.settings.iter().flat_map(|setting| {
            std::iter::once(setting.label.as_str()).chain(setting.help.as_deref())
        });
        pages.chain(actions).chain(settings)
    }

    /// The words a person reads, in `catalog`'s language where it translates them: the labels and help of pages, actions and
    /// settings, each looked up by its English text. Ids, choices and every other value stay as written, so translating
    /// changes what is shown and never what anything does. A translation that would not be valid where it goes is not
    /// used: the English stays.
    pub fn translate(&mut self, catalog: &crate::language::Catalog) {
        let put = |text: &mut String, limit: usize| {
            if let Some(translated) = catalog.get(None, text)
                && name::text(translated, limit)
            {
                *text = translated.to_owned();
            }
        };
        for page in &mut self.pages {
            put(&mut page.label, MAX_LABEL);
        }
        for action in &mut self.actions {
            for label in std::iter::once(&mut action.label).chain(action.short_label.as_mut()) {
                if let Some(translated) = catalog.get(None, label)
                    && action_label(translated)
                {
                    *label = translated.to_owned();
                }
            }
            if let Some(help) = &mut action.help {
                put(help, MAX_HELP);
            }
        }
        for setting in &mut self.settings {
            put(&mut setting.label, MAX_LABEL);
            if let Some(help) = &mut setting.help {
                put(help, MAX_HELP);
            }
        }
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn translating_changes_only_the_words_and_never_into_something_invalid() {
        let catalog = crate::language::Catalog::parse(&format!(
            "msgid \"\"\nmsgstr \"Language: ro\\n\"\n\nmsgid \"Share\"\nmsgstr \"Partajează\"\n\nmsgid \"Automatic\"\nmsgstr \"{}\"\n",
            "x".repeat(MAX_LABEL + 1)
        ))
        .unwrap();
        let mut c = Contributions {
            pages: vec![page("share")],
            actions: vec![Action {
                id: "share".into(),
                label: "Share".into(),
                short_label: None,
                input: ActionInput::None,
                target: Source {
                    service: "svc".into(),
                    method: "share".into(),
                },
                capability: None,
                needs_device: false,
                destructive: false,
                timeout_ms: default_action_timeout(),
                chord: None,
                help: Some("Not in the file".into()),
            }],
            settings: vec![toggle("automatic", Scope::Global)],
            ..Default::default()
        };
        c.settings[0].label = "Automatic".into();
        c.translate(&catalog);
        assert_eq!(c.actions[0].label, "Partajează");
        assert_eq!(c.actions[0].id, "share", "ids are never translated");
        assert_eq!(c.actions[0].help.as_deref(), Some("Not in the file"));
        assert_eq!(
            c.settings[0].label, "Automatic",
            "too long a translation is not used"
        );
        assert!(c.validate().is_ok());
    }

    #[test]
    fn the_words_a_language_file_translates_are_the_ones_translating_looks_up() {
        let mut c = Contributions {
            pages: vec![page("share")],
            actions: vec![Action {
                id: "share".into(),
                label: "Share".into(),
                short_label: Some("Go".into()),
                input: ActionInput::None,
                target: Source {
                    service: "svc".into(),
                    method: "share".into(),
                },
                capability: None,
                needs_device: false,
                destructive: false,
                timeout_ms: default_action_timeout(),
                chord: None,
                help: Some("Sends it".into()),
            }],
            settings: vec![toggle("automatic", Scope::Global)],
            ..Default::default()
        };
        c.settings[0].help = Some("When on".into());
        let words: Vec<String> = c.words().map(str::to_owned).collect();
        assert_eq!(words.len(), 6, "{words:?}");
        // A file translating each listed word into a marked one: afterwards every word reads marked, so nothing a person
        // reads is left out of the list and nothing is translated that the list does not name.
        let mut file = String::from("msgid \"\"\nmsgstr \"Language: ro\\n\"\n");
        for word in &words {
            file.push_str(&format!("\nmsgid \"{word}\"\nmsgstr \"T {word}\"\n"));
        }
        c.translate(&crate::language::Catalog::parse(&file).unwrap());
        let translated: Vec<String> = c.words().map(str::to_owned).collect();
        assert_eq!(
            translated,
            words.iter().map(|w| format!("T {w}")).collect::<Vec<_>>()
        );
    }
    use super::*;

    #[test]
    fn input_capabilities_never_invent_support_from_missing_or_unknown_fields() {
        for bad in [
            r#"{}"#,
            r#"{"independent_pointer":"yes"}"#,
            r#"{"independent_pointer":true,"permission":true}"#,
        ] {
            assert!(serde_json::from_str::<InputCapabilities>(bad).is_err());
        }
        for supported in [false, true] {
            let capability = InputCapabilities {
                independent_pointer: supported,
            };
            let wire = serde_json::to_vec(&capability).unwrap();
            assert_eq!(
                serde_json::from_slice::<InputCapabilities>(&wire).unwrap(),
                capability
            );
        }
    }

    fn page(id: &str) -> Page {
        Page {
            id: id.into(),
            label: "Clipboard".into(),
            placement: Placement::Primary,
            icon: None,
            source: None,
            surface: None,
            needs_device: false,
            capability: None,
            platforms: Vec::new(),
        }
    }

    #[test]
    fn a_surface_page_needs_somewhere_to_send_and_a_device_only_with_a_surface() {
        let surface: Page = serde_json::from_str(
            r#"{"id":"remote","label":"Remote","placement":"primary","surface":"elements","needs_device":false,
                "capability":null,"platforms":["android"],"source":{"service":"control.pointer","method":"view"}}"#,
        )
        .unwrap();
        let valid = |page: Page| {
            Contributions {
                pages: vec![page],
                ..Default::default()
            }
            .validate()
            .is_ok()
        };
        assert!(valid(surface.clone()));
        let mut nowhere = surface.clone();
        nowhere.source = None;
        assert!(
            !valid(nowhere),
            "a surface with nothing to receive its input"
        );
        let mut plain = page("plain");
        plain.needs_device = true;
        assert!(
            !valid(plain),
            "a device chosen for a page that sends nothing"
        );
        // A page written before surfaces existed reads, and writes, exactly as before.
        let old = page("old");
        let written = serde_json::to_value(&old).unwrap();
        assert_eq!(
            written.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["icon", "id", "label", "placement", "source"]
        );
        let encoded = serde_json::to_vec(&surface).unwrap();
        assert_eq!(serde_json::from_slice::<Page>(&encoded).unwrap(), surface);
    }

    #[test]
    fn pointer_input_is_bounded_and_reads_the_same_everywhere() {
        let read = |json: &str| serde_json::from_str::<PointerInput>(json).unwrap();
        assert_eq!(
            read(r#"{"t":"move","dx":1.5,"dy":-2}"#),
            PointerInput::Move { dx: 1.5, dy: -2. }
        );
        assert_eq!(
            read(r#"{"t":"button","button":"right","down":true}"#),
            PointerInput::Button {
                button: Button::Right,
                down: true
            }
        );
        assert_eq!(
            read(r#"{"t":"key","key":"enter"}"#),
            PointerInput::Key { key: Key::Enter }
        );
        assert!(
            serde_json::from_str::<PointerInput>(r#"{"t":"move","dx":1,"dy":1,"x":0}"#).is_err()
        );
        assert!(serde_json::from_str::<PointerInput>(r#"{"t":"teleport"}"#).is_err());
        for good in [
            PointerInput::Move { dx: 3., dy: 4. },
            PointerInput::Scroll { dx: 0, dy: -3 },
            PointerInput::Text {
                text: "Șală ✓".into(),
            },
        ] {
            good.validate().unwrap();
        }
        for bad in [
            PointerInput::Move {
                dx: f32::NAN,
                dy: 0.,
            },
            PointerInput::Move { dx: 1e9, dy: 0. },
            PointerInput::Scroll { dx: 0, dy: 1000 },
            PointerInput::Text {
                text: String::new(),
            },
            PointerInput::Text {
                text: "a\u{1b}[2J".into(),
            },
            PointerInput::Text {
                text: "x".repeat(MAX_TYPED_TEXT + 1),
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }
    fn toggle(id: &str, scope: Scope) -> Setting {
        Setting {
            platforms: Vec::new(),
            id: id.into(),
            label: "Sync".into(),
            help: None,
            scope,
            kind: Kind::Toggle { default: false },
            public_read: None,
        }
    }

    #[test]
    fn public_http_consent_is_one_opt_in_toggle_with_a_disclosed_https_subtree() {
        let mut setting = toggle("public-data", Scope::Global);
        let resource = PublicRead {
            origin: "https://example.test".into(),
            root: "/maps/v1".into(),
        };
        setting.public_read = Some(resource.clone());
        assert_eq!(
            resource.url().unwrap().as_str(),
            "https://example.test/maps/v1"
        );
        assert!(
            Contributions {
                settings: vec![setting.clone()],
                ..Default::default()
            }
            .validate()
            .is_ok()
        );
        for invalid in [
            PublicRead {
                root: "/maps/../files".into(),
                ..resource.clone()
            },
            PublicRead {
                origin: "https://user@example.test".into(),
                ..resource.clone()
            },
            PublicRead {
                origin: "https://127.0.0.1".into(),
                ..resource.clone()
            },
        ] {
            assert!(invalid.url().is_none());
        }
        let mut second = setting.clone();
        second.id = "other".into();
        assert!(
            Contributions {
                settings: vec![setting.clone(), second],
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        setting.kind = Kind::Toggle { default: true };
        assert!(setting.validate().is_err());
        setting.kind = Kind::Toggle { default: false };
        setting.scope = Scope::Device;
        assert!(setting.validate().is_err());
    }

    #[test]
    fn contribution_round_trips_as_plain_data_with_scope() {
        let c = Contributions {
            pages: vec![page("clipboard")],
            actions: vec![Action {
                id: "share".into(),
                label: "Share now".into(),
                short_label: None,
                input: ActionInput::None,
                target: Source {
                    service: "clipboard".into(),
                    method: "capture".into(),
                },
                capability: Some("clipboard.read".into()),
                needs_device: true,
                destructive: false,
                timeout_ms: default_action_timeout(),
                chord: None,
                help: None,
            }],
            settings: vec![
                toggle("send", Scope::Device),
                toggle("enabled", Scope::Global),
            ],
            input: None,
            initially_withheld_actions: BTreeSet::new(),
        };
        c.validate().unwrap();
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<Contributions>(&json).unwrap(), c);
        assert!(json.contains(r#""scope":"device""#));
    }

    #[test]
    fn selected_file_actions_require_send_input_not_a_device() {
        let mut action: Action = serde_json::from_str(
            r#"{"id":"upload","label":"Upload","input":"selected_file","target":{"service":"file","method":"upload"}}"#
        ).unwrap();
        let contributions = |action| Contributions {
            actions: vec![action],
            ..Default::default()
        };
        contributions(action.clone()).validate().unwrap();
        let encoded = serde_json::to_vec(&action).unwrap();
        assert_eq!(serde_json::from_slice::<Action>(&encoded).unwrap(), action);
        action.needs_device = true;
        assert!(contributions(action).validate().is_err());
    }

    #[test]
    fn initial_hiding_only_names_declared_actions_and_labels_are_single_line() {
        let mut contributions: Contributions = serde_json::from_value(serde_json::json!({
            "initially_withheld_actions":["send"],
            "actions":[{"id":"send", "label":"Send", "short_label":"Send now",
                "target":{"service":"files","method":"send"}}]
        }))
        .unwrap();
        assert!(contributions.validate().is_ok());
        contributions
            .initially_withheld_actions
            .insert("unknown".into());
        assert!(contributions.validate().is_err());
        contributions.initially_withheld_actions.remove("unknown");
        for invalid in [
            "",
            "two\nlines",
            "two\u{2029}lines",
            "\u{1b}control",
            &"x".repeat(81),
        ] {
            contributions.actions[0].short_label = Some(invalid.to_owned());
            assert!(contributions.validate().is_err());
        }
    }

    #[test]
    fn secret_text_is_a_local_non_destructive_input_declaration() {
        let mut action: Action = serde_json::from_str(
            r#"{"id":"credential","label":"Credential","input":"secret_text","target":{"service":"settings","method":"configure"}}"#
        ).unwrap();
        let valid = |action| {
            Contributions {
                actions: vec![action],
                ..Default::default()
            }
            .validate()
            .is_ok()
        };
        assert!(valid(action.clone()));
        action.needs_device = true;
        assert!(!valid(action.clone()));
        action.needs_device = false;
        action.destructive = true;
        assert!(!valid(action));
    }

    #[test]
    fn shared_text_actions_require_a_device_and_every_other_input_may_go_without_one() {
        let action = |input: &str, needs_device: bool| -> Action {
            serde_json::from_str(&format!(
                r#"{{"id":"send","label":"Send","input":"{input}","needs_device":{needs_device},"target":{{"service":"text","method":"send"}}}}"#
            ))
            .unwrap()
        };
        let valid = |action: Action| {
            Contributions {
                actions: vec![action],
                ..Default::default()
            }
            .validate()
            .is_ok()
        };
        // The name a manifest writes is the name the parser reads: an action declared with it is not refused for its spelling.
        assert_eq!(action("shared_text", true).input, ActionInput::SharedText);
        // The whole table: shared text needs the device the person chose, a file never has one, and a plain action either way.
        assert!(valid(action("shared_text", true)));
        assert!(
            !valid(action("shared_text", false)),
            "shared text with nobody to send it to is refused"
        );
        assert!(valid(action("selected_file", false)));
        assert!(!valid(action("selected_file", true)));
        assert!(valid(action("none", false)));
        assert!(valid(action("none", true)));
        // And it survives a round trip as plain data, so a stored or transmitted action keeps its meaning.
        let kept = action("shared_text", true);
        let encoded = serde_json::to_vec(&kept).unwrap();
        assert_eq!(serde_json::from_slice::<Action>(&encoded).unwrap(), kept);
    }

    #[test]
    fn a_suggested_chord_is_only_for_an_action_a_key_can_run_as_it_is() {
        let action = |id: &str, more: &str| -> Action {
            serde_json::from_str(&format!(
                r#"{{"id":"{id}","label":"Go","target":{{"service":"go","method":"go"}}{more}}}"#
            ))
            .unwrap()
        };
        let valid = |actions: Vec<Action>| {
            Contributions {
                actions,
                ..Default::default()
            }
            .validate()
            .is_ok()
        };
        let suggested = action("go", r#","chord":"ctrl+alt+1","needs_device":true"#);
        assert_eq!(
            suggested.chord.map(|chord| chord.to_string()).as_deref(),
            Some("ctrl+alt+1")
        );
        assert!(valid(vec![suggested.clone()]));
        let encoded = serde_json::to_vec(&suggested).unwrap();
        assert_eq!(
            serde_json::from_slice::<Action>(&encoded).unwrap(),
            suggested
        );
        // An action written before chords existed is read, and written, exactly as before.
        let plain = action("go", "");
        assert!(plain.chord.is_none());
        assert!(
            !serde_json::to_value(&plain)
                .unwrap()
                .as_object()
                .unwrap()
                .contains_key("chord")
        );
        // Nothing a key cannot run by itself, and nothing a slip of the hand should run.
        for (more, why) in [
            (
                r#","chord":"ctrl+alt+1","input":"selected_file""#,
                "needs a file staged",
            ),
            (
                r#","chord":"ctrl+alt+1","input":"shared_text","needs_device":true"#,
                "needs a text staged",
            ),
            (
                r#","chord":"ctrl+alt+1","input":"secret_text""#,
                "needs a secret typed",
            ),
            (r#","chord":"ctrl+alt+1","destructive":true"#, "destructive"),
        ] {
            assert!(!valid(vec![action("go", more)]), "{why}");
        }
        // One extension does not suggest one chord for two of its actions.
        assert!(!valid(vec![
            action("one", r#","chord":"ctrl+alt+1""#),
            action("two", r#","chord":"ctrl+alt+1""#),
        ]));
        assert!(valid(vec![
            action("one", r#","chord":"ctrl+alt+1""#),
            action("two", r#","chord":"ctrl+alt+2""#),
        ]));
        // A chord that is not one is a manifest that is not read, like any field not understood.
        for bad in [r#""a""#, r#""alt+ctrl+1""#, r#"{"key":"1"}"#, "7"] {
            let json = format!(
                r#"{{"id":"go","label":"Go","target":{{"service":"go","method":"go"}},"chord":{bad}}}"#
            );
            assert!(serde_json::from_str::<Action>(&json).is_err(), "{bad}");
        }
    }

    #[test]
    fn compact_action_labels_are_optional_and_reject_hostile_text() {
        let legacy = r#"{"id":"send","label":"Send clipboard text","target":{"service":"clipboard","method":"send"}}"#;
        let mut action: Action = serde_json::from_str(legacy).unwrap();
        assert!(action.short_label.is_none());
        let legacy_keys: BTreeSet<_> = serde_json::to_value(&action)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(
            legacy_keys,
            [
                "id",
                "label",
                "target",
                "capability",
                "needs_device",
                "destructive",
                "timeout_ms"
            ]
            .map(str::to_owned)
            .into()
        );
        for label in [
            "".to_owned(),
            "   ".to_owned(),
            "bad\nlabel".to_owned(),
            "é".repeat(MAX_LABEL + 1),
        ] {
            action.short_label = Some(label);
            assert!(
                Contributions {
                    actions: vec![action.clone()],
                    ..Default::default()
                }
                .validate()
                .is_err()
            );
        }
        action.short_label = Some("Send".into());
        Contributions {
            actions: vec![action.clone()],
            ..Default::default()
        }
        .validate()
        .unwrap();
        let json = serde_json::to_string(&action).unwrap();
        assert_eq!(serde_json::from_str::<Action>(&json).unwrap(), action);
    }

    #[test]
    fn an_action_description_is_optional_and_never_longer_than_the_limit() {
        let mut action: Action = serde_json::from_str(
            r#"{"id":"go","label":"Go","target":{"service":"go","method":"go"}}"#,
        )
        .unwrap();
        assert!(action.help.is_none());
        let valid = |action: &Action| {
            Contributions {
                actions: vec![action.clone()],
                ..Default::default()
            }
            .validate()
            .is_ok()
        };
        assert!(valid(&action));
        action.help = Some("é".repeat(MAX_HELP));
        assert!(valid(&action));
        for help in [
            "é".repeat(MAX_HELP + 1),
            "two\nlines".to_owned(),
            " ".to_owned(),
        ] {
            action.help = Some(help);
            assert!(!valid(&action));
        }
    }

    #[test]
    fn duplicates_collide_within_a_kind_but_not_across_kinds() {
        let dup = Contributions {
            pages: vec![page("a"), page("a")],
            ..Default::default()
        };
        assert_eq!(dup.validate(), Err(Error::InvalidManifest));
        let cross = Contributions {
            pages: vec![page("x")],
            settings: vec![toggle("x", Scope::Global)],
            ..Default::default()
        };
        cross.validate().unwrap();
    }

    #[test]
    fn unknown_fields_and_hostile_text_fail_closed() {
        assert!(serde_json::from_str::<Contributions>(r#"{"identity":"host"}"#).is_err());
        assert!(
            serde_json::from_str::<Setting>(
                r#"{"id":"a","label":"A","kind":{"type":"toggle","default":true,"x":1}}"#
            )
            .is_err()
        );
        let mut bad = page("p");
        bad.label = "bad\u{7}".into();
        assert!(
            Contributions {
                pages: vec![bad],
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn defaults_must_satisfy_their_own_constraints() {
        let kinds_bad = [
            Kind::Number {
                min: 1,
                max: 10,
                default: 11,
            },
            Kind::Number {
                min: 5,
                max: 5,
                default: 5,
            },
            Kind::Choice {
                choices: vec!["a".into(), "a".into()],
                default: "a".into(),
            },
            Kind::Choice {
                choices: vec!["a".into()],
                default: "b".into(),
            },
            Kind::Choice {
                choices: vec![],
                default: "a".into(),
            },
            Kind::Text {
                max_len: 3,
                default: "toolong".into(),
            },
            Kind::Text {
                max_len: 0,
                default: String::new(),
            },
        ];
        for k in kinds_bad {
            assert!(k.validate().is_err(), "{k:?}");
        }
        assert!(
            Kind::Number {
                min: 1,
                max: 10,
                default: 5
            }
            .validate()
            .is_ok()
        );
        assert!(
            Kind::Choice {
                choices: vec!["a".into()],
                default: "a".into()
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn collections_are_bounded() {
        let pages = (0..=MAX_PAGES).map(|i| page(&format!("p{i}"))).collect();
        assert!(
            Contributions {
                pages,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
}
