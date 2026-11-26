//! What an extension shows on a page of its own: a tree of plain elements, which any frontend
//! draws its own way inside the page it gives the extension, and the typed events a person's
//! actions on them become.
//!
//! Extensions own their page's layout, appearance and interaction logic. Frontends render only
//! these generic primitives and report raw interaction events. A source may answer `null` when
//! an event changes no visual state; continuous input need not rebuild or retransmit its tree.
use crate::{
    Error, Result,
    canvas::{self, CanvasA11y, CanvasPalette},
    name,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Most elements in one tree.
pub const MAX_NODES: usize = 512;
/// Deepest nesting of rows and columns: room for a conversation's list beside its messages, a thread under a message and a
/// voice message's waveform in its bubble.
pub const MAX_DEPTH: usize = 16;
/// Longest text of one element, in characters.
pub const MAX_TEXT: usize = 2_000;
/// Longest text a field holds or sends, in characters.
pub const MAX_FIELD: usize = 1_000;
/// Largest tree, as JSON bytes: what an owner refuses before it parses.
pub const MAX_BYTES: usize = 128 * 1024;

/// Whether a value is its type's default, which the wire leaves out: what is absent reads back as the default.
fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}
/// Most shapes of one drawing.
pub const MAX_SHAPES: usize = 32;
/// Longest outline of all the shapes of one drawing together, in characters.
pub const MAX_PATH: usize = 16 * 1024;
/// Most devices one [`Node::Devices`] lists.
pub const MAX_DEVICES: usize = 64;
/// Most statuses one device of a [`Node::Devices`] shows.
pub const MAX_DEVICE_STATUSES: usize = 4;
/// Most shapes of one kept layer ([`Keep`]).
pub const MAX_LAYER_SHAPES: usize = 64;
/// Longest outline of all the shapes of one kept layer together, in characters. A `keep` is one call, and the hub carries a call
/// only whole, within `message::MAX_PAYLOAD` (48 KiB): what a shape says besides its outline, and the call's own words, count
/// too, and this leaves room for them at the most shapes ([`MAX_LAYER_SHAPES`]).
pub const MAX_LAYER_PATH: usize = 40 * 1024;
/// Longest id of a kept layer, in characters.
pub const MAX_LAYER_ID: usize = 64;
/// Most layers one extension keeps at once.
pub const MAX_KEPT_LAYERS: usize = 1024;
/// Longest outline of all the layers one extension keeps together, in characters.
pub const MAX_KEPT_PATH: usize = 16 * 1024 * 1024;
/// Most ids one `forget` names.
pub const MAX_FORGET: usize = 1024;
/// Most kept layers one drawing places.
pub const MAX_PLACED: usize = 256;
/// Largest `scale` of a placed layer, in millionths: a thousand times as large as it was kept.
pub const MAX_PLACED_SCALE: u32 = 1_000_000_000;
/// Farthest a placed layer's `at` may lie from the drawing's corner, in thousandths of a pixel, either way, on either axis.
pub const MAX_PLACED_AT: u32 = 16_384_000;
/// The field of `peersus.config` `get` that lists the optional page features the application offers, by the names below. Absent:
/// none, and an extension then writes none of what a feature allows.
pub const PAGE_FEATURES_KEY: &str = "page_features";
/// A page feature: a box may observe `pinch` ([`Observe::Pinch`]), which is sent as [`Event::Pinch`].
pub const FEATURE_PINCH: &str = "pinch";
/// A page feature: a drawing may place kept layers ([`Node::Drawing`]'s `layers`), which an extension keeps through `peersus.drawings`.
pub const FEATURE_LAYERS: &str = "layers";
/// A page feature: a clipped canvas can preview gestures on its last bitmap ([`Style::follow`]).
pub const FEATURE_FOLLOW: &str = "follow";
/// A page feature: `text` may override its tone's font size ([`Node::Text`]'s `size`).
pub const FEATURE_TEXT_SIZE: &str = "text_size";
/// A page feature: a box may observe `rotate` ([`Observe::Rotate`]), the turn of two touches, which is sent as [`Event::Rotate`].
pub const FEATURE_ROTATE: &str = "rotate";
/// A page feature: the extension can publish finished frames for [`Node::Canvas`] from its own process.
pub const FEATURE_CANVAS: &str = "canvas";
/// A box can request clipped visibility and foreground transitions.
pub const FEATURE_VISIBILITY: &str = "visibility";
/// A page feature: a box may observe `press` ([`Observe::Press`]): a tap sent as [`Event::Press`], a long press as
/// [`Event::Hold`], and a drag that starts on it moves the box that scrolls around it.
pub const FEATURE_PRESS: &str = "press";
/// A page feature: a box may observe `back` ([`Observe::Back`]): the application's Back is sent to it as [`Event::Back`].
pub const FEATURE_BACK: &str = "back";
/// The [`Node::Avatar`] `device` that shows this device's own picture, the one the person chose for it.
pub const THIS_DEVICE: &str = "this-device";
/// Optional base features. The owner also advertises [`FEATURE_CANVAS`] for its frame transport.
pub const PAGE_FEATURES: [&str; 8] = [
    FEATURE_PINCH,
    FEATURE_LAYERS,
    FEATURE_FOLLOW,
    FEATURE_TEXT_SIZE,
    FEATURE_ROTATE,
    FEATURE_VISIBILITY,
    FEATURE_PRESS,
    FEATURE_BACK,
];

/// One shape of a [`Node::Drawing`]: an outline in the path notation of SVG (`M 10 10 L 90 10 A 40 40 0 0 1 …`),
/// in the drawing's pixels, stroked and filled with the person's theme colours. Only the notation's commands,
/// numbers, spaces and commas are accepted: a shape cannot name a file, a link, a font or another document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shape {
    pub path: String,
    #[serde(default)]
    pub stroke: Colour,
    /// Width of the stroke, in tenths of a pixel; zero draws no stroke.
    #[serde(default)]
    pub line: u16,
    #[serde(default)]
    pub fill: Colour,
    /// Opacity in hundredths of a percent; absent means fully opaque.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<u16>,
    /// Round line ends and corners instead of square ones.
    #[serde(default)]
    pub round: bool,
}

impl Shape {
    fn valid(&self) -> bool {
        self.line <= 1000
            && self.opacity.is_none_or(|n| n <= 10000)
            && !self.path.is_empty()
            && self
                .path
                .bytes()
                .all(|b| b.is_ascii_digit() || b" .,-MmLlHhVvCcSsQqTtAaZz".contains(&b))
    }
}

/// The characters of outline of all of `shapes` together.
fn path_len(shapes: &[Shape]) -> usize {
    shapes.iter().map(|shape| shape.path.len()).sum()
}

/// Whether `id` names a kept layer: 1 to [`MAX_LAYER_ID`] characters of `a-z`, `0-9`, `.`, `_`, `/` and `-`.
pub fn layer_id(id: &str) -> bool {
    (1..=MAX_LAYER_ID).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._/-".contains(&b))
}

