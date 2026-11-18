//! The big headline of a screen and the portal under it: the circle that is pressed, held and lifted to send or receive.
use super::*;

/// The big headline of a screen: the first line plain, the second in the screen's accent and
/// slanted, as in the prototype.
///
/// It measures the room it is given and takes the largest size at which its longest line fits
/// (`view::Headline`), and each line is one row that is never broken: a title that wraps puts its
/// second row over the first. Until the room is known it is as small as a title gets, and the
/// first measurement, a frame later, puts it right.
#[derive(PartialEq)]
pub(crate) struct HeadlineView {
    pub(crate) first: &'static str,
    pub(crate) second: &'static str,
    pub(crate) palette: Palette,
    pub(crate) accent: (u8, u8, u8),
    pub(crate) form: view::Form,
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
            let mut words = label()
                .text(text)
                .color(colour)
                .font_size(size)
                .font_weight(FontWeight::BOLD)
                .max_lines(1);
            if slanted {
                words = words.font_slant(FontSlant::Italic);
            }
            rect()
                .width(Size::fill())
                .height(Size::px(size * 1.12))
                .child(words)
                .into()
        };
        rect()
            .width(Size::fill())
            .on_sized(move |e: Event<SizedEventData>| room.set_if_modified(e.area.width()))
            .child(line(first, palette.text, false))
            .child(line(second, accent, true))
    }
}

pub(crate) fn headline(
    first: &'static str,
    second: &'static str,
    palette: Palette,
    accent: (u8, u8, u8),
    form: view::Form,
) -> impl IntoElement {
    HeadlineView {
        first,
        second,
        palette,
        accent,
        form,
    }
}

/// The dark disc at the heart of a screen, with rings, as in the prototype.
///
/// `extent` is the width and height of the whole of it, rings included. The disc and the two rings are drawn in
/// the proportions the prototype has them in at its own size ([`qr::PORTAL_OUTER`]), so that a smaller portal is
/// the same picture made smaller and not a different one: three sizes written in the code would be the right
/// ones for one screen only.
pub(crate) fn portal(
    palette: Palette,
    accent: (u8, u8, u8),
    on_press: Option<EventHandler<()>>,
    children: Vec<Element>,
    extent: f32,
) -> impl IntoElement {
    Portal {
        palette,
        accent,
        on_press,
        children,
        extent,
        life: None,
        sparks: 0,
        hold: None,
        going: 0.,
    }
}

/// The portal of a code that has a life: the same, and the ring round it shows how much of that life is left (1 whole, 0 gone), so that the
/// person sees how long the code has before it ends and has to be asked for again.
pub(crate) fn portal_with_life(
    palette: Palette,
    accent: (u8, u8, u8),
    children: Vec<Element>,
    extent: f32,
    life: f32,
    sparks: u32,
) -> impl IntoElement {
    Portal {
        palette,
        accent,
        on_press: None,
        children,
        extent,
        life: Some(life),
        sparks,
        hold: None,
        going: 0.,
    }
}

/// One layer of the fuse's light: its width, its blur (none for the thin core), its colour and its opacity.
pub(crate) type FuseLight = (f32, Option<f32>, (u8, u8, u8), u8);

/// The title above the circle, centred, as a phone shows them on Send AND on Receive: the one place that decides the space between
/// them, so the two circles are at the same height on the two pages. (Two pages that each wrote their own number were 12 pixels
/// apart.) What comes after the circle is the page's own.
pub(crate) fn title_above_circle(
    form: view::Form,
    hero: impl IntoElement,
    circle: impl IntoElement,
) -> Rect {
    rect()
        .width(Size::fill())
        .spacing(form.page_section_gap())
        .cross_align(Alignment::center())
        .child(hero)
        .child(circle)
}

/// Whether a finger is held on a circle long enough to be a hold (and not a swipe), written by the circle and read by the page under it, so
/// that the page stops following the finger once the circle is held and carries on following it before that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HoldingCircle(pub(crate) bool);

/// Lowers the holding flag when it is dropped: the end of the task that raised it, or the task being dropped with the circle that ran it.
pub(crate) struct ReleaseHold(pub(crate) State<HoldingCircle>);

