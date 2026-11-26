//! The panel that announces an offer, or a notification that reaches outside, outside the window, on a desktop (Linux).
//!
//! The owner starts this program with `peersus_app::offer_panel::ARGUMENT` while an offer waits and no window is in
//! front, and stops it when that is no longer so: this never decides when it is shown. It is the window's own bar of an
//! offer ([`crate::IncomingOffer`]) in a window of its own: the same words, the same answers, the same commands, in the
//! theme and the language the person chose. It attaches beside the windows (see [`crate::link::Session::beside`]), so it
//! is never counted as one.
//!
//! The window is one the window manager does not manage (override-redirect): it takes no focus, has no frame, stands over
//! what the person is doing, and is placed by this program at the edge the person chose
//! ([`crate::view::panel_origin`]). It is as tall as what it shows, and is moved when that changes, so that it stays at
//! its edge. Where the screen keeps a bar of its own at an edge the panel stands clear of it by a fixed margin: the
//! desktop's free area is not asked for.
//!
//! It can be carried: held anywhere and moved, it follows the pointer, and stays where it was put for as long as it is
//! up (growing or shrinking from there) unless another edge is chosen in Settings. Nothing of that is saved: the next
//! announcement stands at the chosen edge again. A window nobody manages cannot ask the desktop to move it, so this moves
//! it itself, by how far the pointer has gone from where it took hold.

use std::{rc::Rc, sync::Arc};

use freya::{
    prelude::*,
    winit::{
        dpi::{PhysicalPosition, PhysicalSize},
        monitor::MonitorHandle,
        platform::x11::{WindowAttributesX11, WindowType},
        window::{Window, WindowLevel},
    },
};
use peersus_core::config::{NoticePlace, Theme as ThemeChoice};
use x11rb::{protocol::randr::ConnectionExt, rust_connection::RustConnection};

use crate::{IncomingOffer, View, font, i18n, link, link::Session, theme, unix_now, view};

/// The panel's width, in the toolkit's logical units: room for the three answers side by side in the longest language.
const WIDTH: f64 = 440.;
/// The height the window opens with, before what it shows has been measured.
const FIRST_HEIGHT: f64 = 150.;
/// What is kept clear between the panel and the top or the bottom of the screen: more than a desktop's own bar is tall.
const CLEAR_OF_A_BAR: f64 = 56.;
/// What is kept clear between the panel and a side of the screen.
const CLEAR_OF_A_SIDE: f64 = 16.;
/// How round the panel's corners are: it stands on its own, with the desktop showing around it.
const CORNER: f32 = 16.;
/// How far the pointer must go, held, before the panel is carried: less is a press on one of its buttons with an unsteady hand.
const CARRIED_FROM: f64 = 4.;

fn margin(place: NoticePlace, scale: f64) -> u32 {
    let clear = match place {
        NoticePlace::Top | NoticePlace::Bottom => CLEAR_OF_A_BAR,
        NoticePlace::Left | NoticePlace::Right => CLEAR_OF_A_SIDE,
    };
    (clear * scale).round() as u32
}

fn pixels(logical: f64, scale: f64) -> u32 {
    (logical * scale).round().max(1.) as u32
}

/// Where a panel of this size stands if the monitor's geometry is available.
fn origin(
    place: NoticePlace,
    monitor: &MonitorHandle,
    size: PhysicalSize<u32>,
    display: Option<&RustConnection>,
) -> Option<PhysicalPosition<i32>> {
    // Winit 0.31's video mode measures the unrotated mode. The CRTC bounds
    // are in desktop coordinates, just as Winit 0.30's monitor size was.
    let (corner, extent) = display
        .and_then(|display| {
            let id = u32::try_from(monitor.native_id())
                .ok()
                .filter(|id| *id != 0)?;
            let bounds = display
                .randr_get_crtc_info(id, x11rb::CURRENT_TIME)
                .ok()?
                .reply()
                .ok()?;
            (bounds.width != 0 && bounds.height != 0).then_some((
                PhysicalPosition::new(i32::from(bounds.x), i32::from(bounds.y)),
                PhysicalSize::new(u32::from(bounds.width), u32::from(bounds.height)),
            ))
        })
        .or_else(|| Some((monitor.position()?, monitor.current_video_mode()?.size())))?;
    let (x, y) = view::panel_origin(
        place,
        ((corner.x, corner.y), (extent.width, extent.height)),
        (size.width, size.height),
        margin(place, monitor.scale_factor()),
    );
    Some(PhysicalPosition::new(x, y))
}

