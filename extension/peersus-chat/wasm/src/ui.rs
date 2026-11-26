//! The guest's page state. The host paints common elements and supplies observations; it never
//! chooses a conversation, interprets a receipt, or decides what a deletion means.
//!
//! One page, one chat: a list of conversations and the open conversation, side by side on a wide
//! page and one at a time on a narrow one. Everything else (the message menu, details, deletion,
//! the conversation's options, a new chat or group, attaching a file) opens over the conversation,
//! which stays where it is behind it.
use crate::{
    core::{Delete, Delivery, Engine, Removal, RoomKey, Route, Row},
    sdk::{
        elements::{self, Colour, Event, Length, Node, Observe, Pick, Presence, Show, Style, Tone},
        extension_data::{ScopeId, Sealed, WallTime},
    },
    wire::{EMOJI, Fault, Id},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write,
};

pub const PAGE: &str = "conversations";
pub const PRIMARY: &str = "show-chat";
const TICK: &str = "refresh";
/// The zero-height box that reports the page's width.
const PROBE: &str = "width";
const FULL: Length = Length::Percent(10_000);
/// Below this page width (thousandths of a logical pixel) one pane is shown at a time.
const WIDE: u32 = 720_000;
/// Widest and tallest a photo is shown in a message, in logical pixels.
const SHOWN_WIDTH: u16 = 220;
const SHOWN_HEIGHT: u16 = 260;
/// Tallest a photo opened over the page is shown.
const VIEW_HEIGHT: u16 = 560;
/// What the photo sheet takes beside the photo, in logical pixels: its padding, gap and buttons, and a margin above it.
const PHOTO_CHROME: u32 = 128;
/// Messages shown at once, newest first, and the text they may carry together.
const ROWS: usize = 40;
const ROW_TEXT: usize = 18_000;
/// The fewest messages a conversation is cut down to when its page would not fit what the application draws.
const MIN_FIT: usize = 4;
const ROOMS: usize = 40;
/// A message this short sits in a bubble as wide as its text, on one line with its time; a longer one wraps. Narrower pages
/// hold fewer letters on that line ([`Page::short`]).
const SHORT: usize = 40;
/// A thread shows its newest replies, this many, until the person asks for all of them.
const THREAD: usize = 6;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Locale {
    #[default]
    En,
    Ro,
}
impl Locale {
    pub fn from_tag(tag: &str) -> Self {
        if tag.split('-').next() == Some("ro") {
            Self::Ro
        } else {
            Self::En
        }
    }
    fn t(self, en: &'static str, ro: &'static str) -> &'static str {
        match self {
            Self::En => en,
            Self::Ro => ro,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Media {
    Photo,
    Audio,
    File,
}
impl Media {
    pub fn kind(self) -> &'static str {
        match self {
            Self::Photo => "photo",
            Self::Audio => "audio",
            Self::File => "file",
        }
    }
    fn title(self, l: Locale) -> &'static str {
        match self {
            Self::Photo => l.t("Photo", "Fotografie"),
            Self::Audio => l.t("Audio", "Audio"),
            Self::File => l.t("File", "Fișier"),
        }
    }
    fn of(kind: &str) -> Self {
        match kind {
            "photo" => Self::Photo,
            "audio" => Self::Audio,
            _ => Self::File,
        }
    }
}

pub struct Peer {
    pub alias: String,
    pub name: String,
    pub route: Route,
    pub sleeping: bool,
    pub policy: crate::core::Policy,
    pub can_reply: bool,
}
#[derive(Clone, Copy, Default)]
pub struct Features {
    pub select: bool,
    pub export: bool,
    pub visibility: bool,
    /// The application tells a pressed box a tap and a long press, and lets a drag on it scroll the conversation.
    pub press: bool,
    /// The application sends its Back (Android's, or Escape) to a box that takes it.
    pub back: bool,
    /// The person let Chat show notifications.
    pub notify: bool,
    /// This process decodes sounds: an audio message plays in its bubble.
    pub listen: bool,
    /// The person let Chat record from the microphone: a voice message can be recorded in a conversation.
    pub record: bool,
}

/// The audio message this extension plays: which, whether it plays now, how far it is and how long it is.
pub struct Listening {
    /// The message's storage key.
    pub row: String,
    pub playing: bool,
    pub position_ms: u64,
    pub length_ms: Option<u64>,
    /// The decoder in this process.
    pub decoder: i64,
    /// Every sample of the sound has been handed to the application.
    pub decoded_all: bool,
    /// When the application was last asked how far it is.
    pub asked: u64,
}

/// A photo's small picture, made in this extension's own process from the photo it holds and kept by the application under
/// `id` for this page to place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thumb {
    pub id: String,
    pub width: u16,
    pub height: u16,
}

/// Longest side of a kept photo picture, in pixels: shown at most [`SHOWN_WIDTH`] wide, sharp on a screen of about twice the
/// density.
pub const THUMB_EDGE: u32 = 384;
/// Longest side of the picture of the one photo opened over the page.
pub const VIEW_EDGE: u32 = crate::sdk::pictures::MAX_EDGE;
/// Most bytes one kept picture of `edge` may take.
const fn picture_bytes(edge: u32) -> usize {
    edge as usize * edge as usize * 4
}
/// Most photo pictures kept at once: what the application keeps for an extension, less the opened photo and the chosen
/// one waiting to be sent, so a keep is never refused for room.
const THUMBS: usize =
    (crate::sdk::pictures::MAX_KEPT_BYTES - picture_bytes(VIEW_EDGE) - picture_bytes(THUMB_EDGE))
        / picture_bytes(THUMB_EDGE);

/// The photo pictures made so far, by the photo's storage key: made once each, the oldest let go of past [`THUMBS`].
#[derive(Default)]
pub struct Thumbs {
    made: BTreeMap<String, Option<Thumb>>,
    order: std::collections::VecDeque<String>,
    next: u64,
}
impl Thumbs {
    pub fn get(&self, asset: &str) -> Option<&Thumb> {
        self.made.get(asset).and_then(Option::as_ref)
    }
    /// The newest photo of `room` whose picture has not been tried yet, among the newest [`THUMBS`]: its storage key and size.
    pub fn wanted(&self, engine: &Engine, room: &RoomKey) -> Option<(String, u32)> {
        let mut photos: Vec<_> = engine
            .data
            .rows
            .values()
            .filter(|row| &row.room == room)
            .filter_map(|row| {
                let body = row.body.as_ref().filter(|body| body.kind == "photo")?;
                Some((
                    row.order,
                    row.asset.as_ref()?,
                    body.attachments.first()?.bytes,
                ))
            })
            .collect();
        photos.sort_by_key(|photo| std::cmp::Reverse(photo.0));
        photos
            .into_iter()
            .take(THUMBS)
            .find(|(_, asset, _)| !self.made.contains_key(*asset))
            .map(|(_, asset, bytes)| (asset.clone(), bytes))
    }
    /// A fresh id for the picture of a photo.
    pub fn id(&mut self) -> String {
        self.next += 1;
        format!("photo/{}", self.next)
    }
    /// Let go of the oldest pictures until one more fits: the ids to forget, oldest first. Called before a picture is kept, so
    /// what the application keeps never goes past [`THUMBS`].
    pub fn make_room(&mut self) -> Vec<String> {
        let mut gone = Vec::new();
        while self.order.len() >= THUMBS {
            if let Some(old) = self.order.pop_front()
                && let Some(Some(thumb)) = self.made.remove(&old)
            {
                gone.push(thumb.id);
            }
        }
        gone
    }
    /// What became of the photo `asset`: its picture, or none when it could not be made (it is not tried again).
    pub fn made(&mut self, asset: String, thumb: Option<Thumb>) {
        if thumb.is_some() {
            self.order.push_back(asset.clone());
        }
        self.made.insert(asset, thumb);
    }
}

/// The waveforms measured here from the sounds this device holds, by the sound's storage key: for an audio message that
/// carries none (another application's WAV, or a recording whose writer measured nothing). Each is tried once.
#[derive(Default)]
pub struct Waves {
    made: BTreeMap<String, Option<String>>,
}
/// No waveform measured: what a page drawn in a test has.
#[cfg(test)]
pub static NO_WAVES: Waves = Waves {
    made: BTreeMap::new(),
};
impl Waves {
    /// The largest sound measured: two minutes of a recording, with room to spare.
    const MAX_BYTES: u32 = 8 * 1024 * 1024;
    /// Most sounds remembered; past them all are forgotten and measured again when shown.
    const KEPT: usize = 256;

    pub fn get(&self, asset: &str) -> Option<&str> {
        self.made.get(asset).and_then(Option::as_deref)
    }
    /// The newest WAV of `room` this device holds whose message carries no waveform and that was not tried yet: its storage
    /// key and size.
    pub fn wanted(&self, engine: &Engine, room: &RoomKey) -> Option<(String, u32)> {
        engine
            .data
            .rows
            .values()
            .filter(|row| &row.room == room)
            .filter_map(|row| {
                let body = row.body.as_ref().filter(|body| body.kind == "audio")?;
                let file = body.attachments.first()?;
                let asset = row.asset.as_ref()?;
                (file.wave.is_none()
                    && file.media_type == "audio/wav"
                    && file.bytes <= Self::MAX_BYTES
                    && !self.made.contains_key(asset))
                .then_some((row.order, asset, file.bytes))
            })
            .max_by_key(|found| found.0)
            .map(|(_, asset, bytes)| (asset.clone(), bytes))
    }
    /// What became of the sound `asset`: its waveform, or none when it is not a WAV this can read (it is not tried again).
    pub fn made(&mut self, asset: String, wave: Option<String>) {
        if self.made.len() >= Self::KEPT {
            self.made.clear();
        }
        self.made.insert(asset, wave);
    }
}

/// The waveform of a WAV file of 16-bit samples, as a voice message carries it ([`crate::wire::wave`]): the loudest sample of
/// each of its bars, over all its channels. `None` for anything else, or a file with no samples.
pub(crate) fn measure_wav(file: &[u8]) -> Option<String> {
    let word = |at: usize, bytes: usize| -> Option<usize> {
        let field = file.get(at..at + bytes)?;
        Some(
            field
                .iter()
                .rev()
                .fold(0usize, |value, byte| value << 8 | usize::from(*byte)),
        )
    };
    if file.get(..4)? != b"RIFF" || file.get(8..12)? != b"WAVE" {
        return None;
    }
    // The chunks after the header, each a name, a length and that many bytes (padded to an even count).
    let (mut at, mut sixteen_bit, mut data) = (12, false, None);
    while let (Some(name), Some(length)) = (file.get(at..at + 4), word(at + 4, 4)) {
        let body = at + 8;
        match name {
            b"fmt " => sixteen_bit = word(body, 2)? == 1 && word(body + 14, 2)? == 16,
            b"data" => {
                data = file.get(body..body.checked_add(length)?.min(file.len()));
                break;
            }
            _ => {}
        }
        at = body.checked_add(length)?.checked_add(length % 2)?;
    }
    let data = data.filter(|data| sixteen_bit && data.len() >= 2)?;
    let samples = data.len() / 2;
    let levels: Vec<u8> = (0..BARS.min(samples))
        .map(|bar| {
            let (from, to) = (
                bar * samples / BARS.min(samples),
                (bar + 1) * samples / BARS.min(samples),
            );
            let peak = data[from * 2..to * 2]
                .chunks_exact(2)
                .map(|sample| i16::from_le_bytes([sample[0], sample[1]]).unsigned_abs())
                .max()
                .unwrap_or(0);
            crate::sdk::audio::level(peak)
        })
        .collect();
    crate::wire::wave(&levels)
}

