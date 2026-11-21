//! The pointer and keyboard of this computer, taken at the edges of the layout of devices and handed
//! back where they return. Through the desktop's InputCapture portal: the desktop draws the barriers,
//! asks the person once whether this application may take the input, and gives it over a libei
//! connection while it is taken. Nothing is read while the pointer is on this computer.
//!
//! What is taken goes out as [`Captured`]; what to do comes in as orders: the barriers to set, and
//! where to let the pointer go when it comes back. The portal runs on a thread of its own, so a desktop
//! that is slow to answer never holds the application.
#![cfg(target_os = "linux")]

use crate::pass_through::Barrier;
use ashpd::desktop::{
    PersistMode, Session,
    input_capture::{
        ActivatedBarrier, Barrier as PortalBarrier, BarrierID, Capabilities, CreateSession2Options,
        CreateSessionOptions, InputCapture, ReleaseOptions, StartOptions,
    },
};
use futures_lite::StreamExt;
use peersus_core::layout::Edge;
use reis::{
    ei,
    event::{DeviceCapability, EiEvent},
};
use std::{collections::HashMap, num::NonZeroU32, os::unix::net::UnixStream, path::PathBuf};
use tokio::sync::mpsc;

/// What the capture saw.
#[derive(Clone, Debug, PartialEq)]
pub enum Captured {
    /// The pointer was stopped at a barrier of `edge`, at `cursor` on this desktop, and is taken.
    Entered {
        edge: Edge,
        cursor: (f64, f64),
    },
    Motion {
        dx: f64,
        dy: f64,
    },
    /// A button by its Linux code.
    Button {
        code: u32,
        down: bool,
    },
    /// Whole notches; positive `dy` is down.
    Scroll {
        dx: i32,
        dy: i32,
    },
    /// A key by its Linux code.
    Key {
        code: u32,
        down: bool,
    },
    /// The desktop gave the pointer back on its own (the person pressed its escape, or it went away).
    Left,
}

enum Order {
    Barriers(Vec<Barrier>),
    /// Hand the pointer back, at this point of this desktop.
    Release {
        at: (f64, f64),
    },
}

/// The way back that needs nothing but this keyboard: both Control keys held down together. The
/// pointer is given back whatever the other device, its extension or the network are doing, so a
/// visit that went wrong can always be left.
#[derive(Default)]
struct WayOut {
    left: bool,
    right: bool,
}

impl WayOut {
    /// Follow what was captured; true while both Control keys are down.
    fn asked(&mut self, captured: &Captured) -> bool {
        const LEFT_CONTROL: u32 = 29;
        const RIGHT_CONTROL: u32 = 97;
        if let Captured::Key { code, down } = *captured {
            match code {
                LEFT_CONTROL => self.left = down,
                RIGHT_CONTROL => self.right = down,
                _ => {}
            }
        }
        self.left && self.right
    }
}

/// The wheel as the desktop says it, gathered into the whole notches a device replays.
///
/// A desktop says a scroll in one of two ways: as notches a wheel turned, in 120ths (a high-resolution wheel says a
/// notch in several parts), or as a distance scrolled, which is all it says for two fingers on a touchpad and, on some
/// desktops, for a wheel too. Either may be all that comes, so both are taken; one frame that carries both is the same
/// scroll said twice, and its notches are the ones kept. What is short of a notch waits here for the rest of it.
#[derive(Default)]
struct Wheel {
    /// What the frame being read said so far: 120ths of a notch, and distance in logical pixels.
    turned: (i32, i32),
    scrolled: (f64, f64),
    /// The part of a notch not yet passed on.
    rest: (f64, f64),
}

impl Wheel {
    /// The desktop's unit for one notch of a wheel.
    const NOTCH: f64 = 120.;
    /// The distance one notch scrolls, in the logical pixels a desktop says a scroll in.
    const NOTCH_DISTANCE: f64 = 10.;
    /// The most notches passed on at once: the contract's own bound for one input.
    const MOST: f64 = 100.;

    fn turned(&mut self, dx: i32, dy: i32) {
        self.turned.0 = self.turned.0.saturating_add(dx);
        self.turned.1 = self.turned.1.saturating_add(dy);
    }

    /// A distance, unless its amounts cannot be a hand's (see [`motion`]).
    fn scrolled(&mut self, dx: f32, dy: f32) {
        const MOST: f32 = 10_000.;
        if dx.abs() <= MOST && dy.abs() <= MOST {
            self.scrolled.0 += f64::from(dx);
            self.scrolled.1 += f64::from(dy);
        }
    }

