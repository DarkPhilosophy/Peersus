//! Caller-scoped identifiers, clocks and private byte objects. No paths, descriptors or OS state.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fmt};

pub const PEERS: &str = "peersus.peers";
pub const REFERENCES: &str = "references";
pub const NEW_SCOPE: &str = "new-scope";
pub const CLOCK: &str = "peersus.clock";
pub const NOW: &str = "now";
/// The date and time on this device, to the minute: `{}` answers [`WallTime`]. Unlike [`NOW`] it tells something of the
/// device (its clock and time zone), so it is guarded by [`WALL_CAPABILITY`], which the person grants.
pub const WALL: &str = "wall";
pub const WALL_CAPABILITY: &str = "clock.wall";
/// The most a time zone is away from UTC, in minutes (UTC+14 and UTC-12 are the extremes in use; this leaves room).
pub const MAX_UTC_OFFSET_MINUTES: i32 = 18 * 60;
pub const STORAGE: &str = "peersus.storage";
pub const FILES: &str = "peersus.files";
pub const STAGE_SELECTED: &str = "stage-selected";
pub const SAVE: &str = "save";
pub const EXPORT: &str = "files.export";

pub const MAX_REFERENCES: usize = 64;
pub const MAX_KEYS: usize = 8192;
pub const MAX_BYTES: u64 = 96 * 1024 * 1024;
pub const MAX_OBJECT: u64 = 8 * 1024 * 1024;
pub const MAX_STAGED: u64 = 24 * 1024 * 1024;
pub const MAX_CHUNK: u32 = 64 * 1024;
pub const MAX_BATCH: usize = 64;
pub const MAX_REMOVE_KEYS: usize = 2048;
pub const MAX_REMOVE_BYTES: u64 = 512 * 1024;
pub const MAX_INLINE: usize = 8 * 1024;
pub const MAX_LIST: usize = 64;

/// The answer to [`WALL`]: the minute it is (whole minutes since 1970-01-01 UTC) and how far this device's time zone is
/// from UTC then, in minutes east. Local time is `unix_minute + utc_offset_minutes`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WallTime {
    pub unix_minute: i64,
    pub utc_offset_minutes: i32,
}
impl WallTime {
    /// Whether the answer is one the application could give: a minute after 1970 and before the year 10000, and an offset
    /// a time zone can have. An extension checks it before trusting it.
    pub fn valid(&self) -> bool {
        (0..=253_402_300_799 / 60).contains(&self.unix_minute)
            && (-MAX_UTC_OFFSET_MINUTES..=MAX_UTC_OFFSET_MINUTES).contains(&self.utc_offset_minutes)
    }
    /// The local minute of the day (0..1440) and the local day (days since 1970-01-01), from this answer.
    pub fn local_day_and_minute(&self) -> (i64, u16) {
        let local = self.unix_minute + i64::from(self.utc_offset_minutes);
        let day = local.div_euclid(24 * 60);
        (day, local.rem_euclid(24 * 60) as u16)
    }
}

/// Canonical 256-bit opaque reference, represented as lowercase hexadecimal.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct ScopeId(String);
impl TryFrom<String> for ScopeId {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        (value.len() == 64
            && value
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)))
        .then_some(Self(value))
        .ok_or("Invalid opaque reference")
    }
}
impl ScopeId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for ScopeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ScopeId(..)")
    }
}