#[derive(Clone, Copy)]
pub struct Context<'a> {
    pub engine: &'a Engine,
    pub directory: &'a BTreeMap<ScopeId, Peer>,
    pub locale: Locale,
    pub features: Features,
    pub thumbs: &'a Thumbs,
    pub listening: Option<&'a Listening>,
    pub waves: &'a Waves,
}
impl Context<'_> {
    fn peer_name(self, id: &ScopeId) -> String {
        self.directory.get(id).map_or_else(
            || {
                self.locale
                    .t("Unavailable device", "Dispozitiv indisponibil")
                    .into()
            },
            |p| p.name.clone(),
        )
    }
    fn room_name(self, room: &RoomKey) -> String {
        match room {
            RoomKey::Direct(id) => self.peer_name(id),
            RoomKey::Group(_) => self
                .engine
                .data
                .rooms
                .get(room)
                .and_then(|r| r.group.as_ref())
                .map(|g| g.title.clone())
                .unwrap_or_else(|| self.locale.t("Group", "Grup").into()),
        }
    }
    fn recipient(self, room: &RoomKey, id: &ScopeId) -> String {
        self.engine
            .data
            .rooms
            .get(room)
            .and_then(|r| r.group.as_ref())
            .and_then(|g| g.roster.iter().find(|m| &m.id == id))
            .map_or_else(|| self.peer_name(id), |m| m.label.clone())
    }
    fn presence(self, peer: Option<&ScopeId>) -> &'static str {
        let l = self.locale;
        match peer.and_then(|id| self.directory.get(id)) {
            Some(p) if p.route == Route::Ready && p.sleeping => l.t("Sleeping", "În repaus"),
            Some(p) if p.route == Route::Ready => l.t("Online", "Online"),
            Some(p) if p.route == Route::Offline => l.t("Offline", "Offline"),
            Some(p) if p.route == Route::Denied => l.t(
                "Local permissions needed",
                "Sunt necesare permisiuni locale",
            ),
            Some(_) => l.t("Not accepting Chat now", "Nu acceptă conversații acum"),
            None => l.t("Not paired or not visible", "Neasociat sau nevizibil"),
        }
    }
    fn online(self, peer: &ScopeId) -> bool {
        self.directory
            .get(peer)
            .is_some_and(|p| p.route == Route::Ready)
    }
    fn members(self, room: &RoomKey) -> usize {
        self.engine
            .data
            .rooms
            .get(room)
            .and_then(|r| r.group.as_ref())
            .map_or(0, |g| g.roster.len())
    }
    /// What a notification of `row` says: who wrote it (and in which group), and the start of what. One paragraph, within
    /// what a notification holds.
    pub(crate) fn notice(self, row: &Row) -> Option<String> {
        let body = row.body.as_ref()?;
        let author = match (&row.room, &row.member) {
            (RoomKey::Group(_), Some(member)) => format!(
                "{} · {}",
                self.recipient(&row.room, member),
                self.room_name(&row.room)
            ),
            _ => self.room_name(&row.room),
        };
        let what = body.text.as_deref().map_or_else(
            || Media::of(&body.kind).title(self.locale).to_owned(),
            one_line,
        );
        Some(
            format!("{author}: {what}")
                .chars()
                .take(crate::sdk::notification::MAX_TEXT)
                .collect(),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feedback {
    Failure(Fault),
    Permissions,
    NoMedia,
    PickerBusy,
    Cancelled,
    Exported,
    DraftExpired,
    TooLarge,
    /// The microphone gave nothing: no device, the system refused, or nothing was heard.
    Recording,
}
impl Feedback {
    fn words(self, l: Locale) -> &'static str {
        match self {
            Self::Failure(Fault::Quota) => l.t("Chat storage or the outbox is full. Clear unneeded history or queued messages, then try again.", "Stocarea sau coada este plină. Golește istoricul ori mesajele în așteptare de care nu mai ai nevoie, apoi încearcă din nou."),
            Self::Failure(Fault::NotDelivered) => l.t("Delivery changed. Open the message again.", "Starea livrării s-a schimbat. Deschide din nou mesajul."),
            Self::Failure(Fault::Storage) => l.t("Private storage is unavailable. This action wasn't confirmed.", "Stocarea privată nu este disponibilă. Acțiunea nu a fost confirmată."),
            Self::Failure(Fault::Rate) => l.t("Too many requests. Try again shortly.", "Prea multe cereri. Încearcă din nou în scurt timp."),
            Self::Failure(Fault::Conflict | Fault::Missing) => l.t("This item changed or is no longer available. Open it again.", "Elementul s-a schimbat sau nu mai este disponibil. Deschide-l din nou."),
            Self::Failure(Fault::Invalid) => l.t("This input wasn't accepted. Check the message, file or group name and try again.", "Datele nu au fost acceptate. Verifică mesajul, fișierul sau numele grupului și încearcă din nou."),
            Self::Permissions => l.t("A required permission is off. Check Chat’s permissions for this device in Settings › Extensions.", "O permisiune necesară este dezactivată. Verifică permisiunile Chat pentru acest dispozitiv în Setări › Extensii."),
            Self::NoMedia => l.t("This format isn't recognized for that media type. You can send it as a file instead.", "Formatul nu este recunoscut pentru acest tip media. Îl poți trimite ca fișier."),
            Self::PickerBusy => l.t("A file operation is already open. Finish or cancel it first.", "O operațiune cu fișiere este deja deschisă. Finalizeaz-o sau anuleaz-o întâi."),
            Self::Cancelled => l.t("File operation not completed.", "Operațiunea cu fișierul nu a fost finalizată."),
            Self::Exported => l.t("File saved to your chosen destination.", "Fișier salvat în destinația aleasă."),
            Self::DraftExpired => l.t("The temporary file expired. Choose it again.", "Fișierul temporar a expirat. Alege-l din nou."),
            Self::TooLarge => l.t("This file is larger than 8 MiB, the most Chat sends. Choose a smaller one.", "Fișierul are peste 8 MiB, cât trimite Chat cel mult. Alege unul mai mic."),
            Self::Recording => l.t("The microphone gave no recording. Check that this device has one and that the system lets Peersus use it.", "Microfonul nu a dat nicio înregistrare. Verifică dacă dispozitivul are unul și dacă sistemul îi permite lui Peersus să-l folosească."),
        }
    }
}

pub struct Draft {
    pub selected: Media,
    pub media_type: Option<&'static str>,
    pub name: String,
    pub stage: Sealed,
    pub expires: u64,
    /// A recorded voice message's waveform ([`crate::wire::Attachment::wave`]).
    pub wave: Option<String>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Busy {
    Picking,
    Importing,
    Saving,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Screen {
    #[default]
    Inbox,
    Room(RoomKey),
}
/// What is open over the page: one thing at a time, closed by its own buttons or a tap beside it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Overlay {
    #[default]
    None,
    NewChat,
    NewGroup,
    Attach,
    Options,
    Members,
    Menu(String),
    Detail(String),
    Delete(String),
    /// A photo opened over the page, by its message.
    Photo(String),
    Confirm(Confirmation),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Confirmation {
    Clear { remote: bool },
    Purge,
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum Action {
    Back,
    Open(RoomKey),
    Choose,
    RemoveMember(ScopeId),
    GroupTitle,
    MessageText,
    Older,
    Show(Overlay),
    Close,
    Confirm,
    React(String, Option<String>),
    Delete(Id, Delete),
    PickFile(Media),
    SendDraft,
    RemoveDraft,
    Save(String),
    /// A message: a tap opens its photo, a long press its menu, being seen marks it read.
    Message(String),
    Retry(ScopeId),
    Offer(ScopeId),
    /// Play or pause an audio message.
    Listen(String),
    /// Answer a message in its thread.
    Reply(String),
    /// Send the next message on its own, in no thread.
    Unreply,
    /// Show all of a thread's replies, or only its newest, by the thread's first message.
    Thread(String),
    /// Fold a thread to its count of replies, or open it again, by its first message.
    Fold(String),
    /// Start recording a voice message.
    Record,
    /// Stop recording and send it.
    FinishRecord,
    /// Stop recording and keep it to listen to its waveform before sending or deleting it.
    StopRecord,
    /// Stop recording and keep nothing.
    CancelRecord,
}
/// What a box this page presses observes: the application's taps and long presses, when it offers them.
fn pressing(c: Context<'_>) -> Vec<Observe> {
    if c.features.press {
        vec![Observe::Press]
    } else {
        Vec::new()
    }
}
#[derive(Debug)]
pub enum Effect {
    Open(RoomKey),
    Text(RoomKey, String),
    Group(Vec<String>, String),
    Choose(RoomKey, Media),
    SendDraft(RoomKey),
    RemoveDraft,
    Save(RoomKey, String),
    Delete(RoomKey, Id, Delete),
    Clear(RoomKey, bool, bool),
    React(RoomKey, String, Option<String>),
    Retry(ScopeId),
    OfferGroup(RoomKey, ScopeId),
    Seen(RoomKey, String),
    Listen(RoomKey, String),
    Record(RoomKey),
    /// Stop recording: send it (`true`), or keep it above the composer to send or delete.
    FinishRecord(RoomKey, bool),
    CancelRecord,
}
/// Where a bubble sits: first or last of one writer's run, and whether in a thread, under the message it answers (it then
/// quotes nothing).
#[derive(Clone, Copy)]
struct Place {
    first: bool,
    last: bool,
    nested: bool,
}
#[derive(Clone, Default)]
struct Bindings {
    old: Vec<(String, Action)>,
    current: Vec<(String, Action)>,
    serial: u64,
}
impl Bindings {
    fn id(&mut self, action: Action) -> String {
        // The same action can be shown in two different controls. Reuse only an id
        // that this tree has not already used.
        let id = self
            .old
            .iter()
            .find(|(id, previous)| {
                id.starts_with("item-")
                    && *previous == action
                    && !self.current.iter().any(|(used, _)| id == used)
            })
            .map(|(id, _)| id.clone())
            .unwrap_or_else(|| {
                self.serial = self.serial.saturating_add(1);
                format!("item-{:x}", self.serial)
            });
        self.bind(id, action)
    }
    fn bind(&mut self, id: String, action: Action) -> String {
        self.current.push((id.clone(), action));
        id
    }
    fn button_at(&mut self, id: String, action: Action, label: impl Into<String>) -> Node {
        Node::Button {
            id: self.bind(id, action),
            label: label.into(),
        }
    }
    fn button(&mut self, action: Action, label: impl Into<String>) -> Node {
        Node::Button {
            id: self.id(action),
            label: label.into(),
        }
    }
    fn action(&self, id: &str) -> Option<Action> {
        self.current
            .iter()
            .find(|(bound, _)| bound == id)
            .map(|(_, action)| action.clone())
    }
}

/// A picture this page keeps for itself: what it shows, and its picture once made (none when it could not be: not tried again).
pub struct Shown<K> {
    pub of: K,
    pub thumb: Option<Thumb>,
}

#[derive(Default)]
pub struct Page {
    screen: Screen,
    overlay: Overlay,
    /// How many more of the older messages are shown than the newest [`ROWS`].
    older: usize,
    group: Vec<ScopeId>,
    pub feedback: Option<Feedback>,
    text: String,
    field: u64,
    pub visit: u64,
    pub draft: Option<Draft>,
    pub busy: Option<Busy>,
    bindings: Bindings,
    last: Option<Node>,
    /// The page's width and height as the window last measured them.
    width: Option<u32>,
    height: Option<u32>,
    /// The picture of the chosen photo, by its stage, shown before it is sent.
    pub draft_picture: Option<Shown<u64>>,
    /// The large picture of the photo opened over the page, by its message.
    pub view_picture: Option<Shown<String>>,
    /// The message the next one sent answers, by its row key.
    pub replying: Option<String>,
    /// The threads shown whole, by their first message; the others show only their newest replies.
    threads: BTreeSet<String>,
    /// The threads folded to their count of replies, by their first message.
    folded: BTreeSet<String>,
    /// The conversation a voice message is being recorded in.
    pub recording: Option<(RoomKey, u64)>,
    /// What the microphone heard of the recording under way, as last read.
    pub live: Option<crate::sdk::audio::Levels>,
    /// The page's clock when the page is made: what a running recording's time is counted against.
    pub now: u64,
    /// While a page is made: the most messages it may show, when the page did not fit with more.
    fit: Option<usize>,
    /// While a page is made: how many messages of the conversation's own line it shows.
    shown: std::cell::Cell<usize>,
}
impl Page {
    pub fn room(&self) -> Option<&RoomKey> {
        if let Screen::Room(room) = &self.screen {
            Some(room)
        } else {
            None
        }
    }
    pub fn open(&mut self, room: RoomKey) {
        self.go(Screen::Room(room));
    }
    pub fn go_inbox(&mut self) {
        self.go(Screen::Inbox);
    }
    fn go(&mut self, screen: Screen) {
        if self.screen == screen {
            self.overlay = Overlay::None;
            return;
        }
        self.screen = screen;
        self.overlay = Overlay::None;
        self.older = 0;
        self.group.clear();
        self.feedback = None;
        self.text.clear();
        self.field = self.field.saturating_add(1);
        self.visit = self.visit.saturating_add(1);
        self.busy = None;
        self.replying = None;
        self.threads.clear();
        self.folded.clear();
    }
    /// The message whose photo is opened over the page.
    pub fn opened_photo(&self) -> Option<String> {
        match &self.overlay {
            Overlay::Photo(key) => Some(key.clone()),
            _ => None,
        }
    }
    fn wide(&self) -> bool {
        self.width.is_some_and(|width| width >= WIDE)
    }
    /// The most letters a message may have to sit on one line with its time: [`SHORT`], fewer on a page so narrow that the
    /// line would run out of it (about one letter in every eleven logical pixels of the page).
    fn short(&self) -> usize {
        self.width
            .map_or(SHORT, |width| (width / 11_000) as usize)
            .clamp(12, SHORT)
    }
    pub fn reset_after_clear(&mut self, purge: bool) {
        if purge {
            self.go_inbox();
        } else {
            self.visit = self.visit.saturating_add(1);
            self.overlay = Overlay::None;
            self.older = 0;
            self.text.clear();
            self.field = self.field.saturating_add(1);
            self.busy = None;
            self.replying = None;
        }
    }
    pub fn submitted(&mut self, result: Result<(), Fault>) {
        match result {
            Ok(()) => {
                self.text.clear();
                self.feedback = None;
                self.older = 0;
                self.replying = None;
            }
            Err(error) => {
                self.feedback = Some(Feedback::Failure(error));
            }
        }
        // The common field clears on submit; remount it with the retained text after failure.
        self.field = self.field.saturating_add(1);
    }
    /// Whether `event` can change nothing on the page: the pointer over a box this page does not press.
    pub fn passing(event: &Event) -> bool {
        matches!(event, Event::Pointer { .. })
    }
    pub fn event(&mut self, context: Context<'_>, event: Event) -> Option<Effect> {
        let id = match &event {
            Event::Size {
                id, width, height, ..
            } => {
                if id == PROBE {
                    self.width = Some(*width);
                    self.height = Some(*height);
                }
                return None;
            }
            Event::Press { id }
            | Event::Hold { id }
            | Event::Back { id }
            | Event::Choose { id, .. }
            | Event::Submit { id, .. }
            | Event::Visibility { id, .. } => id.clone(),
            _ => return None,
        };
        let action = self.bindings.action(&id)?;
        self.act(context, event, action)
    }
    fn act(&mut self, context: Context<'_>, event: Event, action: Action) -> Option<Effect> {
        let room = self.room().cloned();
        match (event, action) {
            (
                Event::Visibility {
                    visible: true,
                    foreground: true,
                    ..
                },
                Action::Message(row),
            ) if context.features.visibility => Some(Effect::Seen(room?, row)),
            (
                Event::Choose {
                    device: Some(alias),
                    ..
                },
                Action::Choose,
            ) => {
                let (id, peer) = context
                    .directory
                    .iter()
                    .find(|(_, p)| p.alias == alias && p.can_reply)?;
                self.feedback = None;
                match (&self.overlay, &self.screen) {
                    (Overlay::NewGroup, _) => {
                        if !self.group.contains(id) && self.group.len() < 7 {
                            self.group.push(id.clone());
                        }
                        None
                    }
                    (Overlay::Members, Screen::Room(RoomKey::Group(g))) => {
                        let group = context
                            .engine
                            .data
                            .rooms
                            .get(&RoomKey::Group(g.clone()))?
                            .group
                            .as_ref()?;
                        (context
                            .engine
                            .group_member(g, id)
                            .is_some_and(|member| group.has(member))
                            && peer.can_reply)
                            .then(|| Effect::Retry(id.clone()))
                    }
                    _ => Some(Effect::Open(RoomKey::Direct(id.clone()))),
                }
            }
            (Event::Submit { text, .. }, Action::MessageText) => {
                if text.trim().is_empty() {
                    return None;
                }
                self.text = text.clone();
                Some(Effect::Text(room?, text))
            }
            (Event::Submit { text, .. }, Action::GroupTitle) if self.group.len() >= 2 => {
                self.text = text.trim().to_owned();
                let aliases: Vec<_> = self
                    .group
                    .iter()
                    .filter_map(|id| {
                        context
                            .directory
                            .get(id)
                            .filter(|p| p.can_reply)
                            .map(|p| p.alias.clone())
                    })
                    .collect();
                if aliases.len() != self.group.len() {
                    self.submitted(Err(Fault::Missing));
                    return None;
                }
                Some(Effect::Group(aliases, self.text.clone()))
            }
            (Event::Back { .. }, _) => {
                self.feedback = None;
                if self.overlay == Overlay::None {
                    self.go_inbox();
                } else {
                    self.overlay = Overlay::None;
                }
                None
            }
            (Event::Press { .. }, action) => {
                self.feedback = None;
                match action {
                    Action::Back => {
                        self.go_inbox();
                        None
                    }
                    Action::Open(key) => Some(Effect::Open(key)),
                    Action::RemoveMember(id) => {
                        self.group.retain(|p| p != &id);
                        None
                    }
                    Action::Older => {
                        self.older = self.older.saturating_add(ROWS);
                        None
                    }
                    Action::Show(overlay) => {
                        if overlay == Overlay::NewGroup && self.overlay != Overlay::NewGroup {
                            self.group.clear();
                            self.text.clear();
                            self.field = self.field.saturating_add(1);
                        }
                        self.overlay = overlay;
                        None
                    }
                    Action::Close => {
                        self.overlay = Overlay::None;
                        None
                    }
                    Action::Message(key) => {
                        // A tap opens a photo; a tap on anything else leaves the conversation as it is.
                        let photo = context.engine.data.rows.get(&key).is_some_and(|row| {
                            Some(&row.room) == room.as_ref()
                                && row.asset.is_some()
                                && row.body.as_ref().is_some_and(|body| body.kind == "photo")
                        });
                        if photo {
                            self.overlay = Overlay::Photo(key);
                        }
                        None
                    }
                    Action::Confirm => {
                        let Overlay::Confirm(choice) = std::mem::take(&mut self.overlay) else {
                            return None;
                        };
                        Some(match choice {
                            Confirmation::Clear { remote } => Effect::Clear(room?, remote, false),
                            Confirmation::Purge => Effect::Clear(room?, false, true),
                        })
                    }
                    Action::Delete(id, choice) => {
                        self.overlay = Overlay::None;
                        Some(Effect::Delete(room?, id, choice))
                    }
                    Action::React(row, emoji) => {
                        self.overlay = Overlay::None;
                        Some(Effect::React(room?, row, emoji))
                    }
                    Action::PickFile(kind) => {
                        self.overlay = Overlay::None;
                        Some(Effect::Choose(room?, kind))
                    }
                    Action::SendDraft => Some(Effect::SendDraft(room?)),
                    Action::RemoveDraft => Some(Effect::RemoveDraft),
                    Action::Save(row) => {
                        self.overlay = Overlay::None;
                        Some(Effect::Save(room?, row))
                    }
                    Action::Retry(peer) => Some(Effect::Retry(peer)),
                    Action::Offer(member) => Some(Effect::OfferGroup(room?, member)),
                    Action::Listen(row) => Some(Effect::Listen(room?, row)),
                    Action::Record => Some(Effect::Record(room?)),
                    Action::FinishRecord => Some(Effect::FinishRecord(room?, true)),
                    Action::StopRecord => Some(Effect::FinishRecord(room?, false)),
                    Action::CancelRecord => Some(Effect::CancelRecord),
                    Action::Reply(row) => {
                        self.overlay = Overlay::None;
                        self.replying = Some(row);
                        None
                    }
                    Action::Unreply => {
                        self.replying = None;
                        None
                    }
                    Action::Thread(root) => {
                        if !self.threads.remove(&root) {
                            self.threads.insert(root);
                        }
                        None
                    }
                    Action::Fold(root) => {
                        if !self.folded.remove(&root) {
                            self.folded.insert(root);
                        }
                        None
                    }
                    _ => None,
                }
            }
            // A long press, or the mouse's other button: what can be done with a message.
            (Event::Hold { .. }, Action::Message(key)) => {
                self.feedback = None;
                self.overlay = Overlay::Menu(key);
                None
            }
            (Event::Hold { .. }, Action::Close) => {
                self.overlay = Overlay::None;
                None
            }
            _ => None,
        }
    }

    pub fn render(&mut self, context: Context<'_>, force: bool) -> Value {
        let mut start = std::mem::take(&mut self.bindings);
        start.old = std::mem::take(&mut start.current);
        let l = context.locale;
        // The page must fit what the application draws (its node and byte bounds). When it does not, fewer of the newest
        // messages are shown, with "Show older messages" above them, rather than nothing.
        self.fit = None;
        loop {
            self.shown.set(0);
            let (tree, bindings) = self.tree(context, start.clone());
            let value = tree
                .validate()
                .is_ok()
                .then(|| serde_json::to_value(&tree).ok())
                .flatten()
                .filter(sdk_wire_size);
            let shown = self.shown.get();
            if value.is_none() && shown > MIN_FIT {
                self.fit = Some(shown * 3 / 4);
                continue;
            }
            self.bindings = bindings;
            self.bindings.old.clear();
            let Some(value) = value else {
                self.bindings.current.clear();
                return json!({"k":"text","text":l.t("This page was too large to display.", "Pagina este prea mare pentru a fi afișată.")});
            };
            if !force && self.last.as_ref() == Some(&tree) {
                return Value::Null;
            }
            self.last = Some(tree);
            return value;
        }
    }

    /// The page's whole tree, with the controls it binds.
    fn tree(&mut self, context: Context<'_>, mut bindings: Bindings) -> (Node, Bindings) {
        let wide = self.wide();
        let body = if wide {
            let conversation = match &self.screen {
                Screen::Room(room) => self.conversation(context, room, &mut bindings, true),
                Screen::Inbox => self.placeholder(context, &mut bindings),
            };
            boxed(
                Style {
                    width: FULL,
                    height: Length::Flex(1),
                    horizontal: true,
                    gap: 12,
                    ..Style::default()
                },
                vec![
                    self.list(context, &mut bindings, Length::Px(330)),
                    boxed(
                        Style {
                            width: Length::Flex(1),
                            height: FULL,
                            background: Colour::Surface,
                            radius: [16; 4],
                            clip: true,
                            ..Style::default()
                        },
                        vec![conversation],
                    ),
                ],
            )
        } else {
            match &self.screen {
                Screen::Inbox => self.list(context, &mut bindings, FULL),
                Screen::Room(room) => self.conversation(context, room, &mut bindings, false),
            }
        };
        let mut children = vec![
            Node::Box {
                id: Some(PROBE.into()),
                // Placed at the page's corner and drawn under it, so that it measures the page's whole height without taking
                // any of it or covering what is on it (an overlay box would be raised over the page).
                style: Style {
                    width: FULL,
                    height: FULL,
                    at: Some([0, 0]),
                    ..Style::default()
                },
                observe: vec![Observe::Size],
                children: Vec::new(),
            },
            body,
        ];
        if self.overlay != Overlay::None {
            match self.sheet(context, &mut bindings) {
                Some(sheet) if !sheet.is_empty() => {
                    children.push(over(&mut bindings, pressing(context), wide, sheet));
                }
                _ => self.overlay = Overlay::None,
            }
        }
        // Back closes what is open over the page first, then leaves the conversation for the list; on the list it is the
        // application's again. Taken by an empty box of its own, so that nothing on the page is pressed differently for it.
        if context.features.back
            && (self.overlay != Overlay::None || matches!(self.screen, Screen::Room(_)))
        {
            children.push(Node::Box {
                id: Some(bindings.bind("page-back".into(), Action::Back)),
                style: Style {
                    width: Length::Px(0),
                    height: Length::Px(0),
                    ..Style::default()
                },
                observe: vec![Observe::Back],
                children: Vec::new(),
            });
        }
        let tree = Node::Interval {
            id: TICK.into(),
            // Faster while recording, so its waveform moves as the voice does.
            milliseconds: if self.recording.is_some() { 200 } else { 750 },
            child: Box::new(boxed(
                Style {
                    width: FULL,
                    height: Length::Flex(1),
                    ..Style::default()
                },
                children,
            )),
        };
        (tree, bindings)
    }

    /// The conversations, newest activity first.
    fn list(&self, c: Context<'_>, b: &mut Bindings, width: Length) -> Node {
        let l = c.locale;
        let header = boxed(
            Style {
                width: FULL,
                horizontal: true,
                padding: 8,
                gap: 8,
                ..Style::default()
            },
            vec![
                grow(vec![text(l.t("Chats", "Conversații"), Tone::Title)]),
                b.button_at(
                    "new-chat".into(),
                    Action::Show(Overlay::NewChat),
                    l.t("New chat", "Conversație nouă"),
                ),
            ],
        );
        let mut children = vec![header];
        if self.room().is_none()
            && let Some(note) = self.feedback
        {
            children.push(note_box(note.words(l)));
        }
        let mut activity: BTreeMap<&RoomKey, (&Row, usize)> = BTreeMap::new();
        for row in c.engine.data.rows.values() {
            let unread = usize::from(!row.mine && !row.seen && row.body.is_some());
            activity
                .entry(&row.room)
                .and_modify(|(last, count)| {
                    if (row.order, &row.key) > (last.order, &last.key) {
                        *last = row;
                    }
                    *count += unread;
                })
                .or_insert((row, unread));
        }
        let mut rooms: Vec<_> = c.engine.data.rooms.keys().collect();
        rooms.sort_by_key(|room| {
            (
                std::cmp::Reverse(activity.get(room).map_or(0, |(row, _)| row.order)),
                *room,
            )
        });
        if rooms.is_empty() {
            let mut empty = vec![
                text(l.t("No chats yet", "Nicio conversație încă"), Tone::Title),
                text(
                    l.t(
                        "Messages and files stay on the devices in the chat.",
                        "Mesajele și fișierele rămân pe dispozitivele din conversație.",
                    ),
                    Tone::Muted,
                ),
            ];
            if c.directory.is_empty() {
                empty.push(text(l.t("Pair a device and allow Chat to see and send to it in Settings.","Asociază un dispozitiv și permite extensiei Chat să îl vadă și să îi trimită date în Setări."),Tone::Muted));
            }
            empty.push(b.button(
                Action::Show(Overlay::NewChat),
                l.t("Start a chat", "Începe o conversație"),
            ));
            children.push(boxed(
                Style {
                    width: FULL,
                    height: Length::Flex(1),
                    center: true,
                    gap: 10,
                    padding: 16,
                    ..Style::default()
                },
                empty,
            ));
        } else {
            let selected = self.room();
            let items = rooms
                .iter()
                .take(ROOMS)
                .map(|room| {
                    let (last, unread) = activity
                        .get(room)
                        .map_or((None, 0), |(row, n)| (Some(*row), *n));
                    self.room_row(c, b, room, last, unread, selected == Some(*room))
                })
                .collect();
            children.push(boxed(
                Style {
                    width: FULL,
                    height: Length::Flex(1),
                    scroll: true,
                    clip: true,
                    gap: 2,
                    ..Style::default()
                },
                items,
            ));
        }
        boxed(
            Style {
                width,
                height: FULL,
                background: Colour::Surface,
                radius: [16; 4],
                padding: 6,
                gap: 4,
                clip: true,
                ..Style::default()
            },
            children,
        )
    }

    fn room_row(
        &self,
        c: Context<'_>,
        b: &mut Bindings,
        room: &RoomKey,
        last: Option<&Row>,
        unread: usize,
        selected: bool,
    ) -> Node {
        let l = c.locale;
        let name = c.room_name(room);
        let preview = match last {
            None => match room {
                RoomKey::Direct(id) => c.presence(Some(id)).to_owned(),
                RoomKey::Group(_) => format!("{} {}", c.members(room), l.t("members", "membri")),
            },
            Some(row) => {
                let what = summary(l, row);
                if row.mine {
                    format!("{} {what}", l.t("You:", "Tu:"))
                } else if let (RoomKey::Group(_), Some(member)) = (room, &row.member) {
                    format!("{}: {what}", c.recipient(room, member))
                } else {
                    what
                }
            }
        };
        let mut side = Vec::new();
        if unread != 0 {
            side.push(badge(unread));
        }
        Node::Box {
            id: Some(b.bind(
                format!("row-{}", room.namespace()),
                Action::Open(room.clone()),
            )),
            style: Style {
                width: FULL,
                horizontal: true,
                gap: 12,
                padding: 10,
                radius: [12; 4],
                background: if selected {
                    Colour::Raised
                } else {
                    Colour::Transparent
                },
                ..Style::default()
            },
            observe: pressing(c),
            children: vec![
                portrait(c, direct(room), &name, 44),
                grow(vec![
                    Node::Text {
                        text: short(&name, 80),
                        tone: Tone::Body,
                        lines: Some(1),
                        size: None,
                    },
                    Node::Text {
                        text: one_line(&preview),
                        tone: Tone::Muted,
                        lines: Some(1),
                        size: Some(13),
                    },
                ]),
                boxed(
                    Style {
                        gap: 4,
                        center: true,
                        ..Style::default()
                    },
                    side,
                ),
            ],
        }
    }

    /// The wide page's right pane before a conversation is chosen.
    fn placeholder(&self, c: Context<'_>, b: &mut Bindings) -> Node {
        let l = c.locale;
        boxed(
            Style {
                width: FULL,
                height: FULL,
                center: true,
                gap: 12,
                ..Style::default()
            },
            vec![
                Node::Icon {
                    name: "chat".into(),
                    size: 48,
                },
                text(
                    l.t(
                        "Choose a chat or start a new one",
                        "Alege o conversație sau începe una nouă",
                    ),
                    Tone::Muted,
                ),
                b.button(
                    Action::Show(Overlay::NewChat),
                    l.t("New chat", "Conversație nouă"),
                ),
            ],
        )
    }

    fn conversation(&self, c: Context<'_>, room: &RoomKey, b: &mut Bindings, wide: bool) -> Node {
        let l = c.locale;
        let name = c.room_name(room);
        let status = match room {
            RoomKey::Direct(id) => c.presence(Some(id)).to_owned(),
            RoomKey::Group(_) => format!("{} {}", c.members(room), l.t("members", "membri")),
        };
        let mut header = Vec::new();
        if !wide {
            header.push(icon_button_at(
                c,
                b,
                Some("back".into()),
                Action::Back,
                "back",
                l.t("Back", "Înapoi"),
                false,
            ));
        }
        header.push(portrait(c, direct(room), &name, 36));
        header.push(grow(vec![
            Node::Text {
                text: short(&name, 80),
                tone: Tone::Body,
                lines: Some(1),
                size: None,
            },
            Node::Text {
                text: status,
                tone: Tone::Muted,
                lines: Some(1),
                size: Some(12),
            },
        ]));
        header.push(b.button_at(
            "options".into(),
            Action::Show(Overlay::Options),
            l.t("Options", "Opțiuni"),
        ));
        let header = boxed(
            Style {
                width: FULL,
                horizontal: true,
                gap: 10,
                padding: 10,
                ..Style::default()
            },
            header,
        );

        let mut rows: Vec<_> = c
            .engine
            .data
            .rows
            .values()
            .filter(|row| &row.room == room)
            .collect();
        rows.sort_by(|a, b| (&a.order, &a.key, &a.member).cmp(&(&b.order, &b.key, &b.member)));
        // A reply hangs under the message it answers, never alone in the conversation's own line. One whose message is not in
        // that line (replies that name each other) stays in the line, quoting what it answers.
        let mut threads: BTreeMap<String, Vec<&Row>> = BTreeMap::new();
        let mut line = Vec::new();
        for row in rows {
            match c.engine.root(row) {
                Some(root) => threads.entry(root.storage_key()).or_default().push(row),
                None => line.push(row),
            }
        }
        let starts: BTreeSet<String> = line.iter().map(|row| row.storage_key()).collect();
        threads.retain(|root, replies| {
            starts.contains(root) || {
                line.append(replies);
                false
            }
        });
        // A message with replies is as recent as its newest reply: a new answer to an old message comes down to where it is
        // seen, with the message it answers above it.
        let mut line: Vec<_> = line
            .into_iter()
            .map(|row| {
                let key = row.storage_key();
                let latest = threads
                    .get(&key)
                    .and_then(|replies| replies.last())
                    .map_or(row.order, |reply| reply.order.max(row.order));
                (latest, key, row)
            })
            .collect();
        line.sort_by(|(x, _, a), (y, _, b)| {
            (x, &a.order, &a.key, &a.member).cmp(&(y, &b.order, &b.key, &b.member))
        });
        let weight = |row: &Row| {
            row.body
                .as_ref()
                .and_then(|body| body.text.as_ref())
                .map_or(64, |text| text.len() + 64)
        };
        // The newest messages, as many as fit the page, and older ones only when asked for.
        let mut start = line.len();
        let mut budget = ROW_TEXT.saturating_add(self.older.saturating_mul(ROW_TEXT / ROWS));
        let limit = ROWS
            .saturating_add(self.older)
            .min(self.fit.unwrap_or(usize::MAX));
        while start > 0 && line.len() - start < limit {
            let (_, key, row) = &line[start - 1];
            let replies = threads
                .get(key)
                .map_or(&[][..], |replies| self.newest_replies(key, replies));
            let cost = weight(row) + replies.iter().map(|reply| weight(reply)).sum::<usize>();
            if cost > budget && line.len() > start {
                break;
            }
            budget = budget.saturating_sub(cost);
            start -= 1;
        }
        self.shown.set(line.len() - start);
        let mut lines = Vec::new();
        if start > 0 {
            lines.push(boxed(
                Style {
                    width: FULL,
                    center: true,
                    ..Style::default()
                },
                vec![b.button_at(
                    "older".into(),
                    Action::Older,
                    l.t("Show older messages", "Arată mesajele mai vechi"),
                )],
            ));
        }
        if line.is_empty() {
            lines.push(boxed(
                Style {
                    width: FULL,
                    center: true,
                    padding: 24,
                    ..Style::default()
                },
                vec![text(
                    l.t(
                        "No messages yet. Say hello.",
                        "Niciun mesaj încă. Spune salut.",
                    ),
                    Tone::Muted,
                )],
            ));
        }
        let shown = &line[start..];
        let answered = |i: usize| {
            shown
                .get(i)
                .is_some_and(|(_, key, _)| threads.contains_key(key))
        };
        for (i, (_, key, row)) in shown.iter().enumerate() {
            let same = |other: Option<&(u64, String, &Row)>| {
                other.is_some_and(|(_, _, o)| o.mine == row.mine && o.member == row.member)
            };
            let first = i == 0 || !same(shown.get(i - 1)) || answered(i - 1);
            let last = !same(shown.get(i + 1)) || answered(i);
            if first && i != 0 {
                lines.push(spacer(8));
            }
            let place = Place {
                first,
                last,
                nested: false,
            };
            lines.push(self.bubble(c, room, row, b, place));
            if let Some(replies) = threads.get(key) {
                lines.push(self.replies(c, room, row, key, replies, b));
            }
        }
        if let Some(info) = c.engine.data.rooms.get(room)
            && !info.cleared.is_empty()
        {
            let summaries: Vec<_> = info
                .cleared
                .iter()
                .map(|(id, status)| {
                    format!("{}: {}", c.recipient(room, id), removal(l, *status, true))
                })
                .collect();
            lines.push(pill(summaries.join(" · ")));
        }
        // The scroll box itself has no padding: it is at least as tall as its viewport, and its one
        // padded column rests on the lower edge without overflowing it.
        let messages = boxed(
            Style {
                width: FULL,
                height: Length::Flex(1),
                scroll: true,
                bottom: true,
                clip: true,
                ..Style::default()
            },
            vec![boxed(
                Style {
                    width: FULL,
                    padding: 12,
                    gap: 2,
                    ..Style::default()
                },
                lines,
            )],
        );

        let mut children = vec![header, Node::Divider, messages];
        if let Some(note) = self.feedback {
            children.push(note_box(note.words(l)));
        }
        if let Some(pending) = self.pending(c, b) {
            children.push(pending);
        }
        if let Some(answered) = self
            .replying
            .as_ref()
            .and_then(|key| c.engine.data.rows.get(key))
            .filter(|row| &row.room == room && row.body.is_some())
        {
            children.push(boxed(
                Style {
                    width: FULL,
                    horizontal: true,
                    gap: 8,
                    padding: 8,
                    background: if wide {
                        Colour::Raised
                    } else {
                        Colour::Surface
                    },
                    ..Style::default()
                },
                vec![
                    boxed(
                        Style {
                            width: Length::Flex(1),
                            gap: 4,
                            ..Style::default()
                        },
                        vec![
                            text(l.t("Replying to", "Răspunzi la"), Tone::Muted),
                            quote(c, room, Some(answered)),
                        ],
                    ),
                    b.button(Action::Unreply, l.t("Cancel", "Anulează")),
                ],
            ));
        }
        // While a voice message is recorded, in place of the composer: a pill with stop, an equaliser of what the microphone
        // hears now (low frequencies first) and how long it has run; under it, delete and send.
        let recording = self
            .recording
            .as_ref()
            .filter(|(at, _)| at == room)
            .map(|(_, since)| {
                self.live
                    .as_ref()
                    .map_or_else(|| self.now.saturating_sub(*since), |live| live.elapsed_ms)
            });
        let mut composer = if let Some(elapsed) = recording {
            let heard = equaliser(self.live.as_ref().map_or(&[][..], |live| &live.bands));
            let stop = icon_button(
                c,
                b,
                Action::StopRecord,
                "stop",
                l.t("Stop", "Oprește"),
                false,
            );
            children.push(boxed(
                Style {
                    width: FULL,
                    padding: 8,
                    ..Style::default()
                },
                vec![voice_pill(stop, &heard, None, timer(elapsed))],
            ));
            vec![
                icon_button(
                    c,
                    b,
                    Action::CancelRecord,
                    "delete",
                    l.t("Delete", "Șterge"),
                    false,
                ),
                grow(Vec::new()),
                icon_button(
                    c,
                    b,
                    Action::FinishRecord,
                    "message-send",
                    l.t("Send", "Trimite"),
                    true,
                ),
            ]
        } else {
            vec![icon_button_at(
                c,
                b,
                Some("attach".into()),
                Action::Show(Overlay::Attach),
                "add",
                l.t("Attach", "Atașează"),
                false,
            )]
        };
        if recording.is_none() {
            composer.push(self.field(
                b,
                Action::MessageText,
                l.t("Write a message", "Scrie un mesaj"),
                Length::Flex(1),
            ));
            if c.features.record {
                composer.push(icon_button(
                    c,
                    b,
                    Action::Record,
                    "microphone",
                    l.t("Record", "Înregistrează"),
                    false,
                ));
            }
        }
        children.push(boxed(
            Style {
                width: FULL,
                horizontal: true,
                gap: 8,
                padding: 8,
                background: if wide {
                    Colour::Raised
                } else {
                    Colour::Surface
                },
                ..Style::default()
            },
            composer,
        ));
        boxed(
            Style {
                width: FULL,
                height: FULL,
                background: if wide {
                    Colour::Transparent
                } else {
                    Colour::Surface
                },
                radius: if wide { [0; 4] } else { [16; 4] },
                clip: true,
                ..Style::default()
            },
            children,
        )
    }

    fn bubble(
        &self,
        c: Context<'_>,
        room: &RoomKey,
        row: &Row,
        b: &mut Bindings,
        place: Place,
    ) -> Node {
        let Place {
            first,
            last,
            nested,
        } = place;
        let l = c.locale;
        let key = row.storage_key();
        let anchor = anchor(c, room, row);
        let mut content = Vec::new();
        if matches!(room, RoomKey::Group(_)) && !row.mine && first {
            let name = row.member.as_ref().map_or_else(
                || l.t("Member", "Membru").into(),
                |id| c.recipient(room, id),
            );
            content.push(Node::Text {
                text: short(&name, 80),
                tone: Tone::Title,
                lines: Some(1),
                size: Some(13),
            });
        }
        let mut narrow = true;
        // The width of the photo shown, when the message is one: its bubble is then as wide as the picture.
        let mut picture = None;
        // A voice message that plays here: its bubble is a pill, as round as a recording's.
        let mut voice = false;
        match &row.body {
            Some(body) => {
                if body.reply.is_some() && !nested {
                    narrow = false;
                    content.push(quote(c, room, c.engine.replied(row)));
                }
                if let Some(message) = &body.text {
                    narrow &= message.chars().count() <= self.short() && !message.contains('\n');
                    // A line break is kept as the start of the next text.
                    for line in message.split('\n').filter(|line| !line.trim().is_empty()) {
                        content.push(text(line, Tone::Body));
                    }
                }
                for attachment in &body.attachments {
                    narrow = false;
                    // A photo whose picture is made shows it; until then, and for any other file, its name and size.
                    if body.kind == "photo"
                        && let Some(thumb) = row.asset.as_deref().and_then(|a| c.thumbs.get(a))
                    {
                        let (width, height) = shown(thumb);
                        picture = Some(width);
                        content.push(Node::Picture {
                            id: thumb.id.clone(),
                            width,
                            height,
                            radius: 12,
                            digest: None,
                        });
                        continue;
                    }
                    // An audio message this device holds plays in its bubble, when this process decodes sounds, drawn as a
                    // voice message is: play, the waveform its writer measured filling as it plays, and how long it is.
                    if body.kind == "audio" && c.features.listen && row.asset.is_some() {
                        let heard = c.listening.filter(|heard| heard.row == key);
                        let playing = heard.is_some_and(|heard| heard.playing);
                        let length = heard
                            .and_then(|heard| heard.length_ms)
                            .or_else(|| recorded_length(attachment));
                        let position = heard.map_or(0, |heard| heard.position_ms);
                        let played = match (heard, length) {
                            (Some(_), Some(length)) if length > 0 => {
                                Some((position.min(length) * BARS as u64 / length) as usize)
                            }
                            (Some(_), _) => Some(0),
                            (None, _) => None,
                        };
                        let time = match (heard, length) {
                            (Some(_), _) => clock(position),
                            (None, Some(length)) => clock(length),
                            (None, None) => file_size(l, attachment.bytes),
                        };
                        voice = true;
                        let button = icon_button(
                            c,
                            b,
                            Action::Listen(key.clone()),
                            if playing { "pause" } else { "play" },
                            if playing {
                                l.t("Pause", "Pauză")
                            } else {
                                l.t("Play", "Redă")
                            },
                            false,
                        );
                        // A chosen sound keeps its name above; a recorded voice message needs none.
                        if attachment.name != VOICE_NAME {
                            content.push(Node::Text {
                                text: short(&attachment.name, 80),
                                tone: Tone::Muted,
                                lines: Some(1),
                                size: Some(12),
                            });
                        }
                        // The waveform its writer measured, or the one measured here from the sound itself.
                        let levels = wave_levels(
                            attachment
                                .wave
                                .as_deref()
                                .or_else(|| c.waves.get(row.asset.as_deref()?)),
                        );
                        content.push(boxed(
                            Style {
                                width: FULL,
                                padding: 2,
                                gap: 10,
                                horizontal: true,
                                center: true,
                                ..Style::default()
                            },
                            vec![
                                button,
                                spectrum(&levels, played),
                                Node::Text {
                                    text: time,
                                    tone: Tone::Body,
                                    lines: Some(1),
                                    size: Some(15),
                                },
                            ],
                        ));
                        continue;
                    }
                    content.push(boxed(
                        Style {
                            width: FULL,
                            padding: 8,
                            gap: 2,
                            radius: [10; 4],
                            background: Colour::Surface,
                            ..Style::default()
                        },
                        vec![
                            labelled(
                                "attachment",
                                Node::Text {
                                    text: short(&attachment.name, 120),
                                    tone: Tone::Body,
                                    lines: Some(2),
                                    size: None,
                                },
                            ),
                            Node::Text {
                                text: format!(
                                    "{} · {}",
                                    Media::of(&body.kind).title(l),
                                    file_size(l, attachment.bytes)
                                ),
                                tone: Tone::Muted,
                                lines: Some(1),
                                size: Some(12),
                            },
                        ],
                    ));
                }
            }
            None => content.push(text(l.t("Message deleted", "Mesaj șters"), Tone::Muted)),
        }
        let time = row.at.map(|at| when(at, c.engine.wall));
        // The time, and for my own message how far it got, as the application's picture of it.
        let marked = |size: u8| {
            let mut parts = Vec::new();
            if let Some(time) = &time {
                parts.push(Node::Text {
                    text: time.clone(),
                    tone: Tone::Body,
                    lines: Some(1),
                    size: Some(size),
                });
            }
            if row.mine {
                parts.push(Node::Icon {
                    name: ticks(row).into(),
                    size: u16::from(size) + 3,
                });
            }
            boxed(
                Style {
                    horizontal: true,
                    center: true,
                    gap: 5,
                    ..Style::default()
                },
                parts,
            )
        };
        if voice {
            // A voice message's pill carries its time under its length; it has no line of its own.
            if let Some(Node::Box { children, .. }) = content.last_mut()
                && let Some(length) = children.pop()
            {
                children.push(boxed(
                    Style {
                        gap: 2,
                        ..Style::default()
                    },
                    vec![length, marked(11)],
                ));
            }
        } else if row.body.is_some() && (row.mine || time.is_some()) {
            let mark = marked(12);
            if narrow {
                // A short message and its mark share one line.
                content = vec![boxed(
                    Style {
                        horizontal: true,
                        gap: 8,
                        ..Style::default()
                    },
                    std::mem::take(&mut content)
                        .into_iter()
                        .chain([mark])
                        .collect(),
                )];
            } else {
                content.push(boxed(
                    Style {
                        width: picture.map_or(FULL, Length::Px),
                        horizontal: true,
                        ..Style::default()
                    },
                    vec![grow(Vec::new()), mark],
                ));
            }
        }
        let observed = c.features.visibility && !row.mine && !row.seen && row.body.is_some();
        // Round everywhere but the corner pointing at its author, on the last of a run.
        let tail = if last { 4 } else { 16 };
        let mut observe = pressing(c);
        if observed {
            observe.push(Observe::Visibility);
        }
        let bubble = Node::Box {
            id: Some(b.bind(anchor, Action::Message(key))),
            observe,
            style: Style {
                // A photo's bubble is as wide as its picture, with a thin rim of the bubble's colour around it; a voice
                // message's fills its column.
                width: if voice {
                    FULL
                } else if narrow || picture.is_some() {
                    Length::Auto
                } else {
                    Length::Percent(7800)
                },
                gap: 4,
                padding: if picture.is_some() { 4 } else { 10 },
                radius: if voice {
                    [28; 4]
                } else if row.mine {
                    [16, 16, tail, 16]
                } else {
                    [16, 16, 16, tail]
                },
                background: if row.body.is_none() {
                    Colour::Surface
                } else if row.mine {
                    Colour::Accent
                } else {
                    Colour::Raised
                },
                border: row.body.is_none(),
                ..Style::default()
            },
            children: content,
        };
        let mut stack = vec![bubble];
        if row.body.is_some()
            && let Some(reactions) = reactions(row)
        {
            stack.push(pill(reactions));
        }
        let column = boxed(
            Style {
                gap: 2,
                // A voice message is as wide as a phone's line allows, and no wider than a messenger's on a wide page.
                width: if voice {
                    if self.wide() {
                        Length::Px(320)
                    } else {
                        Length::Percent(8_500)
                    }
                } else if narrow || picture.is_some() {
                    Length::Auto
                } else {
                    Length::Percent(10_000)
                },
                ..Style::default()
            },
            stack,
        );
        let mut line = Vec::new();
        if row.mine {
            line.push(grow(Vec::new()));
            line.push(column);
        } else {
            line.push(column);
            line.push(grow(Vec::new()));
        }
        boxed(
            Style {
                width: FULL,
                horizontal: true,
                ..Style::default()
            },
            line,
        )
    }

    /// The replies of a thread this page shows: the newest few, or all of them when the person asked.
    fn newest_replies<'r>(&self, root: &str, replies: &'r [&'r Row]) -> &'r [&'r Row] {
        if self.threads.contains(root) {
            replies
        } else {
            &replies[replies.len().saturating_sub(THREAD)..]
        }
    }

    /// The thread of `root`, hung under it as messengers show one: "N replies" that folds and opens it, then, set in behind a
    /// thin line, the replies themselves and "Add a reply…". What each reply answers is the message above the thread; one that
    /// answers another reply quotes the one it answers.
    fn replies(
        &self,
        c: Context<'_>,
        room: &RoomKey,
        root: &Row,
        key: &str,
        replies: &[&Row],
        b: &mut Bindings,
    ) -> Node {
        let l = c.locale;
        let open = !self.folded.contains(key);
        let count = match replies.len() {
            1 => l.t("1 reply", "1 răspuns").to_owned(),
            n => format!("{n} {}", l.t("replies", "răspunsuri")),
        };
        let toggle = if c.features.press {
            Node::Box {
                id: Some(b.id(Action::Fold(key.to_owned()))),
                observe: vec![Observe::Press],
                style: Style {
                    horizontal: true,
                    center: true,
                    gap: 6,
                    padding: 8,
                    radius: [14; 4],
                    border: true,
                    ..Style::default()
                },
                children: vec![
                    text(count, Tone::Body),
                    Node::Icon {
                        name: if open { "chevron-up" } else { "chevron-down" }.into(),
                        size: 16,
                    },
                ],
            }
        } else {
            b.button(Action::Fold(key.to_owned()), count)
        };
        if !open {
            return boxed(
                Style {
                    width: FULL,
                    padding: 4,
                    ..Style::default()
                },
                vec![toggle],
            );
        }
        let shown = self.newest_replies(key, replies);
        let mut items = vec![toggle];
        if shown.len() < replies.len() {
            items.push(b.button(
                Action::Thread(key.to_owned()),
                format!(
                    "{} ({})",
                    l.t("Show earlier replies", "Arată răspunsurile anterioare"),
                    replies.len() - shown.len()
                ),
            ));
        }
        for (i, reply) in shown.iter().enumerate() {
            let same = |other: Option<&&Row>| {
                other.is_some_and(|o| o.mine == reply.mine && o.member == reply.member)
            };
            let first = i == 0 || !same(shown.get(i - 1));
            let last = !same(shown.get(i + 1));
            if first && i != 0 {
                items.push(spacer(4));
            }
            let place = Place {
                first,
                last,
                nested: c.engine.replied(reply).map(|row| row.storage_key())
                    == Some(root.storage_key()),
            };
            items.push(self.bubble(c, room, reply, b, place));
        }
        // Answer this thread: the composer then shows the message it answers.
        let add = l.t("Add a reply…", "Adaugă un răspuns…");
        items.push(if c.features.press {
            Node::Box {
                id: Some(b.id(Action::Reply(key.to_owned()))),
                observe: vec![Observe::Press],
                style: Style {
                    width: FULL,
                    padding: 8,
                    radius: [14; 4],
                    border: true,
                    ..Style::default()
                },
                children: vec![text(add, Tone::Muted)],
            }
        } else {
            b.button(Action::Reply(key.to_owned()), add)
        });
        boxed(
            Style {
                width: FULL,
                horizontal: true,
                gap: 10,
                ..Style::default()
            },
            vec![
                // The thin line down the thread's left, as tall as the thread.
                boxed(
                    Style {
                        width: Length::Px(2),
                        height: Length::FillMinimum,
                        radius: [1; 4],
                        background: Colour::Border,
                        ..Style::default()
                    },
                    Vec::new(),
                ),
                boxed(
                    Style {
                        // Beside a box as tall as its row, a flexible width is not shared out: a share of the row instead.
                        width: Length::Percent(9_500),
                        gap: 2,
                        ..Style::default()
                    },
                    items,
                ),
            ],
        )
    }

    /// What waits above the composer: an operation in progress or a chosen file.
    fn pending(&self, c: Context<'_>, b: &mut Bindings) -> Option<Node> {
        let l = c.locale;
        if let Some(busy) = self.busy {
            return Some(note_box(match busy {
                Busy::Picking => l.t(
                    "Waiting for file selection…",
                    "Se așteaptă alegerea fișierului…",
                ),
                Busy::Importing => l.t(
                    "Preparing a private copy…",
                    "Se pregătește o copie privată…",
                ),
                Busy::Saving => l.t("Save dialog open…", "Dialogul de salvare este deschis…"),
            }));
        }
        let draft = self.draft.as_ref()?;
        // A stopped recording: its pill, then delete and send.
        if draft.name == VOICE_NAME && draft.media_type == Some("audio/wav") {
            let length = u64::from(draft.stage.bytes as u32)
                .saturating_sub(crate::sdk::audio::WAV_HEADER_BYTES as u64)
                * 1000
                / (u64::from(crate::sdk::audio::RECORD_RATE) * 2);
            let lead = boxed(
                Style {
                    width: Length::Px(40),
                    height: Length::Px(40),
                    center: true,
                    ..Style::default()
                },
                vec![Node::Icon {
                    name: "microphone".into(),
                    size: 22,
                }],
            );
            return Some(boxed(
                Style {
                    width: FULL,
                    gap: 8,
                    padding: 8,
                    ..Style::default()
                },
                vec![
                    voice_pill(
                        lead,
                        &wave_levels(draft.wave.as_deref()),
                        None,
                        timer(length),
                    ),
                    boxed(
                        Style {
                            width: FULL,
                            horizontal: true,
                            center: true,
                            ..Style::default()
                        },
                        vec![
                            icon_button(
                                c,
                                b,
                                Action::RemoveDraft,
                                "delete",
                                l.t("Delete", "Șterge"),
                                false,
                            ),
                            grow(Vec::new()),
                            icon_button(
                                c,
                                b,
                                Action::SendDraft,
                                "message-send",
                                l.t("Send", "Trimite"),
                                true,
                            ),
                        ],
                    ),
                ],
            ));
        }
        // The chosen photo itself, before it is sent.
        let picture = self
            .draft_picture
            .as_ref()
            .filter(|shown| shown.of == draft.stage.stage)
            .and_then(|shown| shown.thumb.as_ref())
            .map(|thumb| {
                let (width, height) = shown(thumb);
                Node::Picture {
                    id: thumb.id.clone(),
                    width,
                    height,
                    radius: 12,
                    digest: None,
                }
            });
        let row = boxed(
            Style {
                width: FULL,
                horizontal: true,
                gap: 8,
                padding: 8,
                background: Colour::Raised,
                radius: [12; 4],
                ..Style::default()
            },
            vec![
                grow(vec![
                    labelled(
                        "attachment",
                        Node::Text {
                            text: short(&draft.name, 120),
                            tone: Tone::Body,
                            lines: Some(1),
                            size: None,
                        },
                    ),
                    Node::Text {
                        text: format!(
                            "{} · {}",
                            draft.selected.title(l),
                            file_size(l, draft.stage.bytes as u32)
                        ),
                        tone: Tone::Muted,
                        lines: Some(1),
                        size: Some(12),
                    },
                ]),
                b.button_at(
                    "send-draft".into(),
                    Action::SendDraft,
                    if draft.media_type.is_some() {
                        l.t("Send", "Trimite")
                    } else {
                        l.t("Send as file", "Trimite ca fișier")
                    },
                ),
                b.button_at("remove-draft".into(), Action::RemoveDraft, "✕"),
            ],
        );
        Some(match picture {
            Some(picture) => boxed(
                Style {
                    width: FULL,
                    gap: 8,
                    ..Style::default()
                },
                vec![picture, row],
            ),
            None => row,
        })
    }

    /// The content of what is open over the page; none when it no longer applies.
    fn sheet(&self, c: Context<'_>, b: &mut Bindings) -> Option<Vec<Node>> {
        let l = c.locale;
        let room = self.room();
        let row = |key: &str| {
            c.engine
                .data
                .rows
                .get(key)
                .filter(|r| Some(&r.room) == room)
        };
        let close = |b: &mut Bindings, label: &'static str| {
            b.button_at("close".into(), Action::Close, label)
        };
        let mut items = Vec::new();
        match &self.overlay {
            Overlay::None => {}
            Overlay::NewChat => {
                items.push(text(l.t("New chat", "Conversație nouă"), Tone::Title));
                items.push(text(
                    l.t(
                        "Choose a paired device to talk to.",
                        "Alege un dispozitiv asociat cu care să vorbești.",
                    ),
                    Tone::Muted,
                ));
                if c.directory.is_empty() {
                    items.push(text(l.t("Pair a device and allow Chat to see and send to it in Settings.","Asociază un dispozitiv și permite extensiei Chat să îl vadă și să îi trimită date în Setări."),Tone::Muted));
                }
                items.push(chooser(b, None));
                items.push(actions(vec![
                    b.button(
                        Action::Show(Overlay::NewGroup),
                        l.t("New group", "Grup nou"),
                    ),
                    close(b, l.t("Cancel", "Anulează")),
                ]));
            }
            Overlay::NewGroup => {
                items.push(text(l.t("New group", "Grup nou"), Tone::Title));
                items.push(text(l.t("Choose two to seven devices. Each receives the group’s name and members; pair with each member for replies both ways.","Alege între două și șapte dispozitive. Fiecare primește numele grupului și membrii; asociază-te cu fiecare membru pentru răspunsuri în ambele sensuri."),Tone::Muted));
                if self.group.len() < 7 {
                    items.push(chooser(b, None));
                }
                if !self.group.is_empty() {
                    let chips = self
                        .group
                        .iter()
                        .map(|member| {
                            b.button(
                                Action::RemoveMember(member.clone()),
                                format!("{} ✕", c.peer_name(member)),
                            )
                        })
                        .collect();
                    items.push(actions(chips));
                }
                if self.group.len() >= 2 {
                    items.push(text(
                        l.t(
                            "Name the group and press Enter to create it.",
                            "Numește grupul și apasă Enter pentru a-l crea.",
                        ),
                        Tone::Muted,
                    ));
                    items.push(self.field(
                        b,
                        Action::GroupTitle,
                        l.t("Group name", "Numele grupului"),
                        FULL,
                    ));
                }
                items.push(actions(vec![close(b, l.t("Cancel", "Anulează"))]));
            }
            Overlay::Attach => {
                room?;
                items.push(text(l.t("Attach", "Atașează"), Tone::Title));
                if c.features.select {
                    items.push(actions(vec![
                        b.button(Action::PickFile(Media::Photo), l.t("Photo", "Fotografie")),
                        b.button(Action::PickFile(Media::Audio), l.t("Audio", "Audio")),
                        b.button(Action::PickFile(Media::File), l.t("File", "Fișier")),
                    ]));
                } else {
                    items.push(text(l.t("File selection is off for Chat. Turn on “Select a file” for Chat in Settings › Extensions.","Selectarea fișierelor este oprită pentru Chat. Pornește „Selectează un fișier” pentru Chat în Setări › Extensii."),Tone::Body));
                }
                items.push(actions(vec![close(b, l.t("Cancel", "Anulează"))]));
            }
            Overlay::Options => {
                let room = room?;
                items.push(text(c.room_name(room), Tone::Title));
                items.push(column(vec![
                    b.button(
                        Action::Show(Overlay::Members),
                        l.t("Devices and delivery", "Dispozitive și livrare"),
                    ),
                    b.button(
                        Action::Show(Overlay::Confirm(Confirmation::Clear { remote: false })),
                        l.t(
                            "Clear chat on this device…",
                            "Golește conversația pe acest dispozitiv…",
                        ),
                    ),
                    b.button(
                        Action::Show(Overlay::Confirm(Confirmation::Clear { remote: true })),
                        l.t(
                            "Clear and ask the others…",
                            "Golește și cere și celorlalți…",
                        ),
                    ),
                    b.button(
                        Action::Show(Overlay::Confirm(Confirmation::Purge)),
                        l.t("Delete everything here…", "Șterge tot de aici…"),
                    ),
                ]));
                items.push(actions(vec![close(b, l.t("Close", "Închide"))]));
            }
            Overlay::Members => {
                let room = room?;
                items.push(text(
                    l.t("Devices and delivery", "Dispozitive și livrare"),
                    Tone::Title,
                ));
                let mut add = |label: String, local: Option<ScopeId>, resend: Option<ScopeId>| {
                    items.push(text(
                        format!("{label} · {}", c.presence(local.as_ref())),
                        Tone::Body,
                    ));
                    if let Some(id) = local
                        && c.online(&id)
                    {
                        let mut row = vec![b.button(
                            Action::Retry(id.clone()),
                            l.t("Retry pending work", "Reîncearcă ce așteaptă"),
                        )];
                        if let Some(member) = resend {
                            row.push(b.button(
                                Action::Offer(member),
                                l.t("Offer the group again", "Oferă din nou grupul"),
                            ));
                        }
                        items.push(actions(row));
                    }
                };
                match room {
                    RoomKey::Direct(id) => add(c.peer_name(id), Some(id.clone()), None),
                    RoomKey::Group(id) => {
                        if let Some(state) = c.engine.data.rooms.get(room)
                            && let Some(group) = &state.group
                        {
                            for member in group
                                .roster
                                .iter()
                                .filter(|m| Some(&m.id) != state.member.as_ref())
                            {
                                add(
                                    member.label.clone(),
                                    c.engine.group_peer(id, &member.id).cloned(),
                                    (state.member.as_ref() == Some(&group.creator))
                                        .then_some(member.id.clone()),
                                );
                            }
                        }
                    }
                }
                items.push(text(l.t("Choose a device to open a connection for this page in Private mode.","Alege un dispozitiv pentru a deschide o conexiune pentru această pagină în modul privat."),Tone::Muted));
                items.push(chooser(
                    b,
                    match room {
                        RoomKey::Direct(id) => c.directory.get(id).map(|p| p.alias.clone()),
                        RoomKey::Group(_) => None,
                    },
                ));
                items.push(actions(vec![close(b, l.t("Close", "Închide"))]));
            }
            Overlay::Menu(key) => {
                let row = row(key)?;
                items.push(profile(c, b, room?, row));
                if row.body.is_some() {
                    let mine = row.reactions.get("self").and_then(|r| r.emoji.as_deref());
                    let emoji = EMOJI
                        .iter()
                        .map(|emoji| {
                            b.button(
                                Action::React(
                                    key.clone(),
                                    (mine != Some(*emoji)).then(|| (*emoji).to_owned()),
                                ),
                                if mine == Some(*emoji) {
                                    format!("[{emoji}]")
                                } else {
                                    (*emoji).to_owned()
                                },
                            )
                        })
                        .collect();
                    items.push(actions(emoji));
                    items.extend(reacted(c, room?, row));
                    if let Some(message) = row.body.as_ref().and_then(|body| body.text.as_ref()) {
                        items.push(Node::Text {
                            text: one_line(message),
                            tone: Tone::Muted,
                            lines: Some(2),
                            size: Some(13),
                        });
                    }
                }
                let mut menu = Vec::new();
                if row.body.is_some() {
                    menu.push(b.button(Action::Reply(key.clone()), l.t("Reply", "Răspunde")));
                }
                menu.push(b.button(
                    Action::Show(Overlay::Detail(key.clone())),
                    l.t("Details", "Detalii"),
                ));
                if row
                    .body
                    .as_ref()
                    .is_some_and(|body| !body.attachments.is_empty())
                    && row.asset.is_some()
                {
                    menu.push(if c.features.export {
                        b.button(Action::Save(key.clone()), l.t("Save file…", "Salvează fișierul…"))
                    } else {
                        text(l.t("Saving files is off for Chat. Turn on “Save a file” for Chat in Settings › Extensions.","Salvarea fișierelor este oprită pentru Chat. Pornește „Salvează un fișier” pentru Chat în Setări › Extensii."),Tone::Muted)
                    });
                }
                if room.is_some_and(|room| !c.engine.options(room, &row.key).is_empty()) {
                    menu.push(b.button(
                        Action::Show(Overlay::Delete(key.clone())),
                        l.t("Delete…", "Șterge…"),
                    ));
                }
                items.push(column(menu));
                items.push(actions(vec![close(b, l.t("Cancel", "Anulează"))]));
            }
            Overlay::Detail(key) => {
                let room = room?;
                let row = row(key)?;
                items.push(profile(c, b, room, row));
                items.push(text(
                    l.t("Message details", "Detaliile mesajului"),
                    Tone::Title,
                ));
                for line in details(c, room, row) {
                    items.push(text(line, Tone::Body));
                }
                items.push(actions(vec![
                    b.button(
                        Action::Show(Overlay::Menu(key.clone())),
                        l.t("Back", "Înapoi"),
                    ),
                    close(b, l.t("Close", "Închide")),
                ]));
            }
            Overlay::Photo(key) => {
                let row = row(key)?;
                // As wide as the sheet's inside: the panel's own on a wide page, the page less its padding on a narrow one.
                let width = if self.wide() {
                    460 - 32
                } else {
                    self.width
                        .map_or(360, |width| (width / 1000).saturating_sub(32))
                        .clamp(120, u32::from(u16::MAX)) as u16
                };
                match self
                    .view_picture
                    .as_ref()
                    .filter(|shown| &shown.of == key)
                    .and_then(|shown| shown.thumb.as_ref())
                {
                    Some(thumb) => {
                        // As tall as the page leaves above the sheet's padding and its buttons, never cut off below.
                        let tallest = self.height.map_or(VIEW_HEIGHT, |height| {
                            (height / 1000)
                                .saturating_sub(PHOTO_CHROME)
                                .clamp(80, u32::from(VIEW_HEIGHT))
                                as u16
                        });
                        let (width, height) = shown_within(thumb, width, tallest);
                        items.push(Node::Picture {
                            id: thumb.id.clone(),
                            width,
                            height,
                            radius: 12,
                            digest: None,
                        });
                    }
                    None => items.push(text(
                        l.t(
                            "This photo can't be shown.",
                            "Fotografia nu poate fi afișată.",
                        ),
                        Tone::Muted,
                    )),
                }
                let mut buttons = Vec::new();
                if c.features.export && row.asset.is_some() {
                    buttons.push(b.button(
                        Action::Save(key.clone()),
                        l.t("Save file…", "Salvează fișierul…"),
                    ));
                }
                buttons.push(close(b, l.t("Close", "Închide")));
                items.push(actions(buttons));
            }
            Overlay::Delete(key) => {
                let room = room?;
                let row = row(key)?;
                let options = c.engine.options(room, &row.key);
                if options.is_empty() {
                    items.push(text(l.t("Delete message", "Șterge mesajul"), Tone::Title));
                    if row.delivery.values().any(|d| *d == Delivery::Unconfirmed) {
                        items.push(text(l.t("A copy may have left this device. Check delivery before deleting this message.","Este posibil ca o copie să fi plecat de pe acest dispozitiv. Verifică livrarea înainte să ștergi mesajul."),Tone::Muted));
                    }
                    items.push(actions(vec![
                        b.button(
                            Action::Show(Overlay::Members),
                            l.t("Check delivery", "Verifică livrarea"),
                        ),
                        close(b, l.t("Cancel", "Anulează")),
                    ]));
                } else {
                    items.push(text(l.t("Delete message?", "Ștergi mesajul?"), Tone::Title));
                    for choice in options {
                        items.push(text(delete_meaning(l, *choice), Tone::Muted));
                    }
                    let mut buttons: Vec<_> = options
                        .iter()
                        .map(|choice| {
                            b.button(
                                Action::Delete(row.key.clone(), *choice),
                                delete_words(l, *choice),
                            )
                        })
                        .collect();
                    buttons.push(close(b, l.t("Cancel", "Anulează")));
                    items.push(actions(buttons));
                }
            }
            Overlay::Confirm(choice) => {
                let room = room?;
                items.push(text(c.room_name(room), Tone::Title));
                let (instruction,button)=match choice {
                    Confirmation::Clear {remote:false}=>(l.t("Clear the history and unsent items on this device. The conversation stays in your list.","Golește istoricul și elementele netrimise de pe acest dispozitiv. Conversația rămâne în listă."),l.t("Clear here","Golește aici")),
                    Confirmation::Clear {remote:true}=>(l.t("Clear here and ask the other devices to remove the shared history. They can decline; their newer or unsent messages stay.","Golește aici și cere celorlalte dispozitive să elimine istoricul comun. Ele pot refuza; mesajele lor mai noi sau netrimise rămân."),l.t("Clear and ask","Golește și cere")),
                    Confirmation::Purge=>(l.t("Remove the conversation, saved attachments and all pending work on this device. Copies on other devices or already saved elsewhere stay there.","Elimină conversația, atașamentele salvate și toate operațiunile în așteptare de pe acest dispozitiv. Copiile de pe alte dispozitive sau deja salvate în altă parte rămân."),l.t("Delete everything","Șterge tot")),
                };
                items.push(text(instruction, Tone::Body));
                items.push(actions(vec![
                    close(b, l.t("Cancel", "Anulează")),
                    b.button(Action::Confirm, button),
                ]));
            }
        }
        Some(items)
    }

    fn field(&self, b: &mut Bindings, action: Action, placeholder: &str, width: Length) -> Node {
        let id = b.id(action);
        Node::Box {
            id: Some(format!("field-{:x}-{id}", self.field)),
            style: Style {
                width,
                ..Style::default()
            },
            observe: Vec::new(),
            children: vec![Node::Field {
                id,
                placeholder: placeholder.into(),
                value: short(&self.text, elements::MAX_FIELD),
                height: None,
            }],
        }
    }
}

/// A stable id for a message: the room, the immutable roster slot of its author, and its id.
/// Different authors may choose the same message epoch and sequence; two 256-bit scopes fit the
/// element name limit when encoded instead of kept as hex.
fn anchor(c: Context<'_>, room: &RoomKey, row: &Row) -> String {
    let author = row
        .member
        .as_ref()
        .and_then(|member| {
            c.engine
                .data
                .rooms
                .get(room)?
                .group
                .as_ref()?
                .roster
                .iter()
                .position(|entry| &entry.id == member)
        })
        .unwrap_or(if row.mine { 8 } else { 9 });
    let (kind, scope) = match room {
        RoomKey::Direct(id) => ('d', id),
        RoomKey::Group(id) => ('g', id),
    };
    let mut anchor = format!("m{kind}{author}-");
    push_scope(&mut anchor, scope);
    anchor.push('.');
    push_scope(&mut anchor, &row.key.epoch);
    let _ = write!(&mut anchor, "-{:x}", row.key.no);
    anchor
}

/// The over-the-page layer: a dimmed area that closes it when tapped, and the sheet. On a narrow
/// page the sheet rises from the bottom edge; on a wide one it stands in the middle.
fn over(b: &mut Bindings, observe: Vec<Observe>, wide: bool, sheet: Vec<Node>) -> Node {
    let mut scrim = |name: &str, width: Length, height: Length, background: Colour| Node::Box {
        id: Some(b.bind(format!("scrim-{name}"), Action::Close)),
        style: Style {
            width,
            height,
            background,
            ..Style::default()
        },
        observe: observe.clone(),
        children: Vec::new(),
    };
    let panel = |width: Length, radius: [u16; 4]| {
        boxed(
            Style {
                width,
                padding: 16,
                gap: 10,
                radius,
                background: Colour::Raised,
                border: true,
                reveal: true,
                ..Style::default()
            },
            sheet,
        )
    };
    if wide {
        // Columns beside the panel, and above and below it inside its own column.
        // The column itself is dimmed, so the panel's round corners show the dimmed page.
        let middle = vec![
            scrim("top", FULL, Length::Flex(1), Colour::Transparent),
            panel(FULL, [16; 4]),
            scrim("bottom", FULL, Length::Flex(1), Colour::Transparent),
        ];
        boxed(
            Style {
                width: FULL,
                height: FULL,
                horizontal: true,
                overlay: true,
                ..Style::default()
            },
            vec![
                scrim("left", Length::Flex(1), FULL, Colour::Scrim),
                boxed(
                    Style {
                        width: Length::Px(460),
                        height: FULL,
                        background: Colour::Scrim,
                        ..Style::default()
                    },
                    middle,
                ),
                scrim("right", Length::Flex(1), FULL, Colour::Scrim),
            ],
        )
    } else {
        boxed(
            Style {
                width: FULL,
                height: FULL,
                overlay: true,
                ..Style::default()
            },
            vec![
                scrim("top", FULL, Length::Flex(1), Colour::Scrim),
                panel(FULL, [16, 16, 0, 0]),
            ],
        )
    }
}

/// The paired device a conversation is with, when it is with one.
fn direct(room: &RoomKey) -> Option<&ScopeId> {
    match room {
        RoomKey::Direct(id) => Some(id),
        RoomKey::Group(_) => None,
    }
}

/// Who wrote `row`: the paired device it came from, when that is known, and the name to show. This device's own messages
/// carry the name the group knows it by, and "You" where there is no group.
fn writer(c: Context<'_>, room: &RoomKey, row: &Row) -> (Option<ScopeId>, String) {
    let l = c.locale;
    if row.mine {
        let own = c
            .engine
            .data
            .rooms
            .get(room)
            .and_then(|r| r.member.as_ref())
            .map(|member| c.recipient(room, member));
        return (None, own.unwrap_or_else(|| l.t("You", "Tu").into()));
    }
    match (room, &row.member) {
        (RoomKey::Direct(id), _) => (Some(id.clone()), c.room_name(room)),
        (RoomKey::Group(group), Some(member)) => (
            c.directory
                .keys()
                .find(|peer| c.engine.group_member(group, peer) == Some(member))
                .cloned(),
            c.recipient(room, member),
        ),
        (RoomKey::Group(_), None) => (None, l.t("Member", "Membru").into()),
    }
}

/// Who wrote a message, the way a profile shows a person: a band of colour with their picture over it, their name, the
/// device's own name when a group knows it by another, whether it can be reached, the group it is open in, how many groups
/// it shares with this device, and a way to write to it alone.
fn profile(c: Context<'_>, b: &mut Bindings, room: &RoomKey, row: &Row) -> Node {
    const BAND: u16 = 56;
    const PICTURE: u16 = 80;
    const RING: u16 = 6;
    let l = c.locale;
    let (peer, name) = writer(c, room, row);
    let line = |text: String, size: u8| Node::Text {
        text: short(&text, 80),
        tone: Tone::Muted,
        lines: Some(1),
        size: Some(size),
    };
    let mut lines = vec![Node::Text {
        text: short(&name, 80),
        tone: Tone::Title,
        lines: Some(1),
        size: Some(20),
    }];
    if let Some(device) = peer.as_ref().and_then(|id| c.directory.get(id))
        && device.name != name
    {
        lines.push(line(device.name.clone(), 14));
    }
    if row.mine {
        let you = l.t("You", "Tu");
        if name != you {
            lines.push(line(you.into(), 13));
        }
    } else {
        lines.push(line(c.presence(peer.as_ref()).to_owned(), 13));
    }
    if let RoomKey::Group(_) = room {
        lines.push(line(
            format!("{} {}", l.t("In", "În"), c.room_name(room)),
            13,
        ));
    }
    if let Some(id) = &peer {
        let shared = c
            .engine
            .data
            .rooms
            .keys()
            .filter(|key| matches!(key, RoomKey::Group(group) if c.engine.group_member(group, id).is_some()))
            .count();
        if shared != 0 {
            lines.push(line(
                match (l, shared) {
                    (Locale::En, 1) => "1 group in common".into(),
                    (Locale::En, n) => format!("{n} groups in common"),
                    (Locale::Ro, 1) => "1 grup în comun".into(),
                    (Locale::Ro, n) if n < 20 => format!("{n} grupuri în comun"),
                    (Locale::Ro, n) => format!("{n} de grupuri în comun"),
                },
                13,
            ));
        }
        if c.directory.contains_key(id) && direct(room) != Some(id) {
            lines.push(spacer(4));
            lines.push(actions(vec![b.button(
                Action::Open(RoomKey::Direct(id.clone())),
                l.t("Write privately", "Scrie în privat"),
            )]));
        }
    }
    let head = boxed(
        Style {
            width: FULL,
            height: Length::Px(BAND + PICTURE / 2 + RING),
            ..Style::default()
        },
        vec![
            boxed(
                Style {
                    width: FULL,
                    height: Length::Px(BAND),
                    radius: [12, 12, 0, 0],
                    background: Colour::Accent,
                    ..Style::default()
                },
                Vec::new(),
            ),
            // The picture stands half over the band, in a ring of the card's own colour.
            boxed(
                Style {
                    width: Length::Px(PICTURE + 2 * RING),
                    height: Length::Px(PICTURE + 2 * RING),
                    radius: [PICTURE / 2 + RING; 4],
                    background: Colour::Surface,
                    center: true,
                    at: Some([12_000, i32::from(BAND - PICTURE / 2 - RING) * 1000]),
                    ..Style::default()
                },
                vec![if row.mine {
                    Node::Avatar {
                        device: peersus_api::elements::THIS_DEVICE.into(),
                        name: short(&name, 80),
                        size: PICTURE,
                    }
                } else {
                    portrait(c, peer.as_ref(), &name, PICTURE)
                }],
            ),
        ],
    );
    boxed(
        Style {
            width: FULL,
            radius: [12; 4],
            background: Colour::Surface,
            border: true,
            clip: true,
            ..Style::default()
        },
        vec![
            head,
            boxed(
                Style {
                    width: FULL,
                    padding: 12,
                    gap: 4,
                    ..Style::default()
                },
                lines,
            ),
        ],
    )
}

/// Who reacted to `row` and with what, one line each: "❤️ Ana".
fn reacted(c: Context<'_>, room: &RoomKey, row: &Row) -> Vec<Node> {
    let l = c.locale;
    row.reactions
        .iter()
        .filter_map(|(actor, reaction)| {
            let emoji = reaction.emoji.as_deref()?;
            let name = match actor.as_str() {
                "self" => l.t("You", "Tu").into(),
                "other" => c.room_name(room),
                id => ScopeId::try_from(id.to_owned()).map_or_else(
                    |_| l.t("Member", "Membru").into(),
                    |id| c.recipient(room, &id),
                ),
            };
            Some(Node::Text {
                text: format!("{emoji} {}", short(&name, 80)),
                tone: Tone::Body,
                lines: Some(1),
                size: Some(14),
            })
        })
        .collect()
}

/// A device's picture, or initials, with a dot for whether a paired device can be reached now.
fn portrait(c: Context<'_>, peer: Option<&ScopeId>, name: &str, size: u16) -> Node {
    let picture = match peer.and_then(|id| c.directory.get(id)) {
        Some(peer) => Node::Avatar {
            device: peer.alias.clone(),
            name: short(name, 80),
            size,
        },
        _ => boxed(
            Style {
                width: Length::Px(size),
                height: Length::Px(size),
                radius: [size / 2; 4],
                background: Colour::Accent,
                center: true,
                ..Style::default()
            },
            vec![Node::Text {
                text: initials(name),
                tone: Tone::Title,
                lines: Some(1),
                size: Some(u8::try_from(size / 3).unwrap_or(14).max(10)),
            }],
        ),
    };
    let mut children = vec![picture];
    if let Some(id) = peer {
        let dot = size / 4;
        children.push(boxed(
            Style {
                width: Length::Px(dot),
                height: Length::Px(dot),
                radius: [dot / 2; 4],
                background: if c.online(id) {
                    Colour::Success
                } else {
                    Colour::Muted
                },
                at: Some([i32::from(size - dot) * 1000, i32::from(size - dot) * 1000]),
                ..Style::default()
            },
            Vec::new(),
        ));
    }
    boxed(
        Style {
            width: Length::Px(size),
            height: Length::Px(size),
            ..Style::default()
        },
        children,
    )
}

fn initials(name: &str) -> String {
    let letters: String = name
        .split_whitespace()
        .filter_map(|word| word.chars().find(|c| c.is_alphanumeric()))
        .take(2)
        .flat_map(char::to_uppercase)
        .collect();
    if letters.is_empty() {
        "#".into()
    } else {
        letters
    }
}

/// One line about a message, as the conversation list shows it.
fn summary(l: Locale, row: &Row) -> String {
    match &row.body {
        None => l.t("Message deleted", "Mesaj șters").into(),
        Some(body) => body
            .text
            .as_deref()
            .filter(|text| !text.trim().is_empty())
            .map(one_line)
            .or_else(|| {
                body.attachments.first().map(|a| match recorded_length(a) {
                    Some(length) => {
                        format!("{} {}", l.t("Voice message", "Mesaj vocal"), clock(length))
                    }
                    None => short(&a.name, 80),
                })
            })
            .unwrap_or_default(),
    }
}

/// The message another one answers, as a small box: who wrote it and a line of it. One this device no longer holds, deleted
/// or never received, says so.
fn quote(c: Context<'_>, room: &RoomKey, answered: Option<&Row>) -> Node {
    let l = c.locale;
    let mut lines = Vec::new();
    let line = match answered {
        Some(row) => {
            lines.push(Node::Text {
                text: short(&writer(c, room, row).1, 80),
                tone: Tone::Title,
                lines: Some(1),
                size: Some(12),
            });
            summary(l, row)
        }
        None => l
            .t(
                "The original message is not here",
                "Mesajul original nu mai este aici",
            )
            .into(),
    };
    lines.push(Node::Text {
        text: line,
        tone: Tone::Muted,
        lines: Some(2),
        size: Some(12),
    });
    boxed(
        Style {
            width: FULL,
            padding: 8,
            gap: 2,
            radius: [10; 4],
            background: Colour::Surface,
            ..Style::default()
        },
        lines,
    )
}

/// How far my message got, as the name of the application's picture of it: waiting, sent, seen, or not confirmed.
fn ticks(row: &Row) -> &'static str {
    let states: Vec<_> = row.delivery.values().copied().collect();
    if states.is_empty() {
        "waiting"
    } else if states
        .iter()
        .any(|d| matches!(d, Delivery::Unconfirmed | Delivery::Rejected))
    {
        "failed"
    } else if states.contains(&Delivery::Queued) {
        "waiting"
    } else if states.iter().all(|d| *d == Delivery::Seen) {
        "seen"
    } else {
        "sent"
    }
}

/// `node` after one of the application's pictures, on one line.
fn labelled(icon: &str, node: Node) -> Node {
    boxed(
        Style {
            horizontal: true,
            center: true,
            gap: 6,
            ..Style::default()
        },
        vec![
            Node::Icon {
                name: icon.into(),
                size: 16,
            },
            node,
        ],
    )
}

/// When a message was written or received, as this device's clock said: the time of day for today, the day and month for
/// another day this year, and the whole date for another year. Without today's date (no reading yet), the whole date.
fn when(at: WallTime, now: Option<WallTime>) -> String {
    let (day, minute) = at.local_day_and_minute();
    let clock = format!("{:02}:{:02}", minute / 60, minute % 60);
    let (year, month, date) = civil(day);
    match now.map(|now| now.local_day_and_minute().0) {
        Some(today) if today == day => clock,
        Some(today) if civil(today).0 == year => format!("{date:02}.{month:02} {clock}"),
        _ => format!("{date:02}.{month:02}.{year} {clock}"),
    }
}

/// The civil date (year, month, day) of a count of days since 1970-01-01, in the proleptic Gregorian calendar
/// (H. Hinnant's `civil_from_days`).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let date = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (yoe + era * 400 + i64::from(month <= 2), month, date)
}

fn reactions(row: &Row) -> Option<String> {
    let counts: Vec<_> = EMOJI
        .iter()
        .filter_map(|emoji| {
            let count = row
                .reactions
                .values()
                .filter(|r| r.emoji.as_deref() == Some(*emoji))
                .count();
            (count != 0).then(|| format!("{emoji} {count}"))
        })
        .collect();
    (!counts.is_empty()).then(|| counts.join("  "))
}

/// What the bubble, and the writer's profile above these lines, do not already show.
fn details(c: Context<'_>, room: &RoomKey, row: &Row) -> Vec<String> {
    let l = c.locale;
    let mut lines = Vec::new();
    if row.mine {
        if row.delivery.is_empty() {
            lines.push(l.t("Not sent yet", "Netrimis încă").into());
        }
        for (id, state) in &row.delivery {
            lines.push(format!(
                "{}: {}",
                c.recipient(room, id),
                delivery(l, *state)
            ));
        }
    } else {
        lines.push(
            if row.seen {
                l.t("You have seen it", "L-ai văzut")
            } else {
                l.t("Not seen yet", "Nevăzut încă")
            }
            .into(),
        );
    }
    if let Some(body) = &row.body {
        for attachment in &body.attachments {
            lines.push(format!(
                "{} · {} · {} {}",
                short(&attachment.name, 120),
                attachment.media_type,
                attachment.bytes,
                l.t("bytes", "octeți")
            ));
            lines.push(format!(
                "SHA-256 {}…",
                attachment.sha256.as_str().get(..16).unwrap_or_default()
            ));
            lines.push(
                if row.asset.is_some() {
                    l.t(
                        "A private copy is on this device",
                        "O copie privată este pe acest dispozitiv",
                    )
                } else {
                    l.t("No copy on this device", "Nicio copie pe acest dispozitiv")
                }
                .into(),
            );
        }
    }
    for (who, reaction) in &row.reactions {
        if let Some(emoji) = &reaction.emoji {
            let name = match who.as_str() {
                "self" => l.t("You", "Tu").into(),
                "other" => match room {
                    RoomKey::Direct(id) => c.peer_name(id),
                    RoomKey::Group(_) => l.t("Member", "Membru").into(),
                },
                member => ScopeId::try_from(member.to_owned()).map_or_else(
                    |_| l.t("Member", "Membru").into(),
                    |id| c.recipient(room, &id),
                ),
            };
            lines.push(format!("{name}: {emoji}"));
        }
    }
    for (id, result) in &row.removed {
        lines.push(format!(
            "{}: {}",
            c.recipient(room, id),
            removal(l, *result, false)
        ));
    }
    lines
}

/// Append the scope as unpadded base64url (43 bytes instead of 64 hex digits).
fn push_scope(id: &mut String, scope: &ScopeId) {
    const URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let nibble = |b: u8| {
        if b.is_ascii_digit() {
            b - b'0'
        } else {
            b - b'a' + 10
        }
    };
    let symbol = |bits: u8| char::from(URL[usize::from(bits)]);
    let (triples, tail) = scope.as_str().as_bytes().as_chunks::<3>();
    for &[a, b, c] in triples {
        let (a, b, c) = (nibble(a), nibble(b), nibble(c));
        id.push(symbol(a << 2 | b >> 2));
        id.push(symbol((b & 3) << 4 | c));
    }
    if let [b] = tail {
        id.push(symbol(nibble(*b) << 2));
    }
}

fn sdk_wire_size(value: &Value) -> bool {
    // Leave room for result framing and capability metadata.
    serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() <= 44 * 1024)
}
fn short(value: &str, max: usize) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect()
}
fn one_line(value: &str) -> String {
    short(&value.split_whitespace().collect::<Vec<_>>().join(" "), 160)
}
fn text(value: impl Into<String>, tone: Tone) -> Node {
    let value: String = value
        .into()
        .chars()
        .filter(|c| !c.is_control())
        .take(elements::MAX_TEXT)
        .collect();
    Node::Text {
        text: if value.trim().is_empty() {
            "—".into()
        } else {
            value
        },
        tone,
        lines: None,
        size: None,
    }
}
fn boxed(style: Style, children: Vec<Node>) -> Node {
    Node::Box {
        id: None,
        style,
        observe: Vec::new(),
        children,
    }
}
fn column(children: Vec<Node>) -> Node {
    boxed(
        Style {
            width: FULL,
            gap: 6,
            ..Style::default()
        },
        children,
    )
}
/// Buttons side by side that move under one another when the width runs out.
fn actions(children: Vec<Node>) -> Node {
    boxed(
        Style {
            width: FULL,
            horizontal: true,
            wrap: true,
            gap: 8,
            ..Style::default()
        },
        children,
    )
}
/// A column that takes the width its siblings leave.
fn grow(children: Vec<Node>) -> Node {
    boxed(
        Style {
            width: Length::Flex(1),
            gap: 2,
            ..Style::default()
        },
        children,
    )
}
fn spacer(height: u16) -> Node {
    boxed(
        Style {
            width: FULL,
            height: Length::Px(height),
            ..Style::default()
        },
        Vec::new(),
    )
}
fn pill(label: String) -> Node {
    boxed(
        Style {
            horizontal: true,
            padding: 4,
            radius: [10; 4],
            background: Colour::Raised,
            border: true,
            ..Style::default()
        },
        vec![Node::Text {
            text: short(&label, 200),
            tone: Tone::Body,
            lines: Some(1),
            size: Some(12),
        }],
    )
}
fn badge(count: usize) -> Node {
    boxed(
        Style {
            padding: 4,
            radius: [10; 4],
            background: Colour::Accent,
            center: true,
            ..Style::default()
        },
        vec![Node::Text {
            text: if count > 99 {
                "99+".into()
            } else {
                count.to_string()
            },
            tone: Tone::Body,
            lines: Some(1),
            size: Some(12),
        }],
    )
}
fn note_box(words: &str) -> Node {
    boxed(
        Style {
            width: FULL,
            padding: 10,
            radius: [10; 4],
            background: Colour::Raised,
            ..Style::default()
        },
        vec![text(words, Tone::Muted)],
    )
}
fn chooser(b: &mut Bindings, chosen: Option<String>) -> Node {
    Node::Devices {
        id: b.id(Action::Choose),
        chosen,
        pick: Pick {
            presence: vec![Presence::Online, Presence::Sleeping, Presence::Offline],
            allowed: true,
            accepts: None,
            status: None,
        },
        show: Show::default(),
        above: false,
        this_device: false,
        entries: Vec::new(),
    }
}
/// The size a photo's picture is shown at in a message: within [`SHOWN_WIDTH`] by [`SHOWN_HEIGHT`].
pub(crate) fn shown(thumb: &Thumb) -> (u16, u16) {
    shown_within(thumb, SHOWN_WIDTH, SHOWN_HEIGHT)
}
/// The size a picture is shown at within `width` by `height` logical pixels: its proportions kept, never larger than it was
/// made.
fn shown_within(thumb: &Thumb, width: u16, height: u16) -> (u16, u16) {
    let (made_width, made_height) = (
        u32::from(thumb.width.max(1)),
        u32::from(thumb.height.max(1)),
    );
    // In thousandths: the smallest of the two limits and one.
    let scale = (u32::from(width.max(1)) * 1000 / made_width)
        .min(u32::from(height.max(1)) * 1000 / made_height)
        .min(1000);
    let side = |n: u32| ((n * scale + 500) / 1000).max(1) as u16;
    (side(made_width), side(made_height))
}
fn file_size(l: Locale, bytes: u32) -> String {
    let kib = u64::from(bytes).div_ceil(1024);
    if bytes < 1024 {
        format!("{bytes} {}", l.t("bytes", "octeți"))
    } else if kib < 1024 {
        format!("{kib} KiB")
    } else {
        format!("{:.1} MiB", f64::from(bytes) / (1024. * 1024.))
    }
}
/// A round control drawn with one of the application's pictures (`icon`), doing `action` when pressed; `filled` in the accent
/// colour, like a send button. Where the application does not tell a pressed box a tap, an ordinary button with `label`.
fn icon_button(
    c: Context<'_>,
    b: &mut Bindings,
    action: Action,
    icon: &str,
    label: &str,
    filled: bool,
) -> Node {
    icon_button_at(c, b, None, action, icon, label, filled)
}

