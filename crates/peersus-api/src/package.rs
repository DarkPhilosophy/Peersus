//! What a plugin says about itself as something that is installed and updated: a name, a
//! version, a description and what changed.
//!
//! This is separate from [`crate::manifest`] on purpose. The manifest is what a plugin may DO on
//! the hub, and it is validated fail-closed by the application. This is what a person reads
//! when deciding whether to install or update, and nothing in it ever reaches the hub or
//! confers a right. It lives in the same file as the manifest, under the key `package`, so that
//! one file says everything about a plugin and cannot fall out of step with another; the key is
//! taken out before the rest is read as a manifest.
//!
//! Everything here is TEXT TO SHOW. It is bounded and free of control characters so that it
//! cannot be used to disturb a terminal or a screen, and it is never interpreted: a changelog is
//! displayed, not followed.
//!
//! A plugin's id is not here and never will be: the host assigns it, and a plugin has no say in
//! who it is.
use crate::name;
use serde::{Deserialize, Serialize};

/// Longest display name, in characters.
pub const MAX_NAME_CHARS: usize = 80;
/// Longest description, in characters.
pub const MAX_DESCRIPTION_CHARS: usize = 1_000;
/// Longest changelog, in characters. Long enough for a real list of changes, short enough to
/// be read before approving.
pub const MAX_CHANGELOG_CHARS: usize = 4_000;
/// Longest version text, in bytes. `999999.999999.999999` is 20.
pub const MAX_VERSION_BYTES: usize = 32;

/// A version: `major.minor.patch`, three plain decimal numbers.
///
/// There is no pre-release or build suffix, and no leading zeros: `1.2.3` and `01.2.3` would
/// otherwise be the same version written two ways, and an update check that disagrees with
/// itself about whether something is newer is worse than none.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version(pub u64, pub u64, pub u64);

impl Version {
    /// Parse `major.minor.patch`, or `None` for anything else.
    pub fn parse(text: &str) -> Option<Self> {
        if text.is_empty() || text.len() > MAX_VERSION_BYTES {
            return None;
        }
        let mut parts = text.split('.');
        let mut number = || -> Option<u64> {
            let part = parts.next()?;
            let plain = !part.is_empty()
                && part.bytes().all(|b| b.is_ascii_digit())
                && (part == "0" || !part.starts_with('0'));
            if plain { part.parse().ok() } else { None }
        };
        let version = Self(number()?, number()?, number()?);
        parts.next().is_none().then_some(version)
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// What a plugin says about itself for people deciding whether to install or update it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Package {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub changelog: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub author: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub link: String,
}

impl Package {
    /// Whether every part is acceptable, and the version is a version.
    pub fn validate(&self) -> bool {
        name::text(&self.name, MAX_NAME_CHARS)
            && Version::parse(&self.version).is_some()
            && Self::optional_text(&self.description, MAX_DESCRIPTION_CHARS)
            && Self::optional_text(&self.changelog, MAX_CHANGELOG_CHARS)
            && Self::optional_text(&self.author, MAX_NAME_CHARS)
            && (self.link.is_empty() || public_link(&self.link))
    }

    /// The version, if it is one. A validated package always has one.
    pub fn version(&self) -> Option<Version> {
        Version::parse(&self.version)
    }

    /// The description in `catalog`'s language where it translates it, looked up by its English text: what a person reads on the
    /// extension's card, and the one word of this block an extension's language file translates. The name, version, changelog,
    /// author and link stay as written; a translation that would not be a valid description is not used.
    pub fn translate(&mut self, catalog: &crate::language::Catalog) {
        if self.description.is_empty() {
            return;
        }
        if let Some(translated) = catalog.get(None, &self.description)
            && Self::optional_text(translated, MAX_DESCRIPTION_CHARS)
        {
            self.description = translated.to_owned();
        }
    }

