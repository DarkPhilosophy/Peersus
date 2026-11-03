//! Plugin-agnostic state: identities, per-device grants and typed setting values.
//!
//! Nothing here names a concrete plugin. A plugin is an opaque [`PluginId`] assigned
//! by the host; what it may do for a device is a set of capability names; what a user
//! configured is a map of setting id to value, validated against the [`Kind`] the
//! plugin declared in its manifest.
use peersus_plugin_api::{
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

    fn setting(id: &str, scope: Scope, kind: Kind) -> Setting {
        Setting {
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
}