/// An ASCII virtual key. Its slash-delimited segments are logical, never filenames.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct Key(String);
impl TryFrom<String> for Key {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        (!value.is_empty()
            && value.len() <= 224
            && value.split('/').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
            }))
        .then_some(Self(value))
        .ok_or("Invalid object key")
    }
}
impl Key {
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn under(&self, prefix: &Self) -> bool {
        self == prefix
            || self
                .0
                .strip_prefix(&prefix.0)
                .is_some_and(|tail| tail.starts_with('/'))
    }
}
impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Key(..)")
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Empty {}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct References {
    pub devices: Vec<String>,
    pub scope: Option<ScopeId>,
    #[serde(default)]
    pub include_self: bool,
}
impl References {
    pub fn valid(&self) -> bool {
        self.devices.len() <= MAX_REFERENCES
            && (!self.include_self || self.scope.is_some())
            && self.devices.iter().all(|a| {
                a.len() == 32
                    && a.bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            })
            && self.devices.iter().collect::<BTreeSet<_>>().len() == self.devices.len()
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PeerReference {
    pub device: String,
    pub local_ref: ScopeId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scoped_ref: Option<ScopeId>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceSet {
    pub peers: Vec<PeerReference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local: Option<LocalReference>,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LocalReference {
    pub scoped_ref: ScopeId,
    pub name: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct List {
    pub prefix: Option<Key>,
    pub after: Option<Key>,
    pub limit: usize,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Listing {
    pub revision: u64,
    pub keys: Vec<Key>,
    pub more: bool,
}
#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Source {
    Key { key: Key, revision: u64 },
    Stage { stage: u64 },
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Read {
    pub source: Source,
    pub offset: u64,
    pub length: u32,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Stage {
    pub bytes: u64,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Staged {
    pub stage: u64,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Append {
    pub stage: u64,
    pub offset: u64,
    pub length: u32,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Sealed {
    pub stage: u64,
    pub bytes: u64,
    pub sha256: ScopeId,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DataClaim {
    pub claim: u64,
    pub bytes: u64,
}

/// A batch either publishes at the exact old revision or leaves every key unchanged.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Commit {
    pub if_revision: u64,
    #[serde(default)]
    pub put: Vec<Put>,
    #[serde(default)]
    pub remove: Vec<Key>,
    #[serde(default)]
    pub remove_prefix: Vec<Key>,
    pub receive_ticket: Option<u64>,
    /// A sealed UTF-8 JSON array of unique virtual keys to remove in this same batch.
    pub remove_from: Option<u64>,
}
#[derive(Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum Put {
    Text { key: Key, text: String },
    Object { key: Key, stage: u64 },
}
impl Put {
    pub fn key(&self) -> &Key {
        match self {
            Self::Text { key, .. } | Self::Object { key, .. } => key,
        }
    }
}
impl Commit {
    pub fn valid(&self) -> bool {
        let count = self.put.len() + self.remove.len() + self.remove_prefix.len();
        if (count == 0 && self.remove_from.is_none())
            || count > MAX_BATCH
            || self.receive_ticket == Some(0)
            || self.remove_from == Some(0)
        {
            return false;
        }
        let mut seen = BTreeSet::new();
        let mut stages = BTreeSet::new();
        self.put.iter().all(|p| {
            let data = match p {
                Put::Text { text, .. } => text.len() <= MAX_INLINE,
                Put::Object { stage, .. } => {
                    *stage != 0 && self.remove_from != Some(*stage) && stages.insert(*stage)
                }
            };
            data && seen.insert(p.key())
                && !self
                    .remove_prefix
                    .iter()
                    .any(|prefix| p.key().under(prefix))
        }) && self.remove.iter().all(|key| seen.insert(key))
            && self.remove_prefix.iter().all(|key| seen.insert(key))
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Selected {
    pub file: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Save {
    pub key: Key,
    pub revision: u64,
    pub suggested_name: String,
}
impl Save {
    pub fn valid(&self) -> bool {
        let name = &self.suggested_name;
        !name.trim().is_empty()
            && name.len() <= 120
            && !matches!(name.as_str(), "." | "..")
            && !name
                .chars()
                .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
    }
}

#[cfg(test)]
mod wall_time_tests {
    use super::*;

    #[test]
    fn a_wall_time_is_refused_outside_what_the_application_gives_and_read_in_local_days() {
        let parse = |text: &str| serde_json::from_str::<WallTime>(text);
        let late = parse(r#"{"unix_minute":29404713,"utc_offset_minutes":120}"#).unwrap();
        assert!(late.valid());
        // 22:33 UTC is 00:33 the next local day at UTC+2.
        let (day, minute) = late.local_day_and_minute();
        assert_eq!(minute, 33);
        assert_eq!(day, 29404713 / 1440 + 1);
        let west = WallTime {
            unix_minute: 0,
            utc_offset_minutes: -60,
        };
        assert_eq!(west.local_day_and_minute(), (-1, 23 * 60));
        assert!(
            !WallTime {
                unix_minute: -1,
                ..late
            }
            .valid()
        );
        assert!(
            !WallTime {
                utc_offset_minutes: 19 * 60,
                ..late
            }
            .valid()
        );
        assert!(
            !WallTime {
                unix_minute: i64::MAX,
                ..late
            }
            .valid()
        );
        assert!(parse(r#"{"unix_minute":1,"utc_offset_minutes":0,"zone":"x"}"#).is_err());
    }
}