/// A kept layer placed on a [`Node::Drawing`]: the layer `id` as kept through `peersus.drawings`, enlarged by `scale` millionths (a
/// million draws it as it was kept) and moved by `at` thousandths of a pixel from the drawing's corner. A point `p` of the layer is
/// drawn at `at / 1000 + p * scale / 1_000_000` pixels of the drawing. The width of a stroke (`line`) is pixels of the drawing,
/// whatever the scale. A layer the extension has not kept, or has let go of, is drawn as nothing and is no error.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Placed {
    pub id: String,
    pub scale: u32,
    pub at: [i32; 2],
}

impl Placed {
    fn valid(&self) -> bool {
        layer_id(&self.id)
            && (1..=MAX_PLACED_SCALE).contains(&self.scale)
            && self.at.iter().all(|n| n.unsigned_abs() <= MAX_PLACED_AT)
    }
}

/// The arguments of `peersus.drawings` `keep`: the layer `id`, which the application keeps for this extension from now on, and the
/// `shapes` it is made of, in the layer's own units. Keeping an id again replaces that layer. The layer is drawn only where a drawing
/// places it ([`Placed`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Keep {
    pub id: String,
    pub shapes: Vec<Shape>,
}

impl Keep {
    /// Read and check the arguments of a `keep`.
    pub fn parse(value: &serde_json::Value) -> Result<Self> {
        let keep = Self::deserialize(value).map_err(|_| Error::InvalidManifest)?;
        keep.validate()?;
        Ok(keep)
    }

    /// Whether it is one the application keeps: a sound id and one to [`MAX_LAYER_SHAPES`] shapes, each as a drawing's, with at most
    /// [`MAX_LAYER_PATH`] characters of outline in all.
    pub fn validate(&self) -> Result<()> {
        ok(layer_id(&self.id)
            && (1..=MAX_LAYER_SHAPES).contains(&self.shapes.len())
            && self.path_len() <= MAX_LAYER_PATH
            && self.shapes.iter().all(Shape::valid))
    }

    /// The characters of outline the layer holds: what counts against [`MAX_KEPT_PATH`].
    pub fn path_len(&self) -> usize {
        path_len(&self.shapes)
    }
}

/// The arguments of `peersus.drawings` `forget`: the ids of the layers to let go of. An id that is not kept is ignored.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Forget {
    pub ids: Vec<String>,
}

impl Forget {
    /// Read and check the arguments of a `forget`.
    pub fn parse(value: &serde_json::Value) -> Result<Self> {
        let forget = Self::deserialize(value).map_err(|_| Error::InvalidManifest)?;
        forget.validate()?;
        Ok(forget)
    }

    /// Whether it is one the application obeys: at most [`MAX_FORGET`] ids, each a sound one.
    pub fn validate(&self) -> Result<()> {
        ok(self.ids.len() <= MAX_FORGET && self.ids.iter().all(|id| layer_id(id)))
    }
}

/// How a text is set: the frontend chooses the sizes and colours.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Title,
    #[default]
    Body,
    Muted,
    /// A short figure that stands out from everything round it: the number a card is about (`76%`, `14%`).
    Figure,
}

/// A bounded layout measurement. Percentages are hundredths of a percent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "unit", content = "value", rename_all = "snake_case")]
pub enum Length {
    #[default]
    Auto,
    Px(u16),
    Percent(u16),
    Flex(u16),
    /// As large as the largest of its siblings across its parent, within a parent that is as large as its content: the rows of a
    /// menu or a list all as wide as the widest, so that their backgrounds and edges line up instead of stepping with each text;
    /// the cards of a row all as tall as the tallest. A box that does not wrap and has such a child lays its children out to fit
    /// them, and a `Flex` length of one of them is then not shared out: give its siblings fixed or percentage lengths. In a
    /// box that wraps it has no effect.
    FillMinimum,
}

/// Semantic colours follow the person's theme without prescribing a widget or a use case.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Colour {
    #[default]
    Transparent,
    Surface,
    Raised,
    Text,
    Muted,
    Border,
    Scrim,
    Accent,
    Success,
    Danger,
}

/// Preview gestures on a clipped [`Node::Canvas`]'s most recent bitmap, without moving the viewport.
///
/// * `drag` moves the bitmap with the held pointer.
/// * `wheel_doubling` sets logical wheel pixels per doubling about the pointer when observing `wheel`;
///   zero disables this preview.
/// * `pinch` previews a change of scale about the pinch midpoint when observing `pinch`.
///
/// A frame's `settled` timestamp acknowledges the gestures it already shows, so the window
/// continues to apply only input with a later `at`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Follow {
    pub wheel_doubling: u16,
    pub pinch: bool,
    pub drag: bool,
}

/// Generic box layout, shared by every extension. No application-specific controls live here. On the wire only what differs
/// from the default is written.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Style {
    #[serde(skip_serializing_if = "is_default")]
    pub width: Length,
    #[serde(skip_serializing_if = "is_default")]
    pub height: Length,
    #[serde(skip_serializing_if = "is_default")]
    pub horizontal: bool,
    #[serde(skip_serializing_if = "is_default")]
    pub gap: u16,
    #[serde(skip_serializing_if = "is_default")]
    pub padding: u16,
    #[serde(skip_serializing_if = "is_default")]
    pub radius: [u16; 4],
    #[serde(skip_serializing_if = "is_default")]
    pub background: Colour,
    #[serde(skip_serializing_if = "is_default")]
    pub border: bool,
    #[serde(skip_serializing_if = "is_default")]
    pub center: bool,
    /// Place this box over its parent; children remain clipped to the extension's page.
    #[serde(skip_serializing_if = "is_default")]
    pub overlay: bool,
    /// Align children against the lower edge (for example a sheet or a toolbar). On a box that scrolls: content shorter than
    /// the box rests on its lower edge, the box opens scrolled to its end, and it follows new content while the person is at
    /// the end (a conversation, a log).
    #[serde(skip_serializing_if = "is_default")]
    pub bottom: bool,
    /// Absolute top-left offset within the parent, in thousandths of a logical pixel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<[i32; 2]>,
    #[serde(skip_serializing_if = "is_default")]
    pub clip: bool,
    /// Opacity in hundredths of a percent; absent means fully opaque.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opacity: Option<u16>,
    /// Open when it appears: from nothing to its own height, fading in, the way a menu grows out of
    /// what opened it. At once when the person turned decorative motion off.
    #[serde(skip_serializing_if = "is_default")]
    pub reveal: bool,
    /// Preview gestures on the last frame of a clipped canvas only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub follow: Option<Follow>,
    /// Its content moves up and down inside it when it is taller than the box.
    #[serde(skip_serializing_if = "is_default")]
    pub scroll: bool,
    /// In a horizontal box, children that no longer fit beside the others go on under them, whole: a grid of cards
    /// that has as many columns as the width holds.
    #[serde(skip_serializing_if = "is_default")]
    pub wrap: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointerPhase {
    Down,
    Move,
    Up,
    Cancel,
}