impl Drop for ReleaseHold {
    fn drop(&mut self) {
        self.0.set_if_modified(HoldingCircle(false));
    }
}

/// What a circle needs to be held until it fills and sends, instead of pressed: said by the page from its own state, and only ever carried
/// out by the circle. The files are never in it; the page holds them and sends them once when it is told the circle is full.
#[derive(Clone, PartialEq)]
pub(crate) struct HoldToSend {
    /// What the circle does at this moment, from one place (`view::circle_does`): choose files, make a code, be held to send, or be held back.
    pub(crate) does: view::CircleDoes,
    /// What the page does when the circle says so: choose files, make the code, or send to the devices picked. Each once for a touch.
    pub(crate) on_act: EventHandler<view::CircleAct>,
}

/// Milliseconds on a clock that only goes forward, from the first time it is asked: what a hold is timed on. Not the system time, which a person
/// or the network can change while a finger is down, and which would then fill the circle at once or never.
pub(crate) fn monotonic_ms() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    u64::try_from(
        START
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

/// The disc with its rings. What presses it is a GESTURE judged by [`view::Touch`], and not the toolkit's own press, which
/// is any touch that ends in the same box it began in: that made the four corners of the square round the circle press it,
/// and a swipe that began and ended on it press it too. The circle of the Send page is held instead, and judged by [`view::CircleTouch`].
///
/// The move and the lift are listened to in the toolkit's CAPTURE phase, which reaches this before anything under it can
/// take the event: a page that is being scrolled stops the ordinary move events from arriving, and a swipe that is not seen
/// is a swipe taken for a tap.
#[derive(PartialEq)]
pub(crate) struct Portal {
    pub(crate) palette: Palette,
    pub(crate) accent: (u8, u8, u8),
    pub(crate) on_press: Option<EventHandler<()>>,
    pub(crate) children: Vec<Element>,
    pub(crate) extent: f32,
    /// The share, from 1 to 0, of the time a code still has, for a portal that shows one: drawn as a fuse that burns round a thin outer ring.
    pub(crate) life: Option<f32>,
    /// The frame of the sparks of that fuse, which counts up as time goes by; held still when motion is reduced.
    pub(crate) sparks: u32,
    /// Held until it fills and sends, for the circle of the Send page; `None` for every other circle, which is pressed.
    pub(crate) hold: Option<HoldToSend>,
    /// How far the files of the batch have come, from 0 to 1, while they go (see `view::circle_fill`); 0 for a circle that is not sending.
    pub(crate) going: f32,
}

impl Component for Portal {
    fn render(&self) -> impl IntoElement {
        let (palette, accent, extent) = (self.palette, self.accent, self.extent);
        let share = extent / qr::PORTAL_OUTER;
        let ring = |size: f32, colour: (u8, u8, u8), width: f32| -> Element {
            let size = size * share;
            rect()
                .position(
                    Position::new_absolute()
                        .left((extent - size) / 2.)
                        .top((extent - size) / 2.),
                )
                .width(Size::px(size))
                .height(Size::px(size))
                .corner_radius(size / 2.)
                .border(
                    Border::new()
                        .fill(colour)
                        .width(width)
                        .alignment(BorderAlignment::Inner),
                )
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
            .border(
                Border::new()
                    .fill(accent)
                    .width(line)
                    .alignment(BorderAlignment::Inner),
            );
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
                    .position(
                        Position::new_absolute()
                            .left((disc_size - growing) / 2.)
                            .top((disc_size - growing) / 2.),
                    )
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
                    tracing::trace!(target: "peersus_ui::gesture", ?in_box, alive, ?does, "Circle pointer down");
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
                            tracing::trace!(target: "peersus_ui::gesture", "Circle clock listening");
                            while receiver.changed().await.is_ok() {
                                // A clock belongs to the touch it was started for. Once that is no longer the current touch (it was lifted, and another
                                // may already have begun on the same state), this clock has nothing to do: it must not fill, empty or send for it.
                                if !touch_number.peek().is_current(claimed) {
                                    tracing::trace!(target: "peersus_ui::gesture", reason = "stale-touch", "Circle clock stopped");
                                    break;
                                }
                                let mut next = *circle.peek();
                                if !next.is_down() {
                                    tracing::trace!(target: "peersus_ui::gesture", reason = "touch-ended", "Circle clock stopped");
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
                                    tracing::trace!(target: "peersus_ui::gesture", "Circle hold threshold reached");
                                }
                                if act != view::CircleAct::Nothing {
                                    tracing::trace!(target: "peersus_ui::gesture", ?act, "Circle clock action");
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
                            tracing::trace!(target: "peersus_ui::gesture", ?window, "Circle swipe ended hold");
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
                    tracing::trace!(target: "peersus_ui::gesture", ?in_box, ?does, ?act, "Circle pointer lifted");
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
                    tracing::trace!(target: "peersus_ui::gesture", "Circle touch lifted outside");
                })
                .on_touch_cancel(move |_| {
                    let mut next = *circle.peek();
                    next.cancel();
                    circle.set(next);
                    let after = touch_number.peek().next();
                    touch_number.set(after);
                    fill.set_if_modified(0.);
                    holding.set_if_modified(HoldingCircle(false));
                    tracing::trace!(target: "peersus_ui::gesture", "Circle touch cancelled");
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
                        .position(
                            Position::new_absolute()
                                .left(x - size / 2.)
                                .top(y - size / 2.),
                        )
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
                        use freya::engine::prelude::{
                            Paint, PaintCap, PaintStyle, SkBlurStyle, SkMaskFilter, SkRect,
                        };
                        let RenderTarget::Background { canvas, .. } = &context.target else {
                            return;
                        };
                        let Some((start, sweep)) = view::fuse_arc(left) else {
                            return;
                        };
                        // The toolkit has already moved the canvas to the corner of this element and scaled it by the display's factor before it calls
                        // this, so everything here is in the same logical units as `centre`, `radius` and `scale`: the line lies on the ring the tip
                        // and the sparks are on, on any screen. Scaled once more by the factor it came out as large as the screen's density.
                        let oval = SkRect::from_xywh(
                            centre - radius,
                            centre - radius,
                            2. * radius,
                            2. * radius,
                        );
                        let (red, green, blue) = accent;
                        // The light of a fuse is not one even band: it falls off gradually from a bright thin core. So the line is three layers, from the
                        // outside in: a wide faint haze, a nearer softer light, and the thin core, which is the accent lifted toward white because
                        // what burns is lighter than what is painted. Each is blurred less and is more opaque than the one outside it. The haze is the
                        // widest and stays within the space between the ring and the disc of the code (half its width and three times its blur, together,
                        // are less than that space): no light of it falls on the code, which is scanned as it was drawn.
                        let lift = |channel: u8, by: f32| {
                            (f32::from(channel) + (255. - f32::from(channel)) * by).round() as u8
                        };
                        let layers: [FuseLight; 3] = [
                            (8., Some(2.), (red, green, blue), 60),
                            (4.5, Some(1.2), (red, green, blue), 120),
                            (
                                2.2,
                                None,
                                (lift(red, 0.35), lift(green, 0.35), lift(blue, 0.35)),
                                255,
                            ),
                        ];
                        for (width, blur, (r, g, b), alpha) in layers {
                            let mut paint = Paint::default();
                            paint.set_anti_alias(true);
                            paint.set_style(PaintStyle::Stroke);
                            paint.set_stroke_cap(PaintCap::Round);
                            paint.set_stroke_width(width * scale);
                            paint.set_color(Color::from_rgb(r, g, b).with_a(alpha));
                            if let Some(sigma) = blur {
                                paint.set_mask_filter(SkMaskFilter::blur(
                                    SkBlurStyle::Normal,
                                    sigma * scale,
                                    false,
                                ));
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
                fuse = fuse
                    .child(dot(
                        tip_x,
                        tip_y,
                        16. * scale,
                        Color::from_rgb(255, 200, 80).with_a(60),
                    ))
                    .child(dot(
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
                    let colour =
                        Color::from_rgb(particle.colour.0, particle.colour.1, particle.colour.2)
                            .with_a((particle.alpha.clamp(0., 1.) * 255.) as u8);
                    fuse = fuse.child(dot(x, y, particle.size * scale, colour));
                }
                fuse
            }))
            .child(ring(384., palette.border, 1.))
            .child(disc)
    }
}
