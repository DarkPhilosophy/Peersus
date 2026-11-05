//! Extension pages are discovered from their declarations. The frontend renders generic API
//! elements; extensions supply their content, styling and interaction logic.
use crate::{
    field::{Profile, field},
    i18n,
    link::Link,
    theme::Palette,
    view::View,
};
use freya::animation::*;
use freya::prelude::*;
use peersus_api::{
    contributions::{Page, Placement, Surface},
    elements::{Colour, Event, Length, Node, Observe, PointerPhase, Style, Tone},
    platform::OperatingSystem,
};
use peersus_core::model::{Command, PluginInfo, PluginStatus, Standing};

/// The pages of running extensions placed at `placement` on this system, in the order of the
/// extensions and of their pages.
pub fn pages_here(view: &View, placement: Placement) -> Vec<(PluginInfo, Page)> {
    let here = OperatingSystem::current();
    view.plugins
        .iter()
        .filter(|plugin| {
            plugin.standing == Standing::Ready && plugin.status == PluginStatus::Running
        })
        .flat_map(|plugin| {
            plugin
                .manifest
                .contributions
                .pages
                .iter()
                .filter(|page| {
                    page.placement == placement
                        && page.surface.is_some()
                        && (page.platforms.is_empty()
                            || here.is_some_and(|os| page.platforms.contains(&os)))
                })
                .map(move |page| (plugin.clone(), page.clone()))
        })
        .collect()
}

/// What is drawn for one extension page, whatever placed it.
#[derive(PartialEq)]
pub struct Body {
    pub link: Link,
    pub view: View,
    pub plugin: PluginInfo,
    pub page: Page,
    pub palette: Palette,
    pub pictures: crate::shelf::Held,
    /// Drawn inside a card (a page placed in Settings) rather than as a whole page: it is as tall as its content.
    pub embedded: bool,
}

impl Component for Body {
    fn render(&self) -> impl IntoElement {
        match self.page.surface {
            Some(Surface::Elements) => Elements {
                link: self.link.clone(),
                plugin: self.plugin.id.clone(),
                page: self.page.id.clone(),
                view: self
                    .view
                    .page_views
                    .iter()
                    .find(|shown| shown.plugin == self.plugin.id && shown.page == self.page.id)
                    .map(|shown| shown.view.clone()),
                avatars: Avatars::of(&self.view),
                pictures: self.pictures.clone(),
                palette: self.palette,
                embedded: self.embedded,
            }
            .into_element(),
            None => rect().into_element(),
        }
    }
}

/// An extension's own content: asked for when the page opens, drawn from what the owner checked,
/// and every action on it sent back as a typed event.
#[derive(PartialEq)]
pub struct Elements {
    pub link: Link,
    pub plugin: peersus_core::plugins::PluginId,
    pub page: String,
    pub view: Option<Node>,
    pub avatars: Avatars,
    pub pictures: crate::shelf::Held,
    pub palette: Palette,
    pub embedded: bool,
}

/// What a page's avatars are drawn from: the picture each device showed, by its id. Small and
/// comparable, so an unchanged page is not drawn again for a change elsewhere in the window.
#[derive(Clone, PartialEq, Default)]
pub struct Avatars {
    pub profile: Option<peersus_core::profile::Digest>,
    pub devices: Vec<(String, Option<peersus_core::profile::Digest>)>,
}

impl Avatars {
    pub fn of(view: &View) -> Self {
        Self {
            profile: view.profile_picture,
            devices: view.devices.iter().map(|d| (d.id.clone(), d.picture)).collect(),
        }
    }
    fn digest_of(&self, device: &str) -> Option<peersus_core::profile::Digest> {
        if device == "this-device" {
            return self.profile;
        }
        self.devices.iter().find(|(id, _)| id == device).and_then(|(_, p)| *p)
    }
}

/// The handler a page's events go through. A handler compares equal to no other, which would have
/// every node of the page drawn again on every snapshot: two are equal here when they reach the
/// same page of the same extension, which is all a node's handler does.
#[derive(Clone)]
struct Send {
    handler: std::rc::Rc<EventHandler<Event>>,
    plugin: peersus_core::plugins::PluginId,
    page: String,
}

