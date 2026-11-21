//! Extension pages are discovered from their declarations. The frontend renders generic API
//! elements; extensions supply their content, styling and interaction logic.
use crate::{
    field::{Profile, field},
    i18n,
    link::Link,
    render_budget::{Paint as BudgetPaint, Phase as DrawPhase, RenderBudgets},
    theme::Palette,
    view::View,
};
use freya::animation::*;
use freya::engine::prelude::{
    Canvas, Paint, PaintCap, PaintJoin, PaintStyle, SkColor, SkPath, SkRect,
};
use freya::prelude::*;
use peersus_api::{
    contributions::{Page, Placement, Surface},
    elements::{Colour, Event, Length, Node, Observe, Placed, PointerPhase, Shape, Style, Tone},
    platform::OperatingSystem,
};
use peersus_core::{
    model::{Command, PluginInfo, PluginStatus, Standing},
    plugins::PluginId,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Instant,
};

fn extension_has_pages(plugin: &PluginInfo) -> bool {
    plugin.agreed && plugin.standing == Standing::Ready && plugin.status == PluginStatus::Running
}

fn page_is_here(page: &Page, here: Option<OperatingSystem>) -> bool {
    page.surface.is_some()
        && (page.platforms.is_empty() || here.is_some_and(|os| page.platforms.contains(&os)))
}

