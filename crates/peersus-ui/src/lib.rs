//! The Peersus interface: one library shared by the desktop entry and the Android one. `view`
//! holds every rule about what a person is told, `link` the connection to the owner, `theme`
//! the palettes and `qr` the pairing picture; the components here only draw. The Android entry
//! (which needs `unsafe`) lives in its own platform crate, never here.
pub mod about;
pub mod accent_picker;
pub mod avatar;
pub mod avatar_file;
pub mod avatar_view;
pub mod clipboard_files;
pub mod font;
pub mod glyph;
pub mod icon;
pub mod interface_size;
pub mod i18n;
pub mod language_picker;
pub mod intake;
pub mod outgoing;
pub mod link;
pub mod motion;
pub mod preview;
pub mod qr_reader;
pub mod qr;
pub mod scanner;
pub mod top_sheet;
pub mod scatter;
pub mod shelf;
pub mod sidebar;
pub mod theme;
mod appearance;
mod system_theme;
pub mod view;
mod plugin_controls;
pub mod support;
pub mod settings_search;
mod settings_navigation;

use freya::prelude::*;
#[cfg(not(target_os = "android"))]
use freya::winit::window::ResizeDirection;
use freya::animation::*;
use link::{Link, Offline, Session};
use peersus_app::runtime::Reply;
use peersus_core::{
    Settings,
    config::{CustomTheme, ThemeBase, SettingsPatch, Theme as ThemeChoice},
    model::{Command, PeerId, PluginInfo, PluginStatus},
};
use std::{path::PathBuf, sync::Arc};
use theme::Palette;
use view::{Approval, MENU_SHORTCUTS, Sidebar, Phase, Presence, Progress, SendState, SettingsTab, View};

/// The launch configuration of the desktop entry: it finds the owner, starting it if none is there.
///
/// `Err` only when not even the program's executor could be started. Not on Android, which is told where its
/// owner is and uses [`config_at`].
#[cfg(not(target_os = "android"))]
pub fn config() -> Result<LaunchConfig, &'static str> {
    Ok(window_config(Session::open()?))
}

/// The same configuration for an owner already listening at `socket`, which this program does not look for
/// and never starts. Android opens this way: its owner is part of the application.
///
/// `platform` is what the platform offers beyond the window (see [`link::Platform`]): where its bars are, a picker for
/// files where the toolkit has no dialog, a camera. Each is present only on the platform that needs it.
pub fn config_at(socket: PathBuf, platform: link::Platform) -> Result<LaunchConfig, &'static str> {
    Ok(window_config(Session::open_at(socket, platform)?))
}

fn window_config(session: Arc<Session>) -> LaunchConfig {
    LaunchConfig::new().with_font(font::FAMILY, font::BYTES).with_default_font(font::FAMILY).with_window(
        WindowConfig::new(move || App { session: session.clone() })
            // The toolkit takes a fixed title; what tells two windows apart is the device's
            // name in the header, which can change while the window is open.
            .with_title("Peersus")
            .with_size(1100., 720.)
            .with_min_size(760., 560.)
            .with_decorations(false)
            // Transparent, so that the rounded corners of the page show what is behind them and not a square of black. The toolkit fills
            // a window with WHITE before drawing unless it is told otherwise, and that white is what showed in the four corners.
            .with_transparency(true)
            .with_background(Color::TRANSPARENT)
            .with_resizable(true),
    )
}

// ---- window chrome ---------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Dot {
    Minimise,
    Maximise,
    Close,
}

/// The diameter of every round control at the right of the header: the menu button and the three
/// window buttons are the same size, so they read as one row, and large enough to hit easily.
const HEADER_ROUND: f32 = 32.;

/// How many frames one run of the sparks of the fuse has: a second at sixty frames in the second. The sparks at each frame are a function of its
/// number (see `view::fuse_particles`), and the run goes round again when it ends.
const FUSE_FRAMES: u32 = 60;

/// The sparks of the tip of a fuse: a run of frames that goes round again for as long as `running`, and the frame it is at. Past the longest life
/// of a particle, so that every frame of the run has its sparks in flight and the picture of the run never empties. The picture of each frame is a
/// function of its number (see `view::fuse_particles`), so the flight of the sparks is as long as the run. With reduced motion, or while nothing
/// burns (`running` false), it does not move: no run, no redraw, and the frame stands still.
///
/// One function for every circle that has a fuse, so that the sparks are the same wherever a code burns. It makes its hook on EVERY draw, in the
/// same order, whether or not it runs: a hook made only sometimes would change the order between two draws of the same component.
fn use_sparks(reduced: bool, running: bool) -> u32 {
    let run = use_animation_with_dependencies(&(reduced, running), move |conf, (reduced, running)| {
        conf.on_creation(OnCreation::Run);
        conf.on_change(OnChange::Rerun);
        conf.on_finish(OnFinish::restart());
        AnimNum::new(0., 1.).time(if *reduced || !*running { 0 } else { 900 }).function(Function::Linear)
    });
    let share: f32 = run.get().value();
    view::FUSE_PARTICLE_FRAMES + if reduced || !running { 0 } else { (share * FUSE_FRAMES as f32) as u32 }
}

/// The share of the life of a code that is left, from 1 to 0, as a smooth line: from where it is when `code` is first seen (`from`, the share left at
/// that moment) to nothing over the `seconds` the code still has, begun again for each code (`code` is what a new one changes). A fuse that is read
/// from the clock once a second moves its tip a step each second, and the sparks that fly from the tip then shake with it: this one moves with every
/// frame, as the fuse of the Pair page always did, so that the two are the same wherever a code burns. With reduced motion it does not move: it is
/// shown whole, and `running` false (no code on show) holds it still.
///
/// Made on EVERY draw, in the same order, whether or not it runs.
fn use_fuse(code: Option<u64>, from: f32, seconds: u64, reduced: bool) -> f32 {
    let run = use_animation_with_dependencies(&(code, reduced), move |conf, (code, reduced)| {
        conf.on_creation(OnCreation::Run);
        conf.on_change(OnChange::Rerun);
        AnimNum::new(from, 0.).time(if *reduced || code.is_none() { 0 } else { seconds.saturating_mul(1_000) }).function(Function::Linear)
    });
    if reduced { 1. } else { run.get().value() }
}

/// One of the application's own window buttons: a disc in the colour of what it does, with the mark of that action on it.
/// The mark is the one colour that can be read on the disc, so it stays readable whatever colour the theme gives the disc.
#[derive(PartialEq)]
struct WindowDot(Dot, Palette);
impl Component for WindowDot {
    fn render(&self) -> impl IntoElement {
        let action = self.0;
        let (colour, mark) = match action {
            Dot::Minimise => (self.1.minimise, "minimise"),
            Dot::Maximise => (self.1.maximise, "maximise"),
            Dot::Close => (self.1.close, "close"),
        };
        let ink = glyph::ink_on(colour);
        let mut hovered = use_state(|| false);
        rect()
            .width(Size::px(HEADER_ROUND))
            .height(Size::px(HEADER_ROUND))
            .corner_radius(HEADER_ROUND / 2.)
            .center()
            // A hovered dot is dimmed slightly so it visibly reacts.
            .background(if hovered() {
                (colour.0 / 5 * 4, colour.1 / 5 * 4, colour.2 / 5 * 4)
            } else {
                colour
            })
            .child(glyph::draw(glyph::by_name(mark), glyph::WINDOW, ink))
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set(false))
            // The press must not start a window drag from under the header.
            .on_pointer_down(|e: Event<PointerEventData>| e.stop_propagation())
            .on_press(move |e: Event<PressEventData>| {
                e.stop_propagation();
                let platform = Platform::get();
                platform.clone().with_window(Platform::window_id(), move |window| match action {
                    Dot::Minimise => window.set_minimized(true),
                    Dot::Maximise => window.set_maximized(!window.is_maximized()),
                    Dot::Close => {
                        platform.close_window(window.id());
                    }
                });
            })
    }
}

/// Edge and corner zones that start a native resize, since there is no OS frame to grab.
#[cfg(not(target_os = "android"))]
fn resize_zones(width: f32, height: f32) -> impl IntoElement {
    const EDGE: f32 = 6.;
    // A generous diagonal grip, separate from the thinner straight edge handles.
    const CORNER: f32 = 16.;
    let zone = |direction: ResizeDirection, cursor: CursorIcon, position: Position, w: f32, h: f32| -> Element {
        rect()
            .position(position)
            .width(Size::px(w.max(0.)))
            .height(Size::px(h.max(0.)))
            .cursor(cursor)
            // Freya lets events pass through exactly transparent backgrounds to non-ancestor
            // listeners too. A minimal solid hit surface prevents the header starting a window
            // move on the same press that starts a resize.
            .background(Color::from_rgb(0, 0, 0).with_a(1))
            .on_pointer_down(move |e: Event<PointerEventData>| {
                if !matches!(&*e, PointerEventData::Mouse(mouse) if mouse.button == Some(MouseButton::Left)) {
                    return;
                }
                e.stop_propagation();
                Platform::get().with_window(Platform::window_id(), move |window| {
                    if window.is_resizable() && !window.is_maximized() && window.fullscreen().is_none() {
                        let _ = window.drag_resize_window(direction);
                    }
                });
            })
            .into()
    };
    let at = || Position::new_absolute();
    // Absolute positioning changes the origin, not Fill's available-space calculation: after the
    // stacked body fills the root there is no height left. Use the measured window size for both
    // the overlay and its strips, and reserve the corners rather than overlapping their hit areas.
    rect()
        .position(at().top(0.).left(0.))
        .width(Size::px(width))
        .height(Size::px(height))
        // Window grips must also receive presses outside the rounded content's clipping path.
        .layer(Layer::Overlay)
        .child(zone(ResizeDirection::North, CursorIcon::NResize, at().top(0.).left(CORNER), width - CORNER * 2., EDGE))
        .child(zone(ResizeDirection::South, CursorIcon::SResize, at().bottom(0.).left(CORNER), width - CORNER * 2., EDGE))
        .child(zone(ResizeDirection::West, CursorIcon::WResize, at().top(CORNER).left(0.), EDGE, height - CORNER * 2.))
        .child(zone(ResizeDirection::East, CursorIcon::EResize, at().top(CORNER).right(0.), EDGE, height - CORNER * 2.))
        .child(zone(ResizeDirection::NorthWest, CursorIcon::NwResize, at().top(0.).left(0.), CORNER, CORNER))
        .child(zone(ResizeDirection::NorthEast, CursorIcon::NeResize, at().top(0.).right(0.), CORNER, CORNER))
        .child(zone(ResizeDirection::SouthWest, CursorIcon::SwResize, at().bottom(0.).left(0.), CORNER, CORNER))
        .child(zone(ResizeDirection::SouthEast, CursorIcon::SeResize, at().bottom(0.).right(0.), CORNER, CORNER))
}

// ---- navigation ------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Send,
    Receive,
    Peers,
    Transfers,
    Settings,
}

impl Page {
    /// The four screens of the main navigation, in the header.
    const MAIN: [Page; 4] = [Page::Send, Page::Receive, Page::Peers, Page::Transfers];

    fn name(self) -> &'static str {
        match self {
            Page::Send => "SEND",
            Page::Receive => "RECEIVE",
            Page::Peers => "PEERS",
            Page::Transfers => "TRANSFERS",
            Page::Settings => "Settings",
        }
    }

    fn number(self) -> &'static str {
        match self {
            Page::Send => "01",
            Page::Receive => "02",
            Page::Peers => "03",
            Page::Transfers => "04",
            Page::Settings => "",
        }
    }

    /// The accent of the screen: what goes out is warm, what comes in is cool.
    fn accent(self, palette: &Palette) -> (u8, u8, u8) {
        match self {
            Page::Receive => palette.receive,
            _ => palette.send,
        }
    }
}

/// A small caption in the monospace the prototype uses for every label.
fn mono(text: impl Into<std::borrow::Cow<'static, str>>, colour: (u8, u8, u8), size: f32) -> Label {
    label()
        .text(text)
        .color(colour)
        .font_size(size)
        .font_family("monospace")
}

#[derive(PartialEq)]
struct Tab {
    page: Page,
    active: bool,
    palette: Palette,
    on_select: EventHandler<Page>,
}
impl Component for Tab {
    fn render(&self) -> impl IntoElement {
        let (page, palette) = (self.page, self.palette);
        let on_select = self.on_select.clone();
        let mut hovered = use_state(|| false);
        let accent = page.accent(&palette);
        rect()
            .padding((0., 22.))
            .height(Size::fill())
            .horizontal()
            .cross_align(Alignment::center())
            .spacing(8.)
            .border(if self.active {
                Border::new()
                    .fill(accent)
                    .width(BorderWidth { top: 0., right: 0., bottom: 3., left: 0. })
                    .alignment(BorderAlignment::Inner)
            } else {
                // No border at all: a default one would box in every inactive tab.
                Border::new().width(BorderWidth { top: 0., right: 0., bottom: 0., left: 0. })
            })
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set(false))
            .on_pointer_down(|e: Event<PointerEventData>| e.stop_propagation())
            .on_press(move |_| on_select.call(page))
            .child(mono(page.number(), palette.muted, 11.))
            .child(mono(
                i18n::tr(page.name()),
                if self.active || hovered() { palette.text } else { palette.muted },
                14.,
            ))
    }
}

/// The place a bottom-bar entry opens.
fn page_of(place: view::BottomPlace) -> Page {
    match place {
        view::BottomPlace::Send => Page::Send,
        view::BottomPlace::Peers => Page::Peers,
        view::BottomPlace::Receive => Page::Receive,
        view::BottomPlace::Transfers => Page::Transfers,
    }
}

/// The name of the picture of a place of the main navigation. The same pictures as everywhere else: see `glyph`.
fn place_name(page: Page) -> &'static str {
    match page {
        Page::Send => "send",
        Page::Receive => "receive",
        Page::Peers => "peers",
        Page::Transfers => "transfers",
        Page::Settings => "settings",
    }
}

/// One place in the bottom bar: its picture over its name, an equal share of the width, and tall enough
/// for a thumb. Marked like a tab is, in the place's own colour.
#[derive(PartialEq)]
struct BottomTab {
    page: Page,
    active: bool,
    palette: Palette,
    on_select: EventHandler<Page>,
}
impl Component for BottomTab {
    fn render(&self) -> impl IntoElement {
        let (page, palette) = (self.page, self.palette);
        let on_select = self.on_select.clone();
        let accent = page.accent(&palette);
        let colour = if self.active { accent } else { palette.muted };
        rect()
            .width(Size::flex(1.))
            .height(Size::fill())
            .center()
            .spacing(4.)
            .border(if self.active {
                Border::new()
                    .fill(accent)
                    .width(BorderWidth { top: 3., right: 0., bottom: 0., left: 0. })
                    .alignment(BorderAlignment::Inner)
            } else {
                Border::new().width(BorderWidth { top: 0., right: 0., bottom: 0., left: 0. })
            })
            .on_press(move |_| on_select.call(page))
            .child(glyph::picture_on(
                place_name(page),
                glyph::BAR,
                accent,
                if self.active { palette.text } else { glyph::resting_ink(&palette) },
                palette.background,
            ))
            .child(mono(i18n::tr(page.name()), colour, 11.))
    }
}

/// The bottom bar of the portrait form, where a thumb reaches. Send is not in it: on a phone Send is the
/// circle itself.
#[derive(PartialEq)]
struct BottomBar {
    active: Page,
    places: &'static [view::BottomPlace],
    palette: Palette,
    on_select: EventHandler<Page>,
}
impl Component for BottomBar {
    fn render(&self) -> impl IntoElement {
        let mut bar = rect()
            .width(Size::fill())
            .height(Size::px(64.))
            .horizontal()
            // `flex` is what makes each place's `Size::flex(1.)` an equal share of the width.
            .content(Content::flex())
            .background(self.palette.background)
            .border(
                Border::new()
                    .fill(self.palette.border)
                    .width(BorderWidth { top: 1., right: 0., bottom: 0., left: 0. })
                    .alignment(BorderAlignment::Inner),
            );
        for place in self.places {
            let page = page_of(*place);
            bar = bar.child(BottomTab {
                page,
                active: page == self.active,
                palette: self.palette,
                on_select: self.on_select.clone(),
            });
        }
        bar
    }
}

/// The menu button at the right of the header. A plain click goes to Settings; the right button
/// opens the menu of the other screens, which is drawn by [`menu_layer`] over the page, because a
/// panel that is a child of this small button would be cut to its size.
#[derive(PartialEq)]
struct MenuButton {
    palette: Palette,
    on_select: EventHandler<Page>,
    open: State<bool>,
    /// Whether a press opens the sheet of places that comes down from the top (a phone), and not Settings itself.
    sheet: bool,
}
impl Component for MenuButton {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let mut open = self.open;
        let mut hovered = use_state(|| false);
        let mut touch = use_state(|| view::Touch::new(HEADER_ROUND));
        // The same kind of control as the three window buttons beside it: a disc, as large as theirs, filled with the accent the person
        // chose and with a mark drawn large enough to be read, so that Settings is no smaller and no paler than the buttons it stands with.
        let fill = if hovered() { glyph::dim(palette.send, palette.background, 0.8) } else { palette.send };
        let ink = glyph::ink_on(fill);
        rect()
            .width(Size::px(HEADER_ROUND))
            .height(Size::px(HEADER_ROUND))
            .corner_radius(HEADER_ROUND / 2.)
            .center()
            .background(fill)
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set(false))
            .on_pointer_down(move |e: Event<PointerEventData>| {
                // The header drags the window from any pointer-down: this button is not a handle.
                e.stop_propagation();
                // A touch that BEGINS here is the only one that can press this button: where it began, and when, is said now, so that a finger that
                // began elsewhere and is lifted over the button (the bar moves under a finger as the page scrolls) does not open Settings.
                let (g, l) = (e.global_location(), e.element_location());
                let mut next = view::Touch::new(HEADER_ROUND);
                next.down((l.x as f32, l.y as f32), (g.x as f32, g.y as f32), monotonic_ms());
                touch.set(next);
                // The right button opens the menu. A single handler does both, because a second
                // one on the same event could replace this one.
                if matches!(&*e, PointerEventData::Mouse(m) if m.button == Some(MouseButton::Right)) {
                    open.set(!open());
                }
            })
            .on_capture_global_pointer_move(move |e: Event<PointerEventData>| {
                let g = e.global_location();
                let mut next = *touch.peek();
                next.moved((g.x as f32, g.y as f32));
                touch.set_if_modified(next);
            })
            .on_touch_cancel(move |_| {
                let mut next = *touch.peek();
                next.cancel();
                touch.set(next);
            })
            .on_pointer_press({
                let on_select = self.on_select.clone();
                let sheet = self.sheet;
                move |e: Event<PointerEventData>| {
                    // Only a touch that began on the button, stayed on it and was pressed to its actuation point is a press; the toolkit says a
                    // press for whatever is under the finger when it is lifted. A mouse click, which has no finger to be carried away, is judged
                    // the same way and passes.
                    let (g, l) = (e.global_location(), e.element_location());
                    let mut next = *touch.peek();
                    let pressed = next.up((l.x as f32, l.y as f32), (g.x as f32, g.y as f32), monotonic_ms());
                    touch.set(next);
                    if !pressed {
                        return;
                    }
                    // On a phone there is no right button, so the press opens the places of Settings, which come down from the top; a
                    // second press closes them. Elsewhere it goes straight to Settings, as it always did.
                    if sheet {
                        open.set(!open());
                    } else {
                        open.set(false);
                        on_select.call(Page::Settings);
                    }
                }
            })
            .child(glyph::draw_duo("settings", glyph::HEADER_MARK, ink, ink, glyph::dim(glyph::soft_of(fill), fill, 0.9), glyph::soft_of(fill)))
    }
}

/// What the right click on the menu button opens: shortcuts into the tabs of Settings, as a panel
/// under the header at the right. Returned in two parts, the panel and the layer under it that
/// takes a click anywhere else to close it, because only the panel moves when it opens.
fn menu_layer(palette: Palette, on_open: EventHandler<SettingsTab>, mut open: State<bool>) -> (Element, Element) {
    let mut panel = rect()
        .position(Position::new_absolute().top(66.).right(28.))
        .width(Size::px(200.))
        .padding(6.)
        .spacing(2.)
        .corner_radius(12.)
        .background(palette.raised)
        .border(Border::new().fill(palette.border).width(1.).alignment(BorderAlignment::Inner))
        // The backdrop closes the menu on a pointer-down: this one must not reach it, or the menu
        // would be gone before the click on an item is finished (a press is a down AND an up).
        .on_pointer_down(|e: Event<PointerEventData>| e.stop_propagation());
    for tab in MENU_SHORTCUTS {
        let on_open = on_open.clone();
        panel = panel.child(MenuItem {
            tab,
            palette,
            on_choose: EventHandler::new(move |tab: SettingsTab| {
                open.set(false);
                on_open.call(tab);
            }),
        });
    }
    let backdrop: Element = rect()
        .width(Size::fill())
        .height(Size::fill())
        .on_pointer_down(move |_| open.set(false))
        .into();
    (panel.into(), backdrop)
}

#[derive(PartialEq)]
struct MenuItem {
    tab: SettingsTab,
    palette: Palette,
    on_choose: EventHandler<SettingsTab>,
}
impl Component for MenuItem {
    fn render(&self) -> impl IntoElement {
        let (tab, palette) = (self.tab, self.palette);
        let on_choose = self.on_choose.clone();
        let mut hovered = use_state(|| false);
        rect()
            .width(Size::fill())
            .padding((9., 12.))
            .horizontal()
            .cross_align(Alignment::center())
            .spacing(10.)
            .corner_radius(8.)
            .background(if hovered() { palette.surface } else { palette.raised })
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set(false))
            .on_press(move |e: Event<PressEventData>| {
                // A press goes up to its parent: the menu is drawn over the page, but without this
                // the press would also reach the layer under it, which closes the menu first.
                e.stop_propagation();
                on_choose.call(tab);
            })
            .child(glyph::picture(glyph::name_of_tab(tab), glyph::LIST, &palette, palette.muted))
            .child(label().text(i18n::tr(tab.title())).color(palette.text).font_size(14.))
    }
}

/// The application's own title bar: mark, name, the two main screens, state, menu, window
/// buttons. The whole bar drags the window; the tabs, menu and dots do not.
/// What of the header is shown beyond the mark and the name. Two gates that always go together: the places to
/// open, and the menu that opens the rest. Both are off until the device has a name, since there is nothing to open.
#[derive(Clone, Copy)]
struct Shown {
    /// The three places across the top (a desktop; a phone has them along the bottom).
    tabs: bool,
    /// The menu button.
    menu: bool,
    /// How much of the bar the page has scrolled out of sight, in pixels; nothing on a desktop.
    hidden: f32,
}

fn header(
    palette: Palette,
    active: Page,
    connected: bool,
    shown: Shown,
    form: view::Form,
    on_select: EventHandler<Page>,
    menu_open: State<bool>,
) -> impl IntoElement {
    let hidden = shown.hidden;
    let mut bar = rect()
        .width(Size::fill())
        .height(Size::px(view::HEADER_HEIGHT))
        .horizontal()
        .cross_align(Alignment::center())
        .content(Content::flex())
        .padding((0., form.header_side_padding()))
        .spacing(10.)
        .background(palette.header)
        .border(Border::new().fill(palette.border).width(BorderWidth { top: 0., right: 0., bottom: 1., left: 0. }).alignment(BorderAlignment::Inner))
        .window_drag()
        .child(
            // The mark stands for the first letter of the name: the word is `eersus` and the picture is its `p`, with no gap between them.
            rect()
                .child(
                    rect()
                        .horizontal()
                        .cross_align(Alignment::center())
                        // The picture's body ends 7 physical px below the word's baseline on the phone (2.53 px per logical px); lifting it by 2
                        // (3 overshot by 3 px, measured) puts the base of its round body on that baseline.
                        .child(rect().margin((0., 0., 4., 0.)).child(Mark { palette }))
                        // The picture has a see-through border (22% of its side, 9.7 px at this size): the word is drawn back over it so that
                        // it starts where the picture's shapes end, with the air that is between two letters.
                        .child(rect().margin((0., 0., 0., -8.)).child(label().text("eersus").color(palette.text).font_size(26.).font_weight(FontWeight::BOLD))),
                )
                .child(mono(crate::i18n::tr("THE TRANSFER PORTAL"), palette.muted, 10.)),
        )
        .child(rect().width(Size::flex(1.)));
    if shown.tabs {
        for page in Page::MAIN {
            bar = bar.child(Tab { page, active: page == active, palette, on_select: on_select.clone() });
        }
    }
    let mut status = rect()
        .horizontal()
        .cross_align(Alignment::center())
        .spacing(8.)
        .child(
            rect()
                .width(Size::px(8.))
                .height(Size::px(8.))
                .corner_radius(4.)
                .background(if connected { palette.good } else { palette.bad }),
        );
    if form.header_says_connected_in_words() {
        status = status.child(mono(if connected { "CONNECTED" } else { "NOT RUNNING" }, palette.muted, 11.));
    }
    let mut bar = bar.child(rect().width(Size::flex(1.))).child(status);
    // The menu is offered only when it opens something: until the device has a name there is nothing to open.
    if shown.menu {
        bar = bar.child(MenuButton { palette, on_select, open: menu_open, sheet: form.settings_places_come_from_the_top() });
    }
    if form.header_has_window_buttons() {
        bar = bar.child(
            rect()
                .horizontal()
                .spacing(8.)
                .child(WindowDot(Dot::Minimise, palette))
                .child(WindowDot(Dot::Maximise, palette))
                .child(WindowDot(Dot::Close, palette)),
        );
    }
    // The bar keeps its own height and is moved up out of a box that is as tall as what is still shown and cuts what is outside it:
    // the page under it takes the room the bar gave up, and the bar itself is never squeezed. Nothing is hidden on a desktop.
    rect()
        .width(Size::fill())
        .height(Size::px(view::HEADER_HEIGHT - hidden))
        .overflow(Overflow::Clip)
        .child(bar.margin((-hidden, 0., 0., 0.)))
}

/// The mark: three nodes joined by lines, as in the application icon.
#[derive(PartialEq)]
struct Mark {
    palette: Palette,
}
impl Component for Mark {
    fn render(&self) -> impl IntoElement {
        match icon::element(44., self.palette.header) {
            Some(picture) => picture,
            None => rect().width(Size::px(44.)).height(Size::px(44.)).corner_radius(12.).background(self.palette.send).into(),
        }
    }
}

// ---- root ------------------------------------------------------------------------------

/// The root of the window: frame, header, and either the screens or why there is no owner.
#[derive(Clone)]
struct App {
    session: Arc<Session>,
}

impl PartialEq for App {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.session, &other.session)
    }
}

