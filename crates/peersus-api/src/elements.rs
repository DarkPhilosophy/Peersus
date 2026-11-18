//! What an extension shows on a page of its own: a tree of plain elements, which any frontend
//! draws its own way inside the page it gives the extension, and the typed events a person's
//! actions on them become.
//!
//! Extensions own their page's layout, appearance and interaction logic. Frontends render only
//! these generic primitives and report raw interaction events. A source may answer `null` when
//! an event changes no visual state; continuous input need not rebuild or retransmit its tree.
use crate::{Error, Result, name};
use serde::{Deserialize, Serialize};

/// Most elements in one tree.
pub const MAX_NODES: usize = 512;
/// Deepest nesting of rows and columns.
pub const MAX_DEPTH: usize = 12;
/// Longest text of one element, in characters.
pub const MAX_TEXT: usize = 2_000;
/// Longest text a field holds or sends, in characters.
pub const MAX_FIELD: usize = 1_000;
/// Largest tree, as JSON bytes: what an owner refuses before it parses.
pub const MAX_BYTES: usize = 128 * 1024;
/// Most shapes of one drawing.
pub const MAX_SHAPES: usize = 32;
/// Longest outline of all the shapes of one drawing together, in characters.
pub const MAX_PATH: usize = 16 * 1024;
/// Most devices one [`Node::Devices`] lists.
pub const MAX_DEVICES: usize = 64;
/// Most statuses one device of a [`Node::Devices`] shows.
pub const MAX_DEVICE_STATUSES: usize = 4;

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

/// Generic box layout, shared by every extension. No application-specific controls live here.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Style {
    pub width: Length,
    pub height: Length,
    pub horizontal: bool,
    pub gap: u16,
    pub padding: u16,
    pub radius: [u16; 4],
    pub background: Colour,
    pub border: bool,
    pub center: bool,
    /// Place this box over its parent; children remain clipped to the extension's page.
    pub overlay: bool,
    /// Align children against the lower edge (for example a sheet or a toolbar).
    pub bottom: bool,
    /// Absolute top-left offset within the parent, in thousandths of a logical pixel.
    pub at: Option<[i32; 2]>,
    pub clip: bool,
    /// Opacity in hundredths of a percent; absent means fully opaque.
    pub opacity: Option<u16>,
    /// Open when it appears: from nothing to its own height, fading in, the way a menu grows out of
    /// what opened it. At once when the person turned decorative motion off.
    pub reveal: bool,
    /// Its content moves up and down inside it when it is taller than the box.
    pub scroll: bool,
    /// In a horizontal box, children that no longer fit beside the others go on under them, whole: a grid of cards
    /// that has as many columns as the width holds.
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
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        style: Style,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        observe: Vec<Observe>,
        #[serde(default)]
        children: Vec<Node>,
    },
    /// An identity image resolved from the frontend's existing, local picture cache.
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
    /// A generic periodic callback, active only while this element is mounted.
    Interval {
        id: String,
        milliseconds: u16,
        child: Box<Node>,
    },
    /// A drawing `width` by `height` pixels made of `shapes`, the first under the others: what an extension draws
    /// that no other element is (a ring, a graph, a chart). The application only strokes and fills the outlines; what
    /// they mean belongs to the extension.
    Drawing {
        width: u16,
        height: u16,
        #[serde(default)]
        shapes: Vec<Shape>,
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
    Press {
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

impl Node {
    /// Whether the tree is one a frontend draws: within every limit, with sound ids and text.
    pub fn validate(&self) -> Result<()> {
        let mut count = 0;
        self.check(1, &mut count)
    }

    fn check(&self, depth: usize, count: &mut usize) -> Result<()> {
        *count += 1;
        ok(*count <= MAX_NODES && depth <= MAX_DEPTH)?;
        match self {
            Node::Column { children } | Node::Row { children } => {
                for child in children {
                    child.check(depth + 1, count)?;
                }
                Ok(())
            }
            Node::Box {
                id,
                style,
                children,
                observe,
            } => {
                ok(id.as_ref().is_none_or(|id| name::valid(id)))?;
                ok(observe.len() <= 2 && (observe.is_empty() || id.is_some()))?;
                let measure = |length: Length| match length {
                    Length::Auto => true,
                    Length::Px(n) => n <= 8192,
                    Length::Percent(n) => n <= 10000,
                    Length::Flex(n) => n > 0 && n <= 1000,
                    Length::FillMinimum => true,
                };
                ok(measure(style.width)
                    && measure(style.height)
                    && style.gap <= 512
                    && style.padding <= 512
                    && style.radius.iter().all(|r| *r <= 8192)
                    && style.opacity.is_none_or(|n| n <= 10000)
                    && style
                        .at
                        .is_none_or(|at| at.iter().all(|n| n.unsigned_abs() <= 16_384_000)))?;
                for child in children {
                    child.check(depth + 1, count)?;
                }
                Ok(())
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
            Node::Interval {
                id,
                milliseconds,
                child,
            } => {
                ok(name::valid(id) && (100..=60000).contains(milliseconds))?;
                child.check(depth + 1, count)
            }
            Node::Drawing {
                width,
                height,
                shapes,
            } => ok((1..=4096).contains(width)
                && (1..=4096).contains(height)
                && shapes.len() <= MAX_SHAPES
                && shapes.iter().map(|shape| shape.path.len()).sum::<usize>() <= MAX_PATH
                && shapes.iter().all(Shape::valid)),
            Node::Text { text, lines, .. } => {
                ok(name::text(text, MAX_TEXT) && lines.is_none_or(|n| (1..=20).contains(&n)))
            }
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
}

impl Event {
    pub fn validate(&self) -> Result<()> {
        match self {
            Event::Open => Ok(()),
            Event::Press { id } | Event::Toggle { id, .. } => ok(name::valid(id)),
            Event::Submit { id, text } => ok(name::valid(id) && field(text)),
            Event::Pointer { id, .. }
            | Event::Tick { id, .. }
            | Event::Size { id, .. }
            | Event::Wheel { id, .. } => ok(name::valid(id)),
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
