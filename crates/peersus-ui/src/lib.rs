//! The Peersus interface: one library shared by the desktop entry and the Android one. `view`
//! holds every rule about what a person is told, `link` the connection to the owner, `theme`
//! the palettes and `qr` the pairing picture; the components here only draw. The Android entry
//! (which needs `unsafe`) lives in its own platform crate, never here.
pub mod about;
pub mod accent_picker;
pub mod agreement_flow;
mod agreement_overlay;
mod appearance;
pub mod avatar;
pub mod avatar_file;
pub mod avatar_view;
mod back;
mod binds_page;
mod canvas_view;
pub mod chords;
pub mod clipboard_files;
mod contributed_page;
/// The read-only demo the `demo` feature makes of the same screens. Absent in a product build.
#[cfg(all(feature = "demo", not(target_os = "android")))]
pub mod demo;
/// Owners that live only in this process: an extension shown as it really runs in a demo, and the window's own tests.
#[cfg(all(any(feature = "demo", test), not(target_os = "android")))]
mod demo_owner;
pub mod field;
mod follow;
pub mod font;
mod foreground;
pub mod glyph;
/// The demo's window with no display behind it, for automated screen tests and frame timings.
#[cfg(all(feature = "headless", not(target_os = "android")))]
pub mod headless;
pub mod i18n;
pub mod icon;
pub mod intake;
pub mod interface_size;
pub mod language_picker;
mod languages_page;
#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod launch_env;
mod layers;
mod layout_regression;
#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[doc(hidden)]
pub use launch_env::prepare as prepare_window_allocator;
pub mod link;
pub mod motion;
/// The panel that announces an offer outside the window, where the desktop has one.
#[cfg(target_os = "linux")]
mod offer_window;
pub mod outgoing;
/// The bindings asked of the desktop while the window is not in front.
#[cfg(target_os = "linux")]
mod outside_chords;
mod picture_view;
mod plugin_controls;
pub mod preview;
#[cfg(feature = "private-store")]
mod private_store;
/// The demo window measuring itself (`--demo-probe`). Absent in a product build.
#[cfg(all(feature = "demo", not(target_os = "android")))]
pub mod probe;
pub mod qr;
pub mod qr_reader;
mod relay_choice;
mod render_budget;
#[cfg(any(not(target_os = "android"), feature = "renderer-probe"))]
mod renderer_bench;
mod renderer_settings;
#[cfg(any(not(target_os = "android"), feature = "renderer-probe"))]
mod replay_fixture;
pub mod scanner;
pub mod settings_index;
mod settings_navigation;
pub mod settings_search;
pub mod shelf;
pub mod sidebar;
mod sliding_line;
/// Optional startup managed by the desktop operating system.
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
mod startup_row;
pub mod support;
mod system_theme;
pub mod text;
pub mod theme;
/// The vendored toolkit's own tests of its scroll views and its slider, run from here: the vendored crates are not members of this
/// workspace, so their `tests/` are never built by it, and those components carry local changes (`vendor/freya-components/PATCHES.md`).
/// Not formatted from here, so the files stay as the toolkit formats them.
#[cfg(all(test, feature = "headless", not(target_os = "android")))]
#[path = "../../../vendor/freya-core/tests/focus_viewport.rs"]
#[rustfmt::skip]
mod toolkit_focus_viewport;
#[cfg(all(test, feature = "headless", not(target_os = "android")))]
#[path = "../../../vendor/freya-components/tests/scrollview_nested.rs"]
#[rustfmt::skip]
mod toolkit_nested_scroll;
#[cfg(all(test, feature = "headless", not(target_os = "android")))]
#[path = "../../../vendor/freya-components/tests/scrollview.rs"]
#[rustfmt::skip]
mod toolkit_scroll;
#[cfg(all(test, feature = "headless", not(target_os = "android")))]
#[path = "../../../vendor/freya-components/tests/slider.rs"]
#[rustfmt::skip]
mod toolkit_slider;
#[cfg(all(test, feature = "headless", not(target_os = "android")))]
#[path = "../../../vendor/freya-components/tests/slider_touch.rs"]
#[rustfmt::skip]
mod toolkit_slider_touch;
pub mod top_sheet;
pub mod view;
// The pages of the window, each drawn by its own module from the shared pieces kept here.
mod bound_keys;
mod devices_page;
mod diagnostics_page;
mod header;
mod navigation;
mod notices;
mod plugin_manager;
mod portal;
mod receive_page;
mod send_page;
mod settings_page;
mod transfers_page;
mod window_chrome;

use freya::animation::*;
use freya::prelude::*;
#[cfg(not(target_os = "android"))]
use freya::winit::window::ResizeDirection;
use link::{Link, Offline, Session};
use peersus_app::runtime::Reply;
use peersus_core::{
    Settings,
    config::{CustomTheme, DesktopBarPosition, SettingsPatch, Theme as ThemeChoice, ThemeBase},
    model::{Command, PeerId, PluginInfo, PluginStatus},
};
use std::{path::PathBuf, rc::Rc, sync::Arc};
use theme::{Palette, state_border};
// Ours, over the toolkit's of the same name.
use view::{Approval, Phase, Presence, Progress, SendState, SettingsTab, Sidebar, View};
use {
    bound_keys::*, devices_page::*, header::*, navigation::*, notices::*, plugin_manager::*,
    portal::*, receive_page::*, send_page::*, settings_page::*, transfers_page::*,
    window_chrome::*,
};
use {navigation::MenuButton, portal::Portal};

/// The immutable model shared by the controls of one window. Connected compares a new
/// model by value before publishing it; components only need to compare revisions.
#[derive(Clone)]
struct PublishedView(Rc<View>);

impl From<View> for PublishedView {
    fn from(view: View) -> Self {
        Self(Rc::new(view))
    }
}

impl std::ops::Deref for PublishedView {
    type Target = View;

    fn deref(&self) -> &View {
        &self.0
    }
}

impl PartialEq for PublishedView {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

/// The launch configuration of the desktop entry: it finds the owner, starting it if none is there.
///
/// `Err` only when not even the program's executor could be started. Android is told where its
/// owner is and uses `config_with_local_canvas` instead.
#[cfg(not(target_os = "android"))]
pub fn config() -> Result<LaunchConfig, &'static str> {
    Ok(window_config(Session::open()?))
}

/// The same, after the program's own arguments are judged. With none this is [`config`]: the
/// window attaches to the owner, starting it when none is there. A `demo` build reads its own
/// arguments here too (see [`demo`]); every other argument fails, so a flag left over from
/// something else is never silently taken along. With the `demo` feature off, its arguments
/// are refused outright rather than read as belonging to a real run.
///
/// `Err` only when not even the program's executor could be started, or an argument cannot be
/// understood. Android is told where its owner is and uses `config_with_local_canvas` instead.
#[cfg(not(target_os = "android"))]
pub fn config_from(args: &[String]) -> Result<LaunchConfig, String> {
    if args.is_empty() {
        return config().map_err(str::to_owned);
    }
    // A store link the system opens the application with (`x-scheme-handler/peersus` in the desktop entry): the window
    // opens on it. Anything else that only looks like an address is refused below, as every unknown argument is.
    if let [address] = args
        && peersus_core::model::store_link_plugin(address).is_some()
    {
        return Ok(window_config(Session::open_with(Some(address.clone()))?));
    }
    #[cfg(target_os = "linux")]
    if args == [peersus_app::offer_panel::ARGUMENT] {
        return offer_window::config().map_err(str::to_owned);
    }
    #[cfg(feature = "demo")]
    if args
        .iter()
        .any(|argument| argument == "--demo" || argument.starts_with("--demo-"))
    {
        let spec = demo::parse(args)?;
        let config = window_config(Session::demo(&spec).map_err(str::to_owned)?);
        return Ok(match spec.probe {
            Some(run) => config.with_plugin(probe::Probe::begin(run)?),
            None => config,
        });
    }
    Err(format!(
        "Unknown argument {}: this program takes none{}",
        args[0],
        if cfg!(feature = "demo") {
            " outside a --demo run"
        } else {
            ""
        }
    ))
}

/// The same configuration for an owner already listening at `socket`, which this program does not
/// look for or start.
///
/// `platform` is what the platform offers beyond the window (see [`link::Platform`]): where its bars are, a picker for
/// files where the toolkit has no dialog, a camera. Each is present only on the platform that needs it.
pub fn config_at(socket: PathBuf, platform: link::Platform) -> Result<LaunchConfig, &'static str> {
    Ok(window_config(Session::open_at(socket, platform, None)?))
}

/// The Android owner is already listening in this process. Only Canvas frames bypass its usual
/// control connection: the compositor watches the owner's raster allocations directly.
#[cfg(target_os = "android")]
pub fn config_with_local_canvas(
    socket: PathBuf,
    platform: link::Platform,
    owner: &peersus_app::runtime::Client,
) -> Result<LaunchConfig, &'static str> {
    Ok(window_config(Session::open_at(
        socket,
        platform,
        Some(owner),
    )?))
}

/// The words every field's edit menu uses: the interface's language, not the toolkit's.
fn edit_labels() -> InputEditLabels {
    InputEditLabels(std::rc::Rc::new(|edit| {
        i18n::tr(match edit {
            InputEdit::SelectAll => "Select all",
            InputEdit::Copy => "Copy",
            InputEdit::Cut => "Cut",
            InputEdit::Paste => "Paste",
        })
        .to_owned()
    }))
}

#[cfg(not(target_os = "android"))]
pub use peersus_core::APPLICATION_ID;

#[cfg(all(target_os = "android", feature = "renderer-probe"))]
#[doc(hidden)]
pub use renderer_bench::run_android_probe as run_android_renderer_probe;
#[cfg(not(target_os = "android"))]
#[doc(hidden)]
pub use renderer_bench::run_worker as run_renderer_worker;

// Headroom for a large canvas plus normal UI resources, without retaining Freya's default
// 256 MiB of idle scratch textures. The comparison uses the same GPU cache budget.
const WINDOW_GPU_CACHE: GpuResourceCacheLimit = GpuResourceCacheLimit::from_mb(64);