/// Additional raw observations a container requests. Pointer capture is requested by its id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Observe {
    Size,
    Wheel,
    Pinch,
    /// The turn of two touches ([`Event::Rotate`]). Offered as the page feature `rotate`; an application without it refuses the tree.
    Rotate,
    /// A mounted box reports visibility in the current, clipped viewport and the window's foreground state.
    Visibility,
    /// The box is pressed, never dragged: the application tells it [`Event::Press`] for a tap and [`Event::Hold`] for a long
    /// press (or the mouse's other button), and no [`Event::Pointer`]; a drag that starts on it moves the box that scrolls
    /// around it. Not with [`Observe::Pinch`] or [`Observe::Rotate`], which follow the touches themselves. Offered as the page
    /// feature `press`.
    Press,
    /// The box takes Back while it is shown: Android's Back button or gesture, or Escape on a desktop, is sent to it as
    /// [`Event::Back`] instead of the application going back. The newest such box of the page shown takes it, and only once
    /// whatever the application opened over the page is closed. A page stops taking Back by no longer observing it. Offered as
    /// the page feature `back`.
    Back,
}

/// How a device is reachable: the application's own three words, the same `peersus.peers` says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Presence {
    Online,
    Sleeping,
    Offline,
}

/// Which devices of a [`Node::Devices`] may be chosen: every condition given must hold. The others are listed too, dimmed,
/// with the application's word for why not.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Pick {
    /// How a device may be reachable to be chosen. Default: `online` only.
    pub presence: Vec<Presence>,
    /// Only a device for which the person allowed this extension everything it asks for a device (`allowed` of
    /// `peersus.peers`). Default: true.
    pub allowed: bool,
    /// Only a device that takes a stream to this service of this extension's own (`accepts` of `peersus.peers`): the same
    /// extension is there, running, and allowed this device. A device that said what it takes and left it out is refused; one
    /// that said nothing (an older device) is offered by the other conditions, as `peersus.peers` says. Another extension's
    /// service is refused on every device.
    pub accepts: Option<String>,
    /// Only a device that shows a status: any of `plugin`'s, or exactly `plugin`'s `id`. Read only with `status.watch`;
    /// without it no device is chosen, so a chooser can never tell an extension a status it may not read.
    pub status: Option<StatusMatch>,
}

impl Default for Pick {
    fn default() -> Self {
        Self {
            presence: vec![Presence::Online],
            allowed: true,
            accepts: None,
            status: None,
        }
    }
}

/// A status, as `peersus.status` names it: the extension that owns it and, optionally, which of its statuses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusMatch {
    pub plugin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// What each device of a [`Node::Devices`] shows beside its picture and name, which are always shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Show {
    /// A dot in the colour of how it is reachable. Default: true.
    pub dot: bool,
    /// The word for how it is reachable. Default: true.
    pub presence: bool,
    /// The statuses it shows, in their authors' words. Shown only with `status.watch`. Default: false.
    pub statuses: bool,
}

impl Default for Show {
    fn default() -> Self {
        Self {
            dot: true,
            presence: true,
            statuses: false,
        }
    }
}

/// Why a device of a [`Node::Devices`] cannot be chosen: the application's words, never an extension's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    /// Not reachable the way [`Pick::presence`] asks: the row says how it is.
    Presence,
    /// The person has not allowed this extension everything it asks for this device.
    NotAllowed,
    /// The device said what it takes and [`Pick::accepts`] is not among it (the extension is not there, not running or not
    /// allowed there), or the service is not this extension's own.
    NotThere,
    /// The device shows no status [`Pick::status`] names.
    NoStatus,
    /// The extension may not read statuses, so no device is chosen by one.
    StatusNotAllowed,
}

/// One device of a [`Node::Devices`], as the APPLICATION fills it in from what it knows: an extension never writes one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceEntry {
    pub device: String,
    pub name: String,
    pub presence: Presence,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub statuses: Vec<String>,
    /// Why it cannot be chosen; absent, it can.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<Refusal>,
}

/// One element. Containers hold others; every element a person acts on has an `id` of the
/// extension's choosing, which comes back in the [`Event`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case", deny_unknown_fields)]
pub enum Node {
    /// Its children one under the other.
    Column {
        #[serde(default)]
        children: Vec<Node>,
    },
    /// Its children side by side, sharing the width equally whatever their content: in a share narrower than one of its
    /// words a text is broken inside that word. Things that stand side by side and must stay whole (captions, chips, a large
    /// figure) go in a horizontal box that wraps ([`Style::wrap`]), which moves the one that no longer fits under the others.
    Row {
        #[serde(default)]
        children: Vec<Node>,
    },
    Text {
        text: String,
        #[serde(default)]
        tone: Tone,
        /// At most this many lines (1 to 20); what does not fit ends in an ellipsis. Absent: as many as it needs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lines: Option<u8>,
        /// Font size in logical pixels (8 to 64). Absent: the tone's size.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<u8>,
    },
    Button {
        id: String,
        label: String,
    },
    Toggle {
        id: String,
        label: String,
        on: bool,
    },
    /// A line of text the person writes; it is sent when they submit it.
    Field {
        id: String,
        #[serde(default)]
        placeholder: String,
        #[serde(default)]
        value: String,
        /// Optional logical-pixel height; absent preserves the frontend's normal field sizing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<u16>,
    },
    /// A thin rule between groups.
    Divider,
    /// An extension-styled container. An id requests raw pointer events, not gesture interpretation.
    Box {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "is_default")]
        style: Style,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        observe: Vec<Observe>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        children: Vec<Node>,
    },
    /// An identity image resolved from the frontend's existing, local picture cache: `device` is an alias the extension holds
    /// for a device, or [`THIS_DEVICE`] for this device's own picture. The extension never receives the picture itself.
    Avatar {
        device: String,
        name: String,
        size: u16,
    },
    /// One of the application's own pictures, by name, `size` pixels square, in the text colour: an
    /// extension shows the same picture the application shows for the same thing (for example
    /// `backspace`). A name the frontend does not know is drawn as its generic picture.
    Icon {
        name: String,
        size: u16,
    },
    /// A picture the extension keeps through `peersus.pictures` ([`crate::pictures`]), by its id, shown `width` by `height`
    /// pixels: the kept pixels cover it, keeping their proportions, and what overflows is cut off; the corners are rounded by
    /// `radius` pixels. A picture that is not kept, or not yet in the window, shows nothing at the same size. `digest` is filled
    /// in by the application, never by the extension, which is refused when it writes one.
    Picture {
        id: String,
        width: u16,
        height: u16,
        #[serde(default)]
        radius: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        digest: Option<String>,
    },
    /// A generic periodic callback, active only while this element is mounted.
    Interval {
        id: String,
        milliseconds: u16,
        child: Box<Node>,
    },
    /// A drawing `width` by `height` pixels made of the kept layers placed on it, in order, and over them `shapes`, the first under
    /// the others: what an extension draws that no other element is (a ring, a graph, a chart, a map). The application only strokes
    /// and fills the outlines; what they mean belongs to the extension. What the layers hold is kept through `peersus.drawings`
    /// and counts toward neither [`MAX_PATH`] nor [`MAX_BYTES`]: only placing them is part of the tree.
    Drawing {
        width: u16,
        height: u16,
        /// At most [`MAX_PLACED`], drawn in order under `shapes`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        layers: Vec<Placed>,
        #[serde(default)]
        shapes: Vec<Shape>,
    },
    /// An extension-rendered image plane. Its id requests raw pointer events. It uses box
    /// layout, but has no declarative children; semantic overlays do not affect its layout.
    Canvas {
        id: String,
        #[serde(default)]
        style: Style,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        observe: Vec<Observe>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        a11y: Vec<CanvasA11y>,
    },
    /// The application's one way to choose a paired device: the chosen device's picture and name in a frame, and, once
    /// pressed, the application's own menu of the devices this extension may see, each with its picture and name, those
    /// that cannot be chosen by `pick` dimmed with the application's word for why. Choosing one sends [`Event::Choose`].
    ///
    /// `chosen` is the device as `peersus.peers` names it to this extension (none: nothing is chosen, or, with
    /// `this_device`, this device). `entries` is filled in by the application from what it knows, never by the extension,
    /// which is refused when it writes one: what a device shows, how it is reachable and why it is refused stay with the
    /// person, and the extension learns only which device was chosen.
    Devices {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chosen: Option<String>,
        #[serde(default)]
        pick: Pick,
        #[serde(default)]
        show: Show,
        /// Try the menu over the frame first instead of under it (a chooser at the foot of a page). Either way the application
        /// opens it where the page has room for it, and never outside the page or the window.
        #[serde(default)]
        above: bool,
        /// This device is offered first, by its own picture and name, and is the one chosen when `chosen` is absent:
        /// for what can be about this device as well as another (its numbers, its files).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        this_device: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        entries: Vec<DeviceEntry>,
    },
}

