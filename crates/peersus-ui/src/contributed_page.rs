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
    elements::{Colour, Event, Length, Node, Observe, PointerPhase, Shape, Style, Tone},
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
    /// Drawn inside a card (a page placed in Settings) rather than as a whole page: it is as tall as its content. A whole
    /// page is drawn inside the application's page frame, as the application's own pages are.
    pub embedded: bool,
}

impl Component for Body {
    fn render(&self) -> impl IntoElement {
        let content = elements(
            &self.link,
            &self.view,
            &self.plugin,
            &self.page,
            &self.pictures,
            self.palette,
            self.embedded,
        );
        if self.embedded {
            content
        } else {
            page_column(fills_the_room(&self.view, &self.plugin, &self.page))
                .child(content)
                .into_element()
        }
    }
}

/// The content of one extension page: the elements its extension drew, as the owner checked them.
fn elements(
    link: &Link,
    view: &View,
    plugin: &PluginInfo,
    page: &Page,
    pictures: &crate::shelf::Held,
    palette: Palette,
    embedded: bool,
) -> Element {
    match page.surface {
        Some(Surface::Elements) => Elements {
            link: link.clone(),
            plugin: plugin.id.clone(),
            page: page.id.clone(),
            view: page_view(view, plugin, page),
            avatars: Avatars::of(view),
            pictures: pictures.clone(),
            palette,
            embedded,
        }
        .into_element(),
        None => rect().into_element(),
    }
}

/// What the extension last drew for its page, if anything.
fn page_view(view: &View, plugin: &PluginInfo, page: &Page) -> Option<Node> {
    view.page_views
        .iter()
        .find(|shown| shown.plugin == plugin.id && shown.page == page.id)
        .map(|shown| shown.view.clone())
}

/// Whether a whole page fills the room under the header instead of being as tall as its content (a pad, a canvas).
fn fills_the_room(view: &View, plugin: &PluginInfo, page: &Page) -> bool {
    page_view(view, plugin, page).is_some_and(|node| shares_the_room(&as_page(&node)))
}

/// What a whole page is laid out in, inside the application's page frame: as wide as the page; as tall as the room
/// when it fills it, so that its root's share of the room is of the room left under the header.
fn page_column(fills: bool) -> Rect {
    rect().width(Size::fill()).maybe(fills, |column| {
        column.height(Size::fill()).content(Content::flex())
    })
}

/// The page chosen in the one place of the main navigation the extensions' pages share when the person chose so
/// (`extension_pages_together`). Which one is chosen in the header, beside the mark ([`Chooser`]); the choice is kept by
/// whoever draws both, so leaving the place and coming back keeps it, and a choice whose page went away falls to the first.
#[derive(PartialEq)]
pub struct Shared {
    pub link: Link,
    pub view: View,
    pub pages: Vec<(PluginInfo, Page)>,
    pub palette: Palette,
    pub pictures: crate::shelf::Held,
    /// The chosen page, by its extension and its own id.
    pub chosen: State<Option<(peersus_core::plugins::PluginId, String)>>,
}

impl Component for Shared {
    fn render(&self) -> impl IntoElement {
        let shown = chosen_index(
            self.pages
                .iter()
                .map(|(plugin, page)| (&plugin.id, page.id.as_str())),
            &self.chosen.read(),
        );
        let Some((plugin, page)) = self.pages.get(shown) else {
            return rect().into_element();
        };
        page_column(fills_the_room(&self.view, plugin, page))
            .child(elements(
                &self.link,
                &self.view,
                plugin,
                page,
                &self.pictures,
                self.palette,
                false,
            ))
            .into_element()
    }
}

/// One page the shared place can show, as the chooser lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub plugin: peersus_core::plugins::PluginId,
    pub page: String,
    pub label: String,
    pub icon: Option<String>,
}

/// The pages the shared place can show, in the order of the extensions and of their pages.
pub fn choices(view: &View) -> Vec<Choice> {
    pages_here(view, Placement::Primary)
        .into_iter()
        .map(|(plugin, page)| Choice {
            plugin: plugin.id,
            page: page.id,
            label: page.label,
            icon: page.icon,
        })
        .collect()
}

/// Which of `pages` is shown: the chosen one while it is there, else the first.
fn chosen_index<'a>(
    pages: impl Iterator<Item = (&'a peersus_core::plugins::PluginId, &'a str)>,
    chosen: &Option<(peersus_core::plugins::PluginId, String)>,
) -> usize {
    chosen
        .as_ref()
        .and_then(|(plugin, page)| {
            pages
                .enumerate()
                .find(|(_, (p, g))| *p == plugin && *g == page.as_str())
                .map(|(index, _)| index)
        })
        .unwrap_or(0)
}

/// The one way to choose which extension page the shared place shows: the chosen page's picture and name beside the
/// application's mark, and under it, once pressed, a menu of the others, as a device is chosen on a page. Only drawn while
/// the shared place is open and has more than one page.
#[derive(PartialEq)]
pub struct Chooser {
    pub choices: Vec<Choice>,
    pub chosen: State<Option<(peersus_core::plugins::PluginId, String)>>,
    pub palette: Palette,
}