impl Component for App {
    fn render(&self) -> impl IntoElement {
        // The choice lives in the owner's settings, and the window opens knowing them: the owner
        // already has a snapshot when this window attaches. Starting from anything else shows
        // the wrong theme until the next change, which on a quiet owner is never.
        let saved = match &self.session.link {
            Ok(link) => link.client().snapshot().settings.theme,
            Err(_) => ThemeChoice::System,
        };
        let choice = use_state(move || saved);
        let initial_language = self.session.link.as_ref()
            .map(|link| link.client().snapshot().settings.language).unwrap_or_default();
        let language = use_state(move || initial_language);
        provide_context(i18n::Current(language));
        // The size of the interface, kept in the owner's settings. The toolkit multiplies every logical size by it, so
        // text, spacing and icons grow together and nothing here is multiplied by hand.
        let initial_scale = self.session.link.as_ref()
            .map(|link| link.client().snapshot().settings.scale).unwrap_or(1.0);
        let scale = use_state(move || initial_scale);
        // The saved size is applied to the window at once when it opens, and after that once it has STOPPED changing for a moment, and
        // only if the window is not already at it. The slider reads the pointer against where the slider is on the screen, and applying
        // every size as it is dragged laid the page out again under the pointer: a pointer at 120 % then meant 90 % where the slider had
        // moved to, so the interface jumped between sizes. Waiting for the drag to rest means the slider does not move while it is held.
        // The first one is not waited for: the window would show 100 % for that moment before it took the size the person had saved.
        // Ctrl and plus or minus change the window's own size without going through the saved one, so the check against the window
        // keeps the saved size from pulling that back.
        let mut applied_once = use_state(|| false);
        use_side_effect_with_deps(&scale(), move |value| {
            let wanted = *value;
            let apply = move || {
                let window = Platform::get().current_window();
                if (*window.scale_factor.peek() - f64::from(wanted)).abs() > f64::EPSILON {
                    window.set_custom_scale_factor(f64::from(wanted));
                }
            };
            if !*applied_once.peek() {
                applied_once.set(true);
                apply();
                return;
            }
            let mut told = link::after(std::time::Duration::from_millis(350), std::time::Duration::from_millis(50));
            spawn(async move {
                if (&mut told).await.is_ok() && (*scale.peek() - wanted).abs() < f32::EPSILON {
                    apply();
                }
            });
        });
        // The accent the person chose, kept in the owner's settings like the size: what goes out is drawn in it, and so are the
        // main shapes of the pictures. It is read here, from the snapshot the window opened on, and kept up to date by `Connected`.
        let initial_accent = self.session.link.as_ref()
            .map(|link| link.client().snapshot().settings.accent).unwrap_or(peersus_core::Settings::default().accent);
        let accent = use_state(move || initial_accent);
        let initial_custom = self.session.link.as_ref().map(|link| link.client().snapshot().settings.custom_theme.clone()).unwrap_or_default();
        let custom = use_state(move || initial_custom);
        let preview = use_state(|| None::<CustomTheme>);
        provide_context(appearance::Preview(preview));
        let mut system_dark = use_state(|| *self.session.system_theme.borrow());
        let mut system_updates = self.session.system_theme.clone();
        use_hook(move || spawn(async move {
            loop {
                system_dark.set_if_modified(*system_updates.borrow_and_update());
                if system_updates.changed().await.is_err() { break; }
            }
        }));
        let platform_dark = system_dark().unwrap_or(*Platform::get().current_window().preferred_theme.read() == PreferredTheme::Dark);
        let custom_palette = preview.read().clone().or_else(|| (choice() == ThemeChoice::Custom).then(|| custom.read().clone()));
        let (palette, dark) = if let Some(custom) = custom_palette {
            (Palette::custom(&custom), custom.base == ThemeBase::Dark)
        } else {
            (Palette::of_choice(choice(), platform_dark, accent()), choice() == ThemeChoice::Dark || (choice() == ThemeChoice::System && platform_dark))
        };
        // The toolkit's own components read this theme when they are drawn. It is set once by
        // the hook, so it is brought up to date here whenever the choice changes it.
        let mut toolkit = use_init_theme(move || theme::theme(palette, dark));
        toolkit.set_if_modified(theme::theme(palette, dark));
        // The surfaces the platform draws itself (the panel and the notification of an offer) are drawn with these colours, so they follow the
        // theme the person chose and the one they made. A preview that was not saved is not given: it is a try, and not what they chose.
        let mut given = use_state(|| [0_u32; 9]);
        if let Some(appearance) = self.session.platform.appearance.clone()
            && preview.read().is_none()
        {
            let word = |(r, g, b): (u8, u8, u8)| (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
            let colours = [
                word(palette.surface),
                word(palette.raised),
                word(palette.border),
                word(palette.text),
                word(palette.muted),
                word(palette.receive),
                word(palette.bad),
                word(palette.button),
                word(palette.button_text),
            ];
            if *given.peek() != colours {
                given.set(colours);
                (appearance.set)(&colours);
            }
        }
        let mut size = use_state(|| (0_f32, 0_f32));
        let form = view::Form::of(size().0, size().1);
        // The margins the system keeps free (status bar, navigation bar, notch), from the platform and
        // divided by the window's own scale factor: nothing here is a size written in this program. A platform
        // has no answer until its window has been laid out, and it changes when the bars or keyboard come and
        // go, so a thread of its own watches it and this only waits on what it publishes. On a desktop there
        // is no source and no margins.
        let mut safe = use_state(view::SafeArea::default);
        // The second now, for every countdown: one thread for the whole window, read here and handed down. The interface has no
        // timer, so the thread keeps the time and this only waits on what it publishes, as it does for the usable area.
        let mut now = use_state(unix_now);
        use_hook(move || {
            let mut ticks = link::clock(unix_now, std::time::Duration::from_millis(250));
            spawn(async move {
                loop {
                    now.set_if_modified(*ticks.borrow_and_update());
                    if ticks.changed().await.is_err() {
                        break;
                    }
                }
            });
        });
        // Where the platform puts a received file in a folder the person chose (Android): provided once to the window, so that
        // the Accept button, wherever it is drawn, reaches it without every page carrying it. Absent on a desktop.
        if let Some(sink) = self.session.platform.folder_sink.clone() {
            provide_context(sink);
        }
        // The permission to draw the panel of an offer over other applications (Android): provided once, for the row of Settings that shows it.
        if let Some(overlay) = self.session.platform.overlay.clone() {
            provide_context(overlay);
        }
        if let Some(opener) = self.session.platform.open_url.clone() {
            provide_context(opener);
        }
        if let Some(picker) = self.session.platform.plugin_picker.clone() {
            provide_context(picker);
        }
        if let Some(clipboard) = self.session.platform.clipboard.clone() {
            provide_context(clipboard);
        }
        if let Some(source) = self.session.platform.safe_area.clone() {
            use_hook(move || {
                let mut area = link::watch_safe_area(source, std::time::Duration::from_millis(250));
                spawn(async move {
                    loop {
                        let rectangle = *area.borrow_and_update();
                        let window = Platform::get().current_window();
                        let scale = *window.scale_factor.peek();
                        let logical = *window.root_size.peek();
                        // Physical pixels, the unit the platform answers in: logical size times the real scale.
                        let screen = (
                            (f64::from(logical.width) * scale).round().max(0.) as u32,
                            (f64::from(logical.height) * scale).round().max(0.) as u32,
                        );
                        safe.set_if_modified(view::SafeArea::from_physical(rectangle, screen, scale));
                        if area.changed().await.is_err() {
                            break;
                        }
                    }
                });
            });
        }
        // `PEERSUS_PAGE` only lets a screenshot start on a given screen; people use the header.
        let (first, first_tab) = match std::env::var("PEERSUS_PAGE").as_deref() {
            Ok("receive") => (Page::Receive, SettingsTab::default()),
            Ok("peers" | "devices") => (Page::Peers, SettingsTab::default()),
            Ok("transfers") => (Page::Transfers, SettingsTab::default()),
            Ok("settings") => (Page::Settings, SettingsTab::default()),
            // The old pages are tabs of Settings now: these names open Settings on that tab.
            Ok("plugins") => (Page::Settings, SettingsTab::Extensions),
            Ok("about") => (Page::Settings, SettingsTab::About),
            // The history is the Transfers page now: the one place that lists what was sent and received.
            Ok("history") => (Page::Transfers, SettingsTab::default()),
            Ok("device") => (Page::Settings, SettingsTab::Device),
            Ok("appearance") => (Page::Settings, SettingsTab::Appearance),
            _ => (Page::Send, SettingsTab::default()),
        };
        let mut page = use_state(move || first);
        // The tab of Settings that is shown lives here, not in the page: the menu has to be able
        // to open Settings on a given tab even when Settings is already on the screen, and a
        // value the page keeps for itself is only ever read the first time it appears.
        let mut settings_tab = use_state(move || first_tab);
        // Like `PEERSUS_PAGE`, only so that a screenshot can show the menu open: a click cannot be
        // sent to a window under test and still get a frame back.
        let mut menu_open = use_state(|| std::env::var("PEERSUS_MENU").is_ok_and(|v| v == "1"));
        // Whether the device has a name yet. `Connected` knows (it holds the owner's settings) and writes it
        // here; the header, the sidebar, the bottom bar and the menu, which live above it, read it. Until it is
        // so none of them is shown: the screen that asks for a name has nothing else on it. It starts from the
        // snapshot the window opened on, so that a device that already has a name never flashes without them.
        let saved_named = match &self.session.link {
            Ok(link) => link.client().snapshot().settings.device_name.trim() != Settings::default().device_name,
            Err(_) => false,
        };
        let named = use_state(move || saved_named);
        // How much of the header the page under it has scrolled out of sight. `frame`, which every page draws its scrolling area
        // with, writes it from the page's own scroll position and the header here reads it, so it is provided to them as context
        // and not carried through every page. A new page starts at its top, so a change of page brings the bar back.
        let mut bar_hide = use_state(view::HeaderHide::default);
        provide_context(bar_hide);
        // Whether a finger is held on a circle (see `Scrolling`): written by the circle, read by the page under it.
        provide_context(use_state(|| HoldingCircle(false)));
        let on_select = EventHandler::new(move |chosen: Page| {
            menu_open.set(false);
            if let Some(opened) = view::Sidebar::page_opened(named(), chosen) {
                page.set(opened);
                bar_hide.set(view::HeaderHide::default());
            }
        });
        let on_open_tab = EventHandler::new(move |chosen: SettingsTab| {
            menu_open.set(false);
            // Until the device has a name the question is all there is: the menu opens nothing.
            if let Some(opened) = view::Sidebar::page_opened(named(), Page::Settings) {
                settings_tab.set(chosen);
                page.set(opened);
                bar_hide.set(view::HeaderHide::default());
            }
        });
        // Files another application shared are waiting to be told who gets them, and the Send page is what asks that. Whatever page the
        // window was left on (a share only brings the window forward, it does not choose a page), Send is brought forward for them, and
        // not before the device has a name, when the question for it is all there is. Only looked at here: Send takes the notice, once.
        if let Some(picker) = &self.session.platform.file_picker
            && picker.picked.has_external()
            && named()
            && page() != Page::Send
        {
            page.set(Page::Send);
            bar_hide.set(view::HeaderHide::default());
        }
        let current = page();
        // What the owner's settings say about where the places are and about motion. Read from
        // here, `Connected` keeps them up to date from every snapshot, so a change made in
        // Settings shows at once and not at the next unrelated redraw.
        let saved_reduced = match &self.session.link {
            Ok(link) => link.client().snapshot().settings.reduced_motion,
            Err(_) => false,
        };
        let reduced_motion = use_state(move || saved_reduced);
        // The one saved option for the sidebar, and the name the profile shows. Connected
        // keeps both up to date from every snapshot, so a change in Settings shows at once.
        let saved_icons = match &self.session.link {
            Ok(link) => link.client().snapshot().settings.collapse_to_icons,
            Err(_) => false,
        };
        let keep_icons = use_state(move || saved_icons);
        // Whether a touch of two fingers opens Settings (Android). On unless the person turned it off; Connected keeps it up to date.
        let saved_two_fingers = match &self.session.link {
            Ok(link) => link.client().snapshot().settings.two_finger_settings,
            Err(_) => true,
        };
        let two_fingers = use_state(move || saved_two_fingers);
        let saved_name = match &self.session.link {
            Ok(link) => link.client().snapshot().settings.device_name.clone(),
            Err(_) => String::new(),
        };
        let device_name = use_state(move || saved_name);
        // The pictures the window holds, by digest. Connected fetches what each snapshot names and fills it.
        let pictures = use_state(shelf::Held::default);
        let saved_picture = match &self.session.link {
            Ok(link) => link.client().snapshot().profile_picture,
            Err(_) => None,
        };
        let own_picture = use_state(move || saved_picture);
        // From the snapshot the window opened on, as the other shared states: a file that was offered before the window opened
        // must be shown at once, and not only when something else changes.
        let saved_offer = match &self.session.link {
            Ok(link) => view::offered_now(&View::of(&link.client().snapshot())),
            Err(_) => None,
        };
        // Like `PEERSUS_MENU`, only so that a screenshot can show the offer over the window: a file cannot be offered to a window
        // under test by another device and still get a frame back. Never set by a person, and it changes nothing the owner holds.
        let saved_offer = if std::env::var("PEERSUS_OFFER").is_ok_and(|v| v == "1") {
            Some(view::sample_offer(unix_now()))
        } else {
            saved_offer
        };
        let offer: State<Option<view::Offered>> = use_state(move || saved_offer);
        let screenshot_run = std::env::var("PEERSUS_PAGE").is_ok_and(|page| !page.is_empty());
        let tabs = self.session.link.is_ok() && (named() || screenshot_run);
        let body: Element = match &self.session.link {
            Ok(link) => Connected {
                link: link.clone(),
                file_picker: self.session.platform.file_picker.clone(),
                camera: self.session.platform.camera.clone(),
                folder_sink: self.session.platform.folder_sink.clone(),
                notifications: self.session.platform.notifications.clone(),
                camera_choice: self.session.platform.camera_choice.clone(),
                form,
                room: size().0,
                page: current,
                choice,
                language,
                scale,
                accent,
                custom,
                named,
                palette,
                settings_tab,
                reduced_motion,
                keep_icons,
                two_fingers,
                device_name,
                pictures,
                own_picture,
                offer,
                now,
                menu_open,
                inset_top: safe().top,
                inset_bottom: safe().bottom + if form.bottom_places().is_empty() || !tabs { 0. } else { 64. },
                room_height: size().1,
            }
            .into(),
            Err(reason) => Unreachable { reason: reason.clone(), palette }.into(),
        };
        // The fingers that are down now, by the number the system gave each: what tells a touch of two fingers from one.
        let fingers: State<view::Fingers> = use_state(view::Fingers::default);
        // The sheet takes every touch while it is shown, so a finger lifted over it is never reported lifted to this root: the fingers are
        // forgotten whenever the sheet opens or closes, and the next touch begins from none.
        use_side_effect_with_deps(&menu_open(), {
            let mut fingers = fingers;
            move |_| fingers.write().clear()
        });
        let mut root = rect()
            .width(Size::fill())
            .height(Size::fill())
            // `flex` on the column is what lets the middle area take what is left and leave the bottom bar
            // its own height, instead of the middle taking everything and pushing the bar off the screen.
            .content(Content::flex())
            // The window's own size, measured where it is laid out, decides the form.
            .on_sized(move |e: Event<SizedEventData>| {
                size.set_if_modified((e.area.width(), e.area.height()));
            })
            // Kept clear of the status bar, the navigation bar and a notch: exactly what the platform said,
            // at the window's own scale. Nothing on a desktop, where the window manager does it.
            .padding((safe().top, safe().right, safe().bottom, safe().left))
            .background(palette.background)
            // A desktop window has rounded corners, and what is drawn inside is cut to them. Where the platform reserves a safe area (a
            // phone), the window is the whole screen and has square corners.
            .corner_radius(if safe().top + safe().right + safe().bottom + safe().left == 0. { 12. } else { 0. })
            .overflow(Overflow::Clip)
            .border(Border::new().fill(palette.border).width(1.).alignment(BorderAlignment::Inner))
            // Two fingers down bring Settings down from the top (Android; an option, on by default). Counted by the fingers that are down at the
            // same moment (`view::Fingers`), so a swipe or a pinch on a page does not open it twice, and it does nothing while Settings is already
            // open: a layer does not open itself. A finger whose lift never reached here (it was lifted over the sheet as it closed) is not counted
            // for ever, or every touch after it would be a touch of two fingers and would open Settings by itself.
            .on_touch_start({
                let mut fingers = fingers;
                move |e: Event<TouchEventData>| {
                    let wanted = fingers.write().went_down(e.finger_id, monotonic_ms());
                    if wanted && two_fingers() && tabs && form.settings_places_come_from_the_top() && !menu_open() {
                        link::circle_log("two fingers: Settings opens");
                        menu_open.set(true);
                    }
                }
            })
            .on_touch_move({
                let mut fingers = fingers;
                move |e: Event<TouchEventData>| fingers.write().moved(e.finger_id, monotonic_ms())
            })
            .on_touch_end({
                let mut fingers = fingers;
                move |e: Event<TouchEventData>| fingers.write().went_up(e.finger_id)
            })
            .on_touch_cancel({
                let mut fingers = fingers;
                move |e: Event<TouchEventData>| fingers.write().went_up(e.finger_id)
            })
            .child(header(
                palette,
                current,
                self.session.link.is_ok(),
                Shown { tabs: tabs && form.bottom_places().is_empty(), menu: tabs, hidden: if form.header_hides_on_scroll() { bar_hide().hidden() } else { 0. } },
                form,
                on_select.clone(),
                menu_open,
            ))
            .child(
                rect()
                    .width(Size::fill())
                    .height(Size::flex(1.))
                    .horizontal()
                    .content(Content::flex())
                    // The one sidebar. Where the person is and the one option decide what it is:
                    // hidden by default, its icons when asked, expanded inside Settings, and the
                    // width is followed, so every move between the three is the same move.
                    .child(sidebar::Bar {
                        active: settings_tab(),
                        device_name: device_name.read().clone(),
                        picture: own_picture()
                            .and_then(|digest| pictures.read().get(&digest))
                            .map(avatar_view::Shared),
                        state: Sidebar::state_for(tabs, current == Page::Settings, keep_icons(), form),
                        reduced_motion: reduced_motion(),
                        palette,
                        on_select: on_open_tab.clone(),
                    })
                    .child(body),
            )
            // The offer of a file, between the page and the bottom bar: a decision with a clock on it, on whatever screen the
            // person is on. It is part of the column and not a layer over it, because a layer placed globally was drawn nowhere
            // on the phone and on the desktop, with the same data that the card in the list is drawn from. In the column it has
            // the room it takes, the page above it gives way, and nothing can be drawn over it or under it. Only a window that
            // reaches the owner has one.
            .maybe_child(match (&self.session.link, offer.read().clone()) {
                (Ok(link), Some(offered)) => {
                    Some(Element::from(IncomingOffer {
                        link: link.clone(),
                        offered,
                        palette,
                        now: now(),
                        buttons: self.session.platform.offer_buttons.clone(),
                    }))
                }
                _ => None,
            })
            .child(if form.bottom_places().is_empty() || !tabs {
                rect().into()
            } else {
                Element::from(BottomBar {
                    active: current,
                    places: form.bottom_places(),
                    palette,
                    on_select: on_select.clone(),
                })
            });
        #[cfg(not(target_os = "android"))]
        {
            root = root.child(resize_zones(size().0, size().1));
        }
        // Always handed to `Reveal`, which shows it while wanted and for as long as it takes to
        // go away: taking it out of the tree the moment it is closed is what made it vanish.
        let (panel, backdrop) = menu_layer(palette, on_open_tab.clone(), menu_open);
        root = root.child(motion::Reveal {
            // On a phone the menu is the sheet from the top, below, and not this small panel in a corner.
            wanted: menu_open() && tabs && !form.settings_places_come_from_the_top(),
            reduced_motion: reduced_motion(),
            origin: TransformOrigin::top_right(),
            child: panel,
            backdrop: Some(backdrop),
        });
        root
    }
}

#[derive(PartialEq)]
struct Unreachable {
    reason: Offline,
    palette: Palette,
}
impl Component for Unreachable {
    fn render(&self) -> impl IntoElement {
        let (title, text) = self.reason.words();
        rect()
            .expanded()
            .center()
            .spacing(10.)
            .child(label().text(title).color(self.palette.text).font_size(28.).font_weight(FontWeight::BOLD))
            .child(label().text(text).color(self.palette.muted).font_size(15.))
    }
}

/// The screens, drawn from the owner's snapshots.
#[derive(Clone, PartialEq)]
struct Connected {
    link: Link,
    /// The shape of the window, measured by the root: portrait is the phone arrangement.
    form: view::Form,
    /// The width the window has, in pixels, from which each page decides how large its own portal is drawn.
    room: f32,
    page: Page,
    choice: State<ThemeChoice>,
    language: State<i18n::Language>,
    scale: State<f32>,
    accent: State<[u8; 3]>,
    custom: State<CustomTheme>,
    named: State<bool>,
    palette: Palette,
    /// The tab of Settings that is shown, kept by the root so that the menu can set it.
    settings_tab: State<SettingsTab>,
    /// Where the places are shown, and whether decorative motion is off: both are read by the
    /// root, which lives above this and so learns of a change only through these.
    reduced_motion: State<bool>,
    /// The one saved option of the sidebar, and the name its profile shows: read by the root.
    keep_icons: State<bool>,
    /// Whether a touch of two fingers opens the Settings overlay (Android): written here from every snapshot, read by the root.
    two_fingers: State<bool>,
    device_name: State<String>,
    /// The pictures this window holds, by digest: filled here from every snapshot, read by what draws one.
    pictures: State<shelf::Held>,
    /// The digest of this device's own picture, written here and read by the root, like the name.
    own_picture: State<Option<peersus_core::profile::Digest>>,
    /// The file that is offered to this person now, written here from every snapshot and drawn by the root over the window.
    offer: State<Option<view::Offered>>,
    /// The second now, Unix seconds, from the window's one clock: handed to the pages that count down. A STATE, read only by the page
    /// that needs it (Send, in its own branch), and not a plain number: a number in this component would make it differ, and so be
    /// drawn again, every second on every page, the lists of Peers and Settings included, for a clock only Send looks at.
    now: State<u64>,
    /// The platform's own file picker, when the toolkit has no dialog (Android).
    file_picker: Option<link::FilePicker>,
    /// The platform's camera, for reading the code of another device (Android).
    camera: Option<link::CameraControl>,
    /// The platform's way to put a received file in a folder the person chose, where that is not a path (Android).
    folder_sink: Option<link::FolderSink>,
    /// The platform's notifications, which the person can block outside the program (Android).
    notifications: Option<link::NotificationsControl>,
    /// The platform's choice of which camera reads a code, for a phone that has several (Android).
    camera_choice: Option<link::CameraChoice>,
    /// Whether the menu is open. On a phone it is the Settings overlay, drawn here because the pages it shows are built here.
    menu_open: State<bool>,
    /// The margin the system keeps at the top of the screen, and what must stay clear at the bottom (the bar of places and the system's margin).
    inset_top: f32,
    inset_bottom: f32,
    /// The height of the window, in pixels.
    room_height: f32,
}

// `render` returns one concrete `Element` because it leaves early with a different screen.
#[allow(refining_impl_trait)]
impl Component for Connected {
    fn render(&self) -> Element {
        let link = self.link.clone();
        let initial = link.client().snapshot();
        // What the record held when the window opened is the history, and Send and Receive never list it: it is said once, here, from the snapshot the
        // window opened on, and `view::refresh` keeps it across every later snapshot.
        let mut view = use_state(|| {
            let mut first = View::of(&initial);
            first.transfers_at_start = view::ids_at_start(&first.transfers);
            first
        });
        let mut alive = use_state(|| true);
        let mut chosen: State<Vec<String>> = use_state(Vec::new);
        // A received file that could not be put in the folder the person chose: said on screen, since it is kept in the program's
        // own folder and the person would otherwise think it arrived where they asked.
        let mut not_delivered: State<Option<String>> = use_state(|| None);
        let mut choice = self.choice;
        let mut language = self.language;
        let mut scale = self.scale;
        let mut accent = self.accent;
        let mut custom = self.custom;
        let mut reduced_motion = self.reduced_motion;
        let mut keep_icons = self.keep_icons;
        let mut two_fingers = self.two_fingers;
        let mut device_name = self.device_name;
        let held = self.pictures;
        let mut own_picture = self.own_picture;
        let mut offer = self.offer;
        let mut named = self.named;
        named.set_if_modified(!view.peek().needs_a_name());

        // Redraw from the snapshots the owner publishes. The view is replaced only when it
        // changed, so a quiet second leaves the tree alone.
        let watching = link.client().clone();
        let sink = self.folder_sink.clone();
        if let Some(sink) = sink.clone() {
            use_hook(move || {
                // The result of a hand-over arrives on a thread of the platform, after the last snapshot, so it is looked for on
                // the window's own clock and not only when a snapshot comes.
                let mut ticks = link::clock(unix_now, std::time::Duration::from_millis(250));
                spawn(async move {
                    loop {
                        let results = sink.picked.take_delivered();
                        if !results.is_empty() {
                            let now = not_delivered.peek().clone();
                            not_delivered.set_if_modified(view::delivery_note(now, &results));
                        }
                        if ticks.changed().await.is_err() {
                            break;
                        }
                    }
                });
            });
        }
        use_hook(move || {
            // The shelf lives in this task and is shared only with the fetches it starts, which run on the
            // same thread as the window: what the rest of the window sees is `held`, a state.
            let shelf = shelf::Driven::default();
            // The watch channel needs no particular executor to be awaited on.
            spawn(async move {
                let mut updates = watching.subscribe();
                loop {
                    let latest = updates.borrow_and_update().clone();
                    let mut next = view.peek().clone();
                    // A file that arrived whole into the program's staging is put in the folder the person chose for it, by the
                    // platform: once, since the folder is given up when asked. A file that could not be put there stays in the
                    // staging (nothing sent to this person is lost); what the platform says of each hand-over is taken so the
                    // list does not grow, and is written to the diagnostics.
                    if let Some(sink) = &sink {
                        for handoff in view::handoffs(&View::of(&latest).transfers, |id| sink.picked.tree_for(id)) {
                            (sink.deliver)(&handoff);
                        }
                        // A transfer that ended without arriving goes nowhere: the folder remembered for it is let go of.
                        for item in View::of(&latest).transfers.iter().filter(|item| item.incoming && matches!(item.phase, view::Phase::Cancelled | view::Phase::Failed(_))) {
                            sink.picked.forget(&item.id);
                        }
                        // Results are taken on the clock (above), not here: a result that arrives after the last snapshot is not lost.
                    }
                    if view::refresh(&mut next, &latest) {
                        choice.set_if_modified(next.settings.theme);
                        language.set_if_modified(next.settings.language);
                        scale.set_if_modified(next.settings.scale);
                        accent.set_if_modified(next.settings.accent);
                        custom.set_if_modified(next.settings.custom_theme.clone());
                        reduced_motion.set_if_modified(next.settings.reduced_motion);
                        keep_icons.set_if_modified(next.settings.collapse_to_icons);
                        two_fingers.set_if_modified(next.settings.two_finger_settings);
                        device_name.set_if_modified(next.settings.device_name.clone());
                        named.set_if_modified(!next.needs_a_name());
                        own_picture.set_if_modified(next.profile_picture);
                        offer.set_if_modified(view::offered_now(&next));
                        view.set(next.clone());
                    }
                    // Every snapshot, whether or not the view changed: the view starts from the snapshot
                    // the window opened on, so a picture saved before it opened would otherwise never be
                    // fetched, and a fetch that failed would never be tried again on a quiet owner.
                    let fetch = {
                        let client = watching.clone();
                        move |digest| {
                            let client = client.clone();
                            async move {
                                match client.command(Command::FetchPicture { digest }).await {
                                    Ok(Reply::Picture(picture)) => Some(picture),
                                    _ => None,
                                }
                            }
                        }
                    };
                    let publish = move |now: shelf::Held| {
                        let mut held = held;
                        held.set_if_modified(now);
                    };
                    for fetching in shelf.follow(&shelf::named_by(&next), fetch, publish) {
                        spawn(fetching);
                    }
                    if updates.changed().await.is_err() {
                        // The refresh task ended: the owner went away.
                        alive.set(false);
                        break;
                    }
                }
            });
        });

        let palette = self.palette;
        if !alive() {
            return Unreachable { reason: Offline::NotRunning, palette }.into();
        }
        let current = view.read().clone();
        let mut page = rect().expanded().vertical();
        // Until the device has a name, there is nothing else on screen but the question.
        let screenshot_run = std::env::var("PEERSUS_PAGE").is_ok_and(|page| !page.is_empty());
        if current.needs_a_name() && !screenshot_run {
            return page.child(Welcome { link: link.clone(), palette, form: self.form }).into();
        }
        for error in &current.errors {
            page = page.child(ErrorBanner { text: error.clone(), palette });
        }
        if let Some(text) = not_delivered.read().clone() {
            page = page.child(ErrorBanner { text, palette });
        }
        // A pairing request has a clock on it and is a decision about who may connect, so it is
        // shown on whatever screen the person is on, above it, and not only on Receive.
        if !current.approvals.is_empty() {
            let mut requests = rect().width(Size::fill()).padding((16., 56.)).spacing(12.).background(palette.surface);
            for approval in &current.approvals {
                requests = requests.child(ApprovalCard { link: link.clone(), approval: approval.clone(), palette });
            }
            page = page.child(requests);
        }
        let body: Element = match self.page {
            Page::Send => SendPage {
                link: link.clone(),
                view: current.clone(),
                chosen: chosen.read().clone(),
                pictures: held.read().clone(),
                palette,
                form: self.form,
                room: self.room,
                // A press on a device that is not picked adds it, after the ones picked before; a second press on one that is picked lets it
                // go. With none picked there is no destination, and a send is by code.
                on_choose: EventHandler::new(move |id: String| {
                    let mut next = chosen.peek().clone();
                    match next.iter().position(|picked| *picked == id) {
                        Some(at) => {
                            next.remove(at);
                        }
                        None => next.push(id),
                    }
                    chosen.set(next);
                }),
                session_picker: self.file_picker.clone(),
                now: *self.now.read(),
            }
            .into(),
            Page::Receive => ReceivePage {
                link: link.clone(),
                view: current.clone(),
                palette,
                form: self.form,
                room: self.room,
                camera: self.camera.clone(),
            }
            .into(),
            Page::Transfers => TransfersPage { link: link.clone(), view: current.clone(), palette, form: self.form }.into(),
            Page::Peers => DevicesPage { link: link.clone(), view: current.clone(), pictures: held.read().clone(), palette, form: self.form, room: self.room, camera: self.camera.clone(), file_picker: self.file_picker.clone() }.into(),
            Page::Settings => {
                SettingsPage {
                    link: link.clone(),
                    view: current.clone(),
                    pictures: held.read().clone(),
                    palette,
                    form: self.form,
                    tab: self.settings_tab,
                    on_close: None,
                    picture: current
                        .profile_picture
                        .and_then(|digest| self.pictures.read().get(&digest))
                        .map(avatar_view::Shared),
                    file_picker: self.file_picker.clone(),
                    notifications: self.notifications.clone(),
                    camera_choice: self.camera_choice.clone(),
                    camera: self.camera.clone(),
                    room: self.room,
                }
                .into()
            }
        };
        // While the Settings overlay is open on a phone, the page under it takes no pointer at all, a touch, a drag or a wheel: an element that is
        // not interactive is skipped by the toolkit before any event is delivered, and so are all of its children (the scrolling of the page
        // included). The overlay is the page's sibling and not its child, so it stays live. A person who closes the overlay has the page back
        // exactly as it was, where it was.
        let under_overlay = self.form.settings_places_come_from_the_top() && *self.menu_open.read();
        let mut page = page.child(rect().expanded().interactive(!under_overlay).child(body));
        // Settings on a phone: the page is not a place to go to but an overlay that comes down from the top over the page that is shown, and
        // is drawn here, where the data it shows is read. Its body is the Settings page itself, all of its groups one under the other.
        if self.form.settings_places_come_from_the_top() {
            let mut menu_open = self.menu_open;
            page = page.child(top_sheet::Sheet {
                wanted: *self.menu_open.read(),
                reduced_motion: *self.reduced_motion.read(),
                inset_top: self.inset_top,
                inset_bottom: self.inset_bottom,
                room: self.room_height,
                palette,
                body: SettingsPage {
                    link: link.clone(),
                    view: current.clone(),
                    pictures: held.read().clone(),
                    palette,
                    form: self.form,
                    tab: self.settings_tab,
                    on_close: Some(EventHandler::new(move |()| menu_open.set(false))),
                    picture: current
                        .profile_picture
                        .and_then(|digest| self.pictures.read().get(&digest))
                        .map(avatar_view::Shared),
                    file_picker: self.file_picker.clone(),
                    notifications: self.notifications.clone(),
                    camera_choice: self.camera_choice.clone(),
                    camera: self.camera.clone(),
                    room: self.room,
                }
                .into(),
                on_close: EventHandler::new(move |()| menu_open.set(false)),
            });
        }
        page.into()
    }
}

/// A message from the owner at the top of the window, which the person can dismiss.
///
/// What is remembered is WHICH text was dismissed, not that something was: a different message
/// that arrives later is a new thing and must be seen.
#[derive(PartialEq)]
struct ErrorBanner {
    text: String,
    palette: Palette,
}

impl Component for ErrorBanner {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let mut dismissed: State<Option<String>> = use_state(|| None);
        if dismissed.read().as_deref() == Some(self.text.as_str()) {
            return rect().into_element();
        }
        let text = self.text.clone();
        rect()
            .width(Size::fill())
            .padding((10., 28.))
            .horizontal()
            .cross_align(Alignment::center())
            .content(Content::flex())
            // The palette's "bad" colour, but quieter: an old message is not an alarm.
            .background((
                ((palette.bad.0 as u16 + palette.surface.0 as u16 * 2) / 3) as u8,
                ((palette.bad.1 as u16 + palette.surface.1 as u16 * 2) / 3) as u8,
                ((palette.bad.2 as u16 + palette.surface.2 as u16 * 2) / 3) as u8,
            ))
            .child(rect().width(Size::flex(1.)).child(label().text(i18n::tr(&self.text).to_owned()).color(palette.text).font_size(13.)))
            .child(
                rect()
                    .padding((2., 10.))
                    .on_press(move |_| dismissed.set(Some(text.clone())))
                    .child(label().text("\u{2715}").color(palette.text).font_size(14.)),
            )
            .into_element()
    }
}

/// The first screen of a fresh install: it asks what this device should be called, because every
/// other device will list it by that name and two called alike cannot be told apart.
#[derive(PartialEq)]
struct Welcome {
    link: Link,
    palette: Palette,
    form: view::Form,
}

