//! A permission: a name together with WHOSE permission it is.
//!
//! A name alone is not an identity. Two participants can each declare a permission called
//! `peers.send` and mean entirely different things by it; if what a user granted were only the
//! name, a grant of one would satisfy the other. So what is granted is always the pair of the
//! participant that REQUIRES the permission, because it is the provider of the guarded method,
//! and the name that provider gave it.
//!
//! The owner is never something a plugin writes. It is the id the host assigned to whoever
//! exports the method (or the reserved `host` for the application itself), so there is no text
//! a plugin can put in a manifest to look like someone else's permission. A plugin cannot own
//! `host`: the hub refuses to register one under that id.
use crate::name;
use serde::{Deserialize, Serialize};

/// Whose permission it is, and what it is called.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Permission {
    /// The participant that requires it: the provider of the method it guards.
    pub owner: String,
    /// What that participant calls it.
    pub name: String,
}

impl Permission {
    /// A permission of `owner`. `None` unless both parts are valid identifiers.
    pub fn new(owner: impl Into<String>, name: impl Into<String>) -> Option<Self> {
        let permission = Self {
            owner: owner.into(),
            name: name.into(),
        };
        permission.is_valid().then_some(permission)
    }

    /// Whether both parts are identifiers, which is checked again wherever one is read back,
    /// because a stored or received permission is not trusted for having been written once.
    pub fn is_valid(&self) -> bool {
        name::valid(&self.owner) && name::valid(&self.name)
    }
}

impl std::fmt::Display for Permission {
    /// For a person: who it belongs to, then its name. Never for parsing back.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} \u{2192} {}", self.owner, self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_name_under_two_owners_is_two_permissions() {
        let host = Permission::new("host", "peers.send").unwrap();
        let plugin = Permission::new("evil", "peers.send").unwrap();
        assert_ne!(host, plugin);
        // And they do not collapse into one when kept in a set, which is where a grant lives.
        let set: std::collections::BTreeSet<_> = [host.clone(), plugin].into();
        assert_eq!(set.len(), 2);
        assert!(set.contains(&host));
    }

    #[test]
    fn both_parts_must_be_identifiers() {
        for (owner, name) in [
            ("", "x"),
            ("x", ""),
            ("a b", "x"),
            ("x", "a b"),
            ("a/b", "x"),
            ("x", "a\n"),
            ("é", "x"),
        ] {
            assert!(
                Permission::new(owner, name).is_none(),
                "{owner:?} {name:?} was accepted"
            );
        }
        assert!(Permission::new("host", "peers.send").is_some());
    }

    #[test]
    fn a_stored_permission_is_checked_again_when_it_is_read_back() {
        // A value read from disk or the wire is built without going through `new`.
        let bad = Permission {
            owner: "host".into(),
            name: "has space".into(),
        };
        assert!(!bad.is_valid());
        let json = serde_json::to_string(&Permission::new("host", "peers.send").unwrap()).unwrap();
        assert_eq!(json, r#"{"owner":"host","name":"peers.send"}"#);
        let back: Permission = serde_json::from_str(&json).unwrap();
        assert!(back.is_valid());
    }

    #[test]
    fn an_unknown_field_is_refused_so_nothing_rides_along() {
        let with_extra = r#"{"owner":"host","name":"peers.send","granted":true}"#;
        assert!(serde_json::from_str::<Permission>(with_extra).is_err());
    }

    #[test]
    fn the_text_shown_to_a_person_names_the_owner_first() {
        let shown = Permission::new("flycar", "notify").unwrap().to_string();
        assert!(shown.starts_with("flycar"), "{shown}");
        assert!(shown.ends_with("notify"), "{shown}");
        // Two owners never look the same on screen.
        assert_ne!(
            shown,
            Permission::new("host", "notify").unwrap().to_string()
        );
    }
}