/// The pages of running extensions placed at `placement` on this system, in the order of the
/// extensions and of their pages. Nothing of an extension whose agreement the person has not accepted (`agreed`) is shown.
pub fn pages_here(view: &View, placement: Placement) -> Vec<(PluginInfo, Page)> {
    let here = OperatingSystem::current();
    view.plugins
        .iter()
        .filter(|plugin| extension_has_pages(plugin))
        .flat_map(|plugin| {
            plugin
                .manifest
                .contributions
                .pages
                .iter()
                .filter(|page| page.placement == placement && page_is_here(page, here))
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
            measurement_page: measurement_page(&plugin.manifest.contributions.pages, page),
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

// A placement has its own event address, but an embedded copy and its primary copy
// measure the same template. Restore the visible copy's dimensions when its sibling unmounts.
fn measurement_page(pages: &[Page], page: &Page) -> String {
    if page.source.is_some() {
        let mut templates = pages.iter().filter(|p| {
            p.placement == Placement::Settings
                && p.source == page.source
                && p.surface == page.surface
                && p.platforms == page.platforms
                && p.capability == page.capability
        });
        if let Some(template) = templates.next()
            && templates.next().is_none()
        {
            return template.id.clone();
        }
    }
    page.id.clone()
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
        dropdown(button, menu, false, false, palette, placing)
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
pub(crate) struct Placing {
    button: State<Option<[f32; 4]>>,
    rows: State<Option<(f32, f32)>>,
}

/// The state a chooser keeps for [`dropdown`]; called in every render of the chooser, open or not.
pub(crate) fn use_placing() -> Placing {
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
pub(crate) fn dropdown(
    button: Button,
    menu: Option<(Rect, usize)>,
    prefer_above: bool,
    wide: bool,
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
    if wide {
        // Placed in the window from where the button was measured, and not hung from a box as wide as the button: the room of a box
        // is all the width its rows may take, and the rows of this menu are as wide as their names, which a button is not.
        let height = size.1.min(place.most_height.unwrap_or(f32::MAX));
        let left = if place.from_right {
            at[2] - size.0
        } else {
            at[0]
        };
        let top = if place.above {
            at[1] - MENU_GAP - height
        } else {
            at[3] + MENU_GAP
        };
        let frame = frame.position(
            Position::new_global()
                .left(left.clamp(
                    room[0] + 2. * MENU_GAP,
                    (room[2] - size.0 - 2. * MENU_GAP).max(room[0] + 2. * MENU_GAP),
                ))
                .top(top.max(room[1])),
        );
        return rect().child(button).child(frame).into_element();
    }
    // As wide as the button, never `fill`: a box that fills makes the chooser as wide as all the room it is given, which in the
    // header pushed the places and the window's buttons over each other while the menu was open.
    let anchor = rect()
        .width(Size::px((at[2] - at[0]).max(0.)))
        .height(Size::px(0.))
        .child(frame.position(hang));
    let chooser = rect();
    if place.above {
        chooser.child(anchor).child(button)
    } else {
        chooser.child(button).child(anchor)
    }
    .into_element()
}

/// The rows of a chooser's menu, the same for every chooser of the application; [`dropdown`] puts them in their frame.
pub(crate) fn menu_rows() -> Rect {
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
        dropdown(button, menu, self.above, false, palette, placing)
    }
}

/// An extension's own content: asked for when the page opens, drawn from what the owner checked,
/// and every action on it sent back as a typed event.
#[derive(PartialEq)]
pub struct Elements {
    pub link: Link,
    pub plugin: peersus_core::plugins::PluginId,
    pub page: String,
    pub measurement_page: String,
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
    measurement_page: String,
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
/// Bounds as the toolkit measured them, in thousandths of a pixel: x, y, width, height.
type Bounds = (i32, i32, u32, u32);

/// Whether a box's measure is said to its extension now: the first time, when its size changed, or (`input`) when pointer
/// or wheel input from it is about to be said and it moved since it was last said, so the input is read against where the
/// box is. A move alone is not said: a page that scrolls moves every box on it at every frame, and each move said was a
/// call to the extension, its whole page answered and drawn again, which made scrolling such a page lag.
fn say_measure(reported: Option<Bounds>, measured: Bounds, input: bool) -> bool {
    match reported {
        None => true,
        Some(said) => (said.2, said.3) != (measured.2, measured.3) || (input && said != measured),
    }
}

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

/// The window owns the accounting; only a phase change notifies Freya subscribers.
#[derive(Clone)]
struct DrawingWindow {
    accounts: Rc<RefCell<RenderBudgets>>,
    revision: State<u64>,
    frame: Rc<Cell<u64>>,
}

impl PartialEq for DrawingWindow {
    fn eq(&self, other: &Self) -> bool {
        self.revision == other.revision
    }
}

impl DrawingWindow {
    fn new() -> Self {
        let frame = Rc::new(Cell::new(0_u64));
        let ticker = RenderingTicker::get();
        let count = frame.clone();
        // The renderer ticks after the whole frame, not after each drawing callback.
        spawn_in_window(async move {
            loop {
                ticker.tick().await;
                count.set(count.get().wrapping_add(1));
            }
        });
        Self {
            accounts: Rc::default(),
            revision: State::create_in_scope(0, ScopeId::ROOT),
            frame,
        }
    }

    fn phase(&self, plugin: &PluginId) -> Option<DrawPhase> {
        let _revision = self.revision.read();
        self.accounts.borrow().phase(plugin)
    }

    fn begin(&self, plugin: &PluginId, now: Instant) -> bool {
        let paint = self
            .accounts
            .borrow_mut()
            .begin(plugin, self.frame.get(), now);
        if paint == BudgetPaint::JustPaused {
            self.changed();
        }
        paint == BudgetPaint::Draw
    }

    fn changed(&self) {
        let mut revision = self.revision;
        if let Some(mut revision) = revision.try_write() {
            *revision = revision.wrapping_add(1);
        }
    }

    fn expire(&self, plugin: &PluginId, until: Instant) {
        let expired = self
            .accounts
            .borrow_mut()
            .expire(plugin, until, Instant::now());
        if expired {
            self.changed();
        }
    }

    fn resume(&self, plugin: &PluginId) {
        let resumed = self.accounts.borrow_mut().resume(plugin);
        if resumed {
            self.changed();
        }
    }
}

/// Forget accounts when pages cease to be available, not when the person switches tabs.
pub(crate) fn forget_drawings_without_pages(view: &View) {
    let Some(window) = try_consume_root_context::<DrawingWindow>() else {
        return;
    };
    let here = OperatingSystem::current();
    let removed = window.accounts.borrow_mut().retain(|id| {
        view.plugins.iter().any(|plugin| {
            &plugin.id == id
                && extension_has_pages(plugin)
                && plugin
                    .manifest
                    .contributions
                    .pages
                    .iter()
                    .any(|page| page_is_here(page, here))
        })
    });
    if removed {
        window.changed();
    }
}

#[derive(Clone)]
struct DrawingPage {
    window: DrawingWindow,
    phase: Memo<Option<DrawPhase>>,
}

#[derive(PartialEq)]
struct DrawingNotice {
    window: DrawingWindow,
    plugin: PluginId,
    phase: Option<DrawPhase>,
    palette: Palette,
}

impl Component for DrawingNotice {
    fn render(&self) -> impl IntoElement {
        let until = use_reactive(&match self.phase {
            Some(DrawPhase::CoolingUntil(until)) => Some(until),
            _ => None,
        });
        use_future({
            let (window, plugin) = (self.window.clone(), self.plugin.clone());
            move || {
                let (window, plugin, until) = (window.clone(), plugin.clone(), *until.read());
                async move {
                    if let Some(until) = until {
                        let _ = crate::link::after(until.saturating_duration_since(Instant::now()))
                            .await;
                        window.expire(&plugin, until);
                    }
                }
            }
        });
        let message = if self.phase.is_some() {
            i18n::tr("This extension draws too much and was paused to keep Peersus responsive.")
        } else {
            i18n::tr("This window cannot manage any more extension drawings.")
        };
        rect()
            .width(Size::fill())
            // Above even nested extension overlays, without escaping the page's clip.
            .layer(Layer::Relative(
                OVERLAY_RAISE * (peersus_api::elements::MAX_DEPTH as i16 + 1),
            ))
            .padding(10.)
            .spacing(6.)
            .background(self.palette.raised)
            .child(
                label()
                    .width(Size::fill())
                    .text(message.to_owned())
                    .color(self.palette.text),
            )
            .maybe_child(self.phase.map(|_| {
                let (window, plugin) = (self.window.clone(), self.plugin.clone());
                Button::new()
                    .child(i18n::tr("Resume").to_owned())
                    .on_press(move |_| window.resume(&plugin))
            }))
    }
}

/// Whether a page's root asks for a share of the room around it rather than its own height.
fn shares_the_room(node: &Node) -> bool {
    match node {
        Node::Interval { child, .. } => shares_the_room(child),
        Node::Box { style, .. } | Node::Canvas { style, .. } => {
            matches!(style.height, Length::Percent(_) | Length::Flex(_))
        }
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
        let window = use_provide_root_context(DrawingWindow::new);
        use_hook(|| {
            let phase = {
                let mut accounts = window.accounts.borrow_mut();
                accounts.track(&self.plugin);
                accounts.phase(&self.plugin)
            };
            if let Some(DrawPhase::CoolingUntil(until)) = phase {
                window.expire(&self.plugin, until);
            }
        });
        let phase = use_memo({
            let (window, plugin) = (window.clone(), self.plugin.clone());
            move || window.phase(&plugin)
        });
        use_provide_context(|| DrawingPage {
            window: window.clone(),
            phase,
        });
        provide_context(crate::canvas_view::Source(self.link.clone()));
        let paused = phase();
        let shows_notice = paused != Some(DrawPhase::Active);

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
                measurement_page: self.measurement_page.clone(),
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
        let node = if self.embedded {
            self.view.clone()
        } else {
            self.view.as_ref().map(as_page)
        };
        let shares = node.as_ref().is_some_and(shares_the_room);
        let fills = !self.embedded && shares;
        let body = rect()
            .width(Size::fill())
            .maybe(self.embedded && shares, |el| {
                el.height(Size::px(EMBEDDED_HEIGHT))
            })
            .maybe(fills, |el| {
                el.height(Size::flex(1.)).content(Content::flex())
            })
            .maybe(self.embedded || fills || shows_notice, |el| {
                el.overflow(Overflow::Clip)
            });
        let mut content = match &node {
            Some(node) => draw(node, self.palette, &context),
            None => label()
                .text(i18n::tr("Loading…").to_owned())
                .color(self.palette.muted)
                .into_element(),
        };
        // An explicit key keeps the page's controls mounted when the notice is inserted.
        // Healthy pages have no extra layout nodes for the notice.
        let key = match &mut content {
            Element::Element { key, .. } | Element::Component { key, .. } => key,
        };
        if matches!(key, DiffKey::None | DiffKey::DefaultU64(_)) {
            *key = DiffKey::from(&(node.as_ref().map(std::mem::discriminant), &*key));
        }
        body.maybe_child(shows_notice.then(|| DrawingNotice {
            window,
            plugin: self.plugin.clone(),
            phase: paused,
            palette: self.palette,
        }))
        .child(content)
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
        Node::Text {
            text,
            tone,
            lines,
            size,
        } => {
            let (usual_size, colour) = match tone {
                Tone::Title => (22., palette.text),
                Tone::Body => (15., palette.text),
                Tone::Muted => (13., palette.muted),
                Tone::Figure => (40., palette.text),
            };
            label()
                .text(text.clone())
                .font_size(size.map_or(usual_size, f32::from))
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
            canvas: None,
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
            layers,
            shapes,
        } => {
            // What the window keeps of each of them, of this extension alone: a layer it has not kept draws nothing.
            let kept = if layers.is_empty() {
                None
            } else {
                try_consume_context::<crate::layers::KeptHandle>()
                    .map(|handle| handle.0.read().clone())
            };
            drawing_with_layers(
                *width,
                *height,
                layers,
                Some(&context.send.plugin),
                kept.as_ref(),
                shapes,
                palette,
            )
        }
        Node::Canvas {
            id,
            style,
            observe,
            a11y,
        } => CanvasBox {
            key: peersus_app::ipc::FrameKey {
                plugin: context.send.plugin.clone(),
                page: context.send.page.clone(),
                canvas: id.clone(),
            },
            a11y: a11y.clone().into(),
            shell: BoxNode {
                id: Some(id.clone()),
                style: Style {
                    clip: true,
                    ..style.clone()
                },
                children: Vec::new(),
                observe: observe.clone(),
                palette,
                context: context.clone(),
                canvas: None,
            },
        }
        .into(),
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

/// A shape of a drawing, ready to paint: an outline in the drawing's pixels, and how the person's colours say it is stroked and filled.
struct Painted {
    path: SkPath,
    fill: Option<Rgba>,
    stroke: Option<Rgba>,
    opacity: f32,
    pen: Paint,
    /// Width of the stroke in drawing pixels (zero: no stroke).
    line: f32,
}

impl Painted {
    /// `shape` with `path` as its outline; `None` for an outline no parser reads.
    fn of(path: Option<SkPath>, shape: &Shape, palette: Palette) -> Option<Self> {
        let mut pen = Paint::default();
        pen.set_anti_alias(true);
        if shape.round {
            pen.set_stroke_cap(PaintCap::Round);
            pen.set_stroke_join(PaintJoin::Round);
        }
        pen.set_stroke_width(f32::from(shape.line) / 10.);
        Some(Painted {
            path: path?,
            fill: paint(palette, shape.fill),
            stroke: paint(palette, shape.stroke).filter(|_| shape.line != 0),
            opacity: shape
                .opacity
                .map_or(1., |opacity| f32::from(opacity) / 10_000.),
            pen,
            line: f32::from(shape.line) / 10.,
        })
    }
}

/// The outline of a shape as the parser reads it. Node validation already permits only SVG path characters; the parser's own
/// precondition is kept for internal callers too, because a byte of zero would make it refuse rather than fail.
fn outline(path: &str) -> Option<SkPath> {
    (!path.as_bytes().contains(&0))
        .then(|| SkPath::from_svg(path))
        .flatten()
}

/// The shapes of a drawing, all of them read again: what a drawing without kept layers paints.
fn painted(shapes: &[Shape], palette: Palette) -> Vec<Painted> {
    shapes
        .iter()
        .filter_map(|shape| Painted::of(outline(&shape.path), shape, palette))
        .collect()
}

/// Draw `painted` in the canvas's current transform: fill and stroke overlap before the opacity of a shape is applied, and only that
/// translucent combination is drawn in a layer, which covers what `area` says of the shape (in the canvas's current units) at most.
fn stroke_painted(canvas: &Canvas, painted: &[Painted], area: impl Fn(&Painted) -> SkRect) {
    let color = |(r, g, b, a): Rgba| SkColor::from_argb(a, r, g, b);
    for drawn in painted {
        if drawn.opacity == 0. {
            continue;
        }
        let fill = drawn.fill.map(color);
        let stroke = drawn.stroke.map(color);
        let group = (drawn.opacity < 1. && fill.is_some() && stroke.is_some())
            .then(|| canvas.save_layer_alpha_f(area(drawn), drawn.opacity));
        let alpha = if group.is_some() { 1. } else { drawn.opacity };
        let mut pen = drawn.pen.clone();
        for (color, style) in [(fill, PaintStyle::Fill), (stroke, PaintStyle::Stroke)] {
            if let Some(color) = color {
                pen.set_color(color)
                    .set_alpha_f(f32::from(color.a()) / 255. * alpha)
                    .set_style(style);
                canvas.draw_path(&drawn.path, &pen);
            }
        }
        if let Some(saved) = group {
            canvas.restore_to_count(saved);
        }
    }
}

/// One kept layer placed on a drawing, ready to paint: the outlines the window parsed for it once, and where it goes.
struct PlacedPaint {
    painted: Vec<Painted>,
    /// Millionths: a million draws the layer as it was kept.
    scale: f32,
    /// Thousandths of a pixel of the drawing.
    at: (f32, f32),
}

/// A drawing of `shapes` and of the kept layers placed on it, under them, `width` by `height` pixels: the outlines are parsed once for
/// each change of the drawing and painted straight into Freya's canvas, so scaling the page does not scale a cached bitmap.
#[cfg(all(test, feature = "headless"))]
pub(crate) fn drawing_view(width: u16, height: u16, shapes: &[Shape], palette: Palette) -> Element {
    drawing_with_layers(width, height, &[], None, None, shapes, palette)
}

/// [`drawing_view`] with the kept layers the page places: `kept` is what the window holds for `plugin`, and a layer it has not kept (or
/// that was let go of) draws nothing. A placed layer's stroke width is pixels of the drawing, whatever its scale.
pub(crate) fn drawing_with_layers(
    width: u16,
    height: u16,
    placed: &[Placed],
    plugin: Option<&peersus_core::plugins::PluginId>,
    kept: Option<&crate::layers::Kept>,
    shapes: &[Shape],
    palette: Palette,
) -> Element {
    let background = rect()
        .width(Size::px(f32::from(width)))
        .height(Size::px(f32::from(height)))
        .interactive(false);
    let page = plugin.and_then(|_| try_consume_context::<DrawingPage>());
    if let Some(page) = &page
        && (page.phase)() != Some(DrawPhase::Active)
    {
        // A drawing has no separate background color. Leave its backdrop and layout alone,
        // without parsing paths, preparing layers, or installing a paint callback.
        return background.into();
    }
    let meter = page
        .zip(plugin.cloned())
        .map(|(page, plugin)| (page.window, plugin));
    let own = painted(shapes, palette);
    let budget = kept.map(crate::layers::Kept::budget);
    let layers: Vec<PlacedPaint> = placed
        .iter()
        .filter_map(|placed| {
            let layer = kept?.layer(plugin?, &placed.id)?;
            let scale = placed.scale as f32 / 1_000_000.;
            if scale <= 0. {
                return None;
            }
            let paths = layer.paths(budget);
            let painted = paths
                .iter()
                .zip(&layer.shapes)
                .filter_map(|(path, shape)| Painted::of(path.clone(), shape, palette))
                .map(|mut paint| {
                    // Placement moves the outline, but its stroke is measured in drawing pixels.
                    paint.pen.set_stroke_width(paint.line / scale);
                    paint
                })
                .collect();
            Some(PlacedPaint {
                painted,
                scale,
                at: (placed.at[0] as f32 / 1000., placed.at[1] as f32 / 1000.),
            })
        })
        .collect();
    let drawing = SkRect::from_wh(f32::from(width), f32::from(height));
    background
        .background(Fill::Callback(RenderCallback::new(move |context| {
            let RenderTarget::Background { canvas, .. } = &context.target else {
                return;
            };
            let started = if let Some((meter, plugin)) = &meter {
                let now = Instant::now();
                if !meter.begin(plugin, now) {
                    // The same frame can contain older callbacks for this extension.
                    return;
                }
                Some(Instant::now())
            } else {
                None
            };
            // The kept layers in the order they are placed, and over them the drawing's own shapes. The canvas is already at the
            // drawing's top-left corner and clipped to it, so a layer placed away from it is not drawn at all.
            for layer in &layers {
                if layer.painted.is_empty() {
                    continue;
                }
                // What of the drawing the layer may paint, in the layer's own units: what a translucent shape's layer covers.
                let inside = SkRect::new(
                    (drawing.left() - layer.at.0) / layer.scale,
                    (drawing.top() - layer.at.1) / layer.scale,
                    (drawing.right() - layer.at.0) / layer.scale,
                    (drawing.bottom() - layer.at.1) / layer.scale,
                );
                let saved = canvas.save();
                canvas.translate((layer.at.0, layer.at.1));
                canvas.scale((layer.scale, layer.scale));
                stroke_painted(canvas, &layer.painted, |painted| {
                    covered(painted, layer.scale, inside)
                });
                canvas.restore_to_count(saved);
            }
            // A translucent shape's layer covers that shape, never the whole drawing: a full-surface layer per shape was most of
            // a zoom's frame (measured, `cargo xtask bench --map-zoom`).
            stroke_painted(canvas, &own, |painted| covered(painted, 1., drawing));
            if let Some(((meter, plugin), started)) = meter.as_ref().zip(started) {
                let elapsed = started.elapsed();
                meter.accounts.borrow_mut().charge(plugin, elapsed);
            }
        })))
        .into()
}

/// What a shape paints, its stroke included, within `inside`, in units where a stroke pixel is `scale`: the most a translucent shape's
/// layer has to cover. Empty when the shape is wholly outside.
fn covered(painted: &Painted, scale: f32, inside: SkRect) -> SkRect {
    let bounds = painted.path.bounds();
    // A sharp corner's miter reaches out up to the miter limit times half the stroke; a round join or cap only half of it.
    let reach = if painted.pen.stroke_join() == PaintJoin::Miter {
        painted.pen.stroke_miter().max(1.)
    } else {
        1.
    };
    let half = painted.line / 2. * reach / scale;
    let mut covered = SkRect::new(
        bounds.left() - half,
        bounds.top() - half,
        bounds.right() + half,
        bounds.bottom() + half,
    );
    if covered.intersect(inside) {
        covered
    } else {
        SkRect::new_empty()
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
        Node::Box { style, .. } | Node::Canvas { style, .. } => {
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
struct CanvasBox {
    key: peersus_app::ipc::FrameKey,
    a11y: Rc<[peersus_api::canvas::CanvasA11y]>,
    shell: BoxNode,
}

impl Component for CanvasBox {
    fn render_key(&self) -> DiffKey {
        DiffKey::from(&(std::any::TypeId::of::<Self>(), &self.key))
    }

    fn render(&self) -> impl IntoElement {
        BoxNode {
            canvas: Some(crate::canvas_view::mount(&self.key, self.a11y.clone())),
            ..self.shell.clone()
        }
    }
}

#[derive(Clone, PartialEq)]
struct BoxNode {
    id: Option<String>,
    style: Style,
    children: Vec<Node>,
    observe: Vec<Observe>,
    palette: Palette,
    context: Context,
    canvas: Option<crate::canvas_view::Mounted>,
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
        let preview = use_hook(|| {
            self.canvas
                .is_some()
                .then(|| (crate::follow::Motion::default(), State::create(0_u32)))
        });
        let following = style.follow.zip(preview);
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
        // Only a box explicitly requesting pinch may capture two contacts.
        let contacts = use_state(|| std::cell::Cell::new([None::<(u64, i32, i32)>; 2]));
        let initial_distance = use_state(|| std::cell::Cell::new(0_f64));
        // The two-touch line's last angle (radians) and how far it has turned since the second touch, unwrapped past a half turn.
        let turned = use_state(|| std::cell::Cell::new((0_f64, 0_f64)));
        // What this box last measured, and what its extension was last told, shared with the registry below (never the
        // toolkit's own storage, which another component's removal must not touch).
        let measured = use_hook(|| std::rc::Rc::new(std::cell::Cell::new(None::<Bounds>)));
        let reported = use_hook(|| std::rc::Rc::new(std::cell::Cell::new(None::<Bounds>)));
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
        // A previewed canvas needs distinct timestamps to match gestures to frame acknowledgments.
        let next_at = {
            let motion = following.as_ref().map(|(_, (motion, _))| motion.clone());
            move || {
                let now = crate::monotonic_ms();
                motion
                    .as_ref()
                    .map_or(now, |motion| motion.gestures().stamp(now))
            }
        };
        if let Some(id) = &self.id {
            // Says the box's measure if `say_measure` wants it: `input` before pointer or wheel input is said.
            let say: Option<std::rc::Rc<dyn Fn(bool)>> =
                self.observe.contains(&Observe::Size).then(|| {
                    let is_canvas = self.canvas.is_some();
                    let metrics = self
                        .canvas
                        .as_ref()
                        .and_then(|canvas| canvas.measure(palette, || reported.set(None)));
                    let padding = style.padding;
                    let (send, id) = (self.context.send.handler.as_ref().clone(), id.clone());
                    let (measured, reported) = (measured.clone(), reported.clone());
                    std::rc::Rc::new(move |input: bool| {
                        let Some(bounds) = measured.get() else {
                            return;
                        };
                        if (!is_canvas || metrics.is_some())
                            && say_measure(reported.get(), bounds, input)
                        {
                            reported.set(Some(bounds));
                            send.call(match metrics {
                                Some(metrics) => metrics.event(id.clone(), bounds, padding),
                                None => Event::Size {
                                    id: id.clone(),
                                    x: bounds.0,
                                    y: bounds.1,
                                    width: bounds.2,
                                    height: bounds.3,
                                },
                            });
                        }
                    }) as std::rc::Rc<dyn Fn(bool)>
                });
            if let Some(say) = &say {
                let key = (
                    self.context.send.plugin.clone(),
                    if self.canvas.is_some() {
                        self.context.send.page.clone()
                    } else {
                        self.context.send.measurement_page.clone()
                    },
                    id.clone(),
                );
                remember_measure(&key, serial, {
                    let (say, reported) = (say.clone(), reported.clone());
                    std::rc::Rc::new(move || {
                        // Said again whatever was said before: another place of the same page may have said its own.
                        reported.set(None);
                        say(false);
                    })
                });
                if let Some(previous) = registered.borrow_mut().replace(key.clone())
                    && previous != key
                {
                    forget_measure(&previous, serial);
                }
                if self.canvas.is_some() {
                    say(false);
                }
                let (say, measured) = (say.clone(), measured.clone());
                node = node.on_sized(move |e: freya::prelude::Event<SizedEventData>| {
                    measured.set(Some((
                        (e.area.min_x() * 1000.) as i32,
                        (e.area.min_y() * 1000.) as i32,
                        (e.area.width() * 1000.) as u32,
                        (e.area.height() * 1000.) as u32,
                    )));
                    say(false);
                });
            } else if let Some(previous) = registered.borrow_mut().take() {
                forget_measure(&previous, serial);
            }
            if self.observe.contains(&Observe::Wheel) {
                let (send, id, say) = (
                    self.context.send.handler.as_ref().clone(),
                    id.clone(),
                    say.clone(),
                );
                let (following, next_at) = (following.clone(), next_at.clone());
                node = node.on_wheel(move |e: freya::prelude::Event<WheelEventData>| {
                    e.stop_propagation();
                    if let Some(say) = &say {
                        say(true);
                    }
                    let at = next_at();
                    let x = (e.global_location.x * 1000.) as i32;
                    let y = (e.global_location.y * 1000.) as i32;
                    let dy = (e.delta_y * 1000.) as i32;
                    if let Some((follow, (motion, moved))) = &following {
                        let about = motion.about(x as f32 / 1000., y as f32 / 1000.);
                        motion
                            .gestures()
                            .wheel(at, *follow, f64::from(dy) / 1000., about);
                        let mut moved = *moved;
                        let next = moved.peek().wrapping_add(1);
                        moved.set(next);
                    }
                    send.call(Event::Wheel {
                        id: id.clone(),
                        x,
                        y,
                        dx: (e.delta_x * 1000.) as i32,
                        dy,
                        at,
                    });
                });
            }
            let emit = {
                let (send, id) = (self.context.send.handler.as_ref().clone(), id.clone());
                let (following, next_at) = (following.clone(), next_at.clone());
                EventHandler::new(move |(phase, x, y): (PointerPhase, i32, i32)| {
                    if let Some(say) = &say {
                        say(true);
                    }
                    let at = next_at();
                    if let Some((follow, (motion, moved))) = &following {
                        let mut gestures = motion.gestures();
                        match phase {
                            PointerPhase::Down => {
                                if follow.drag {
                                    gestures.press(x, y);
                                }
                            }
                            PointerPhase::Move => {
                                if follow.drag {
                                    gestures.drag(at, x, y);
                                }
                            }
                            PointerPhase::Up | PointerPhase::Cancel => gestures.release(),
                        }
                        drop(gestures);
                        let mut moved = *moved;
                        let next = moved.peek().wrapping_add(1);
                        moved.set(next);
                    }
                    send.call(Event::Pointer {
                        id: id.clone(),
                        phase,
                        x,
                        y,
                        at,
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
            let pinch = self.observe.contains(&Observe::Pinch);
            let rotate = self.observe.contains(&Observe::Rotate);
            // Two touches are followed for either gesture; each is sent only to a box that asked for it.
            let two_touches = pinch || rotate;
            let pinch_event = {
                let (id, send) = (id.clone(), self.context.send.handler.as_ref().clone());
                let (following, next_at) = (following.clone(), next_at.clone());
                EventHandler::new(move |begin: bool| {
                    let [Some((_, ax, ay)), Some((_, bx, by))] = contacts.peek().get() else {
                        return;
                    };
                    let (dx, dy) = (f64::from(bx) - f64::from(ax), f64::from(by) - f64::from(ay));
                    let distance = dx.hypot(dy).max(1.);
                    if begin {
                        initial_distance.peek().set(distance);
                    }
                    let (x, y) = (ax.midpoint(bx), ay.midpoint(by));
                    let at = next_at();
                    if pinch {
                        let scale = (10000. * distance / initial_distance.peek().get())
                            .round()
                            .max(1.) as u32;
                        if let Some((follow, (motion, moved))) = &following
                            && follow.pinch
                        {
                            let about = motion.about(x as f32 / 1000., y as f32 / 1000.);
                            motion.gestures().pinch(at, scale, begin, about);
                            let mut moved = *moved;
                            let next = moved.peek().wrapping_add(1);
                            moved.set(next);
                        }
                        send.call(Event::Pinch {
                            id: id.clone(),
                            x,
                            y,
                            scale,
                            begin,
                            at,
                        });
                    }
                    if rotate {
                        // Screen y grows downwards, so a growing angle is a clockwise turn.
                        let angle = dy.atan2(dx);
                        let (last, total) = turned.peek().get();
                        let total = if begin {
                            0.
                        } else {
                            let step = (angle - last + std::f64::consts::PI)
                                .rem_euclid(std::f64::consts::TAU)
                                - std::f64::consts::PI;
                            total + step
                        };
                        turned.peek().set((angle, total));
                        let turn = (total.to_degrees() * 100.)
                            .round()
                            .clamp(f64::from(i32::MIN), f64::from(i32::MAX))
                            as i32;
                        // Its own `at`, later than the pinch's: every gesture event of a box is later than the one before it.
                        let at = if pinch { next_at() } else { at };
                        send.call(Event::Rotate {
                            id: id.clone(),
                            x,
                            y,
                            turn,
                            begin,
                            at,
                        });
                    }
                })
            };
            node = node
                .on_pointer_down({
                    let emit = emit.clone();
                    let pinch_event = pinch_event.clone();
                    move |e: freya::prelude::Event<PointerEventData>| {
                        e.stop_propagation();
                        // An element pressed by one finger stays that finger's until it lifts; the
                        // mouse, whose release may be lost outside the window, can always press again.
                        let pointer = finger(&e);
                        if two_touches && let Some(id) = pointer {
                            let mut slots = contacts.peek().get();
                            if slots.iter().flatten().any(|s| s.0 == id) {
                                return;
                            }
                            let Some(slot) = slots.iter_mut().find(|s| s.is_none()) else {
                                return;
                            };
                            let (x, y) = position(&e);
                            *slot = Some((id, x, y));
                            contacts.peek().set(slots);
                            if slots.iter().all(Option::is_some) {
                                active.peek().set(None);
                                emit.call((PointerPhase::Cancel, x, y));
                                pinch_event.call(true);
                                return;
                            }
                        }
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
                        if two_touches && let Some(id) = finger(&e) {
                            let mut slots = contacts.peek().get();
                            if let Some(slot) = slots.iter_mut().flatten().find(|s| s.0 == id) {
                                let (x, y) = position(&e);
                                *slot = (id, x, y);
                                contacts.peek().set(slots);
                                if slots.iter().all(Option::is_some) {
                                    pinch_event.call(false);
                                    return;
                                }
                            }
                        }
                        if active.peek().get() == Some(finger(&e)) {
                            let (x, y) = position(&e);
                            emit.call((PointerPhase::Move, x, y));
                        }
                    }
                })
                .on_global_pointer_up({
                    let emit = emit.clone();
                    move |e: freya::prelude::Event<PointerEventData>| {
                        if two_touches && let Some(id) = finger(&e) {
                            contacts
                                .peek()
                                .set(contacts.peek().get().map(|s| s.filter(|t| t.0 != id)));
                        }
                        if active.peek().get() == Some(finger(&e)) {
                            active.peek().set(None);
                            let (x, y) = position(&e);
                            emit.call((PointerPhase::Up, x, y));
                        }
                    }
                })
                .on_touch_cancel(move |e: freya::prelude::Event<TouchEventData>| {
                    if two_touches {
                        contacts.peek().set(
                            contacts
                                .peek()
                                .get()
                                .map(|s| s.filter(|t| t.0 != e.finger_id)),
                        );
                    }
                    if active.peek().get() == Some(Some(e.finger_id)) {
                        active.peek().set(None);
                        emit.call((PointerPhase::Cancel, 0, 0));
                    }
                });
        }
        if let Some(canvas) = &self.canvas {
            node = node.child(canvas.bitmap(
                following.map(|(_, preview)| preview),
                self.context.send.handler.clone(),
            ));
        } else {
            let content = if style.wrap {
                Content::wrap_spacing(f32::from(style.gap))
            } else if matches_largest_sibling(style.horizontal, &self.children) {
                // The toolkit sizes a child to its largest sibling only in a box laid out to fit them.
                Content::fit()
            } else {
                Content::flex()
            };
            node = node.content(content).children(
                self.children
                    .iter()
                    .map(|child| draw(child, palette, &self.context)),
            );
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
        let clock = use_reactive(&(
            self.id.clone(),
            self.milliseconds,
            self.context.send.clone(),
        ));
        use_future(move || {
            let (id, milliseconds, send) = clock.read().clone();
            async move {
                let mut ticks = crate::link::clock(
                    crate::monotonic_ms,
                    std::time::Duration::from_millis(u64::from(milliseconds)),
                );
                while ticks.changed().await.is_ok() {
                    let at = *ticks.borrow_and_update();
                    send.handler.call(Event::Tick { id: id.clone(), at });
                }
            }
        });
        draw(&self.child, self.palette, &self.context)
    }
}

#[cfg(all(test, feature = "headless"))]
#[test]
fn an_open_pages_tick_cadence_changes_without_reopening_it() {
    use std::{
        cell::Cell,
        rc::Rc,
        time::{Duration, Instant},
    };

    let ticks = Rc::new(Cell::new(0));
    let sink = ticks.clone();
    let (mut window, mut milliseconds) = freya_testing::TestingRunner::new(
        move || {
            let sink = sink.clone();
            IntervalNode {
                id: "clock".into(),
                milliseconds: use_consume::<State<u16>>()(),
                child: Node::parse(&serde_json::json!({"k":"text","text":"Map"})).unwrap(),
                palette: crate::theme::DARK,
                context: Context {
                    send: Send {
                        handler: Rc::new(EventHandler::new(move |_| sink.set(sink.get() + 1))),
                        plugin: peersus_core::plugins::PluginId::new("fixture").unwrap(),
                        page: "map".into(),
                        measurement_page: "map".into(),
                    },
                    avatars: Avatars::default(),
                    reduced: true,
                    pictures: crate::shelf::Held(Default::default()),
                },
            }
        },
        (200., 200.).into(),
        |runner| runner.provide_root_context(|| State::create(5_000_u16)),
        1.,
    );
    window.poll_n(Duration::from_millis(5), 4);
    milliseconds.set(100);
    let deadline = Instant::now() + Duration::from_secs(2);
    while ticks.get() < 2 && Instant::now() < deadline {
        window.poll_n(Duration::from_millis(10), 2);
    }
    assert!(
        ticks.get() >= 2,
        "the mounted page ignored its faster cadence"
    );
    milliseconds.set(5_000);
    window.poll_n(Duration::from_millis(5), 4);
    let count = ticks.get();
    let deadline = Instant::now() + Duration::from_millis(450);
    while Instant::now() < deadline {
        window.poll_n(Duration::from_millis(10), 2);
    }
    assert_eq!(
        ticks.get(),
        count,
        "the previous fast clock is still running"
    );
}

#[cfg(test)]
mod measure_tests {
    use super::*;

    #[test]
    fn when_one_place_of_a_page_goes_the_place_that_stays_says_its_size_again() {
        let template: Page = serde_json::from_value(serde_json::json!({
            "id":"mon","placement":"settings","label":"Info","surface":"elements",
            "source":{"service":"info.page","method":"view"}
        }))
        .unwrap();
        let copy = Page {
            id: "show-info".into(),
            placement: Placement::Primary,
            ..template.clone()
        };
        let pages = [template.clone(), copy.clone()];
        let key: MeasureKey = (
            peersus_core::plugins::PluginId::new("monitor").unwrap(),
            measurement_page(&pages, &template),
            "width".into(),
        );
        let said = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let place = |name: &'static str| -> std::rc::Rc<dyn Fn()> {
            let said = said.clone();
            std::rc::Rc::new(move || said.borrow_mut().push(name))
        };
        let (page, settings) = (next_view(), next_view());
        let primary_key = (
            key.0.clone(),
            measurement_page(&pages, &copy),
            key.2.clone(),
        );
        remember_measure(&primary_key, page, place("page"));
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

    #[test]
    fn a_translucent_shapes_layer_covers_its_sharp_corner_and_no_more_of_the_drawing() {
        let shape = |round| Shape {
            // A thin wedge whose tip at x=100 is sharp: a mitred stroke reaches well past it.
            path: "M0 0 L100 5 L0 10 Z".into(),
            stroke: Colour::Border,
            line: 100,
            fill: Colour::Accent,
            opacity: Some(5000),
            round,
        };
        let drawing = SkRect::from_wh(1000., 1000.);
        let sharp = Painted::of(
            outline(&shape(false).path),
            &shape(false),
            crate::theme::DARK,
        )
        .unwrap();
        let area = covered(&sharp, 1., drawing);
        // Ten pixels of stroke with Skia's miter limit of four: the tip may reach twenty pixels past the outline.
        assert!(area.right() >= 120., "the miter tip is cut: {area:?}");
        assert!(
            area.width() < drawing.width(),
            "the layer must not cover the whole drawing"
        );
        let round =
            Painted::of(outline(&shape(true).path), &shape(true), crate::theme::DARK).unwrap();
        assert!((covered(&round, 1., drawing).right() - 105.).abs() < 0.01);
        // Wholly outside: nothing to cover.
        assert!(covered(&sharp, 1., SkRect::new(500., 500., 600., 600.)).is_empty());
    }

    #[test]
    fn a_box_that_only_moves_is_not_said_until_input_from_it_needs_where_it_is() {
        let first = (0, 100_000, 200_000, 120_000);
        assert!(say_measure(None, first, false), "the first measure");
        // Scrolled: the same box, lower. Not said on its own, however many frames the scroll takes.
        let scrolled = (0, 40_000, 200_000, 120_000);
        assert!(!say_measure(Some(first), scrolled, false));
        // Touched after it moved: said first, so the touch is read against where the box is.
        assert!(say_measure(Some(first), scrolled, true));
        assert!(
            !say_measure(Some(scrolled), scrolled, true),
            "nothing moved"
        );
        // A change of size is always said.
        assert!(say_measure(
            Some(scrolled),
            (0, 40_000, 180_000, 120_000),
            false
        ));
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

#[cfg(all(test, feature = "headless"))]
#[test]
fn text_size_changes_the_rendered_glyphs_without_changing_the_tone() {
    let palette = crate::theme::DARK;
    let (mut window, ()) = freya_testing::TestingRunner::new(
        move || {
            let context = Context {
                send: Send {
                    handler: Rc::new(EventHandler::new(|_: Event| {})),
                    plugin: peersus_core::plugins::PluginId::new("fixture").unwrap(),
                    page: "main".into(),
                    measurement_page: "main".into(),
                },
                avatars: Avatars::default(),
                reduced: true,
                pictures: crate::shelf::Held(Default::default()),
            };
            [None, Some(13), Some(9), Some(34)].into_iter().fold(
                rect()
                    .width(Size::fill())
                    .height(Size::fill())
                    .background(palette.surface),
                |page, size| {
                    let text = Node::Text {
                        text: "Canada".into(),
                        tone: Tone::Muted,
                        lines: Some(1),
                        size,
                    };
                    page.child(
                        rect()
                            .height(Size::px(52.))
                            .width(Size::fill())
                            .child(draw(&text, palette, &context)),
                    )
                },
            )
        },
        (220., 208.).into(),
        |_| {},
        1.,
    );
    window.sync_and_update();
    let image = image::load_from_memory(window.render().as_bytes())
        .unwrap()
        .to_rgb8();
    let (r, g, b) = palette.surface;
    let rows = |start| {
        (start..start + 52)
            .filter(|&y| (0..220).any(|x| image.get_pixel(x, y).0 != [r, g, b]))
            .count()
    };
    assert_eq!(rows(0), rows(52));
    assert!(0 < rows(104) && rows(104) < rows(0) && rows(156) > 2 * rows(0));
    let (r, g, b) = palette.muted;
    assert!((156..208).any(|y| (0..220).any(|x| image.get_pixel(x, y).0 == [r, g, b])));
}

#[cfg(all(test, feature = "headless"))]
#[test]
fn two_touch_contacts_emit_pinch_instead_of_continuing_a_drag() {
    use freya_testing::TestingRunner;
    use freya_testing::prelude::{PlatformEvent, TouchEventName, TouchPhase};
    use std::{cell::RefCell, rc::Rc, time::Duration};

    let events = Rc::new(RefCell::new(Vec::<Event>::new()));
    let sink = events.clone();
    let (mut window, ()) = TestingRunner::new(
        move || {
            let sink = sink.clone();
            BoxNode {
                canvas: None,
                id: Some("canvas".into()),
                style: Style {
                    width: Length::Px(200),
                    height: Length::Px(200),
                    ..Style::default()
                },
                children: Vec::new(),
                observe: vec![Observe::Pinch],
                palette: crate::theme::DARK,
                context: Context {
                    send: Send {
                        handler: Rc::new(EventHandler::new(move |event| {
                            sink.borrow_mut().push(event);
                        })),
                        plugin: peersus_core::plugins::PluginId::new("fixture").unwrap(),
                        page: "main".into(),
                        measurement_page: "main".into(),
                    },
                    avatars: Avatars::default(),
                    reduced: true,
                    pictures: crate::shelf::Held(Default::default()),
                },
            }
        },
        (200., 200.).into(),
        |_| {},
        1.,
    );
    window.poll_n(Duration::from_millis(2), 12);
    for (finger_id, phase, name, x) in [
        (1, TouchPhase::Started, TouchEventName::TouchStart, 60.),
        (2, TouchPhase::Started, TouchEventName::TouchStart, 140.),
        (1, TouchPhase::Moved, TouchEventName::TouchMove, 20.),
        (2, TouchPhase::Moved, TouchEventName::TouchMove, 180.),
        (1, TouchPhase::Moved, TouchEventName::TouchMove, 60.),
        (2, TouchPhase::Moved, TouchEventName::TouchMove, 140.),
        (2, TouchPhase::Ended, TouchEventName::TouchEnd, 140.),
        (1, TouchPhase::Ended, TouchEventName::TouchEnd, 60.),
    ] {
        window.send_event(PlatformEvent::Touch {
            name,
            location: (x, 100.).into(),
            finger_id,
            phase,
            force: None,
        });
        window.poll_n(Duration::from_millis(2), 4);
    }
    let events = events.borrow();
    assert!(matches!(
        events.first(),
        Some(Event::Pointer {
            phase: PointerPhase::Down,
            ..
        })
    ));
    assert!(matches!(
        events.get(1),
        Some(Event::Pointer {
            phase: PointerPhase::Cancel,
            ..
        })
    ));
    let scales: Vec<_> = events[2..]
        .iter()
        .map(|event| match event {
            Event::Pinch { scale, begin, .. } => (*begin, *scale),
            other => panic!("expected only two-contact samples after cancel, got {other:?}"),
        })
        .collect();
    assert_eq!(
        scales,
        [
            (true, 10000),
            (false, 15000),
            (false, 20000),
            (false, 15000),
            (false, 10000)
        ]
    );
}

#[cfg(all(test, feature = "headless"))]
#[test]
fn a_declarative_drawing_forwards_wheel_input_without_previewing_its_subtree() {
    use freya_testing::TestingRunner;
    use peersus_api::elements::Follow;
    use std::{cell::RefCell, rc::Rc, time::Duration};

    /// A filled square of `side` pixels at (x, y) of a drawing 200 by 200.
    fn square(x: i32, y: i32, side: i32) -> Shape {
        Shape {
            path: format!(
                "M{x} {y} L{} {y} L{} {} L{x} {} Z",
                x + side,
                x + side,
                y + side,
                y + side
            ),
            stroke: Colour::Transparent,
            line: 0,
            fill: Colour::Accent,
            opacity: None,
            round: false,
        }
    }

    let events = Rc::new(RefCell::new(Vec::<Event>::new()));
    let sink = events.clone();
    let (mut runner, mut answered) = TestingRunner::new(
        move || {
            let answered: State<bool> = use_consume();
            let sink = sink.clone();
            // The extension paints a different square only in its answer to the wheel event.
            let shapes = if answered() {
                vec![square(20, 20, 80)]
            } else {
                vec![square(60, 60, 40)]
            };
            BoxNode {
                canvas: None,
                id: Some("canvas".into()),
                style: Style {
                    width: Length::Px(200),
                    height: Length::Px(200),
                    clip: true,
                    // Even an internally constructed box cannot activate the canvas-only preview.
                    follow: Some(Follow {
                        wheel_doubling: 100,
                        ..Follow::default()
                    }),
                    ..Style::default()
                },
                children: vec![Node::Drawing {
                    width: 200,
                    height: 200,
                    layers: Vec::new(),
                    shapes,
                }],
                observe: vec![Observe::Wheel],
                palette: crate::theme::DARK,
                context: Context {
                    send: Send {
                        handler: Rc::new(EventHandler::new(move |event| {
                            sink.borrow_mut().push(event);
                        })),
                        plugin: peersus_core::plugins::PluginId::new("fixture").unwrap(),
                        page: "map".into(),
                        measurement_page: "map".into(),
                    },
                    avatars: Avatars::default(),
                    reduced: true,
                    pictures: crate::shelf::Held(Default::default()),
                },
            }
        },
        (200., 200.).into(),
        |runner| runner.provide_root_context(|| State::create(false)),
        1.,
    );
    runner.sync_and_update();
    let before = image::load_from_memory(runner.render().as_bytes())
        .unwrap()
        .to_rgba8();
    assert_ne!(
        before.get_pixel(80, 80),
        before.get_pixel(10, 10),
        "the square the extension drew is not there to begin with"
    );
    // A wheel event reaches the extension, but the old declarative drawing must not move.
    runner.scroll((100., 100.), (0., -100.));
    runner.poll_n(Duration::from_millis(2), 4);
    runner.sync_and_update();
    let pending = image::load_from_memory(runner.render().as_bytes())
        .unwrap()
        .to_rgba8();
    assert_eq!(before, pending);
    assert!(matches!(&events.borrow()[..], [Event::Wheel { .. }]));
    answered.set(true);
    runner.sync_and_update();
    let after = image::load_from_memory(runner.render().as_bytes())
        .unwrap()
        .to_rgba8();
    assert_ne!(
        before, after,
        "the extension's new drawing was not rendered"
    );
    assert_ne!(after.get_pixel(40, 40), before.get_pixel(40, 40));
}

#[cfg(all(test, feature = "headless"))]
#[test]
fn a_nested_item_in_a_clipped_box_receives_the_whole_press() {
    use freya_testing::TestingRunner;
    use std::cell::RefCell;

    let events = Rc::new(RefCell::new(Vec::new()));
    let sink = events.clone();
    let (mut window, ()) = TestingRunner::new(
        move || {
            let sink = sink.clone();
            let tree = Node::parse(&serde_json::json!({
                "k":"box","id":"map","style":{
                    "width":{"unit":"px","value":200},"height":{"unit":"px","value":200},
                    "clip":true
                },
                "children":[{"k":"box","style":{"at":[-50000,-50000],
                    "width":{"unit":"px","value":300},"height":{"unit":"px","value":300}},
                    "children":[
                        {"k":"box","id":"tag1","style":{"at":[125000,92000],
                            "width":{"unit":"px","value":70},"height":{"unit":"px","value":30}},
                            "children":[{"k":"text","text":"A device","tone":"body"}]},
                        {"k":"box","id":"pin1","style":{"at":[90000,90000],
                            "width":{"unit":"px","value":32},"height":{"unit":"px","value":32},"clip":true},
                            "children":[{"k":"avatar","device":"fixture-device","name":"A device","size":32}]}
                    ]
                }]
            })).unwrap();
            let context = Context {
                send: Send {
                    handler: Rc::new(EventHandler::new(move |event| {
                        sink.borrow_mut().push(event)
                    })),
                    plugin: peersus_core::plugins::PluginId::new("fixture").unwrap(),
                    page: "map".into(),
                    measurement_page: "map".into(),
                },
                avatars: Avatars::default(),
                reduced: true,
                pictures: crate::shelf::Held(Default::default()),
            };
            draw(&tree, crate::theme::DARK, &context)
        },
        (200., 200.).into(),
        |_| (),
        1.,
    );
    window.sync_and_update();
    for cursor in [(56., 56.), (88., 56.)] {
        window.click_cursor(cursor);
        window.handle_events_immediately();
        window.sync_and_update();
    }
    let seen = events.borrow();
    let pointer: Vec<_> = seen
        .iter()
        .filter_map(|event| match event {
            Event::Pointer {
                id,
                phase: phase @ (PointerPhase::Down | PointerPhase::Up),
                ..
            } => Some((id.as_str(), *phase)),
            _ => None,
        })
        .collect();
    assert_eq!(
        pointer,
        [
            ("pin1", PointerPhase::Down),
            ("pin1", PointerPhase::Up),
            ("tag1", PointerPhase::Down),
            ("tag1", PointerPhase::Up)
        ]
    );
}

#[cfg(all(test, feature = "headless", not(target_os = "android")))]
#[test]
fn pausing_a_page_skips_its_paint_and_preparation_and_resume_restores_it() {
    use crate::{
        layers::{Kept, KeptHandle},
        render_budget::{LIMIT, SUSTAIN_WINDOWS, WINDOW},
    };
    use freya_testing::TestingRunner;
    use peersus_core::model::LayerContent;
    use std::time::Duration;

    let palette = crate::theme::DARK;
    let plugin = PluginId::new("fixture").unwrap();
    let session = crate::link::Session::demo(&crate::demo::parse(&[]).unwrap()).unwrap();
    let link = session.link.as_ref().unwrap().clone();
    let drawing = Node::parse(&serde_json::json!({
        "k":"drawing", "width":160, "height":100,
        "layers":[{"id":"tile", "scale":1000000, "at":[0,0]}],
        "shapes":[{"path":"M90 10 L140 10 L140 60 L90 60 Z", "fill":"danger"}]
    }))
    .unwrap();
    let page = Node::Column {
        children: vec![
            drawing,
            Node::Button {
                id: "fixture-button".into(),
                label: "Fixture action".into(),
            },
        ],
    };
    let kept = |stamp| {
        Kept::of([(
            plugin.clone(),
            vec![LayerContent {
                id: "tile".into(),
                stamp,
                shapes: vec![Shape {
                    path: "M10 10 L60 10 L60 60 L10 60 Z".into(),
                    stroke: Colour::Transparent,
                    line: 0,
                    fill: Colour::Accent,
                    opacity: None,
                    round: false,
                }],
            }],
        )])
    };
    let initial = kept(1);
    let (mut runner, (window, mut shown, mut layers)) = TestingRunner::new(
        {
            let plugin = plugin.clone();
            move || {
                use_init_theme(move || crate::theme::theme(palette, true));
                let shown: State<bool> = use_consume();
                rect()
                    .expanded()
                    .overflow(Overflow::Clip)
                    .background(palette.surface)
                    .maybe_child(shown().then(|| Elements {
                        link: link.clone(),
                        plugin: plugin.clone(),
                        page: "main".into(),
                        measurement_page: "main".into(),
                        view: Some(page.clone()),
                        avatars: Avatars::default(),
                        pictures: crate::shelf::Held::default(),
                        palette,
                        embedded: false,
                    }))
            }
        },
        (380., 320.).into(),
        |runner| {
            let window = runner.provide_root_context(DrawingWindow::new);
            let shown = runner.provide_root_context(|| State::create(true));
            let layers = runner.provide_root_context(|| KeptHandle(State::create(initial)));
            (window, shown, layers.0)
        },
        1.,
    );
    let png = |runner: &mut TestingRunner| {
        image::load_from_memory(runner.render().as_bytes())
            .unwrap()
            .to_rgb8()
    };
    let rgb = |(r, g, b)| [r, g, b];
    runner.sync_and_update();
    let initial = png(&mut runner);
    assert_eq!(initial.get_pixel(20, 20).0, rgb(palette.receive));
    assert_eq!(initial.get_pixel(100, 20).0, rgb(palette.bad));

    shown.set(false);
    runner.sync_and_update();
    // A new instance after the owner removed its old page starts with a fresh account.
    window.accounts.borrow_mut().retain(|id| id != &plugin);
    shown.set(true);
    runner.sync_and_update();
    let focus = |runner: &TestingRunner| {
        runner.run_in(|| {
            *Platform::get()
                .current_window()
                .focused_accessibility_id
                .peek()
        })
    };
    let root_focus = focus(&runner);
    runner.press_key(Key::Named(NamedKey::Tab));
    runner.poll_n(Duration::ZERO, 4);
    let focused_control = focus(&runner);
    assert_ne!(
        focused_control, root_focus,
        "the extension's button should take focus"
    );

    // Put five costly seconds in the account, without relying on slow draws or sleeps.
    // The next real paint callback observes the completed window and initiates the pause.
    let start = Instant::now() - WINDOW * SUSTAIN_WINDOWS;
    for second in 0..SUSTAIN_WINDOWS {
        window.frame.set(u64::from(second));
        assert!(window.begin(&plugin, start + WINDOW * second));
        window
            .accounts
            .borrow_mut()
            .charge(&plugin, LIMIT + Duration::from_millis(1));
    }
    window.frame.set(u64::from(SUSTAIN_WINDOWS));
    let immediate = png(&mut runner);
    assert_eq!(immediate.get_pixel(20, 20).0, rgb(palette.surface));
    assert_eq!(immediate.get_pixel(100, 20).0, rgb(palette.surface));

    runner.poll_n(Duration::ZERO, 4);
    assert_eq!(
        focus(&runner),
        focused_control,
        "the notice replaced a focused control"
    );
    assert!(
        runner
            .find(|node, element| {
                let label = Label::try_downcast(element)?;
                let message =
                    "This extension draws too much and was paused to keep Peersus responsive.";
                (label.text == message && node.is_visible()).then_some(())
            })
            .is_some()
    );
    let replacement = kept(2);
    layers.set(replacement.clone());
    runner.sync_and_update();
    assert_eq!(
        replacement.budget().get(),
        0,
        "paused drawing parsed a new layer"
    );

    let paused = window.accounts.borrow().phase(&plugin);
    shown.set(false);
    runner.sync_and_update();
    assert_eq!(window.accounts.borrow().phase(&plugin), paused);
    shown.set(true);
    runner.sync_and_update();
    assert_eq!(
        replacement.budget().get(),
        0,
        "changing tabs lifted the pause"
    );

    let resume = runner
        .find(|node, element| {
            let label = Label::try_downcast(element)?;
            (label.text == "Resume" && node.is_visible()).then(|| node.layout().area.center())
        })
        .expect("the application should offer Resume");
    runner.click_cursor((f64::from(resume.x), f64::from(resume.y)));
    runner.poll_n(Duration::ZERO, 4);
    assert_eq!(
        window.accounts.borrow().phase(&plugin),
        Some(DrawPhase::Active)
    );
    let resumed = png(&mut runner);
    assert_eq!(resumed.get_pixel(20, 20).0, rgb(palette.receive));
    assert_eq!(resumed.get_pixel(100, 20).0, rgb(palette.bad));
}