/// Make the window as tall as what it shows and, when `place` is given, put it at that edge; with none it stays where it is.
fn stand(
    window: &dyn Window,
    place: Option<NoticePlace>,
    height: f32,
    display: Option<&RustConnection>,
) {
    let scale = window.scale_factor();
    let size = PhysicalSize::new(pixels(WIDTH, scale), pixels(f64::from(height), scale));
    let _ = window.request_surface_size(size.into());
    if let Some(place) = place
        && let Some(position) = window
            .primary_monitor()
            .or_else(|| window.current_monitor())
            .or_else(|| window.available_monitors().next())
            .and_then(|monitor| origin(place, &monitor, size, display))
    {
        window.set_outer_position(position.into());
    }
}

/// Move the window by how far the pointer has gone, in the toolkit's logical units, from where it took hold of it. The
/// pointer is told relative to the window, so once the window has followed, the distance is nothing again.
fn carry(window: &dyn Window, across: f64, down: f64) {
    let scale = window.scale_factor();
    if let Ok(at) = window.outer_position() {
        window.set_outer_position(
            PhysicalPosition::new(
                at.x.saturating_add((across * scale).round() as i32),
                at.y.saturating_add((down * scale).round() as i32),
            )
            .into(),
        );
    }
}

/// The launch configuration of the panel. `Err` when there is no owner to show an offer of.
pub(crate) fn config() -> Result<LaunchConfig, &'static str> {
    let session = Session::beside()?;
    let (place, renderer) = session
        .link
        .as_ref()
        .map(|link| {
            let snapshot = link.client().snapshot();
            (snapshot.settings.notice_place, snapshot.settings.renderer)
        })
        .unwrap_or_default();
    let display = x11rb::connect(None)
        .ok()
        .map(|(display, _)| Rc::new(display));
    let panel_display = display.clone();
    Ok(LaunchConfig::new()
        .with_global_context(crate::edit_labels())
        .with_font(font::FAMILY, font::BYTES)
        .with_default_font(font::FAMILY)
        .with_window(
            WindowConfig::new(move || Panel {
                session: session.clone(),
                display: panel_display.clone(),
            })
            .with_title("Peersus")
            .with_renderer(crate::renderer_settings::preference(renderer))
            .with_size(WIDTH, FIRST_HEIGHT)
            .with_decorations(false)
            .with_transparency(true)
            .with_background(Color::TRANSPARENT)
            .with_resizable(false)
            .with_window_attributes(move |attributes, event_loop| {
                let attributes = attributes
                    .with_platform_attributes(Box::new(
                        WindowAttributesX11::default()
                            .with_override_redirect(true)
                            .with_x11_window_type(vec![WindowType::Notification]),
                    ))
                    .with_window_level(WindowLevel::AlwaysOnTop)
                    .with_active(false);
                // Where it will stand, before it is first shown: a window that is not managed is shown where it is made.
                let position = event_loop
                    .primary_monitor()
                    .or_else(|| event_loop.available_monitors().next())
                    .and_then(|monitor| {
                        let scale = monitor.scale_factor();
                        let size =
                            PhysicalSize::new(pixels(WIDTH, scale), pixels(FIRST_HEIGHT, scale));
                        origin(place, &monitor, size, display.as_deref())
                    });
                match position {
                    Some(position) => attributes.with_position(position),
                    None => attributes,
                }
            }),
        ))
}

struct Panel {
    session: Arc<Session>,
    display: Option<Rc<RustConnection>>,
}

impl PartialEq for Panel {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.session, &other.session)
    }
}