impl Component for Welcome {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let form = self.form;
        let name = use_state(String::new);
        let link = self.link.clone();
        let save = move |_| {
            let value = name.read().trim().to_owned();
            if !value.is_empty() {
                link.send(Command::PatchSettings {
                    patch: SettingsPatch { device_name: Some(value), ..Default::default() },
                });
            }
        };
        // The field fills the room it has, up to a width a person can read a name in. It is never given a
        // fixed width: on a phone that width is wider than the screen, and the button beside it was off it.
        let field = Input::new(name).placeholder(crate::i18n::tr("For example: Alex's laptop")).width(Size::fill());
        let button = Button::new().filled().on_press(save).child(crate::i18n::tr("Continue"));
        let ask: Element = if form.ask_stacks_field_and_button() {
            // One above the other, the button as wide as the field: easy to reach, and always on screen.
            rect().width(Size::fill()).spacing(12.).child(field).child(button.width(Size::fill())).into()
        } else {
            rect().width(Size::fill()).horizontal().spacing(10.).child(field).child(button).into()
        };
        rect()
            .expanded()
            .center()
            .padding((0., form.ask_side_margin()))
            .spacing(18.)
            .child(mono(crate::i18n::tr("WELCOME"), palette.send, 13.))
            .child(
                label().text(crate::i18n::tr("Name this device"))
                    .color(palette.text)
                    .font_size(form.ask_title_size())
                    .font_weight(FontWeight::BOLD),
            )
            .child(
                label().text(crate::i18n::tr("Your other devices will list it by this name, so pick one you will recognise."))
                    .color(palette.muted)
                    .font_size(18.),
            )
            .child(rect().width(Size::fill()).max_width(Size::px(520.)).child(ask))
    }
}

// ---- shared pieces ---------------------------------------------------------------------

/// The big headline of a screen: the first line plain, the second in the screen's accent and
/// slanted, as in the prototype.
///
/// It measures the room it is given and takes the largest size at which its longest line fits
/// (`view::Headline`), and each line is one row that is never broken: a title that wraps puts its
/// second row over the first. Until the room is known it is as small as a title gets, and the
/// first measurement, a frame later, puts it right.
#[derive(PartialEq)]
struct HeadlineView {
    first: &'static str,
    second: &'static str,
    palette: Palette,
    accent: (u8, u8, u8),
    form: view::Form,
}

impl Component for HeadlineView {
    fn render(&self) -> impl IntoElement {
        let (palette, accent) = (self.palette, self.accent);
        let mut room = use_state(|| 0.0_f32);
        let (first, second) = (i18n::tr(self.first), i18n::tr(self.second));
        let longest = first.chars().count().max(second.chars().count());
        let size = view::Headline::size_in(self.form, room(), longest);
        // A line is as tall as its letters need, with a little more for the tail of a "y", so the
        // second sits close under the first as in the prototype and nothing is cut.
        let line = move |text: &'static str, colour: (u8, u8, u8), slanted: bool| -> Element {
            let mut words = label().text(text).color(colour).font_size(size).font_weight(FontWeight::BOLD).max_lines(1);
            if slanted {
                words = words.font_slant(FontSlant::Italic);
            }
            rect().width(Size::fill()).height(Size::px(size * 1.12)).child(words).into()
        };
        rect()
            .width(Size::fill())
            .on_sized(move |e: Event<SizedEventData>| room.set_if_modified(e.area.width()))
            .child(line(first, palette.text, false))
            .child(line(second, accent, true))
    }
}

fn headline(first: &'static str, second: &'static str, palette: Palette, accent: (u8, u8, u8), form: view::Form) -> impl IntoElement {
    HeadlineView { first, second, palette, accent, form }
}

/// The dark disc at the heart of a screen, with rings, as in the prototype.
///
/// `extent` is the width and height of the whole of it, rings included. The disc and the two rings are drawn in
/// the proportions the prototype has them in at its own size ([`qr::PORTAL_OUTER`]), so that a smaller portal is
/// the same picture made smaller and not a different one: three sizes written in the code would be the right
/// ones for one screen only.
fn portal(
    palette: Palette,
    accent: (u8, u8, u8),
    on_press: Option<EventHandler<()>>,
    children: Vec<Element>,
    extent: f32,
) -> impl IntoElement {
    Portal { palette, accent, on_press, children, extent, life: None, sparks: 0, hold: None, going: 0. }
}

/// The portal of a code that has a life: the same, and the ring round it shows how much of that life is left (1 whole, 0 gone), so that the
/// person sees how long the code has before it ends and has to be asked for again.
fn portal_with_life(
    palette: Palette,
    accent: (u8, u8, u8),
    children: Vec<Element>,
    extent: f32,
    life: f32,
    sparks: u32,
) -> impl IntoElement {
    Portal { palette, accent, on_press: None, children, extent, life: Some(life), sparks, hold: None, going: 0. }
}

/// The circle of the Send page: pressed to choose files when none are listed, and when they are, made to do what the page says (`HoldToSend`): a
/// touch makes a code when no device is picked, and it is held until it fills to send to the devices that are. `on_press` is the choosing.
/// `life` is the share, from 1 to 0, of the life a code on show still has: the fuse burns round it, outside the disc and never in it, so that the
/// code inside is as it was drawn and scans as it did; `None` when the circle shows no code.
fn portal_held(
    palette: Palette,
    accent: (u8, u8, u8),
    on_press: Option<EventHandler<()>>,
    children: Vec<Element>,
    extent: f32,
    hold: Option<HoldToSend>,
    going: f32,
    life: Option<f32>,
    sparks: u32,
) -> impl IntoElement {
    Portal { palette, accent, on_press, children, extent, life, sparks, hold, going }
}

/// The title above the circle, centred, as a phone shows them on Send AND on Receive: the one place that decides the space between
/// them, so the two circles are at the same height on the two pages. (Two pages that each wrote their own number were 12 pixels
/// apart.) What comes after the circle is the page's own.
fn title_above_circle(form: view::Form, hero: impl IntoElement, circle: impl IntoElement) -> Rect {
    rect().width(Size::fill()).spacing(form.page_section_gap()).cross_align(Alignment::center()).child(hero).child(circle)
}

/// Whether a finger is held on a circle long enough to be a hold (and not a swipe), written by the circle and read by the page under it, so
/// that the page stops following the finger once the circle is held and carries on following it before that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HoldingCircle(bool);

/// Lowers the holding flag when it is dropped: the end of the task that raised it, or the task being dropped with the circle that ran it.
struct ReleaseHold(State<HoldingCircle>);

impl Drop for ReleaseHold {
    fn drop(&mut self) {
        self.0.set_if_modified(HoldingCircle(false));
    }
}

/// What a circle needs to be held until it fills and sends, instead of pressed: said by the page from its own state, and only ever carried
/// out by the circle. The files are never in it; the page holds them and sends them once when it is told the circle is full.
#[derive(Clone, PartialEq)]
struct HoldToSend {
    /// What the circle does at this moment, from one place (`view::circle_does`): choose files, make a code, be held to send, or be held back.
    does: view::CircleDoes,
    /// What the page does when the circle says so: choose files, make the code, or send to the devices picked. Each once for a touch.
    on_act: EventHandler<view::CircleAct>,
}

/// Milliseconds on a clock that only goes forward, from the first time it is asked: what a hold is timed on. Not the system time, which a person
/// or the network can change while a finger is down, and which would then fill the circle at once or never.
fn monotonic_ms() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    u64::try_from(START.get_or_init(std::time::Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// The disc with its rings. What presses it is a GESTURE judged by [`view::Touch`], and not the toolkit's own press, which
/// is any touch that ends in the same box it began in: that made the four corners of the square round the circle press it,
/// and a swipe that began and ended on it press it too. The circle of the Send page is held instead, and judged by [`view::CircleTouch`].
///
/// The move and the lift are listened to in the toolkit's CAPTURE phase, which reaches this before anything under it can
/// take the event: a page that is being scrolled stops the ordinary move events from arriving, and a swipe that is not seen
/// is a swipe taken for a tap.
#[derive(PartialEq)]
struct Portal {
    palette: Palette,
    accent: (u8, u8, u8),
    on_press: Option<EventHandler<()>>,
    children: Vec<Element>,
    extent: f32,
    /// The share, from 1 to 0, of the time a code still has, for a portal that shows one: drawn as a fuse that burns round a thin outer ring.
    life: Option<f32>,
    /// The frame of the sparks of that fuse, which counts up as time goes by; held still when motion is reduced.
    sparks: u32,
    /// Held until it fills and sends, for the circle of the Send page; `None` for every other circle, which is pressed.
    hold: Option<HoldToSend>,
    /// How far the files of the batch have come, from 0 to 1, while they go (see `view::circle_fill`); 0 for a circle that is not sending.
    going: f32,
}

impl Component for Portal {
    fn render(&self) -> impl IntoElement {
        let (palette, accent, extent) = (self.palette, self.accent, self.extent);
        let share = extent / qr::PORTAL_OUTER;
        let ring = |size: f32, colour: (u8, u8, u8), width: f32| -> Element {
            let size = size * share;
            rect()
                .position(Position::new_absolute().left((extent - size) / 2.).top((extent - size) / 2.))
                .width(Size::px(size))
                .height(Size::px(size))
                .corner_radius(size / 2.)
                .border(Border::new().fill(colour).width(width).alignment(BorderAlignment::Inner))
                .into()
        };
        let disc_size = qr::PORTAL_DISC * share;
        // The line round the disc is a whole number of device pixels (see `Form::portal_line`), from the scale this window has.
        let line = view::Form::portal_line(*Platform::get().current_window().scale_factor.peek());
        // The gesture of the finger that is on the disc, kept between events. Only a finger that went down ON the circle
        // is followed; anywhere else it is nothing.
        let mut disc = rect()
            .width(Size::px(disc_size))
            .height(Size::px(disc_size))
            .corner_radius(disc_size / 2.)
            .center()
            .spacing(8.)
            .background(palette.portal)
            .border(Border::new().fill(accent).width(line).alignment(BorderAlignment::Inner));
        // How full the circle is while it is held, from 0 to 1. Only the Send circle has one; it is set from the clock while a finger is down, and
        // is back to 0 the moment it is lifted, so that nothing is left filling in a page nobody touches. Made BEFORE the disc is finished, because
        // the disc is what it fills.
        let mut fill = use_state(|| 0f32);
        // The circle filling itself: a disc of the accent that grows from the middle to the edge as the finger holds, whole at the moment it sends.
        // It is the FIRST thing in the disc, so that its words are drawn over it, and it is translucent, so that they stay readable on any accent.
        // It only draws: it is not interactive, and takes no part in any gesture.
        // What the finger has held and how far the files have come are two things that never happen together (a circle that is going is shut and
        // takes no finger), so the larger of the two is the one that is on.
        let growing = view::fill_disc((*fill.read()).max(self.going), disc_size);
        if growing > 0. {
            disc = disc.child(
                rect()
                    .position(Position::new_absolute().left((disc_size - growing) / 2.).top((disc_size - growing) / 2.))
                    .width(Size::px(growing))
                    .height(Size::px(growing))
                    .corner_radius(growing / 2.)
                    .background(Color::from_rgb(accent.0, accent.1, accent.2).with_a(140))
                    .interactive(false),
            );
        }
        for child in self.children.clone() {
            disc = disc.child(child);
        }
        let point = |e: &PointerEventData| {
            let (g, l) = (e.global_location(), e.element_location());
            ((l.x as f32, l.y as f32), (g.x as f32, g.y as f32))
        };
        // The hooks are made on EVERY draw, in the same order, whether or not this circle is held to send: a hook made only sometimes would
        // change the order between two draws of the same circle. What they do is gated on `self.hold`.
        let mut circle = use_state(|| view::CircleTouch::new(disc_size));
        // The page under the circle follows a finger that moves, until the finger has been held long enough to be a hold; `Scrolling` reads it.
        let mut holding: State<HoldingCircle> = consume_context();
        // The number of the touch that is current: every touch is given the next one, and a clock acts only while the number it was started for is
        // still this one (see `view::TouchNumber`). Made on every draw with the other hooks.
        let mut touch_number: State<view::TouchNumber> = use_state(view::TouchNumber::default);
        // ONE mechanism for every circle: the Send page's circle is held or tapped for what it does, and a circle that only has a press (Receive) is the
        // same circle whose one act is that press. Two mechanisms were two behaviours: one answered at once and the other did not.
        let hold = self.hold.clone().or_else(|| {
            self.on_press.clone().map(|press| HoldToSend {
                does: view::CircleDoes::ChooseFiles,
                on_act: EventHandler::new(move |_: view::CircleAct| press.call(())),
            })
        });
        if let Some(hold) = hold {
            let tick_act = hold.on_act.clone();
            // One touch, with its own clock, only while a finger is down (see above).
            let does = hold.does;
            let up_act = hold.on_act.clone();
            let choose = self.on_press.clone();
            disc = disc
                .on_pointer_down(move |e: Event<PointerEventData>| {
                    let (in_box, window) = point(&e);
                    // A touch is given the next number before anything else: whatever clock still runs for the one before has just become stale.
                    let after = touch_number.peek().next();
                    touch_number.set(after);
                    let mut next = view::CircleTouch::new(disc_size);
                    next.down(in_box, window, monotonic_ms());
                    // A mouse button that is down stays down until it is released, and its release reaches the circle, in or out of it: a hand that
                    // rests says nothing, and that is not a lost lift. Only a touch can lose its lift, so only a touch is judged by its silence. By
                    // the kind of the event, and not the platform: a touch screen on a desktop is a finger.
                    if matches!(&*e, PointerEventData::Mouse(_)) {
                        next.steady();
                    }
                    let alive = next.is_down();
                    holding.set_if_modified(HoldingCircle(false));
                    link::circle_log(&format!("down at {in_box:?}, alive={alive}, does={does:?}"));
                    circle.set(next);
                    fill.set_if_modified(0.);
                    // The page under the circle takes a finger that moves for the start of a scroll (the toolkit starts after 2 px, less than a held
                    // finger trembles), and it must: a circle that took the touch from the page left a person with nothing to scroll by when it
                    // covers the screen. A hold that the page carries off is a swipe, and it ends (see `CircleTouch::moved`): touch, hold and
                    // move is not a press.
                    // A clock only for a touch that is alive and could fill: not outside the round, and only for a circle that is held to send. The
                    // task that listens is started HERE, where its scope is stable, owns the clock's receiver, and ends when the finger is no longer
                    // down, which drops the receiver and ends the clock's thread.
                    if alive && does == view::CircleDoes::HoldToSend {
                        let mut receiver = link::clock(monotonic_ms, std::time::Duration::from_millis(33));
                        let (mut circle, mut fill, mut holding, tick_act) = (circle, fill, holding, tick_act.clone());
                        let claimed = *touch_number.peek();
                        spawn(async move {
                            // However this ends (the finger lifted, the touch strayed, or the circle went away mid-hold and the task is dropped), the page
                            // is let go: the flag lives at the application, and one left raised would stop every page from scrolling.
                            let _release = ReleaseHold(holding);
                            link::circle_log("the clock listens");
                            while receiver.changed().await.is_ok() {
                                // A clock belongs to the touch it was started for. Once that is no longer the current touch (it was lifted, and another
                                // may already have begun on the same state), this clock has nothing to do: it must not fill, empty or send for it.
                                if !touch_number.peek().is_current(claimed) {
                                    link::circle_log("the clock of an earlier touch ends");
                                    break;
                                }
                                let mut next = *circle.peek();
                                if !next.is_down() {
                                    link::circle_log("the touch is no longer down, the clock ends");
                                    break;
                                }
                                let now = *receiver.borrow_and_update();
                                let act = next.tick(now, does);
                                let shown = next.fill(now, does);
                                circle.set(next);
                                fill.set_if_modified(shown);
                                // Held long enough to be a hold: the page under it stops following the finger (see `Scrolling`).
                                let was_holding = holding.peek().0;
                                holding.set_if_modified(HoldingCircle(next.is_holding(now)));
                                if !was_holding && next.is_holding(now) {
                                    link::circle_log("held long enough, the page stops following the finger");
                                }
                                if act != view::CircleAct::Nothing {
                                    link::circle_log(&format!("the clock says {act:?}"));
                                    tick_act.call(act);
                                }
                            }
                            fill.set_if_modified(0.);
                        });
                    }
                })
                .on_capture_global_pointer_move(move |e: Event<PointerEventData>| {
                    let (_, window) = point(&e);
                    let mut next = *circle.peek();
                    next.moved(window);
                    // Every move the toolkit reports of the finger is a sign that it is still there (a resting finger trembles, and the toolkit says
                    // so): a finger that is no longer heard of is taken to be gone, and its hold sends nothing (see `CircleTouch::tick`).
                    next.seen(monotonic_ms());
                    // A touch that strayed is over: the circle empties and the listening task sees it and ends.
                    if !next.is_down() {
                        if circle.peek().is_down() {
                            link::circle_log(&format!("the finger strayed, so it is a swipe and the hold ends (at {window:?})"));
                        }
                        fill.set_if_modified(0.);
                        holding.set_if_modified(HoldingCircle(false));
                    }
                    circle.set(next);
                })
                // The lift is the LOCAL press and not a global one (see `Touch`): here the position in the box is the real one.
                .on_pointer_press(move |e: Event<PointerEventData>| {
                    let (in_box, window) = point(&e);
                    let mut next = *circle.peek();
                    let act = next.up(in_box, window, monotonic_ms(), does);
                    link::circle_log(&format!("lifted at {in_box:?}, does={does:?}, act={act:?}"));
                    circle.set(next);
                    // The touch is over: its number moves on, so that its clock, if it is still running, finds it is no longer the current one.
                    let after = touch_number.peek().next();
                    touch_number.set(after);
                    fill.set_if_modified(0.);
                    holding.set_if_modified(HoldingCircle(false));
                    match act {
                        view::CircleAct::Nothing => {}
                        // Choosing files is the circle's own press, as it always was: not the page's to carry out.
                        view::CircleAct::Choose => {
                            if let Some(choose) = &choose {
                                choose.call(());
                            }
                        }
                        view::CircleAct::MakeCode | view::CircleAct::Send => up_act.call(act),
                    }
                })
                // A lift anywhere in the window that the circle's own press did NOT take: a finger lifted outside the round, or carried off by the page,
                // never makes that press, and without this the touch would go on, and the circle with it, until the finger had been silent for as long
                // as a finger may be. The toolkit fires the non-capture global events LAST (`freya-core` `events/name.rs`: "Non-capture globals fire
                // last"), so for a lift on the circle the press above has already taken the touch and there is nothing left here; no timer, and no task:
                // the toolkit's tasks run with no reactor, and anything that waits there panics. It does nothing at all unless THIS circle has a touch in
                // progress, so a lift anywhere else in the window costs one comparison and changes nothing.
                .on_global_pointer_up(move |_| {
                    let mut next = *circle.peek();
                    if !next.is_down() {
                        return;
                    }
                    next.cancel();
                    circle.set(next);
                    let after = touch_number.peek().next();
                    touch_number.set(after);
                    fill.set_if_modified(0.);
                    holding.set_if_modified(HoldingCircle(false));
                    link::circle_log("lifted away from the circle: the touch ends and sends nothing");
                })
                .on_touch_cancel(move |_| {
                    let mut next = *circle.peek();
                    next.cancel();
                    circle.set(next);
                    let after = touch_number.peek().next();
                    touch_number.set(after);
                    fill.set_if_modified(0.);
                    holding.set_if_modified(HoldingCircle(false));
                    link::circle_log("the system cancelled the touch");
                });
        }
        rect()
            .width(Size::px(extent))
            .height(Size::px(extent))
            .center()
            .child(ring(418., palette.border, 1.))
            .maybe_child(self.life.map(|left| {
                // The time a code has left, as a fuse that burns round a THIN ring outside the disc of the code, from the top and clockwise: the part that
                // is not yet burnt is lit as one continuous line with a glow, the tip that burns is bright, and it throws sparks. Nothing of it lies on the
                // disc of the code (a conic fill under the disc covered half of it).
                let outer = 418. * share;
                let centre = extent / 2.;
                let radius = outer / 2. - 3. * share;
                let scale = share.max(0.5);
                let dot = |x: f32, y: f32, size: f32, colour: Color| -> Element {
                    rect()
                        .position(Position::new_absolute().left(x - size / 2.).top(y - size / 2.))
                        .width(Size::px(size))
                        .height(Size::px(size))
                        .corner_radius(size / 2.)
                        .background(colour)
                        .into()
                };
                let mut fuse = rect()
                    .position(Position::new_absolute().left(0.).top(0.))
                    .width(Size::px(extent))
                    .height(Size::px(extent))
                    .interactive(false)
                    // What is left of the fuse is ONE line, continuous, with a glow, and not a row of dots: drawn on the canvas as an arc from the tip
                    // round to the top (`view::fuse_arc`, the same place `view::fuse_tip` puts the tip). A wide faint stroke blurred under a thin bright
                    // one is the glow. It lies on the thin ring outside the disc of the code and never reaches it, so what is scanned is as it was drawn.
                    .background(RenderCallback::new(move |context| {
                        use freya::engine::prelude::{Paint, PaintCap, PaintStyle, SkBlurStyle, SkMaskFilter, SkRect};
                        let RenderTarget::Background { canvas, .. } = &context.target else { return };
                        let Some((start, sweep)) = view::fuse_arc(left) else { return };
                        // The toolkit has already moved the canvas to the corner of this element and scaled it by the display's factor before it calls
                        // this, so everything here is in the same logical units as `centre`, `radius` and `scale`: the line lies on the ring the tip
                        // and the sparks are on, on any screen. Scaled once more by the factor it came out as large as the screen's density.
                        let oval = SkRect::from_xywh(centre - radius, centre - radius, 2. * radius, 2. * radius);
                        let (red, green, blue) = accent;
                        // The light of a fuse is not one even band: it falls off gradually from a bright thin core. So the line is three layers, from the
                        // outside in: a wide faint haze, a nearer softer light, and the thin core, which is the accent lifted toward white because
                        // what burns is lighter than what is painted. Each is blurred less and is more opaque than the one outside it. The haze is the
                        // widest and stays within the space between the ring and the disc of the code (half its width and three times its blur, together,
                        // are less than that space): no light of it falls on the code, which is scanned as it was drawn.
                        let lift = |channel: u8, by: f32| (f32::from(channel) + (255. - f32::from(channel)) * by).round() as u8;
                        let layers: [(f32, Option<f32>, (u8, u8, u8), u8); 3] = [
                            (8., Some(2.), (red, green, blue), 60),
                            (4.5, Some(1.2), (red, green, blue), 120),
                            (2.2, None, (lift(red, 0.35), lift(green, 0.35), lift(blue, 0.35)), 255),
                        ];
                        for (width, blur, (r, g, b), alpha) in layers {
                            let mut paint = Paint::default();
                            paint.set_anti_alias(true);
                            paint.set_style(PaintStyle::Stroke);
                            paint.set_stroke_cap(PaintCap::Round);
                            paint.set_stroke_width(width * scale);
                            paint.set_color(Color::from_rgb(r, g, b).with_a(alpha));
                            if let Some(sigma) = blur {
                                paint.set_mask_filter(SkMaskFilter::blur(SkBlurStyle::Normal, sigma * scale, false));
                            }
                            // A whole fuse is a closed ring: an arc of 360 degrees leaves a seam where its two ends meet, so it is drawn as a circle.
                            if sweep >= 360. {
                                canvas.draw_oval(oval, &paint);
                            } else {
                                canvas.draw_arc(oval, start, sweep, false, &paint);
                            }
                        }
                    }));
                // The tip: a bright point with a halo, where the burnt part ends.
                let (tip_x, tip_y) = view::fuse_tip(left, radius, centre);
                fuse = fuse.child(dot(tip_x, tip_y, 16. * scale, Color::from_rgb(255, 200, 80).with_a(60))).child(dot(
                    tip_x,
                    tip_y,
                    7. * scale,
                    Color::from_rgb(255, 245, 210),
                ));
                // The sparks, which fly off the tip: only OUTSIDE the disc of the code. One that would be inside it is not drawn, so that nothing of
                // them lies on the code.
                let disc_radius = qr::PORTAL_DISC * share / 2.;
                for particle in view::fuse_particles(self.sparks) {
                    let (x, y) = (tip_x + particle.x * scale, tip_y + particle.y * scale);
                    if view::point_on_disc(x, y, centre, disc_radius, 2. * scale) {
                        continue;
                    }
                    let colour = Color::from_rgb(particle.colour.0, particle.colour.1, particle.colour.2).with_a((particle.alpha.clamp(0., 1.) * 255.) as u8);
                    fuse = fuse.child(dot(x, y, particle.size * scale, colour));
                }
                fuse
            }))
            .child(ring(384., palette.border, 1.))
            .child(disc)
    }
}

/// The solid button of the prototype: dark in the light theme, light in the dark one.
#[derive(PartialEq)]
struct Solid {
    text: &'static str,
    accent: (u8, u8, u8),
    palette: Palette,
    enabled: bool,
    on_press: EventHandler<()>,
}
impl Component for Solid {
    fn render(&self) -> impl IntoElement {
        let (palette, on_press, enabled) = (self.palette, self.on_press.clone(), self.enabled);
        let mut hovered = use_state(|| false);
        rect()
            .padding((14., 22.))
            .horizontal()
            .cross_align(Alignment::center())
            .spacing(12.)
            .corner_radius(12.)
            // Disabled is plainly flat and grey, not the solid button faded: a faded dark button
            // on a dark page reads as dirty rather than as unavailable.
            .background(if !enabled {
                palette.raised
            } else if hovered() {
                (palette.button.0 / 10 * 8, palette.button.1 / 10 * 8, palette.button.2 / 10 * 8)
            } else {
                palette.button
            })
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set(false))
            .on_press(move |_| {
                if enabled {
                    on_press.call(());
                }
            })
            .child(
                rect()
                    .width(Size::px(26.))
                    .height(Size::px(26.))
                    .corner_radius(13.)
                    .background(if enabled { self.accent } else { palette.border }),
            )
            .child(
                label()
                    .text(i18n::tr(self.text))
                    .color(if enabled { palette.button_text } else { palette.muted })
                    .font_size(17.)
                    .font_weight(FontWeight::BOLD),
            )
    }
}

fn section(text: &'static str, palette: Palette) -> Label {
    mono(i18n::tr(text), palette.muted, 12.)
}

/// Who made a package and where to read about it, as text a person reads before trusting it: the author and version, and
/// the link. What the package SAYS about itself, shown as such; it is never made something to press, because following an
/// address out of a list is how a person is led somewhere else.
fn package_lines(card: Rect, package: &peersus_plugin_api::package::Package, palette: Palette) -> Rect {
    let mut card = card;
    if !package.author.is_empty() {
        card = card.child(mono(
            i18n::format(i18n::language(), "by {author} · version {version}", &[("author", &package.author), ("version", &package.version)]),
            palette.send,
            11.,
        ));
    }
    if !package.link.is_empty() {
        card = card.child(mono(package.link.clone(), palette.muted, 11.));
    }
    card
}

/// The scrolling area under a screen's header. Its side margin is the form's: the prototype's wide one on a
/// desktop, a small one on a phone, where the same space would take a third of the screen.
fn frame(palette: Palette, form: view::Form, content: impl IntoElement) -> impl IntoElement {
    Scrolling { palette, form, content: content.into_element(), see_through: false, tops: None, go_to: None }
}


/// The scrolling area of a page, with its OWN scroll position (a page that is left starts again at its top) and the header's hiding
/// told from it: on a phone the bar goes up as the page is read down and comes back as it is scrolled up (see [`view::HeaderHide`]).
/// The position is read after each change of it, here, and what the header shows is written to the state the root provides.
#[derive(PartialEq)]
struct Scrolling {
    palette: Palette,
    form: view::Form,
    content: Element,
    /// Draw no background of its own, so that what is behind it (the panel of the Settings overlay, with its lifted colour, its outline
    /// and its rounded lower corners) shows through. Every page of the application draws its background, as it always did.
    see_through: bool,
    /// Where the groups of the page begin in what is scrolled, from the first, when the page has groups: they are drawn on its scrollbar.
    tops: Option<State<Vec<f32>>>,
    /// A place in what is scrolled that something else asked for (the search of Settings): the page goes there with its own controller, which
    /// nothing outside can reach, and the request is taken off so that it is carried out once.
    go_to: Option<State<Option<f32>>>,
}

impl Component for Scrolling {
    fn render(&self) -> impl IntoElement {
        let (palette, form) = (self.palette, self.form);
        let mut controller = use_scroll_controller(ScrollConfig::default);
        if let Some(mut asked) = self.go_to {
            // Read in the way that this component is drawn again for, so that a request written by the search is seen at once. Copied out
            // first, so that the read is over before the request is taken off. No place asked for goes nowhere: it does not take the person
            // to the top of the page.
            let place: Option<f32> = *asked.read();
            if let Some(place) = place {
                controller.scroll_to_y(-(place as i32));
                asked.set(None);
            }
        }
        let hide = try_consume_context::<State<view::HeaderHide>>();
        // A finger that has been held on a circle long enough to be a hold is the circle's, and the page under it stays where it is: it
        // must not follow a finger that trembles, which carried the page, and the bar over it, up and down under the hold. Before that a
        // finger that moves is a swipe, and the page scrolls as it always did.
        let holding = try_consume_context::<State<HoldingCircle>>().is_some_and(|state| state.read().0);
        // `position()` reads the position in a way that this component is drawn again for, so it runs after every move of the page.
        let top = -controller.position().y;
        // The bar hides as a PAGE is read down. The panel of Settings is not a page under the bar but a layer over it, so what is scrolled in it
        // says nothing to the bar: only the pages (Send, Receive, Peers) write how much of it is hidden.
        if form.header_hides_on_scroll()
            && !self.see_through
            && let Some(mut hide) = hide
        {
            let next = hide.peek().after(top);
            if next != *hide.peek() {
                hide.set(next);
            }
        }
        let scroll = ScrollView::new_controlled(controller).expanded().drag_scrolling(!holding).child(
            rect()
                .width(Size::fill())
                .padding((form.page_vertical_padding(), form.page_side_margin()))
                .spacing(18.)
                .background(if self.see_through { Color::TRANSPARENT } else { Color::from(palette.background) })
                .child(self.content.clone()),
        );
        let Some(tops) = self.tops else {
            return scroll.into_element();
        };
        // The scrollbar is still the toolkit's own, drawn by it; the group icons are added on its track.
        let offsets = tops.read().clone();
        scroll
            .scrollbar(move |context: ScrollBarContext| GroupMarks::on(palette, offsets.clone(), context))
            .into_element()
    }
}

/// The scrollbar of a page that has groups: the toolkit's own scrollbar, unchanged, with an icon for each group on its track at the place where that
/// group is in the real scroll (and never closer to the one before than an icon is wide). The icon of the group being read is larger and lit; a press
/// on an icon scrolls to its group. Nothing else is drawn, and nothing is put in the page.
struct GroupMarks;

impl GroupMarks {
    /// How far before a group's top it already counts as the one being read: its title has come up under the header.
    const LEAD: f32 = 80.;
    /// The disc of a group with its icon, and the larger one of the group being read.
    const NODE: f32 = 24.;
    const NODE_CURRENT: f32 = 32.;
    const ICON: f32 = 14.;
    const ICON_CURRENT: f32 = 19.;
    /// The margin the toolkit's scrollbar leaves at the two ends of its track (its own `SCROLLBAR_MARGIN`).
    const BAR_MARGIN: f32 = 3.;
    /// The width of the column the icons are centred in, at the right edge of the area, over the toolkit's scrollbar: wide enough for the larger icon.
    const COLUMN: f32 = 36.;

    fn on(palette: Palette, offsets: Vec<f32>, context: ScrollBarContext) -> Element {
        let bar = ScrollBar::new(context.clone());
        let viewport = context.viewport_size.height;
        let range = view::scroll_range(context.content_size.height, viewport);
        if range <= 0. || offsets.is_empty() || viewport <= Self::NODE_CURRENT * 2. {
            return bar.into();
        }
        let accent = palette.send;
        let scrolled = -context.scroll_position.y;
        let current = view::current_group(&offsets, scrolled, Self::LEAD, scrolled + 1. >= range);
        // The icons go down the whole length of the toolkit's scrollbar, the track being the area less its margin at both ends.
        let track = (viewport - Self::BAR_MARGIN * 2.).max(0.);
        let controller = context.scroll_controller;
        // On the layer of the toolkit's own scrollbar (999), above the cards and controls of the page, which would otherwise be drawn over it.
        let mut marks = rect().position(Position::new_absolute().top(0.).right(0.)).layer(999).width(Size::px(Self::COLUMN)).height(Size::px(viewport));
        // Where each icon sits: the share of the bar that its group's top is of the whole page, so that a long group has room on the bar and short
        // ones are near each other. Never closer than one icon to the one before, so that two do not lie on one another, and never past the end.
        let content = context.content_size.height.max(1.);
        let room = (track - Self::NODE_CURRENT).max(0.);
        let mut places: Vec<f32> = offsets.iter().map(|top| (top / content).clamp(0., 1.) * track).collect();
        for index in 1..places.len() {
            places[index] = places[index].max(places[index - 1] + Self::NODE);
        }
        for index in (0..places.len().saturating_sub(1)).rev() {
            places[index] = places[index].min(places[index + 1] - Self::NODE);
        }
        for place in places.iter_mut() {
            *place = place.clamp(0., room);
        }
        for (index, which) in SettingsTab::ALL.into_iter().enumerate() {
            let Some(place) = places.get(index).copied() else { break };
            let is_current = current == Some(index);
            // Lit: the group being read and the ones before it.
            let passed = current.is_some_and(|read| index <= read);
            let (size, icon) = if is_current { (Self::NODE_CURRENT, Self::ICON_CURRENT) } else { (Self::NODE, Self::ICON) };
            let fill = if passed || is_current { accent } else { palette.raised };
            let ink = if passed || is_current { glyph::ink_on(accent) } else { glyph::resting_ink(&palette) };
            let target = offsets.get(index).copied().unwrap_or(0.);
            let mut controller = controller;
            marks = marks.child(
                rect()
                    .position(Position::new_absolute().top(Self::BAR_MARGIN + Self::NODE_CURRENT / 2. + place - size / 2.).left((Self::COLUMN - size) / 2.))
                    .width(Size::px(size))
                    .height(Size::px(size))
                    .corner_radius(size / 2.)
                    .center()
                    .background(fill)
                    .border(Border::new().fill(if passed || is_current { accent } else { palette.border }).width(2.).alignment(BorderAlignment::Inner))
                    // A press on an icon scrolls to its group: the scroll position is negative as the content goes up.
                    .on_press(move |_| {
                        controller.scroll_to_y(-(target as i32));
                    })
                    .child(glyph::picture_on(glyph::name_of_tab(which), icon, fill, ink, fill)),
            );
        }
        rect().child(bar).child(marks).into()
    }
}

// ---- Send ------------------------------------------------------------------------------

/// What the hint line says about where a send will go.
fn hint(state: &SendState) -> &'static str {
    match state {
        SendState::Ready(devices) if devices.len() > 1 => "Goes straight to the devices you picked",
        SendState::Ready(_) => "Goes straight to the device you picked",
        SendState::ByCode => "No device picked: a code will be made",
        SendState::ChosenUnavailable => "A device you picked is not available. Pick again, or none to use a code",
    }
}