fn window_config(session: Arc<Session>) -> LaunchConfig {
    let renderer = session
        .link
        .as_ref()
        .map(|link| link.client().snapshot().settings.renderer)
        .unwrap_or_default();
    let window = WindowConfig::new(move || App {
        session: session.clone(),
    });
    #[cfg(not(target_os = "android"))]
    let window = window.with_app_id(APPLICATION_ID);
    LaunchConfig::new()
        .with_gpu_resource_cache_limit(WINDOW_GPU_CACHE)
        .with_global_context(edit_labels())
        .with_font(font::FAMILY, font::BYTES)
        .with_default_font(font::FAMILY)
        .with_window(
            window
                // The toolkit takes a fixed title; what tells two windows apart is the device's
                // name in the header, which can change while the window is open.
                .with_title("Peersus")
                .with_size(1100., 720.)
                .with_min_size(760., 560.)
                .with_decorations(false)
                .with_renderer(renderer_settings::preference(renderer))
                // Desktop rounded corners show what is behind them. Android owns the
                // whole opaque activity surface, including its system-bar background.
                .with_transparency(!cfg!(target_os = "android"))
                .with_background(Color::TRANSPARENT)
                .with_resizable(true),
        )
}

#[cfg(all(test, not(target_os = "android")))]
mod application_id_tests {
    /// The desktop matches a window to an entry by the window's id: the entry's file is named by it, the entry says its
    /// windows carry it, and the icon is named by it. Change one without the others and the dock shows a window with no icon.
    #[test]
    fn the_windows_id_names_the_desktop_entry_its_windows_and_its_icon() {
        let id = super::APPLICATION_ID;
        let entry = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../packaging/linux")
                .join(format!("{id}.desktop")),
        )
        .expect("no desktop entry is named by the window's id");
        for line in [format!("StartupWMClass={id}"), format!("Icon={id}")] {
            assert!(
                entry.lines().any(|found| found == line),
                "{line} is not in the entry"
            );
        }
    }
}

#[cfg(all(test, feature = "demo", not(target_os = "android")))]
mod demo_gate_tests {
    /// The program's own arguments are judged before any owner is looked for or started: a
    /// demo flag opens the read-only demo, and anything else fails.
    #[test]
    fn demo_arguments_open_the_demo_and_everything_else_fails() {
        assert!(super::config_from(&["--demo".to_owned()]).is_ok());
        assert!(
            super::config_from(&[
                "--demo-page=settings".to_owned(),
                "--demo-peers=2".to_owned(),
            ])
            .is_ok()
        );
        assert!(super::config_from(&["--release".to_owned()]).is_err());
        assert!(
            super::config_from(&["--demo".to_owned(), "--something-else".to_owned(),]).is_err()
        );
    }
}

// ---- back ------------------------------------------------------------------------------

/// Where every Back goes, the platform's and a desktop's Escape: what is open over the window closes first, then a page that
/// takes Back itself is told it (both through [`back::BackStack`]), then the application goes back to its first screen, and
/// from there the platform's Back leaves, at a second press when the person keeps that so.
#[derive(Clone)]
struct BackRoute {
    stack: back::BackStack,
    page: State<Page>,
    /// Opens a page the way the navigation does: what Back uses to go to the first screen.
    home: EventHandler<Page>,
    /// When the last Back on the first screen said that one more leaves.
    again_since: State<Option<u64>>,
    platform: Option<link::BackControl>,
    link: Option<Link>,
}

impl BackRoute {
    /// One Back; `can_leave` for the platform's, not a desktop's Escape. Says whether it did anything.
    fn press(&self, can_leave: bool) -> bool {
        let mut again_since = self.again_since;
        if self.stack.close_top() {
            again_since.set_if_modified(None);
            return true;
        }
        let twice = self
            .link
            .as_ref()
            .is_none_or(|link| link.client().snapshot().settings.back_twice_to_leave);
        let now = portal::monotonic_ms();
        let platform = self.platform.as_ref().filter(|_| can_leave);
        let home = *self.page.peek() == Page::Send;
        let since = *again_since.peek();
        match view::back_step(home, platform.is_some(), twice, since, now) {
            view::BackStep::Home => {
                again_since.set_if_modified(None);
                self.home.call(Page::Send);
            }
            view::BackStep::Again => {
                again_since.set(Some(now));
                if let Some(back) = platform {
                    (back.hint)(i18n::tr("Press Back again to leave"));
                }
            }
            view::BackStep::Leave => {
                again_since.set_if_modified(None);
                if let Some(back) = platform {
                    (back.leave)();
                }
            }
            view::BackStep::Nothing => return false,
        }
        true
    }
}

// ---- navigation ------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Send,
    Receive,
    Peers,
    Transfers,
    Settings,
    /// The one place the extensions' pages share (see `contributed_entries`).
    Extensions,
    /// A page an extension registered in the main navigation, by its stable key (see `tab_key`).
    Contributed(u64),
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
            Page::Extensions => "EXTENSIONS",
            Page::Contributed(_) => "",
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