/// [`icon_button`] under a fixed `id`, for a control whose id must stay the same from one page to the next.
fn icon_button_at(
    c: Context<'_>,
    b: &mut Bindings,
    id: Option<String>,
    action: Action,
    icon: &str,
    label: &str,
    filled: bool,
) -> Node {
    if !c.features.press {
        return match id {
            Some(id) => b.button_at(id, action, label),
            None => b.button(action, label),
        };
    }
    let side = if filled { 48 } else { 40 };
    Node::Box {
        id: Some(match id {
            Some(id) => b.bind(id, action),
            None => b.id(action),
        }),
        observe: vec![Observe::Press],
        style: Style {
            width: Length::Px(side),
            height: Length::Px(side),
            radius: [side / 2; 4],
            background: if filled {
                Colour::Accent
            } else {
                Colour::Transparent
            },
            center: true,
            ..Style::default()
        },
        children: vec![Node::Icon {
            name: icon.into(),
            size: 22,
        }],
    }
}

/// A voice message as a pill in the accent colour: what leads it (stop, play or the microphone), its waveform and its time.
fn voice_pill(lead: Node, levels: &[u8], played: Option<usize>, time: String) -> Node {
    boxed(
        Style {
            width: FULL,
            horizontal: true,
            center: true,
            gap: 14,
            padding: 12,
            radius: [28; 4],
            background: Colour::Accent,
            ..Style::default()
        },
        vec![
            lead,
            spectrum(levels, played),
            Node::Text {
                text: time,
                tone: Tone::Body,
                lines: Some(1),
                size: Some(16),
            },
        ],
    )
}