#[derive(Clone, Copy)]
enum SendNotice {
    Message(&'static str),
    Refused(u32),
    Intake(intake::Summary),
}

impl SendNotice {
    fn intake(intake: &intake::Intake) -> Option<Self> {
        let summary = intake::Summary::from(intake);
        (summary != intake::Summary::default()).then_some(Self::Intake(summary))
    }

    fn words(self) -> String {
        let language = i18n::language();
        match self {
            Self::Message(message) => i18n::translate(language, message).to_owned(),
            Self::Refused(count) => i18n::format(language, "{refused} selected files could not be offered.", &[("refused", &count.to_string())]),
            Self::Intake(summary) => summary.words_in(language).unwrap_or_default(),
        }
    }
}

/// Presentation observes a process-owned producer; leaving the page cannot cancel it.
async fn show_files_code(
    production: link::CodeProduction,
    mut shown: State<Option<ShownCode>>,
    mut expired: State<Option<u64>>,
    mut notice: State<Option<SendNotice>>,
) {
    match production.ready.await {
        Ok(Ok(Some(link::ReadyCode { code, generation, expires_at, total, names: _ }))) => {
            expired.set(None);
            shown.set(Some(ShownCode { code, generation, expected: total, made_at: unix_now(), ends_at: expires_at }));
            // The owner publishes nothing when a code runs out unscanned: the window stops drawing it itself, at the
            // deadline the reply carried, and only if it is still that code.
            let left = view::until_expiry(expires_at, unix_now());
            let mut told = link::after(left, std::time::Duration::from_millis(250));
            spawn(async move {
                if (&mut told).await.is_ok() {
                    expired.set(Some(generation));
                }
            });
            match production.finished.await {
                Ok(refused) => {
                    if refused > 0 {
                        notice.set(Some(SendNotice::Refused(refused)));
                    }
                    if shown.peek().as_ref().is_some_and(|code| code.generation == generation) {
                        let expected = total.saturating_sub(refused);
                        if expected == 0 {
                            shown.set(None);
                        } else if let Some(code) = shown.write().as_mut() {
                            code.expected = expected;
                        }
                    }
                }
                Err(_) => notice.set(Some(SendNotice::Message("The file producer stopped before finishing the selection."))),
            }
        }
        Ok(Ok(None)) => {}
        Ok(Err(reason)) => notice.set(Some(SendNotice::Message(reason))),
        Err(_) => notice.set(Some(SendNotice::Message("The file producer stopped before creating a code."))),
    }
}

/// A files code this window made and shows: the code as text, drawn only while the owner says it can still admit a device, and
/// the generation the owner gave it, which is how everything the owner reports about it (and the files that came of it) is found.
struct ShownCode {
    code: String,
    generation: u64,
    expected: u32,
    /// When the code was shown and when the owner ends it, in seconds on the clock of `unix_now`: what the circle's fuse is drawn from, so that
    /// the time the code has left is the owner's own deadline and not a guess of the window's.
    made_at: u64,
    ends_at: u64,
}

/// Seconds since 1970, the clock a code's deadline is in. Only used to know how long a code has left; a clock that cannot be
/// read gives 0, which leaves the whole life of the code, and the owner refuses it at its own deadline anyway.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Offer the files the person picked to the destinations. With no destination nothing is sent:
/// the choice of files alone never decides who receives them.
fn send_files(link: &Link, destinations: &[String], paths: Vec<PathBuf>) -> tokio::sync::oneshot::Receiver<link::Sent> {
    link.send_paths(destinations.iter().cloned().map(PeerId).collect(), paths)
}

/// Pick files and send them straight to the paired device `peer`: the quick Send on a device's own card, which needs no page of its
/// own and no code. The same rules as Send to a picked device: on a platform with a picker of its own (Android) the files come back
/// open and go in commands of at most the most the owner accepts, and on a desktop the dialog's paths are judged by the same intake
/// (a folder, a file that is gone) and offered together. What was left out is said in `said`, as on the Send page.
fn choose_and_send(link: &Link, picker: Option<link::FilePicker>, peer: String, mut said: State<Option<SendNotice>>) {
    said.set(None);
    if let Some(picker) = picker {
        picker.picked.opened_streaming();
        (picker.open)(true);
        let batches = link::await_picked(picker.picked.clone(), link::LOOK_EVERY, link::PICKER_PATIENCE);
        let handed_off = link.send_picked(vec![PeerId(peer)], batches);
        spawn(async move {
            if let Ok(refused) = handed_off.await && refused > 0 {
                said.set(Some(SendNotice::Refused(refused)));
            }
        });
        return;
    }
    let link = link.clone();
    spawn(async move {
        #[cfg(not(target_os = "android"))]
        if let Some(files) = rfd::AsyncFileDialog::new().pick_files().await {
            let intake = intake::accept(files.iter().map(|file| file.path().to_path_buf()));
            said.set(SendNotice::intake(&intake));
            // The quick send of a device's own card shows no batch under a circle: the answer is not wanted.
            let _ = send_files(&link, std::slice::from_ref(&peer), intake.taken);
        }
        #[cfg(target_os = "android")]
        let _ = (&link, &peer, &said);
    });
}

#[derive(PartialEq)]
struct SendPage {
    link: Link,
    view: View,
    chosen: Vec<String>,
    pictures: shelf::Held,
    palette: Palette,
    form: view::Form,
    /// The width the window has, in pixels, from which each page decides how large its own portal is drawn:
    /// the code in it is not the same on every page, and what it needs is what decides.
    room: f32,
    on_choose: EventHandler<String>,
    /// The platform's own file picker, on a platform that has one in place of the toolkit's dialog.
    session_picker: Option<link::FilePicker>,
    /// The second now, Unix seconds, from the window's one clock: the circle counts down from it while the files wait.
    now: u64,
}

impl Component for SendPage {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let accent = palette.send;
        // Where a send goes is decided in the model and only worded here: to the paired devices
        // the person picked, or else behind a code. Never to a device chosen for them.
        let state = self.view.send_state(&self.chosen);
        let unavailable = matches!(state, SendState::ChosenUnavailable);
        // The files code the window made and is showing, with the generation the owner gave it: what the circle is worked out
        // from (see `view::code_circle`). One state, so that the code and what it is the code OF are never apart.
        let mut shown: State<Option<ShownCode>> = use_state(|| None);
        // The generation of a code whose time ran out unscanned. A newer code is another generation and is not touched by it.
        let mut expired: State<Option<u64>> = use_state(|| None);
        // What the last batch of files left out, worded for the person; `None` when all were taken.
        let mut notice: State<Option<SendNotice>> = use_state(|| None);
        // The files that were chosen and wait to be sent, as the paths of a desktop. Choosing starts nothing and makes no code: the files are only
        // listed here, the person says where they go (the cards, and the button for a code), and sending them is a hold on the circle.
        let mut staged_paths: State<Vec<PathBuf>> = use_state(Vec::new);
        // The transfers as of this draw, for the handlers, which cannot see `self`: what a new choice of files reads to know what still goes.
        let mut transfers_for_handlers: State<Vec<view::Progress>> = use_state(Vec::new);
        // How many FILES were just handed over and not yet answered for by the owner: set when the circle sends, and let go of in the same step that
        // writes the batch from the owner's answer, or when the send is given up. Files, not transfers: a file sent to two devices is one file here, as it
        // is in the circle after the owner answers, so the number never changes between the gap and the answer.
        let mut handed_over: State<usize> = use_state(|| 0);
        // The number of the lap of the circle (files chosen, sent, ended) and the first second the page saw the batch as ended, with the lap it was
        // noted for: what the fade of the result is worked out from, in the window's own clock.
        let mut lap: State<view::Lap> = use_state(view::Lap::default);
        let mut result_noted: State<Option<(view::Lap, u64)>> = use_state(|| None);
        transfers_for_handlers.set_if_modified(self.view.transfers.clone());
        let link = self.link.clone();
        // The ids of the transfers the owner says it made of the files that were sent: the batch, from the owner's own answer (see `link::Sent`). The
        // list under the circle and the circle read this and nothing else to know which transfers are the batch; no guess from the transfers there were.
        let mut batch_ids: State<Vec<String>> = use_state(Vec::new);
        // Every way a file comes in ends here: chosen in the dialog, dropped on the window, or
        // pasted. The same rule judges them all, so a file is never treated by how it arrived.
        let take = EventHandler::new({
            move |offered: Vec<PathBuf>| {
                let intake = intake::accept(offered);
                notice.set(SendNotice::intake(&intake));
                if intake.is_empty() {
                    return;
                }
                // Listed, and nothing more: a device that was picked and has gone is told in the circle, and nothing is sent before the
                // person holds it.
                // Files that are chosen now are a new transfer: the code of the one before (stopped, sent, or gone) is no longer what the circle
                // describes, and would keep saying "stopped 0/1" over files that have not been sent.
                // A new lap: the result of the one before is left behind at once (see `view::Lap`).
                let next_lap = lap.peek().next();
                lap.set(next_lap);
                shown.set(None);
                expired.set(None);
                let kept = view::ids_kept_on_new_choice(&transfers_for_handlers.peek(), &batch_ids.peek(), None);
                batch_ids.set(kept);
                // More files join what already waits (the model refuses a file that is already in the list); a choice made with nothing waiting is
                // the first, and is the whole list.
                let mut waiting = view::Waiting::new(staged_paths.peek().clone());
                waiting.add(intake.taken);
                staged_paths.set(waiting.files().to_vec());
            }
        });
        // The files of an Android selection, held by number in the registry and only LISTED: they wait here, in a guard that lets go of them if
        // the page is left, until the circle is used. Not state: a guard cannot be compared or copied, and what the page shows of it is
        // `staged_names`.
        let held_files: std::rc::Rc<std::cell::RefCell<Option<link::Gathered>>> = use_hook(Default::default);
        let mut staged_names: State<Vec<String>> = use_state(Vec::new);
        // Where what is listed would go: the devices picked (as `send_state` has judged them), none, or one that went away.
        let aim = view::aim_of(&state);
        // Carries out what the circle says, once for a touch. The listed files are taken out of their place first, so that nothing can send the same
        // files twice; with nothing listed or nowhere to go it does nothing.
        let on_circle = EventHandler::new({
            let (link, held_files) = (link.clone(), held_files.clone());
            let aim = aim.clone();
            move |act: view::CircleAct| {
                let (devices, with_code) = match (&act, &aim) {
                    (view::CircleAct::Send, view::Aim::Devices(devices)) => (devices.clone(), false),
                    (view::CircleAct::MakeCode, view::Aim::Code) => (Vec::new(), true),
                    _ => return,
                };
                // An Android selection: ONE hand-over to the devices or the code, from the very files that were listed. `send_gathered` empties
                // the guard, so that ending it lets go of nothing twice.
                // What waits is ONE thing seen in two places (the paths of a desktop, the held files of an Android selection and their names): taken
                // together, and emptied together, so that the circle can never count files that were already taken.
                let gathered = held_files.borrow_mut().take();
                let paths = std::mem::take(&mut *staged_paths.write());
                // How many files go, said BEFORE the listing is emptied: the names of an Android selection, the paths of a desktop one.
                let files_going = staged_names.read().len().max(paths.len());
                staged_names.set(Vec::new());
                if gathered.is_some() {
                    handed_over.set(files_going);
                }
                // An Android selection: ONE hand-over to the devices or the code, from the very files that were listed. `send_gathered` empties
                // the guard, so that ending it lets go of nothing twice.
                if let Some(gathered) = gathered {
                    let peers = devices.iter().cloned().map(PeerId).collect();
                    let (production, handed_off) = link.send_gathered(peers, with_code, gathered);
                    if let Some(production) = production {
                        spawn(show_files_code(production, shown, expired, notice));
                    }
                    spawn(async move {
                        match handed_off.await {
                            Ok(sent) => {
                                // The batch is what the owner says it made of these files: not what a screen can make out from the transfers that
                                // were there before. The count of files handed over is let go of in the same step.
                                batch_ids.set(sent.transfers.clone());
                                handed_over.set(0);
                                if sent.refused > 0 {
                                    notice.set(Some(SendNotice::Refused(sent.refused)));
                                }
                            }
                            Err(_) => handed_over.set(0),
                        }
                    });
                    return;
                }
                if paths.is_empty() {
                    return;
                }
                if with_code {
                    let production = link.code_paths(paths);
                    spawn(show_files_code(production, shown, expired, notice));
                } else {
                    let count = paths.len();
                    handed_over.set(count);
                    let answer = send_files(&link, &devices, paths);
                    spawn(async move {
                        match answer.await {
                            Ok(sent) => {
                                // The batch is what the owner says it made of these files (see `link::Sent`), the same as on Android; the count of
                                // files handed over is let go of in the same step, so there is no moment with both or with neither.
                                batch_ids.set(sent.transfers.clone());
                                handed_over.set(0);
                                if sent.refused > 0 {
                                    notice.set(Some(SendNotice::Refused(sent.refused)));
                                }
                            }
                            // The owner stopped before it answered: nothing is on its way, and a circle left saying so would say it for ever.
                            Err(_) => handed_over.set(0),
                        }
                    });
                }
            }
        });
        let file_picker = self.session_picker.clone();
        // The files an Android picker or a share hands over are read to the end and LISTED into `held_files` (see above).
        // What happens to files the platform's picker is handing over, from the moment its selection is open: they are read to the end and
        // LISTED, and nothing is sent. The same for a picker the person opened and for files another application shared, so that the two can
        // never be treated differently.
        let begin_files = EventHandler::new({
            let (file_picker, held_files) = (file_picker.clone(), held_files.clone());
            move |()| {
                let Some(picker) = file_picker.clone() else { return };
                // Ten minutes is far more than choosing files takes, and it ends a wait nothing will answer.
                let mut batches = link::await_picked(picker.picked.clone(), link::LOOK_EVERY, link::PICKER_PATIENCE);
                let (picked, held_files) = (picker.picked.clone(), held_files.clone());
                spawn(async move {
                    let mut gathered = link::Gathered::new(picked);
                    let mut refused = 0_u32;
                    while let Some(taken) = batches.recv().await {
                        gathered.add(taken.files);
                        refused = refused.saturating_add(taken.refused);
                        if !taken.still_open {
                            break;
                        }
                    }
                    if refused > 0 {
                        notice.set(Some(SendNotice::Refused(refused)));
                    }
                    // A pick that ended with nothing taken (backed out of, or every file refused) leaves what was listed before as it was, as the
                    // desktop does; only a listing with files replaces it, and the earlier guard lets go of its files as it is dropped.
                    if gathered.names().is_empty() {
                        return;
                    }
                    // More files JOIN the guard that already holds some (their numbers stay held, and they come first in the list); with nothing held
                    // this selection is the first and is the whole list. Nothing is let go of by adding, and nothing is held twice.
                    let names = {
                        let mut guard = held_files.borrow_mut();
                        match guard.as_mut() {
                            Some(earlier) => earlier.absorb(gathered),
                            None => *guard = Some(gathered),
                        }
                        guard.as_ref().map(link::Gathered::names).unwrap_or_default()
                    };
                    // A new selection is a new transfer: the code of the one before no longer describes the circle.
                    shown.set(None);
                    expired.set(None);
                    let kept = view::ids_kept_on_new_choice(&transfers_for_handlers.peek(), &batch_ids.peek(), None);
                    batch_ids.set(kept);
                    staged_names.set(names);
                });
            }
        });
        // Files another application shared are already a selection in the registry. Told once, here, as the page draws; a share that is
        // not taken stays held and nobody would ever be asked who gets it.
        if let Some(picker) = &file_picker
            && !unavailable
            && picker.picked.take_external().is_some()
        {
            let begin = begin_files.clone();
            spawn(async move { begin.call(()) });
        }
        let choose = EventHandler::new({
            let take = take.clone();
            let begin_files = begin_files.clone();
            move |()| {
                if unavailable {
                    return;
                }
                // A platform with no dialog of the toolkit's (Android) gives its own picker: files come back as open
                // files that are kept and sent from where they are, never as paths and never copied.
                if let Some(picker) = file_picker.clone() {
                    picker.picked.opened_streaming();
                    (picker.open)(true);
                    begin_files.call(());
                    return;
                }
                let take = take.clone();
                // The dialog runs on its own thread; awaiting it here does not stop the window
                // from drawing while it is open.
                spawn(async move {
                    #[cfg(not(target_os = "android"))]
                    if let Some(files) = rfd::AsyncFileDialog::new().pick_files().await {
                        take.call(files.iter().map(|file| file.path().to_path_buf()).collect());
                    }
                    #[cfg(target_os = "android")]
                    let _ = &take;
                });
            }
        });
        // What the person can do with the files that wait, ONE implementation for both ways a list is held (the paths of a desktop selection, the held
        // files of an Android one with their names): each action takes from both, together, so the list can never be half changed.
        let file_actions = FileActions {
            remove: EventHandler::new({
                let held_files = held_files.clone();
                move |index: usize| {
                    let mut from_guard = held_files.borrow_mut();
                    if let Some(gathered) = from_guard.as_mut() {
                        // The guard lets go of the file in the registry; what the page shows of it follows from what is left.
                        gathered.remove(index);
                        let names = gathered.names();
                        if names.is_empty() {
                            *from_guard = None;
                        }
                        staged_names.set(names);
                    } else {
                        let mut waiting = view::Waiting::new(staged_paths.peek().clone());
                        waiting.remove(index);
                        staged_paths.set(waiting.files().to_vec());
                    }
                }
            }),
            clear: EventHandler::new({
                let held_files = held_files.clone();
                move |()| {
                    // The guard lets go of every held file as it is dropped.
                    held_files.borrow_mut().take();
                    staged_names.set(Vec::new());
                    staged_paths.set(Vec::new());
                }
            }),
            // The picker that opens for the first choice opens to add more, and what it brings joins the list (see `take` and `begin_files`).
            add_more: choose.clone(),
        };

        // A row of cards side by side on a desktop; on a phone one under another, each as wide as the screen.
        let mut devices = if self.form.device_card_width().is_some() {
            rect().width(Size::fill()).horizontal().spacing(14.)
        } else {
            rect().width(Size::fill()).spacing(10.)
        };
        for device in &self.view.devices {
            let on_choose = self.on_choose.clone();
            devices = devices.child(DeviceCard {
                id: device.id.clone(),
                name: device.name.clone(),
                picture: device.picture.and_then(|digest| self.pictures.get(&digest)).map(avatar_view::Shared),
                online: device.presence == Presence::Online,
                picked: self.chosen.contains(&device.id),
                palette,
                form: self.form,
                on_choose,
            });
        }
        let devices: Element = if self.view.devices.is_empty() {
            label().text(crate::i18n::tr("No saved devices yet. Send by code, and keep the device when asked.")).color(palette.muted).font_size(15.).into()
        } else {
            devices.into()
        };

        let beside = self.form.page_puts_portal_beside_text();
        let hero = rect()
            .width(if beside { Size::flex(1.) } else { Size::fill() })
            .spacing(if beside { 16. } else { 8. })
            .child(mono(
                i18n::tr(if beside { "01 / OUTGOING     THE DISTANCE, REMOVED." } else { "01 / OUTGOING" }),
                accent,
                13.,
            ))
            .child(headline("Make files", "fly.", palette, accent, self.form))
            .child(
                label().text(crate::i18n::tr("One action. One destination. Move files between your devices without a labyrinth of menus."))
                    .color(palette.muted)
                    .font_size(if beside { 20. } else { 15. }),
            );

        // What the circle shows and does is ONE decision (`view::circle_view`), made from everything that bears on it at once: the files listed, where
        // they would go, the code that was made and what the owner reports of it, and the transfers of the batch chosen now. The page reads the answer and
        // chooses nothing of its own: not the words, not whether a touch does anything, not what it does.
        let code_state = shown.read().as_ref().map(|made| {
            if expired.read().is_some_and(|generation| generation == made.generation) {
                view::CodeCircle::Gone
            } else {
                view::code_circle_selection(made.generation, self.view.files_code, &self.view.transfers, made.expected as usize)
            }
        });
        let batch_now = view::batch_of(&self.view.transfers, &batch_ids.read(), shown.read().as_ref().map(|made| made.generation));
        let batch_transfers = &batch_now;
        let listed_count = if staged_names.read().is_empty() { staged_paths.read().len() } else { staged_names.read().len() };
        let circle = view::circle_view(&view::CircleInputs {
            listed: listed_count,
            aim: aim.clone(),
            send_instantly: self.view.settings.send_instantly,
            code: code_state.clone(),
            batch: &batch_transfers,
            handed_over: *handed_over.read(),
        });
        let picked = match &aim {
            view::Aim::Devices(devices) => devices.len(),
            _ => 0,
        };
        // Read through the cache that knows what the drawing was made for, so that a window turned or resized after
        // the code was made gets one drawn for its new size; a memo that captured the size never saw it change.
        let drawn = use_state(|| std::rc::Rc::new(std::cell::RefCell::new(qr::Drawn::default())));
        let scale = *Platform::get().current_window().scale_factor.peek();
        let disc = qr::disc_physical(view::Form::portal_extent(qr::PORTAL_OUTER, self.room), scale);
        // The picture is drawn only while the circle says it is the code, and never from a code that was spent.
        let picture = match (circle.shows_code, shown.read().as_ref()) {
            (true, Some(made)) => drawn.read().borrow_mut().of(&made.code, disc).map(|handle| qr::picture_from(handle, scale)),
            _ => None,
        };
        // While the files wait for the other device, the time this device still waits for the answer, from the window's clock.
        let circle_clock = code_state
            .as_ref()
            .zip(shown.read().as_ref())
            .and_then(|(code, made)| view::circle_time_in(i18n::language(), code, made.generation, &self.view.transfers, self.now));
        // How long the result of an ended batch has been on the circle, from the window's own clock: the page notes the first second it saw the batch
        // as ended (the owner keeps no end time of a transfer), for the lap it belongs to, and the model says how clear it still is (see
        // `view::result_opacity`). A newer lap (files chosen, or a send) leaves an older result behind at once.
        let ended_now = matches!(view::stage_of(&view::CircleInputs {
            listed: listed_count, aim: aim.clone(), send_instantly: self.view.settings.send_instantly, code: code_state.clone(),
            batch: &batch_transfers, handed_over: *handed_over.read(),
        }), view::Stage::Ended { .. });
        {
            let noted = *result_noted.peek();
            match (ended_now, noted) {
                (true, None) => result_noted.set(Some((*lap.peek(), self.now))),
                (false, Some(_)) => result_noted.set(None),
                _ => {}
            }
        }
        let (result_shown, clearness) = match *result_noted.read() {
            Some((noted_lap, ended_at)) => {
                let ended_ms = ended_at.saturating_mul(1_000);
                let now_ms = self.now.saturating_mul(1_000);
                (view::shows_result(noted_lap, *lap.read(), ended_ms, now_ms), view::result_opacity(ended_ms, now_ms))
            }
            None => (true, 1.),
        };
        let words = view::circle_view_words_in(i18n::language(), &circle, code_state.as_ref(), picked, result_shown);
        // The words of a result fade toward the colour of the circle, in the clearness the model gives; every other state is drawn clear.
        let ink = if ended_now && result_shown { glyph::dim(palette.portal_text, palette.portal, clearness) } else { palette.portal_text };
        let centre: Vec<Element> = match picture {
            Some(picture) => vec![picture],
            None if words.asks_for_files => vec![
                mono(words.kicker.clone(), palette.portal_text, 12.).into(),
                label().text(words.headline.clone()).color(palette.portal_text).font_size(34.).font_weight(FontWeight::BOLD).into(),
                label().text(words.line.clone()).color(palette.portal_text).font_size(34.).font_weight(FontWeight::BOLD).into(),
                label().text(i18n::tr(if unavailable { hint(&state) } else { self.form.circle_invitation() })).color(palette.portal_text).font_size(14.).into(),
            ],
            None => [
                mono(words.kicker.clone(), ink, 12.).into(),
                label().text(words.headline.clone()).color(ink).font_size(34.).font_weight(FontWeight::BOLD).into(),
                label().text(words.line.clone()).color(ink).font_size(14.).into(),
            ]
            .into_iter()
            .chain(circle_clock.map(|text| {
                let colour = if text.ends_with("0:00") { palette.bad } else { palette.portal_text };
                mono(text, colour, 12.).into()
            }))
            .collect(),
        };
        // The sparks and the fuse of the code on show: made here, on every draw and in the same order as every other hook of this page. The fuse is a
        // smooth line from what the code has left when it is first seen to nothing at the owner's deadline (see `use_fuse`), the same as on the Pair page,
        // and not a number read from the clock once a second: the sparks fly from its tip, and a tip that moves a step a second made them shake.
        let reduced = self.link.client().snapshot().settings.reduced_motion;
        let code_sparks = use_sparks(reduced, circle.shows_code);
        let on_show = shown.read().as_ref().filter(|_| circle.shows_code).map(|made| (made.generation, made.made_at, made.ends_at));
        let (burns, left, remaining) = match on_show {
            Some((generation, made_at, ends_at)) => (
                Some(generation),
                view::code_fuse(made_at, ends_at, self.now),
                ends_at.saturating_sub(self.now),
            ),
            None => (None, 0., 0),
        };
        let code_fuse = use_fuse(burns, left, remaining, reduced);
        // A circle that is shut (a code drawn, or a transfer going) answers no touch at all; one that is open does what the decision says.
        let disc = portal_held(
            palette,
            accent,
            (!unavailable && circle.does.is_some()).then(|| choose.clone()),
            centre,
            view::Form::portal_extent(qr::PORTAL_OUTER, self.room),
            circle.does.filter(|_| !unavailable).map(|does| HoldToSend { does, on_act: on_circle.clone() }),
            // How far the files have come while they go: the circle fills as they move, as it fills under a finger, in the same colour and way.
            view::circle_fill(&circle.batch, 0.),
            // The time the code has left, as the fuse that burns round it: smooth, from the owner's own deadline (`use_fuse`), and only while the code
            // is what the circle shows. Nothing is written in the code or on the disc, so what is scanned is what was drawn.
            burns.map(|_| code_fuse),
            // The sparks of its tip, the same as the fuse of a code on the Pair page (`use_sparks`): in flight only while a code burns.
            code_sparks,
        );