/// The screen and Settings tab a name opens on: the one table `PEERSUS_PAGE` and the demo's
/// `--demo-page` are read by, so the two can never drift apart. A name that is none of these
/// falls to Send, as the environment always has.
fn page_named(name: &str) -> (Page, SettingsTab) {
    match name {
        "receive" => (Page::Receive, SettingsTab::default()),
        "peers" | "devices" => (Page::Peers, SettingsTab::default()),
        "transfers" => (Page::Transfers, SettingsTab::default()),
        "settings" => (Page::Settings, SettingsTab::default()),
        // The old pages are tabs of Settings now: these names open Settings on that tab.
        "plugins" => (Page::Settings, SettingsTab::Extensions),
        "extensions" => (Page::Extensions, SettingsTab::default()),
        "about" => (Page::Settings, SettingsTab::About),
        "diagnostics" => (Page::Settings, SettingsTab::Diagnostics),
        // The history is the Transfers page now: the one place that lists what was sent and received.
        "history" => (Page::Transfers, SettingsTab::default()),
        "device" => (Page::Settings, SettingsTab::Device),
        "appearance" => (Page::Settings, SettingsTab::Appearance),
        _ => (Page::Send, SettingsTab::default()),
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

/// A transfer's failed state with the owner's reason, both in the window's language.
fn failed_words(reason: &str) -> String {
    i18n::format(
        i18n::language(),
        "FAILED · {reason}",
        &[("reason", i18n::tr(reason))],
    )
}

// ---- root ------------------------------------------------------------------------------

/// The root of the window: frame, header, and either the screens or why there is no owner.
#[derive(Clone)]
struct App {
    session: Arc<Session>,
}

/// Whether this desktop announces an offer outside the window: where the owner starts a panel for it (Linux) and the session
/// has what the panel is placed on. The owner's own rule, asked of the same session, so Settings offers no edge for nothing.
fn desktop_panel() -> bool {
    #[cfg(target_os = "linux")]
    {
        peersus_app::offer_panel::has_display(std::env::var_os("DISPLAY").as_deref())
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// The colours a surface is drawn in, and whether they are a dark set: the person's own theme when one is given, otherwise
/// their choice, with the system's answer where they left it to the system. One rule for the window and the panel of an offer.
fn appearance_of(
    choice: ThemeChoice,
    custom: Option<&CustomTheme>,
    platform_dark: bool,
    accent: [u8; 3],
) -> (Palette, bool) {
    match custom {
        Some(custom) => (Palette::custom(custom), custom.base == ThemeBase::Dark),
        None => (
            Palette::of_choice(choice, platform_dark, accent),
            choice == ThemeChoice::Dark || (choice == ThemeChoice::System && platform_dark),
        ),
    }
}

impl PartialEq for App {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.session, &other.session)
    }
}

/// Development builds only: tick the watchdog once a second from a task of this window's own runtime. That task is
/// polled by the window's event loop (public toolkit API only: a future of the root component), so a loop that is
/// blocked or no longer wakes stops the ticks, and the watchdog writes one report. Nothing in any other build.
fn use_window_heartbeat(session: &Arc<Session>) {
    if !peersus_diagnostics::DEVELOPMENT || peersus_diagnostics::incident::report_folder().is_none()
    {
        return;
    }
    let session = Arc::clone(session);
    use_future(move || {
        let mut clock = session.diagnostic_clock();
        async move {
            let heartbeat = peersus_diagnostics::incident::watch("window-loop");
            loop {
                clock.tick().await;
                heartbeat.tick();
            }
        }
    });
}

impl Component for App {
    fn render(&self) -> impl IntoElement {
        use_window_heartbeat(&self.session);
        foreground::provide(self.session.platform.activity.clone());
        // The choice lives in the owner's settings, and the window opens knowing them: the owner
        // already has a snapshot when this window attaches. Starting from anything else shows
        // the wrong theme until the next change, which on a quiet owner is never.
        let saved = match &self.session.link {
            Ok(link) => link.client().snapshot().settings.theme,
            Err(_) => ThemeChoice::System,
        };
        let choice = use_state(move || saved);
        // English until the owner's language file is read, which the first snapshot does at once (below).
        let initial_language = self
            .session
            .link
            .as_ref()
            .map(|link| i18n::shown(&link.client().snapshot().languages))
            .unwrap_or_default();
        let language = use_state(move || initial_language);
        provide_context(i18n::Current(language));
        // The size of the interface, kept in the owner's settings. The toolkit multiplies every logical size by it, so
        // text, spacing and icons grow together and nothing here is multiplied by hand.
        let initial_scale = self
            .session
            .link
            .as_ref()
            .map(|link| link.client().snapshot().settings.scale)
            .unwrap_or(1.0);
        let scale = use_state(move || initial_scale);
        // The saved size is applied to the window at once when it opens, and after that once it has STOPPED changing for a moment, and
        // only if the window is not already at it. The slider reads the pointer against where the slider is on the screen, and applying
        // every size as it is dragged laid the page out again under the pointer: a pointer at 120 % then meant 90 % where the slider had
        // moved to, so the interface jumped between sizes. Waiting for the drag to rest means the slider does not move while it is held.
        // The first one is not waited for: the window would show 100 % for that moment before it took the size the person had saved.
        // Ctrl and plus or minus change the window's own size without going through the saved one, so the check against the window
        // keeps the saved size from pulling that back.
        let mut applied_once = use_state(|| false);
        // On a narrow screen a large saved size is drawn at the largest the window holds (`interface_size::Percent::largest`), and drawn
        // at the saved one again when the window is wide enough for it (a phone turned, a desktop window made wider).
        let largest = interface_size::use_largest();
        use_side_effect_with_deps(&(scale(), largest), move |(saved, largest)| {
            let saved = *saved;
            let wanted = saved.min(*largest);
            let apply = move || {
                let window = Platform::get().current_window();
                // The window's own size of the interface, not its whole scale (that one also carries the screen's density).
                if (*window.custom_scale_factor.peek() - f64::from(wanted)).abs() > f64::EPSILON {
                    window.set_custom_scale_factor(f64::from(wanted));
                }
            };
            if !*applied_once.peek() {
                applied_once.set(true);
                apply();
                return;
            }
            let mut told = link::after(std::time::Duration::from_millis(350));
            spawn(async move {
                if (&mut told).await.is_ok() && (*scale.peek() - saved).abs() < f32::EPSILON {
                    apply();
                }
            });
        });
        // Whether this window has the person's attention, said to the owner each time it changes: with no window in front an
        // offer is announced outside it (see `peersus_app::offer_panel`). Only a window that has the way to say it does.
        let focused = *Platform::get().current_window().is_app_focused.read();
        let front = self.session.front.clone();
        use_side_effect_with_deps(&focused, move |focused| {
            if let Some(front) = &front {
                front.tell(*focused);
            }
        });
        // The accent the person chose, kept in the owner's settings like the size: what goes out is drawn in it, and so are the
        // main shapes of the pictures. It is read here, from the snapshot the window opened on, and kept up to date by `Connected`.
        let initial_accent = self
            .session
            .link
            .as_ref()
            .map(|link| link.client().snapshot().settings.accent)
            .unwrap_or(peersus_core::Settings::default().accent);
        let accent = use_state(move || initial_accent);
        let initial_custom = self
            .session
            .link
            .as_ref()
            .map(|link| link.client().snapshot().settings.custom_theme.clone())
            .unwrap_or_default();
        let custom = use_state(move || initial_custom);
        let preview = use_state(|| None::<CustomTheme>);
        provide_context(appearance::Preview(preview));
        // Back: what is open on top closes first, then a page that takes Back itself, then the application goes back (see `BackRoute`).
        let back_stack = back::BackStack(use_state(Vec::new));
        provide_context(back_stack);
        #[cfg(not(target_os = "android"))]
        provide_context(window_chrome::PageBackdropDismissal(use_state(|| None)));
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
        let custom_palette = preview
            .read()
            .clone()
            .or_else(|| (choice() == ThemeChoice::Custom).then(|| custom.read().clone()));
        let (palette, dark) =
            appearance_of(choice(), custom_palette.as_ref(), platform_dark, accent());
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
            let word =
                |(r, g, b): (u8, u8, u8)| (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
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
        // A phone is held in the hands whichever way it is turned; nothing else is.
        let form = view::Form::of(size().0, size().1, cfg!(target_os = "android"));
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
        // Whether the system leaves this program out of its battery optimisation (Android): provided once, for its row of Settings.
        if let Some(battery) = self.session.platform.battery.clone() {
            provide_context(battery);
        }
        if let Some(opener) = self.session.platform.open_url.clone() {
            provide_context(opener);
        }
        if let Some(picker) = self.session.platform.plugin_picker.clone() {
            provide_context(picker);
        }
        if let Some(picker) = self.session.platform.language_picker.clone() {
            provide_context(picker);
        }
        // How an extension's action is run on the file chosen on Send where that file is held and not a path (Android): provided
        // once, for the controls of Send.
        if let Some(run) = self.session.platform.selected_action.clone() {
            provide_context(run);
        }
        // The platform's settings that each serve one of the application's services to extensions (Android): provided once, for Settings,
        // which shows one only while an installed extension uses its service.
        provide_context(self.session.platform.service_settings.clone());
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
                        safe.set_if_modified(view::SafeArea::from_physical(
                            rectangle, screen, scale,
                        ));
                        if area.changed().await.is_err() {
                            break;
                        }
                    }
                });
            });
        }
        // Where the window starts: what a demo run asked for, or `PEERSUS_PAGE`, which only
        // lets a screenshot start on a given screen. People use the header.
        let (first, first_tab) = self
            .session
            .start
            .unwrap_or_else(|| page_named(std::env::var("PEERSUS_PAGE").as_deref().unwrap_or("")));
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
            Ok(link) => {
                link.client().snapshot().settings.device_name.trim()
                    != Settings::default().device_name
            }
            Err(_) => false,
        };
        let named = use_state(move || saved_named);
        // How much of the header the page under it has scrolled out of sight. `frame`, which every page draws its scrolling area
        // with, writes it from the page's own scroll position and the header here reads it, so it is provided to them as context
        // and not carried through every page. A new page starts at its top, so a change of page brings the bar back.
        let mut bar_hide = use_state(view::HeaderHide::default);
        // Whether the person has done anything in the window yet: the first touch, click or key, and never undone (see `Brand`).
        let mut awake = use_state(|| false);
        provide_context(bar_hide);
        // Whether a finger is held on a circle (see `Scrolling`): written by the circle, read by the page under it.
        provide_context(use_state(|| HoldingCircle(false)));
        // Whether the Binds page is recording a chord: no binding is run meanwhile (see `binds_page::Recording`).
        let recording_keys = use_state(|| false);
        provide_context(binds_page::Recording(recording_keys));
        // What stopped an Accept run by a key, if anything. Not drawn: an offer that was not accepted stays over the window with its own Accept.
        let key_said: State<Option<String>> = use_state(|| None);
        // Which waiting package the person pressed Install on: the press only asks, and the overlay (see `agreement_overlay`) installs it once
        // its agreement is accepted.
        provide_context(agreement_overlay::Ask(use_state(|| None)));
        let mut bar_top: State<Option<f32>> = use_state(|| None);
        provide_context(BottomBarTop(bar_top));
        // A bar that is not along the bottom has no top to stop at: what an upright screen measured is forgotten when the form changes, and
        // the bar says it again when it is laid out along the bottom.
        use_side_effect_with_deps(&form, move |_| bar_top.set(None));
        // Where the window is on its way to after a file was accepted (see `view::ReceiveJump`): `Connected` says a file was accepted, the root
        // shows Receive, and Receive scrolls to its list. A place the person chooses meanwhile ends it: they are taken nowhere after that.
        let mut receive_jump = use_state(view::ReceiveJump::default);
        if receive_jump() == view::ReceiveJump::ToPage {
            receive_jump.set(if named() {
                view::ReceiveJump::ToList
            } else {
                view::ReceiveJump::Nowhere
            });
            if named() && page() != Page::Receive {
                page.set(Page::Receive);
                bar_hide.set(view::HeaderHide::default());
            }
        }
        // A store link (`peersus://store/<id>`): the extension it names (read by the Extensions tab of Settings, which goes to its row and
        // starts its Install), and whether Settings is still to be opened for it. `Connected` writes both, from the address the window was
        // opened with or from the owner's `store.asked`; the root opens Settings on Extensions, as a sheet on a phone and as the page on a desktop.
        let store_link = use_state(|| None::<peersus_core::plugins::PluginId>);
        provide_context(plugin_manager::StoreLink(store_link));
        let mut store_jump = use_state(|| false);
        if store_jump() {
            store_jump.set(false);
            if named() {
                settings_tab.set(SettingsTab::Extensions);
                if form.settings_places_come_from_the_top() {
                    menu_open.set(true);
                } else if let Some(opened) = view::Sidebar::page_opened(true, Page::Settings) {
                    page.set(opened);
                    bar_hide.set(view::HeaderHide::default());
                }
            }
        }
        // An extension's page a tapped notification asked for (see `Snapshot::opened`): `Connected` finds it, the root shows it.
        let mut page_jump = use_state(|| None::<Page>);
        if let Some(asked) = page_jump() {
            page_jump.set(None);
            if named() {
                menu_open.set(false);
                receive_jump.set_if_modified(view::ReceiveJump::Nowhere);
                page.set(asked);
                bar_hide.set(view::HeaderHide::default());
            }
        }
        let on_select = EventHandler::new(move |chosen: Page| {
            menu_open.set(false);
            receive_jump.set_if_modified(view::ReceiveJump::Nowhere);
            if let Some(opened) = view::Sidebar::page_opened(named(), chosen) {
                page.set(opened);
                bar_hide.set(view::HeaderHide::default());
            }
        });
        // One route for every Back: the platform's (Android's callback, or its key before Android 13) and a desktop's Escape.
        let back_route = BackRoute {
            stack: back_stack,
            page,
            home: on_select.clone(),
            again_since: use_state(|| None),
            platform: self.session.platform.back.clone(),
            link: self.session.link.as_ref().ok().cloned(),
        };
        use_hook({
            let route = back_route.clone();
            move || {
                if let Some(back) = route.platform.clone() {
                    let mut presses = back.presses.clone();
                    spawn(async move {
                        while presses.changed().await.is_ok() {
                            route.press(true);
                        }
                    });
                }
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
        let bar_position = use_state(|| {
            self.session
                .link
                .as_ref()
                .map(|link| link.client().snapshot().settings.desktop_bar_position)
                .unwrap_or_default()
        });
        provide_context(bar_position);
        let dock = form.window_bar(bar_position());
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
        // What extensions keep for their drawings, by layer id. Connected fetches what each snapshot names and fills it, and every
        // drawing that places a layer reads it from here.
        let kept_layers: State<layers::Kept> = use_state(layers::Kept::default);
        provide_context(layers::KeptHandle(kept_layers));
        // The pictures extensions keep for their pages, by digest: filled from what each snapshot's pages place, read by every
        // page that places one.
        let kept_pictures = use_state(shelf::Held::<{ peersus_core::profile::KEPT_EDGE }>::default);
        provide_context(shelf::KeptPictures(kept_pictures));
        let saved_picture = match &self.session.link {
            Ok(link) => link.client().snapshot().profile_picture,
            Err(_) => None,
        };
        let own_picture = use_state(move || saved_picture);
        // Seed the header once. Connected publishes changes; rebuilding this component must
        // not rebuild and then discard a View of every manifest and extension page.
        let (offer, contributed, extension_choices) = use_hook(|| {
            let first = self
                .session
                .link
                .as_ref()
                .ok()
                .map(|link| View::of(&link.client().snapshot()));
            let offer = if std::env::var("PEERSUS_OFFER").is_ok_and(|v| v == "1") {
                Some(view::sample_offer(unix_now()))
            } else {
                first.as_ref().and_then(view::offered_now)
            };
            (
                State::create(offer),
                State::create(first.as_ref().map(contributed_entries).unwrap_or_default()),
                State::create(
                    first
                        .as_ref()
                        .map(contributed_page::choices)
                        .unwrap_or_default(),
                ),
            )
        });
        // Which page the shared Extensions place shows: chosen in the header, beside the mark, and kept here so that leaving the place
        // and coming back keeps it.
        let extension_page: State<Option<(peersus_core::plugins::PluginId, String)>> =
            use_state(|| None);
        // A page an extension took away (stopped, removed) leaves no window on a page that is no more. A page that lost its own
        // place because the person put the pages together again is found in the place they share.
        if matches!(current, Page::Contributed(_) | Page::Extensions)
            && !contributed.read().iter().any(|entry| entry.page == current)
        {
            let shared = contributed
                .read()
                .iter()
                .any(|entry| entry.page == Page::Extensions);
            page.set(if shared { Page::Extensions } else { Page::Send });
        }
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
                bar_position,
                keep_icons,
                two_fingers,
                device_name,
                pictures,
                kept_layers,
                kept_pictures,
                own_picture,
                contributed,
                extension_choices,
                extension_page,
                offer,
                receive_jump,
                store_jump,
                page_jump,
                opened_with: self.session.platform.opened_with.clone(),
                now,
                menu_open,
                inset_top: safe().top,
                inset_bottom: safe().bottom
                    + if form.bottom_places().is_empty() || form.places_are_at_the_side() || !tabs {
                        0.
                    } else {
                        64.
                    },
                room_height: size().1,
            }
            .into(),
            Err(reason) => Unreachable {
                reason: reason.clone(),
                palette,
            }
            .into(),
        };
        // The fingers that are down now, by the number the system gave each: what tells a touch of two fingers from one.
        let fingers: State<view::Fingers> = use_state(view::Fingers::default);
        // The sheet takes every touch while it is shown, so a finger lifted over it is never reported lifted to this root: the fingers are
        // forgotten whenever the sheet opens or closes, and the next touch begins from none.
        use_side_effect_with_deps(&menu_open(), {
            let mut fingers = fingers;
            move |_| fingers.write().clear()
        });
        // The last chord run and when: a press the window hears and the desktop also reports for the outside bindings is run once.
        let last_chord: State<Option<(peersus_api::chord::Chord, std::time::Instant)>> =
            use_state(|| None);
        // The bindings asked of the desktop while the window is not in front, when the person switched that on (see `outside_chords`).
        #[cfg(target_os = "linux")]
        if let Ok(link) = &self.session.link {
            let link = link.clone();
            let sink = self.session.platform.folder_sink.clone();
            use_hook(move || {
                spawn(outside_keys(OutsideRun {
                    link,
                    page,
                    offer,
                    sink,
                    said: key_said,
                    recording: recording_keys,
                    last: last_chord,
                }));
            });
        }
        // The one list of the main navigation, for the header on a desktop and the bottom bar on a phone.
        let entries = nav_entries(form, &contributed.read());
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
            .corner_radius(
                if safe().top + safe().right + safe().bottom + safe().left == 0. {
                    12.
                } else {
                    0.
                },
            )
            .overflow(Overflow::Clip)
            .border(
                Border::new()
                    .fill(palette.border)
                    .width(1.)
                    .alignment(BorderAlignment::Inner),
            )
            // Two fingers down bring Settings down from the top (Android; an option, on by default). Counted by the fingers that are down at the
            // same moment (`view::Fingers`), so a swipe or a pinch on a page does not open it twice, and it does nothing while Settings is already
            // open: a layer does not open itself. A finger whose lift never reached here (it was lifted over the sheet as it closed) is not counted
            // for ever, or every touch after it would be a touch of two fingers and would open Settings by itself.
            .on_touch_start({
                let mut fingers = fingers;
                move |e: Event<TouchEventData>| {
                    let wanted = fingers.write().went_down(e.finger_id, monotonic_ms());
                    if wanted
                        && two_fingers()
                        && tabs
                        && form.settings_places_come_from_the_top()
                        && !menu_open()
                    {
                        tracing::trace!(target: "peersus_ui::gesture", event = "two-fingers", "Settings opened");
                        menu_open.set(true);
                    }
                }
            })
            .on_touch_move({
                let mut fingers = fingers;
                move |e: Event<TouchEventData>| fingers.write().moved(e.finger_id, monotonic_ms())
            })
            // Anywhere in the window, and before any control takes it: a press that a button keeps to itself is still something done.
            .on_global_pointer_down(move |_| {
                if !*awake.peek() {
                    awake.set(true);
                }
            })
            .on_global_key_down({
                let link = self.session.link.as_ref().ok().cloned();
                let sink = self.session.platform.folder_sink.clone();
                // The Back key (a keyboard's, or the system's before Android 13), and a desktop's Escape: one route for both. Where the
                // activity's back callback carries Back, a key that also arrives is that same press, already acted on.
                let route = back_route.clone();
                move |e: Event<KeyboardEventData>| {
                    if !*awake.peek() {
                        awake.set(true);
                    }
                    if e.key == Key::Named(NamedKey::BrowserBack) {
                        if route.platform.as_ref().is_none_or(|back| back.by_key) {
                            route.press(true);
                        }
                        return;
                    }
                    // Escape is Back on a desktop, except where it already means something: a field being edited lets go of the
                    // keyboard with it, and a chord being recorded is left as it was. Escape with nothing to go back to stays free
                    // for a binding.
                    if e.key == Key::Named(NamedKey::Escape)
                        && !cfg!(target_os = "android")
                        && e.modifiers.is_empty()
                        && !*recording_keys.peek()
                        && !field::editing_text()
                        && route.press(false)
                    {
                        return;
                    }
                    // A phone binds no keys; a chord being recorded runs nothing; a read-only window changes nothing.
                    if cfg!(target_os = "android") || *recording_keys.peek() {
                        return;
                    }
                    let Some(link) = &link else {
                        return;
                    };
                    let Some(chord) = chords::of(&e.key, e.code, e.modifiers) else {
                        return;
                    };
                    run_chord(link, chord, page, offer, sink.clone(), key_said, last_chord);
                }
            })
            .on_touch_end({
                let mut fingers = fingers;
                move |e: Event<TouchEventData>| fingers.write().went_up(e.finger_id)
            })
            .on_touch_cancel({
                let mut fingers = fingers;
                move |e: Event<TouchEventData>| fingers.write().went_up(e.finger_id)
            });
        // Scrolling decides whether the bar goes (`view::HeaderHide`); the bar then goes the whole way, or comes the whole way back, on
        // the motion every other thing has, and never stops halfway because the finger did.
        let going = motion::use_progress(
            form.header_hides_on_scroll() && bar_hide().hidden(),
            reduced_motion(),
        );
        let chooser = (tabs && current == Page::Extensions && extension_choices.read().len() > 1)
            .then(|| {
                contributed_page::Chooser {
                    choices: extension_choices.read().clone(),
                    chosen: extension_page,
                    palette,
                    prefer_above: dock.opens_up(),
                }
                .into_element()
            });
        let bar = header(
            palette,
            current,
            self.session.link.is_ok(),
            Shown {
                tabs: if tabs && form.bottom_places().is_empty() {
                    &entries
                } else {
                    &[]
                },
                menu: tabs,
                width: size().0,
                hidden: view::HEADER_HEIGHT * view::Popup::at(going).opacity,
                reduced_motion: reduced_motion(),
                awake: awake(),
                chooser: chooser.as_ref(),
                bar: dock,
            },
            form,
            on_select.clone(),
            menu_open,
        )
        .key("window-bar");
        let page = rect()
            .key("page")
            .width(Size::fill())
            .height(Size::flex(1.))
            .horizontal()
            .content(Content::flex())
            // The one sidebar. Where the person is and the one option decide what it is:
            // hidden by default, its icons when asked, expanded inside Settings, and the
            // width is followed, so every move between the three is the same move.
            .maybe(
                !cfg!(target_os = "android") && form != view::Form::Compact,
                |el| {
                    el.child(sidebar::Bar {
                        active: settings_tab(),
                        device_name: device_name.read().clone(),
                        picture: own_picture()
                            .and_then(|digest| pictures.read().get(&digest))
                            .map(avatar_view::Shared),
                        state: Sidebar::state_for(
                            tabs,
                            current == Page::Settings,
                            keep_icons(),
                            form,
                        ),
                        reduced_motion: reduced_motion(),
                        palette,
                        clearance: dock.clearance(),
                        on_select: on_open_tab.clone(),
                    })
                },
            )
            .child(body);
        // The offer is still in the column, at the edge opposite the desktop dock. Its own
        // space never falls between the page and the dock, and moving it keeps its controls mounted.
        let mut incoming = match (&self.session.link, offer.read().clone()) {
            (Ok(link), Some(offered)) => Some(
                rect()
                    .key("incoming-offer")
                    .width(Size::fill())
                    .child(IncomingOffer {
                        link: link.clone(),
                        offered,
                        palette,
                        now: now(),
                        buttons: self.session.platform.offer_buttons.clone(),
                        corner: 0.,
                    }),
            ),
            _ => None,
        };
        root = if tabs && form.places_are_at_the_side() {
            // The places of a phone on its side, down the left edge: the same bar as along the bottom of an upright one. It stands
            // beside the header and not under it: the short side of the screen is all the bar has, and under the header its places
            // had what the header left, which is not a thumb's height once extensions add theirs.
            root.child(
                rect()
                    .width(Size::fill())
                    .height(Size::flex(1.))
                    .horizontal()
                    .content(Content::flex())
                    .child(BottomBar {
                        active: current,
                        entries: entries.clone(),
                        side: true,
                        palette,
                        on_select: on_select.clone(),
                    })
                    .child(
                        rect()
                            .width(Size::flex(1.))
                            .height(Size::fill())
                            .content(Content::flex())
                            .child(bar)
                            .child(page),
                    ),
            )
        } else {
            root.child(bar)
                .maybe_child(if dock.opens_up() {
                    incoming.take()
                } else {
                    None
                })
                .child(page)
        };
        root = root.maybe_child(incoming).child(
            if form.bottom_places().is_empty() || form.places_are_at_the_side() || !tabs {
                rect().into()
            } else {
                Element::from(BottomBar {
                    active: current,
                    entries: entries.clone(),
                    side: false,
                    palette,
                    on_select: on_select.clone(),
                })
            },
        );
        #[cfg(not(target_os = "android"))]
        {
            root = root.child(resize_zones(
                size().0,
                size().1,
                (!form.settings_places_come_from_the_top()).then_some(menu_open),
            ));
        }
        // Always handed to `Reveal`, which shows it while wanted and for as long as it takes to
        // go away: taking it out of the tree the moment it is closed is what made it vanish.
        let (panel, backdrop) = menu_layer(palette, on_open_tab.clone(), menu_open, dock, size().1);
        root = root.child(motion::Reveal {
            // On a phone the menu is the sheet from the top, below, and not this small panel in a corner.
            wanted: menu_open() && tabs && !form.settings_places_come_from_the_top(),
            reduced_motion: reduced_motion(),
            origin: if dock.opens_up() {
                TransformOrigin::bottom_right()
            } else {
                TransformOrigin::top_right()
            },
            room_height: size().1,
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

/// Start over with a new identity from the window, for a person who cannot use a command line: every identity record is deleted and the
/// pairings are put aside (`peersus_app::identity::reset`). It works without the owner, which is what has not started.
#[cfg(not(target_os = "android"))]
fn start_over_with_a_new_identity() -> Result<(), &'static str> {
    let directory =
        peersus_app::instance::data_directory().ok_or("There is no data directory to look in")?;
    peersus_app::identity::reset(&directory)
}

impl Component for Unreachable {
    fn render(&self) -> impl IntoElement {
        let (title, text) = self.reason.words(i18n::language());
        let palette = self.palette;
        // Whether the owner stopped for want of an identity: then the window offers the way out, because nobody should have to remove the
        // application to get past it. Two steps, as it deletes something that cannot be got back.
        let identity_failed = matches!(&self.reason, Offline::NotStarted(Some(reason))
            if reason.starts_with(peersus_app::IDENTITY_STARTUP_ERROR_PREFIX));
        let mut sure: State<bool> = use_state(|| false);
        let mut outcome: State<Option<Result<(), &'static str>>> = use_state(|| None);
        let mut busy: State<bool> = use_state(|| false);
        // Only a desktop adds to it below (the way out of a lost identity is the desktop's).
        #[cfg_attr(
            target_os = "android",
            expect(unused_mut, reason = "only a desktop adds to it")
        )]
        let mut body = rect()
            .expanded()
            .center()
            .spacing(10.)
            .child(
                label()
                    .text(title)
                    .color(palette.text)
                    .font_size(28.)
                    .font_weight(FontWeight::BOLD),
            )
            .child(label().text(text).color(palette.muted).font_size(15.));
        // The window never ends in a screen with nothing to press. An owner that ended is started again by the window itself (see where the
        // snapshots are followed); this screen is for what that did not cure. The button starts the program afresh. For an identity the system
        // would not give, that makes the owner ask the system for the keyring again, and the button says so; for anything else it says Restart.
        // For a missing identity there is also the way to start over.
        #[cfg(not(target_os = "android"))]
        {
            body = body.child(Solid {
                text: if identity_failed {
                    "Request unlock"
                } else {
                    "Restart"
                },
                accent: palette.receive,
                palette,
                enabled: true,
                on_press: EventHandler::new(move |()| {
                    link::start_afresh();
                    // Still here: the program could not be started.
                    outcome.set(Some(Err("Peersus could not be started again")));
                }),
            });
        }
        // What could not be done, for either button.
        #[cfg(not(target_os = "android"))]
        if let Some(Err(refused)) = *outcome.read() {
            body = body.child(
                label()
                    .text(i18n::tr(refused))
                    .color(palette.muted)
                    .font_size(14.),
            );
        }
        #[cfg(not(target_os = "android"))]
        if identity_failed {
            match *outcome.read() {
                Some(Ok(())) => {
                    body = body.child(
                        label()
                            .text(i18n::tr(
                                "The identity was reset. Close this window and open Peersus again.",
                            ))
                            .color(palette.text)
                            .font_size(15.),
                    );
                }
                _ => {
                    if *sure.read() {
                        body = body
                            .child(
                                label()
                                    .text(i18n::tr("This deletes the identity kept by the system and sets the pairings aside. Devices must be paired again. It cannot be undone."))
                                    .color(palette.muted)
                                    .font_size(14.),
                            )
                            .child(Solid {
                                text: "Yes, delete the previous identity and generate a new one",
                                accent: palette.send,
                                palette,
                                enabled: !*busy.read(),
                                on_press: EventHandler::new(move |()| {
                                    if *busy.peek() {
                                        return;
                                    }
                                    busy.set(true);
                                    // The system may ask for the keyring's password for a while: that is waited for off the window's thread.
                                    let (done, received) = std::sync::mpsc::channel();
                                    std::thread::spawn(move || {
                                        let _ = done.send(start_over_with_a_new_identity());
                                    });
                                    spawn(async move {
                                        loop {
                                            match received.try_recv() {
                                                Ok(result) => {
                                                    outcome.set(Some(result));
                                                    busy.set(false);
                                                    break;
                                                }
                                                Err(std::sync::mpsc::TryRecvError::Empty) => {
                                                    let _ = link::after(std::time::Duration::from_millis(250)).await;
                                                }
                                                Err(_) => {
                                                    outcome.set(Some(Err("The identity could not be reset")));
                                                    busy.set(false);
                                                    break;
                                                }
                                            }
                                        }
                                    });
                                }),
                            });
                    } else {
                        body = body.child(Solid {
                            text: "Generate a new identity",
                            accent: palette.send,
                            palette,
                            enabled: true,
                            on_press: EventHandler::new(move |()| sure.set(true)),
                        });
                    }
                }
            }
        }
        #[cfg(target_os = "android")]
        let _ = (identity_failed, &mut sure, &mut outcome, &mut busy);
        body
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
    language: State<i18n::Shown>,
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
    bar_position: State<DesktopBarPosition>,
    /// The one saved option of the sidebar, and the name its profile shows: read by the root.
    keep_icons: State<bool>,
    /// Whether a touch of two fingers opens the Settings overlay (Android): written here from every snapshot, read by the root.
    two_fingers: State<bool>,
    device_name: State<String>,
    /// The pictures this window holds, by digest: filled here from every snapshot, read by what draws one.
    pictures: State<shelf::Held>,
    /// What extensions keep for their drawings (`peersus.drawings`): filled here from every snapshot, read by every drawing that
    /// places a layer.
    kept_layers: State<layers::Kept>,
    /// What extensions keep for their pages' pictures (`peersus.pictures`): filled here from every snapshot's pages.
    kept_pictures: State<shelf::Held<{ peersus_core::profile::KEPT_EDGE }>>,
    /// The digest of this device's own picture, written here and read by the root, like the name.
    own_picture: State<Option<peersus_core::profile::Digest>>,
    /// The file that is offered to this person now, written here from every snapshot and drawn by the root over the window.
    offer: State<Option<view::Offered>>,
    /// Where the window is on its way to after a file was accepted: written here when the owner reports one accepted, carried out by the root
    /// (the page) and by Receive (its scroll).
    receive_jump: State<view::ReceiveJump>,
    /// Whether Settings is to be opened on Extensions for a store link: written here, carried out by the root.
    store_jump: State<bool>,
    /// The page to show for a tapped notification: written here, carried out by the root.
    page_jump: State<Option<Page>>,
    /// The address the system opened the window with (a store link), shown once when the window is attached.
    opened_with: Option<String>,
    /// The names of the pages extensions add to the bottom bar: written here from every snapshot, read by the root.
    contributed: State<Vec<NavEntry>>,
    /// The pages the shared Extensions place chooses between: written here from every snapshot, read by the header's chooser.
    extension_choices: State<Vec<contributed_page::Choice>>,
    /// Which page the shared Extensions place shows, chosen in the header.
    extension_page: State<Option<(peersus_core::plugins::PluginId, String)>>,
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
            first.page_views =
                layers::Mirror::default().ready_pages(first.page_views, &[], &initial.kept_layers);
            first.transfers_at_start = view::ids_at_start(&first.transfers);
            PublishedView::from(first)
        });
        let mut alive = use_state(|| true);
        // This window's own crash capture, as the person's preference says (the owner keeps its own).
        diagnostics_page::use_window_capture(view.read().settings.crash_capture);
        // Store links. The one the window was opened with is shown once, now that it is attached; one asked of the owner later
        // (`Command::ShowStoreEntry`, `store.asked`) is acted on once per number, and a number this window opened on is old news.
        let store_link = try_consume_context::<plugin_manager::StoreLink>();
        let mut store_jump = self.store_jump;
        let opened_with = self.opened_with.clone();
        use_hook(move || {
            if let (Some(plugin), Some(plugin_manager::StoreLink(mut asked))) = (
                opened_with
                    .as_deref()
                    .and_then(peersus_core::model::store_link_plugin),
                store_link,
            ) {
                asked.set(Some(plugin));
                store_jump.set(true);
            }
        });
        let mut acted_on = use_state(|| initial.store.asked.as_ref().map(|asked| asked.serial));
        if let Some(asked) = &view.read().store.asked
            && *acted_on.peek() != Some(asked.serial)
        {
            acted_on.set(Some(asked.serial));
            if let Some(plugin_manager::StoreLink(mut wanted)) = store_link {
                wanted.set(Some(asked.plugin.clone()));
                store_jump.set(true);
            }
        }
        // A tapped notification that opens (`Command::OpenNotification`, the owner's `opened`): its sender's page, once per number,
        // with the window brought forward. A page in the bar is shown; one that lives only in the extension's settings is reached
        // the way a store link reaches an installed extension, at its row in Settings.
        let mut page_jump = self.page_jump;
        let mut extension_page = self.extension_page;
        let mut opened_on = use_state(|| initial.opened.as_ref().map(|asked| asked.serial));
        let asked = view.read().opened.clone();
        if let Some(asked) = asked
            && *opened_on.peek() != Some(asked.serial)
        {
            opened_on.set(Some(asked.serial));
            let together = view.peek().settings.extension_pages_together;
            let published = view.peek();
            let primary = contributed_page::pages_here(
                &published,
                peersus_api::contributions::Placement::Primary,
            )
            .into_iter()
            .find(|(plugin, _)| plugin.id == asked.plugin);
            match primary {
                Some((plugin, page)) if together => {
                    extension_page.set(Some((plugin.id.clone(), page.id.clone())));
                    page_jump.set(Some(Page::Extensions));
                }
                Some((plugin, page)) => page_jump.set(Some(Page::Contributed(
                    navigation::tab_key(plugin.id.as_str(), &page.id),
                ))),
                None => link.send(Command::ShowStoreEntry {
                    plugin: asked.plugin,
                }),
            }
            #[cfg(not(target_os = "android"))]
            Platform::get().with_window(Platform::window_id(), |window| {
                window.set_minimized(false);
                window.focus_window();
            });
        }
        let mut chosen: State<Vec<String>> = use_state(Vec::new);
        let extension_page = self.extension_page;
        let mut extension_choices = self.extension_choices;
        // A received file that could not be put in the folder the person chose: said on screen, since it is kept in the program's
        // own folder and the person would otherwise think it arrived where they asked.
        let mut not_delivered: State<Option<String>> = use_state(|| None);
        let mut choice = self.choice;
        let mut language = self.language;
        let mut scale = self.scale;
        let mut accent = self.accent;
        let mut custom = self.custom;
        let mut reduced_motion = self.reduced_motion;
        let mut bar_position = self.bar_position;
        let mut keep_icons = self.keep_icons;
        let mut two_fingers = self.two_fingers;
        let mut device_name = self.device_name;
        let held = self.pictures;
        let kept_held = self.kept_pictures;
        let mut own_picture = self.own_picture;
        let mut offer = self.offer;
        let mut receive_jump = self.receive_jump;
        let mut contributed = self.contributed;
        let mut named = self.named;
        named.set_if_modified(!view.peek().needs_a_name());

        // Redraw from the snapshots the owner publishes. The view is replaced only when it
        // changed, so a quiet second leaves the tree alone.
        let watching = link.client().clone();
        #[cfg(all(unix, not(target_os = "android")))]
        let restarting = link.clone();
        #[cfg(not(target_os = "android"))]
        let closing = link.clone();
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
        let mut kept_layers = self.kept_layers;
        use_hook(move || {
            // The shelf lives in this task and is shared only with the fetches it starts, which run on the
            // same thread as the window: what the rest of the window sees is `held`, a state.
            let shelf = shelf::Driven::default();
            let kept_shelf = shelf::Driven::<{ peersus_core::profile::KEPT_EDGE }>::default();
            // What extensions keep for their drawings, kept here beside the shelf and asked for only when a snapshot names a set this
            // window does not hold: each layer crosses once for each change, and none of it travels in a snapshot.
            let mut layers = layers::Mirror::default();
            // The watch channel needs no particular executor to be awaited on.
            spawn(async move {
                let mut updates = watching.subscribe();
                loop {
                    let latest = updates.borrow_and_update().clone();
                    let mut next = View::of(&latest);
                    next.transfers_at_start = view.peek().transfers_at_start.clone();
                    // A file that arrived whole into the program's staging is put in the folder the person chose for it, by the
                    // platform: once, since the folder is given up when asked. A file that could not be put there stays in the
                    // staging (nothing sent to this person is lost); what the platform says of each hand-over is taken so the
                    // list does not grow, and is written to the diagnostics.
                    if let Some(sink) = &sink {
                        for handoff in
                            view::handoffs(&next.transfers, |id| sink.picked.tree_for(id))
                        {
                            (sink.deliver)(&handoff);
                        }
                        // A transfer that ended without arriving goes nowhere: the folder remembered for it is let go of.
                        for item in next.transfers.iter().filter(|item| {
                            item.incoming
                                && matches!(
                                    item.phase,
                                    view::Phase::Cancelled | view::Phase::Failed(_)
                                )
                        }) {
                            sink.picked.forget(&item.id);
                        }
                        // Results are taken on the clock (above), not here: a result that arrives after the last snapshot is not lost.
                    }
                    // The language the owner says the windows are shown in: its file is asked for once for each revision,
                    // before the words change, so the window never shows half of one language and half of another.
                    language.set_if_modified(i18n::follow(&watching, &latest.languages).await);
                    // Before the page that places them is drawn from this snapshot: the layers it names, fetched once for each change.
                    // Bounded, and nothing is asked for when the stamps are the ones this window holds.
                    for _ in 0..8 {
                        let Some(job) = layers.next(&latest.kept_layers) else {
                            break;
                        };
                        if !layers::follow(&watching, &mut layers, job).await {
                            // The index may already be superseded. Leave that plugin's previous
                            // page up, and retry on the next owner snapshot, not in a loop.
                            break;
                        }
                    }
                    next.page_views = layers.ready_pages(
                        next.page_views,
                        &view.peek().page_views,
                        &latest.kept_layers,
                    );
                    kept_layers.set_if_modified(layers.held());
                    let pictures_named = shelf::named_by(&next);
                    let kept_named = shelf::kept_named_by(&next);
                    if **view.peek() != next {
                        contributed_page::forget_drawings_without_pages(&next);
                        choice.set_if_modified(next.settings.theme);
                        scale.set_if_modified(next.settings.scale);
                        accent.set_if_modified(next.settings.accent);
                        custom.set_if_modified(next.settings.custom_theme.clone());
                        reduced_motion.set_if_modified(next.settings.reduced_motion);
                        bar_position.set_if_modified(next.settings.desktop_bar_position);
                        keep_icons.set_if_modified(next.settings.collapse_to_icons);
                        two_fingers.set_if_modified(next.settings.two_finger_settings);
                        device_name.set_if_modified(next.settings.device_name.clone());
                        named.set_if_modified(!next.needs_a_name());
                        own_picture.set_if_modified(next.profile_picture);
                        offer.set_if_modified(view::offered_now(&next));
                        if view::accepted_between(&view.peek().transfers, &next.transfers) {
                            receive_jump.set(view::ReceiveJump::ToPage);
                        }
                        contributed.set_if_modified(contributed_entries(&next));
                        extension_choices.set_if_modified(contributed_page::choices(&next));
                        view.set(next.into());
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
                    for fetching in shelf.follow(&pictures_named, fetch, publish) {
                        spawn(fetching);
                    }
                    let fetch_kept = {
                        let client = watching.clone();
                        move |digest| {
                            let client = client.clone();
                            async move {
                                match client.command(Command::FetchKeptPicture { digest }).await {
                                    Ok(Reply::KeptPicture(picture)) => Some(picture),
                                    _ => None,
                                }
                            }
                        }
                    };
                    let publish_kept = move |now| {
                        let mut held = kept_held;
                        held.set_if_modified(now);
                    };
                    for fetching in kept_shelf.follow(&kept_named, fetch_kept, publish_kept) {
                        spawn(fetching);
                    }
                    if updates.changed().await.is_err() {
                        // The refresh task ended: the owner went away. One that went so that an update runs comes
                        // back by itself, and this window with it; the wait ends only when that did not happen.
                        #[cfg(all(unix, not(target_os = "android")))]
                        if view.peek().update.status
                            == peersus_core::model::UpdateStatus::Restarting
                        {
                            restarting.follow_restart().await;
                        }
                        // An owner that was told to stop is the application being closed: the window goes with it. One that ended
                        // in any other way is started again here, with nobody having to: the window is replaced by a new one, which
                        // starts the owner as any window does. Only when that is not done (it was, too many times in a few minutes)
                        // does the window say the owner stopped.
                        #[cfg(not(target_os = "android"))]
                        match closing.owner_ending().await {
                            Some(true) => std::process::exit(0),
                            Some(false) => closing.start_again_unasked(),
                            // Not a window attached to an owner: nothing to close with, nothing to start.
                            None => {}
                        }
                        alive.set(false);
                        break;
                    }
                }
            });
        });

        let palette = self.palette;
        if !alive() {
            return Unreachable {
                reason: Offline::NotRunning,
                palette,
            }
            .into();
        }
        let current = view.read().clone();
        let mut slot_height = use_state(|| 0.);
        let wide = self.form == view::Form::Wide;
        let mut page = rect().expanded().vertical().maybe(wide, |el| {
            el.on_sized(move |e: Event<SizedEventData>| {
                slot_height.set_if_modified(e.area.height());
            })
        });
        // Until the device has a name, there is nothing else on screen but the question.
        let screenshot_run = std::env::var("PEERSUS_PAGE").is_ok_and(|page| !page.is_empty());
        if current.needs_a_name() && !screenshot_run {
            return page
                .child(Welcome {
                    link: link.clone(),
                    palette,
                    form: self.form,
                })
                .into();
        }
        // Messages float over the page, newest last, and go away by themselves: they never push the page down.
        let mut said: Vec<String> = current.errors.clone();
        let owned = said.len();
        said.extend(view::extension_health_notices(
            &current.plugins,
            &current.store,
            i18n::language(),
        ));
        if let Some(text) = not_delivered.read().clone() {
            said.push(text);
        }
        page = page.child(Notices {
            link: link.clone(),
            posted: current.notifications.clone(),
            approvals: current.approvals.clone(),
            said,
            owned,
            said_count: current.errors_said,
            palette,
            clearance: self
                .form
                .window_bar(current.settings.desktop_bar_position)
                .clearance(),
            slot_height: wide.then_some(slot_height),
            plugins: current
                .plugins
                .iter()
                .map(|plugin| {
                    (
                        plugin.id.as_str().to_owned(),
                        plugin
                            .package
                            .as_ref()
                            .map_or_else(|| plugin.id.as_str().to_owned(), |p| p.name.clone()),
                    )
                })
                .collect(),
        });
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
                now: self.now,
            }
            .into(),
            Page::Receive => ReceivePage {
                link: link.clone(),
                view: current.clone(),
                palette,
                form: self.form,
                room: self.room,
                camera: self.camera.clone(),
                jump: self.receive_jump,
            }
            .into(),
            Page::Transfers => TransfersPage {
                link: link.clone(),
                view: current.clone(),
                palette,
                form: self.form,
            }
            .into(),
            Page::Peers => DevicesPage {
                link: link.clone(),
                view: current.clone(),
                pictures: held.read().clone(),
                palette,
                form: self.form,
                room: self.room,
                camera: self.camera.clone(),
                file_picker: self.file_picker.clone(),
            }
            .into(),
            // An extension's page is a page: drawn in the same frame as the application's own, which keeps the room of the
            // header's pieces free at its top, scrolls, and hides the header as it is read down.
            // A covered extension is not a second active layout surface. Settings
            // may mount its own embedded copy with entirely different dimensions.
            Page::Extensions | Page::Contributed(_)
                if self.form.settings_places_come_from_the_top() && *self.menu_open.read() =>
            {
                rect().into()
            }
            Page::Extensions => frame(
                palette,
                self.form,
                contributed_page::Shared {
                    link: link.clone(),
                    view: current.clone(),
                    palette,
                    pictures: held.read().clone(),
                    chosen: extension_page,
                },
            )
            .into_element(),
            Page::Contributed(key) => {
                match contributed_page::pages_here(
                    &current,
                    peersus_api::contributions::Placement::Primary,
                )
                .into_iter()
                .find(|(plugin, page)| tab_key(plugin.id.as_str(), &page.id) == key)
                {
                    Some((plugin, page)) => frame(
                        palette,
                        self.form,
                        contributed_page::Body {
                            link: link.clone(),
                            view: current.clone(),
                            plugin: plugin.clone(),
                            page: page.clone(),
                            pictures: held.read().clone(),
                            palette,
                            embedded: false,
                        },
                    )
                    .into_element(),
                    None => rect().into(),
                }
            }
            Page::Settings => SettingsPage {
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
                inset_top: self.inset_top,
                inset_bottom: self.inset_bottom,
            }
            .into(),
        };
        // While the Settings overlay is open on a phone, the page under it takes no pointer at all, a touch, a drag or a wheel: an element that is
        // not interactive is skipped by the toolkit before any event is delivered, and so are all of its children (the scrolling of the page
        // included). The overlay is the page's sibling and not its child, so it stays live. A person who closes the overlay has the page back
        // exactly as it was, where it was.
        let under_overlay = self.form.settings_places_come_from_the_top() && *self.menu_open.read();
        // Clip every page to its slot. The window bar overlays it; the mobile navigation and
        // in-flow offer remain outside the slot.
        // While the person's switch for nearness is on, a window that is open says so to the owner every few seconds, on whatever page it is:
        // only then are devices nearby listened for (see `NearbyBeat`). What this device is announced as to them is the page's: to be paired
        // with while Peers is shown, to be sent files while Receive is, and nothing at all on any other page. Two different things. A page
        // that lies under the Settings overlay of a phone is not the page its person looks at.
        let takes = match self.page {
            _ if under_overlay => None,
            Page::Peers => Some(peersus_core::nearby::Takes::Pairing),
            Page::Receive => Some(peersus_core::nearby::Takes::Files),
            _ => None,
        };
        let mut page = page.child(
            rect()
                .expanded()
                .overflow(Overflow::Clip)
                .interactive(!under_overlay)
                .child(body)
                .maybe_child(current.settings.nearby_send.then(|| NearbyBeat {
                    link: link.clone(),
                    says: NearbySays::Seek(takes),
                })),
        );
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
                    inset_top: self.inset_top,
                    inset_bottom: self.inset_bottom,
                }
                .into(),
                on_close: EventHandler::new(move |()| menu_open.set(false)),
            });
        }
        // The agreement of a package the person asked to install, over everything, while they read it. It is drawn only for a package the
        // owner is still holding: if that one is gone (installed, given up, refused) the request is forgotten, so that it can never come
        // back by itself when the same name is shown again. Asked for an installed extension (its Details), the agreement it was
        // accepted with is shown again to be read, with nothing to accept.
        if let Some(agreement_overlay::Ask(mut asked)) =
            try_consume_context::<agreement_overlay::Ask>()
        {
            let wanted = asked.read().clone();
            if let Some(plugin) = wanted {
                let staged = current.staged.iter().find(|staged| staged.id == plugin);
                let shown = staged.map(|staged| (staged, false)).or_else(|| {
                    current
                        .plugins
                        .iter()
                        .find(|installed| installed.id == plugin)
                        .map(|installed| (installed, true))
                });
                match shown {
                    Some((package, review)) => {
                        page = page.child(agreement_overlay::Overlay {
                            link: link.clone(),
                            plugin: package.clone(),
                            // The application's own warning about a package from outside the store is accepted once, by the person, not once for
                            // each extension: the extension's licence and notice are the extension's, and are read for each.
                            risk_accepted: current.settings.outside_store_risk_accepted,
                            palette,
                            window: (self.room, self.room_height),
                            review,
                            on_close: EventHandler::new(move |()| asked.set(None)),
                        });
                    }
                    None => asked.set(None),
                }
            }
        }
        page.into()
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
                    patch: Box::new(SettingsPatch {
                        device_name: Some(value),
                        ..Default::default()
                    }),
                });
            }
        };
        // The field fills the room it has, up to a width a person can read a name in. It is never given a
        // fixed width: on a phone that width is wider than the screen, and the button beside it was off it.
        // Beside the button it takes what the button leaves (`flex`), as the device name field in Settings does:
        // `fill` would take the whole row and squeeze the button to a sliver.
        let field = field::field(field::Profile::TEXT, name)
            .placeholder(crate::i18n::tr("For example: Alex's laptop"));
        let button = Button::new()
            .filled()
            .on_press(save)
            .child(crate::i18n::tr("Continue"));
        let ask: Element = if form.ask_stacks_field_and_button() {
            // One above the other, the button as wide as the field: easy to reach, and always on screen.
            rect()
                .width(Size::fill())
                .spacing(12.)
                .child(field.width(Size::fill()))
                .child(button.width(Size::fill()))
                .into()
        } else {
            rect()
                .width(Size::fill())
                .horizontal()
                .content(Content::flex())
                .spacing(10.)
                .child(field.width(Size::flex(1.)))
                .child(button)
                .into()
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

/// The big two-choice button (Pair, Connect), as on the site: a panel of the page's own surface with an outline, its accent in the
/// dot and its words in the text colour, in either theme. A solid button of the opposite shade was a white slab on a dark page.
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
            .width(Size::fill())
            .padding((14., 22.))
            .horizontal()
            .main_align(Alignment::center())
            .cross_align(Alignment::center())
            .spacing(12.)
            .corner_radius(12.)
            .background(if enabled && hovered() {
                palette.raised
            } else {
                palette.surface
            })
            .border(
                Border::new()
                    .fill(palette.border)
                    .width(1.)
                    .alignment(BorderAlignment::Inner),
            )
            .on_pointer_enter(move |_| hovered.set(true))
            .on_pointer_leave(move |_| hovered.set(false))
            .on_press(move |_| {
                if enabled {
                    on_press.call(());
                }
            })
            .child(
                rect()
                    .width(Size::px(20.))
                    .height(Size::px(20.))
                    .corner_radius(10.)
                    .background(if enabled { self.accent } else { palette.border }),
            )
            .child(
                text::caption(i18n::tr(self.text))
                    .color(if enabled { palette.text } else { palette.muted })
                    .font_size(17.)
                    .font_weight(FontWeight::BOLD),
            )
    }
}