    /// The fingers left or the scroll was called off: what was short of a notch is not owed to the next one.
    fn stopped(&mut self) {
        self.rest = (0., 0.);
    }

    /// The frame ended: the whole notches it completes, if any.
    fn frame(&mut self) -> Option<Captured> {
        let turned = std::mem::take(&mut self.turned);
        let scrolled = std::mem::take(&mut self.scrolled);
        let notches = |turned: i32, scrolled: f64| {
            if turned == 0 {
                scrolled / Self::NOTCH_DISTANCE
            } else {
                f64::from(turned) / Self::NOTCH
            }
        };
        let whole = |rest: &mut f64, more: f64| {
            let all = *rest + more;
            // More than can be passed on at once is dropped, never owed: a wheel does not turn on by itself later.
            *rest = all.fract();
            all.trunc().clamp(-Self::MOST, Self::MOST) as i32
        };
        let dx = whole(&mut self.rest.0, notches(turned.0, scrolled.0));
        let dy = whole(&mut self.rest.1, notches(turned.1, scrolled.1));
        (dx != 0 || dy != 0).then_some(Captured::Scroll { dx, dy })
    }
}

/// The capture, while it runs. Dropping it ends the session.
pub struct Capture {
    orders: mpsc::UnboundedSender<Order>,
}

impl Capture {
    /// Start capturing, telling `events` what is taken. `token` keeps the person's permission between
    /// runs, so they are asked once.
    pub fn start(events: mpsc::UnboundedSender<Captured>, token: PathBuf) -> Self {
        let (orders, receive) = mpsc::unbounded_channel();
        std::thread::Builder::new()
            .name("peersus-capture".into())
            .spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                runtime.block_on(run(receive, events, token));
            })
            .ok();
        Self { orders }
    }

    /// The barriers to set; none takes nothing.
    pub fn set_barriers(&self, barriers: Vec<Barrier>) {
        let _ = self.orders.send(Order::Barriers(barriers));
    }

    /// The pointer came back: let it go at `at` on this desktop.
    pub fn release(&self, at: (f64, f64)) {
        let _ = self.orders.send(Order::Release { at });
    }
}

/// Why one session ended.
enum Ended {
    /// New barriers: a new session with them. (Mutter does not take new barriers on a session that
    /// was enabled once, so the session is made again.)
    Barriers(Vec<Barrier>),
    /// The application stopped.
    Stopped,
    /// A session that was taking the pointer failed: it is closed, and made again after a pause.
    Lost,
}

/// How long after a session was lost another is made, unless the barriers change first.
const AGAIN_AFTER: std::time::Duration = std::time::Duration::from_secs(5);

async fn run(
    mut orders: mpsc::UnboundedReceiver<Order>,
    events: mpsc::UnboundedSender<Captured>,
    token: PathBuf,
) {
    let mut barriers: Vec<Barrier> = Vec::new();
    loop {
        // Nothing to cross to: nothing is taken, and no session is open to ask the person about.
        while barriers.is_empty() {
            match orders.recv().await {
                Some(Order::Barriers(next)) => barriers = next,
                Some(Order::Release { .. }) => {}
                None => return,
            }
        }
        match session(&barriers, &mut orders, &events, &token).await {
            Ok(Ended::Barriers(next)) => barriers = next,
            Ok(Ended::Stopped) => return,
            Ok(Ended::Lost) => {
                peersus_diagnostics::dev_trace!(
                    stage = "capture-end",
                    outcome = "lost",
                    barriers = barriers.len(),
                    "Desktop input capture ended"
                );
                tracing::warn!("Taking the pointer at the edges stopped and is started again");
                let _ = events.send(Captured::Left);
                match tokio::time::timeout(AGAIN_AFTER, orders.recv()).await {
                    Ok(Some(Order::Barriers(next))) => barriers = next,
                    Ok(Some(Order::Release { .. })) | Err(_) => {}
                    Ok(None) => return,
                }
            }
            Err(_) => {
                tracing::warn!("Taking the pointer at the edges could not start");
                peersus_diagnostics::dev_trace!(
                    stage = "capture-end",
                    outcome = "failed",
                    barriers = barriers.len(),
                    "Desktop input capture ended"
                );
                let _ = events.send(Captured::Left);
                // A desktop without the portal, or a person who said no: tried again only when the
                // barriers change, never in a loop that asks them again and again.
                match orders.recv().await {
                    Some(Order::Barriers(next)) => barriers = next,
                    Some(Order::Release { .. }) => {}
                    None => return,
                }
            }
        }
    }
}