        // The files that were chosen and are only listed, as the circle's own context: no button of its own, no history. The circle is the only
        // control: with no device picked a touch on it makes a code; with devices picked it is held until it fills and sends to them.
        // The list that belongs to the circle, from the moment files are chosen: a row for each, with where it stands (prepared, going, done,
        // failed). One list, for the files that are listed and for the transfers made of them; never the history, which is the Transfers tab's.
        let listed_names: Vec<String> = if staged_names.read().is_empty() {
            staged_paths
                .read()
                .iter()
                .filter_map(|path| path.file_name().map(|name| name.to_string_lossy().into_owned()))
                .collect()
        } else {
            staged_names.read().clone()
        };
        let files_list = {
            // The batch that is chosen now, and nothing before it: the transfers that were there when the files were chosen (sent, cancelled,
            // failed) are the Transfers tab's, and what is still going stays until it ends (see `view::of_this_run`).
            let batch = batch_now.clone();
            let prepared: Vec<String> = view::list_under_circle(&listed_names, &batch_now)
                .into_iter()
                .filter_map(|row| match row {
                    view::ListRow::Prepared(name) => Some(name),
                    view::ListRow::Transfer(_) => None,
                })
                .collect();
            Transfers { context: true, items: batch, prepared, actions: Some(file_actions.clone()), ..transfers(&self.link, &self.view, palette) }
        };
        let mut page = rect().width(Size::fill()).spacing(self.form.page_section_gap());
        if self.form.send_is_the_circle_alone() {
            // A phone held upright: the title small above, then the circle, which is itself the button that chooses.
            // The saved devices are under it, and the pairing is in Peers, a tap away at the bottom.
            page = page.cross_align(Alignment::center()).child(title_above_circle(self.form, hero, disc));
            if let Some(said) = *notice.read() {
                page = page.child(label().text(said.words()).color(palette.muted).font_size(14.));
            }
            // What is going on or has gone is the circle's own business, so it comes right under it, before the devices: where a transfer
            // can be followed and stopped without scrolling past a list of devices.
            page = page.child(files_list);
            // The devices whose pairing was finished, under that: to send to one of them is a tap, and they are the only ones here. A
            // device only seen in the history is not, until it has been paired.
            page = page.child(section("OR PICK A SAVED DEVICE", palette)).child(devices);
        } else {
            page = page.child(
                rect().width(Size::fill()).horizontal().cross_align(Alignment::center()).content(Content::flex()).child(hero).child(disc),
            );
            // What the last batch left out, in a sentence: a folder dropped in, a file that is gone.
            if let Some(said) = *notice.read() {
                page = page.child(label().text(said.words()).color(palette.muted).font_size(14.));
            }
            page = page
                .child(files_list)
                .child(section("02 / OR PICK A SAVED DEVICE", palette))
                .child(devices);
        }
        // Dropping files anywhere on the page, and pasting with Ctrl+V (or Command+V), are two more
        // ways in, and end in the same `take` as the dialog.
        let on_drop = take.clone();
        let on_paste = take;
        let paste_link = self.link.clone();
        frame(
            palette,
            self.form,
            rect()
                .width(Size::fill())
                .on_file_drop(move |e: Event<FileEventData>| on_drop.call(e.file_paths.clone()))
                .on_global_key_down(move |e: Event<KeyboardEventData>| {
                    if intake::is_paste(e.code, e.modifiers) {
                        let on_paste = on_paste.clone();
                        // Reading the clipboard may block while its owner answers, so it is done on
                        // the program's own executor (the window's loop is not a tokio runtime) and
                        // the window keeps drawing while it waits.
                        let reading = paste_link.blocking(clipboard_files::read);
                        spawn(async move {
                            if let Ok(found) = reading.await
                                && !found.is_empty()
                            {
                                on_paste.call(found);
                            }
                        });
                    }
                })
                .child(page),
        )
    }
}

#[derive(PartialEq)]
struct DeviceCard {
    id: String,
    name: String,
    picture: Option<avatar_view::Shared>,
    online: bool,
    picked: bool,
    palette: Palette,
    form: view::Form,
    on_choose: EventHandler<String>,
}

impl Component for DeviceCard {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let mut hovered = use_state(|| false);
        let (online, id, on_choose) = (self.online, self.id.clone(), self.on_choose.clone());
        rect()
            .key(self.id.clone())
            .width(self.form.device_card_width().map_or(Size::fill(), Size::px))
            .padding(16.)
            .horizontal()
            .cross_align(Alignment::center())
            .spacing(14.)
            .corner_radius(14.)
            .opacity(if online { 1. } else { 0.55 })
            .background(if hovered() && online { palette.raised } else { palette.surface })
            .border(
                Border::new()
                    .fill(if self.picked { palette.send } else { palette.border })
                    .width(if self.picked { 2. } else { 1. })
                    .alignment(BorderAlignment::Inner),
            )
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set(false))
            .on_press(move |_| {
                if online {
                    on_choose.call(id.clone());
                }
            })
            .child(avatar_view::Avatar {
                picture: self.picture.as_ref().map(|shared| shared.0.clone()),
                name: self.name.clone(),
                size: 40.,
                palette,
            })
            .child(
                rect()
                    .child(label().text(self.name.clone()).color(palette.text).font_size(17.).font_weight(FontWeight::BOLD))
                    .child(mono(if online { "ONLINE" } else { "OFFLINE" }, if online { palette.good } else { palette.muted }, 11.)),
            )
    }
}

// ---- Receive ---------------------------------------------------------------------------

#[derive(PartialEq)]
struct ReceivePage {
    link: Link,
    view: View,
    palette: Palette,
    form: view::Form,
    /// The width the window has, in pixels, from which the portal's size is decided.
    room: f32,
    /// The platform's camera, for reading the code of another device; absent where there is none.
    camera: Option<link::CameraControl>,
}

/// Everything that reading another device's code with the camera needs, kept in one place so that Receive and Peers do
/// the same thing the same way: whether the camera is on, the picture of what it sees, and how to start and stop it.
///
/// A code that is read is joined once and the camera is closed; a refusal is said in the circle. The camera is closed when
/// the component that uses this is left, so that it does not stay on with nobody to read it.
struct CodeScan {
    looking: State<view::Looking>,
    picture: State<Option<Arc<preview::Shown>>>,
    open: EventHandler<()>,
    stop: EventHandler<()>,
}

fn use_code_scan(link: &Link, camera: Option<link::CameraControl>, room: f32, owner_said: State<Option<String>>) -> CodeScan {
    let mut looking: State<view::Looking> = use_state(view::Looking::default);
    // What the camera sees, as the circle shows it: the latest picture, empty until the first frame and again after.
    let mut picture: State<Option<Arc<preview::Shown>>> = use_state(|| None);
    // The scan in progress, kept where Stop can reach it: stopping ends it from here, whatever the watcher is doing.
    let scan: std::rc::Rc<std::cell::RefCell<Option<scanner::Scanner>>> = use_hook(Default::default);
    // The disc's size on the glass in pixels, which is the side of the picture made for it: a picture larger than the disc
    // is wasted work and a smaller one is stretched.
    let side = qr::disc_physical(
        view::Form::portal_extent(qr::PORTAL_OUTER, room),
        *Platform::get().current_window().scale_factor.peek(),
    )
    .round() as u32;
    let open = EventHandler::new({
        let (camera, link, scan) = (camera.clone(), link.clone(), scan.clone());
        move |()| {
            let Some(camera) = camera.clone() else { return };
            if looking() == view::Looking::Scanning {
                return;
            }
            let scanner = scanner::Scanner::new();
            *scan.borrow_mut() = Some(scanner.clone());
            (camera.open)(scanner.clone());
            looking.set(view::Looking::Scanning);
            picture.set(None);
            let link = link.clone();
            // Two minutes is far more than reading a code takes, and it ends a camera that nobody is pointing.
            let (mut ticks, mut shown) = link::await_scan_shown(
                scanner,
                camera,
                link::LOOK_EVERY,
                link::CAMERA_PATIENCE,
                side,
            );
            spawn(async move {
                while let Some(tick) = ticks.recv().await {
                    looking.set(tick.looking);
                    if let Some(invitation) = tick.join {
                        // The owner's answer is waited for off this loop, so that the next tick of the camera is not held up by it.
                        let (joined, mut said) = (link.join(invitation), owner_said);
                        said.set(None);
                        spawn(async move {
                            if let Err(reason) = joined.await {
                                said.set(Some(reason));
                            }
                        });
                    }
                    if tick.close {
                        picture.set(None);
                        break;
                    }
                }
            });
            // The pictures are waited on without a timer: a `watch` wakes its reader when the value is replaced, which
            // needs no clock, and the reader stops when the scan has ended and the sender is gone.
            spawn(async move {
                while shown.changed().await.is_ok() {
                    let latest = shown.borrow_and_update().clone();
                    if let Some(latest) = latest {
                        picture.set(Some(latest));
                    }
                }
            });
        }
    });
    // Stop: the person ends the scan, and the camera is closed with nothing joined. The watcher's own end is the same
    // thing from the other side; the scanner is stopped here as well, so that what a frame in flight finds is dropped.
    let stop = EventHandler::new({
        let (camera, scan) = (camera.clone(), scan.clone());
        move |()| {
            let tick = view::stop(looking());
            if let Some(scanner) = scan.borrow_mut().take() {
                scanner.stop();
            }
            if tick.close
                && let Some(camera) = camera.clone()
            {
                (camera.close)();
            }
            looking.set(tick.looking);
            picture.set(None);
        }
    });
    // The page is left with the camera open: it is closed, so that it does not stay on with nobody to read it.
    if let Some(camera) = camera {
        let scan = scan.clone();
        use_drop(move || {
            if let Some(scanner) = scan.borrow_mut().take() {
                scanner.stop();
            }
            (camera.close)();
        });
    }
    CodeScan { looking, picture, open, stop }
}

/// What the circle of Receive holds while the camera is on: the picture of what it sees, filling the disc, a dot on each
/// corner of every code the camera has found, and a button to stop.
///
/// The dots are what tells the person how to aim. A code that is found but not yet read (too far, too near, at a slant)
/// is marked all the same, so that they can see it is seen and move until it reads; no code marked means none is found,
/// and that is to move until one is. The picture is the camera's own, turned upright, and only its centre is shown, which
/// is also where a code must be to be marked: a code half out of the circle is not marked, since a mark with a corner
/// missing would say it is where it is not.
#[derive(PartialEq)]
struct CameraCircle {
    picture: Arc<preview::Shown>,
    /// The width the window has, in pixels, from which the disc's size is decided, as for the portal.
    room: f32,
    palette: Palette,
    on_stop: EventHandler<()>,
}

impl Component for CameraCircle {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let extent = view::Form::portal_extent(qr::PORTAL_OUTER, self.room);
        let disc = qr::PORTAL_DISC * extent / qr::PORTAL_OUTER;
        // Made on every draw and not kept: a handle kept by a memo that captured the first picture would go on showing it,
        // which is the mistake `qr::Drawn` was written against. It costs one copy of a picture about as large as the disc,
        // a few times a second, which is what the picture is.
        let handle = qr::portal_handle(&self.picture.picture);
        let mut camera = rect().width(Size::px(disc)).height(Size::px(disc)).corner_radius(disc / 2.);
        if let Some(handle) = handle {
            camera = camera.child(
                image(handle).sampling_mode(SamplingMode::Nearest).width(Size::px(disc)).height(Size::px(disc)),
            );
        }
        // A dot on every corner of every code that was found, in the green of "good".
        const DOT: f32 = 12.;
        for code in &self.picture.codes {
            for &corner in code {
                let (left, top) = preview::dot_origin(corner, self.picture.picture.side, disc, DOT);
                camera = camera.child(
                    rect()
                        .position(Position::new_absolute().left(left).top(top))
                        .width(Size::px(DOT))
                        .height(Size::px(DOT))
                        .corner_radius(DOT / 2.)
                        .background(palette.good)
                        .border(Border::new().fill(palette.scanner_outline).width(2.).alignment(BorderAlignment::Inner)),
                );
            }
        }
        let on_stop = self.on_stop.clone();
        // Stop sits at the bottom of the circle, over the picture, where a thumb finds it.
        camera.child(
            rect()
                .position(Position::new_absolute().left(disc / 2. - 44.).top(disc - 64.))
                .child(Button::new().on_press(move |_| on_stop.call(())).child(crate::i18n::tr("Stop"))),
        )
    }
}

impl Component for ReceivePage {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let accent = palette.receive;
        let form = self.form;
        let mut join_text = use_state(String::new);
        // What the owner said when it would not take the code up: shown under the field, and gone with the next try.
        let mut owner_said: State<Option<String>> = use_state(|| None);
        let join_link = self.link.clone();
        let join = move |_| {
            // Only what plainly is an invitation goes to the owner, in the form it reads. If it
            // is not one, the field is left as it is, so that the person can see and mend it.
            let code = qr::invitation_to_join(&join_text.read());
            if let Some(code) = code {
                owner_said.set(None);
                join_text.set(String::new());
                let joined = join_link.join(code);
                spawn(async move {
                    if let Err(reason) = joined.await {
                        owner_said.set(Some(reason));
                    }
                });
            }
        };

        // The circle makes no code: a device that receives has nothing to show. It is the place where a code from the
        // other device is taken: pasted below, or, on a phone, read by its camera. Pairing is in Peers.
        let camera = self.camera.clone();
        let CodeScan { looking, picture, open: open_camera, stop: stop_camera } =
            use_code_scan(&self.link, camera.clone(), self.room, owner_said);
        let shown_now = picture.read().clone();
        let words = |headline: &'static str, line: &'static str| -> Vec<Element> {
            vec![
                mono(i18n::tr("READY TO RECEIVE"), palette.portal_text, 12.).into(),
                label().text(i18n::tr(headline)).color(palette.portal_text).font_size(34.).font_weight(FontWeight::BOLD).into(),
                label().text(i18n::tr(line)).color(palette.portal_text).font_size(14.).into(),
            ]
        };
        let mut camera_in_circle = false;
        let centre: Vec<Element> = if camera.is_some() {
            match view::circle_for(looking(), shown_now.is_some()) {
                view::Circle::Words(headline, line) => words(headline, line),
                // One small line and nothing else, for the moment the camera takes to give its first frame.
                view::Circle::Starting(line) => vec![label().text(i18n::tr(line)).color(palette.portal_text).font_size(14.).into()],
                // The camera, and nothing written over it.
                view::Circle::Camera => match shown_now {
                    Some(picture) => {
                        camera_in_circle = true;
                        vec![CameraCircle { picture, room: self.room, palette, on_stop: stop_camera.clone() }.into()]
                    }
                    None => Vec::new(),
                },
            }
        } else {
            vec![
                mono(i18n::tr("READY TO RECEIVE"), palette.portal_text, 12.).into(),
                label().text(i18n::tr("PASTE")).color(palette.portal_text).font_size(34.).font_weight(FontWeight::BOLD).into(),
                label().text(i18n::tr("THE CODE")).color(palette.portal_text).font_size(34.).font_weight(FontWeight::BOLD).into(),
                label().text(i18n::tr("from the other device")).color(palette.portal_text).font_size(14.).into(),
            ]
        };

        // The title and its two lines are kept on every screen, at a size that fits the one they are on: a
        // phone's is smaller, so that they do not take the room the portal needs.
        let hero = rect()
            .width(if form.page_puts_portal_beside_text() { Size::flex(1.) } else { Size::fill() })
            .spacing(if form.page_puts_portal_beside_text() { 16. } else { 8. })
            .child(mono(
                i18n::tr(if form.page_puts_portal_beside_text() { "02 / INCOMING     THE DISTANCE, REMOVED." } else { "02 / INCOMING" }),
                accent,
                13.,
            ))
            .child(headline("Ready when", "you are.", palette, accent, form))
            .child(
                label().text(crate::i18n::tr("Choose what you accept. Keep everything on your terms. Nothing arrives until you say so."))
                    .color(palette.muted)
                    .font_size(if form.page_puts_portal_beside_text() { 20. } else { 15. }),
            );

        let paste = Input::new(join_text).placeholder(crate::i18n::tr("Paste the code")).width(if form.page_puts_portal_beside_text() {
            Size::px(520.)
        } else {
            Size::fill()
        });
        let join_button = Button::new().on_press(join).child(crate::i18n::tr("Join"));
        let join_row: Element = if form.page_puts_portal_beside_text() {
            rect().horizontal().spacing(10.).child(paste).child(join_button).into()
        } else {
            rect().width(Size::fill()).spacing(10.).child(paste).child(join_button.width(Size::fill())).into()
        };
        // With the camera in the circle a press on it does nothing: Stop is the one way to end a scan, and a touch that
        // restarted the camera would be a button that does something other than what it is.
        let on_press = (camera.is_some() && !camera_in_circle).then(|| open_camera.clone());
        let portal = portal(palette, accent, on_press, centre, view::Form::portal_extent(qr::PORTAL_OUTER, self.room));
        let introduction: Element = if form.page_puts_portal_beside_text() {
            rect()
                .width(Size::fill())
                .horizontal()
                .cross_align(Alignment::center())
                .content(Content::flex())
                .child(hero)
                .child(portal)
                .into()
        } else {
            // A phone: the title small and short above the portal, which is centred under it, as on Send.
            title_above_circle(form, hero, portal).into()
        };
        let mut sections = rect()
            .width(Size::fill())
            .spacing(self.form.page_section_gap())
            .child(introduction)
            .child(
                rect()
                    .width(Size::fill())
                    .spacing(10.)
                    .child(section("HAVE A CODE FROM ANOTHER DEVICE?", palette))
                    .child(join_row)
                    .maybe_child(owner_said.read().clone().map(|reason| label().text(i18n::tr(&reason).to_owned()).color(palette.bad).font_size(14.))),
            );
        // Pasting a code belongs to the Receive circle, before the bounded list of incoming files.
        if !self.view.transfers.is_empty() {
            sections = sections.child(transfers_here(&self.link, &self.view, palette));
        }
        frame(palette, self.form, sections)
    }
}

/// Where a device is paired, at the top of Peers: the button that asks for a code, the line that says nothing
/// moves before an approval, and the code itself once it exists, to paste on the other device. Pairing is the
/// way a device becomes trusted for good, and it lives here, with the devices it adds to.
#[derive(PartialEq)]
struct PairingSection {
    link: Link,
    palette: Palette,
    /// The width the window has, in pixels, from which the size of the portal with the code is decided.
    room: f32,
    form: view::Form,
    /// The platform's camera, for reading the code of the other device; absent where there is none.
    camera: Option<link::CameraControl>,
}

/// The side of pairing that TAKES a code instead of showing one: the code of the other device, read by the camera where
/// there is one (the circle) and pasted where there is not, and joined only if it plainly is an invitation. The same
/// way Receive takes a code, through the same hook, so what a scan does is decided in one place.
#[derive(PartialEq)]
struct CodeTaker {
    link: Link,
    palette: Palette,
    accent: (u8, u8, u8),
    form: view::Form,
    room: f32,
    camera: Option<link::CameraControl>,
}

impl Component for CodeTaker {
    fn render(&self) -> impl IntoElement {
        let (palette, accent, form) = (self.palette, self.accent, self.form);
        let mut text = use_state(String::new);
        let mut refused = use_state(|| false);
        // What the owner said when it would not take the code up: shown under the field, and gone with the next try.
        let mut owner_said: State<Option<String>> = use_state(|| None);
        let join_link = self.link.clone();
        let join = move |_| {
            // Only what plainly is an invitation goes to the owner, in the form it reads. If it is not one, the field is
            // left as it is, so that the person can see and mend it, and is told so.
            let code = qr::invitation_to_join(&text.read());
            match code {
                Some(code) => {
                    refused.set(false);
                    owner_said.set(None);
                    text.set(String::new());
                    let joined = join_link.join(code);
                    spawn(async move {
                        if let Err(reason) = joined.await {
                            owner_said.set(Some(reason));
                        }
                    });
                }
                None => refused.set(true),
            }
        };
        let CodeScan { looking, picture, open, stop } = use_code_scan(&self.link, self.camera.clone(), self.room, owner_said);
        let shown_now = picture.read().clone();
        let words = |headline: &'static str, line: &'static str| -> Vec<Element> {
            vec![
                mono(i18n::tr("CONNECT"), palette.portal_text, 12.).into(),
                label().text(i18n::tr(headline)).color(palette.portal_text).font_size(34.).font_weight(FontWeight::BOLD).into(),
                label().text(i18n::tr(line)).color(palette.portal_text).font_size(14.).into(),
            ]
        };
        let mut camera_in_circle = false;
        let centre: Vec<Element> = match view::circle_for(looking(), shown_now.is_some()) {
            view::Circle::Words(headline, line) => words(headline, line),
            view::Circle::Starting(line) => vec![label().text(i18n::tr(line)).color(palette.portal_text).font_size(14.).into()],
            view::Circle::Camera => match shown_now {
                Some(picture) => {
                    camera_in_circle = true;
                    vec![CameraCircle { picture, room: self.room, palette, on_stop: stop.clone() }.into()]
                }
                None => Vec::new(),
            },
        };
        let paste = Input::new(text).placeholder(crate::i18n::tr("Paste the code")).width(if form.page_puts_portal_beside_text() {
            Size::px(520.)
        } else {
            Size::fill()
        });
        let join_button = Button::new().on_press(join).child(crate::i18n::tr("Join"));
        let join_row: Element = if form.page_puts_portal_beside_text() {
            rect().horizontal().spacing(10.).child(paste).child(join_button).into()
        } else {
            rect().width(Size::fill()).spacing(10.).child(paste).child(join_button.width(Size::fill())).into()
        };
        let mut taker = rect().width(Size::fill()).spacing(12.);
        // The circle is only where there is a camera to read with; elsewhere the code is pasted.
        if self.camera.is_some() {
            let on_press = (!camera_in_circle).then(|| open.clone());
            taker = taker.child(
                rect().width(Size::fill()).cross_align(Alignment::center()).child(portal(
                    palette,
                    accent,
                    on_press,
                    centre,
                    view::Form::portal_extent(qr::PORTAL_OUTER, self.room),
                )),
            );
        }
        taker = taker.child(section("OR PASTE THE CODE FROM THE OTHER DEVICE", palette)).child(join_row);
        if *refused.read() {
            taker = taker.child(label().text(crate::i18n::tr("That is not a code from another device.")).color(palette.muted).font_size(14.));
        }
        if let Some(reason) = owner_said.read().clone() {
            taker = taker.child(label().text(i18n::tr(&reason).to_owned()).color(palette.bad).font_size(14.));
        }
        taker
    }
}

impl Component for PairingSection {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let accent = palette.send;
        // The code is only ever in the reply to the command, never in the snapshot, so it is awaited here.
        let mut invitation: State<Option<String>> = use_state(|| None);
        let mut failure: State<Option<String>> = use_state(|| None);
        // Which side is open: this device's code, the other device's code, or neither. One state, so that the two are never open
        // together.
        let mut mode: State<view::PairMode> = use_state(view::PairMode::default);
        let client = self.link.client().clone();
        // A code lives five minutes in the owner, which refuses it itself at its deadline (`Admission::new`). The window makes no new one by itself: a
        // code that nobody asked for would be a pairing left open that nobody is looking at. When its life is over it goes off the screen, and Pair is
        // the button that asks for another (`PairMode::pair_asks_for_a_code` is true again, with no code on show). `generation` tells the timer of a
        // code that was replaced since (by Pair again, or by another device or the command line) not to take the screen of the one that replaced it.
        let generation: State<u64> = use_state(|| 0);
        // Whether a code has been asked for and has not come yet.
        let mut asking: State<bool> = use_state(|| false);
        let create = EventHandler::new({
            let client = client.clone();
            move |()| {
                let client = client.clone();
                let mut generation = generation;
                let mine = *generation.peek() + 1;
                generation.set(mine);
                asking.set(true);
                spawn(async move {
                    let answer = client.command(Command::CreateInvitation {}).await;
                    // However it was answered, the code is no longer on its way.
                    asking.set(false);
                    match answer {
                        Ok(Reply::Invitation(code)) => {
                            failure.set(None);
                            invitation.set(Some(code));
                            // At the end of its life: the code leaves the screen, if it is still the code on show. Nothing is asked for in its place.
                            let mut over = link::after(std::time::Duration::from_secs(view::CODE_LIFE_SECONDS), std::time::Duration::from_millis(250));
                            spawn(async move {
                                if (&mut over).await.is_ok() && *generation.peek() == mine {
                                    invitation.set(None);
                                }
                            });
                        }
                        // Never swallowed: an error nobody can see is a button that seems to do nothing.
                        Ok(_) => failure.set(Some("The owner answered with something else.".to_owned())),
                        Err(error) => failure.set(Some(error.to_string())),
                    }
                });
            }
        });
        // Pair shows this device's code and takes the other side's off the screen, then asks for the code: but only when there is none on show. A
        // press on it while a code is showing changes nothing (see `PairMode::pair_asks_for_a_code`); when the code is gone, it asks again.
        let create = EventHandler::new({
            let create = create.clone();
            move |()| {
                // A code that has been asked for and has not come yet counts as one on its way: a second press before the owner answers asks for nothing.
                let on_its_way = invitation.peek().is_some() || *asking.peek();
                let asks = mode.peek().pair_asks_for_a_code(on_its_way);
                let next = mode.peek().after_pair();
                mode.set(next);
                if asks {
                    create.call(());
                }
            }
        });
        // The fuse: from whole to nothing over the whole life of the code, in a line, begun again for each code (`generation` is what a new code changes),
        // so that it is gone at the moment the code leaves the screen and the owner stops admitting it. With reduced motion it does not move: it is
        // drawn whole, and the code still leaves the screen on its time.
        let reduced = self.link.client().snapshot().settings.reduced_motion;
        let fuse = use_animation_with_dependencies(&(*generation.read(), reduced), move |conf, (_, reduced)| {
            conf.on_creation(OnCreation::Run);
            conf.on_change(OnChange::Rerun);
            AnimNum::new(1., 0.).time(if *reduced { 0 } else { view::CODE_LIFE_SECONDS * 1000 }).function(Function::Linear)
        });
        // The sparks of its tip (see `use_sparks`): still with reduced motion.
        let sparks = use_sparks(reduced, true);
        // The drawing is kept between draws, together with what it was made for. It is read through the cache and
        // not through a memo that captures a size: a memo does not see the size change when the window is turned.
        let drawn = use_state(|| std::rc::Rc::new(std::cell::RefCell::new(qr::Drawn::default())));
        let disc = qr::disc_physical(
            view::Form::portal_extent(qr::PORTAL_OUTER, self.room),
            *Platform::get().current_window().scale_factor.peek(),
        );
        let shown = mode.read().shows_code().then(|| invitation.read().clone()).flatten();
        let picture = shown.as_deref().and_then(|code| {
            drawn.read().borrow_mut().of(code, disc).map(|handle| {
                qr::picture_from(handle, *Platform::get().current_window().scale_factor.peek())
            })
        });
        let mut section_box = rect()
            .width(Size::fill())
            .spacing(12.)
            .child(
                // Side by side: the words are short enough for both to keep them on one line, even on a phone.
                rect()
                    .horizontal()
                    .spacing(12.)
                    .child(Solid { text: "Pair", accent, palette, enabled: true, on_press: create })
                    .child(Solid {
                        text: "Connect",
                        accent: palette.receive,
                        palette,
                        enabled: true,
                        on_press: EventHandler::new(move |()| {
                            let next = mode.peek().after_connect();
                            mode.set(next);
                        }),
                    }),
            )
            .child(mono(i18n::tr(view::PAIR_EXPLAINS), palette.muted, 12.))
            .child(mono(i18n::tr(view::PAIR_WARNS), palette.muted, 12.));
        if let Some(said) = failure.read().clone() {
            section_box = section_box.child(label().text(i18n::tr(&said).to_owned()).color(palette.muted).font_size(14.));
        }
        if let (true, Some(picture)) = (mode.read().shows_code(), picture) {
            // The code as a picture in the portal, which is how a code this long is carried to another device. The ring round it is a fuse that
            // burns down over the whole life of the code; when it is gone the code leaves the screen, and Pair is pressed for another.
            let extent = view::Form::portal_extent(qr::PORTAL_OUTER, self.room);
            // With reduced motion nothing moves: the fuse is shown whole (an animation of no length would jump to its end and show it burnt) and the
            // sparks stand at one phase (`use_sparks` gives that frame itself); the code still leaves the screen on its time.
            let life: f32 = if reduced { 1. } else { fuse.get().value() };
            let frame = sparks;
            section_box = section_box.child(
                rect()
                    .width(Size::fill())
                    .cross_align(Alignment::center())
                    .child(portal_with_life(palette, accent, vec![picture], extent, life, frame)),
            );
        }
        if let Some(code) = shown {
            // The code to paste on the other device, as text that can be selected.
            section_box = section_box
                .child(section("OR PASTE THIS CODE ON THE OTHER DEVICE", palette))
                .child(
                    rect()
                        .width(Size::fill())
                        .padding(12.)
                        .corner_radius(10.)
                        .background(palette.surface)
                        .border(Border::new().fill(palette.border).width(1.).alignment(BorderAlignment::Inner))
                        .child(
                            SelectableText::new()
                                .span(Span::new(code).color(palette.text).font_size(11.).font_family("monospace"))
                                .width(Size::fill()),
                        ),
                );
        }
        // The other side of pairing: the code of the device that is pairing with this one, scanned or pasted. Without it a
        // device that was shown a code has no way to answer it from here.
        if mode.read().takes_code() {
            section_box = section_box.child(section("CONNECT TO THE OTHER DEVICE", palette)).child(CodeTaker {
                link: self.link.clone(),
                palette,
                accent: palette.receive,
                form: self.form,
                room: self.room,
                camera: self.camera.clone(),
            });
        }
        section_box
    }
}

#[derive(PartialEq)]
struct ApprovalCard {
    link: Link,
    approval: Approval,
    palette: Palette,
}

impl Component for ApprovalCard {
    fn render(&self) -> impl IntoElement {
        let (approval, palette) = (&self.approval, self.palette);
        let meta = format!(
            "{} · {}s",
            i18n::tr(if approval.answered {
                "ANSWERED, WAITING FOR THE OTHER DEVICE"
            } else {
                "PICK THE NUMBER THE OTHER DEVICE SHOWS"
            }),
            approval.seconds_left
        );
        let mut card = rect()
            .key(approval.request.clone())
            .width(Size::fill())
            .padding(20.)
            .spacing(12.)
            .corner_radius(14.)
            .background(palette.surface)
            .border(Border::new().fill(palette.receive).width(1.).alignment(BorderAlignment::Inner))
            .child(label().text(i18n::format(i18n::language(), "{name} wants to pair", &[("name", &approval.name)])).color(palette.text).font_size(20.).font_weight(FontWeight::BOLD))
            .child(mono(meta, palette.muted, 11.));
        if !approval.answered {
            let mut choices = rect().horizontal().spacing(10.);
            for number in approval.options {
                let link = self.link.clone();
                let request = approval.request.clone();
                choices = choices.child(
                    Button::new()
                        .filled()
                        .on_press(move |_| link.send(Command::ChoosePairing { request: request.clone(), number }))
                        .child(label().text(number.to_string()).font_size(24.).font_weight(FontWeight::BOLD)),
                );
            }
            let link = self.link.clone();
            let request = approval.request.clone();
            choices = choices.child(
                Button::new()
                    .on_press(move |_| link.send(Command::RejectPairing { request: request.clone() })).child(crate::i18n::tr("Refuse")),
            );
            card = card.child(choices);
        }
        card
    }
}

