//! The foreground seen by any contributed element. Its permission to observe is not inferred
//! from a mounted page or a polling timer. Each native input is event-driven.
use freya::prelude::*;
use tokio::sync::watch;

/// The activity and screen state on a platform whose renderer cannot report suspension.
/// A platform publishes changes to either fact, even if the window keeps its keyboard focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Activity {
    pub resumed: bool,
    pub screen_on: bool,
}
impl Activity {
    const DESKTOP: Self = Self {
        resumed: true,
        screen_on: true,
    };
    const HIDDEN: Self = Self {
        resumed: false,
        screen_on: false,
    };
}

#[derive(Clone, Copy)]
struct Foreground {
    active: Memo<bool>,
    #[cfg(not(target_os = "android"))]
    drawable: State<bool>,
}

/// Combine the window and lifecycle signals once at the root of the application.
pub(crate) fn provide(lifecycle: Option<watch::Receiver<Activity>>) {
    let window = Platform::get().current_window();
    let activity = watch_activity(lifecycle);
    let focus = window.is_app_focused;
    let size = window.root_size;
    #[cfg(not(target_os = "android"))]
    let mut drawable = use_state(|| false);
    #[cfg(target_os = "android")]
    let drawable = use_state(|| true);
    #[cfg(not(target_os = "android"))]
    {
        let can_show = *focus.read() && size.read().width > 0. && size.read().height > 0.;
        use_side_effect_with_deps(&can_show, move |can_show| {
            if !*can_show {
                drawable.set_if_modified(false);
                return;
            }
            // Focus/resize are native events, not polling. Check minimization and mapped
            // visibility where Winit supplies them; on compositors that can't answer either,
            // the fresh native focus flag remains the authority.
            Platform::get().with_window(Platform::window_id(), move |window| {
                drawable.set_if_modified(
                    window.has_focus()
                        && window.is_minimized() != Some(true)
                        && window.is_visible() != Some(false),
                );
            });
        });
    }
    combine(focus, size, drawable, activity);
}

fn watch_activity(lifecycle: Option<watch::Receiver<Activity>>) -> State<Activity> {
    let initial = lifecycle.as_ref().map_or(
        if cfg!(target_os = "android") {
            Activity::HIDDEN
        } else {
            Activity::DESKTOP
        },
        |source| *source.borrow(),
    );
    let mut state = use_state(move || initial);
    use_future(move || {
        let mut lifecycle = lifecycle.clone();
        async move {
            let Some(source) = lifecycle.as_mut() else {
                return;
            };
            loop {
                state.set_if_modified(*source.borrow_and_update());
                if source.changed().await.is_err() {
                    state.set_if_modified(Activity::HIDDEN);
                    break;
                }
            }
        }
    });
    state
}

fn combine(
    focus: State<bool>,
    size: State<Size2D>,
    drawable: State<bool>,
    activity: State<Activity>,
) {
    let active = use_memo(move || {
        let size = *size.read();
        let active = *activity.read();
        *focus.read()
            && *drawable.read()
            && size.width > 0.
            && size.height > 0.
            && active.resumed
            && active.screen_on
    });
    provide_context(Foreground {
        active,
        #[cfg(not(target_os = "android"))]
        drawable,
    });
}
/// A minimize action is known before the window manager sends its focus event.
#[cfg(not(target_os = "android"))]
pub(crate) fn desktop_drawable() -> Option<State<bool>> {
    try_consume_context::<Foreground>().map(|front| front.drawable)
}
/// Missing root context is never evidence of foreground (e.g. a standalone measurement).
pub(crate) fn active() -> bool {
    try_consume_context::<Foreground>().is_some_and(|value| *value.active.read())
}

#[cfg(all(test, feature = "headless", not(target_os = "android")))]
pub(crate) fn fixture(
    focus: State<bool>,
    drawable: State<bool>,
    lifecycle: watch::Receiver<Activity>,
) {
    let size = use_state(|| Size2D::new(400., 250.));
    let activity = watch_activity(Some(lifecycle));
    combine(focus, size, drawable, activity);
}