async fn open(
    portal: &InputCapture,
    token: &PathBuf,
) -> Result<Session<InputCapture>, ashpd::Error> {
    let capabilities = Capabilities::KEYBOARD | Capabilities::POINTER;
    match portal
        .create_session2(CreateSession2Options::default())
        .await
    {
        Ok(session) => {
            let saved = std::fs::read_to_string(token)
                .ok()
                .map(|t| t.trim().to_owned())
                .filter(|t| !t.is_empty());
            let options = StartOptions::default()
                .set_capabilities(capabilities)
                .set_restore_token(saved)
                .set_persist_mode(PersistMode::ExplicitlyRevoked);
            let response = portal.start(&session, None, options).await?.response()?;
            if let Some(next) = response.restore_token() {
                let _ = std::fs::write(token, next);
            }
            Ok(session)
        }
        Err(ashpd::Error::RequiresVersion(_, _)) => {
            let options = CreateSessionOptions::default().set_capabilities(capabilities);
            Ok(portal.create_session(None, options).await?.0)
        }
        Err(error) => Err(error),
    }
}

/// One session from its opening to its end. Whatever ends it, the desktop is told to close it: a session left open keeps its
/// barriers, and a pointer that reaches one of them is held there with nobody to let it go.
async fn session(
    barriers: &[Barrier],
    orders: &mut mpsc::UnboundedReceiver<Order>,
    events: &mpsc::UnboundedSender<Captured>,
    token: &PathBuf,
) -> Result<Ended, Box<dyn std::error::Error>> {
    peersus_diagnostics::dev_trace!(
        stage = "capture-open",
        barriers = barriers.len(),
        "Desktop input capture opening"
    );
    let portal = InputCapture::new().await?;
    let session = open(&portal, token).await?;
    peersus_diagnostics::dev_trace!(
        stage = "capture-session",
        "Desktop input capture session opened"
    );
    let mut enabled = false;
    let ended = serve(&portal, &session, barriers, orders, events, &mut enabled).await;
    let _ = session.close().await;
    match ended {
        Err(_) if enabled => Ok(Ended::Lost),
        ended => ended,
    }
}