impl Component for Chooser {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let mut open = use_state(|| false);
        crate::back::use_back(open(), EventHandler::new(move |()| open.set(false)));
        let placing = use_placing();
        let shown = chosen_index(
            self.choices
                .iter()
                .map(|choice| (&choice.plugin, choice.page.as_str())),
            &self.chosen.read(),
        );
        let face = |choice: &Choice| {
            rect()
                .horizontal()
                .spacing(8.)
                .cross_align(Alignment::center())
                .child(crate::glyph::picture_on(
                    crate::glyph::known_or_generic(choice.icon.as_deref()),
                    crate::glyph::BAR,
                    palette.send,
                    palette.text,
                    palette.surface,
                ))
                .child(
                    label()
                        .text(i18n::tr(&choice.label).to_owned())
                        .color(palette.text)
                        .max_lines(1)
                        // A page's name may be 80 characters: it is cut, never wider than the header's piece.
                        .max_width(Size::px(160.))
                        .text_overflow(TextOverflow::Ellipsis),
                )
        };
        let Some(current) = self.choices.get(shown) else {
            return rect().into_element();
        };
        let button = Button::new()
            .on_press(move |_| open.set(!open()))
            .child(face(current).child(label().text("▾").color(palette.muted)));
        // The menu holds the OTHER pages: the one shown is already on the button, and choosing it again would go nowhere.
        let menu = open().then(|| {
            let mut menu = menu_rows();
            let others = self
                .choices
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != shown);
            let mut count = 0;
            for (_, choice) in others {
                let key = (choice.plugin.clone(), choice.page.clone());
                let mut chosen = self.chosen;
                menu = menu.child(
                    Button::new()
                        .width(Size::fill_minimum())
                        .on_press(move |_| {
                            chosen.set(Some(key.clone()));
                            open.set(false);
                        })
                        .child(rect().width(Size::fill()).child(face(choice))),
                );
                count += 1;
            }
            (menu, count)
        });
        dropdown(button, menu, false, palette, placing)
    }
}

/// The room a chooser's menu may open in: the visible area of the page frame it is drawn in (under the header's pieces, over
/// the navigation), in the window's logical pixels as last laid out, as `[left, top, right, bottom]`. Given by the page frame;
/// a chooser outside every page frame (the header's) has the window.
#[derive(Clone, Copy)]
pub(crate) struct MenuRoom(pub(crate) State<Option<[f32; 4]>>);

/// What a chooser measured of itself, to place its menu: its button, and the rows of its menu once they were laid out, in the
/// window's logical pixels.
#[derive(Clone, Copy)]
struct Placing {
    button: State<Option<[f32; 4]>>,
    rows: State<Option<(f32, f32)>>,
}

/// The state a chooser keeps for [`dropdown`]; called in every render of the chooser, open or not.
fn use_placing() -> Placing {
    Placing {
        button: use_state(|| None),
        rows: use_state(|| None),
    }
}

/// Between a chooser's button and its menu.
const MENU_GAP: f32 = 4.;
/// The padding inside a menu's frame, around its rows.
const MENU_PADDING: f32 = 6.;
/// How tall a menu's row is guessed to be before the menu was laid out once: a button and the space under it.
const MENU_ROW: f32 = 48.;

/// Where a menu opens, decided by [`menu_place`].
#[derive(Clone, Copy, Debug, PartialEq)]
struct MenuPlace {
    /// Over the button rather than under it.
    above: bool,
    /// Its right edge on the button's right edge rather than its left edge on the button's left edge.
    from_right: bool,
    /// The most it may be tall, when it does not fit whole on the side it opens to: it is scrolled within that.
    most_height: Option<f32>,
}

/// Where a menu of `size` (width, height) opens for a button at `button`, inside `room`, all `[left, top, right, bottom]` in
/// the same pixels: under the button when it fits there, over it when it fits there and not under it; the page's wish
/// (`prefer_above`, a chooser at its foot) only decides which of the two is tried first. When it fits on neither side it
/// opens on the side with more room and is never taller than that room. It starts at the button's left edge, unless that
/// would carry it out of the room's right edge and ending at the button's right edge does not carry it out of the left one.
fn menu_place(button: [f32; 4], size: (f32, f32), room: [f32; 4], prefer_above: bool) -> MenuPlace {
    let over = button[1] - room[1] - MENU_GAP;
    let under = room[3] - button[3] - MENU_GAP;
    let fits = |space: f32| space >= size.1;
    let above = if prefer_above {
        fits(over) || (!fits(under) && over > under)
    } else {
        !fits(under) && (fits(over) || over > under)
    };
    let space = if above { over } else { under };
    MenuPlace {
        above,
        from_right: button[0] + size.0 > room[2] && button[2] - size.0 >= room[0],
        most_height: (!fits(space)).then_some(space.max(0.)),
    }
}

