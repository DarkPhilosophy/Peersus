//! Chat wire v1. Fixed header envelope; only attachment chunks and clear IDs use raw bytes.
use crate::sdk::{
    extension_data::{ScopeId, WallTime},
    name,
};
use crate::serde::{self, Deserialize, Serialize};

pub(crate) const SERVICE: &str = "chat.events";
pub(crate) const GUARD: &str = "chat.use";
pub(crate) const VERSION: u32 = 1;
pub(crate) const HEADER: usize = 8192;
pub(crate) const CHUNK: usize = 32 * 1024;
pub(crate) const FRAME: usize = 8 + HEADER + CHUNK;
pub(crate) const MAX_ATTACHMENT: u32 = 8 * 1024 * 1024;
pub(crate) const MAX_ROOM_ROWS: usize = 512;
pub(crate) const MAX_ORDER: u64 = (1 << 53) - 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fault {
    Invalid,
    Quota,
    Rate,
    Missing,
    Conflict,
    Storage,
    NotDelivered,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(crate = "serde", deny_unknown_fields)]
pub(crate) struct Id {
    pub epoch: ScopeId,
    pub no: u64,
}
impl Id {
    pub fn valid(&self) -> bool {
        self.no != 0
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "serde", rename_all = "kebab-case")]
pub(crate) enum Scope {
    Direct,
    Group(ScopeId),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "serde", rename_all = "kebab-case")]
pub(crate) enum Actor {
    Sender,
    Receiver,
    Member(ScopeId),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "serde", deny_unknown_fields)]
pub(crate) struct Target {
    pub actor: Actor,
    pub id: Id,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "serde", deny_unknown_fields)]
pub(crate) struct Member {
    pub id: ScopeId,
    pub label: String,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "serde", deny_unknown_fields)]
pub(crate) struct Group {
    pub id: ScopeId,
    pub title: String,
    pub creator: ScopeId,
    pub roster: Vec<Member>,
}
impl Group {
    pub fn valid(&self) -> bool {
        (3..=8).contains(&self.roster.len())
            && label(&self.title)
            && self.roster.iter().all(|m| label(&m.label))
            && self.roster.windows(2).all(|pair| pair[0].id < pair[1].id)
            && self.roster.iter().any(|m| m.id == self.creator)
    }
    pub fn has(&self, id: &ScopeId) -> bool {
        self.roster.iter().any(|m| &m.id == id)
    }
}
fn label(value: &str) -> bool {
    name::text(value, 80) && value.len() <= 320
}
/// Bars of a voice message's waveform, as its writer measured it.
pub(crate) const WAVE_BARS: usize = 40;
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "serde", deny_unknown_fields)]
pub(crate) struct Attachment {
    pub name: String,
    pub media_type: String,
    pub bytes: u32,
    pub sha256: ScopeId,
    /// How loud a recorded voice message was, [`WAVE_BARS`] steps from start to end, each a hexadecimal digit (0 silence,
    /// f loudest), as the device that recorded it heard it. Only what its bubble is drawn with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wave: Option<String>,
}
impl Attachment {
    fn valid(&self) -> bool {
        !self.name.trim().is_empty()
            && self.name.len() <= 120
            && !matches!(self.name.as_str(), "." | "..")
            && !self
                .name
                .chars()
                .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
            && self.media_type.len() <= 63
            && self.media_type.split_once('/').is_some_and(|(a, b)| {
                let valid = |s: &str| {
                    !s.is_empty()
                        && s.bytes().all(|c| {
                            c.is_ascii_lowercase() || c.is_ascii_digit() || b"+.-".contains(&c)
                        })
                };
                valid(a) && valid(b)
            })
            && (1..=MAX_ATTACHMENT).contains(&self.bytes)
            && self.wave.as_ref().is_none_or(|wave| {
                wave.len() == WAVE_BARS
                    && wave.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
            })
    }
}

