//! Plugin-agnostic state: identities, per-device grants and typed setting values.
//!
//! Nothing here names a concrete plugin. A plugin is an opaque [`PluginId`] assigned
//! by the host; what it may do for a device is a set of capability names; what a user
//! configured is a map of setting id to value, validated against the [`Kind`] the
//! plugin declared in its manifest.
use peersus_api::{
    contributions::{Kind, Scope, Setting},
    name,
    permission::Permission,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

/// Host-assigned plugin identity, validated with the shared identifier grammar.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PluginId(String);

impl PluginId {
    pub fn new(value: impl Into<String>) -> Result<Self, PluginStateError> {
        let value = value.into();
        name::valid(&value)
            .then_some(Self(value))
            .ok_or(PluginStateError::Identifier)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for PluginId {
    type Error = PluginStateError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<PluginId> for String {
    fn from(value: PluginId) -> Self {
        value.0
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PluginStateError {
    #[error("invalid plugin or setting identifier")]
    Identifier,
    #[error("unknown setting")]
    UnknownSetting,
    #[error("value does not match the declared setting type")]
    Type,
    #[error("value is outside the declared setting limits")]
    Range,
    #[error("setting is not valid for this scope")]
    Scope,
    #[error("invalid status identifier or label")]
    Status,
    #[error("a plugin may not have this many statuses")]
    TooManyStatuses,
}

/// How many statuses one plugin may define on this device.
pub const MAX_STATUSES_PER_PLUGIN: usize = 8;
/// How many custom statuses may be active on one device at once, whoever defined them. A device says this much of itself to every peer.
pub const MAX_ACTIVE_STATUSES: usize = 8;
/// The longest label a status may have, in characters: it is read by a person on another device. The same bound as every other label.
pub const MAX_STATUS_LABEL: usize = peersus_api::contributions::MAX_LABEL;

/// The words only the application says of a device. A plugin can neither define nor set them: one that could say `offline` or `online`
/// could make a device look gone or awake when it is not. The set of everything else is open and is listed nowhere.
pub const SYSTEM_STATUSES: [&str; 3] = ["online", "sleeping", "offline"];

/// Characters that change how text is drawn or take no room. A label must not carry them: one word could be made to read as another.
fn hides_or_redirects(c: char) -> bool {
    matches!(c,
        '\u{00AD}'                // soft hyphen
        | '\u{200B}'..='\u{200F}' // zero-width space, joiners, left-to-right and right-to-left marks
        | '\u{202A}'..='\u{202E}' // directional embeddings and overrides
        | '\u{2060}'..='\u{2064}' // word joiner and invisible operators
        | '\u{2066}'..='\u{2069}' // directional isolates
        | '\u{FEFF}'              // zero-width no-break space
    )
}

/// The identifier of one status inside the plugin that defined it. Chosen by that plugin, so only ever meaningful together with the
/// plugin: see [`StatusKey`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct StatusId(String);

impl StatusId {
    /// The shared identifier grammar, and not one of the application's own words, whatever the case.
    pub fn new(value: impl Into<String>) -> Result<Self, PluginStateError> {
        let value = value.into();
        (name::valid(&value) && !SYSTEM_STATUSES.contains(&value.to_ascii_lowercase().as_str()))
            .then_some(Self(value))
            .ok_or(PluginStateError::Status)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for StatusId {
    type Error = PluginStateError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<StatusId> for String {
    fn from(value: StatusId) -> Self {
        value.0
    }
}

/// What a status is called, in the words of whoever wrote the plugin. Never translated or rewritten by the application, and shown as it is.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct StatusLabel(String);

impl StatusLabel {
    /// Plain visible text of bounded length, with nothing that hides or redirects, and not one of the application's own words.
    pub fn new(value: impl Into<String>) -> Result<Self, PluginStateError> {
        let value = value.into();
        let plain = name::text(&value, MAX_STATUS_LABEL)
            && value == value.trim()
            && !value.chars().any(hides_or_redirects)
            && !SYSTEM_STATUSES.contains(&value.to_lowercase().as_str());
        plain.then_some(Self(value)).ok_or(PluginStateError::Status)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for StatusLabel {
    type Error = PluginStateError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<StatusLabel> for String {
    fn from(value: StatusLabel) -> Self {
        value.0
    }
}

/// Who said what: the plugin that owns a status and the identifier it gave it. The plugin is the one the HOST assigned, never one a
/// message claims, so a plugin cannot speak for another; this pair is how a status is named everywhere, from the plugin that set it to
/// the screen of a device that has never heard of that plugin, and nothing in between drops either half.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusKey {
    pub plugin: PluginId,
    pub id: StatusId,
}

/// A status as another device sees it: whose it is, what it is called, and the words its author gave it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusInfo {
    #[serde(flatten)]
    pub key: StatusKey,
    pub label: StatusLabel,
}

/// What a user granted one plugin for one device: permissions, each OF SOMEONE.
///
/// A grant is never a bare name. `peers.send` of the application and a `peers.send` that a
/// plugin declared for itself are different permissions, and a user granting one has not
/// granted the other, so what is stored is the pair of the participant that requires it and
/// the name it gave it. Default-deny: an absent or empty set grants nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "BTreeSet<Permission>", into = "BTreeSet<Permission>")]
pub struct Grants(BTreeSet<Permission>);

/// Upper bounds keep persisted or remote state from growing without limit.
pub const MAX_GRANTS: usize = 64;
pub const MAX_VALUES: usize = 64;

impl TryFrom<BTreeSet<Permission>> for Grants {
    type Error = PluginStateError;
    /// Checked again whenever one is read back, because a stored or received grant is not
    /// trusted for having been valid when it was written.
    fn try_from(set: BTreeSet<Permission>) -> Result<Self, Self::Error> {
        (set.len() <= MAX_GRANTS && set.iter().all(Permission::is_valid))
            .then_some(Self(set))
            .ok_or(PluginStateError::Identifier)
    }
}
impl From<Grants> for BTreeSet<Permission> {
    fn from(grants: Grants) -> Self {
        grants.0
    }
}

impl Grants {
    /// Build from permissions; every one must have a valid owner and name.
    pub fn new<I>(permissions: I) -> Result<Self, PluginStateError>
    where
        I: IntoIterator<Item = Permission>,
    {
        Self::try_from(permissions.into_iter().collect::<BTreeSet<_>>())
    }
    /// Whether exactly this permission, of exactly this owner, was granted.
    pub fn allows(&self, permission: &Permission) -> bool {
        self.0.contains(permission)
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn iter(&self) -> impl Iterator<Item = &Permission> {
        self.0.iter()
    }
}

/// What putting a package under a plugin's name is, for what the person allowed under that name. ONE rule, read by the owner
/// that installs and by the window that says what pressing Update costs, so that the two never disagree.
///
/// The name (the plugin's id) is all that joins two packages, and a name proves nothing about who made a package. Only the
/// signed store does: the maintainer reviewed both versions and the catalog binds the name to its owner. So an update keeps
/// what was allowed only when BOTH the installed package and the new one came through the store; anything else under a taken
/// name, from a file or the command line, or a store package over one put there from elsewhere, starts with nothing allowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Installation {
    /// Nothing is installed under the name. It starts with nothing allowed. Removing a plugin and installing one again under
    /// the same name is this too: the removal forgot everything.
    New,
    /// A strictly newer version, from the signed store, of an extension in good standing that was itself installed from the
    /// signed store. What the person allowed it that the new version still asks for is kept; whatever the new version asks
    /// for that the old one did not starts off until the person allows it; and what it no longer asks for is forgotten, so a
    /// later version that asks for it again asks the person again.
    StoreUpdate,
    /// Any other package put in place of an installed one: from outside the store, over a package that did not come from
    /// the store or that is damaged, or not newer. It replaces the installed one and starts with nothing allowed.
    Replacement,
}

/// What is installed under a name, as far as [`Installation::of`] needs to know.
#[derive(Clone, Copy, Debug)]
pub struct Present<'a> {
    /// It was installed from the signed store (recorded in its folder by the owner when it was installed).
    pub from_store: bool,
    /// It is what was installed and approved (ready, or switched off by the person): not changed, damaged or unapproved.
    pub sound: bool,
    /// The version its package declares; `None` when it declares none that can be read.
    pub version: Option<&'a str>,
}

impl Installation {
    /// What installing a package declaring `incoming_version`, from the store or not, over `installed` is.
    pub fn of(
        installed: Option<Present<'_>>,
        incoming_from_store: bool,
        incoming_version: Option<&str>,
    ) -> Self {
        use peersus_api::package::Version;
        let Some(installed) = installed else {
            return Self::New;
        };
        let newer = matches!(
            (installed.version.and_then(Version::parse), incoming_version.and_then(Version::parse)),
            (Some(have), Some(next)) if next > have
        );
        if incoming_from_store && installed.from_store && installed.sound && newer {
            Self::StoreUpdate
        } else {
            Self::Replacement
        }
    }

    /// Whether what the person allowed under the name outlives the installation (narrowed to what the new version asks for).
    pub fn keeps_grants(self) -> bool {
        matches!(self, Self::StoreUpdate)
    }
}

/// A user-chosen value for one declared setting.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Toggle(bool),
    Number(i64),
    Text(String),
}

impl Value {
    /// The value a manifest declares as the default for a setting kind.
    pub fn default_for(kind: &Kind) -> Self {
        match kind {
            Kind::Toggle { default } => Self::Toggle(*default),
            Kind::Number { default, .. } => Self::Number(*default),
            Kind::Choice { default, .. } | Kind::Text { default, .. } => {
                Self::Text(default.clone())
            }
        }
    }

    /// Check a candidate against the declared type and its own constraints.
    pub fn check(&self, kind: &Kind) -> Result<(), PluginStateError> {
        match (self, kind) {
            (Self::Toggle(_), Kind::Toggle { .. }) => Ok(()),
            (Self::Number(n), Kind::Number { min, max, .. }) => (min..=max)
                .contains(&n)
                .then_some(())
                .ok_or(PluginStateError::Range),
            (Self::Text(t), Kind::Choice { choices, .. }) => choices
                .contains(t)
                .then_some(())
                .ok_or(PluginStateError::Range),
            (Self::Text(t), Kind::Text { max_len, .. }) => (t.chars().count() <= *max_len
                && !t.chars().any(char::is_control))
            .then_some(())
            .ok_or(PluginStateError::Range),
            _ => Err(PluginStateError::Type),
        }
    }
}

/// One plugin's saved setting values, keyed by setting id.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "BTreeMap<String, Value>", into = "BTreeMap<String, Value>")]
pub struct Values(BTreeMap<String, Value>);

impl TryFrom<BTreeMap<String, Value>> for Values {
    type Error = PluginStateError;
    /// Keys must be valid setting ids. Whether each value matches its manifest is
    /// checked by [`Values::set`] and [`Values::effective`], which hold the manifest.
    fn try_from(map: BTreeMap<String, Value>) -> Result<Self, Self::Error> {
        (map.len() <= MAX_VALUES && map.keys().all(|k| name::valid(k)))
            .then_some(Self(map))
            .ok_or(PluginStateError::Identifier)
    }
}
impl From<Values> for BTreeMap<String, Value> {
    fn from(values: Values) -> Self {
        values.0
    }
}

impl Values {
    pub fn get(&self, id: &str) -> Option<&Value> {
        self.0.get(id)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Set one value after checking it against the manifest's declaration for the
    /// given scope. Unknown ids and wrong-scope settings are rejected, so stored
    /// state can never contain a value no manifest declared.
    pub fn set(
        &mut self,
        declared: &[Setting],
        scope: Scope,
        id: &str,
        value: Value,
    ) -> Result<(), PluginStateError> {
        let setting = declared
            .iter()
            .find(|s| s.id == id)
            .ok_or(PluginStateError::UnknownSetting)?;
        if setting.scope != scope {
            return Err(PluginStateError::Scope);
        }
        value.check(&setting.kind)?;
        self.0.insert(id.to_owned(), value);
        Ok(())
    }

    /// Effective value: the saved one if still valid, otherwise the declared default.
    pub fn effective(&self, setting: &Setting) -> Value {
        self.0
            .get(&setting.id)
            .filter(|v| v.check(&setting.kind).is_ok())
            .cloned()
            .unwrap_or_else(|| Value::default_for(&setting.kind))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_status_id_is_a_plain_identifier_and_never_one_of_the_applications_own_words() {
        for good in ["watching", "in-a-call", "focus.mode", "a1_b2"] {
            assert!(StatusId::new(good).is_ok(), "{good}");
        }
        // The application's words are reserved whatever the case, so a plugin cannot say a device is offline or awake.
        for system in [
            "online", "sleeping", "offline", "Offline", "ONLINE", "Sleeping",
        ] {
            assert_eq!(
                StatusId::new(system),
                Err(PluginStateError::Status),
                "{system}"
            );
        }
        for bad in ["", "a b", "a/b", "é", "a:b", &"x".repeat(129)] {
            assert_eq!(StatusId::new(bad), Err(PluginStateError::Status), "{bad:?}");
        }
    }

    #[test]
    fn a_status_label_is_plain_visible_text_that_cannot_pass_for_another_word() {
        for good in [
            "Watching a video",
            "În apel",
            "A",
            &"é".repeat(MAX_STATUS_LABEL),
        ] {
            assert!(StatusLabel::new(good).is_ok(), "{good}");
        }
        let refused: [(&str, &str); 14] = [
            ("", "empty"),
            ("   ", "blank"),
            (" padded", "leading space"),
            ("padded ", "trailing space"),
            ("bad\u{7}", "control"),
            ("two\nlines", "newline"),
            ("zero\u{200B}width", "zero-width space"),
            ("a\u{202E}b", "right-to-left override"),
            ("a\u{2066}b", "directional isolate"),
            ("a\u{FEFF}b", "byte order mark"),
            ("soft\u{00AD}hyphen", "soft hyphen"),
            ("Offline", "the application's word"),
            ("sLeEpInG", "the application's word, another case"),
            ("Online", "the application's word again"),
        ];
        for (label, why) in refused {
            assert_eq!(
                StatusLabel::new(label),
                Err(PluginStateError::Status),
                "{why}: {label:?}"
            );
        }
        assert_eq!(
            StatusLabel::new("é".repeat(MAX_STATUS_LABEL + 1)),
            Err(PluginStateError::Status),
            "too long"
        );
    }

    #[test]
    fn a_status_is_checked_again_when_it_is_read_back_from_anywhere() {
        // What arrives from another device or from a plugin is not trusted for having been valid where it was written.
        assert!(serde_json::from_str::<StatusId>(r#""watching""#).is_ok());
        assert!(serde_json::from_str::<StatusId>(r#""offline""#).is_err());
        assert!(serde_json::from_str::<StatusId>(r#""a b""#).is_err());
        assert!(serde_json::from_str::<StatusLabel>(r#""Watching""#).is_ok());
        assert!(serde_json::from_str::<StatusLabel>(r#""a\u202eb""#).is_err());
        assert!(serde_json::from_str::<StatusLabel>(r#""online""#).is_err());
    }

    #[test]
    fn a_status_keeps_both_whose_it_is_and_what_it_is_called_through_a_round_trip() {
        let info = StatusInfo {
            key: StatusKey {
                plugin: PluginId::new("player").unwrap(),
                id: StatusId::new("watching").unwrap(),
            },
            label: StatusLabel::new("Watching a video").unwrap(),
        };
        let json = serde_json::to_string(&info).unwrap();
        // Flat on the wire: the owner, the identifier and the words, with nothing dropped and nothing added.
        assert_eq!(
            json,
            r#"{"plugin":"player","id":"watching","label":"Watching a video"}"#
        );
        assert_eq!(serde_json::from_str::<StatusInfo>(&json).unwrap(), info);
        // Two plugins may use the same identifier: they are two different statuses, and neither is the other.
        let other = StatusKey {
            plugin: PluginId::new("timer").unwrap(),
            id: StatusId::new("watching").unwrap(),
        };
        assert_ne!(info.key, other);
        // Nothing rides along, and neither half may be missing.
        for bad in [
            r#"{"plugin":"player","id":"watching","label":"x","extra":1}"#,
            r#"{"id":"watching","label":"x"}"#,
            r#"{"plugin":"player","label":"x"}"#,
            r#"{"plugin":"player","id":"watching"}"#,
            r#"{"plugin":"p q","id":"watching","label":"x"}"#,
        ] {
            assert!(
                serde_json::from_str::<StatusInfo>(bad).is_err(),
                "{bad} was accepted"
            );
        }
    }

    fn setting(id: &str, scope: Scope, kind: Kind) -> Setting {
        Setting {
            public_read: None,
            platforms: Vec::new(),
            id: id.into(),
            label: id.into(),
            help: None,
            scope,
            kind,
        }
    }

    #[test]
    fn plugin_ids_share_the_api_identifier_grammar_including_when_deserialized() {
        assert!(PluginId::new("clipboard").is_ok());
        for bad in ["", "a b", "a/b", "é"] {
            assert_eq!(PluginId::new(bad), Err(PluginStateError::Identifier));
        }
        assert!(serde_json::from_str::<PluginId>("\"a/b\"").is_err());
        assert_eq!(
            serde_json::to_string(&PluginId::new("a.b").unwrap()).unwrap(),
            "\"a.b\""
        );
    }

    fn permission(owner: &str, name: &str) -> Permission {
        Permission::new(owner, name).unwrap()
    }

    #[test]
    fn grants_default_deny_and_a_grant_is_a_permission_of_someone() {
        assert!(!Grants::default().allows(&permission("host", "peers.send")));
        let g = Grants::new([permission("host", "peers.send")]).unwrap();
        assert!(g.allows(&permission("host", "peers.send")));
        // The same name under another owner is another permission, and is not granted.
        assert!(!g.allows(&permission("evil", "peers.send")));
        // And another name under the same owner is not granted either.
        assert!(!g.allows(&permission("host", "peers.list")));
    }

    #[test]
    fn grants_refuse_what_is_not_a_valid_permission_whatever_way_it_arrives() {
        // Built in code: a permission whose parts are not identifiers cannot be put in.
        let bad = Permission {
            owner: "host".into(),
            name: "bad name".into(),
        };
        assert_eq!(Grants::new([bad]), Err(PluginStateError::Identifier));
        // Read from disk or the wire: the same refusal, checked when it is read back.
        let malformed = r#"[{"owner":"host","name":"bad name"}]"#;
        assert!(serde_json::from_str::<Grants>(malformed).is_err());
        // The old shape, a bare list of names with no owner, is refused: a name alone does
        // not say whose permission it is.
        assert!(serde_json::from_str::<Grants>(r#"["peers.send"]"#).is_err());
        // Nothing may ride along with a permission.
        let extra = r#"[{"owner":"host","name":"peers.send","granted":true}]"#;
        assert!(serde_json::from_str::<Grants>(extra).is_err());
    }

    #[test]
    fn grants_round_trip_and_keep_the_owner() {
        let g = Grants::new([permission("host", "peers.send"), permission("p", "go")]).unwrap();
        let json = serde_json::to_string(&g).unwrap();
        assert!(json.contains(r#""owner":"host""#), "{json}");
        let back: Grants = serde_json::from_str(&json).unwrap();
        assert_eq!(back, g);
    }

    #[test]
    fn grants_can_be_listed_with_their_owners() {
        let g = Grants::new([permission("b", "x"), permission("a", "y")]).unwrap();
        let listed: Vec<String> = g
            .iter()
            .map(|p| format!("{}/{}", p.owner, p.name))
            .collect();
        assert_eq!(
            listed,
            ["a/y", "b/x"],
            "listing is ordered and keeps the owner"
        );
    }

    #[test]
    fn values_are_checked_against_the_declared_kind() {
        let declared = [
            setting("on", Scope::Global, Kind::Toggle { default: false }),
            setting(
                "n",
                Scope::Global,
                Kind::Number {
                    min: 1,
                    max: 5,
                    default: 2,
                },
            ),
            setting(
                "c",
                Scope::Global,
                Kind::Choice {
                    choices: vec!["a".into(), "b".into()],
                    default: "a".into(),
                },
            ),
            setting(
                "t",
                Scope::Global,
                Kind::Text {
                    max_len: 3,
                    default: String::new(),
                },
            ),
            setting("d", Scope::Device, Kind::Toggle { default: false }),
        ];
        let mut v = Values::default();
        v.set(&declared, Scope::Global, "on", Value::Toggle(true))
            .unwrap();
        v.set(&declared, Scope::Global, "n", Value::Number(5))
            .unwrap();
        v.set(&declared, Scope::Global, "c", Value::Text("b".into()))
            .unwrap();
        v.set(&declared, Scope::Global, "t", Value::Text("abc".into()))
            .unwrap();
        let rejected = |r: Result<(), PluginStateError>| r.unwrap_err();
        assert_eq!(
            rejected(v.set(&declared, Scope::Global, "n", Value::Number(6))),
            PluginStateError::Range
        );
        assert_eq!(
            rejected(v.set(&declared, Scope::Global, "c", Value::Text("z".into()))),
            PluginStateError::Range
        );
        assert_eq!(
            rejected(v.set(&declared, Scope::Global, "t", Value::Text("abcd".into()))),
            PluginStateError::Range
        );
        assert_eq!(
            rejected(v.set(&declared, Scope::Global, "on", Value::Number(1))),
            PluginStateError::Type
        );
        assert_eq!(
            rejected(v.set(&declared, Scope::Global, "nope", Value::Toggle(true))),
            PluginStateError::UnknownSetting
        );
        // A per-device setting cannot be written as a global one, nor the reverse.
        assert_eq!(
            rejected(v.set(&declared, Scope::Global, "d", Value::Toggle(true))),
            PluginStateError::Scope
        );
        assert_eq!(
            rejected(v.set(&declared, Scope::Device, "on", Value::Toggle(true))),
            PluginStateError::Scope
        );
        assert_eq!(v.len(), 4);
    }

    #[test]
    fn effective_value_falls_back_to_default_when_saved_value_became_invalid() {
        let s = setting(
            "n",
            Scope::Global,
            Kind::Number {
                min: 1,
                max: 5,
                default: 2,
            },
        );
        let mut v = Values::default();
        assert_eq!(v.effective(&s), Value::Number(2));
        v.set(
            std::slice::from_ref(&s),
            Scope::Global,
            "n",
            Value::Number(4),
        )
        .unwrap();
        assert_eq!(v.effective(&s), Value::Number(4));
        // A newer manifest narrowed the range; the stale stored value is not trusted.
        let narrowed = setting(
            "n",
            Scope::Global,
            Kind::Number {
                min: 1,
                max: 3,
                default: 1,
            },
        );
        assert_eq!(v.effective(&narrowed), Value::Number(1));
    }

    #[test]
    fn untagged_value_round_trips_distinctly() {
        for value in [
            Value::Toggle(true),
            Value::Number(-3),
            Value::Text("x".into()),
        ] {
            let json = serde_json::to_string(&value).unwrap();
            assert_eq!(serde_json::from_str::<Value>(&json).unwrap(), value);
        }
    }

    #[test]
    fn only_a_newer_store_package_over_a_sound_store_install_keeps_what_was_allowed() {
        let store = |version| Present {
            from_store: true,
            sound: true,
            version: Some(version),
        };
        assert_eq!(
            Installation::of(None, true, Some("1.0.0")),
            Installation::New
        );
        assert_eq!(
            Installation::of(None, false, Some("1.0.0")),
            Installation::New
        );
        let update = Installation::of(Some(store("1.0.0")), true, Some("1.1.0"));
        assert_eq!(update, Installation::StoreUpdate);
        assert!(update.keeps_grants());
        // From outside the store, even a newer version of a store extension starts with nothing.
        assert_eq!(
            Installation::of(Some(store("1.0.0")), false, Some("2.0.0")),
            Installation::Replacement
        );
        // The store over a package put there from elsewhere: the same name does not make it the same author.
        let outside = Present {
            from_store: false,
            ..store("1.0.0")
        };
        assert_eq!(
            Installation::of(Some(outside), true, Some("2.0.0")),
            Installation::Replacement
        );
        // A damaged or unapproved folder is repaired, not trusted.
        let damaged = Present {
            sound: false,
            ..store("1.0.0")
        };
        assert_eq!(
            Installation::of(Some(damaged), true, Some("2.0.0")),
            Installation::Replacement
        );
        // Not newer, or a version that cannot be read, is never taken for an update on a guess.
        for (have, next) in [
            (Some("1.0.0"), Some("1.0.0")),
            (Some("2.0.0"), Some("1.0.0")),
            (None, Some("1.0.0")),
            (Some("1.0.0"), None),
            (Some("1.0.0"), Some("x")),
        ] {
            let installed = Present {
                version: have,
                ..store("1.0.0")
            };
            let kind = Installation::of(Some(installed), true, next);
            assert_eq!(kind, Installation::Replacement, "{have:?} -> {next:?}");
            assert!(!kind.keeps_grants());
        }
        assert!(!Installation::New.keeps_grants());
    }
}