/// Set the barriers of an open session, enable it and follow it until it ends. `enabled` is set once the desktop may take the pointer.
async fn serve(
    portal: &InputCapture,
    session: &Session<InputCapture>,
    barriers: &[Barrier],
    orders: &mut mpsc::UnboundedReceiver<Order>,
    events: &mpsc::UnboundedSender<Captured>,
    enabled: &mut bool,
) -> Result<Ended, Box<dyn std::error::Error>> {
    let zones = portal
        .zones(session, Default::default())
        .await?
        .response()?;
    let mut edges: HashMap<BarrierID, Edge> = HashMap::new();
    let mut set = Vec::new();
    for (index, barrier) in barriers.iter().enumerate() {
        let Some(id) = u32::try_from(index + 1).ok().and_then(NonZeroU32::new) else {
            continue;
        };
        edges.insert(id, barrier.edge);
        set.push(PortalBarrier::new(id, barrier.line));
    }
    portal
        .set_pointer_barriers(session, &set, zones.zone_set(), Default::default())
        .await?
        .response()?;
    let fd = portal.connect_to_eis(session, Default::default()).await?;
    let context = ei::Context::new(UnixStream::from(fd))?;
    let (_connection, mut input) = context
        .handshake_tokio("io.peersus.Peersus", ei::handshake::ContextType::Receiver)
        .await?;
    // Listened to before enabling: the desktop may hand the pointer over at once.
    let mut activated = portal.receive_activated().await?;
    let mut deactivated = portal.receive_deactivated().await?;
    portal.enable(session, Default::default()).await?;
    *enabled = true;
    peersus_diagnostics::dev_trace!(
        stage = "capture-enabled",
        barriers = set.len(),
        "Desktop input capture enabled"
    );
    let mut active: Option<Option<u32>> = None;
    let mut way_out = WayOut::default();
    let mut wheel = Wheel::default();
    // New barriers while the pointer is taken: a new session now would drop the pointer on the floor in the middle of a visit (the
    // desktop forgets the capture, the visit goes on), which is the pointer that seems to be on both computers at once. They wait
    // until the pointer is given back.
    let mut pending: Option<Vec<Barrier>> = None;
    loop {
        tokio::select! {
            signal = activated.next() => {
                let Some(signal) = signal else { return Err("the desktop stopped saying when it takes the pointer".into()) };
                let edge = match signal.barrier_id() {
                    Some(ActivatedBarrier::Barrier(id)) => edges.get(&id).copied(),
                    _ => None,
                };
                let (Some(edge), Some(cursor)) = (edge, signal.cursor_position()) else { continue };
                active = Some(signal.activation_id());
                way_out = WayOut::default();
                wheel = Wheel::default();
                peersus_diagnostics::dev_trace!(stage = "capture-activated", activation = signal.activation_id().unwrap_or(0), x = cursor.0, y = cursor.1, "Desktop took the pointer");
                let _ = events.send(Captured::Entered { edge, cursor: (f64::from(cursor.0), f64::from(cursor.1)) });
            }
            signal = deactivated.next() => {
                let Some(signal) = signal else { return Err("the desktop stopped saying when it gives the pointer back".into()) };
                // Only the taking that is going on ends here. The word that an earlier one ended can be read after the next has
                // begun: taken for this one, the desktop would go on holding the pointer with nothing here to let it go.
                let current = active == Some(signal.activation_id());
                peersus_diagnostics::dev_trace!(stage = "capture-deactivated", activation = signal.activation_id().unwrap_or(0), current, "Desktop gave the pointer back");
                if !current {
                    continue;
                }
                active = None;
                let _ = events.send(Captured::Left);
                if let Some(next) = pending.take() {
                    return Ok(Ended::Barriers(next));
                }
            }
            event = input.next() => {
                let Some(event) = event else { return Err("the input connection closed".into()) };
                let event = event?;
                if let EiEvent::SeatAdded(added) = &event {
                    added.seat.bind_capabilities(
                        DeviceCapability::Pointer | DeviceCapability::Keyboard | DeviceCapability::Scroll | DeviceCapability::Button,
                    );
                    context.flush()?;
                    continue;
                }
                if active.is_none() {
                    continue;
                }
                // A scroll is whole only at the end of its frame: see `Wheel`.
                let captured = match &event {
                    EiEvent::ScrollDiscrete(scroll) => {
                        wheel.turned(scroll.discrete_dx, scroll.discrete_dy);
                        continue;
                    }
                    EiEvent::ScrollDelta(scroll) => {
                        wheel.scrolled(scroll.dx, scroll.dy);
                        continue;
                    }
                    EiEvent::ScrollStop(_) | EiEvent::ScrollCancel(_) => {
                        wheel.stopped();
                        continue;
                    }
                    EiEvent::Frame(_) => wheel.frame(),
                    other => translate(other),
                };
                let Some(captured) = captured else { continue };
                if way_out.asked(&captured) {
                    // No place is named: the desktop lets the pointer go where it holds it. The second key is not passed on,
                    // and the visit ends as when the desktop takes the pointer back itself, which lets go of the first.
                    if let Some(activation) = active.take() {
                        portal.release(session, ReleaseOptions::default().set_activation_id(activation)).await?;
                        let _ = events.send(Captured::Left);
                    }
                    if let Some(next) = pending.take() {
                        return Ok(Ended::Barriers(next));
                    }
                    continue;
                }
                let _ = events.send(captured);
            }
            order = orders.recv() => match order {
                None => return Ok(Ended::Stopped),
                Some(Order::Barriers(next)) if next.as_slice() != barriers => {
                    if active.is_some() {
                        pending = Some(next);
                    } else {
                        return Ok(Ended::Barriers(next));
                    }
                }
                Some(Order::Barriers(_)) => pending = None,
                Some(Order::Release { at }) => {
                    if let Some(activation) = active.take() {
                        peersus_diagnostics::dev_trace!(stage = "capture-release", activation = activation.unwrap_or(0), x = at.0, y = at.1, "Pointer given back to the desktop");
                        let options = ReleaseOptions::default()
                            .set_activation_id(activation)
                            .set_cursor_position(Some(at));
                        portal.release(session, options).await?;
                    }
                    if let Some(next) = pending.take() {
                        return Ok(Ended::Barriers(next));
                    }
                }
            },
        }
    }
}

/// A motion as the desktop sent it, or nothing when its amounts cannot be a hand's. Mutter passes on a motion it made itself
/// (the pointer it places when it gives it back) with amounts it never set, so they are anything: not a number, or a number no
/// mouse moves in one step. One such motion would put the visited pointer nowhere, for good.
fn motion(dx: f32, dy: f32) -> Option<Captured> {
    const MOST: f32 = 10_000.;
    (dx.abs() <= MOST && dy.abs() <= MOST).then(|| Captured::Motion {
        dx: f64::from(dx),
        dy: f64::from(dy),
    })
}