impl PartialEq for Send {
    fn eq(&self, other: &Self) -> bool {
        self.plugin == other.plugin && self.page == other.page
    }
}

/// What every node of a page is drawn with. Equal between two renders of an unchanged page.
#[derive(Clone, PartialEq)]
struct Context {
    send: Send,
    avatars: Avatars,
    /// Decorative motion turned off: boxes that reveal themselves appear at once.
    reduced: bool,
    pictures: crate::shelf::Held,
}

/// How tall a page that would fill the screen is inside a settings card.
const EMBEDDED_HEIGHT: f32 = 360.;

/// Whether a page's root asks for a share of the room around it rather than its own height.
fn shares_the_room(node: &Node) -> bool {
    match node {
        Node::Interval { child, .. } => shares_the_room(child),
        Node::Box { style, .. } => matches!(style.height, Length::Percent(_) | Length::Flex(_)),
        _ => false,
    }
}

impl Component for Elements {
    fn render(&self) -> impl IntoElement {
        let send = {
            let (link, plugin, page) = (self.link.clone(), self.plugin.clone(), self.page.clone());
            let handler = EventHandler::new({
                let (plugin, page) = (plugin.clone(), page.clone());
                move |event: Event| {
                    link.send(Command::PageEvent {
                        plugin: plugin.clone(),
                        page: page.clone(),
                        event,
                    })
                }
            });
            Send { handler: std::rc::Rc::new(handler), plugin, page }
        };
        let reduced = self.link.client().snapshot().settings.reduced_motion;
        let context = Context { send: send.clone(), avatars: self.avatars.clone(), pictures: self.pictures.clone(), reduced };
        // Asked for whenever there is nothing to show and it was not asked for already: on opening, and again after the
        // extension restarted, which took away what it showed while the person stayed on the page.
        let mut asked = use_state(|| false);
        if self.view.is_some() {
            asked.set_if_modified(false);
        } else if !*asked.peek() {
            asked.set(true);
            send.handler.call(Event::Open);
        }
        // A whole page takes the room left on the screen. Inside a card that is as tall as what it holds, "the room
        // left" depends on the card itself: the page was not counted in the card's height and was drawn over what came
        // after it. Embedded, it is as wide as the card and as tall as its content; a page whose root asks for a share
        // of the room (so that it fills a whole page) is given a fixed one here, since a card has no room to share.
        let body = if self.embedded {
            let shares = self.view.as_ref().is_some_and(shares_the_room);
            rect().width(Size::fill()).overflow(Overflow::Clip)
                .maybe(shares, |el| el.height(Size::px(EMBEDDED_HEIGHT)))
        } else {
            rect().expanded().content(Content::flex()).overflow(Overflow::Clip)
        };
        match &self.view {
            Some(node) => body.child(draw(node, self.palette, &context)),
            None => body.child(
                label()
                    .text(i18n::tr("Loading…").to_owned())
                    .color(self.palette.muted),
            ),
        }
    }
}