fn section(text: &'static str, palette: Palette) -> Label {
    mono(i18n::tr(text), palette.muted, 12.)
}

/// Where an update of the application has got to, and the one thing that can be pressed about it.
fn update_card(link: &Link, panel: &view::UpdatePanel, palette: Palette) -> Element {
    let mut card = rect()
        .width(Size::fill())
        .padding(18.)
        .spacing(8.)
        .corner_radius(12.)
        .background(palette.surface)
        .border(
            Border::new()
                .fill(palette.border)
                .width(1.)
                .alignment(BorderAlignment::Inner),
        )
        .child(
            label()
                .text(i18n::format(
                    i18n::language(),
                    panel.line,
                    &[("version", panel.version.as_str())],
                ))
                .color(palette.text)
                .font_size(15.),
        );
    // What "Update now" downloads, said before it is pressed.
    if panel.bytes > 0 {
        card = card.child(mono(view::size_in_words(panel.bytes), palette.muted, 12.));
    }
    if let Some(said) = &panel.said {
        card = card.child(
            label()
                .text(i18n::tr(said).to_owned())
                .color(palette.muted)
                .font_size(13.),
        );
    }
    if let Some(action) = panel.action {
        let link = link.clone();
        let button = Button::new()
            .on_press(move |_| {
                link.send(match action {
                    view::UpdateAction::Check => Command::UpdateCheck {},
                    view::UpdateAction::Install => Command::UpdateInstall {},
                    view::UpdateAction::Restart => Command::UpdateRestart {},
                })
            })
            .child(i18n::tr(action.label()));
        card = card.child(if action == view::UpdateAction::Check {
            button
        } else {
            button.filled()
        });
    }
    card.into()
}

