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
    /// Exported `(service, method)` returning the page's view data.
    #[serde(default)]
    pub source: Option<Source>,
}

/// An exported service method addressed by data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub service: String,
    pub method: String,
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
        let mut seen = BTreeSet::new();
        for p in &self.pages {
            ok(name::valid(&p.id)
                && name::text(&p.label, MAX_LABEL)
                && p.icon.as_deref().is_none_or(name::valid)
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
        };
        c.validate().unwrap();
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<Contributions>(&json).unwrap(), c);
        assert!(json.contains(r#""scope":"device""#));
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