/// A recording's levels (0 to 255, one a step) as [`WAVE_BARS`] hexadecimal digits: the loudest of the steps each bar covers.
pub(crate) fn wave(levels: &[u8]) -> Option<String> {
    if levels.is_empty() {
        return None;
    }
    Some(
        (0..WAVE_BARS)
            .map(|bar| {
                let from = bar * levels.len() / WAVE_BARS;
                let to = ((bar + 1) * levels.len() / WAVE_BARS).max(from + 1);
                let loudest = levels[from.min(levels.len() - 1)..to.min(levels.len())]
                    .iter()
                    .copied()
                    .max()
                    .unwrap_or(0);
                char::from_digit(u32::from(loudest) * 15 / 255, 16).unwrap_or('0')
            })
            .collect(),
    )
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "serde", deny_unknown_fields)]
pub(crate) struct Body {
    pub kind: String,
    pub text: Option<String>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    /// The message this one answers, when it answers one: named by its writer and id, the way a reaction names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply: Option<Target>,
    /// The minute its writer sent it, by the writer's clock (whole minutes since 1970 UTC), when that device could read
    /// the date and time. The other device's word only: the receiver keeps it only when it is near its own clock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent: Option<i64>,
}
impl Body {
    pub fn text(text: String) -> Self {
        Self {
            kind: "text".into(),
            text: Some(text),
            attachments: Vec::new(),
            reply: None,
            sent: None,
        }
    }
    pub fn valid(&self) -> bool {
        let text_ok = |value: &str| value.len() <= 4000 && name::text(value, 1000);
        if self.text.as_deref().is_some_and(|text| !text_ok(text))
            || self.reply.as_ref().is_some_and(|reply| !reply.id.valid())
            || self.sent.is_some_and(|minute| {
                !WallTime {
                    unix_minute: minute,
                    utc_offset_minutes: 0,
                }
                .valid()
            })
        {
            return false;
        }
        match (self.kind.as_str(), self.attachments.as_slice()) {
            ("text", []) => self.text.is_some(),
            ("photo", [a]) => a.valid() && a.media_type.starts_with("image/"),
            ("audio", [a]) => a.valid() && a.media_type.starts_with("audio/"),
            ("file", [a]) => a.valid(),
            _ => false,
        }
    }
}
pub(crate) const EMOJI: &[&str] = &["👍", "❤️", "😂", "😮", "😢", "🙏", "🎉", "👎"];
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(crate = "serde", deny_unknown_fields)]
pub(crate) struct Key {
    pub epoch: ScopeId,
    pub seq: u64,
    pub operation: u64,
}
impl Key {
    pub fn valid(&self) -> bool {
        self.seq != 0 && self.operation != 0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    crate = "serde",
    tag = "state",
    content = "offset",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub(crate) enum Verdict {
    Applied,
    Absent,
    Declined,
    Invalid,
    Quota,
    NeedGroup,
    NeedTarget,
    Stale,
    Offset(u32),
    Unknown,
}
impl Verdict {
    pub fn terminal(self) -> bool {
        !matches!(self, Self::Offset(_) | Self::Unknown)
    }
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    crate = "serde",
    tag = "op",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub(crate) enum Op {
    Define {
        group: Group,
    },
    Message {
        id: Id,
        order: u64,
        body: Body,
    },
    Begin {
        id: Id,
        order: u64,
        body: Body,
    },
    Chunk {
        offset: u32,
        len: u32,
    },
    Finish,
    React {
        target: Target,
        revision: u64,
        emoji: Option<String>,
    },
    Seen {
        target: Target,
    },
    Delete {
        id: Id,
    },
    Clear {
        count: u16,
    },
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    crate = "serde",
    tag = "packet",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub(crate) enum Packet {
    Mutation { key: Key, scope: Scope, change: Op },
    Result { key: Key, verdict: Verdict },
    Status { key: Key },
}
impl Packet {
    pub fn key(&self) -> &Key {
        match self {
            Self::Mutation { key, .. } | Self::Result { key, .. } | Self::Status { key } => key,
        }
    }
    pub fn valid(&self, payload: &[u8]) -> bool {
        if !self.key().valid() || payload.len() > CHUNK {
            return false;
        }
        let Self::Mutation { scope, change, .. } = self else {
            return payload.is_empty();
        };
        match change {
            Op::Define { group } => {
                payload.is_empty() && group.valid() && *scope == Scope::Group(group.id.clone())
            }
            Op::Message { id, order, body } | Op::Begin { id, order, body } => {
                payload.is_empty()
                    && id.valid()
                    && (1..=MAX_ORDER).contains(order)
                    && body.valid()
                    && (matches!(change, Op::Message { .. }) == body.attachments.is_empty())
            }
            Op::Chunk { offset, len } => {
                *len != 0
                    && *len as usize == payload.len()
                    && offset
                        .checked_add(*len)
                        .is_some_and(|end| end <= MAX_ATTACHMENT)
            }
            Op::React {
                target,
                revision,
                emoji,
            } => {
                payload.is_empty()
                    && target.id.valid()
                    && *revision != 0
                    && target_fits(scope, target)
                    && emoji.as_deref().is_none_or(|e| EMOJI.contains(&e))
            }
            Op::Seen { target } => {
                payload.is_empty() && target.id.valid() && target_fits(scope, target)
            }
            Op::Delete { id } => payload.is_empty() && id.valid(),
            Op::Clear { count } => {
                usize::from(*count) <= MAX_ROOM_ROWS && payload.len() == usize::from(*count) * 41
            }
            Op::Finish => payload.is_empty(),
        }
    }
}
fn target_fits(scope: &Scope, target: &Target) -> bool {
    matches!(
        (scope, &target.actor),
        (Scope::Direct, Actor::Sender | Actor::Receiver) | (Scope::Group(_), Actor::Member(_))
    )
}
pub(crate) fn encode(packet: &Packet, payload: &[u8]) -> Result<Vec<u8>, Fault> {
    if !packet.valid(payload) {
        return Err(Fault::Invalid);
    }
    let head = serde_json::to_vec(packet).map_err(|_| Fault::Invalid)?;
    if head.is_empty() || head.len() > HEADER {
        return Err(Fault::Invalid);
    }
    let mut bytes = Vec::with_capacity(8 + head.len() + payload.len());
    bytes.extend(b"PCH1");
    bytes.extend((head.len() as u32).to_be_bytes());
    bytes.extend(head);
    bytes.extend(payload);
    Ok(bytes)
}
pub(crate) fn decode(bytes: &[u8]) -> Result<(Packet, &[u8]), Fault> {
    if bytes.len() > FRAME || !bytes.starts_with(b"PCH1") {
        return Err(Fault::Invalid);
    }
    let n = u32::from_be_bytes(
        bytes
            .get(4..8)
            .and_then(|b| b.try_into().ok())
            .ok_or(Fault::Invalid)?,
    ) as usize;
    if !(1..=HEADER).contains(&n) {
        return Err(Fault::Invalid);
    }
    let head = bytes.get(8..8 + n).ok_or(Fault::Invalid)?;
    let tail = bytes.get(8 + n..).ok_or(Fault::Invalid)?;
    let packet: Packet = serde_json::from_slice(head).map_err(|_| Fault::Invalid)?;
    if !packet.valid(tail) {
        return Err(Fault::Invalid);
    }
    Ok((packet, tail))
}

pub(crate) fn clear_ids(targets: &[Target], group: Option<&Group>) -> Result<Vec<u8>, Fault> {
    let mut ids = Vec::with_capacity(targets.len());
    for target in targets {
        let slot = match (&target.actor, group) {
            (Actor::Sender, None) => 0,
            (Actor::Receiver, None) => 1,
            (Actor::Member(id), Some(group)) => group
                .roster
                .iter()
                .position(|m| &m.id == id)
                .ok_or(Fault::Invalid)? as u8,
            _ => return Err(Fault::Invalid),
        };
        let mut bytes = [0; 41];
        bytes[0] = slot;
        for (byte, pair) in bytes[1..33]
            .iter_mut()
            .zip(target.id.epoch.as_str().as_bytes().chunks_exact(2))
        {
            fn digit(c: u8) -> u8 {
                if c <= b'9' { c - b'0' } else { c - b'a' + 10 }
            }
            *byte = digit(pair[0]) * 16 + digit(pair[1]);
        }
        bytes[33..].copy_from_slice(&target.id.no.to_be_bytes());
        ids.push(bytes);
    }
    ids.sort_unstable();
    ids.dedup();
    Ok(ids.into_iter().flatten().collect())
}
pub(crate) fn read_clear(bytes: &[u8], group: Option<&Group>) -> Result<Vec<Target>, Fault> {
    let mut ids = Vec::with_capacity(bytes.len() / 41);
    let mut prev: Option<&[u8]> = None;
    for row in bytes.chunks_exact(41) {
        if prev.is_some_and(|p| p >= row) {
            return Err(Fault::Invalid);
        }
        prev = Some(row);
        let actor = match group {
            Some(g) => Actor::Member(
                g.roster
                    .get(usize::from(row[0]))
                    .ok_or(Fault::Invalid)?
                    .id
                    .clone(),
            ),
            None => match row[0] {
                0 => Actor::Sender,
                1 => Actor::Receiver,
                _ => return Err(Fault::Invalid),
            },
        };
        let mut text = String::with_capacity(64);
        for byte in &row[1..33] {
            use std::fmt::Write;
            write!(text, "{byte:02x}").map_err(|_| Fault::Invalid)?;
        }
        let id = Id {
            epoch: ScopeId::try_from(text).map_err(|_| Fault::Invalid)?,
            no: u64::from_be_bytes(row[33..].try_into().map_err(|_| Fault::Invalid)?),
        };
        if !id.valid() {
            return Err(Fault::Invalid);
        }
        ids.push(Target { actor, id });
    }
    if !bytes.len().is_multiple_of(41) {
        return Err(Fault::Invalid);
    }
    Ok(ids)
}