/// Who made a package and where to read about it, as text a person reads before trusting it: the author and version, and
/// the link. What the package SAYS about itself, shown as such; it is never made something to press, because following an
/// address out of a list is how a person is led somewhere else.
fn package_lines(card: Rect, package: &peersus_api::package::Package, palette: Palette) -> Rect {
    let mut card = card;
    if !package.author.is_empty() {
        card = card.child(mono(
            i18n::format(
                i18n::language(),
                "by {author} · version {version}",
                &[("author", &package.author), ("version", &package.version)],
            ),
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
    Scrolling {
        palette,
        form,
        content: content.into_element(),
        see_through: false,
        fixed_heading: 0.,
        tops: None,
        go_to: None,
    }
}

/// The scrolling area of a page, with its OWN scroll position (a page that is left starts again at its top) and the header's hiding
/// told from it: on a phone the bar goes once the page is read down and comes back once it is scrolled up (see [`view::HeaderHide`]).
/// The position is read after each change of it, here, and what the header shows is written to the state the root provides.
#[derive(PartialEq)]
struct Scrolling {
    palette: Palette,
    form: view::Form,
    content: Element,
    /// Draw no background of its own, so that what is behind it (the panel of the Settings overlay, with its lifted colour, its outline
    /// and its rounded lower corners) shows through. Every page of the application draws its background, as it always did.
    see_through: bool,
    /// A fixed heading inside this viewport (Wide Settings), measured without the bar.
    fixed_heading: f32,
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
        let wide = form == view::Form::Wide;
        let mut room = use_state(|| None::<contributed_page::PageArea>);
        provide_context(contributed_page::PageRoom { room, wide });
        let (mut top, bottom) = if self.see_through {
            (0., 0.)
        } else {
            let saved = try_consume_context::<State<DesktopBarPosition>>()
                .map(|state| state())
                .unwrap_or_default();
            form.window_bar(saved).clearance()
        };
        top += self.fixed_heading;
        let vertical_padding = form.page_vertical_padding();
        let measured = move |e: freya::prelude::Event<SizedEventData>| {
            let first = e.visible_area.min_y() + top;
            room.set_if_modified(Some(contributed_page::PageArea {
                menu_bounds: [
                    e.visible_area.min_x(),
                    first,
                    e.visible_area.max_x(),
                    (e.visible_area.max_y() - bottom).max(first),
                ],
                // What is left between the document's own padding, which also clears the window bar above and below:
                // a page that fills the room is exactly that tall and never makes the document scroll.
                filling_height: (e.area.height() - 2. * vertical_padding - top - bottom).max(0.),
            }));
        };
        if let Some(mut asked) = self.go_to {
            // Read in the way that this component is drawn again for, so that a request written by the search is seen at once. Copied out
            // first, so that the read is over before the request is taken off. No place asked for goes nowhere: it does not take the person
            // to the top of the page.
            let place: Option<f32> = *asked.read();
            if let Some(place) = place {
                controller.scroll_to_y(f64::from(place));
                asked.set(None);
            }
        }
        let hide = try_consume_context::<State<view::HeaderHide>>();
        // A finger that has been held on a circle long enough to be a hold is the circle's, and the page under it stays where it is: it
        // must not follow a finger that trembles, which carried the page, and the bar over it, up and down under the hold. Before that a
        // finger that moves is a swipe, and the page scrolls as it always did.
        let holding =
            try_consume_context::<State<HoldingCircle>>().is_some_and(|state| state.read().0);
        // The bar hides as a PAGE is read down. The panel of Settings is not a page under the bar but a layer over it, so what is scrolled in it
        // says nothing to the bar: only the pages (Send, Receive, Peers) write how much of it is hidden.
        if form.header_hides_on_scroll()
            && !self.see_through
            && let Some(mut hide) = hide
        {
            // Subscribe the wrapper only on pages whose header follows the scroll.
            // Settings has a fixed header; its ScrollView already follows these samples.
            let top = controller.animated_position(controller.position()).y as f32;
            let next = hide.peek().after(top);
            if next != *hide.peek() {
                hide.set(next);
            }
        }
        // Both forms clear the bar inside the scrolled document. The viewport still reaches the
        // edge, so content passes under the painted pieces and is visible in the gaps.
        let above = vertical_padding + top;
        // The icons of the groups stand in a column over the right edge of the area: the page keeps that column free, so no card,
        // button or title is drawn under them.
        let right = if self.tops.is_some() {
            form.page_side_margin().max(GroupMarks::CLEAR)
        } else {
            form.page_side_margin()
        };
        let scroll = ScrollView::new_controlled(controller)
            .expanded()
            .drag_scrolling(!holding)
            .child(
                rect()
                    .width(Size::fill())
                    .padding((
                        above,
                        right,
                        vertical_padding + bottom,
                        form.page_side_margin(),
                    ))
                    .spacing(18.)
                    .background(if self.see_through {
                        Color::TRANSPARENT
                    } else {
                        Color::from(palette.background)
                    })
                    .child(self.content.clone()),
            );
        let viewport = rect()
            .expanded()
            .on_sized(measured)
            .maybe(wide, |el| el.focus_viewport_insets((top, 0., bottom, 0.)));
        let Some(tops) = self.tops else {
            return viewport.child(scroll).into_element();
        };
        // The scrollbar is still the toolkit's own, drawn by it; the group icons are added on its track.
        let offsets = tops.read().clone();
        viewport
            .child(scroll.scrollbar(move |context: ScrollBarContext| {
                GroupMarks::on(palette, offsets.clone(), context)
            }))
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
    /// What the page keeps free at its right edge for that column: the column and a small gap before it. No more: a phone's page at the
    /// narrowest window (300) still holds a translated word of a row's name whole beside its value.
    const CLEAR: f32 = Self::COLUMN + 2.;

    fn on(palette: Palette, offsets: Vec<f32>, context: ScrollBarContext) -> Element {
        let bar = ScrollBar::new(context.clone());
        let viewport = context.viewport_size.height as f32;
        let content = context.content_size.height as f32;
        let range = view::scroll_range(content, viewport);
        if range <= 0. || offsets.is_empty() || viewport <= Self::NODE_CURRENT * 2. {
            return bar.into();
        }
        let accent = palette.send;
        let scrolled = context.scroll_position.y as f32;
        let current = view::current_group(&offsets, scrolled, Self::LEAD, scrolled + 1. >= range);
        // The icons go down the whole length of the toolkit's scrollbar, the track being the area less its margin at both ends.
        let track = (viewport - Self::BAR_MARGIN * 2.).max(0.);
        let controller = context.scroll_controller;
        // On the layer of the toolkit's own scrollbar (999), above the cards and controls of the page, which would otherwise be drawn over it.
        let mut marks = rect()
            .position(Position::new_absolute().top(0.).right(0.))
            .layer(999)
            .width(Size::px(Self::COLUMN))
            .height(Size::px(viewport));
        // Where each icon sits: the share of the bar that its group's top is of the whole page, so that a long group has room on the bar and short
        // ones are near each other. Never closer than one icon to the one before, so that two do not lie on one another, and never past the end.
        let content = content.max(1.);
        let room = (track - Self::NODE_CURRENT).max(0.);
        let mut places: Vec<f32> = offsets
            .iter()
            .map(|top| (top / content).clamp(0., 1.) * track)
            .collect();
        for index in 1..places.len() {
            places[index] = places[index].max(places[index - 1] + Self::NODE);
        }
        for index in (0..places.len().saturating_sub(1)).rev() {
            places[index] = places[index].min(places[index + 1] - Self::NODE);
        }
        for place in places.iter_mut() {
            *place = place.clamp(0., room);
        }
        for (index, &which) in SettingsTab::shown().iter().enumerate() {
            let Some(place) = places.get(index).copied() else {
                break;
            };
            let is_current = current == Some(index);
            // Lit: the group being read and the ones before it.
            let passed = current.is_some_and(|read| index <= read);
            let (size, icon) = if is_current {
                (Self::NODE_CURRENT, Self::ICON_CURRENT)
            } else {
                (Self::NODE, Self::ICON)
            };
            let fill = if passed || is_current {
                accent
            } else {
                palette.raised
            };
            let ink = if passed || is_current {
                glyph::ink_on(accent)
            } else {
                glyph::resting_ink(&palette)
            };
            let target = offsets.get(index).copied().unwrap_or(0.);
            let mut controller = controller;
            marks = marks.child(
                rect()
                    .position(
                        Position::new_absolute()
                            .top(Self::BAR_MARGIN + Self::NODE_CURRENT / 2. + place - size / 2.)
                            .left((Self::COLUMN - size) / 2.),
                    )
                    .width(Size::px(size))
                    .height(Size::px(size))
                    .corner_radius(size / 2.)
                    .center()
                    .background(fill)
                    .border(
                        Border::new()
                            .fill(if passed || is_current {
                                accent
                            } else {
                                palette.border
                            })
                            .width(2.)
                            .alignment(BorderAlignment::Inner),
                    )
                    // A press on an icon scrolls to its group's offset from the top.
                    .on_press(move |_| {
                        controller.scroll_to_y(f64::from(target));
                    })
                    .child(glyph::picture_on(
                        glyph::name_of_tab(which),
                        icon,
                        fill,
                        ink,
                        fill,
                    )),
            );
        }
        rect().child(bar).child(marks).into()
    }
}