    /// Optional text may be empty, but if it is there it is held to the rule for text.
    fn optional_text(value: &str, max_chars: usize) -> bool {
        value.is_empty() || name::text(value, max_chars)
    }
}

/// Links are display metadata, never a fetch instruction or an authority. No credentials,
/// control characters, executable schemes, query or fragment are accepted.
pub fn public_link(value: &str) -> bool {
    if value.len() > 2_048 || value.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    let Ok(link) = url::Url::parse(value) else {
        return false;
    };
    link.scheme() == "https"
        && link.host_str().is_some()
        && link.username().is_empty()
        && link.password().is_none()
        && link.port().is_none()
        && link.query().is_none()
        && link.fragment().is_none()
}

/// The result of reading a manifest file that may carry package metadata.
#[derive(Debug, PartialEq, Eq)]
pub struct Split {
    /// The file with the `package` key removed, ready to be read as a manifest.
    pub manifest: Vec<u8>,
    /// The metadata, if the file had any.
    pub package: Option<Package>,
}

/// Why a file could not be split.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitError {
    /// The file is not a JSON object.
    NotAnObject,
    /// The `package` key is there but is not valid metadata.
    InvalidPackage,
    /// The author did not explicitly select an executor.
    MissingExecution,
}

/// Take the `package` key out of a manifest file's JSON, leaving the rest as it was.
///
/// The manifest is read with unknown keys refused, which is what keeps a plugin from
/// smuggling anything into what the hub trusts. So this key cannot be left in place: it is
/// removed first, validated on its own, and the rest is handed on untouched.
/// Every package must explicitly declare its execution kind.
pub fn split(text: &[u8]) -> Result<Split, SplitError> {
    let mut value: serde_json::Value =
        serde_json::from_slice(text).map_err(|_| SplitError::NotAnObject)?;
    let object = value.as_object_mut().ok_or(SplitError::NotAnObject)?;
    if !object.contains_key("execution") {
        return Err(SplitError::MissingExecution);
    }
    let package = match object.remove("package") {
        None => None,
        Some(found) => {
            let package: Package =
                serde_json::from_value(found).map_err(|_| SplitError::InvalidPackage)?;
            if !package.validate() {
                return Err(SplitError::InvalidPackage);
            }
            Some(package)
        }
    };
    let manifest = serde_json::to_vec(&value).map_err(|_| SplitError::NotAnObject)?;
    Ok(Split { manifest, package })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(version: &str) -> Package {
        Package {
            name: "Thing".into(),
            version: version.into(),
            description: String::new(),
            changelog: String::new(),
            author: String::new(),
            link: String::new(),
        }
    }

    #[test]
    fn only_the_description_is_translated_and_only_into_a_valid_one() {
        let catalog = |description: &str| {
            crate::language::Catalog::parse(&format!(
                "msgid \"\"\nmsgstr \"Language: ro\\n\"\n\nmsgid \"Shares text\"\nmsgstr \"{description}\"\n\
                 \nmsgid \"Thing\"\nmsgstr \"Lucru\"\n\nmsgid \"Fixed it\"\nmsgstr \"Reparat\"\n"
            ))
            .unwrap()
        };
        let mut described = package("1.0.0");
        described.description = "Shares text".into();
        described.changelog = "Fixed it".into();
        let mut translated = described.clone();
        translated.translate(&catalog("Partajează text"));
        assert_eq!(translated.description, "Partajează text");
        // The name and the changelog stay as written, though the file translates them.
        assert_eq!(
            (translated.name.as_str(), translated.changelog.as_str()),
            ("Thing", "Fixed it")
        );
        // A translation that would not be a valid description leaves the English.
        let mut refused = described.clone();
        refused.translate(&catalog(&"x".repeat(MAX_DESCRIPTION_CHARS + 1)));
        assert_eq!(refused, described);
        // Nothing written is nothing to look up.
        let mut empty = package("1.0.0");
        empty.translate(&catalog("Partajează text"));
        assert_eq!(empty, package("1.0.0"));
    }

    #[test]
    fn a_version_is_three_plain_numbers() {
        assert_eq!(Version::parse("1.2.3"), Some(Version(1, 2, 3)));
        assert_eq!(Version::parse("0.0.0"), Some(Version(0, 0, 0)));
        assert_eq!(Version::parse("10.20.30"), Some(Version(10, 20, 30)));
        for bad in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "1.2.",
            ".1.2",
            "1..3",
            "a.b.c",
            "1.2.3-beta",
            "1.2.3+build",
            "v1.2.3",
            " 1.2.3",
            "1.2.3 ",
            "-1.2.3",
            "+1.2.3",
            "1.2.-3",
            "0x1.2.3",
            "１.２.３",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad:?} was accepted");
        }
    }

    #[test]
    fn a_leading_zero_is_not_a_second_way_to_write_a_version() {
        // Otherwise 1.2.3 and 01.2.3 would be one version with two spellings.
        for bad in ["01.2.3", "1.02.3", "1.2.03", "00.0.0", "1.2.00"] {
            assert_eq!(Version::parse(bad), None, "{bad:?} was accepted");
        }
        assert_eq!(Version::parse("0.10.0"), Some(Version(0, 10, 0)));
    }

    #[test]
    fn a_version_too_large_for_a_number_or_too_long_is_refused() {
        assert_eq!(Version::parse("18446744073709551616.0.0"), None);
        assert_eq!(
            Version::parse("18446744073709551615.0.0"),
            Some(Version(u64::MAX, 0, 0))
        );
        assert_eq!(Version::parse(&format!("1.2.{}", "9".repeat(40))), None);
    }

    #[test]
    fn versions_are_ordered_by_number_not_by_text() {
        let older = Version::parse("1.9.0").unwrap();
        let newer = Version::parse("1.10.0").unwrap();
        // As text "1.10.0" sorts before "1.9.0"; as a version it is later.
        assert!("1.10.0" < "1.9.0");
        assert!(newer > older);
        assert!(Version(2, 0, 0) > Version(1, 99, 99));
        assert!(Version(1, 2, 4) > Version(1, 2, 3));
        assert_eq!(Version(1, 2, 3).to_string(), "1.2.3");
    }

    #[test]
    fn a_package_needs_a_name_and_a_version_and_nothing_else() {
        assert!(package("1.0.0").validate());
        let mut full = package("1.0.0");
        full.description = "Copies text between devices.".into();
        full.changelog = "Fixed a crash when the clipboard is empty.".into();
        assert!(full.validate());
    }

    #[test]
    fn text_that_cannot_be_shown_safely_is_refused() {
        /// A name for what is wrong and the change that makes a package wrong that way.
        type Fault = (&'static str, fn(&mut Package));
        let cases: [Fault; 8] = [
            ("blank name", |p| p.name = "   ".into()),
            ("empty name", |p| p.name = String::new()),
            ("control in name", |p| p.name = "a\u{7}b".into()),
            ("escape in changelog", |p| {
                p.changelog = "\u{1b}[2J cleared".into()
            }),
            ("newline in name", |p| p.name = "a\nb".into()),
            ("long name", |p| p.name = "x".repeat(MAX_NAME_CHARS + 1)),
            ("long description", |p| {
                p.description = "x".repeat(MAX_DESCRIPTION_CHARS + 1);
            }),
            ("long changelog", |p| {
                p.changelog = "x".repeat(MAX_CHANGELOG_CHARS + 1)
            }),
        ];
        for (what, change) in cases {
            let mut candidate = package("1.0.0");
            change(&mut candidate);
            assert!(!candidate.validate(), "{what} was accepted");
        }
    }

    #[test]
    fn text_exactly_at_each_limit_is_accepted() {
        let mut candidate = package("1.0.0");
        candidate.name = "é".repeat(MAX_NAME_CHARS);
        candidate.description = "é".repeat(MAX_DESCRIPTION_CHARS);
        candidate.changelog = "é".repeat(MAX_CHANGELOG_CHARS);
        assert!(candidate.validate());
    }

    #[test]
    fn a_changelog_may_be_empty_but_not_blank() {
        let mut candidate = package("1.0.0");
        candidate.changelog = String::new();
        assert!(candidate.validate(), "an absent changelog is fine");
        candidate.changelog = "   ".into();
        assert!(
            !candidate.validate(),
            "a blank one says nothing and is refused"
        );
    }

    #[test]
    fn splitting_takes_the_package_out_and_leaves_the_rest_untouched() {
        let text = br#"{
            "manifest_version": 1, "execution": {"kind": "process"},
            "capabilities": {"c": "Lets the thing do what it does."},
            "package": {"name": "Thing", "version": "1.2.3", "changelog": "fixes"},
            "imports": [], "exports": []
        }"#;
        let split = split(text).unwrap();
        let package = split.package.unwrap();
        assert_eq!(package.name, "Thing");
        assert_eq!(package.version, "1.2.3");
        assert_eq!(package.changelog, "fixes");
        // What is left reads as a manifest, which refuses any key it does not know.
        let manifest: crate::manifest::Manifest = serde_json::from_slice(&split.manifest).unwrap();
        assert_eq!(manifest.capabilities.len(), 1);
        let rest: serde_json::Value = serde_json::from_slice(&split.manifest).unwrap();
        assert!(rest.get("package").is_none(), "the key was left in");
    }

    #[test]
    fn a_file_with_no_package_key_is_still_a_manifest() {
        let text = br#"{"manifest_version": 1, "execution": {"kind": "process"}, "capabilities": {}, "imports": [], "exports": []}"#;
        let split = split(text).unwrap();
        assert_eq!(split.package, None);
        assert!(serde_json::from_slice::<crate::manifest::Manifest>(&split.manifest).is_ok());
    }

    #[test]
    fn a_package_that_is_not_valid_refuses_the_whole_file() {
        for (what, package) in [
            ("no version", r#"{"name": "T"}"#),
            ("no name", r#"{"version": "1.0.0"}"#),
            ("bad version", r#"{"name": "T", "version": "1.0"}"#),
            (
                "unknown key",
                r#"{"name": "T", "version": "1.0.0", "id": "other"}"#,
            ),
            ("not an object", r#""just text""#),
            (
                "control character",
                "{\"name\": \"T\\u0007\", \"version\": \"1.0.0\"}",
            ),
        ] {
            let text = format!(
                r#"{{"manifest_version":1,"execution":{{"kind":"process"}},"capabilities":{{}},"imports":[],"exports":[],"package":{package}}}"#
            );
            assert_eq!(
                split(text.as_bytes()),
                Err(SplitError::InvalidPackage),
                "{what} was accepted"
            );
        }
    }

    #[test]
    fn a_package_cannot_say_who_it_is() {
        // The host assigns the id; a plugin has no say in it. The unknown-key refusal is what
        // enforces that, so `id` is tested by name.
        let text = br#"{"manifest_version":1,"execution":{"kind":"process"},"capabilities":{},"imports":[],"exports":[],
            "package":{"id":"someone-else","name":"T","version":"1.0.0"}}"#;
        assert_eq!(split(text), Err(SplitError::InvalidPackage));
    }

    #[test]
    fn something_that_is_not_an_object_is_refused() {
        for text in [&b"[]"[..], b"\"text\"", b"42", b"null", b"", b"{"] {
            assert_eq!(split(text), Err(SplitError::NotAnObject), "{text:?}");
        }
    }

    #[test]
    fn the_package_key_is_the_only_thing_removed() {
        // A manifest key that is not `package` must survive, so a misspelt key is still
        // caught by the manifest's own refusal of unknown keys rather than silently dropped.
        let text = br#"{"manifest_version":1,"execution":{"kind":"process"},"capabilities":{},"imports":[],"exports":[],
            "packages":{"name":"T","version":"1.0.0"}}"#;
        let split = split(text).unwrap();
        assert_eq!(split.package, None);
        assert!(
            serde_json::from_slice::<crate::manifest::Manifest>(&split.manifest).is_err(),
            "an unknown key was dropped instead of refused"
        );
    }
}