/// A chooser: its button and, while it is open, its menu (the frame from [`menu_frame`] with its rows, and how many rows).
/// The menu opens over the page, out of every piece it is in, where [`menu_place`] finds room for it in the page frame
/// ([`MenuRoom`]) or the window, wherever the chooser is on whatever page. It hangs from a box of no height on the button's
/// top or bottom edge, so how tall the chooser's own box is laid out never moves it. Until the menu was laid out once its
/// size is guessed from its rows; the measure then places it again.
fn dropdown(
    button: Button,
    menu: Option<(Rect, usize)>,
    prefer_above: bool,
    palette: Palette,
    placing: Placing,
) -> Element {
    let Placing {
        button: mut measured_button,
        rows: mut measured_rows,
    } = placing;
    let button = rect()
        .on_sized(move |e: freya::prelude::Event<SizedEventData>| {
            let area = [
                e.area.min_x(),
                e.area.min_y(),
                e.area.max_x(),
                e.area.max_y(),
            ];
            if *measured_button.peek() != Some(area) {
                measured_button.set(Some(area));
            }
        })
        .child(button);
    let Some((rows, count)) = menu else {
        return rect().child(button).into_element();
    };
    let window = Platform::get().current_window();
    let root = *window.root_size.read();
    let room = try_consume_context::<MenuRoom>()
        .and_then(|room| *room.0.read())
        .unwrap_or([0., 0., root.width, root.height]);
    let at = (*measured_button.read()).unwrap_or([0., 0., 0., 0.]);
    let size = (*measured_rows.read())
        .map(|(width, height)| (width + 2. * MENU_PADDING, height + 2. * MENU_PADDING))
        .unwrap_or((
            at[2] - at[0],
            count.max(1) as f32 * MENU_ROW + 2. * MENU_PADDING,
        ));
    let place = menu_place(at, size, room, prefer_above);
    let rows = rows.on_sized(move |e: freya::prelude::Event<SizedEventData>| {
        let size = (e.area.width(), e.area.height());
        if *measured_rows.peek() != Some(size) {
            measured_rows.set(Some(size));
        }
    });
    let content: Element = match place.most_height {
        Some(most) => ScrollView::new()
            .max_height(Size::px((most - 2. * MENU_PADDING).max(0.)))
            .child(rows)
            .into(),
        None => rows.into(),
    };
    let mut hang = Position::new_absolute();
    hang = if place.above {
        hang.bottom(MENU_GAP)
    } else {
        hang.top(MENU_GAP)
    };
    hang = if place.from_right {
        hang.right(0.)
    } else {
        hang.left(0.)
    };
    let frame = rect()
        .position(hang)
        .layer(Layer::Overlay)
        .padding(MENU_PADDING)
        .corner_radius(12.)
        .background(palette.raised)
        .border(
            Border::new()
                .fill(palette.border)
                .width(1.)
                .alignment(BorderAlignment::Inner),
        )
        .child(content);
    let anchor = rect().width(Size::fill()).height(Size::px(0.)).child(frame);
    let chooser = rect();
    if place.above {
        chooser.child(anchor).child(button)
    } else {
        chooser.child(button).child(anchor)
    }
    .into_element()
}

/// The rows of a chooser's menu, the same for every chooser of the application; [`dropdown`] puts them in their frame.
fn menu_rows() -> Rect {
    // Laid out to fit its rows, each as wide as the widest: one column of equal rows, not a ragged edge.
    rect().spacing(4.).content(Content::fit())
}

/// The application's word for how a device is reachable, and its colour: the same the device cards say.
fn presence_of(
    palette: Palette,
    presence: peersus_api::elements::Presence,
) -> (&'static str, (u8, u8, u8)) {
    use peersus_api::elements::Presence as Said;
    palette.of_presence(&match presence {
        Said::Online => crate::view::Presence::Online,
        Said::Sleeping => crate::view::Presence::Sleeping,
        Said::Offline => crate::view::Presence::Offline,
    })
}

/// The words a refused device of a chooser says instead of how it is reachable.
fn refusal_words(refusal: peersus_api::elements::Refusal) -> Option<&'static str> {
    use peersus_api::elements::Refusal;
    match refusal {
        Refusal::Presence => None,
        Refusal::NotAllowed => Some("NOT ALLOWED"),
        Refusal::NotThere => Some("NOT READY THERE"),
        Refusal::NoStatus => Some("NO STATUS"),
        Refusal::StatusNotAllowed => Some("STATUS NOT ALLOWED"),
    }
}

/// A page's device chooser, drawn by the application from what the owner filled in: the chosen device's picture and name in a frame,
/// and, once pressed, the application's menu of the devices, the refused ones dimmed with the word for why. Only a device that can
/// be chosen sends anything, and what it sends is the owner's key, which the owner turns into the extension's own name for it.
#[derive(PartialEq)]
struct DevicesNode {
    id: String,
    chosen: Option<String>,
    show: peersus_api::elements::Show,
    above: bool,
    this_device: bool,
    entries: Vec<peersus_api::elements::DeviceEntry>,
    palette: Palette,
    context: Context,
}

