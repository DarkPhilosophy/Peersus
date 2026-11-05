//! Declarative, toolkit-neutral extension points.
//!
//! A plugin describes *what* it adds with stable identifiers and plain data. It never
//! names a widget, renderer, window or toolkit; any frontend renders the same
//! description. Contributor identity is assigned by the host, not claimed here.
use crate::{Error, Result, name};
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
    /// A top-level destination in primary navigation.
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Setting {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub help: Option<String>,
    #[serde(default)]
    pub scope: Scope,
    pub kind: Kind,
}

/// An extension that transports platform pointer input to a paired desktop. This is independent
/// of pages: changing or removing its editor cannot change forwarding or its per-device guard.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub source: Source,
    pub capability: String,
}

/// Everything one plugin adds to the product surface.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contributions {
    #[serde(default)]
    pub pages: Vec<Page>,
    #[serde(default)]
    pub actions: Vec<Action>,
    #[serde(default)]
    pub settings: Vec<Setting>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Input>,
}

fn ok(valid: bool) -> Result<()> {
    valid.then_some(()).ok_or(Error::InvalidManifest)
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
    fn validate(&self) -> Result<()> {
        ok(name::valid(&self.id)
            && name::text(&self.label, MAX_LABEL)
            && self.help.as_deref().is_none_or(|h| name::text(h, MAX_HELP)))?;
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
                && name::text(&a.label, MAX_LABEL)
                && a.short_label
                    .as_deref()
                    .is_none_or(|label| name::text(label, MAX_LABEL))
                && a.capability.as_deref().is_none_or(name::valid)
                && !(a.input == ActionInput::SelectedFile && a.needs_device)
                // Shared text is sent to a device the person chose, so it needs one declared.
                && (a.input != ActionInput::SharedText || a.needs_device)
                && (a.input != ActionInput::SecretText || (!a.needs_device && !a.destructive))
                && a.timeout_ms > 0
                && a.timeout_ms <= MAX_ACTION_TIMEOUT_MS
                && seen.insert(("action", a.id.as_str())))?;
            a.target.validate()?;
        }
        for s in &self.settings {
            s.validate()?;
            ok(seen.insert(("setting", s.id.as_str())))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            id: id.into(),
            label: "Sync".into(),
            help: None,
            scope,
            kind: Kind::Toggle { default: false },
        }
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
            }],
            settings: vec![
                toggle("send", Scope::Device),
                toggle("enabled", Scope::Global),
            ],
            input: None,
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
