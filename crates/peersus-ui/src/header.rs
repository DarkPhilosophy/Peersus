//! The application's own title bar: the mark and the name, the places to open, state, the menu and the window's buttons.
use super::*;

/// The application's own title bar: mark, name, the two main screens, state, menu, window
/// buttons. The whole bar drags the window; the tabs, menu and dots do not.
/// What of the header is shown beyond the mark and the name. Two gates that always go together: the places to
/// open, and the menu that opens the rest. Both are off until the device has a name, since there is nothing to open.
#[derive(Clone, Copy)]
pub(crate) struct Shown<'a> {
    /// The places across the top, from [`nav_entries`] (a desktop; a phone has them along the bottom): none when hidden.
    pub(crate) tabs: &'a [NavEntry],
    /// The menu button.
    pub(crate) menu: bool,
    /// How much of the bar is out of sight, in pixels, as it goes or comes back on its own motion; nothing on a desktop.
    pub(crate) hidden: f32,
    /// Whether the person asked for no decorative motion: then the name is there at once and is not written.
    pub(crate) reduced_motion: bool,
    /// Whether the person has done anything in the window yet (see `Brand`).
    pub(crate) awake: bool,
    /// Available logical width, for keeping the header's controls within the window.
    pub(crate) width: f32,
    /// Which page the shared Extensions place shows, chosen right after the mark: only while that place is open.
    pub(crate) chooser: Option<&'a Element>,
}

pub(crate) fn header(
    palette: Palette,
    active: Page,
    connected: bool,
    shown: Shown<'_>,
    form: view::Form,
    on_select: EventHandler<Page>,
    menu_open: State<bool>,
) -> impl IntoElement {
    // Keep navigation icons and window controls at narrow widths; status words and spare padding give way.
    let dense = form == view::Form::Wide
        && (!shown.width.is_finite() || shown.width < 420. + shown.tabs.len() as f32 * 160.);
    // Where the bar gives way to a page that is read down (a phone), the mark and the way to Settings do not go with it: they stay in
    // the two top corners as tabs of the bar, so the bar is split into its two ends and its middle, and only the middle goes.
    let split = form.header_hides_on_scroll();
    let side = if dense {
        14.
    } else {
        form.header_side_padding()
    };
    let brand = Brand {
        palette,
        awake: shown.awake,
        reduced_motion: shown.reduced_motion,
    };
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
    if form.header_says_connected_in_words() && !dense {
        status = status.child(mono(
            i18n::tr(if connected {
                "CONNECTED"
            } else {
                "NOT RUNNING"
            }),
            palette.muted,
            11.,
        ));
    }
    // The menu is offered only when it opens something: until the device has a name there is nothing to open.
    if shown.menu {
        status = status.child(MenuButton {
            palette,
            on_select: on_select.clone(),
            open: menu_open,
            sheet: form.settings_places_come_from_the_top(),
        });
    }
    // The bar is in pieces with the empty middle between them: `|mark)` at the left, `(state, Settings|` at the right, and on a
    // desktop the places in their own `(places)` between. A piece is square where it meets the window's edge and round where it meets
    // the empty middle, like the two halves of a bracket.
    let height = if split {
        view::HEADER_TAB
    } else {
        view::HEADER_HEIGHT
    };
    let round = height / 2.;
    let piece = |at_left_edge: bool, at_right_edge: bool| {
        let corner = |edge: bool| if edge { 0. } else { round };
        let line = |edge: bool| if edge { 0. } else { 1. };
        // Away from an edge the content keeps clear of the curve.
        let inset = |edge: bool| if edge { side } else { round * 0.75 };
        rect()
            .height(Size::px(height))
            .horizontal()
            .cross_align(Alignment::center())
            .spacing(if dense { 6. } else { 10. })
            .padding((0., inset(at_right_edge), 0., inset(at_left_edge)))
            .background(palette.header)
            .corner_radius((
                corner(at_left_edge),
                corner(at_right_edge),
                corner(at_right_edge),
                corner(at_left_edge),
            ))
            .border(
                Border::new()
                    .fill(palette.border)
                    .width(BorderWidth {
                        top: 0.,
                        right: line(at_right_edge),
                        bottom: 1.,
                        left: line(at_left_edge),
                    })
                    .alignment(BorderAlignment::Inner),
            )
            .window_drag()
    };
    // While the shared Extensions place is open, which of its pages is shown is chosen right after the mark.
    let left = piece(true, false)
        .child(brand)
        .maybe_child(shown.chooser.cloned());
    let mut right = piece(false, true).child(status);
    if form.header_has_window_buttons() {
        right = right.child(
            rect()
                .horizontal()
                .spacing(8.)
                .child(WindowDot(Dot::Minimise, palette))
                .child(WindowDot(Dot::Maximise, palette))
                .child(WindowDot(Dot::Close, palette)),
        );
    }
    if !split {
        // The empty middle still moves the window, as the whole bar did.
        let gap = || {
            rect()
                .width(Size::px(12.))
                .height(Size::fill())
                .window_drag()
        };
        let mut row = rect()
            .width(Size::fill())
            .height(Size::px(height))
            .horizontal()
            .content(Content::flex())
            .child(left);
        if shown.tabs.is_empty() {
            row = row.child(
                rect()
                    .width(Size::flex(1.))
                    .height(Size::fill())
                    .window_drag(),
            );
        } else {
            let mut places = piece(false, false)
                .width(Size::flex(1.))
                .content(Content::flex());
            for entry in shown.tabs.iter().cloned() {
                places = places.child(Tab {
                    active: entry.page == active,
                    entry,
                    palette,
                    dense,
                    on_select: on_select.clone(),
                });
            }
            row = row.child(gap()).child(places).child(gap());
        }
        return rect().width(Size::fill()).child(row.child(right));
    }
    // Where the page is read down, the pieces are no part of its layout: they hang in the two top corners in front of the page, which
    // keeps their room free at its own top (see `Scrolling`) and scrolls under them with nothing jumping. When scrolling says the
    // bar is to go, each piece slides up out of its corner, and comes back the same way. A piece's box is only as tall as what is
    // still seen of it and cuts the rest, so a piece on its way out is not drawn over what is above the window's top (the system's
    // bar on a phone) and a piece that has gone takes no press.
    let gone = (shown.hidden / view::HEADER_HEIGHT).clamp(0., 1.);
    let corner = |piece: Rect| {
        rect()
            .height(Size::px(height * (1. - gone)))
            .overflow(Overflow::Clip)
            .child(piece.margin((-height * gone, 0., 0., 0.)))
    };
    rect()
        .width(Size::fill())
        .height(Size::px(0.))
        .layer(Layer::Overlay)
        .child(corner(left).position(Position::new_absolute().top(0.).left(0.)))
        .child(corner(right).position(Position::new_absolute().top(0.).right(0.)))
}