/// The heading of a list of transfers, with Clear at its right end: it forgets the transfers that are over and keeps the ones
/// still going. It asks twice, because the record is gone for good, and is not there at all when nothing is over.
#[derive(PartialEq)]
struct TransfersHeader {
    link: Link,
    palette: Palette,
    can_clear: bool,
}

impl Component for TransfersHeader {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let mut asking = use_state(view::Confirm::default);
        let mut row = rect().width(Size::fill()).horizontal().cross_align(Alignment::center()).content(Content::flex());
        row = row.child(section("TRANSFERS", palette)).child(rect().width(Size::flex(1.)));
        // Nothing over, nothing to clear: a button that does nothing is not shown, and a question left open is dropped with it.
        if !self.can_clear {
            if *asking.peek() != view::Confirm::Idle {
                asking.set(view::Confirm::Idle);
            }
            return row.into_element();
        }
        let confirming = *asking.read() == view::Confirm::Asking;
        let link = self.link.clone();
        let mut buttons = rect().horizontal().spacing(8.);
        if confirming {
            buttons = buttons.child(
                Button::new()
                    .on_press(move |_| {
                        let given_up = asking.peek().cancelled();
                        asking.set(given_up);
                    }).child(crate::i18n::tr("Keep")),
            );
        }
        buttons = buttons.child(
            Button::new()
                .on_press(move |_| {
                    let (next, now) = asking.peek().pressed();
                    asking.set(next);
                    if now {
                        link.send(Command::ClearHistory {});
                    }
                })
                .child(if confirming { "Clear them?" } else { "Clear" }),
        );
        row.child(buttons).into_element()
    }
}

/// The heading of the list of transfers on Send and Receive. Where [`TransfersHeader`] forgets the record for good, this only puts the
/// list out of the way: while anything is going it folds the list into one line and opens it again (the transfer carries on), and once
/// everything is over it is X, which hides what is over from this page and nothing more. The record is untouched, and the Transfers
/// tab still lists all of it. What is hidden is kept here, by id, for as long as the page is open.
#[derive(PartialEq)]
struct ListHeader {
    action: view::HeaderAction,
    on_press: EventHandler<view::HeaderAction>,
}

impl Component for ListHeader {
    fn render(&self) -> impl IntoElement {
        let (action, on_press) = (self.action, self.on_press.clone());
        // The same small button for the three: only its words differ, and X is a letter and not a picture, so that it needs nothing drawn.
        let words = match action {
            view::HeaderAction::Minimize => "Minimize",
            view::HeaderAction::Expand => "Expand",
            view::HeaderAction::Hide => "\u{2715}",
        };
        rect()
            .width(Size::fill())
            .horizontal()
            .cross_align(Alignment::center())
            .content(Content::flex())
            .child(rect().width(Size::flex(1.)))
            .child(Button::new().on_press(move |_| on_press.call(action)).child(crate::i18n::tr(words)))
    }
}

/// The list of transfers, with what saving on its own is set to, from the settings the owner holds now. Complete, with Clear: the one
/// of Settings, which forgets the record on purpose.
fn transfers(link: &Link, view: &View, palette: Palette) -> Transfers {
    Transfers {
        link: link.clone(),
        items: view.transfers.clone(),
        palette,
        auto_save: view.settings.auto_save,
        save_folder: view.settings.save_folder.clone(),
        context: false,
        prepared: Vec::new(),
        actions: None,
    }
}

/// The list as it sits under the big circle on Send and Receive: what is going, and what is over until it is put away with X. Never
/// Clear: that forgets the record, and is in Settings and in the Transfers tab. Minimize while anything goes, X once all is over.
///
/// Send and Receive list only what was made since the window opened and what is still going (see `view::of_this_run`): one file sent is one row,
/// not the whole record, which is the Transfers tab's.
fn transfers_here(link: &Link, view: &View, palette: Palette) -> Transfers {
    let items = view::of_this_run(&view.transfers, &view.transfers_at_start);
    Transfers { context: true, items, ..transfers(link, view, palette) }
}


#[derive(PartialEq)]
struct Transfers {
    link: Link,
    items: Vec<Progress>,
    palette: Palette,
    /// Whether a file that is offered is saved on its own, and into which folder the person chose. From the settings.
    auto_save: bool,
    save_folder: Option<std::path::PathBuf>,
    /// Whether this is the list under the big circle on Send and Receive (Minimize and X, which only put it out of the way) and not the
    /// full list with Clear that forgets the record.
    context: bool,
    /// The names of the files that are listed to go and have not begun to: their rows, with the status prepared. Only the list under the circle of
    /// Send has any.
    prepared: Vec<String>,
    /// What the person can do with the files that wait, given by the page that owns them (see [`FileActions`]); `None` on every list that has no
    /// files of its own waiting.
    actions: Option<FileActions>,
}

/// What can be done with the files that are listed and wait for the circle, as three actions the page carries out on its one list of them: the list
/// that shows them keeps no rule about them. Compared by what they are for and not by what they do, since a handler cannot be compared.
#[derive(Clone)]
struct FileActions {
    /// The file at this place in the list is taken away.
    remove: EventHandler<usize>,
    /// Every file is taken away.
    clear: EventHandler<()>,
    /// The picker opens to add more files to the list.
    add_more: EventHandler<()>,
}

impl PartialEq for FileActions {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

/// The Accept button, with the line that says what stopped it. The same button on the offer over the window and on a transfer's
/// card, so that what Accept does, and what is said when it cannot, is decided in one place.
#[derive(PartialEq)]
struct AcceptButton {
    link: Link,
    ids: Vec<String>,
    auto_save: bool,
    save_folder: Option<std::path::PathBuf>,
    palette: Palette,
    /// The solid look, for the offer over the window; the card's own button is the plain one.
    filled: bool,
}

impl Component for AcceptButton {
    fn render(&self) -> impl IntoElement {
        let said: State<Option<String>> = use_state(|| None);
        let (link, ids, auto_save, chosen) = (self.link.clone(), self.ids.clone(), self.auto_save, self.save_folder.clone());
        let sink = try_consume_context::<link::FolderSink>();
        let button = Button::new().on_press(move |_| accept(&link, &ids, auto_save, chosen.as_deref(), sink.clone(), said));
        let button = if self.filled { button.filled() } else { button };
        // The column takes what is left beside the other button of its row and no more, so that the sentence that says why the folder was not
        // chosen wraps inside it: left to the width of its text it widened the row and pushed Cancel off the edge of the screen.
        rect()
            .width(Size::flex(1.))
            .spacing(6.)
            .maybe_child(said.read().clone().map(|words| label().text(words).color(self.palette.bad).font_size(13.)))
            .child(button.child(crate::i18n::tr("Accept")))
    }
}

/// Answer an offer with Accept: where it goes is decided by the rule, and it is either saved there or the person is asked
/// for a folder, and only then is it accepted. Whatever stops it is said through `said`, in words, so that a button that cannot
/// do what it says is never silent: a folder dialog closed with no folder, the owner refusing the answer (the offer is gone, or
/// was answered already), a phone that has no dialog to choose a folder.
///
/// What the disk says of the chosen folder is looked at now, when the button is pressed and not when the page was drawn: a
/// folder that was there then can be gone.
fn accept(link: &Link, ids: &[String], auto_save: bool, chosen: Option<&std::path::Path>, sink: Option<link::FolderSink>, mut said: State<Option<String>>) {
    let state = chosen.map_or(view::Folder::Unusable, view::look_at);
    let (client, ids) = (link.client().clone(), ids.to_vec());
    // The folder is asked for once, for the first file: the others go where that one goes. Nothing to accept is nothing to do.
    let Some(first) = ids.first().cloned() else { return };
    #[cfg(target_os = "android")]
    let (transfer, rest) = (first.clone(), ids.clone());
    #[cfg(not(target_os = "android"))]
    let _ = (&sink, &first);
    // Every file is accepted into `directory`. On a phone `directory` is the staging root, and each file gets a directory of its own beneath
    // it, which the platform empties into the folder the person chose once the file is whole; elsewhere all go to the one folder.
    let answer = move |directory: std::path::PathBuf, mut said: State<Option<String>>| async move {
        for id in ids {
            let directory = if cfg!(target_os = "android") { directory.join(&id) } else { directory.clone() };
            let _ = std::fs::create_dir_all(&directory);
            match client.command(Command::AcceptTransfer { transfer: id, directory }).await {
                Ok(_) => said.set(None),
                Err(reason) => said.set(Some(reason.to_owned())),
            }
        }
    };
    match view::destination(auto_save, chosen, state) {
        view::Destination::Save(directory) => {
            said.set(None);
            spawn(answer(directory, said));
        }
        view::Destination::Ask { lost } => {
            #[cfg(not(target_os = "android"))]
            {
                said.set(lost.map(|_| "The folder chosen for incoming files is gone. Choose another.".to_owned()));
                spawn(async move {
                    match rfd::AsyncFileDialog::new().set_title("Save the file in").pick_folder().await {
                        Some(folder) => answer(folder.path().to_path_buf(), said).await,
                        None => said.set(Some("No folder was chosen, so the file was not accepted.".to_owned())),
                    }
                });
            }
            // On a phone the folder is not a path the program can write: the person chooses it with the system's picker, the
            // file is received into the program's staging, and the system puts it in the chosen folder once it has arrived whole.
            #[cfg(target_os = "android")]
            {
                let _ = lost;
                let Some(sink) = sink else {
                    said.set(Some("Choosing a folder is not available on this device.".to_owned()));
                    return;
                };
                // Saving on its own is on and the person chose a folder in Settings: the files go there without asking, which is what the
                // setting says. The same rule as the one for a path on a desktop: off, or no folder, and each file asks.
                if auto_save && let Some(tree) = (sink.saved)() {
                    said.set(None);
                    for id in &rest {
                        sink.picked.destined(id, tree.clone());
                    }
                    spawn(answer(sink.staging.clone(), said));
                    return;
                }
                // The system shows one folder picker at a time: a second Accept while one is open is told so, and does not
                // take the place of the first question (its answer would then be taken for the wrong file).
                if !sink.picked.opened(&transfer) {
                    said.set(Some("A folder is being chosen for another file. Finish that first, then accept this one.".to_owned()));
                    return;
                }
                said.set(None);
                (sink.choose)();
                let answered = link::await_folder(
                    sink.picked.clone(),
                    transfer.clone(),
                    link::LOOK_EVERY,
                    link::PICKER_PATIENCE,
                );
                spawn(async move {
                    match answered.await {
                        Ok(Some(tree)) => {
                            // Received where the program can write, in a directory of its own for each file (two files of one name must not
                            // meet there); the system puts each in the folder chosen when it is whole. One choice serves every file ticked.
                            for id in &rest {
                                sink.picked.destined(id, tree.clone());
                            }
                            answer(sink.staging.clone(), said).await;
                        }
                        _ => said.set(Some("No folder was chosen, so the file was not accepted.".to_owned())),
                    }
                });
            }
        }
    }
}

/// A file offered to this person, over the whole window, fixed at its bottom and above the navigation: on whatever screen they
/// are on, and not as a row under a circle where scanning a code left it unseen. It names who sends what and has the two
/// answers, Accept and Cancel. It is a decision with a clock on it (an offer unanswered in two minutes is refused).
#[derive(PartialEq)]
struct IncomingOffer {
    link: Link,
    offered: view::Offered,
    palette: Palette,
    /// The second now, Unix seconds, from the window's one clock: the bar is drawn again each second it changes, and the time
    /// left is worked out from it and from the deadline the owner reported, so that it is the same number a plugin reads.
    now: u64,
    /// What the buttons of the notification asked of the window, where the platform has such buttons (Android).
    buttons: Option<link::OfferButtons>,
}

impl Component for IncomingOffer {
    fn render(&self) -> impl IntoElement {
        let (palette, offer) = (self.palette, &self.offered.offer);
        // The files of the offer, and which of them are ticked: all of them at first, so that Accept with nothing touched takes the whole offer.
        // Kept in state, since the bar is drawn again every second by the window's clock and a tick must stay where the person put it.
        let files = &self.offered.files;
        let mut listing = use_state(|| false);
        // View files pressed on the notification brought the window forward: the list opens with it, since the person pressed it to see the
        // files. Asked at each drawing, and the window draws this bar every second; the answer is taken once, so a later closing stays closed.
        let said_on_accept: State<Option<String>> = use_state(|| None);
        if let Some(buttons) = &self.buttons {
            if (buttons.take_view)() && !*listing.peek() {
                listing.set(true);
            }
            // An Accept pressed on the notification or the panel that needed a window to ask for a folder: the window is here now, and asks.
            // Taken once, so a second drawing does not ask again.
            let pressed = (buttons.take_accepts)();
            if !pressed.is_empty() {
                let chosen = self.offered.save_folder.clone();
                let sink = try_consume_context::<link::FolderSink>();
                accept(&self.link, &pressed, self.offered.auto_save, chosen.as_deref(), sink, said_on_accept);
            }
        }
        let mut unticked: State<std::collections::BTreeSet<String>> = use_state(std::collections::BTreeSet::new);
        let ticked: Vec<String> = files.iter().filter(|file| !unticked.read().contains(&file.id)).map(|file| file.id.clone()).collect();
        let all: Vec<String> = files.iter().map(|file| file.id.clone()).collect();
        let several = files.len() > 1;
        let (cancel_link, cancel_all) = (self.link.clone(), if all.is_empty() { vec![offer.id.clone()] } else { all.clone() });
        // With the list open, the one name is the list said worse: it is left out, so that the bar does not name one file above and show
        // two below. The line that says how many wait stays, since it is true of the whole queue.
        let open = several && listing();
        let bar = rect()
            .width(Size::fill())
            .height(Size::auto())
            .padding(16.)
            .spacing(10.)
            .background(palette.surface)
            .border(Border::new().fill(palette.receive).width(1.).alignment(BorderAlignment::Inner))
            .child(label().text(view::offer_words_in(i18n::language(), offer)).color(palette.text).font_size(16.).font_weight(FontWeight::BOLD))
            .maybe_child((!open).then(|| label().text(offer.name.clone()).color(palette.muted).font_size(14.)))
            .maybe_child(view::queue_words_in(i18n::language(), self.offered.waiting).map(|words| mono(words, palette.muted, 11.)))
            .maybe_child(view::time_left(offer.expires_at, self.now).map(|seconds| {
                mono(i18n::format(i18n::language(), "ENDS IN {time}", &[("time", &view::clock_words(seconds))]), if seconds <= 10 { palette.bad } else { palette.muted }, 12.)
            }));
        let mut bar = bar;
        if several && listing() {
            let mut list = rect().width(Size::fill()).spacing(6.);
            for file in files {
                let id = file.id.clone();
                let on = !unticked.read().contains(&id);
                let place = file.folders.iter().map(String::as_str).chain(std::iter::once(file.name.as_str())).collect::<Vec<_>>().join("/");
                list = list.child(
                    rect()
                        .key(id.clone())
                        .width(Size::fill())
                        .horizontal()
                        .content(Content::flex())
                        .spacing(10.)
                        .cross_align(Alignment::center())
                        .child(Switch::new(WritableSetter::new(on, move |value: bool| {
                            let mut now = unticked.write();
                            if value { now.remove(&id); } else { now.insert(id.clone()); }
                        })))
                        .child(label().text(place).color(palette.text).font_size(14.).width(Size::flex(1.)))
                        .child(rect().width(Size::auto()).child(mono(view::size_in_words(file.bytes), palette.muted, 12.))),
                );
            }
            bar = bar.child(list);
        }
        let accept_ids = if several { ticked.clone() } else { vec![offer.id.clone()] };
        let mut buttons = rect().width(Size::fill()).horizontal().content(Content::flex()).cross_align(Alignment::start()).spacing(10.);
        buttons = buttons.child(Button::new().on_press(move |_| {
            for transfer in &cancel_all {
                cancel_link.send(Command::CancelTransfer { transfer: transfer.clone() });
            }
        }).child(crate::i18n::tr("Cancel")));
        if several {
            buttons = buttons.child(Button::new().on_press(move |_| listing.set(!listing())).child(crate::i18n::tr("View files")));
        }
        buttons = buttons.child(AcceptButton {
            link: self.link.clone(),
            ids: accept_ids,
            auto_save: self.offered.auto_save,
            save_folder: self.offered.save_folder.clone(),
            palette,
            filled: true,
        });
        bar.child(buttons)
    }
}

/// One transfer as a full card, with the buttons it has (cancel, accept): the same card on the list under the circle and for what is going on in the
/// Transfers tab, so what it shows and what its buttons do is decided in one place.
#[derive(PartialEq)]
struct TransferCard {
    link: Link,
    item: Progress,
    palette: Palette,
    auto_save: bool,
    save_folder: Option<std::path::PathBuf>,
}

impl Component for TransferCard {
    fn render(&self) -> impl IntoElement {
        let (item, palette) = (&self.item, self.palette);
        let (state, colour) = match &item.phase {
            Phase::Waiting => ("WAITING FOR APPROVAL".to_owned(), palette.muted),
            Phase::Moving => (format!("{}%", item.percent), palette.send),
            Phase::Done => ("DONE".to_owned(), palette.good),
            Phase::Cancelled => ("CANCELLED".to_owned(), palette.muted),
            Phase::Failed(reason) => (format!("FAILED · {reason}"), palette.bad),
        };
        let direction = if item.incoming { "INCOMING" } else { "OUTGOING" };
        let mut card = rect()
            .key(item.id.clone())
            .width(Size::fill())
            .padding(16.)
            .spacing(8.)
            .corner_radius(12.)
            .background(palette.surface)
            .border(Border::new().fill(palette.border).width(1.).alignment(BorderAlignment::Inner))
            .child(
                rect()
                    .horizontal()
                    .spacing(8.)
                    .cross_align(Alignment::center())
                    .maybe_child((item.phase == Phase::Done).then(|| label().text("\u{2713}").color(palette.good).font_size(16.).font_weight(FontWeight::BOLD)))
                    .child(label().text(item.name.clone()).color(palette.text).font_size(16.).font_weight(FontWeight::BOLD)),
            )
            .child(mono(format!("{direction} · {state}"), colour, 11.))
            .child(ProgressBar::new(f32::from(item.percent)).width(Size::fill()));
        // Cancel on the left and Accept on the right, on one row: Accept is the answer the right hand presses, and its column takes what is
        // left beside Cancel, so that the sentence that says why a folder was not chosen wraps inside it and never pushes Cancel off the edge.
        let mut answers = rect().width(Size::fill()).horizontal().content(Content::flex()).cross_align(Alignment::start()).spacing(10.);
        if item.stoppable() {
            let link = self.link.clone();
            let id = item.id.clone();
            answers = answers.child(
                Button::new().on_press(move |_| link.send(Command::CancelTransfer { transfer: id.clone() })).child(crate::i18n::tr("Cancel")),
            );
        }
        if item.acceptable() {
            answers = answers.child(AcceptButton {
                link: self.link.clone(),
                ids: vec![item.id.clone()],
                auto_save: self.auto_save,
                save_folder: self.save_folder.clone(),
                palette,
                filled: false,
            });
        }
        if item.stoppable() || item.acceptable() {
            card = card.child(answers);
        }
        card
    }
}

/// One finished transfer of the history, as a row of ONE fixed height (`view::HISTORY_ROW_HEIGHT`): its name, and under it which way it went and
/// how it ended. No buttons and no bar, so that every row is the same height and a long history is a column of equal, cheap rows.
#[derive(PartialEq)]
struct HistoryRow {
    item: Progress,
    palette: Palette,
}

impl Component for HistoryRow {
    fn render(&self) -> impl IntoElement {
        let (item, palette) = (&self.item, self.palette);
        let (state, colour) = match &item.phase {
            Phase::Done => ("DONE".to_owned(), palette.good),
            Phase::Cancelled => ("CANCELLED".to_owned(), palette.muted),
            Phase::Failed(reason) => (format!("FAILED · {reason}"), palette.bad),
            // Not in the history: what is going is a card above it. Said plainly if one ever got here, never hidden.
            Phase::Waiting | Phase::Moving => (format!("{}%", item.percent), palette.send),
        };
        let direction = if item.incoming { "INCOMING" } else { "OUTGOING" };
        rect()
            .width(Size::fill())
            .height(Size::px(view::HISTORY_ROW_HEIGHT))
            .padding((6., 12.))
            .spacing(2.)
            .corner_radius(10.)
            .background(palette.surface)
            .border(Border::new().fill(palette.border).width(1.).alignment(BorderAlignment::Inner))
            .main_align(Alignment::center())
            .child(label().text(item.name.clone()).color(palette.text).font_size(15.).font_weight(FontWeight::BOLD).max_lines(1))
            .child(mono(format!("{direction} · {state}"), colour, 11.))
    }
}

impl Component for Transfers {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        // Only what the list under the big circle needs, and kept here: the transfers are the owner's, and nothing of this touches them.
        // `folded` folds the list into a line while something goes; `put_away` holds the ids that X hid, so that what was hidden stays hidden
        // only as long as it is the same transfer (a new one has a new id, and shows).
        let mut folded = use_state(|| false);
        let mut put_away: State<Vec<String>> = use_state(Vec::new);
        if self.items.is_empty() && self.prepared.is_empty() {
            return rect().into_element();
        }
        // What the list draws is ONE kind of row. In the circle's context it is the batch as its own model orders it (`view::batch_rows`: a heading for
        // each device when it went to more than one, then that device's files), from what X has not hidden; elsewhere, the transfers in the order they
        // always had. The drawing below walks these and decides nothing about grouping.
        let shown: Vec<view::BatchRow> = if self.context {
            let visible: Vec<Progress> = view::not_put_away(&self.items, &put_away.read()).into_iter().cloned().collect();
            view::batch_rows(&visible)
        } else {
            view::transfer_list(&self.items).cloned().map(view::BatchRow::Transfer).collect()
        };
        let header: Element = if self.context {
            match view::header_action(&self.items, &put_away.read(), *folded.read()) {
                Some(action) => {
                    let items = self.items.clone();
                    ListHeader {
                        action,
                        on_press: EventHandler::new(move |action: view::HeaderAction| match action {
                            view::HeaderAction::Minimize => folded.set(true),
                            view::HeaderAction::Expand => folded.set(false),
                            // Only what is over is put away, and only when nothing is going: the model says which (see `put_away_now`).
                            view::HeaderAction::Hide => {
                                let mut now = put_away.peek().clone();
                                now.extend(view::put_away_now(&items));
                                now.sort();
                                now.dedup();
                                put_away.set(now);
                            }
                        }),
                    }
                    .into_element()
                }
                None => rect().into_element(),
            }
        } else {
            TransfersHeader {
                link: self.link.clone(),
                palette,
                can_clear: self.items.iter().any(|item| item.phase.is_over()),
            }
            .into_element()
        };
        // Folded: while something goes, the list is one line, and the transfer carries on.
        if self.context
            && *folded.read()
            && let Some(line) = view::folded_line(&self.items)
        {
            return rect()
                .width(Size::fill())
                .spacing(10.)
                .child(header)
                .child(label().text(line).color(palette.muted).font_size(14.))
                .into_element();
        }
        // The Transfers tab has the whole record, which has no end: what is going stays as full cards above (few, and each has buttons), and what is
        // over (the history) is drawn as compact rows of ONE fixed height, only the newest of them (`view::HISTORY_ROWS_MADE`), so that the cost does
        // not grow with the history (`view::sections`, `view::HISTORY_ROW_HEIGHT`). The list under the circle is the batch and is small: it keeps
        // its cards.
        if !self.context {
            let parts = view::sections(&self.items);
            let history: Vec<Progress> = parts.history.iter().map(|item| (*item).clone()).collect();
            let mut going = rect().width(Size::fill()).spacing(10.);
            for item in parts.going {
                going = going.child(TransferCard {
                    link: self.link.clone(),
                    item: item.clone(),
                    palette,
                    auto_save: self.auto_save,
                    save_folder: self.save_folder.clone(),
                });
            }
            // The history has no end, so only the newest of it is made as rows (`view::HISTORY_ROWS_MADE`): the cost does not grow with it. They go
            // straight into the page's own column, which already scrolls.
            let mut history_list = rect().width(Size::fill()).spacing(view::HISTORY_ROW_GAP);
            for item in history.iter().take(view::HISTORY_ROWS_MADE) {
                history_list = history_list.child(rect().key(item.id.clone()).child(HistoryRow { item: item.clone(), palette }));
            }
            return rect().width(Size::fill()).spacing(10.).child(header).child(going).child(history_list).into_element();
        }
        let mut list = rect().width(Size::fill()).spacing(10.);
        for row in shown {
            let item = match row {
                view::BatchRow::Transfer(item) => item,
                // The heading of one device's part of the batch: who, and how that device stands by itself, so that a problem with one device is
                // read here and not guessed from the circle, which says only how the batch as a whole goes.
                view::BatchRow::Device { name, state, .. } => {
                    let (words, tone) = view::device_state_words_in(i18n::language(), &state);
                    let colour = match tone {
                        view::Tone::Good => palette.good,
                        view::Tone::Ask | view::Tone::Going => palette.send,
                        view::Tone::Bad => palette.bad,
                        view::Tone::Quiet => palette.muted,
                    };
                    list = list.child(
                        rect()
                            .width(Size::fill())
                            .horizontal()
                            .spacing(8.)
                            .cross_align(Alignment::center())
                            .child(label().text(name).color(palette.text).font_size(14.).font_weight(FontWeight::BOLD))
                            .child(mono(words, colour, 11.)),
                    );
                    continue;
                }
            };
            list = list.child(TransferCard {
                link: self.link.clone(),
                item,
                palette,
                auto_save: self.auto_save,
                save_folder: self.save_folder.clone(),
            });
        }
        // The files that are listed and have not begun to go: a row each, with their status, in the same list. Never on a list that is not the
        // one under the circle (`prepared` is empty there).
        // What can be done with the files that wait, from the one list of them: more can be added, one can be taken away, all can be cleared. The
        // page gives these three actions (this list has no rule of its own about the files), and only when there is at least one file is anything
        // offered: with none, the circle is what chooses.
        if let Some(actions) = self.actions.clone().filter(|_| !self.prepared.is_empty()) {
            let add_more = actions.add_more.clone();
            let clear = actions.clear.clone();
            list = list.child(
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .content(Content::flex())
                    .spacing(10.)
                    .cross_align(Alignment::center())
                    .child(Button::new().on_press(move |_| add_more.call(())).child(i18n::tr("+ Add more files")))
                    .child(rect().width(Size::flex(1.)))
                    .child(Button::new().on_press(move |_| clear.call(())).child(i18n::tr("Clear the list"))),
            );
        }
        for (index, name) in self.prepared.iter().enumerate() {
            let remove = self.actions.as_ref().map(|actions| actions.remove.clone());
            list = list.child(
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .content(Content::flex())
                    .cross_align(Alignment::center())
                    .padding(16.)
                    .spacing(8.)
                    .corner_radius(12.)
                    .background(palette.surface)
                    .border(Border::new().fill(palette.border).width(1.).alignment(BorderAlignment::Inner))
                    .child(
                        rect()
                            .width(Size::flex(1.))
                            .spacing(8.)
                            .child(label().text(name.clone()).color(palette.text).font_size(16.).font_weight(FontWeight::BOLD))
                            .child(mono("OUTGOING \u{b7} PREPARED".to_owned(), palette.muted, 11.)),
                    )
                    .maybe_child(remove.map(|remove| {
                        Element::from(Button::new().on_press(move |_| remove.call(index)).child("\u{2715}"))
                    })),
            );
        }
        let list: Element = match view::transfer_list_height(self.items.len() + self.prepared.len()) {
            Some(height) => rect()
                .width(Size::fill())
                .height(Size::px(height))
                .child(ScrollView::new().auto_hide_scrollbars(false).child(list))
                .into(),
            None => list.into(),
        };
        // Keep the controls outside the scrolling viewport. History stays in the model and every
        // row remains reachable, but more rows cannot grow the enclosing page indefinitely.
        rect()
            .width(Size::fill())
            .spacing(10.)
            .child(header)
            .child(list)
            .into_element()
    }
}

// ---- Transfers -------------------------------------------------------------------------

/// Every transfer there is a record of, found by what is typed and by one filter. Searching and filtering only choose what is listed: they never
/// remove anything, and Clear (which does) is the one of the list below, asking twice, as everywhere else.
#[derive(PartialEq)]
struct TransfersPage {
    link: Link,
    view: View,
    palette: Palette,
    form: view::Form,
}

impl Component for TransfersPage {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let query = use_state(String::new);
        let mut filter = use_state(view::TransferFilter::default);
        let typed = query.read().clone();
        let found: Vec<Progress> = view::find_transfers(&self.view.transfers, &typed, *filter.read()).into_iter().cloned().collect();
        // One chip for each filter, the one in use filled. Words only, so that they are translated like any other and need no picture. Each word is
        // ONE line (`max_lines(1)`), as on the plugin buttons: a chip is as wide as its word, and the row, wider than the page, scrolls sideways. A
        // word left free to wrap was squeezed to what was left of the page and broke letter by letter down a chip 170 px tall.
        let mut chips = rect().width(Size::auto()).horizontal().spacing(8.);
        for which in view::TransferFilter::ALL {
            let button = Button::new().on_press(move |_| filter.set(which)).child(label().text(i18n::tr(which.label())).max_lines(1));
            chips = chips.child(if *filter.read() == which { Element::from(button.filled()) } else { Element::from(button) });
        }
        let nothing = self.view.transfers.is_empty() || found.is_empty();
        let mut page = rect()
            .width(Size::fill())
            .spacing(self.form.page_section_gap())
            .child(section("TRANSFERS", palette))
            .child(Input::new(query).placeholder(i18n::tr("Search transfers")).width(Size::fill()))
            // The chips are one row of buttons, and the scroll view they sit in is as tall as that row (`auto`), and not the toolkit's default of
            // `fill`, which takes all the height that is left in the page and leaves nothing for the list after it.
            .child(ScrollView::new().direction(Direction::Horizontal).height(Size::auto()).child(chips));
        if nothing {
            page = page.child(
                label()
                    .text(i18n::tr(if self.view.transfers.is_empty() { "Nothing has been sent or received yet." } else { "No transfer matches." }))
                    .color(palette.muted)
                    .font_size(15.),
            );
        } else {
            page = page.child(Transfers {
                link: self.link.clone(),
                items: found,
                palette,
                auto_save: self.view.settings.auto_save,
                save_folder: self.view.settings.save_folder.clone(),
                context: false,
                prepared: Vec::new(),
                actions: None,
            });
        }
        frame(palette, self.form, page)
    }
}