fn draw(node: &Node, palette: Palette, context: &Context) -> Element {
    let send = &context.send.handler;
    match node {
        Node::Column { children } => children
            .iter()
            .fold(rect().width(Size::fill()).spacing(10.), |column, child| {
                column.child(draw(child, palette, context))
            })
            .into(),
        Node::Row { children } => children
            .iter()
            .fold(
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .content(Content::flex())
                    .spacing(8.),
                |row, child| {
                    row.child(
                        rect()
                            .width(Size::flex(1.))
                            .child(draw(child, palette, context)),
                    )
                },
            )
            .into(),
        Node::Text { text, tone } => {
            let (size, colour) = match tone {
                Tone::Title => (22., palette.text),
                Tone::Body => (15., palette.text),
                Tone::Muted => (13., palette.muted),
            };
            label()
                .text(text.clone())
                .font_size(size)
                .color(colour)
                .into()
        }
        Node::Button { id, label: caption } => {
            let (send, id) = (send.clone(), id.clone());
            Button::new()
                .child(caption.clone())
                .on_press(move |_| send.call(Event::Press { id: id.clone() }))
                .into()
        }
        Node::Toggle {
            id,
            label: caption,
            on,
        } => {
            let (send, id) = (send.clone(), id.clone());
            rect()
                .width(Size::fill())
                .horizontal()
                .content(Content::flex())
                .cross_align(Alignment::center())
                .child(
                    rect().width(Size::flex(1.)).child(
                        label()
                            .text(caption.clone())
                            .color(palette.text)
                            .font_size(15.),
                    ),
                )
                .child(Switch::new(WritableSetter::new(*on, move |on| {
                    send.call(Event::Toggle { id: id.clone(), on })
                })))
                .into()
        }
        Node::Field {
            id,
            placeholder,
            value,
            height,
        } => FieldNode {
            id: id.clone(),
            placeholder: placeholder.clone(),
            value: value.clone(),
            height: *height,
            send: send.as_ref().clone(),
            palette,
        }
        .into(),
        Node::Divider => rect()
            .width(Size::fill())
            .height(Size::px(1.))
            .background(palette.border)
            .into(),
        Node::Box { id, style, children, observe } => BoxNode {
            id: id.clone(), style: style.clone(), children: children.clone(), observe: observe.clone(),
            palette, context: context.clone(),
        }.into(),
        Node::Avatar { device, name, size } => crate::avatar_view::Avatar {
            picture: context.avatars.digest_of(device).and_then(|digest| context.pictures.get(&digest)),
            name: name.clone(), size: f32::from(*size), palette,
        }.into(),
        Node::Icon { name, size } => crate::glyph::draw(
            crate::glyph::by_name(crate::glyph::known_or_generic(Some(name))),
            f32::from(*size),
            palette.text,
        ),
        Node::Interval { id, milliseconds, child } => IntervalNode {
            id: id.clone(), milliseconds: *milliseconds,
            child: child.as_ref().clone(), palette, context: context.clone(),
        }.into(),
    }
}

/// A line the person writes, kept here while it is written and sent when they submit it.
#[derive(PartialEq)]
struct FieldNode {
    id: String,
    placeholder: String,
    value: String,
    height: Option<u16>,
    send: EventHandler<Event>,
    palette: Palette,
}

impl Component for FieldNode {
    fn render(&self) -> impl IntoElement {
        let value = self.value.clone();
        let mut draft = use_state(move || value);
        let (send, id) = (self.send.clone(), self.id.clone());
        // The application's one field with its Enter inside it, the same as every other field: Enter on the keyboard and
        // the button submit alike, and what was sent is cleared.
        crate::field::submit(
            field(Profile::TEXT, draft).placeholder(self.placeholder.clone()),
            crate::field::Submit {
                on_submit: EventHandler::new(move |()| {
                    let text = draft.read().clone();
                    send.call(Event::Submit {
                        id: id.clone(),
                        text,
                    });
                    draft.set(String::new());
                }),
                button: true,
                enabled: true,
                colour: self.palette.send.into(),
            },
        )
        .width(Size::fill())
        .height(self.height.map(|n| Size::px(f32::from(n))).unwrap_or_default())
    }
}

fn dimension(length: Length) -> Size {
    match length {
        Length::Auto => Size::auto(),
        Length::Px(value) => Size::px(f32::from(value)),
        Length::Percent(value) => Size::percent(f32::from(value) / 100.),
        Length::Flex(value) => Size::flex(f32::from(value)),
        Length::FillMinimum => Size::fill_minimum(),
    }
}

#[derive(PartialEq)]
struct BoxNode {
    id: Option<String>,
    style: Style,
    children: Vec<Node>,
    observe: Vec<Observe>,
    palette: Palette,
    context: Context,
}