/// What a person did on a page, sent to the page's source as `{"event": ..}`. The answer is the
/// page's whole tree as it is after it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
    /// The page was opened: what it shows first.
    Open,
    /// Visibility of one observed box in the page that emitted this event, not a read receipt.
    Visibility {
        id: String,
        visible: bool,
        foreground: bool,
    },
    Press {
        id: String,
    },
    /// A long press on a box observing [`Observe::Press`], or a press of the mouse's other button on it: never also a
    /// [`Event::Press`].
    Hold {
        id: String,
    },
    /// Back reached a box observing [`Observe::Back`].
    Back {
        id: String,
    },
    Toggle {
        id: String,
        on: bool,
    },
    Submit {
        id: String,
        text: String,
    },
    /// Coordinates are thousandths of a logical pixel; time is monotonic milliseconds.
    Pointer {
        id: String,
        phase: PointerPhase,
        x: i32,
        y: i32,
        at: u64,
    },
    Tick {
        id: String,
        at: u64,
    },
    /// Measured bounds, in the same global coordinates as pointer input.
    Size {
        id: String,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    },
    /// Canvas bounds in thousandths of a global logical pixel, device scale in thousandths,
    /// and the theme tokens for drawing in the extension's process. Sent for size, scale, or
    /// theme changes when the canvas observes `size`.
    CanvasSize {
        id: String,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        scale: u32,
        palette: CanvasPalette,
    },
    /// Two-touch midpoint in pointer units and absolute scale since the second touch (10000 = 1x). `at` is monotonic milliseconds, as
    /// that of the other gesture events.
    Pinch {
        id: String,
        x: i32,
        y: i32,
        scale: u32,
        begin: bool,
        at: u64,
    },
    /// Two-touch midpoint in pointer units and how far the line between the two touches has turned since the second touch, in
    /// hundredths of a degree, clockwise on the screen positive, continuous past a half turn. `begin` starts at zero.
    Rotate {
        id: String,
        x: i32,
        y: i32,
        turn: i32,
        begin: bool,
        at: u64,
    },
    Wheel {
        id: String,
        x: i32,
        y: i32,
        dx: i32,
        dy: i32,
        at: u64,
    },
    /// A device was chosen on the [`Node::Devices`] `id`: `device` as `peersus.peers` names it to this extension, or none
    /// for this device (offered only with `this_device`). Only a device its `pick` allowed is ever sent.
    Choose {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },
}

fn ok(valid: bool) -> Result<()> {
    valid.then_some(()).ok_or(Error::InvalidManifest)
}

/// How a device is named in a page: an alias or the owner's key for it, bounded and printable.
fn device_name(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

/// A field's text: bounded, and no control characters, but it may be empty.
fn field(value: &str) -> bool {
    value.chars().count() <= MAX_FIELD && !value.chars().any(char::is_control)
}

fn valid_observations(observe: &[Observe], has_id: bool) -> bool {
    observe.len() <= 6
        && observe
            .iter()
            .enumerate()
            .all(|(i, o)| !observe[..i].contains(o))
        && (observe.is_empty() || has_id)
        && !(observe.contains(&Observe::Press)
            && (observe.contains(&Observe::Pinch) || observe.contains(&Observe::Rotate)))
}

fn valid_style(style: &Style) -> bool {
    let measure = |length: Length| match length {
        Length::Auto | Length::FillMinimum => true,
        Length::Px(n) => n <= 8192,
        Length::Percent(n) => n <= 10000,
        Length::Flex(n) => n > 0 && n <= 1000,
    };
    measure(style.width)
        && measure(style.height)
        && style.gap <= 512
        && style.padding <= 512
        && style.radius.iter().all(|r| *r <= 8192)
        && style.opacity.is_none_or(|n| n <= 10000)
        && style
            .at
            .is_none_or(|at| at.iter().all(|n| n.unsigned_abs() <= 16_384_000))
}

impl Node {
    /// Whether the tree is one a frontend draws: within every limit, with sound ids and text.
    pub fn validate(&self) -> Result<()> {
        self.check(1, &mut 0, &mut BTreeSet::new())
    }

    fn check<'a>(
        &'a self,
        depth: usize,
        count: &mut usize,
        canvases: &mut BTreeSet<&'a str>,
    ) -> Result<()> {
        *count += 1;
        ok(*count <= MAX_NODES && depth <= MAX_DEPTH)?;
        match self {
            Node::Column { children } | Node::Row { children } => {
                for child in children {
                    child.check(depth + 1, count, canvases)?;
                }
                Ok(())
            }
            Node::Box {
                id,
                style,
                children,
                observe,
            } => {
                ok(id.as_deref().is_none_or(name::valid)
                    && valid_observations(observe, id.is_some())
                    && valid_style(style)
                    && style.follow.is_none())?;
                for child in children {
                    child.check(depth + 1, count, canvases)?;
                }
                Ok(())
            }
            Node::Canvas {
                id,
                style,
                observe,
                a11y,
            } => {
                ok(canvas::valid_id(id)
                    && valid_observations(observe, true)
                    && valid_style(style)
                    && (style.follow.is_none() || style.clip)
                    && canvases.len() < canvas::MAX_CANVASES_PER_EXTENSION
                    && canvases.insert(id.as_str())
                    && a11y.len() <= canvas::MAX_CANVAS_A11Y)?;
                let mut ids = BTreeSet::from([id.as_str()]);
                ok(a11y
                    .iter()
                    .all(|item| item.valid() && ids.insert(item.id.as_str())))
            }
            Node::Avatar {
                device,
                name: caption,
                size,
            } => ok(!device.is_empty()
                && device.len() <= 256
                && name::text(caption, MAX_TEXT)
                && (8..=256).contains(size)),
            Node::Icon { name, size } => ok(!name.is_empty()
                && name.len() <= 32
                && name.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
                && (8..=256).contains(size)),
            Node::Picture {
                id,
                width,
                height,
                radius,
                digest,
            } => ok(layer_id(id)
                && (1..=4096).contains(width)
                && (1..=4096).contains(height)
                && *radius <= 2048
                // The application fills the digest in; an extension that writes one is refused.
                && digest.is_none()),
            Node::Interval {
                id,
                milliseconds,
                child,
            } => {
                ok(name::valid(id) && (100..=60000).contains(milliseconds))?;
                child.check(depth + 1, count, canvases)
            }
            Node::Drawing {
                width,
                height,
                layers,
                shapes,
            } => ok((1..=4096).contains(width)
                && (1..=4096).contains(height)
                && layers.len() <= MAX_PLACED
                && layers.iter().all(Placed::valid)
                && shapes.len() <= MAX_SHAPES
                && path_len(shapes) <= MAX_PATH
                && shapes.iter().all(Shape::valid)),
            Node::Text {
                text, lines, size, ..
            } => ok(name::text(text, MAX_TEXT)
                && lines.is_none_or(|n| (1..=20).contains(&n))
                && size.is_none_or(|n| (8..=64).contains(&n))),
            Node::Button { id, label } | Node::Toggle { id, label, .. } => {
                ok(name::valid(id) && name::text(label, crate::contributions::MAX_LABEL))
            }
            Node::Field {
                id,
                placeholder,
                value,
                height,
            } => ok(name::valid(id)
                && field(placeholder)
                && field(value)
                && height.is_none_or(|n| (1..=8192).contains(&n))),
            Node::Divider => Ok(()),
            Node::Devices {
                id,
                chosen,
                pick,
                show: _,
                above: _,
                this_device: _,
                entries,
            } => {
                let mut presence = pick.presence.clone();
                presence.sort_unstable();
                presence.dedup();
                ok(name::valid(id)
                    && chosen.as_deref().is_none_or(device_name)
                    && !pick.presence.is_empty()
                    && presence.len() == pick.presence.len()
                    && pick.accepts.as_deref().is_none_or(name::valid)
                    && pick.status.as_ref().is_none_or(|status| {
                        name::valid(&status.plugin) && status.id.as_deref().is_none_or(name::valid)
                    })
                    // The application fills the devices in; an extension that writes one is refused.
                    && entries.is_empty())
            }
        }
    }

    /// Read and check a tree an extension answered with.
    pub fn parse(value: &serde_json::Value) -> Result<Self> {
        let node: Node =
            serde_json::from_value(value.clone()).map_err(|_| Error::InvalidManifest)?;
        node.validate()?;
        Ok(node)
    }

    /// Every element of the tree, itself first.
    pub fn visit(&self, visit: &mut impl FnMut(&Node)) {
        visit(self);
        match self {
            Node::Box { children, .. } | Node::Column { children } | Node::Row { children } => {
                for child in children {
                    child.visit(visit);
                }
            }
            Node::Interval { child, .. } => child.visit(visit),
            _ => {}
        }
    }
}