// ---- Devices ---------------------------------------------------------------------------

#[derive(PartialEq)]
struct DevicesPage {
    link: Link,
    view: View,
    /// The pictures this window holds, by digest: what a device's avatar is drawn from.
    pictures: shelf::Held,
    palette: Palette,
    form: view::Form,
    /// The width the window has, in pixels, from which the size of the pairing portal is decided.
    room: f32,
    /// The platform's camera, for reading the code of the device that is pairing; absent where there is none.
    camera: Option<link::CameraControl>,
    /// The platform's own file picker where there is one (Android), for the quick Send on a device's card.
    file_picker: Option<link::FilePicker>,
}

impl Component for DevicesPage {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let mut list = rect().width(Size::fill()).spacing(10.);
        for device in &self.view.devices {
            let link = self.link.clone();
            let id = device.id.clone();
            let online = device.presence == Presence::Online;
            let mut card = rect()
                .key(device.id.clone())
                .width(Size::fill())
                .padding(16.)
                .spacing(10.)
                .corner_radius(12.)
                .background(palette.surface)
                .border(Border::new().fill(palette.border).width(1.).alignment(BorderAlignment::Inner))
                .child(
                    rect()
                        .width(Size::fill())
                        .horizontal()
                        .cross_align(Alignment::center())
                        .content(Content::flex())
                        .spacing(14.)
                        .child(avatar_view::Avatar {
                            picture: device.picture.and_then(|digest| self.pictures.get(&digest)).map(avatar_view::Shared).map(|shared| shared.0.clone()),
                            name: device.name.clone(),
                            size: 40.,
                            palette,
                        })
                        .child(
                            rect()
                                .width(Size::flex(1.))
                                .child(label().text(device.name.clone()).color(palette.text).font_size(17.).font_weight(FontWeight::BOLD))
                                .child(mono(if online { "ONLINE" } else { "OFFLINE" }, if online { palette.good } else { palette.muted }, 11.)),
                        )
                        .child(SendToDevice { link: link.clone(), peer: id.clone(), online, file_picker: self.file_picker.clone() })
                        .child(Button::new().on_press(move |_| link.send(Command::ForgetPeer { peer: PeerId(id.clone()) })).child(crate::i18n::tr("Forget"))),
                );
            // What was sent to and received from THIS device, told apart from every other by its identity and not by the name it
            // gave itself.
            let (history, more) = view::history_shown(&device.id, &self.view.transfers);
            if !history.is_empty() {
                card = card.child(mono(crate::i18n::tr("HISTORY"), palette.muted, 11.));
            }
            for item in history {
                card = card.child(label().text(view::history_line_in(i18n::language(), item)).color(palette.muted).font_size(13.));
            }
            if more > 0 {
                card = card.child(label().text(i18n::format(i18n::language(), "+ {count} more", &[("count", &more.to_string())])).color(palette.muted).font_size(13.));
            }
            list = list.child(card);
        }
        if self.view.devices.is_empty() {
            list = list.child(label().text(crate::i18n::tr("No paired peers yet. Pair a device above.")).color(palette.muted).font_size(16.));
        }
        // Devices met by a code and never paired have no saved entry, but what was sent to and received from them is a history
        // all the same: one card for each identity, with the name it gave itself as a label. Nothing here pairs or allows anything.
        let mut met = rect().width(Size::fill()).spacing(10.);
        let unpaired = view::unpaired_with_history(&self.view.devices, &self.view.transfers);
        for (peer, name) in &unpaired {
            let mut card = rect()
                .key(format!("met-{peer}"))
                .width(Size::fill())
                .padding(16.)
                .spacing(10.)
                .corner_radius(12.)
                .background(palette.surface)
                .border(Border::new().fill(palette.border).width(1.).alignment(BorderAlignment::Inner))
                .child(
                    rect()
                        .width(Size::fill())
                        .horizontal()
                        .cross_align(Alignment::center())
                        .content(Content::flex())
                        .spacing(14.)
                        // A device met by a code never shows a picture (none crosses a link made for files), so this is its initial.
                        .child(avatar_view::Avatar { picture: None, name: name.map_or("Another device".to_owned(), str::to_owned), size: 40., palette })
                        .child(
                            rect()
                                .width(Size::flex(1.))
                                .child(label().text(name.map_or("Another device".to_owned(), str::to_owned)).color(palette.text).font_size(17.).font_weight(FontWeight::BOLD))
                                .child(mono(crate::i18n::tr("NOT PAIRED"), palette.muted, 11.)),
                        )
                )
                .child(mono(crate::i18n::tr("HISTORY"), palette.muted, 11.));
            let (shown, more) = view::history_shown(peer, &self.view.transfers);
            for item in shown {
                card = card.child(label().text(view::history_line_in(i18n::language(), item)).color(palette.muted).font_size(13.));
            }
            if more > 0 {
                card = card.child(label().text(i18n::format(i18n::language(), "+ {count} more", &[("count", &more.to_string())])).color(palette.muted).font_size(13.));
            }
            met = met.child(card);
        }
        let mut body = rect()
            .width(Size::fill())
            .spacing(18.)
            .child(label().text(crate::i18n::tr("Peers")).color(palette.text).font_size(44.).font_weight(FontWeight::BOLD))
            .child(PairingSection { link: self.link.clone(), palette, room: self.room, form: self.form, camera: self.camera.clone() })
            .child(section("PAIRED PEERS", palette))
            .child(list);
        if !unpaired.is_empty() {
            body = body.child(section("MET BY CODE", palette)).child(met);
        }
        frame(palette, self.form, body)
    }
}

/// The quick Send on a paired device's card: pick files and they go straight to that device, with no page between. Offered only while
/// the device is online (a file offered to a device that is not there would wait for nothing), and what the choice left out is said
/// under it, as the Send page says it. It keeps its own state, so it is a component and not a part of the loop that draws the cards.
#[derive(PartialEq)]
struct SendToDevice {
    link: Link,
    peer: String,
    online: bool,
    file_picker: Option<link::FilePicker>,
}

impl Component for SendToDevice {
    fn render(&self) -> impl IntoElement {
        let said: State<Option<SendNotice>> = use_state(|| None);
        let (link, peer, picker, online) = (self.link.clone(), self.peer.clone(), self.file_picker.clone(), self.online);
        let button = Button::new()
            .enabled(online)
            .on_press(move |_| {
                if online {
                    choose_and_send(&link, picker.clone(), peer.clone(), said);
                }
            }).child(crate::i18n::tr("Send"));
        rect().spacing(4.).cross_align(Alignment::end()).child(button).maybe_child((*said.read()).map(|notice| label().text(notice.words()).font_size(11.)))
    }
}

// ---- Plugins ---------------------------------------------------------------------------

/// The switches of one plugin for one scope, either one paired device (`peer`) or no device at all.
/// One row for each permission the OWNER offers in that scope, on if it is held. A switch names ONE
/// permission and what it should become; the owner applies that to what it holds at that moment, so
/// two quick flips cannot undo each other and a flip never takes away something it did not touch.
fn permission_block(
    link: &Link,
    plugin: &PluginInfo,
    offered: &[peersus_plugin_api::permission::Permission],
    held: &peersus_core::plugins::Grants,
    peer: Option<peersus_core::model::PeerId>,
    palette: Palette,
) -> Element {
    let mut block = rect().width(Size::fill()).spacing(8.);
    for row in view::PermissionRows::of_in(i18n::language(), plugin, offered, held) {
        let (link, id, permission, peer) =
            (link.clone(), plugin.id.clone(), row.permission.clone(), peer.clone());
        let target = settings_search::Target::Permission { plugin: id.clone(), permission: permission.clone(), peer: peer.clone() };
        block = block.child(settings_navigation::anchor(target,
            rect()
                .width(Size::fill())
                .horizontal()
                .cross_align(Alignment::center())
                .content(Content::flex())
                .spacing(12.)
                .child(
                    rect()
                        .width(Size::flex(1.))
                        .child(mono(row.permission.name.clone(), palette.send, 11.))
                        .child(label().text(row.description.clone()).color(palette.muted).font_size(13.)),
                )
                .child(Switch::new(WritableSetter::new(row.allowed, move |value: bool| {
                    link.send(Command::Allow {
                        plugin: id.clone(),
                        peer: peer.clone(),
                        permission: permission.clone(),
                        on: value,
                    });
                }))),
            palette));
    }
    block.into()
}

/// The installed plugins as cards with what the person needs to manage them: the state, a switch
/// that turns a plugin off or on, a restart, a removal that asks twice, and what each one can do.
/// The one place plugins are listed is the Extensions tab of Settings.
#[derive(PartialEq)]
struct PluginManager {
    link: Link,
    view: View,
    pictures: shelf::Held,
    palette: Palette,
    compact: bool,
}

impl Component for PluginManager {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let mut asking = use_state(view::RemoveAsk::default);
        let store_query = use_state(String::new);
        let mut list = rect().width(Size::fill()).spacing(10.);
        #[cfg(not(target_os = "android"))]
        let mut bundles = use_state(Vec::<peersus_app::discovery::Bundle>::new);
        #[cfg(not(target_os = "android"))]
        use_hook(move || {
            let (send, receive) = tokio::sync::oneshot::channel();
            std::thread::spawn(move || {
                let found = std::env::current_exe().ok().map(|path| peersus_app::discovery::bundles_near(&path)).unwrap_or_default();
                let _ = send.send(found);
            });
            spawn(async move { if let Ok(found) = receive.await { bundles.set(found); } });
        });
        // THE STORE. What the maintainer approved, with who made it, installed or removed with one click. Every rule
        // (which button, which words, what the search finds) is `view`'s; this only draws it. The link is shown as
        // text and is not opened from here: following an address out of a list is how a person is led somewhere else.
        {
            let store = &self.view.store;
            let typed = store_query.read().clone();
            let rows = view::store_rows(store, &typed);
            let acting = view::store_can_act(store);
            let refresh_link = self.link.clone();
            let mut section = rect()
                .width(Size::fill())
                .spacing(8.)
                .child(self::section("STORE", palette))
                .child(
                    rect()
                        .width(Size::fill())
                        .spacing(8.)
                        .maybe(!self.compact, |row| row.horizontal().cross_align(Alignment::center()).content(Content::flex()))
                        .child(rect().width(if self.compact { Size::fill() } else { Size::flex(1.) }).child(
                            Input::new(store_query).placeholder(i18n::tr("Search the store")).width(Size::fill()),
                        ))
                        .child(
                            Button::new()
                                .enabled(acting)
                                .on_press(move |_| refresh_link.send(Command::StoreRefresh {}))
                                .child(i18n::tr(if store.listings.is_empty() { "Refresh" } else { "Refresh the list" })),
                        ),
                );
            if let Some(words) = view::store_empty(store, &rows, &typed) {
                section = section.child(label().text(i18n::tr(words)).color(palette.muted).font_size(13.));
            }
            for row in &rows {
                let name = String::from(row.listing.plugin.clone());
                let removing = *asking.read() == view::RemoveAsk::Asking(row.listing.plugin.clone());
                let mut buttons = rect().spacing(8.);
                if !self.compact { buttons = buttons.horizontal(); }
                if let Some(text) = row.action.button() {
                    let (link, plugin) = (self.link.clone(), row.listing.plugin.clone());
                    buttons = buttons.child(
                        Button::new()
                            .filled()
                            .enabled(acting)
                            .on_press(move |_| link.send(Command::StoreInstall { plugin: plugin.clone() }))
                            .child(i18n::tr(text)),
                    );
                }
                if row.action.can_remove() {
                    let (link, plugin) = (self.link.clone(), row.listing.plugin.clone());
                    buttons = buttons.child(
                        Button::new()
                            .on_press(move |_| {
                                let (next, remove) = asking.read().clone().pressed(&plugin);
                                asking.set(next);
                                if remove {
                                    link.send(Command::RemovePackage { plugin: plugin.clone() });
                                }
                            })
                            .child(i18n::tr(if removing { "Remove for good?" } else { "Remove" })),
                    );
                }
                if removing {
                    buttons = buttons.child(Button::new().on_press(move |_| asking.set(view::RemoveAsk::None.cancelled())).child(i18n::tr("Keep")));
                }
                let mut card = rect()
                    .key(format!("store-{name}"))
                    .width(Size::fill())
                    .padding(14.)
                    .spacing(6.)
                    .corner_radius(12.)
                    .background(palette.surface)
                    .border(Border::new().fill(palette.border).width(1.).alignment(BorderAlignment::Inner))
                    .child(label().text(name.clone()).color(palette.text).font_size(17.).font_weight(FontWeight::BOLD))
                    .child(mono(
                        i18n::format(i18n::language(), "by {author} · version {version}", &[("author", &row.listing.author), ("version", &row.listing.version)]),
                        palette.send,
                        11.,
                    ));
                if !row.listing.description.is_empty() {
                    card = card.child(label().text(row.listing.description.clone()).color(palette.muted).font_size(13.));
                }
                card = card.child(mono(row.listing.link.clone(), palette.muted, 11.));
                if let Some(installed) = row.listing.installed_version.as_deref() {
                    card = card.child(mono(
                        i18n::format(i18n::language(), "Installed: version {version}", &[("version", installed)]),
                        palette.good,
                        11.,
                    ));
                }
                if let Some(note) = row.action.note() {
                    card = card.child(label().text(i18n::tr(note)).color(palette.muted).font_size(13.));
                }
                if removing {
                    card = card.child(
                        label().text(i18n::tr("This also forgets everything it was allowed to do, on every device. Press again to remove it."))
                            .color(palette.bad)
                            .font_size(13.),
                    );
                }
                section = section.child(card.child(buttons));
            }
            list = list.child(section);
        }
        // Everything below is what is installed, and the manual way to install: its own heading.
        list = list.child(self::section("INSTALLED", palette));
        // One way to install. A package is a folder or one archive of one; the person does not say
        // which, the owner looks at the path. The button opens the system's file dialog, which is
        // where an archive is found; a folder is dropped on this page, because the system's dialogs
        // cannot pick a folder and a file in the same window. Both send the same command and nothing
        // is installed by either: the owner reads the package and shows it below first.
        let install_link = self.link.clone();
        let drop_link = self.link.clone();
        let mut refused = use_state(|| None::<&'static str>);
        #[cfg(target_os = "android")]
        let plugin_picker = try_consume_context::<link::PluginPicker>();
        list = list.child(
            rect()
                .width(Size::fill())
                .spacing(6.)
                .on_file_drop(move |e: Event<FileEventData>| {
                    match view::Install::of_drop(e.file_paths.clone(), view::DropKind::of) {
                        view::Install::Take(path) => {
                            refused.set(None);
                            if let Some(source) = peersus_core::model::SourceDir::new(path) {
                                drop_link.send(Command::StageInstall { source });
                            }
                        }
                        view::Install::Refuse(words) => refused.set(Some(words)),
                        view::Install::Nothing => {}
                    }
                })
                .child(
                    Button::new()
                        .filled()
                        .on_press(move |_| {
                            let link = install_link.clone();
                            #[cfg(target_os = "android")]
                            let plugin_picker = plugin_picker.clone();
                            spawn(async move {
                                #[cfg(not(target_os = "android"))]
                                // No filter: every file is shown. A filter would hide a package
                                // whose name does not end as expected, with no word about why, and a
                                // file that is not an archive is refused by the owner in plain words.
                                if let Some(file) = rfd::AsyncFileDialog::new()
                                    .set_title("Choose a plugin archive (.tar, .tar.gz, .tgz)")
                                    .pick_file()
                                    .await
                                    && let Some(source) = peersus_core::model::SourceDir::new(file.path())
                                {
                                    link.send(Command::StageInstall { source });
                                }
                                #[cfg(target_os = "android")]
                                {
                                    let _ = &link;
                                    match plugin_picker.as_ref().ok_or("The plugin picker is unavailable").and_then(|picker| (picker.0)()) {
                                        Ok(()) => refused.set(None),
                                        Err(words) => refused.set(Some(words)),
                                    }
                                }
                            });
                        })
                        .child(i18n::tr("Install")),
                )
                .child(
                    label().text(crate::i18n::tr("Choose a plugin archive, or drop a folder or an archive here."))
                        .color(palette.muted)
                        .font_size(13.),
                )
                .child(label().text(i18n::tr("Manual installation does not imply official approval. Review the extension and its source; you are responsible for code installed outside the Store."))
                    .color(palette.muted).font_size(13.))
                .children(refused.read().iter().map(|words| {
                    label().text(i18n::tr(words)).color(palette.muted).font_size(13.)
                })),
        );
        #[cfg(not(target_os = "android"))]
        for bundle in bundles.read().iter().filter(|bundle| !self.view.plugins.iter().any(|installed| installed.id.as_str() == bundle.id)) {
            let (link, source) = (self.link.clone(), bundle.source.clone());
            list = list.child(Button::new()
                .child(i18n::format(i18n::language(), "Install bundled {name}", &[("name", &bundle.id)]))
                .on_press(move |_| {
                    if let Some(source) = peersus_core::model::SourceDir::new(source.clone()) {
                        link.send(Command::StageInstall { source });
                    }
                }));
        }
        for info in &self.view.plugins {
            let PluginInfo { id, manifest, .. } = info;
            let (word, tone) = view::plugin_word(info);
            let actions = view::PluginActions::for_plugin(info);
            let name = String::from(id.clone());
            let confirming = *asking.read() == view::RemoveAsk::Asking(id.clone());
            let mut buttons = rect().spacing(8.);
            if !self.compact { buttons = buttons.horizontal(); }
            {
                let plugin = id.clone();
                let main = match actions.main.clone() {
                    view::PluginMain::Disable => Some(("Disable", Command::DisablePlugin { plugin })),
                    view::PluginMain::Enable => Some(("Enable", Command::EnablePlugin { plugin })),
                    // Given back exactly as the owner reported it: what is approved is what was displayed.
                    view::PluginMain::Approve { program } => Some(("Approve", Command::ApprovePlugin { plugin, program })),
                    view::PluginMain::None => None,
                };
                if let Some((text, command)) = main {
                    let link = self.link.clone();
                    buttons = buttons.child(Button::new().on_press(move |_| link.send(command.clone())).child(i18n::tr(text)));
                }
            }
            if actions.can_restart {
                let (link, plugin) = (self.link.clone(), id.clone());
                buttons = buttons
                    .child(Button::new().on_press(move |_| link.send(Command::RestartPlugin { plugin: plugin.clone() })).child(i18n::tr("Restart")));
            }
            let (link, plugin) = (self.link.clone(), id.clone());
            buttons = buttons.child(
                Button::new()
                    .on_press(move |_| {
                        let (next, remove) = asking.read().clone().pressed(&plugin);
                        asking.set(next);
                        if remove {
                            link.send(Command::RemovePackage { plugin: plugin.clone() });
                        }
                    })
                    .child(i18n::tr(if confirming { "Remove for good?" } else { "Remove" })),
            );
            if confirming {
                buttons = buttons.child(Button::new().on_press(move |_| asking.set(view::RemoveAsk::None.cancelled())).child(crate::i18n::tr("Keep")));
            }
            let mut heading = rect().width(Size::fill()).spacing(6.)
                .child(label().text(name.clone()).color(palette.text).font_size(18.).font_weight(FontWeight::BOLD))
                .child(mono(
                    i18n::tr(word),
                    match tone {
                        view::Tone::Good => palette.good,
                        view::Tone::Ask => palette.send,
                        view::Tone::Bad => palette.bad,
                        view::Tone::Quiet => palette.muted,
                        view::Tone::Going => palette.send,
                    },
                    11.,
                ));
            if !self.compact {
                heading = heading.horizontal().cross_align(Alignment::center()).content(Content::flex());
            }
            let mut card = rect()
                .key(name.clone())
                .width(Size::fill())
                .padding(18.)
                .spacing(12.)
                .corner_radius(12.)
                .background(palette.surface)
                .border(Border::new().fill(palette.border).width(1.).alignment(BorderAlignment::Inner))
                .child(heading)
                .child(buttons);
            if confirming {
                card = card.child(
                    label().text(crate::i18n::tr("This also forgets everything it was allowed to do, on every device. Press again to remove it."))
                        .color(palette.bad)
                        .font_size(13.),
                );
            }
            if let Some(note) = view::plugin_note(info) {
                card = card.child(label().text(i18n::tr(note)).color(palette.muted).font_size(13.));
            }
            if let Some(package) = &info.package {
                card = package_lines(card, package, palette);
            }
            for (name, description) in &manifest.capabilities {
                card = card.child(
                    rect()
                        .width(Size::fill())
                        .spacing(6.)
                        .child(mono(name.clone(), palette.send, 11.))
                        .child(label().text(description.clone()).color(palette.muted).font_size(13.)),
                );
            }
            card = card.child(plugin_controls::Controls { link: self.link.clone(), view: self.view.clone(), plugin: info.clone(), pictures: self.pictures.clone(), palette });
            list = list.child(settings_navigation::anchor(settings_search::Target::Plugin(id.clone()), card, palette));
        }
        if self.view.plugins.is_empty() {
            list = list.child(label().text(crate::i18n::tr("No plugins installed.")).color(palette.muted).font_size(16.));
        }
        // Packages the owner has read and is holding for confirmation: what each is and what it asks
        // for, shown BEFORE anything is installed, and installed only if the person says so.
        for staged in &self.view.staged {
            let name = String::from(staged.id.clone());
            let how = view::Staging::of(staged, &self.view.plugins);
            let (confirm_link, plugin) = (self.link.clone(), staged.id.clone());
            let (cancel_link, cancelled) = (self.link.clone(), staged.id.clone());
            let mut card = rect()
                .key(format!("staged-{name}"))
                .width(Size::fill())
                .padding(18.)
                .spacing(8.)
                .corner_radius(12.)
                .background(palette.surface)
                .border(Border::new().fill(palette.send).width(1.5).alignment(BorderAlignment::Inner))
                .child(
                    rect()
                        .width(Size::fill())
                        .spacing(8.)
                        .child(label().text(name.clone()).color(palette.text).font_size(18.).font_weight(FontWeight::BOLD))
                        .child(mono(crate::i18n::tr("WAITING FOR YOU"), palette.send, 11.))
                        .child(
                            rect()
                                .spacing(8.)
                                .child(
                                    Button::new()
                                        .filled()
                                        .on_press(move |_| confirm_link.send(Command::ConfirmInstall { plugin: plugin.clone() }))
                                        .child(i18n::tr(how.button())),
                                )
                                .child(
                                    Button::new()
                                        .on_press(move |_| cancel_link.send(Command::CancelInstall { plugin: cancelled.clone() })).child(crate::i18n::tr("Not now")),
                                ),
                        ),
                )
                .child(
                    label()
                        .text(i18n::tr(if how == view::Staging::Replaces {
                            "A plugin with this name is installed. This replaces it and starts with nothing allowed."
                        } else {
                            "It starts with nothing allowed. You decide what it may do after it is installed."
                        }))
                        .color(palette.muted)
                        .font_size(13.),
                );
            // Who made it and where to read about it, shown BEFORE the person decides.
            if let Some(package) = &staged.package {
                card = package_lines(card, package, palette);
            }
            for (capability, description) in &staged.manifest.capabilities {
                card = card.child(
                    rect()
                        .width(Size::fill())
                        .spacing(6.)
                        .child(mono(capability.clone(), palette.send, 11.))
                        .child(label().text(description.clone()).color(palette.muted).font_size(13.)),
                );
            }
            list = list.child(card);
        }
        list
    }
}

fn plugin_cards(link: &Link, view: &View, pictures: &shelf::Held, palette: Palette, compact: bool) -> Element {
    PluginManager { link: link.clone(), view: view.clone(), pictures: pictures.clone(), palette, compact }.into()
}

// ---- Settings --------------------------------------------------------------------------

#[derive(PartialEq)]
struct SettingsPage {
    link: Link,
    view: View,
    palette: Palette,
    form: view::Form,
    tab: State<SettingsTab>,
    /// The compact sheet's close action; desktop navigation stays outside Settings.
    on_close: Option<EventHandler<()>>,
    /// This device's own picture, if it has one: drawn large on the Device tab.
    picture: Option<avatar_view::Shared>,
    /// Peer pictures already fetched by the shared shelf; no additional requests or pixel copies.
    pictures: shelf::Held,
    /// The platform's own file picker, when the toolkit has no dialog (Android).
    file_picker: Option<link::FilePicker>,
    /// The platform's notifications, when the person can block them outside the program (Android): shown as a row of the Device tab.
    notifications: Option<link::NotificationsControl>,
    /// The platform's choice of which camera reads a code (Android): shown as a row of the Device tab.
    camera_choice: Option<link::CameraChoice>,
    /// The platform's camera and the window's width, for the test of the camera in its row (Android).
    camera: Option<link::CameraControl>,
    room: f32,
}

/// The folder incoming files are saved in on their own, said in full so that nothing about where a file goes is hidden,
/// with the two ways to change it: choose one, or have none (then every file asks). There is no default folder.
fn save_folder_row(link: &Link, folder: Option<&std::path::Path>, palette: Palette) -> Element {
    let shown = folder.map_or_else(
        || i18n::tr("No folder chosen. Every file asks where it goes.").to_owned(),
        |path| path.display().to_string(),
    );
    let choose_link = link.clone();
    let choose = move |_| {
        #[cfg(not(target_os = "android"))]
        {
            let link = choose_link.clone();
            spawn(async move {
                if let Some(folder) = rfd::AsyncFileDialog::new().set_title("Folder for incoming files").pick_folder().await {
                    link.send(Command::PatchSettings {
                        patch: SettingsPatch {
                            save_folder: Some(peersus_core::config::SaveFolderChange::Folder(folder.path().to_path_buf())),
                            ..SettingsPatch::default()
                        },
                    });
                }
            });
        }
        #[cfg(target_os = "android")]
        let _ = &choose_link;
    };
    let clear_link = link.clone();
    let clear = move |_| {
        clear_link.send(Command::PatchSettings {
            patch: SettingsPatch {
                save_folder: Some(peersus_core::config::SaveFolderChange::Default),
                ..SettingsPatch::default()
            },
        });
    };
    let mut row = rect().width(Size::fill()).spacing(8.).child(mono(shown, palette.text, 12.));
    row = row.child(
        rect()
            .horizontal()
            .spacing(10.)
            .child(Button::new().on_press(choose).child(crate::i18n::tr("Choose folder")))
            .maybe_child(folder.map(|_| Button::new().on_press(clear).child(crate::i18n::tr("Use none")))),
    );
    row.into()
}

/// An on/off preference that can be set from the Settings screen.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Preference {
    ReducedMotion,
    Relay,
    Background,
    CollapseToIcons,
    AutoSave,
    SendInstantly,
    TwoFingerSettings,
}

impl Preference {
    /// The edit that sets this preference to `on`, and changes nothing else, except for the relays: see
    /// [`view::relay_patch`].
    fn patch(self, on: bool) -> SettingsPatch {
        let mut patch = SettingsPatch::default();
        match self {
            Preference::ReducedMotion => patch.reduced_motion = Some(on),
            Preference::Relay => return view::relay_patch(on),
            Preference::Background => patch.background = Some(on),
            Preference::CollapseToIcons => patch.collapse_to_icons = Some(on),
            Preference::AutoSave => patch.auto_save = Some(on),
            Preference::SendInstantly => patch.send_instantly = Some(on),
            Preference::TwoFingerSettings => patch.two_finger_settings = Some(on),
        }
        patch
    }
}

/// One on/off preference. The switch is its own state; when the person flips it the change is
/// sent to the owner, and the saved value that comes back is what is shown afterwards.
#[derive(PartialEq)]
struct ToggleRow {
    link: Link,
    palette: Palette,
    text: &'static str,
    note: &'static str,
    on: bool,
    which: Preference,
}

impl Component for ToggleRow {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        // Controlled: the switch always shows the value the owner holds, and flipping it only
        // asks the owner to change it. Nothing is remembered here, so flipping it back and
        // forth always sends each change, and a refusal leaves the switch where it was.
        let (link, which) = (self.link.clone(), self.which);
        rect()
            .width(Size::fill())
            .padding(16.)
            .horizontal()
            .cross_align(Alignment::center())
            .content(Content::flex())
            .corner_radius(12.)
            .background(palette.surface)
            .border(Border::new().fill(palette.border).width(1.).alignment(BorderAlignment::Inner))
            .child(
                rect()
                    .width(Size::flex(1.))
                    .child(label().text(i18n::tr(self.text)).color(palette.text).font_size(16.).font_weight(FontWeight::BOLD))
                    .child(label().text(i18n::tr(self.note)).color(palette.muted).font_size(13.)),
            )
            .child(Switch::new(WritableSetter::new(self.on, move |value: bool| {
                link.send(Command::PatchSettings { patch: which.patch(value) });
            })))
    }
}

/// The state of the platform's notifications and the way to allow them. Asked of the platform each time it is drawn, so that coming back
/// from the system's settings shows what the person chose there.
#[derive(PartialEq)]
struct NotificationsRow {
    control: link::NotificationsControl,
    palette: Palette,
}

impl Component for NotificationsRow {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let allowed = (self.control.allowed)();
        let open = self.control.open.clone();
        rect()
            .width(Size::fill())
            .padding(16.)
            .horizontal()
            .cross_align(Alignment::center())
            .content(Content::flex())
            .corner_radius(12.)
            .background(palette.surface)
            .border(Border::new().fill(palette.border).width(1.).alignment(BorderAlignment::Inner))
            .child(
                rect()
                    .width(Size::flex(1.))
                    .child(label().text(i18n::tr("Notifications")).color(palette.text).font_size(16.).font_weight(FontWeight::BOLD))
                    .child(label().text(i18n::tr(if allowed {
                        "On: an offer of a file is shown even when the program is closed."
                    } else {
                        "Blocked: an offer of a file is not shown until you allow notifications."
                    })).color(palette.muted).font_size(13.)),
            )
            .child(Button::new().on_press(move |_| open()).child(i18n::tr(if allowed { "Settings" } else { "Allow" })))
    }
}

/// The permission to draw the panel of an offer over other applications, and the way to the screen of the system where the person gives it. Read
/// again while the row is on the screen, because the person gives it on a screen of the system and comes back to the answer already given.
#[derive(PartialEq)]
struct OverlayRow {
    control: link::OverlayControl,
    palette: Palette,
}

