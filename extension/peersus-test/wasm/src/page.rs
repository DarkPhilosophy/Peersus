//! The Test page: every element the application draws for an extension, each probe of what it answers, and, when the
//! setting asks for them, the buttons that misbehave on purpose. Only plain elements of `peersus-api`; the application
//! chooses every size, colour and font.
use crate::State;
use crate::probes::{PROBES, button};
use crate::trouble::{SPAM, TROUBLES};
use serde_json::{Value, json};

/// The application's own pictures this page shows, by name.
const ICONS: &[&str] = &[
    "send",
    "receive",
    "peers",
    "plugins",
    "settings",
    "history",
    "keyboard",
    "pointer",
    "network",
    "device",
    "languages",
    "backspace",
];
/// Every colour a box may be filled with, by its contract name.
const COLOURS: &[&str] = &[
    "surface", "raised", "text", "muted", "border", "scrim", "accent", "success", "danger",
];

fn px(n: u16) -> Value {
    json!({"unit":"px","value":n})
}
fn text(text: impl Into<String>, tone: &str) -> Value {
    json!({"k":"text","text":text.into(),"tone":tone})
}
fn button_node(id: &str, label: &str) -> Value {
    json!({"k":"button","id":id,"label":label})
}
fn bx(id: Option<&str>, style: Value, children: Vec<Value>) -> Value {
    json!({"k":"box","id":id,"style":style,"children":children})
}
fn row(children: Vec<Value>) -> Value {
    json!({"k":"row","children":children})
}
/// A titled group of the page: a raised card with its own heading.
fn section(title: &str, about: &str, mut children: Vec<Value>) -> Value {
    let mut all = vec![text(title, "title"), text(about, "muted")];
    all.append(&mut children);
    bx(
        None,
        json!({"padding":12,"gap":8,"radius":[12,12,12,12],"background":"raised","border":true}),
        all,
    )
}

impl State {
    /// The whole page as it is now.
    pub fn page(&self) -> Value {
        let mut children = vec![
            text("Test", "title"),
            text(
                concat!(
                    "Everything here is drawn by the application from what this extension answered, and every button asks \
                     the application something through its contract. Version ",
                    env!("CARGO_PKG_VERSION"),
                    "."
                ),
                "muted",
            ),
            self.elements(),
            self.surfaces(),
            self.answers(),
        ];
        if self.show_trouble {
            children.push(self.troubles());
        } else {
            children.push(text(
                "The buttons that misbehave on purpose are hidden: turn on \"Show the misbehaving buttons\" in this \
                 extension's settings.",
                "muted",
            ));
        }
        let page = json!({"k":"column","children":children});
        // While something waits for the application, the page asks to be drawn again by itself, so the answer shows.
        if self.waiting() {
            json!({"k":"interval","id":"answers","milliseconds":500,"child":page})
        } else {
            page
        }
    }

