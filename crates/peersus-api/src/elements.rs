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

/// How a text is set: the frontend chooses the sizes and colours.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Title,
    #[default]
    Body,
    Muted,
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
    /// As large as the largest of its siblings along this axis, within a parent that is as large as its content: the rows of a
    /// menu or a list all as wide as the widest, so that their backgrounds and edges line up instead of stepping with each text.
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
    /// Its children side by side, sharing the width.
    Row {
        #[serde(default)]
        children: Vec<Node>,
    },
    Text {
        text: String,
        #[serde(default)]
        tone: Tone,
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
}

fn ok(valid: bool) -> Result<()> {
    valid.then_some(()).ok_or(Error::InvalidManifest)
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
            Node::Box { id, style, children, observe } => {
                ok(id.as_ref().is_none_or(|id| name::valid(id)))?;
                ok(observe.len() <= 2 && (observe.is_empty() || id.is_some()))?;
                let measure = |length: Length| match length {
                    Length::Auto => true,
                    Length::Px(n) => n <= 8192,
                    Length::Percent(n) => n <= 10000,
                    Length::Flex(n) => n > 0 && n <= 1000,
                    Length::FillMinimum => true,
                };
                ok(measure(style.width) && measure(style.height)
                    && style.gap <= 512 && style.padding <= 512
                    && style.radius.iter().all(|r| *r <= 8192)
                    && style.opacity.is_none_or(|n| n <= 10000)
                    && style.at.is_none_or(|at| at.iter().all(|n| n.unsigned_abs() <= 16_384_000)))?;
                for child in children {
                    child.check(depth + 1, count)?;
                }
                Ok(())
            }
            Node::Avatar { device, name: caption, size } => {
                ok(!device.is_empty() && device.len() <= 256
                    && name::text(caption, MAX_TEXT) && (8..=256).contains(size))
            }
            Node::Icon { name, size } => {
                ok(!name.is_empty() && name.len() <= 32
                    && name.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
                    && (8..=256).contains(size))
            }
            Node::Interval { id, milliseconds, child } => {
                ok(name::valid(id) && (100..=60000).contains(milliseconds))?;
                child.check(depth + 1, count)
            }
            Node::Text { text, .. } => ok(name::text(text, MAX_TEXT)),
            Node::Button { id, label } | Node::Toggle { id, label, .. } => {
                ok(name::valid(id) && name::text(label, crate::contributions::MAX_LABEL))
            }
            Node::Field {
                id,
                placeholder,
                value,
                height,
            } => ok(name::valid(id) && field(placeholder) && field(value)
                && height.is_none_or(|n| (1..=8192).contains(&n))),
            Node::Divider => Ok(()),
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
            Event::Pointer { id, .. } | Event::Tick { id, .. }
                | Event::Size { id, .. } | Event::Wheel { id, .. } => ok(name::valid(id)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        let valid=json!({"k":"box","id":"canvas","observe":["size","wheel"],
            "style":{"at":[-16384000,16384000],"opacity":10000,"clip":true},"children":[]});
        assert!(Node::parse(&valid).is_ok());
        for (key,value) in [("id",serde_json::Value::Null),("observe",json!(["size","wheel","size"]))] {
            let mut bad=valid.clone();bad[key]=value;
            assert!(Node::parse(&bad).is_err());
        }
        for (key,value) in [("at",json!([16384001,0])),("opacity",json!(10001))] {
            let mut bad=valid.clone();bad["style"][key]=value;
            assert!(Node::parse(&bad).is_err());
        }
    }

    #[test]
    fn size_and_relative_wheel_events_preserve_signed_global_coordinates() {
        for value in [
            json!({"t":"size","id":"canvas","x":-1500,"y":2500,"width":600000,"height":360000}),
            json!({"t":"wheel","id":"canvas","x":-1500,"y":2500,"dx":-1000,"dy":2000,"at":123}),
        ] {
            let event:Event=serde_json::from_value(value.clone()).unwrap();
            event.validate().unwrap();
            assert_eq!(serde_json::to_value(event).unwrap(),value);
            let mut bad=value;bad["id"]=json!("");
            assert!(serde_json::from_value::<Event>(bad).unwrap().validate().is_err());
        }
    }
    #[test]
    fn an_icon_is_named_and_bounded() {
        assert!(Node::parse(&json!({"k":"icon","name":"backspace","size":22})).is_ok());
        for bad in [json!({"k":"icon","name":"","size":22}), json!({"k":"icon","name":"Back/space","size":22}),
            json!({"k":"icon","name":"backspace","size":4}), json!({"k":"icon","name":"backspace","size":22,"path":"M0"})] {
            assert!(Node::parse(&bad).is_err(), "{bad}");
        }
    }
    #[test]
    fn a_length_can_take_the_widest_sibling_and_needs_no_value() {
        let tree = json!({"k":"box","style":{"width":{"unit":"fill_minimum"}},"children":[]});
        let node = Node::parse(&tree).unwrap();
        let Node::Box { style, .. } = &node else { panic!("a box") };
        assert_eq!(style.width, Length::FillMinimum);
        assert_eq!(serde_json::to_value(Length::FillMinimum).unwrap(), json!({"unit":"fill_minimum"}));
    }
    #[test]
    fn fields_preserve_default_sizing_and_reject_unrenderable_heights() {
        let mut field=json!({"k":"field","id":"message","value":""});
        assert!(Node::parse(&field).is_ok());
        field["height"]=json!(48);
        assert!(Node::parse(&field).is_ok());
        for height in [0,8193] {
            field["height"]=json!(height);
            assert!(Node::parse(&field).is_err());
        }
    }
}