impl Component for Panel {
    fn render(&self) -> impl IntoElement {
        let Ok(link) = &self.session.link else {
            return Element::from(rect());
        };
        let first = link.client().snapshot();
        let mut choice = use_state(|| first.settings.theme);
        let mut accent = use_state(|| first.settings.accent);
        let mut custom = use_state(|| first.settings.custom_theme.clone());
        let mut language = use_state(|| i18n::shown(&first.languages));
        let mut place = use_state(|| first.settings.notice_place);
        let mut offer: State<Option<view::Offered>> =
            use_state(|| view::offered_now(&View::of(&first)));
        let mut cards: State<Vec<view::NotificationCard>> =
            use_state(|| view::outside_cards(&first));
        provide_context(i18n::Current(language));
        // The owner's state, followed for as long as there is an owner. When it has gone there is nothing to show and nobody to
        // stop this program, so it ends itself.
        use_hook({
            let client = link.client().clone();
            move || {
                spawn(async move {
                    let mut updates = client.subscribe();
                    loop {
                        let latest = updates.borrow_and_update().clone();
                        choice.set_if_modified(latest.settings.theme);
                        accent.set_if_modified(latest.settings.accent);
                        custom.set_if_modified(latest.settings.custom_theme.clone());
                        language.set_if_modified(i18n::follow(&client, &latest.languages).await);
                        place.set_if_modified(latest.settings.notice_place);
                        offer.set_if_modified(view::offered_now(&View::of(&latest)));
                        cards.set_if_modified(view::outside_cards(&latest));
                        if updates.changed().await.is_err() {
                            Platform::get().exit();
                            break;
                        }
                    }
                });
            }
        });
        let mut system_dark = use_state(|| *self.session.system_theme.borrow());
        let mut system_updates = self.session.system_theme.clone();
        use_hook(move || {
            spawn(async move {
                loop {
                    system_dark.set_if_modified(*system_updates.borrow_and_update());
                    if system_updates.changed().await.is_err() {
                        break;
                    }
                }
            })
        });
        let platform_dark = system_dark().unwrap_or(
            *Platform::get().current_window().preferred_theme.read() == PreferredTheme::Dark,
        );
        let own = (choice() == ThemeChoice::Custom).then(|| custom.read().clone());
        let (palette, dark) = crate::appearance_of(choice(), own.as_ref(), platform_dark, accent());
        let mut toolkit = use_init_theme(move || theme::theme(palette, dark));
        toolkit.set_if_modified(theme::theme(palette, dark));
        // The second now, for the time the offer has left: the same clock the window has.
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
        // As tall as what is shown, at the edge the person chose: again whenever either changes (the list of files opened,
        // another edge chosen in Settings while the panel is up). A panel the person carried somewhere stays there, and only
        // changes its height, until another edge is chosen.
        let mut height = use_state(|| 0_f32);
        let mut carried = use_state(|| false);
        let mut stood_at: State<Option<NoticePlace>> = use_state(|| None);
        let display = self.display.clone();
        use_side_effect_with_deps(&(height(), place()), move |&(height, place)| {
            if height <= 0. {
                return;
            }
            if *stood_at.peek() != Some(place) {
                stood_at.set(Some(place));
                carried.set_if_modified(false);
            }
            let to = (!*carried.peek()).then_some(place);
            let display = display.clone();
            Platform::get().with_window(Platform::window_id(), move |window| {
                stand(window, to, height, display.as_deref());
            });
        });
        // Where the pointer took hold of the panel, while its button is down; and whether that hold has become a carry, which the
        // buttons of the offer ask before they act: letting go at the end of a carry is not a press of what was under the pointer.
        let mut held_at: State<Option<CursorPoint>> = use_state(|| None);
        let mut moving = use_state(|| false);
        let carrying = crate::Carrying(moving);
        provide_context(carrying);
        let shown = offer.read().clone();
        let posted = cards.read().clone();
        rect()
            .spacing(8.)
            .width(Size::fill())
            .height(Size::auto())
            .on_sized(move |e: Event<SizedEventData>| height.set_if_modified(e.area.height()))
            .on_pointer_down(move |e: Event<PointerEventData>| {
                if matches!(&*e, PointerEventData::Mouse(mouse) if mouse.button == Some(MouseButton::Left)) {
                    held_at.set(Some(e.global_location()));
                    moving.set_if_modified(false);
                }
            })
            .on_global_pointer_move(move |e: Event<PointerEventData>| {
                let Some(from) = *held_at.peek() else {
                    return;
                };
                let now = e.global_location();
                let (across, down) = (now.x - from.x, now.y - from.y);
                if !*moving.peek() && across.abs().max(down.abs()) < CARRIED_FROM {
                    return;
                }
                moving.set_if_modified(true);
                carried.set_if_modified(true);
                Platform::get().with_window(Platform::window_id(), move |window| {
                    carry(window, across, down);
                });
            })
            .on_global_pointer_up(move |_| held_at.set_if_modified(None))
            .maybe_child(shown.map(|offered| IncomingOffer {
                link: link.clone(),
                offered,
                palette,
                now: now(),
                buttons: None,
                corner: CORNER,
            }))
            .children(
                posted
                    .iter()
                    .map(|card| crate::posted_card(link, palette, card, Some(carrying)).into_element()),
            )
            .into()
    }
}