impl Event {
    pub fn validate(&self) -> Result<()> {
        match self {
            Event::Open => Ok(()),
            Event::Pinch {
                id, scale, begin, ..
            } => ok(name::valid(id) && *scale > 0 && (!begin || *scale == 10000)),
            Event::Rotate {
                id, turn, begin, ..
            } => ok(name::valid(id) && (!begin || *turn == 0)),
            Event::Press { id }
            | Event::Hold { id }
            | Event::Back { id }
            | Event::Toggle { id, .. } => ok(name::valid(id)),
            Event::Submit { id, text } => ok(name::valid(id) && field(text)),
            Event::CanvasSize {
                id,
                scale,
                width,
                height,
                ..
            } => ok(canvas::valid_id(id)
                && (1..=canvas::MAX_CANVAS_SCALE).contains(scale)
                && *width <= 16_384_000
                && *height <= 16_384_000),
            Event::Pointer { id, .. }
            | Event::Tick { id, .. }
            | Event::Size { id, .. }
            | Event::Wheel { id, .. } => ok(name::valid(id)),
            Event::Visibility { id, .. } => ok(name::valid(id)),
            Event::Choose { id, device } => {
                ok(name::valid(id) && device.as_deref().is_none_or(device_name))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn a_tree_is_read_and_its_limits_hold() {
        let tree = json!({"k":"column","children":[
            {"k":"text","text":"Hello","tone":"title"},
            {"k":"row","children":[{"k":"button","id":"go","label":"Go"},{"k":"toggle","id":"on","label":"On","on":true}]},
            {"k":"field","id":"name","placeholder":"Name"},
            {"k":"divider"}
        ]});
        assert!(Node::parse(&tree).is_ok());
        // An element no frontend knows is refused whole, not drawn in part.
        assert!(Node::parse(&json!({"k":"canvas"})).is_err());
        assert!(Node::parse(&json!({"k":"text","text":"x","colour":"red"})).is_err());
        // Too deep, too many, a bad id, text with control characters.
        let mut deep = json!({"k":"divider"});
        for _ in 0..MAX_DEPTH {
            deep = json!({"k":"column","children":[deep]});
        }
        assert!(Node::parse(&deep).is_err());
        let many: Vec<_> = (0..MAX_NODES).map(|_| json!({"k":"divider"})).collect();
        assert!(Node::parse(&json!({"k":"column","children":many})).is_err());
        assert!(Node::parse(&json!({"k":"button","id":"a b","label":"Go"})).is_err());
        assert!(Node::parse(&json!({"k":"text","text":"a\u{7}"})).is_err());
    }

    #[test]
    fn text_can_override_its_tone_size_but_only_within_the_supported_range() {
        let plain = json!({"k":"text","text":"Plain","tone":"muted"});
        assert_eq!(
            serde_json::to_value(Node::parse(&plain).unwrap()).unwrap(),
            plain
        );
        for n in [8, 26, 64] {
            let sized = json!({"k":"text","text":"Sized","tone":"muted","size":n});
            assert_eq!(
                serde_json::to_value(Node::parse(&sized).unwrap()).unwrap(),
                sized
            );
        }
        for n in [0, 7, 65, 255] {
            assert!(Node::parse(&json!({"k":"text","text":"Sized","size":n})).is_err());
        }
    }

    #[test]
    fn events_are_typed_and_checked() {
        let event: Event =
            serde_json::from_value(json!({"t":"submit","id":"name","text":""})).unwrap();
        assert!(event.validate().is_ok());
        assert!(serde_json::from_value::<Event>(json!({"t":"press","id":"go","x":1})).is_err());
        assert!(Event::Press { id: "a b".into() }.validate().is_err());
        assert!(
            Event::Submit {
                id: "a".into(),
                text: "x".repeat(MAX_FIELD + 1)
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn styled_interactive_trees_are_bounded_and_keep_raw_coordinates() {
        let tree = json!({"k":"interval","id":"clock","milliseconds":250,"child":{
            "k":"box","id":"area","style":{"width":{"unit":"percent","value":10000},
                "height":{"unit":"flex","value":1},"radius":[20,20,10,10],"background":"surface"},
            "children":[{"k":"avatar","device":"scoped","name":"Computer","size":28}]
        }});
        assert!(Node::parse(&tree).is_ok());
        let event = json!({"t":"pointer","id":"area","phase":"move","x":-1500,"y":2500,"at":123});
        let parsed: Event = serde_json::from_value(event.clone()).unwrap();
        assert!(parsed.validate().is_ok());
        assert_eq!(serde_json::to_value(parsed).unwrap(), event);
        let mut invalid = tree.clone();
        invalid["milliseconds"] = json!(0);
        assert!(Node::parse(&invalid).is_err());
        invalid = tree.clone();
        invalid["child"]["style"]["width"]["value"] = json!(10001);
        assert!(Node::parse(&invalid).is_err());
        let mut deep = tree;
        for _ in 0..MAX_DEPTH {
            deep = json!({"k":"interval","id":"clock","milliseconds":250,"child":deep});
        }
        assert!(Node::parse(&deep).is_err());
    }

    #[test]
    fn observed_boxes_require_an_id_and_bound_their_visual_plane() {
        let valid = json!({"k":"box","id":"canvas","observe":["size","wheel"],
            "style":{"at":[-16384000,16384000],"opacity":10000,"clip":true},"children":[]});
        assert!(Node::parse(&valid).is_ok());
        for (key, value) in [
            ("id", serde_json::Value::Null),
            ("observe", json!(["size", "wheel", "size"])),
        ] {
            let mut bad = valid.clone();
            bad[key] = value;
            assert!(Node::parse(&bad).is_err());
        }
        for (key, value) in [("at", json!([16384001, 0])), ("opacity", json!(10001))] {
            let mut bad = valid.clone();
            bad["style"][key] = value;
            assert!(Node::parse(&bad).is_err());
        }
    }

    #[test]
    fn size_and_relative_wheel_events_preserve_signed_global_coordinates() {
        for value in [
            json!({"t":"size","id":"canvas","x":-1500,"y":2500,"width":600000,"height":360000}),
            json!({"t":"wheel","id":"canvas","x":-1500,"y":2500,"dx":-1000,"dy":2000,"at":123}),
        ] {
            let event: Event = serde_json::from_value(value.clone()).unwrap();
            event.validate().unwrap();
            assert_eq!(serde_json::to_value(event).unwrap(), value);
            let mut bad = value;
            bad["id"] = json!("");
            assert!(
                serde_json::from_value::<Event>(bad)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
    }
    #[test]
    fn an_icon_is_named_and_bounded() {
        assert!(Node::parse(&json!({"k":"icon","name":"backspace","size":22})).is_ok());
        for bad in [
            json!({"k":"icon","name":"","size":22}),
            json!({"k":"icon","name":"Back/space","size":22}),
            json!({"k":"icon","name":"backspace","size":4}),
            json!({"k":"icon","name":"backspace","size":22,"path":"M0"}),
        ] {
            assert!(Node::parse(&bad).is_err(), "{bad}");
        }
    }
    #[test]
    fn a_length_can_take_the_widest_sibling_and_needs_no_value() {
        let tree = json!({"k":"box","style":{"width":{"unit":"fill_minimum"}},"children":[]});
        let node = Node::parse(&tree).unwrap();
        let Node::Box { style, .. } = &node else {
            panic!("a box")
        };
        assert_eq!(style.width, Length::FillMinimum);
        assert_eq!(
            serde_json::to_value(Length::FillMinimum).unwrap(),
            json!({"unit":"fill_minimum"})
        );
    }
    #[test]
    fn a_text_may_be_cut_to_a_bounded_number_of_lines() {
        let text = |lines: Value| json!({"k":"text","text":"Ryzen 7 PRO 4750U","lines":lines});
        for good in [json!(1), json!(20), Value::Null] {
            assert!(Node::parse(&text(good.clone())).is_ok(), "{good}");
        }
        for bad in [json!(0), json!(21), json!(-1), json!("2")] {
            assert!(Node::parse(&text(bad.clone())).is_err(), "{bad}");
        }
        // Absent, it is not written back, so an older window reads the same tree.
        let plain = Node::parse(&json!({"k":"text","text":"x"})).unwrap();
        assert!(serde_json::to_value(&plain).unwrap().get("lines").is_none());
    }
    #[test]
    fn fields_preserve_default_sizing_and_reject_unrenderable_heights() {
        let mut field = json!({"k":"field","id":"message","value":""});
        assert!(Node::parse(&field).is_ok());
        field["height"] = json!(48);
        assert!(Node::parse(&field).is_ok());
        for height in [0, 8193] {
            field["height"] = json!(height);
            assert!(Node::parse(&field).is_err());
        }
    }
    #[test]
    fn a_drawing_is_bounded_and_its_outlines_can_name_nothing_outside_them() {
        let drawing = json!({"k":"drawing","width":120,"height":120,"shapes":[
            {"path":"M 60 6 A 54 54 0 1 1 59.9 6","stroke":"border","line":100},
            {"path":"M60,6 A54,54 0 0 1 114,60","stroke":"accent","line":100,"round":true,"opacity":8000},
            {"path":"M0 120 L10 80 L20 90 L20 120 Z","fill":"accent","opacity":1800}
        ]});
        assert!(Node::parse(&drawing).is_ok());
        let shape = &drawing["shapes"][0];
        for (key, value) in [
            // What would leave the outline: a reference, markup, a quote, an exponent, a scale out of bounds.
            ("path", json!("url(#x)")),
            ("path", json!("M 0 0\" onload=\"x")),
            ("path", json!("M 0 0 <image href=x>")),
            ("path", json!("M 1e9 0")),
            ("path", json!("")),
            ("line", json!(1001)),
            ("opacity", json!(10001)),
            ("colour", json!("red")),
        ] {
            let mut bad = drawing.clone();
            bad["shapes"][0][key] = value;
            assert!(Node::parse(&bad).is_err(), "{bad}");
        }
        let mut bad = drawing.clone();
        bad["shapes"] = json!(vec![shape.clone(); MAX_SHAPES + 1]);
        assert!(Node::parse(&bad).is_err());
        let long = format!("M0 0{}", " L1 1".repeat(MAX_PATH / 5 + 1));
        assert!(long.len() > MAX_PATH);
        bad["shapes"] = json!([{"path": long}]);
        assert!(Node::parse(&bad).is_err());
        for size in [0, 4097] {
            let mut bad = drawing.clone();
            bad["width"] = json!(size);
            assert!(Node::parse(&bad).is_err());
        }
        // A box that scrolls and one that wraps its children are a box like any other.
        let grid = json!({"k":"box","style":{"horizontal":true,"wrap":true,"scroll":true},"children":[drawing]});
        assert!(Node::parse(&grid).is_ok());
    }

    /// An outline of exactly `len` characters, made of text an outline may hold.
    fn outline(len: usize) -> String {
        let mut path = String::from("M0 0");
        while path.len() + 5 <= len {
            path.push_str(" L1 1");
        }
        while path.len() < len {
            path.push(' ');
        }
        path
    }

    #[test]
    fn a_drawing_places_kept_layers_by_id_within_bounds_and_they_cost_it_no_outline() {
        let placed =
            json!({"id":"tile/12/3401/2210.v2","scale":1_000_000,"at":[-16_384_000,16_384_000]});
        let drawing = json!({"k":"drawing","width":600,"height":400,"layers":[placed],
            "shapes":[{"path":"M0 0 L1 1","stroke":"accent","line":10}]});
        let Node::Drawing { layers, .. } = Node::parse(&drawing).unwrap() else {
            panic!("a drawing")
        };
        assert_eq!(
            layers,
            [Placed {
                id: "tile/12/3401/2210.v2".into(),
                scale: 1_000_000,
                at: [-16_384_000, 16_384_000]
            }]
        );
        // With none, none is written back: an older window reads the same tree.
        let plain = Node::parse(&json!({"k":"drawing","width":10,"height":10})).unwrap();
        assert!(
            serde_json::to_value(&plain)
                .unwrap()
                .get("layers")
                .is_none()
        );
        let long = "a".repeat(MAX_LAYER_ID + 1);
        for (key, value) in [
            ("id", json!("")),
            ("id", json!("Tile")),
            ("id", json!("a b")),
            ("id", json!(long)),
            ("scale", json!(0)),
            ("scale", json!(MAX_PLACED_SCALE + 1)),
            ("at", json!([16_384_001, 0])),
            ("at", json!([0, -16_384_001])),
            ("name", json!("x")),
        ] {
            let mut bad = drawing.clone();
            bad["layers"][0][key] = value;
            assert!(Node::parse(&bad).is_err(), "{bad}");
        }
        let mut many = drawing.clone();
        many["layers"] = json!(vec![placed.clone(); MAX_PLACED]);
        assert!(Node::parse(&many).is_ok());
        // What the layers hold is outline kept elsewhere: the most of them leave the drawing its own whole outline.
        many["shapes"] = json!([{"path": outline(MAX_PATH)}]);
        assert!(Node::parse(&many).is_ok());
        many["layers"] = json!(vec![placed; MAX_PLACED + 1]);
        assert!(Node::parse(&many).is_err());
    }

    #[test]
    fn a_layer_is_kept_whole_or_refused_whole() {
        let keep =
            json!({"id":"tile/1/2/3","shapes":[{"path":"M0 0 L9 9","stroke":"border","line":5}]});
        let kept = Keep::parse(&keep).unwrap();
        assert_eq!((kept.id.as_str(), kept.path_len()), ("tile/1/2/3", 9));
        let long = "a".repeat(MAX_LAYER_ID + 1);
        for id in ["", "Tile", "a b", "é", "a\"b", long.as_str()] {
            let mut bad = keep.clone();
            bad["id"] = json!(id);
            assert!(Keep::parse(&bad).is_err(), "{id:?}");
        }
        let shape = keep["shapes"][0].clone();
        for bad in [
            json!({"id":"a","shapes":[]}),
            json!({"id":"a","shapes":vec![shape.clone(); MAX_LAYER_SHAPES + 1]}),
            json!({"id":"a","shapes":[{"path":"url(#x)"}]}),
            json!({"id":"a","shapes":[{"path":"M0 0","line":1001}]}),
            json!({"id":"a","shapes":[shape.clone()],"extra":1}),
            json!({"shapes":[shape.clone()]}),
            json!({"id":"a"}),
        ] {
            assert!(Keep::parse(&bad).is_err(), "{bad}");
        }
        // Exactly at the most shapes and the most outline, and one character over.
        let each = MAX_LAYER_PATH / MAX_LAYER_SHAPES;
        let shapes = |extra: usize| -> Vec<Value> {
            (0..MAX_LAYER_SHAPES)
                .map(|n| json!({"path": outline(each + usize::from(n == 0) * extra)}))
                .collect()
        };
        assert!(Keep::parse(&json!({"id":"a","shapes":shapes(0)})).is_ok());
        assert!(Keep::parse(&json!({"id":"a","shapes":shapes(1)})).is_err());
    }

    #[test]
    fn forgetting_names_sound_ids_and_at_most_so_many() {
        assert_eq!(
            Forget::parse(&json!({"ids":[]})).unwrap(),
            Forget::default()
        );
        assert!(Forget::parse(&json!({"ids":["a","b/c"]})).is_ok());
        let ids = |count: usize| (0..count).map(|n| format!("t{n}")).collect::<Vec<_>>();
        assert!(Forget::parse(&json!({"ids": ids(MAX_FORGET)})).is_ok());
        assert!(Forget::parse(&json!({"ids": ids(MAX_FORGET + 1)})).is_err());
        for bad in [
            json!({"ids":["A"]}),
            json!({"ids":[""]}),
            json!({"ids":"a"}),
            json!({}),
            json!({"ids":[],"all":true}),
        ] {
            assert!(Forget::parse(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_largest_layer_that_may_be_kept_fits_the_one_control_message_the_hub_carries() {
        use crate::message::{Operation, bounded};
        // Every optional word of every shape written, and the longest of everything the call itself says.
        let shape = |path| Shape {
            path,
            stroke: Colour::Transparent,
            line: 1000,
            fill: Colour::Transparent,
            opacity: Some(10_000),
            round: true,
        };
        let keep = Keep {
            id: "a".repeat(MAX_LAYER_ID),
            shapes: (0..MAX_LAYER_SHAPES)
                .map(|_| shape(outline(MAX_LAYER_PATH / MAX_LAYER_SHAPES)))
                .collect(),
        };
        keep.validate().unwrap();
        assert_eq!(keep.path_len(), MAX_LAYER_PATH);
        let call = Operation::Call {
            id: u64::MAX,
            provider: "host".into(),
            service: "peersus.drawings".into(),
            version: u32::MAX,
            method: "keep".into(),
            args: serde_json::to_value(&keep).unwrap(),
            timeout_ms: u32::MAX,
        };
        let bytes = serde_json::to_vec(&call).unwrap().len();
        assert!(bounded(&call), "{bytes} bytes");
    }

    #[test]
    fn only_a_clipped_canvas_can_preview_gestures() {
        let preview = json!({"wheel_doubling":600,"pinch":true,"drag":true});
        let canvas = json!({"k":"canvas","id":"scene","observe":["wheel","pinch"],
            "style":{"clip":true,"follow":preview}});
        let Node::Canvas { style, .. } = Node::parse(&canvas).unwrap() else {
            panic!("expected a canvas")
        };
        assert_eq!(
            style.follow,
            Some(Follow {
                wheel_doubling: 600,
                pinch: true,
                drag: true,
            })
        );
        let mut lean = canvas.clone();
        lean["style"]["follow"] = json!({});
        let Node::Canvas { style, .. } = Node::parse(&lean).unwrap() else {
            panic!("expected a canvas")
        };
        assert_eq!(style.follow, Some(Follow::default()));

        let mut no_clip = canvas.clone();
        no_clip["style"]["clip"] = json!(false);
        let mut no_id = canvas.clone();
        no_id["id"] = Value::Null;
        let mut drawing_box = canvas.clone();
        drawing_box["k"] = json!("box");
        drawing_box["children"] = json!([{"k":"drawing","width":8,"height":8,
            "shapes":[{"path":"M0 0 L8 8","stroke":"accent","line":10}]}]);
        for bad in [no_clip, no_id, drawing_box.clone()] {
            assert!(Node::parse(&bad).is_err(), "{bad}");
        }
        drawing_box["style"]
            .as_object_mut()
            .unwrap()
            .remove("follow");
        assert!(Node::parse(&drawing_box).is_ok(), "keep small drawings");
        // Absent, it is not written back.
        let plain = Node::parse(&json!({"k":"box","id":"a","style":{"clip":true}})).unwrap();
        assert!(
            serde_json::to_value(&plain).unwrap()["style"]
                .get("follow")
                .is_none()
        );
    }

    #[test]
    fn bitmap_preview_rejects_removed_fields() {
        let mut canvas = json!({"k":"canvas","id":"scene","style":{"clip":true,"follow":{}}});
        for obsolete in [
            json!({"scale":{"min":500000,"max":2000000,"overscan":[64,32]}}),
            json!({"settled":4210}),
            json!({"inertia":true}),
        ] {
            canvas["style"]["follow"] = obsolete;
            assert!(Node::parse(&canvas).is_err(), "{canvas}");
        }
    }

    #[test]
    fn a_pinch_says_when_it_was_made_as_the_other_gestures_do() {
        let pinch = json!({"t":"pinch","id":"canvas","x":120000,"y":90000,"scale":15000,"begin":false,"at":4242});
        let event: Event = serde_json::from_value(pinch.clone()).unwrap();
        event.validate().unwrap();
        assert!(matches!(event, Event::Pinch { at: 4242, .. }));
        assert_eq!(serde_json::to_value(&event).unwrap(), pinch);
        let mut timeless = pinch;
        timeless.as_object_mut().unwrap().remove("at");
        assert!(serde_json::from_value::<Event>(timeless).is_err());
    }

    #[test]
    fn a_rotate_starts_at_no_turn_and_is_refused_by_a_tree_that_does_not_know_it() {
        let rotate = json!({"t":"rotate","id":"canvas","x":120000,"y":90000,"turn":-4550,"begin":false,"at":7});
        let event: Event = serde_json::from_value(rotate.clone()).unwrap();
        event.validate().unwrap();
        assert_eq!(serde_json::to_value(&event).unwrap(), rotate);
        let started = json!({"t":"rotate","id":"canvas","x":0,"y":0,"turn":10,"begin":true,"at":7});
        assert!(
            serde_json::from_value::<Event>(started)
                .unwrap()
                .validate()
                .is_err()
        );
        // A box asks for it by name, as for a pinch.
        assert_eq!(
            serde_json::to_value(Observe::Rotate).unwrap(),
            json!("rotate")
        );
    }

    #[test]
    fn a_pressed_box_has_an_id_and_does_not_also_follow_two_touches() {
        let pressed = |observe: Value| json!({"k":"box","id":"row","observe":observe});
        assert!(Node::parse(&pressed(json!(["press", "visibility"]))).is_ok());
        for bad in [json!(["press", "pinch"]), json!(["press", "rotate"])] {
            assert!(Node::parse(&pressed(bad.clone())).is_err(), "{bad}");
        }
        assert!(Node::parse(&json!({"k":"box","observe":["press"]})).is_err());
        let hold = json!({"t":"hold","id":"row"});
        let event: Event = serde_json::from_value(hold.clone()).unwrap();
        event.validate().unwrap();
        assert_eq!(serde_json::to_value(&event).unwrap(), hold);
        assert!(Event::Hold { id: "a b".into() }.validate().is_err());
    }

    #[test]
    fn a_box_taking_back_has_an_id_and_is_told_back_by_it() {
        assert!(Node::parse(&json!({"k":"box","id":"room","observe":["back", "press"]})).is_ok());
        assert!(Node::parse(&json!({"k":"box","observe":["back"]})).is_err());
        let back = json!({"t":"back","id":"room"});
        let event: Event = serde_json::from_value(back.clone()).unwrap();
        event.validate().unwrap();
        assert_eq!(serde_json::to_value(&event).unwrap(), back);
        assert!(Event::Back { id: "a b".into() }.validate().is_err());
    }

    #[test]
    fn the_optional_page_features_have_the_names_an_extension_looks_for() {
        assert_eq!(PAGE_FEATURES_KEY, "page_features");
        assert_eq!(
            PAGE_FEATURES,
            [
                "pinch",
                "layers",
                "follow",
                "text_size",
                "rotate",
                "visibility",
                "press",
                "back"
            ]
        );
    }
}

#[cfg(test)]
mod device_chooser_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_chooser_says_what_may_be_chosen_and_never_writes_a_device_itself() {
        // The fewest words: online devices allowed everything, with dot and word.
        let plain = Node::parse(&json!({"k":"devices","id":"target"})).unwrap();
        let Node::Devices {
            pick,
            show,
            chosen,
            entries,
            ..
        } = &plain
        else {
            panic!("a chooser")
        };
        assert_eq!(pick, &Pick::default());
        assert_eq!(pick.presence, [Presence::Online]);
        assert!(pick.allowed && show.dot && show.presence && !show.statuses);
        assert!(chosen.is_none() && entries.is_empty());
        let full = json!({"k":"devices","id":"target","chosen":"d1","above":true,"this_device":true,
            "pick":{"presence":["online","sleeping"],"allowed":false,"accepts":"peersus.monitor",
                "status":{"plugin":"peersus.media","id":"watching"}},
            "show":{"dot":false,"presence":false,"statuses":true}});
        let node = Node::parse(&full).unwrap();
        assert_eq!(
            serde_json::to_value(&node).unwrap(),
            full,
            "read and written back the same"
        );
        for (key, value) in [
            ("id", json!("a b")),
            ("chosen", json!("")),
            ("pick", json!({"presence":[]})),
            ("pick", json!({"presence":["online","online"]})),
            ("pick", json!({"presence":["awake"]})),
            ("pick", json!({"accepts":"a b"})),
            ("pick", json!({"status":{"id":"watching"}})),
            ("show", json!({"colour":true})),
            // What a device is, is the application's to say.
            (
                "entries",
                json!([{"device":"d1","name":"Desk","presence":"online"}]),
            ),
        ] {
            let mut bad = full.clone();
            bad[key] = value;
            assert!(Node::parse(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_choice_names_the_chooser_and_the_device() {
        let value = json!({"t":"choose","id":"target","device":"d1"});
        let event: Event = serde_json::from_value(value.clone()).unwrap();
        event.validate().unwrap();
        assert_eq!(serde_json::to_value(&event).unwrap(), value);
        let this_device: Event =
            serde_json::from_value(json!({"t":"choose","id":"target"})).unwrap();
        assert_eq!(
            this_device,
            Event::Choose {
                id: "target".into(),
                device: None
            }
        );
        this_device.validate().unwrap();
        for bad in [
            Event::Choose {
                id: "".into(),
                device: Some("d1".into()),
            },
            Event::Choose {
                id: "target".into(),
                device: Some("".into()),
            },
            Event::Choose {
                id: "target".into(),
                device: Some("d\u{7}".into()),
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }
}