impl Component for BoxNode {
    fn render_key(&self) -> DiffKey {
        self.id.as_ref()
            .map(|id| DiffKey::from(&(std::any::TypeId::of::<Self>(), id)))
            .unwrap_or_else(|| self.default_key())
    }

    fn render(&self) -> impl IntoElement {
        let style = &self.style;
        let palette = self.palette;
        // A box that reveals itself opens from nothing to its own height: measured once drawn,
        // then grown to it. The hooks run for every box, so their order never depends on the style;
        // only a revealing box runs its animation, every other one starts finished and draws nothing more.
        let natural = use_state(|| None::<f32>);
        let opening = use_animation_with_dependencies(&(self.context.reduced, style.reveal), |conf, (reduced, reveal)| {
            conf.on_creation(if *reveal && !*reduced { OnCreation::Run } else { OnCreation::Finish });
            AnimNum::new(0., 1.).time(220).function(Function::Cubic).ease(Ease::Out)
        });
        let mut node = rect()
            .width(dimension(style.width)).height(dimension(style.height))
            .content(Content::flex())
            .padding(f32::from(style.padding)).spacing(f32::from(style.gap))
            .corner_radius((f32::from(style.radius[0]), f32::from(style.radius[1]),
                f32::from(style.radius[2]), f32::from(style.radius[3])));
        if style.horizontal { node = node.horizontal(); }
        if style.center { node = node.center(); }
        if style.bottom { node = node.main_align(Alignment::end()); }
        if style.border { node = node.border(Border::new().fill(palette.border).width(1.)); }
        node = match style.background {
            Colour::Transparent => node,
            Colour::Surface => node.background(palette.surface),
            Colour::Raised => node.background(palette.raised),
            Colour::Text => node.background(palette.text),
            Colour::Muted => node.background(palette.muted),
            Colour::Border => node.background(palette.border),
            Colour::Scrim => node.background((0, 0, 0, 180)),
            Colour::Accent => node.background(palette.receive),
            Colour::Success => node.background(palette.good),
            Colour::Danger => node.background(palette.bad),
        };
        if style.overlay {
            node = node.position(Position::new_absolute().top(0.).left(0.)).layer(Layer::Overlay);
        }
        if let Some([x, y]) = style.at {
            node = node.position(Position::new_absolute().left(x as f32 / 1000.).top(y as f32 / 1000.));
        }
        if style.clip { node = node.overflow(Overflow::Clip); }
        if let Some(opacity) = style.opacity { node = node.opacity(f32::from(opacity) / 10000.); }
        // Capture is generic and reports raw coordinates; interpretation belongs to the extension.
        // Cell state never holds a reactive borrow across an application callback.
        // Which pointer pressed this element: the mouse (`Some(None)`) or one finger. With several
        // fingers down, each element follows only its own, so a finger held on one element is not
        // let go when another finger lifts elsewhere (hold a button, drag with another finger).
        let active = use_state(|| std::cell::Cell::new(None::<Option<u64>>));
        let measured = use_state(|| std::cell::Cell::new(None));
        if let Some(id) = &self.id {
            if self.observe.contains(&Observe::Size) {
                let (send, id) = (self.context.send.handler.as_ref().clone(), id.clone());
                node = node.on_sized(move |e: freya::prelude::Event<SizedEventData>| {
                    let bounds = ((e.area.min_x() * 1000.) as i32, (e.area.min_y() * 1000.) as i32,
                        (e.area.width() * 1000.) as u32, (e.area.height() * 1000.) as u32);
                    if measured.peek().replace(Some(bounds)) != Some(bounds) {
                        send.call(Event::Size { id: id.clone(), x: bounds.0, y: bounds.1, width: bounds.2, height: bounds.3 });
                    }
                });
            }
            if self.observe.contains(&Observe::Wheel) {
                let (send, id) = (self.context.send.handler.as_ref().clone(), id.clone());
                node = node.on_wheel(move |e: freya::prelude::Event<WheelEventData>| {
                    e.stop_propagation();
                    send.call(Event::Wheel { id: id.clone(), x: (e.global_location.x * 1000.) as i32,
                        y: (e.global_location.y * 1000.) as i32, dx: (e.delta_x * 1000.) as i32,
                        dy: (e.delta_y * 1000.) as i32, at: crate::monotonic_ms() });
                });
            }
            let emit = {
                let (send, id) = (self.context.send.handler.as_ref().clone(), id.clone());
                EventHandler::new(move |(phase, x, y): (PointerPhase, i32, i32)| {
                    send.call(Event::Pointer { id: id.clone(), phase, x, y, at: crate::monotonic_ms() });
                })
            };
            let position = |e: &PointerEventData| {
                let p = e.global_location();
                ((p.x * 1000.) as i32, (p.y * 1000.) as i32)
            };
            let finger = |e: &PointerEventData| match e {
                PointerEventData::Touch(touch) => Some(touch.finger_id),
                PointerEventData::Mouse(_) => None,
            };
            node = node.on_pointer_down({
                let emit = emit.clone();
                move |e: freya::prelude::Event<PointerEventData>| {
                    e.stop_propagation();
                    // An element pressed by one finger stays that finger's until it lifts; the
                    // mouse, whose release may be lost outside the window, can always press again.
                    let pointer = finger(&e);
                    if let (Some(Some(holder)), Some(new)) = (active.peek().get(), pointer) && holder != new { return; }
                    active.peek().set(Some(pointer));
                    let (x, y) = position(&e);
                    emit.call((PointerPhase::Down, x, y));
                }
            }).on_capture_global_pointer_move({
                let emit = emit.clone();
                move |e: freya::prelude::Event<PointerEventData>| {
                    if active.peek().get() == Some(finger(&e)) {
                        let (x, y) = position(&e);
                        emit.call((PointerPhase::Move, x, y));
                    }
                }
            }).on_global_pointer_up({
                let emit = emit.clone();
                move |e: freya::prelude::Event<PointerEventData>| {
                    if active.peek().get() == Some(finger(&e)) {
                        active.peek().set(None);
                        let (x, y) = position(&e);
                        emit.call((PointerPhase::Up, x, y));
                    }
                }
            }).on_touch_cancel(move |e: freya::prelude::Event<TouchEventData>| {
                if active.peek().get() == Some(Some(e.finger_id)) {
                    active.peek().set(None);
                    emit.call((PointerPhase::Cancel, 0, 0));
                }
            });
        }
        for child in &self.children {
            node = node.child(draw(child, palette, &self.context));
        }
        if style.reveal {
            let progress = opening.get().value();
            let measured = rect()
                .width(if style.width == Length::Auto { Size::auto() } else { Size::fill() })
                .on_sized(move |e: freya::prelude::Event<SizedEventData>| {
                    let mut natural = natural;
                    natural.set_if_modified(Some(e.area.height()));
                })
                .child(node);
            // Grown from nothing while it opens; then its own height, free to change with its content.
            let height = match *natural.read() {
                _ if progress >= 1. => Size::auto(),
                Some(full) => Size::px(full * progress),
                None => Size::px(0.),
            };
            return rect().width(dimension(style.width)).height(height)
                .overflow(Overflow::Clip).opacity(progress).child(measured);
        }
        node
    }
}

#[derive(PartialEq)]
struct IntervalNode {
    id: String,
    milliseconds: u16,
    child: Node,
    palette: Palette,
    context: Context,
}

impl Component for IntervalNode {
    fn render(&self) -> impl IntoElement {
        use_hook({
            let (id, send, milliseconds) = (self.id.clone(), self.context.send.handler.as_ref().clone(), self.milliseconds);
            move || {
                let mut ticks = crate::link::clock(crate::monotonic_ms,
                    std::time::Duration::from_millis(u64::from(milliseconds)));
                spawn(async move {
                    while ticks.changed().await.is_ok() {
                        let at = *ticks.borrow_and_update();
                        send.call(Event::Tick { id: id.clone(), at });
                    }
                });
            }
        });
        draw(&self.child, self.palette, &self.context)
    }
}