impl Component for OverlayRow {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let control = self.control.clone();
        let mut allowed: State<bool> = use_state(|| (control.allowed)());
        use_hook({
            let control = control.clone();
            move || {
                let mut ticks = link::clock(unix_now, std::time::Duration::from_millis(500));
                spawn(async move {
                    loop {
                        allowed.set_if_modified((control.allowed)());
                        if ticks.changed().await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        let on = *allowed.read();
        let open = self.control.open.clone();
        rect()
            .width(Size::fill())
            .padding(16.)
            .horizontal()
            .cross_align(Alignment::center())
            .content(Content::flex())
            .corner_radius(12.)
            .background(palette.surface)
            .border(Border::new().fill(palette.border).width(1.).alignment(BorderAlignment::Inner))
            .child(
                rect()
                    .width(Size::flex(1.))
                    .child(label().text(i18n::tr("Offers over other apps")).color(palette.text).font_size(16.).font_weight(FontWeight::BOLD))
                    .child(label().text(i18n::tr(if on {
                        "On: the list of the files of an offer opens over what you are doing, and Accept does not open Peersus."
                    } else {
                        "Off: the list of the files of an offer opens in Peersus. Allow it to see the list over other apps."
                    })).color(palette.muted).font_size(13.)),
            )
            .child(Button::new().on_press(move |_| open()).child(i18n::tr(if on { "Settings" } else { "Allow" })))
    }
}

#[derive(PartialEq)]
struct ClipboardRow { control: link::ClipboardControl, palette: Palette }

impl Component for ClipboardRow {
    fn render(&self) -> impl IntoElement {
        let control = self.control.clone();
        let mut status = use_state(|| (control.status)());
        use_hook(move || {
            let mut ticks = link::clock(unix_now, std::time::Duration::from_millis(500));
            spawn(async move {
                loop {
                    status.set_if_modified((control.status)());
                    if ticks.changed().await.is_err() { break; }
                }
            });
        });
        let palette = self.palette;
        let open = self.control.open.clone();
        rect().width(Size::fill()).padding(16.).spacing(8.).corner_radius(12.)
            .background(palette.surface)
            .border(Border::new().fill(palette.border).width(1.).alignment(BorderAlignment::Inner))
            .child(label().text(i18n::tr("Background clipboard access")).color(palette.text).font_size(16.).font_weight(FontWeight::BOLD))
            .child(label().text(status.read().clone()).color(palette.muted).font_size(13.))
            .child(Button::new().on_press(move |_| open()).child(i18n::tr("Settings")))
    }
}

/// The folder that incoming files are saved in on their own, on a phone: where the folder is not a path, so it is not a setting of the program but
/// of the platform, which keeps it and is asked. Read again while the row is on the screen, because the system's picker answers to the platform
/// and not to the program, and the person comes back to the row with the answer already given.
#[derive(PartialEq)]
struct SavedFolderRow {
    sink: link::FolderSink,
    palette: Palette,
}

impl Component for SavedFolderRow {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let sink = self.sink.clone();
        let mut tree: State<Option<String>> = use_state(|| (sink.saved)());
        use_hook({
            let sink = sink.clone();
            move || {
                let mut ticks = link::clock(unix_now, std::time::Duration::from_millis(500));
                spawn(async move {
                    loop {
                        tree.set_if_modified((sink.saved)());
                        if ticks.changed().await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        // The address is the system's own text and says nothing a person can read, so what is shown is whether a folder is chosen; the system's
        // picker shows its name when it is opened.
        let chosen = tree.read().is_some();
        let (choose, forget) = (sink.choose_saved.clone(), sink.forget_saved.clone());
        let mut row = rect().width(Size::fill()).spacing(8.).child(mono(
            i18n::tr(if chosen { "A folder is chosen. Files go straight into it." } else { "No folder chosen. Every file asks where it goes." }),
            palette.text,
            12.,
        ));
        row = row.child(
            rect()
                .horizontal()
                .spacing(10.)
                .child(Button::new().on_press(move |_| choose()).child(i18n::tr("Choose folder")))
                .maybe_child(chosen.then(|| {
                    let mut tree = tree;
                    Button::new()
                        .on_press(move |_| {
                            forget();
                            tree.set(None);
                        })
                        .child(i18n::tr("Use none"))
                })),
        );
        row
    }
}

/// What a line of the list of back cameras says, said in words: a wide lens (short focal length), the main one, or a long one, with the
/// nearest it can focus on, because those two numbers are what tells which camera sees a screen held at a hand's length.
fn camera_words(line: &str) -> Option<(String, String, bool)> {
    let mut parts = line.split('\t');
    let id = parts.next()?.to_owned();
    let tenths: u32 = parts.next()?.parse().ok()?;
    let centimetres: u32 = parts.next()?.parse().ok()?;
    let chosen = parts.next()? == "1";
    let kind = match tenths {
        0 => "Camera",
        1..=39 => "Wide",
        40..=79 => "Main",
        _ => "Zoom",
    };
    let near = if centimetres == 0 { String::new() } else { format!(" · {centimetres} cm") };
    Some((id.clone(), format!("{} {id}{near}", i18n::tr(kind)), chosen))
}

/// The choice of the camera that reads a code: one button for each back camera of the phone, and one that leaves the choice to the program.
/// The camera that is open starts again on the one chosen, so the circle of scanning, in Peers, shows what that camera makes of a screen.
#[derive(PartialEq)]
struct CameraRow {
    link: Link,
    choice: link::CameraChoice,
    camera: Option<link::CameraControl>,
    room: f32,
    palette: Palette,
}

impl Component for CameraRow {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        // The list is asked once for each drawing, and a press is remembered here at once, so the buttons answer before the phone does.
        let mut picked: State<Option<String>> = use_state(|| None);
        let lines = (self.choice.list)();
        let cameras: Vec<(String, String, bool)> = lines.lines().filter_map(camera_words).collect();
        let current = picked.read().clone().unwrap_or_else(|| cameras.iter().find(|camera| camera.2).map(|camera| camera.0.clone()).unwrap_or_default());
        let button = |id: String, words: String| -> Element {
            let choose = self.choice.choose.clone();
            let selected = current == id;
            let target = id.clone();
            let button = Button::new().width(Size::fill()).on_press(move |_| {
                picked.set(Some(target.clone()));
                choose(&target);
            }).child(words);
            if selected { button.filled().into() } else { button.into() }
        };
        // A grid of two columns: every button fills a cell of the same width, and a last row of one leaves its second cell empty, so that
        // the buttons line up in columns and none runs off the edge of the card or breaks its words.
        let cell = |content: Option<Element>| -> Element {
            let mut cell = rect().width(Size::flex(1.));
            if let Some(content) = content {
                cell = cell.child(content);
            }
            cell.into()
        };
        let mut buttons: Vec<Element> = vec![button(String::new(), i18n::tr("Automatic").to_owned())];
        for (id, words, _) in cameras {
            buttons.push(button(id, words));
        }
        let mut row = rect().width(Size::fill()).spacing(8.);
        for pair in buttons.chunks(2) {
            row = row.child(
                rect()
                    .width(Size::fill())
                    .horizontal()
                    .content(Content::flex())
                    .spacing(8.)
                    .child(cell(Some(pair[0].clone())))
                    .child(cell(pair.get(1).cloned())),
            );
        }
        // The test: the camera is started here, in the row, and its picture is shown under the buttons, so that each camera is chosen by
        // looking at what it shows of a screen. It is the same scan as the circle of Peers (a code read here is joined all the same), and
        // it is closed when this page is left.
        let owner_said: State<Option<String>> = use_state(|| None);
        let CodeScan { looking, picture, open, stop } = use_code_scan(&self.link, self.camera.clone(), self.room, owner_said);
        let shown = picture.read().clone();
        let scanning = looking() == view::Looking::Scanning;
        let test: Element = match shown {
            Some(picture) => CameraCircle { picture, room: self.room, palette, on_stop: stop.clone() }.into(),
            None if scanning => label().text(i18n::tr("starting the camera")).color(palette.muted).font_size(14.).into(),
            None => {
                let open = open.clone();
                Button::new().width(Size::fill()).on_press(move |_| open.call(())).child(i18n::tr("Test the camera")).into()
            }
        };
        rect()
            .width(Size::fill())
            .padding(16.)
            .spacing(10.)
            .corner_radius(12.)
            .background(palette.surface)
            .border(Border::new().fill(palette.border).width(1.).alignment(BorderAlignment::Inner))
            .child(label().text(i18n::tr("Camera")).color(palette.text).font_size(16.).font_weight(FontWeight::BOLD))
            .child(label().text(i18n::tr("Choose the camera that reads a code, and press Test to see what it shows.")).color(palette.muted).font_size(13.))
            .child(row)
            .child(rect().width(Size::fill()).cross_align(Alignment::center()).child(test))
    }
}

impl Component for SettingsPage {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        let settings = &self.view.settings;
        let mut query = use_state(String::new);
        let typed = query.read().clone();
        let searching = !typed.trim().is_empty();
        let origin = use_state(|| None::<f32>);
        let requested = use_state(|| None::<settings_search::Target>);
        let selected = use_state(|| None::<settings_search::Target>);
        let go_to: State<Option<f32>> = use_state(|| None);
        let mut tap = use_state(view::Tap::default);
        let navigation = settings_navigation::Navigation { origin, requested, selected, go_to, tap };
        provide_context(navigation);
        let name = use_state(|| settings.device_name.clone());
        let save_link = self.link.clone();
        let save_name = move |_| {
            let value = name.read().trim().to_owned();
            if !value.is_empty() {
                save_link.send(Command::PatchSettings {
                    patch: SettingsPatch { device_name: Some(value), ..Default::default() },
                });
            }
        };
        // The picture is read and decoded off the window's thread, and a refusal is said in words under
        // the buttons. The owner is given the pixels, never the file or its path.
        let mut picture_refused: State<Option<&'static str>> = use_state(|| None);
        let choose_link = self.link.clone();
        let file_picker = self.file_picker.clone();
        let choose_picture = move |_| {
            let link = choose_link.clone();
            // A platform with no dialog of the toolkit's (Android) gives its own picker: one file comes back open, with
            // no path, and is decoded from where it is, under the same limits as a file on a disk. What a provider
            // hands over beyond the one asked for is let go of, or it would stay open in the registry for good.
            if let Some(picker) = file_picker.clone() {
                picker.picked.opened();
                (picker.open)(false);
                let mut said = link::await_picked(
                    picker.picked.clone(),
                    link::LOOK_EVERY,
                    link::PICKER_PATIENCE,
                );
                let picked = picker.picked.clone();
                spawn(async move {
                    while let Some(taken) = said.recv().await {
                        if let Some((used, rest)) = view::picture_choice(&taken.files) {
                            picked.release(&rest);
                            if let Some(file) = picked.take_file(used.0) {
                                match link.blocking(move || avatar_file::read_file(file)).await {
                                    Ok(Ok(picture)) => {
                                        picture_refused.set(None);
                                        let client = link.client().clone();
                                        let _ = client.command(Command::SetProfilePicture { picture }).await;
                                    }
                                    Ok(Err(refused)) => picture_refused.set(Some(refused.words())),
                                    Err(_) => picture_refused.set(Some("That picture could not be read.")),
                                }
                            }
                        }
                        if taken.refused > 0 {
                            picture_refused.set(Some("That picture could not be kept; try again."));
                        }
                        if !taken.still_open {
                            break;
                        }
                    }
                });
                return;
            }
            spawn(async move {
                #[cfg(not(target_os = "android"))]
                if let Some(file) = rfd::AsyncFileDialog::new()
                    .set_title("Choose a profile picture (PNG or JPEG)")
                    .pick_file()
                    .await
                {
                    let path = file.path().to_path_buf();
                    match link.blocking(move || avatar_file::read(&path)).await {
                        Ok(Ok(picture)) => {
                            picture_refused.set(None);
                            let client = link.client().clone();
                            let _ = client.command(Command::SetProfilePicture { picture }).await;
                        }
                        Ok(Err(refused)) => picture_refused.set(Some(refused.words())),
                        Err(_) => picture_refused.set(Some("That picture could not be read.")),
                    }
                }
                // Android always has a picker of its own (the branch above), so with none there is nothing to open.
                #[cfg(target_os = "android")]
                {
                    let _ = &link;
                    picture_refused.set(Some("This phone has no way to choose a picture here."));
                }
            });
        };
        let remove_link = self.link.clone();
        let remove_picture = move |_| {
            picture_refused.set(None);
            remove_link.send(Command::ClearProfilePicture {});
        };

        let appearance::Preview(mut theme_preview) = consume_context::<appearance::Preview>();
        let theme_button = |text: &'static str, value: ThemeChoice| -> Element {
            let link = self.link.clone();
            let selected = settings.theme == value;
            let button = Button::new()
                .on_press(move |_| {
                    theme_preview.set(None);
                    link.send(Command::PatchSettings {
                        patch: SettingsPatch { theme: Some(value), ..Default::default() },
                    })
                })
                .child(i18n::tr(text));
            if selected { button.filled().into() } else { button.into() }
        };

        let toggle = |text: &'static str, note: &'static str, on: bool, which: Preference| -> Element {
            ToggleRow { link: self.link.clone(), palette, text, note, on, which }.into()
        };

        let tab = self.tab;
        let card = |children: Vec<Element>| -> Element {
            let mut card = rect()
                .width(Size::fill())
                .padding(18.)
                .spacing(8.)
                .corner_radius(12.)
                .background(palette.surface)
                .border(Border::new().fill(palette.border).width(1.).alignment(BorderAlignment::Inner));
            for child in children {
                card = card.child(child);
            }
            card.into()
        };
        let fact = |name: &'static str, value: String| -> Element {
            rect()
                .width(Size::fill())
                .horizontal()
                .content(Content::flex())
                .child(label().text(i18n::tr(name)).color(palette.muted).font_size(14.))
                .child(rect().width(Size::flex(1.)))
                .child(label().text(value).color(palette.text).font_size(14.).font_weight(FontWeight::BOLD))
                .into()
        };

        let build = |which: SettingsTab| -> Element { match which {
            SettingsTab::Overview => {
                let moving = self.view.transfers.iter().filter(|t| t.stoppable()).count();
                let online = self.view.devices.iter().filter(|d| d.presence == Presence::Online).count();
                let running = self.view.plugins.iter().filter(|p| p.status == PluginStatus::Running).count();
                let totals = self.view.file_totals();
                rect()
                    .width(Size::fill())
                    .spacing(14.)
                    .child(section("HOW THINGS STAND", palette))
                    .child(card(vec![
                        fact("This device", settings.device_name.clone()),
                        fact("Appearance", i18n::tr(&format!("{:?}", settings.theme)).to_owned()),
                        fact("Internet relays", i18n::tr(if settings.network.allow_relay { "on" } else { "off" }).to_owned()),
                        fact("Runs in the background", i18n::tr(if settings.background { "yes" } else { "no" }).to_owned()),
                    ]))
                    .child(card(vec![
                        fact("Paired devices", i18n::format(i18n::language(), "{total} ({online} online)", &[("total", &self.view.devices.len().to_string()), ("online", &online.to_string())])),
                        fact("Transfers in progress", moving.to_string()),
                        fact("Extensions", i18n::format(i18n::language(), "{total} ({running} running)", &[("total", &self.view.plugins.len().to_string()), ("running", &running.to_string())])),
                    ]))
                    // What moved, said of the history the list still holds: Clear and forgetting a device take their lines out of it, and it keeps
                    // the thousand latest, so these are the files of that record and not of every day since the first.
                    .child(card(vec![
                        fact("Files sent", totals.sent.to_string()),
                        fact("Files received", totals.received.to_string()),
                        fact("Files in all", totals.total.to_string()),
                    ]))
                    .into()
            }
            SettingsTab::Device => rect()
                .width(Size::fill())
                .spacing(14.)
                .child(section("THIS DEVICE", palette))
                .child(
                    rect()
                        .width(Size::fill())
                        .horizontal()
                        .cross_align(Alignment::Center)
                        .content(Content::flex())
                        .spacing(16.)
                        // The picture is the button: pressing it opens the picker. A separate "Choose a picture"
                        // button did nothing a press on the picture does not, and took room that a phone lacks.
                        // The press is on this wrapper and not on `Avatar`, which every list of devices also draws.
                        .child(
                            rect()
                                .on_press(choose_picture.clone())
                                .child(avatar_view::Avatar {
                                    picture: self.picture.as_ref().map(|shared| shared.0.clone()),
                                    name: settings.device_name.clone(),
                                    size: 96.,
                                    palette,
                                }),
                        )
                        // The room between them takes what is left, which puts Remove at the right edge.
                        .child(rect().width(Size::flex(1.)))
                        .child(
                            Button::new()
                                .on_press(remove_picture.clone())
                                .enabled(self.picture.is_some()).child(crate::i18n::tr("Remove")),
                        ),
                )
                .children(picture_refused.read().iter().map(|words| {
                    label().text(i18n::tr(words)).color(palette.muted).font_size(13.)
                }))
                .child(if self.form.ask_stacks_field_and_button() {
                    rect()
                        .width(Size::fill())
                        .spacing(10.)
                        .child(Input::new(name).placeholder(crate::i18n::tr("Device name")).width(Size::fill()))
                        .child(Button::new().filled().on_press(save_name.clone()).width(Size::fill()).child(crate::i18n::tr("Save")))
                } else {
                    rect()
                        .horizontal()
                        .spacing(10.)
                        .child(Input::new(name).placeholder(crate::i18n::tr("Device name")).width(Size::px(360.)))
                        .child(Button::new().filled().on_press(save_name.clone()).child(crate::i18n::tr("Save")))
                })
                .child(language_picker::Picker {
                    link: self.link.clone(),
                    settings: settings.clone(),
                    palette,
                    // On a phone this page lies on the panel of the Settings overlay, whose colour is lifted; elsewhere on the page's own.
                    ground: if self.form == view::Form::Compact { palette.raised } else { palette.background },
                })
                .child(interface_size::Picker { link: self.link.clone(), settings: settings.clone(), palette })
                .child(toggle(
                    "Run in the background",
                    "Start without opening a window.",
                    settings.background,
                    Preference::Background,
                ))
                .maybe_child(cfg!(target_os = "android").then(|| {
                    toggle(
                        "Two fingers open Settings",
                        "Touch the screen with two fingers to bring Settings down from the top. It does nothing while Settings is already open.",
                        settings.two_finger_settings,
                        Preference::TwoFingerSettings,
                    )
                }))
                .maybe_child(self.notifications.clone().map(|control| -> Element { NotificationsRow { control, palette }.into() }))
                .maybe_child(try_consume_context::<link::OverlayControl>().map(|control| -> Element { OverlayRow { control, palette }.into() }))
                .maybe_child(try_consume_context::<link::ClipboardControl>().map(|control| -> Element { ClipboardRow { control, palette }.into() }))
                .maybe_child(self.camera_choice.clone().map(|choice| -> Element {
                    CameraRow { link: self.link.clone(), choice, camera: self.camera.clone(), room: self.room, palette }.into()
                }))
                .child(section("OUTGOING FILES", palette))
                .child(toggle(
                    "Send instantly",
                    "Off: after choosing where to send, you hold the circle until it fills, so a wrong choice costs nothing. On: touching the circle sends at once.",
                    settings.send_instantly,
                    Preference::SendInstantly,
                ))
                .child(section("INCOMING FILES", palette))
                .child(toggle(
                    "Save incoming files on their own",
                    "Off: you are asked where each file goes. On: files go straight to the folder below, without asking.",
                    settings.auto_save,
                    Preference::AutoSave,
                ))
                .child(match try_consume_context::<link::FolderSink>() {
                    Some(sink) => SavedFolderRow { sink, palette }.into(),
                    None => save_folder_row(&self.link, settings.save_folder.as_deref(), palette),
                })
                .into(),
            SettingsTab::Appearance => rect()
                .width(Size::fill())
                .spacing(14.)
                .child(section("THEME", palette))
                .child(
                    rect()
                        .horizontal()
                        .spacing(10.)
                        .child(theme_button("System", ThemeChoice::System))
                        .child(theme_button("Light", ThemeChoice::Light))
                        .child(theme_button("Dark", ThemeChoice::Dark))
                        .child(theme_button("Custom", ThemeChoice::Custom)),
                )
                .child(accent_picker::Picker { link: self.link.clone(), settings: settings.clone(), palette })
                .child(appearance::Editor { link: self.link.clone(), settings: settings.clone(), palette })
                .child(toggle(
                    "Keep icons when the bar is closed",
                    "A closed bar shows only its pictures. Off, a closed bar is not shown at all.",
                    settings.collapse_to_icons,
                    Preference::CollapseToIcons,
                ))
                .child(toggle(
                    "Reduce motion",
                    "Turn off decorative animation.",
                    settings.reduced_motion,
                    Preference::ReducedMotion,
                ))
                .into(),
            SettingsTab::Network => rect()
                .width(Size::fill())
                .spacing(14.)
                .child(section("CONNECTIONS", palette))
                .child(toggle(
                    "Use internet relays",
                    "Lets devices connect across networks. Off keeps traffic on this network.",
                    settings.network.allow_relay,
                    Preference::Relay,
                ))
                .into(),
            SettingsTab::Extensions => rect()
                .width(Size::fill())
                .spacing(14.)
                // The headings are drawn inside the cards component, because the store and the installed list
                // share one removal confirmation and so one component.
                .child(plugin_cards(&self.link, &self.view, &self.pictures, palette, self.form == view::Form::Compact))
                .into(),
            SettingsTab::Permissions => {
                let mut page = rect().width(Size::fill()).spacing(14.).child(section("WHAT EACH PLUGIN MAY DO", palette));
                for plugin in &self.view.plugins {
                    let name = String::from(plugin.id.clone());
                    let mut body: Vec<Element> =
                        vec![label().text(name).color(palette.text).font_size(18.).font_weight(FontWeight::BOLD).into()];
                    let nothing = plugin.grantable.per_device.is_empty() && plugin.grantable.outright.is_empty();
                    if nothing {
                        body.push(label().text(crate::i18n::tr("It cannot be granted anything.")).color(palette.muted).font_size(14.).into());
                    }
                    // What can be allowed once, for no device in particular.
                    if !plugin.grantable.outright.is_empty() {
                        let held = self.view.grants.get(&plugin.id).cloned().unwrap_or_default();
                        body.push(mono(crate::i18n::tr("NOT TIED TO ANY DEVICE"), palette.muted, 11.).into());
                        body.push(permission_block(&self.link, plugin, &plugin.grantable.outright, &held, None, palette));
                    }
                    // What is allowed for one paired device at a time: one block for each device.
                    if !plugin.grantable.per_device.is_empty() {
                        if self.view.devices.is_empty() {
                            body.push(label().text(crate::i18n::tr("Pair a device to allow this plugin anything for it.")).color(palette.muted).font_size(14.).into());
                        }
                        for device in &self.view.devices {
                            let held = device.grants.get(&plugin.id).cloned().unwrap_or_default();
                            body.push(mono(format!("FOR {}", device.name.to_uppercase()), palette.muted, 11.).into());
                            body.push(permission_block(
                                &self.link,
                                plugin,
                                &plugin.grantable.per_device,
                                &held,
                                Some(peersus_core::model::PeerId(device.id.clone())),
                                palette,
                            ));
                        }
                    }
                    page = page.child(settings_navigation::anchor(settings_search::Target::PluginPermissions(plugin.id.clone()), card(body), palette));
                }
                if self.view.plugins.is_empty() {
                    page = page.child(label().text(crate::i18n::tr("No plugins installed, so nothing to allow.")).color(palette.muted).font_size(16.));
                }
                page.into()
            }
            SettingsTab::About => {
                let mut page = rect().width(Size::fill()).spacing(14.).child(card(vec![
                    label().text("peersus").color(palette.text).font_size(20.).font_weight(FontWeight::BOLD).into(),
                    label().text(i18n::tr("Move it. Don't store it.")).color(palette.text).font_size(15.).into(),
                    label()
                        .text(i18n::tr("Private connections between your own devices. Files go directly from one device to another, only after the receiving person approves, and no cloud folder is involved."))
                        .color(palette.muted)
                        .font_size(13.)
                        .into(),
                    mono(
                        format!("VERSION {} \u{b7} {}", env!("CARGO_PKG_VERSION"), about::APPLICATION_LICENCE),
                        palette.muted,
                        12.,
                    )
                    .into(),
                ]));
                let mut disclaimer = rect()
                    .width(Size::fill())
                    .padding(18.)
                    .spacing(12.)
                    .corner_radius(14.)
                    .background(palette.warning_background)
                    .child(label().text(crate::i18n::tr("IMPORTANT — DATA AND SECURITY RISKS"))
                        .color(palette.warning).font_size(15.).font_weight(FontWeight::BOLD));
                for paragraph in about::DISCLAIMER {
                    disclaimer = disclaimer.child(label().text(i18n::tr(paragraph)).color(palette.warning).font_size(14.));
                }
                page = page.child(disclaimer);
                page = page.child(support::Support { palette });
                // The notice the licence asks every redistributor to keep, as the file has it.
                page = page.child(section("NOTICE", palette)).child(card(vec![label()
                    .text(about::PROJECT_NOTICE.trim_end())
                    .color(palette.muted)
                    .font_size(13.)
                    .into()]));
                for section_of_credits in about::SECTIONS {
                    page = page.child(section(section_of_credits.title, palette));
                    let mut rows = Vec::new();
                    for credit in section_of_credits.credits {
                        rows.push(
                            rect()
                                .width(Size::fill())
                                .spacing(2.)
                                .child(
                                    rect()
                                        .width(Size::fill())
                                        .horizontal()
                                        .content(Content::flex())
                                        .child(label().text(credit.name).color(palette.text).font_size(15.).font_weight(FontWeight::BOLD))
                                        .child(rect().width(Size::flex(1.)))
                                        .child(mono(credit.licence, palette.muted, 11.)),
                                )
                                .child(label().text(i18n::tr(credit.purpose)).color(palette.muted).font_size(13.))
                                .child(
                                    rect()
                                        .on_press({
                                            let url = credit.url;
                                            move |_| Platform::get().open_url(url)
                                        })
                                        .child(mono(credit.url, palette.receive, 11.)),
                                )
                                .into(),
                        );
                    }
                    page = page.child(card(rows));
                }
                page = page.child(section("NOTICES", palette));
                let notices = about::NOTICES
                    .iter()
                    .map(|notice| label().text(*notice).color(palette.muted).font_size(13.).into())
                    .collect::<Vec<Element>>();
                page.child(card(notices)).into()
            }
        } };
        // Search replaces the page content; it never creates a second scroll area in the header.
        let found = settings_search::search(&typed, &self.view, i18n::language(), settings_search::Features {
            notifications: self.notifications.is_some(), camera: self.camera_choice.is_some(),
            overlay: try_consume_context::<link::OverlayControl>().is_some(),
            clipboard: try_consume_context::<link::ClipboardControl>().is_some(),
        });
        let mut results = rect().width(Size::fill()).spacing(10.);
        for entry in &found {
            let mut tab = self.tab;
            let mut navigation = navigation;
            results = results.child(settings_navigation::ResultButton {
                entry: entry.clone(), palette,
                on_select: EventHandler::new(move |target: settings_search::Target| {
                    query.set(String::new());
                    tab.set(target.tab());
                    navigation.selected.set(Some(target.clone()));
                    navigation.requested.set(Some(target));
                }),
            });
        }
        if searching && found.is_empty() {
            results = results.child(label().text(i18n::tr("Nothing in Settings looks like that.")).color(palette.muted).font_size(14.));
        }
        // Where the container of the groups and each group's title are laid out, in the window's own coordinates: a group's top in what is scrolled is
        // the difference of the two, which does not change as the page moves under the window. `None` until it was laid out.
        let title_tops: State<[Option<f32>; 7]> = use_state(|| [None; 7]);
        let tops: State<Vec<f32>> = use_state(Vec::new);
        let content: Element = if searching {
            results.into()
        } else if self.form == view::Form::Compact {
            let mut page = rect().width(Size::fill()).spacing(28.).on_sized({
                let mut origin = origin;
                move |e: Event<SizedEventData>| origin.set_if_modified(Some(e.area.min_y()))
            });
            for (index, which) in SettingsTab::ALL.into_iter().enumerate() {
                page = page
                    .child(settings_navigation::anchor(settings_search::Target::Group(which),
                        rect()
                            .width(Size::fill())
                            .horizontal()
                            .main_align(Alignment::Center)
                            .on_sized({
                                let mut title_tops = title_tops;
                                move |e: Event<SizedEventData>| {
                                    let mut next = *title_tops.peek();
                                    next[index] = Some(e.area.min_y());
                                    title_tops.set_if_modified(next);
                                }
                            })
                            .child(label().text(i18n::tr(which.title())).color(palette.text).font_size(26.).font_weight(FontWeight::BOLD)),
                        palette))
                    .child(build(which));
            }
            page.into()
        } else {
            rect().width(Size::fill()).on_sized({
                let mut origin = origin;
                move |e: Event<SizedEventData>| origin.set_if_modified(Some(e.area.min_y()))
            }).child(settings_navigation::anchor(settings_search::Target::Group(tab()), build(tab()), palette)).into()
        };
        // The tops of the groups once the container and every title have been laid out.
        if let (Some(origin), titles) = (*origin.peek(), *title_tops.read())
            && titles.iter().all(Option::is_some)
        {
            let first = titles[0].unwrap_or(origin);
            let measured: Vec<f32> = titles.iter().flatten().map(|top| (top - first).max(0.)).collect();
            let mut tops = tops;
            tops.set_if_modified(measured);
        }

        // The fixed header owns only title/input; the page has one scrolling body.
        let compact = self.form == view::Form::Compact;
        let narrow_header = compact && self.room < 320.;
        let title = label().text(i18n::tr("Settings")).color(palette.text)
            .font_size(if compact { 20. } else { 32. }).font_weight(FontWeight::BOLD).max_lines(1);
        let mut controls = rect().width(Size::fill()).horizontal().content(Content::flex())
            .cross_align(Alignment::center()).spacing(if compact { 10. } else { 20. });
        if !narrow_header { controls = controls.child(title.clone()); }
        controls = controls.child(rect().width(Size::flex(1.)).child(
            Input::new(query).placeholder(i18n::tr("Search settings")).width(Size::fill()),
        )).maybe_child(self.on_close.clone().map(|close| {
            Button::new().on_press(move |_| close.call(())).child(label().text(i18n::tr("Close")).max_lines(1))
        }));
        let mut header = rect().width(Size::fill()).spacing(8.)
            .padding((12., self.form.page_side_margin()));
        if narrow_header { header = header.child(title); }
        let header = header.child(controls);
        let scroll = Scrolling {
            palette, form: self.form, content, see_through: compact,
            tops: (compact && !searching).then_some(tops), go_to: Some(go_to),
        };
        rect()
            .expanded()
            .content(Content::flex())
            .background(if compact { palette.raised } else { palette.background })
            .on_capture_global_pointer_move(move |event: Event<PointerEventData>| {
                let point = event.global_location();
                let mut next = *tap.peek();
                next.moved((point.x as f32, point.y as f32));
                tap.set_if_modified(next);
            })
            .on_touch_cancel(move |_| tap.set(view::Tap::default()))
            .child(header)
            .child(rect().width(Size::fill()).height(Size::px(1.)).background(palette.border))
            .child(rect().key(if searching { "settings-search-results" } else { "settings-content" })
                .width(Size::fill()).height(Size::flex(1.)).child(scroll))
            .into_element()
    }
}