fn translate(event: &EiEvent) -> Option<Captured> {
    Some(match event {
        EiEvent::PointerMotion(moved) => return motion(moved.dx, moved.dy),
        EiEvent::Button(button) => Captured::Button {
            code: button.button,
            down: button.state == ei::button::ButtonState::Press,
        },
        EiEvent::KeyboardKey(key) => Captured::Key {
            code: key.key,
            down: key.state == ei::keyboard::KeyState::Press,
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_control_keys_together_ask_for_the_pointer_back_and_nothing_less_does() {
        let key = |code, down| Captured::Key { code, down };
        let mut way_out = WayOut::default();
        assert!(!way_out.asked(&key(29, true)), "one Control is typing");
        assert!(
            !way_out.asked(&key(46, true)),
            "Control and a letter is a shortcut"
        );
        assert!(!way_out.asked(&key(29, false)));
        assert!(
            !way_out.asked(&key(97, true)),
            "the other one alone, after the first was let go"
        );
        assert!(!way_out.asked(&Captured::Motion { dx: 1., dy: 0. }));
        assert!(way_out.asked(&key(29, true)), "both down, in either order");
        assert!(!way_out.asked(&key(97, false)), "over once one is let go");
        // A button with the code of a key is not that key.
        let mut other = WayOut::default();
        other.asked(&key(29, true));
        assert!(!other.asked(&Captured::Button {
            code: 97,
            down: true
        }));
    }

    #[test]
    fn a_motion_whose_amounts_cannot_be_a_hands_is_not_passed_on() {
        assert_eq!(
            motion(-3.5, 2.),
            Some(Captured::Motion { dx: -3.5, dy: 2. })
        );
        for (dx, dy) in [
            (f32::NAN, 0.),
            (0., f32::NAN),
            (f32::INFINITY, 1.),
            (1., f32::NEG_INFINITY),
            (4.0e18, 0.),
            (0., -1.0e30),
        ] {
            assert_eq!(motion(dx, dy), None, "{dx} {dy}");
        }
    }

    #[test]
    fn a_wheel_is_whole_notches_however_the_desktop_says_it() {
        let scroll = |dx, dy| Some(Captured::Scroll { dx, dy });
        // A plain wheel: one notch, 120.
        let mut wheel = Wheel::default();
        wheel.turned(0, 120);
        assert_eq!(wheel.frame(), scroll(0, 1));
        wheel.turned(-240, 0);
        assert_eq!(wheel.frame(), scroll(-2, 0));
        // A high-resolution wheel says a notch in parts: nothing until it is whole, then one.
        let mut parts = Vec::new();
        for _ in 0..4 {
            wheel.turned(0, -30);
            parts.push(wheel.frame());
        }
        assert_eq!(parts, [None, None, None, scroll(0, -1)]);
        // A distance (two fingers, or a wheel said as distance): a notch every `NOTCH_DISTANCE`.
        let mut wheel = Wheel::default();
        wheel.scrolled(0., 4.);
        assert_eq!(wheel.frame(), None);
        wheel.scrolled(0., 6.5);
        assert_eq!(wheel.frame(), scroll(0, 1));
        wheel.scrolled(25., 0.);
        assert_eq!(wheel.frame(), scroll(2, 0));
        // Both in one frame are one scroll said twice: its notches count, its distance does not.
        let mut wheel = Wheel::default();
        wheel.turned(0, 120);
        wheel.scrolled(0., 10.);
        assert_eq!(wheel.frame(), scroll(0, 1));
        // A frame with no scroll in it says nothing.
        assert_eq!(wheel.frame(), None);
    }

    #[test]
    fn what_is_short_of_a_notch_is_dropped_when_the_scroll_stops_and_never_grows_without_bound() {
        let mut wheel = Wheel::default();
        wheel.scrolled(0., 9.);
        assert_eq!(wheel.frame(), None);
        wheel.stopped();
        wheel.scrolled(0., 9.);
        assert_eq!(
            wheel.frame(),
            None,
            "the first nine are not owed to the second scroll"
        );
        // Amounts no hand makes are not a scroll, and one huge frame is bounded by the contract.
        let mut wheel = Wheel::default();
        wheel.scrolled(f32::NAN, f32::INFINITY);
        wheel.scrolled(4.0e18, 0.);
        assert_eq!(wheel.frame(), None);
        wheel.turned(0, i32::MAX);
        assert_eq!(wheel.frame(), Some(Captured::Scroll { dx: 0, dy: 100 }));
        assert_eq!(wheel.frame(), None, "nothing of it is owed later");
    }
}