    fn elements(&self) -> Value {
        let swatches = COLOURS
            .iter()
            .map(|colour| {
                bx(
                    None,
                    json!({"padding":8,"radius":[8,8,8,8],"background":colour,"border":*colour == "surface"}),
                    vec![text(*colour, "muted")],
                )
            })
            .collect();
        let shapes = bx(
            None,
            json!({"horizontal":true,"gap":8,"wrap":true}),
            vec![
                bx(
                    None,
                    json!({"padding":8,"opacity":5000,"background":"accent"}),
                    vec![text("half seen", "body")],
                ),
                bx(
                    None,
                    json!({"padding":8,"radius":[16,0,16,0],"border":true}),
                    vec![text("two round corners", "body")],
                ),
                bx(
                    None,
                    json!({"height":px(64),"scroll":true,"padding":8,"border":true,"width":px(180)}),
                    (1..=8)
                        .map(|n| text(format!("Line {n} scrolls"), "body"))
                        .collect(),
                ),
                bx(
                    None,
                    json!({"height":px(64),"width":px(160),"border":true,"clip":true}),
                    vec![
                        text("Under", "muted"),
                        bx(
                            None,
                            json!({"overlay":true,"at":[40000,20000],"padding":4,"background":"success"}),
                            vec![text("over it", "body")],
                        ),
                    ],
                ),
            ],
        );
        let icons = ICONS
            .iter()
            .map(|name| json!({"k":"icon","name":name,"size":20}))
            .collect();
        let mut revealed = vec![
            json!({"k":"toggle","id":"reveal","label":"Open a part that grows in","on":self.reveal}),
        ];
        if self.reveal {
            revealed.push(bx(
                None,
                json!({"reveal":true,"padding":8,"background":"surface","radius":[8,8,8,8]}),
                vec![text("This grew out of the switch above.", "body")],
            ));
        }
        let mut interactive_controls = vec![
            bx(None, json!({"gap":8}), revealed),
            bx(
                None,
                json!({"horizontal":true,"gap":8,"wrap":true}),
                vec![
                    button_node("grow", "Add a row"),
                    button_node("clear", "Clear rows"),
                    button_node("flood", "Too large"),
                ],
            ),
        ];
        interactive_controls.extend((1..=self.rows).map(|n| {
            row(vec![
                text(format!("Row {n}"), "body"),
                text("drawn inside this card", "muted"),
            ])
        }));
        section(
            "Elements",
            "Texts in every tone, controls, boxes in every colour and shape, and the application's pictures.",
            vec![
                // Side by side and whole: a box that wraps moves what does not fit under the rest (a row would share the width out
                // equally and break the figure inside its share on a phone).
                bx(
                    None,
                    json!({"horizontal":true,"gap":12,"wrap":true}),
                    vec![
                        text("Title", "title"),
                        text("Body", "body"),
                        text("Muted", "muted"),
                        text("42%", "figure"),
                    ],
                ),
                json!({"k":"text","lines":2,"tone":"body","text":"A long text that may take at most two lines: whatever does not fit \
                    ends in an ellipsis, however wide the page is and however much more it would have to say about itself."}),
                row(vec![
                    button_node("count", "Press"),
                    text(format!("Pressed {} times", self.pressed), "body"),
                ]),
                json!({"k":"toggle","id":"switch","label":"A switch","on":self.on}),
                json!({"k":"field","id":"say","placeholder":"Write something and send it","value":"","height":44}),
                text(
                    if self.said.is_empty() {
                        "Nothing sent yet".to_owned()
                    } else {
                        format!("You sent: {}", self.said)
                    },
                    "body",
                ),
                json!({"k":"divider"}),
                text(
                    "Colour samples: each box names the theme colour it uses.",
                    "muted",
                ),
                bx(
                    None,
                    json!({"horizontal":true,"gap":8,"wrap":true}),
                    swatches,
                ),
                text(
                    "Box samples: transparency, corners, scrolling and layering.",
                    "muted",
                ),
                shapes,
                text("Application icons.", "muted"),
                bx(
                    None,
                    json!({"horizontal":true,"gap":12,"wrap":true,"center":true}),
                    icons,
                ),
                bx(None, json!({"gap":8}), interactive_controls),
            ],
        )
    }

    /// A ring as full as the pad's last touch was far across it, and a line through the last wheel steps.
    fn drawing(&self) -> Value {
        let share = self.pointer_share.unwrap_or(0).min(9_999);
        let angle = f64::from(share) / 10_000.0 * std::f64::consts::TAU;
        let (x, y) = (40.0 + 32.0 * angle.sin(), 40.0 - 32.0 * angle.cos());
        let large = u8::from(angle > std::f64::consts::PI);
        let mut graph = String::from("M 100 40");
        for (n, step) in self.wheel_steps.iter().enumerate() {
            let height = 40 - (*step).clamp(-30, 30);
            graph.push_str(&format!(" L {} {height}", 100 + 10 * (n + 1)));
        }
        let mut shapes = vec![
            json!({"path":"M 40 8 A 32 32 0 1 1 39.9 8","stroke":"border","line":60}),
            json!({"path":"M 230 70 L 260 10 L 290 70 Z","fill":"success","opacity":6000,"round":true}),
        ];
        if share > 0 {
            shapes.push(json!({"path":format!("M 40 8 A 32 32 0 {large} 1 {x:.1} {y:.1}"),"stroke":"accent","line":60,"round":true}));
        }
        if !self.wheel_steps.is_empty() {
            shapes.push(json!({"path":graph,"stroke":"text","line":20,"round":true}));
        }
        json!({"k":"drawing","width":300,"height":80,"shapes":shapes})
    }