/// The mark and the name beside it: what the header says of the application. The mark stands for the first
/// letter of the name: the word is `eersus` and the picture is its `p`, with no gap between them.
///
/// The name is written beside the mark when the window opens, the mark nodding first, and rubbed out, letter by letter, at the first
/// thing the person does in the window (a touch, a click, a key); from then on the mark alone stands for it. A press on the mark
/// writes the name again, and the next press anywhere (the mark too) rubs it out. A window nobody is using writes it by itself now
/// and then, at uneven waits, and rubs it out a moment later (never when motion is reduced). The same on every form: a phone, a
/// phone on its side and a desktop. That is all it follows: whether the bar is shown or hidden as a page scrolls is the bar's own
/// rule, apart from this one.
#[derive(PartialEq)]
pub(crate) struct Brand {
    pub(crate) palette: Palette,
    /// Whether the person has done anything in the window yet.
    pub(crate) awake: bool,
    pub(crate) reduced_motion: bool,
}
impl Component for Brand {
    fn render(&self) -> impl IntoElement {
        let palette = self.palette;
        // How much of the name is written, 0 to 1, going toward all of it until the person does something, or while they asked for it
        // with a press on the mark, and toward none otherwise. A move starts from what is drawn, so a press while it is still being
        // written rubs it out from there. A bar drawn again after that (the screen turned) starts with none of it.
        let reduced = self.reduced_motion;
        let mut asked = use_state(|| false);
        // Whether the name had been asked for when the current press went down: every press anywhere takes the request back, and a
        // press that ends on the mark turns it over from what it was, so the mark both writes and rubs out.
        let mut asked_before = use_state(|| false);
        // Written unasked, in a window nobody is using (see below); a press anywhere takes that back too.
        let mut nudged = use_state(|| false);
        // When the person last did something in the window, and whether motion is reduced now: both are read by the task below
        // at the moment it would write the name, and it is drawn again for neither.
        let mut active_at = use_state(monotonic_ms);
        let mut still = use_state(|| reduced);
        still.set_if_modified(reduced);
        // Whether this window is the one in front, as the platform says it: a name is written for somebody to see.
        let in_front = Platform::get().current_window().is_app_focused;
        use_hook(move || {
            spawn(async move {
                let seed = unix_now();
                let mut round = 0_u32;
                loop {
                    let wait = view::idle_name_after(seed, round);
                    round = round.wrapping_add(1);
                    // One sleep for the whole wait, on the thread the interface has for deadlines: nothing wakes in between.
                    if link::after(std::time::Duration::from_millis(wait))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    // Only a window in front that was left alone for the whole wait, and never for a person who asked for no
                    // decorative motion: a window behind another, or an application in the background, draws nothing for nobody.
                    if *still.peek()
                        || !*in_front.peek()
                        || monotonic_ms().saturating_sub(*active_at.peek()) < wait
                    {
                        continue;
                    }
                    nudged.set(true);
                    let held = std::time::Duration::from_millis(view::IDLE_NAME_HELD_MS);
                    if link::after(held).await.is_err() {
                        return;
                    }
                    nudged.set_if_modified(false);
                }
            });
        });
        let wanted = !self.awake || asked() || nudged();
        let mut drawn = use_state(|| 0.0_f32);
        let from = *drawn.peek();
        let animation =
            use_animation_with_dependencies(&(wanted, reduced), move |conf, (wanted, reduced)| {
                conf.on_creation(OnCreation::Run);
                conf.on_change(OnChange::Rerun);
                let to = if *wanted { 1.0_f32 } else { 0.0_f32 };
                let time = if *reduced {
                    0
                } else {
                    (view::NAME_WRITTEN_MS as f32 * (to - from).abs()) as u64
                };
                AnimNum::new(from, to).time(time).function(Function::Linear)
            });
        let progress: f32 = animation.get().value().clamp(0., 1.);
        drawn.set_if_modified(progress);
        // The mark nods by getting a little smaller in the room it keeps, so that nothing beside it moves.
        let side = view::MARK * (1. - 0.16 * view::mark_nod(progress));
        // The picture is decoded once for the ground it stands on, and not for every frame the name is written or rubbed out in.
        let for_light = icon::for_light(palette.header);
        let mut picture = use_state(|| (for_light, icon::picture(for_light)));
        if picture.peek().0 != for_light {
            picture.set((for_light, icon::picture(for_light)));
        }
        let mark: Element = match picture.read().1.clone() {
            Some(picture) => icon::drawn(picture, side),
            None => rect()
                .width(Size::px(side))
                .height(Size::px(side))
                .corner_radius(12.)
                .background(palette.send)
                .into(),
        };
        let mut done = move || {
            active_at.set(monotonic_ms());
            nudged.set_if_modified(false);
        };
        rect()
            .horizontal()
            .cross_align(Alignment::center())
            .on_global_pointer_down(move |_| {
                asked_before.set_if_modified(*asked.peek());
                asked.set_if_modified(false);
                done();
            })
            .on_global_key_down(move |_| done())
            .on_press(move |_| asked.set(!*asked_before.peek()))
            // The picture's body ends 7 physical px below the word's baseline on the phone (2.53 px per logical px); lifting it by 2
            // (3 overshot by 3 px, measured) puts the base of its round body on that baseline.
            .child(
                rect()
                    .width(Size::px(view::MARK))
                    .height(Size::px(view::MARK))
                    .center()
                    .margin((0., 0., 4., 0.))
                    .child(mark),
            )
            // The picture has a see-through border (22% of its side, 9.7 px at this size): the word is drawn back over it so that
            // it starts where the picture's shapes end, with the air that is between two letters.
            .child(
                rect().margin((0., 0., 0., -8.)).child(
                    label()
                        .text(view::name_shown(view::name_part(progress)))
                        .color(palette.text)
                        .font_size(26.)
                        .font_weight(FontWeight::BOLD),
                ),
            )
    }
}