impl Component for DevicesNode {
    fn render(&self) -> impl IntoElement {
        use peersus_api::elements::{DeviceEntry, Presence};
        let palette = self.palette;
        let mut open = use_state(|| false);
        crate::back::use_back(open(), EventHandler::new(move |()| open.set(false)));
        let placing = use_placing();
        let context = &self.context;
        let show = self.show;
        // This device, by its own picture and name, as the first row: it is always there and reachable, so it says no word.
        let own = self.this_device.then(|| DeviceEntry {
            device: "this-device".to_owned(),
            name: context.avatars.name.clone(),
            presence: Presence::Online,
            statuses: Vec::new(),
            refused: None,
        });
        // Picture and name always; the dot, the word and the statuses as the page asked. A refused device says why instead.
        // A filled row (the one chosen, in the menu) takes the filled button's ink.
        let face = |entry: &DeviceEntry, itself: bool, filled: bool| {
            let ink = if filled {
                palette.button_text
            } else {
                palette.text
            };
            let (word, colour) = presence_of(palette, entry.presence);
            let why = entry.refused.and_then(refusal_words);
            let mut face = rect()
                .horizontal()
                .spacing(8.)
                .cross_align(Alignment::center())
                .maybe(entry.refused.is_some(), |face| face.opacity(0.45));
            if show.dot {
                face = face.child(
                    rect()
                        .width(Size::px(8.))
                        .height(Size::px(8.))
                        .corner_radius(4.)
                        .background(colour),
                );
            }
            face = face
                .child(crate::avatar_view::Avatar {
                    picture: context
                        .avatars
                        .digest_of(&entry.device)
                        .and_then(|digest| context.pictures.get(&digest)),
                    name: entry.name.clone(),
                    size: 24.,
                    palette,
                })
                .child(
                    label()
                        .text(entry.name.clone())
                        .color(ink)
                        .max_lines(1)
                        .max_width(Size::px(180.))
                        .text_overflow(TextOverflow::Ellipsis),
                );
            let said = match why {
                Some(why) => Some((why, palette.muted)),
                None if itself => None,
                // Refused for how it is reachable: the word says it, whether or not the page shows words.
                None if show.presence || entry.refused.is_some() => Some((word, colour)),
                None => None,
            };
            if let Some((said, colour)) = said {
                face = face.child(
                    label()
                        .text(i18n::tr(said).to_owned())
                        .color(colour)
                        .font_size(11.)
                        .max_lines(1),
                );
            }
            for status in &entry.statuses {
                face = face.child(
                    label()
                        .text(status.clone())
                        .color(palette.muted)
                        .font_size(11.)
                        .max_lines(1)
                        .max_width(Size::px(140.))
                        .text_overflow(TextOverflow::Ellipsis),
                );
            }
            face
        };
        // The row chosen now: the device named, else this device when it is offered, else none.
        let current: Option<(Option<&str>, &DeviceEntry)> = match &self.chosen {
            Some(chosen) => self
                .entries
                .iter()
                .find(|entry| entry.device == *chosen)
                .map(|entry| (Some(entry.device.as_str()), entry)),
            None => own.as_ref().map(|own| (None, own)),
        };
        let closed = match current {
            Some((key, entry)) => face(entry, key.is_none(), false),
            None => rect().child(
                label()
                    .text(i18n::tr("Choose a device").to_owned())
                    .color(palette.text),
            ),
        }
        .child(
            label()
                .text(if open() { "▴" } else { "▾" })
                .color(palette.muted),
        );
        let button = Button::new()
            .on_press(move |_| open.set(!open()))
            .child(closed);
        let menu = open().then(|| {
            let mut menu = menu_rows();
            if self.entries.is_empty() && own.is_none() {
                menu = menu.child(
                    label()
                        .text(i18n::tr("No device is paired").to_owned())
                        .color(palette.muted),
                );
            }
            let rows = own.iter().map(|own| (None, own)).chain(
                self.entries
                    .iter()
                    .map(|entry| (Some(entry.device.as_str()), entry)),
            );
            for (key, entry) in rows {
                let shown = current.is_some_and(|(chosen, _)| chosen == key);
                // The row's face starts at its left edge, as a list's rows do, however wide the widest row made it.
                let mut item = Button::new().width(Size::fill_minimum()).child(
                    rect()
                        .width(Size::fill())
                        .child(face(entry, key.is_none(), shown)),
                );
                if entry.refused.is_none() {
                    let (send, id, device) = (
                        context.send.handler.clone(),
                        self.id.clone(),
                        key.map(str::to_owned),
                    );
                    item = item.on_press(move |_| {
                        open.set(false);
                        send.call(Event::Choose {
                            id: id.clone(),
                            device: device.clone(),
                        });
                    });
                }
                if shown {
                    item = item.filled();
                }
                menu = menu.child(item);
            }
            (menu, self.entries.len() + usize::from(own.is_some()))
        });
        dropdown(button, menu, self.above, palette, placing)
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
    /// This device's own name: what a chooser that offers this device calls it.
    pub name: String,
    pub devices: Vec<(String, Option<peersus_core::profile::Digest>)>,
}

impl Avatars {
    pub fn of(view: &View) -> Self {
        Self {
            profile: view.profile_picture,
            name: view.settings.device_name.clone(),
            devices: view
                .devices
                .iter()
                .map(|d| (d.id.clone(), d.picture))
                .collect(),
        }
    }
    fn digest_of(&self, device: &str) -> Option<peersus_core::profile::Digest> {
        if device == "this-device" {
            return self.profile;
        }
        self.devices
            .iter()
            .find(|(id, _)| id == device)
            .and_then(|(_, p)| *p)
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

/// How many layers an extension's overlay box is raised above its parent: more than the deepest subtree a page can hold
/// (every element of [`peersus_api::elements::MAX_DEPTH`] levels drawn as a few nested boxes), so no sibling shows through it.
const OVERLAY_RAISE: i16 = (peersus_api::elements::MAX_DEPTH as i16 + 1) * 8;

/// A measured box of a page: its extension, its page and its id.
type MeasureKey = (peersus_core::plugins::PluginId, String, String);
/// How one drawn box says its size again.
type SayAgain = std::rc::Rc<dyn Fn()>;
/// The drawn boxes of each measured page id: their numbers and how each says its size again, the last drawn last.
type Measured = std::collections::HashMap<MeasureKey, Vec<(u64, SayAgain)>>;

thread_local! {
    /// Every measured box drawn now, by page and id, each able to say its size again. One page can be drawn in two places at
    /// once (in its settings and in its own place) and its extension keeps one measure per id: whichever said its size last
    /// won, and when that place went away (the settings closed) the page kept a measure of a place nobody saw, with cards
    /// laid out for it (a column of narrow cards on a phone). The box that stays says its size again then.
    static MEASURED: std::cell::RefCell<Measured> = Default::default();
    static VIEWS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// A number for one drawn box, different from every other's.
fn next_view() -> u64 {
    VIEWS.with(|views| {
        let next = views.get().wrapping_add(1);
        views.set(next);
        next
    })
}

/// Keep (or renew) how the box `serial` says the size of `key` again.
fn remember_measure(key: &MeasureKey, serial: u64, again: std::rc::Rc<dyn Fn()>) {
    MEASURED.with(|measured| {
        let mut measured = measured.borrow_mut();
        let boxes = measured.entry(key.clone()).or_default();
        match boxes.iter_mut().find(|(view, _)| *view == serial) {
            Some(entry) => entry.1 = again,
            None => boxes.push((serial, again)),
        }
    });
}

/// The box `serial` of `key` is no longer drawn: the last other box of the same page and id still drawn says its size again,
/// so the extension is left with the measure of a place a person sees.
fn forget_measure(key: &MeasureKey, serial: u64) {
    let again = MEASURED.with(|measured| {
        let mut measured = measured.borrow_mut();
        let boxes = measured.get_mut(key)?;
        boxes.retain(|(view, _)| *view != serial);
        let again = boxes.last().map(|(_, again)| again.clone());
        if boxes.is_empty() {
            measured.remove(key);
        }
        again
    });
    // Outside the registry's borrow: saying the size sends a command, which may come back here.
    if let Some(again) = again {
        again();
    }
}

/// Whether a page's root asks for a share of the room around it rather than its own height.
fn shares_the_room(node: &Node) -> bool {
    match node {
        Node::Interval { child, .. } => shares_the_room(child),
        Node::Box { style, .. } => matches!(style.height, Length::Percent(_) | Length::Flex(_)),
        _ => false,
    }
}

/// A whole page as it is drawn in the application's page frame, which scrolls as every page of the application does and
/// hides the header as it is read down. A root that asks to scroll and to be as tall as the room is asking for what the
/// frame already does: it is laid out as tall as its content, and the frame's scroll is its scroll. Anything else is
/// unchanged, a box that scrolls inside the page included.
fn as_page(node: &Node) -> Node {
    match node {
        Node::Interval {
            id,
            milliseconds,
            child,
        } => Node::Interval {
            id: id.clone(),
            milliseconds: *milliseconds,
            child: Box::new(as_page(child)),
        },
        Node::Box {
            id,
            style,
            children,
            observe,
        } if style.scroll && matches!(style.height, Length::Percent(_) | Length::Flex(_)) => {
            Node::Box {
                id: id.clone(),
                style: Style {
                    scroll: false,
                    height: Length::Auto,
                    ..style.clone()
                },
                children: children.clone(),
                observe: observe.clone(),
            }
        }
        other => other.clone(),
    }
}

impl Component for Elements {
    /// One page is one instance: drawn in the same place after another page (the shared Extensions place switches them), it
    /// starts afresh and asks for its own content, instead of keeping the other page's state and waiting for nothing.
    fn render_key(&self) -> DiffKey {
        DiffKey::from(&(
            std::any::TypeId::of::<Self>(),
            self.plugin.as_str(),
            self.page.as_str(),
        ))
    }

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
            Send {
                handler: std::rc::Rc::new(handler),
                plugin,
                page,
            }
        };
        let reduced = self.link.client().snapshot().settings.reduced_motion;
        let context = Context {
            send: send.clone(),
            avatars: self.avatars.clone(),
            pictures: self.pictures.clone(),
            reduced,
        };
        // Asked for whenever there is nothing to show and it was not asked for already: on opening, and again after the
        // extension restarted, which took away what it showed while the person stayed on the page.
        let mut asked = use_state(|| false);
        if self.view.is_some() {
            asked.set_if_modified(false);
        } else if !*asked.peek() {
            asked.set(true);
            send.handler.call(Event::Open);
        }
        // Inside a card that is as tall as what it holds, "the room left" depends on the card itself: embedded, a page is
        // as wide as the card and as tall as its content; a page whose root asks for a share of the room (so that it fills
        // a whole page) is given a fixed one here, since a card has no room to share. A whole page is drawn in the
        // application's page frame (`as_page`): as tall as its content, which the frame scrolls, or the room's height when
        // its root shares the room (laid out in a `page_column` as tall as the room).
        let (body, node) = if self.embedded {
            let shares = self.view.as_ref().is_some_and(shares_the_room);
            (
                rect()
                    .width(Size::fill())
                    .overflow(Overflow::Clip)
                    .maybe(shares, |el| el.height(Size::px(EMBEDDED_HEIGHT))),
                self.view.clone(),
            )
        } else {
            let node = self.view.as_ref().map(as_page);
            let fills = node.as_ref().is_some_and(shares_the_room);
            (
                rect().width(Size::fill()).maybe(fills, |el| {
                    el.height(Size::flex(1.))
                        .content(Content::flex())
                        .overflow(Overflow::Clip)
                }),
                node,
            )
        };
        match &node {
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
        Node::Text { text, tone, lines } => {
            let (size, colour) = match tone {
                Tone::Title => (22., palette.text),
                Tone::Body => (15., palette.text),
                Tone::Muted => (13., palette.muted),
                Tone::Figure => (40., palette.text),
            };
            label()
                .text(text.clone())
                .font_size(size)
                .font_weight(if *tone == Tone::Figure {
                    FontWeight::BOLD
                } else {
                    FontWeight::NORMAL
                })
                .color(colour)
                .max_lines(lines.map(usize::from))
                .maybe(lines.is_some(), |label| {
                    label.text_overflow(TextOverflow::Ellipsis)
                })
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
        Node::Box {
            id,
            style,
            children,
            observe,
        } => BoxNode {
            id: id.clone(),
            style: style.clone(),
            children: children.clone(),
            observe: observe.clone(),
            palette,
            context: context.clone(),
        }
        .into(),
        Node::Avatar { device, name, size } => crate::avatar_view::Avatar {
            picture: context
                .avatars
                .digest_of(device)
                .and_then(|digest| context.pictures.get(&digest)),
            name: name.clone(),
            size: f32::from(*size),
            palette,
        }
        .into(),
        Node::Icon { name, size } => crate::glyph::draw(
            crate::glyph::by_name(crate::glyph::known_or_generic(Some(name))),
            f32::from(*size),
            palette.text,
        ),
        Node::Interval {
            id,
            milliseconds,
            child,
        } => IntervalNode {
            id: id.clone(),
            milliseconds: *milliseconds,
            child: child.as_ref().clone(),
            palette,
            context: context.clone(),
        }
        .into(),
        Node::Drawing {
            width,
            height,
            shapes,
        } => drawing_view(*width, *height, shapes, palette),
        Node::Devices {
            id,
            chosen,
            pick: _,
            show,
            above,
            this_device,
            entries,
        } => DevicesNode {
            id: id.clone(),
            chosen: chosen.clone(),
            show: *show,
            above: *above,
            this_device: *this_device,
            entries: entries.clone(),
            palette,
            context: context.clone(),
        }
        .into(),
    }
}

/// A colour with its opacity.
type Rgba = (u8, u8, u8, u8);

fn rgba((r, g, b): (u8, u8, u8)) -> Rgba {
    (r, g, b, 255)
}

/// The theme's colour for a semantic one; `None` for transparent.
fn paint(palette: Palette, colour: Colour) -> Option<Rgba> {
    Some(match colour {
        Colour::Transparent => return None,
        Colour::Surface => rgba(palette.surface),
        Colour::Raised => rgba(palette.raised),
        Colour::Text => rgba(palette.text),
        Colour::Muted => rgba(palette.muted),
        Colour::Border => rgba(palette.border),
        Colour::Scrim => (0, 0, 0, 180),
        Colour::Accent => rgba(palette.receive),
        Colour::Success => rgba(palette.good),
        Colour::Danger => rgba(palette.bad),
    })
}

/// An extension's drawing as it is shown. A drawing is new bytes whenever the extension changes it (a graph, once a second),
/// and the toolkit keeps each raster an hour after it stops being shown by default: a page drawing a moving graph grew by a
/// raster a second, over a megabyte a second, until the hour was up. So it is dropped as soon as it is replaced.
pub(crate) fn drawing_view(width: u16, height: u16, shapes: &[Shape], palette: Palette) -> Element {
    SvgViewer::new(Bytes::from(drawing(width, height, shapes, palette)))
        .asset_age(AssetAge::zero())
        .width(Size::px(f32::from(width)))
        .height(Size::px(f32::from(height)))
        .into()
}

/// The SVG of a drawing: its shapes in the theme's colours, in its own pixels. Every value written comes from the
/// checked tree (outlines of commands and numbers only, colours of the theme, bounded numbers), so nothing an extension
/// sends can add an attribute or an element.
fn drawing(width: u16, height: u16, shapes: &[Shape], palette: Palette) -> String {
    use std::fmt::Write;
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {width} {height}\" width=\"{width}\" height=\"{height}\">"
    );
    let hex = |(r, g, b, _): Rgba| format!("#{r:02x}{g:02x}{b:02x}");
    let alpha = |(_, _, _, a): Rgba| f32::from(a) / 255.;
    for shape in shapes {
        let _ = write!(svg, "<path d=\"{}\"", shape.path);
        match paint(palette, shape.fill) {
            Some(fill) => {
                let _ = write!(
                    svg,
                    " fill=\"{}\" fill-opacity=\"{:.3}\"",
                    hex(fill),
                    alpha(fill)
                );
            }
            None => svg.push_str(" fill=\"none\""),
        }
        if let Some(stroke) = paint(palette, shape.stroke).filter(|_| shape.line > 0) {
            let _ = write!(
                svg,
                " stroke=\"{}\" stroke-opacity=\"{:.3}\" stroke-width=\"{:.1}\"",
                hex(stroke),
                alpha(stroke),
                f32::from(shape.line) / 10.
            );
            if shape.round {
                svg.push_str(" stroke-linecap=\"round\" stroke-linejoin=\"round\"");
            }
        }
        if let Some(opacity) = shape.opacity {
            let _ = write!(svg, " opacity=\"{:.4}\"", f32::from(opacity) / 10000.);
        }
        svg.push_str("/>");
    }
    svg.push_str("</svg>");
    svg
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
        .height(
            self.height
                .map(|n| Size::px(f32::from(n)))
                .unwrap_or_default(),
        )
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
/// Whether a child of a box asks to be as large as its largest sibling across the box: its height in a row, its width
/// in a column.
fn matches_largest_sibling(horizontal: bool, children: &[Node]) -> bool {
    children.iter().any(|child| match child {
        Node::Box { style, .. } => {
            (if horizontal {
                style.height
            } else {
                style.width
            }) == Length::FillMinimum
        }
        _ => false,
    })
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
        self.id
            .as_ref()
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
        let opening = use_animation_with_dependencies(
            &(self.context.reduced, style.reveal),
            |conf, (reduced, reveal)| {
                conf.on_creation(if *reveal && !*reduced {
                    OnCreation::Run
                } else {
                    OnCreation::Finish
                });
                AnimNum::new(0., 1.)
                    .time(220)
                    .function(Function::Cubic)
                    .ease(Ease::Out)
            },
        );
        // A box that scrolls is as tall as its content inside the room it is given.
        let mut node = rect()
            .width(if style.scroll {
                Size::fill()
            } else {
                dimension(style.width)
            })
            .height(if style.scroll {
                Size::auto()
            } else {
                dimension(style.height)
            })
            .content(if style.wrap {
                Content::wrap_spacing(f32::from(style.gap))
            } else if matches_largest_sibling(style.horizontal, &self.children) {
                // The toolkit sizes a child to its largest sibling only in a box laid out to fit them.
                Content::fit()
            } else {
                Content::flex()
            })
            .padding(f32::from(style.padding))
            .spacing(f32::from(style.gap))
            .corner_radius((
                f32::from(style.radius[0]),
                f32::from(style.radius[1]),
                f32::from(style.radius[2]),
                f32::from(style.radius[3]),
            ));
        if style.horizontal {
            node = node.horizontal();
        }
        if style.center {
            node = node.center();
        }
        if style.bottom {
            node = node.main_align(Alignment::end());
        }
        if style.border {
            node = node.border(Border::new().fill(palette.border).width(1.));
        }
        if let Some(colour) = paint(palette, style.background) {
            node = node.background(colour);
        }
        if style.overlay {
            // Above its siblings, but inside every clip of its ancestors: the toolkit's own overlay layer clears inherited
            // clipping, and an extension's box then painted over the header and the navigation, outside its page. The toolkit
            // paints every element one layer above its parent, so a sibling's text nested a few boxes deep sat higher than an
            // overlay raised by one and showed through it (a pad's "Left click" over an open menu): raised above the deepest
            // subtree a page can hold instead.
            node = node
                .position(Position::new_absolute().top(0.).left(0.))
                .layer(Layer::Relative(OVERLAY_RAISE));
        }
        if let Some([x, y]) = style.at {
            node = node.position(
                Position::new_absolute()
                    .left(x as f32 / 1000.)
                    .top(y as f32 / 1000.),
            );
        }
        if style.clip {
            node = node.overflow(Overflow::Clip);
        }
        if let Some(opacity) = style.opacity {
            node = node.opacity(f32::from(opacity) / 10000.);
        }
        // Capture is generic and reports raw coordinates; interpretation belongs to the extension.
        // Cell state never holds a reactive borrow across an application callback.
        // Which pointer pressed this element: the mouse (`Some(None)`) or one finger. With several
        // fingers down, each element follows only its own, so a finger held on one element is not
        // let go when another finger lifts elsewhere (hold a button, drag with another finger).
        let active = use_state(|| std::cell::Cell::new(None::<Option<u64>>));
        // What this box last measured, shared with the registry below (never the toolkit's own storage, which another
        // component's removal must not touch).
        let measured =
            use_hook(|| std::rc::Rc::new(std::cell::Cell::new(None::<(i32, i32, u32, u32)>)));
        let serial = use_hook(next_view);
        let registered = use_hook(|| std::rc::Rc::new(std::cell::RefCell::new(None::<MeasureKey>)));
        use_drop({
            let registered = registered.clone();
            move || {
                if let Some(key) = registered.borrow_mut().take() {
                    forget_measure(&key, serial);
                }
            }
        });
        if let Some(id) = &self.id {
            if self.observe.contains(&Observe::Size) {
                let (send, id) = (self.context.send.handler.as_ref().clone(), id.clone());
                let key = (
                    self.context.send.plugin.clone(),
                    self.context.send.page.clone(),
                    id.clone(),
                );
                remember_measure(&key, serial, {
                    let (send, id, measured) = (send.clone(), id.clone(), measured.clone());
                    std::rc::Rc::new(move || {
                        if let Some(bounds) = measured.get() {
                            send.call(Event::Size {
                                id: id.clone(),
                                x: bounds.0,
                                y: bounds.1,
                                width: bounds.2,
                                height: bounds.3,
                            });
                        }
                    })
                });
                if let Some(previous) = registered.borrow_mut().replace(key.clone())
                    && previous != key
                {
                    forget_measure(&previous, serial);
                }
                let measured = measured.clone();
                node = node.on_sized(move |e: freya::prelude::Event<SizedEventData>| {
                    let bounds = (
                        (e.area.min_x() * 1000.) as i32,
                        (e.area.min_y() * 1000.) as i32,
                        (e.area.width() * 1000.) as u32,
                        (e.area.height() * 1000.) as u32,
                    );
                    if measured.replace(Some(bounds)) != Some(bounds) {
                        send.call(Event::Size {
                            id: id.clone(),
                            x: bounds.0,
                            y: bounds.1,
                            width: bounds.2,
                            height: bounds.3,
                        });
                    }
                });
            } else if let Some(previous) = registered.borrow_mut().take() {
                forget_measure(&previous, serial);
            }
            if self.observe.contains(&Observe::Wheel) {
                let (send, id) = (self.context.send.handler.as_ref().clone(), id.clone());
                node = node.on_wheel(move |e: freya::prelude::Event<WheelEventData>| {
                    e.stop_propagation();
                    send.call(Event::Wheel {
                        id: id.clone(),
                        x: (e.global_location.x * 1000.) as i32,
                        y: (e.global_location.y * 1000.) as i32,
                        dx: (e.delta_x * 1000.) as i32,
                        dy: (e.delta_y * 1000.) as i32,
                        at: crate::monotonic_ms(),
                    });
                });
            }
            let emit = {
                let (send, id) = (self.context.send.handler.as_ref().clone(), id.clone());
                EventHandler::new(move |(phase, x, y): (PointerPhase, i32, i32)| {
                    send.call(Event::Pointer {
                        id: id.clone(),
                        phase,
                        x,
                        y,
                        at: crate::monotonic_ms(),
                    });
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
            node = node
                .on_pointer_down({
                    let emit = emit.clone();
                    move |e: freya::prelude::Event<PointerEventData>| {
                        e.stop_propagation();
                        // An element pressed by one finger stays that finger's until it lifts; the
                        // mouse, whose release may be lost outside the window, can always press again.
                        let pointer = finger(&e);
                        if let (Some(Some(holder)), Some(new)) = (active.peek().get(), pointer)
                            && holder != new
                        {
                            return;
                        }
                        active.peek().set(Some(pointer));
                        let (x, y) = position(&e);
                        emit.call((PointerPhase::Down, x, y));
                    }
                })
                .on_capture_global_pointer_move({
                    let emit = emit.clone();
                    move |e: freya::prelude::Event<PointerEventData>| {
                        if active.peek().get() == Some(finger(&e)) {
                            let (x, y) = position(&e);
                            emit.call((PointerPhase::Move, x, y));
                        }
                    }
                })
                .on_global_pointer_up({
                    let emit = emit.clone();
                    move |e: freya::prelude::Event<PointerEventData>| {
                        if active.peek().get() == Some(finger(&e)) {
                            active.peek().set(None);
                            let (x, y) = position(&e);
                            emit.call((PointerPhase::Up, x, y));
                        }
                    }
                })
                .on_touch_cancel(move |e: freya::prelude::Event<TouchEventData>| {
                    if active.peek().get() == Some(Some(e.finger_id)) {
                        active.peek().set(None);
                        emit.call((PointerPhase::Cancel, 0, 0));
                    }
                });
        }
        for child in &self.children {
            node = node.child(draw(child, palette, &self.context));
        }
        let node: Element = if style.scroll {
            ScrollView::new()
                .width(dimension(style.width))
                .height(dimension(style.height))
                .child(node)
                .into()
        } else {
            node.into()
        };
        if style.reveal {
            let progress = opening.get().value();
            let measured = rect()
                .width(if style.width == Length::Auto {
                    Size::auto()
                } else {
                    Size::fill()
                })
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
            return rect()
                .width(dimension(style.width))
                .height(height)
                .overflow(Overflow::Clip)
                .opacity(progress)
                .child(measured)
                .into();
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
            let (id, send, milliseconds) = (
                self.id.clone(),
                self.context.send.handler.as_ref().clone(),
                self.milliseconds,
            );
            move || {
                let mut ticks = crate::link::clock(
                    crate::monotonic_ms,
                    std::time::Duration::from_millis(u64::from(milliseconds)),
                );
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

#[cfg(test)]
mod measure_tests {
    use super::*;

    #[test]
    fn when_one_place_of_a_page_goes_the_place_that_stays_says_its_size_again() {
        let key: MeasureKey = (
            peersus_core::plugins::PluginId::new("monitor").unwrap(),
            "mon".into(),
            "width".into(),
        );
        let said = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let place = |name: &'static str| -> std::rc::Rc<dyn Fn()> {
            let said = said.clone();
            std::rc::Rc::new(move || said.borrow_mut().push(name))
        };
        let (page, settings) = (next_view(), next_view());
        remember_measure(&key, page, place("page"));
        remember_measure(&key, settings, place("settings"));
        // Drawn again: renewed, not added twice.
        remember_measure(&key, settings, place("settings"));
        // The settings close: the page says its size again, once.
        forget_measure(&key, settings);
        assert_eq!(*said.borrow(), ["page"]);
        // The page goes too: nobody is left to say anything, and nothing is kept for the page.
        forget_measure(&key, page);
        assert_eq!(*said.borrow(), ["page"]);
        assert!(MEASURED.with(|measured| !measured.borrow().contains_key(&key)));
    }
}

#[cfg(test)]
mod menu_place_tests {
    use super::*;

    /// A phone's page frame, 400 wide, from under the header (80) to over the navigation (760).
    const ROOM: [f32; 4] = [0., 80., 400., 760.];

    #[test]
    fn a_chooser_at_the_foot_of_a_page_opens_over_its_button_whatever_the_page_asked() {
        // Remote's chooser, over the pad's lower left corner: a menu of five rows has no room under it.
        let button = [12., 640., 212., 690.];
        for prefer_above in [true, false] {
            let place = menu_place(button, (230., 260.), ROOM, prefer_above);
            assert_eq!(
                place,
                MenuPlace {
                    above: true,
                    from_right: false,
                    most_height: None
                },
                "prefer_above = {prefer_above}"
            );
        }
    }

    #[test]
    fn a_chooser_at_the_top_of_a_page_opens_under_its_button_whatever_the_page_asked() {
        // Monitor's chooser, at the top: no room over it.
        let button = [12., 96., 212., 146.];
        for prefer_above in [true, false] {
            assert!(!menu_place(button, (230., 260.), ROOM, prefer_above).above);
        }
    }

    #[test]
    fn the_page_wish_decides_only_when_both_sides_have_room() {
        let button = [12., 400., 212., 450.];
        assert!(menu_place(button, (230., 120.), ROOM, true).above);
        assert!(!menu_place(button, (230., 120.), ROOM, false).above);
    }

    #[test]
    fn a_menu_taller_than_both_sides_opens_on_the_larger_one_and_is_cut_to_it() {
        let button = [12., 500., 212., 550.];
        let place = menu_place(button, (230., 900.), ROOM, false);
        assert!(place.above);
        assert_eq!(place.most_height, Some(500. - 80. - MENU_GAP));
    }

    #[test]
    fn a_menu_squeezed_on_both_sides_never_overhangs_the_room() {
        // A chooser filling a room barely taller than itself: less than one row free on either side.
        let room = [0., 80., 400., 200.];
        let button = [12., 100., 212., 180.];
        let place = menu_place(button, (230., 260.), room, true);
        let most = place.most_height.expect("cut to the room");
        let side = if place.above {
            button[1] - room[1] - MENU_GAP
        } else {
            room[3] - button[3] - MENU_GAP
        };
        assert!(most <= side.max(0.), "{most} > {side}");
    }

    #[test]
    fn a_menu_that_would_leave_the_right_edge_ends_at_the_buttons_right_edge() {
        let button = [250., 96., 390., 146.];
        assert!(menu_place(button, (230., 100.), ROOM, false).from_right);
        // Left-aligned when right-aligning would leave the left edge instead.
        let wide = [250., 96., 390., 146.];
        assert!(!menu_place(wide, (420., 100.), ROOM, false).from_right);
    }
}