/// Minutes and seconds as a recording shows them (`00:03`).
fn timer(ms: u64) -> String {
    let seconds = ms / 1000;
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

/// A time into a sound, as minutes and seconds.
fn clock(ms: u64) -> String {
    let seconds = ms / 1000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// The name a recorded voice message is sent under.
pub const VOICE_NAME: &str = "Voice message.wav";
/// Places of a waveform, in a voice message's bubble and while recording.
const BARS: usize = crate::wire::WAVE_BARS;

/// How long a voice message recorded here is, from its size: a WAV of the recording's rate, one 16-bit channel, after its
/// header. `None` for any other sound, whose length is known once it plays.
pub(crate) fn recorded_length(file: &crate::wire::Attachment) -> Option<u64> {
    use crate::sdk::audio::{RECORD_RATE, WAV_HEADER_BYTES};
    (file.name == VOICE_NAME && file.media_type == "audio/wav").then(|| {
        u64::from(file.bytes).saturating_sub(WAV_HEADER_BYTES as u64) * 1000
            / (u64::from(RECORD_RATE) * 2)
    })
}

/// A waveform as voice messages are drawn: one place a step, a bar as tall as the step was loud, the shortest where it was silent
/// (or not yet heard). `levels` are 0 to 15. The places after `played` (all when `None`) are drawn faded, so a bar fills as
/// it plays.
pub(crate) fn spectrum(levels: &[u8], played: Option<usize>) -> Node {
    // Each place is an equal share of whatever width is left, its bar a little narrower than the share: the waveform
    // shrinks with the screen instead of running out of it.
    let bars = levels
        .iter()
        .take(BARS)
        .enumerate()
        .map(|(i, &level)| {
            let faded = played.is_some_and(|played| i >= played);
            // Silence is a short bar, never a dot.
            let size = 6 + u16::from(level.min(15)) * 26 / 15;
            boxed(
                Style {
                    width: Length::Flex(1),
                    height: FULL,
                    center: true,
                    ..Style::default()
                },
                vec![boxed(
                    Style {
                        width: Length::Percent(6_000),
                        height: Length::Px(size),
                        radius: [2; 4],
                        background: Colour::Text,
                        opacity: faded.then_some(4_500),
                        ..Style::default()
                    },
                    Vec::new(),
                )],
            )
        })
        .collect();
    boxed(
        Style {
            width: Length::Flex(1),
            height: Length::Px(32),
            horizontal: true,
            center: true,
            ..Style::default()
        },
        bars,
    )
}

/// A voice message's waveform from what was measured (`wave`, hexadecimal digits), scaled so that its loudest moment is the
/// tallest bar: a quiet recording shows its shape as a loud one does. The shortest bars for a sound with none.
fn wave_levels(wave: Option<&str>) -> Vec<u8> {
    let levels: Vec<u8> = wave.map_or_else(
        || vec![0; BARS],
        |wave| {
            wave.chars()
                .map(|c| c.to_digit(16).unwrap_or(0) as u8)
                .collect()
        },
    );
    match levels.iter().copied().max() {
        Some(loudest) if loudest > 0 => levels
            .iter()
            .map(|level| (u16::from(*level) * 15).div_ceil(u16::from(loudest)) as u8)
            .collect(),
        _ => levels,
    }
}

/// What a recording's microphone hears now as an equaliser's bars: how strong each range of frequencies is (0 to 255),
/// lowest first, on the scale a waveform is drawn with.
fn equaliser(bands: &[u8]) -> Vec<u8> {
    bands
        .iter()
        .take(BARS)
        .map(|level| (u16::from(*level) * 15 / 255) as u8)
        .collect()
}
fn delivery(l: Locale, value: Delivery) -> &'static str {
    match value {
        Delivery::Queued => l.t(
            "Waiting to be sent (device away)",
            "Așteaptă să fie trimis (dispozitiv absent)",
        ),
        Delivery::Sent => l.t("Delivered", "Livrat"),
        Delivery::Seen => l.t("Seen", "Văzut"),
        Delivery::Unconfirmed => l.t("Delivery not confirmed", "Livrare neconfirmată"),
        Delivery::Rejected => l.t("Not delivered", "Nelivrat"),
    }
}
fn removal(l: Locale, value: Removal, clear: bool) -> &'static str {
    match (clear, value) {
        (true, Removal::Pending) => l.t("Clear requested", "Golire solicitată"),
        (true, Removal::Applied) => l.t("Shared history cleared", "Istoric comun golit"),
        (true, Removal::LocalOnly) => l.t(
            "Not cleared there (local only)",
            "Negolit acolo (doar local)",
        ),
        (false, Removal::Pending) => l.t("Deletion requested", "Ștergere solicitată"),
        (false, Removal::Applied) => l.t("Deleted there", "Șters acolo"),
        (false, Removal::LocalOnly) => l.t(
            "Not deleted there (local only)",
            "Neșters acolo (doar local)",
        ),
        (_, Removal::Absent) => l.t("Already absent", "Deja absent"),
        (_, Removal::Unconfirmed) => l.t("Not confirmed", "Neconfirmat"),
    }
}
fn delete_words(l: Locale, value: Delete) -> &'static str {
    match value {
        Delete::Unsent => l.t("Delete", "Șterge"),
        Delete::Local => l.t("Delete for me", "Șterge pentru mine"),
        Delete::Everyone => l.t("Delete for everyone", "Șterge pentru toți"),
    }
}
fn delete_meaning(l: Locale, value: Delete) -> &'static str {
    match value {
        Delete::Unsent => l.t(
            "This message was not sent yet. It is removed with its place in the queue.",
            "Mesajul nu a fost trimis încă. Este eliminat împreună cu locul lui din coadă.",
        ),
        Delete::Local => l.t(
            "For me: it is replaced here by a deletion mark. The other devices keep their copies.",
            "Pentru mine: este înlocuit aici cu un semn de ștergere. Celelalte dispozitive își păstrează copiile.",
        ),
        Delete::Everyone => l.t(
            "For everyone: deleted here, and the others are asked to delete theirs. Their owners can decline.",
            "Pentru toți: șters aici, iar celorlalți li se cere să îl șteargă. Proprietarii lor pot refuza.",
        ),
    }
}

#[cfg(test)]
mod when_tests {
    use super::*;

    fn at(utc: (i64, u16), offset: i32) -> WallTime {
        WallTime {
            unix_minute: utc.0 * 1440 + i64::from(utc.1),
            utc_offset_minutes: offset,
        }
    }

    #[test]
    fn a_message_shows_its_time_today_its_date_on_another_day_and_its_year_in_another_year() {
        // Day 20417 is 2025-11-25; day 0 is 1970-01-01; 2024-02-29 is day 19782.
        assert_eq!(civil(20417), (2025, 11, 25));
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(19782), (2024, 2, 29));
        let now = at((20417, 12 * 60), 120);
        assert_eq!(when(at((20417, 5 * 60 + 33), 120), Some(now)), "07:33");
        // 22:50 UTC the day before is 00:50 today at UTC+2.
        assert_eq!(when(at((20416, 22 * 60 + 50), 120), Some(now)), "00:50");
        assert_eq!(when(at((20400, 9 * 60), 120), Some(now)), "08.11 11:00");
        assert_eq!(when(at((19782, 0), 0), Some(now)), "29.02.2024 00:00");
        assert_eq!(when(at((20417, 60), 0), None), "25.11.2025 01:00");
    }
}