    fn surfaces(&self) -> Value {
        let mut avatars: Vec<Value> = self
            .peers
            .iter()
            .take(6)
            .map(|(device, name)| json!({"k":"avatar","device":device,"name":name,"size":32}))
            .collect();
        if avatars.is_empty() {
            avatars.push(text(
                "Press \"Paired devices\" below to see their pictures here.",
                "muted",
            ));
        }
        let chosen = match &self.chosen {
            Some(Some(device)) => format!("Chosen: {device}"),
            Some(None) => "Chosen: this device".to_owned(),
            None => "Nothing chosen yet".to_owned(),
        };
        section(
            "Touch, devices and time",
            "Raw pointer, size and wheel events from a box with an id, the application's device chooser, pictures of paired \
             devices and a clock that ticks only while this page is shown.",
            vec![
                bx(
                    None,
                    json!({"horizontal":true,"wrap":true,"gap":16}),
                    vec![
                        json!({"k":"box","id":"pad","observe":["size","wheel"],
                        "style":{"height":px(120),"padding":12,"radius":[12,12,12,12],"background":"surface","border":true},
                        "children":[
                            text("Touch, drag or scroll here", "body"),
                            text(self.pointer.clone(), "muted"),
                            text(self.size.clone(), "muted"),
                            text(self.wheel.clone(), "muted"),
                        ]}),
                        bx(
                            None,
                            json!({"width":px(300),"gap":8}),
                            vec![
                                self.drawing(),
                                text(
                                    "The ring follows touch; the line follows scrolling. The green triangle is a sample shape.",
                                    "muted",
                                ),
                            ],
                        ),
                    ],
                ),
                json!({"k":"devices","id":"chooser","this_device":true,
                    "pick":{"presence":["online","sleeping","offline"],"allowed":false}}),
                text(chosen, "body"),
                bx(
                    None,
                    json!({"horizontal":true,"gap":8,"wrap":true,"center":true}),
                    avatars,
                ),
                json!({"k":"interval","id":"clock","milliseconds":1000,
                    "child":text(format!("The page's clock ticked {} times", self.ticks), "body")}),
            ],
        )
    }

    fn answers(&self) -> Value {
        let mut children = Vec::new();
        for (index, probe) in PROBES.iter().enumerate() {
            if probe.id == "define" {
                children.push(text(
                    "This device's status: type a label and press Enter to define or change it; Testing is the default. \
                     Show turns it on; Hide turns it off. Paired devices see active statuses on their Devices page.",
                    "muted",
                ));
                children.push(json!({"k":"field","id":"status-label","placeholder":"Status label (press Enter to apply)","value":"","height":44}));
                children.push(text(
                    match &self.status_label {
                        Some(label) => format!(
                            "This device: {label} ({})",
                            if self.status_on { "on" } else { "off" }
                        ),
                        None => "No status defined on this device yet".to_owned(),
                    },
                    "body",
                ));
            }
            let said = self.probed[index].clone().unwrap_or_else(|| {
                if self.pending_probe(index) {
                    "Asked…".to_owned()
                } else {
                    "Not asked yet".to_owned()
                }
            });
            children.push(row(vec![
                button_node(&button(probe), probe.label),
                text(said, "body"),
            ]));
            children.push(text(probe.expect, "muted"));
            if probe.id == "define" {
                children.push(button_node("reset-status", "Reset label to Testing"));
            }
        }
        children.push(json!({"k":"divider"}));
        children.push(text(
            "Notifications (permission: notify.show): Say something appears only in this window. Ask a question appears \
             here when the window is in front, otherwise through the system when supported and permitted. Keys: on \
             Settings › Binds, bind actions to count or ask for either notification.",
            "muted",
        ));
        children.push(row(vec![
            button_node("notify", "Say something"),
            button_node("question", "Ask a question"),
        ]));
        children.push(text(self.notified(), "body"));
        children.push(text(format!("Run by a key {} times", self.keyed), "body"));
        section(
            "What the application answers",
            "Each button sends one call of the contract and shows the answer. A permission not granted is answered \
             \"denied\": that is the right answer.",
            children,
        )
    }

    fn notified(&self) -> String {
        match (self.asked, self.told.is_empty()) {
            (0, _) => "No notification asked for yet".to_owned(),
            (_, true) => format!("{} waiting for an answer", self.notifying),
            (_, false) => format!("Last answer: {} ({} waiting)", self.told, self.notifying),
        }
    }

    fn troubles(&self) -> Value {
        let mut children = Vec::new();
        for (_, id, label, expect) in TROUBLES {
            children.push(row(vec![button_node(id, label), text(*expect, "muted")]));
        }
        children.push(text(
            format!(
                "Flooded {} times, {} operations each",
                self.spammed,
                2 * SPAM
            ),
            "body",
        ));
        section(
            "Misbehave on purpose",
            "Whatever this extension does here, only it may suffer for it: the window, the connections and the other \
             extensions go on.",
            children,
        )
    }
}

/// A tree far deeper than the application draws: it must keep what it showed and not draw this.
pub fn flood() -> Value {
    let mut tree = json!({"k":"text","text":"too deep"});
    for _ in 0..40 {
        tree = json!({"k":"column","children":[tree]});
    }
    tree
}
