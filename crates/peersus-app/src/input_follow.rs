//! The pointer taken at this desktop's edges and what is done beyond them carried to the device there:
//! the capture ([`crate::desktop_capture`]) driven by where the layout says the pointer goes
//! ([`crate::pass_through`]), for as long as the owner runs. The one place that knows both; neither
//! of them knows this.
use crate::desktop_capture::{Captured, Sample};
use crate::pass_through::{Barrier, Push, Pushed, Route, Step, Visit, let_go, route};
use crate::{runtime::CaptureInput, surfaces::Ending};
use peersus_api::contributions::PointerInput;
use peersus_core::{layout::Edge, model::PeerId, plugins::PluginId};
use std::{
    collections::HashMap,
    num::NonZeroU16,
    sync::{Arc, atomic::AtomicU64},
    time::Duration,
};
use tokio::{sync::mpsc, time::Instant};

/// The barriers to set now. An edge the pointer cannot be carried through at this moment has none, so the desktop never takes
/// the pointer there. That is every edge while `closed` (a device drives this desktop: its pointer here is that device's, and
/// taking it would carry it back to where it came from; or the pointer was just given back: see [`COOL`]), and the edges
/// toward a device that just could not be reached.
fn open_edges(
    route: &Route,
    broken: &HashMap<String, tokio::time::Instant>,
    closed: bool,
) -> Vec<Barrier> {
    if closed {
        return Vec::new();
    }
    route
        .plane
        .barriers()
        .into_iter()
        .filter(|barrier| !broken.contains_key(&barrier.device))
        .collect()
}

/// How long the edges stay closed after the pointer is given back. Mutter (seen on 51.1, and reported for 50 by another
/// program that shares a mouse) can leave a pointer it gives back on the barrier it was stopped at, whatever place it was asked
/// to put it, and a pointer on a barrier is taken again by its next motion in any direction: given back, taken, given back, a
/// hundred times a second, held on the edge and only sliding along it. With no barrier there for this long, the hand that is
/// coming back carries the pointer off the edge, and a pointer left on it is free the moment it moves.
const COOL: std::time::Duration = std::time::Duration::from_millis(350);

/// Give the pointer back (at `at`, when the desktop holds it) and close the edges for [`COOL`].
fn give_back(
    capture: &crate::desktop_capture::Capture,
    at: Option<(f64, f64)>,
    cooling: &mut Option<tokio::time::Instant>,
) {
    if let Some(at) = at {
        capture.release(at);
    }
    capture.set_barriers(Vec::new());
    *cooling = Some(tokio::time::Instant::now() + COOL);
}

/// What a captured input is on the other device, or `None` for what is not passed on. That includes whatever the
/// contract's own bounds refuse (a key code in the range of tools and buttons, say): one such input would have the
/// whole batch refused, and with it the pointer's way on that device.
fn passed(
    captured: &crate::desktop_capture::Captured,
) -> Option<peersus_api::contributions::PointerInput> {
    use crate::desktop_capture::Captured;
    use peersus_api::contributions::{Button, PointerInput};
    let input = match *captured {
        Captured::Button { code, down } => PointerInput::Button {
            button: match code {
                0x110 => Button::Left,
                0x111 => Button::Right,
                0x112 => Button::Middle,
                _ => return None,
            },
            down,
        },
        Captured::Scroll { dx, dy } => PointerInput::Scroll {
            dx: dx.clamp(-100, 100),
            dy: dy.clamp(-100, 100),
        },
        Captured::Key { code, down } => PointerInput::KeyCode {
            code: u16::try_from(code).ok()?,
            down,
        },
        _ => return None,
    };
    input.validate().is_ok().then_some(input)
}

/// While a departure is being measured the desktop holds the keyboard too. Do not leave it
/// captured at an edge at rest. A return at rest gives up only the push, not the active visit.
const PUSH_IDLE: std::time::Duration = std::time::Duration::from_millis(300);

type Departure = Option<(Visit, Push)>;

/// A new activation always replaces the previous attempt. Off starts the visit right away.
fn begin_departure(
    pending: &mut Departure,
    visit: Visit,
    edge: Edge,
    pressure: Option<NonZeroU16>,
) -> Option<Visit> {
    if let Some(amount) = pressure {
        *pending = Some((visit, Push::at(edge, amount)));
        None
    } else {
        *pending = None;
        Some(visit)
    }
}

/// Return an accepted visit or an abandoned one. In either case the attempt is consumed once.
fn push_departure(pending: &mut Departure, dx: f64, dy: f64) -> Option<Result<Visit, Visit>> {
    let accepted = match pending.as_mut()?.1.moved(dx, dy) {
        Pushed::More => return None,
        Pushed::Through { .. } => true,
        Pushed::Back => false,
    };
    pending
        .take()
        .map(|(visit, _)| if accepted { Ok(visit) } else { Err(visit) })
}

#[derive(Debug, PartialEq)]
enum MotionStep {
    Unchanged,
    Enter(Visit),
    GiveBack(Visit),
    Visiting(Option<Step>),
}

/// Reset an idle return without ending the visit; an idle departure must let the pointer go.
fn expire_pressure(
    at: Instant,
    until: &mut Option<Instant>,
    pushing: &mut Departure,
    visit: Option<&mut Visit>,
) -> Option<Visit> {
    if until.is_none_or(|deadline| at < deadline) {
        return None;
    }
    *until = None;
    if let Some((departure, _)) = pushing.take() {
        return Some(departure);
    }
    if let Some(there) = visit {
        there.clear_return();
    }
    None
}

fn follow_motion(
    route: &Route,
    pushing: &mut Departure,
    mut visit: Option<&mut Visit>,
    until: &mut Option<Instant>,
    at: Instant,
    dx: f64,
    dy: f64,
) -> MotionStep {
    if let Some(gone) = expire_pressure(at, until, pushing, visit.as_deref_mut()) {
        return MotionStep::GiveBack(gone);
    }
    // Some backends report zero-delta frames. They indicate neither effort nor activity.
    if dx == 0. && dy == 0. {
        return MotionStep::Unchanged;
    }
    if pushing.is_some() {
        let result = push_departure(pushing, dx, dy);
        *until = pushing.is_some().then(|| at + PUSH_IDLE);
        return match result {
            Some(Ok(there)) => MotionStep::Enter(there),
            Some(Err(there)) => MotionStep::GiveBack(there),
            None => MotionStep::Unchanged,
        };
    }
    if let Some(there) = visit {
        let step = there.moved(&route.plane, dx, dy, route.pressure);
        *until = there.pushing_back().then(|| at + PUSH_IDLE);
        MotionStep::Visiting(step)
    } else {
        MotionStep::Unchanged
    }
}

/// Return to the select loop after a fixed number of samples even if a mouse never stops reporting.
const CAPTURE_TURN: usize = 32;

#[derive(Debug, PartialEq)]
enum CaptureTurn {
    Read(usize),
    Idle(Instant),
}

async fn capture_turn(
    taken: &mut tokio::sync::mpsc::UnboundedReceiver<Sample>,
    batch: &mut Vec<Sample>,
    deadline: Option<Instant>,
) -> CaptureTurn {
    let Some(at) = deadline else {
        return CaptureTurn::Read(taken.recv_many(batch, CAPTURE_TURN).await);
    };
    // Read already queued samples in capture order, even if the wall-clock timer is also ready.
    tokio::select! {
        biased;
        read = taken.recv_many(batch, CAPTURE_TURN) => CaptureTurn::Read(read),
        () = tokio::time::sleep_until(at) => CaptureTurn::Idle(at),
    }
}

/// What the capture decided to send to a device: extension, device, inputs, whether to
/// end the copy or the visit, and the serial of the visit.
pub(crate) type Outgoing = (PluginId, PeerId, Vec<PointerInput>, Option<Ending>, u64);

/// How long the places a pointer passes through are gathered into one call.
///
/// A fast mouse can report a thousand times a second, more than the replaying extension's
/// whole broker budget. A visit's continuous motion costs about 125 calls/s when one frame
/// needs one replay. Positions are absolute, so keep the last within a frame.
pub(crate) const FRAME: Duration = Duration::from_millis(8);

/// Nothing but where the pointer is: the one kind of input that may be replaced by the one after it.
fn only_position(batch: &Outgoing) -> bool {
    batch.3.is_none() && matches!(batch.2.as_slice(), [PointerInput::Position { .. }])
}

/// Whether `later` takes the place of `earlier`: another place of the same device in the same visit.
fn replaces(earlier: &Outgoing, later: &Outgoing) -> bool {
    only_position(later) && later.0 == earlier.0 && later.1 == earlier.1 && later.4 == earlier.4
}

/// Hand what the capture decided to `forward`, in the order it was decided, one at a time.
///
/// A place that waits behind a slow send or inside a frame is replaced by the newer one. Anything else (a button, a key, a
/// wheel, the end of a visit, the first place of a visit) goes at once and in its place in the order, after the place that
/// came before it. The first place after a quiet frame goes at once too, so a pointer that has just arrived is not held.
/// Input of a visit that was stopped is dropped. Returns when the capture is gone.
pub(crate) async fn forward_in_order<F, Fut>(
    mut queued: mpsc::UnboundedReceiver<Outgoing>,
    stopped_through: Arc<AtomicU64>,
    mut forward: F,
) where
    F: FnMut(Outgoing) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    use mpsc::error::TryRecvError;
    let mut ahead: Option<Outgoing> = None;
    // When the last place was sent, and for which visit.
    let mut last_place: Option<(Instant, u64)> = None;
    loop {
        let Some(mut next) = (match ahead.take() {
            Some(next) => Some(next),
            None => queued.recv().await,
        }) else {
            return;
        };
        if only_position(&next) {
            let frame_ends = last_place
                .filter(|(_, visit)| *visit == next.4)
                .map(|(at, _)| at + FRAME);
            // Keep the newest place from the backlog already here, plus reports that
            // arrive during this frame. A slow send can leave more than one capture
            // turn queued; drain it cooperatively instead of sending stale intermediate places.
            let mut backlog = queued.len();
            let mut scanned = 0;
            loop {
                let until = frame_ends.filter(|ends| Instant::now() < *ends);
                if backlog == 0 && until.is_none() {
                    break;
                }
                if scanned == CAPTURE_TURN {
                    tokio::task::yield_now().await;
                    scanned = 0;
                }
                let later = match queued.try_recv() {
                    Ok(later) => later,
                    Err(TryRecvError::Disconnected) => break,
                    Err(TryRecvError::Empty) => {
                        let Some(ends) = until else { break };
                        tokio::select! {
                            biased;
                            () = tokio::time::sleep_until(ends) => break,
                            later = queued.recv() => {
                                let Some(later) = later else { break };
                                later
                            }
                        }
                    }
                };
                backlog = backlog.saturating_sub(1);
                scanned += 1;
                if replaces(&next, &later) {
                    next = later;
                } else {
                    ahead = Some(later);
                    break;
                }
            }
            last_place = Some((Instant::now(), next.4));
        }
        if next.4 <= stopped_through.load(std::sync::atomic::Ordering::Relaxed) {
            continue;
        }
        forward(next).await;
    }
}

/// Take the pointer at the layout's edges and carry what is done on the other side to the extension
/// that sends it there, for as long as the owner runs.
pub async fn follow(client: crate::runtime::Client, data: std::path::PathBuf) {
    use crate::desktop_capture::Capture;
    use peersus_core::model::Command;
    let (events, mut taken) = tokio::sync::mpsc::unbounded_channel();
    let capture = Capture::start(events, data.join("input-capture.token"));
    let mut snapshots = client.subscribe();
    let mut current = route(&snapshots.borrow_and_update());
    peersus_diagnostics::dev_trace!(
        stage = "capture-route",
        carrier = current.carrier.is_some(),
        barriers = current.plane.barriers().len(),
        "Desktop capture route prepared"
    );
    let mut visit: Option<Visit> = None;
    // While a connection to the visited device is open, its extension serves nothing else (its own
    // Remote page, another device's control). It is closed once the pointer rests there; the next
    // motion opens another. The pointer stays where it is on that device.
    const REST: std::time::Duration = std::time::Duration::from_millis(400);
    // While something is held the connection stays open, and a press held with nothing else to say (a
    // key kept down, a drag that pauses) would look like a silent connection to both applications, which
    // end one after `IDLE_LIMIT` and let go of what it held. The pointer's place is said again before
    // that, twice over, so a hold lasts for as long as this computer is there to hold it.
    const HOLD: std::time::Duration = std::time::Duration::from_secs(10);
    const { assert!(HOLD.as_secs() * 2 < peersus_plugin::pump::IDLE_LIMIT.as_secs()) };
    let mut open_until: Option<tokio::time::Instant> = None;
    let mut held: u32 = 0;
    let mut pushing: Departure = None;
    let mut push_until: Option<tokio::time::Instant> = None;
    let mut batch = Vec::with_capacity(CAPTURE_TURN);
    // How long after the last input a device replayed here this desktop still counts as driven by it.
    const DRIVEN_GUARD: std::time::Duration = std::time::Duration::from_secs(2);
    let mut driven = crate::desktop_input::replayed_within(DRIVEN_GUARD);
    let mut driven_check = tokio::time::Instant::now() + DRIVEN_GUARD;
    // Sending waits for the owner and, through it, for the device: on a slow or failing link that is seconds. The capture loop never
    // waits for it, so the pointer comes back the moment it is pushed back, whatever the link does. One task sends, in order, and no
    // faster than a frame at a time (see [`forward_in_order`]).
    let (outgoing, queued) = mpsc::unbounded_channel::<Outgoing>();
    // A device whose forward just failed: the pointer is given back at once and the edges toward it are closed for a short
    // while — a link that is down must not hold the person's pointer, and must not be entered again at every push while it is.
    const BROKEN_GUARD: std::time::Duration = std::time::Duration::from_secs(10);
    let mut broken: HashMap<String, tokio::time::Instant> = HashMap::new();
    // Until when the edges are closed because the pointer was just given back.
    let mut cooling: Option<tokio::time::Instant> = None;
    capture.set_barriers(open_edges(&current, &broken, driven));
    let (dead, mut dead_rx) = tokio::sync::mpsc::unbounded_channel::<(PeerId, bool, Option<u64>)>();
    let serial = std::sync::atomic::AtomicU64::new(0);
    let stopped_through = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let sender_stops = stopped_through.clone();
    // A batch is accepted long before it is known whether it arrived: the extension answers later, or
    // not at all within its time. That later failure is heard here, so the pointer is given back then
    // too, and not only when a command is refused outright.
    let mut failures = client.surface_failures();
    let failed_later = dead.clone();
    let sender_client = client.clone();
    tokio::spawn(forward_in_order(queued, sender_stops, move |batch| {
        let client = sender_client.clone();
        let dead = dead.clone();
        async move {
            let (plugin, peer, events, ending, visit) = batch;
            let end = ending.is_some();
            // What a refused batch was, for the log: how many inputs and whether it was the pointer's place. Never which key or button.
            let count = events.len();
            let place = matches!(events.as_slice(), [PointerInput::Position { .. }]);
            let events = events
                .iter()
                .filter_map(|event| serde_json::to_value(event).ok())
                .collect();
            let result = client
                .forward_capture(
                    Command::ForwardInput {
                        plugin,
                        peer: peer.clone(),
                        events,
                        end,
                    },
                    CaptureInput {
                        serial: visit,
                        resting: ending == Some(Ending::Copy),
                    },
                )
                .await;
            let blocked = matches!(&result, Ok(crate::runtime::Reply::MissingPermission { .. }));
            peersus_diagnostics::dev_trace!(
                stage = "capture-forward",
                end,
                outcome = if blocked {
                    "blocked"
                } else if result.is_ok() {
                    "ok"
                } else {
                    "failed"
                },
                "Captured input dispatched"
            );
            // The device could not be reached: the follower is told, so the pointer it holds
            // is given back instead of disappearing into a link that is down.
            if blocked {
                let _ = dead.send((peer, true, Some(visit)));
            } else if let Err(reason) = result {
                tracing::debug!(reason, count, place, end, "Captured input was refused");
                let _ = dead.send((peer, false, Some(visit)));
            }
        }
    }));
    let send = |route: &Route, device: &str, events: Vec<PointerInput>, end: Option<Ending>| {
        if let Some(plugin) = &route.carrier {
            // Queued in capture order, including the end before another device is entered.
            let _ = outgoing.send((
                plugin.clone(),
                PeerId(device.to_owned()),
                events,
                end,
                serial.load(std::sync::atomic::Ordering::Relaxed),
            ));
        }
    };
    let begin = |route: &Route, device: &str, x: f32, y: f32| {
        serial.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        send(route, device, vec![PointerInput::Position { x, y }], None);
    };
    // Position samples have already been evaluated against the edges. Only their visual replay
    // can be coalesced, and never past a button, key, new visit or end.
    let flush_position = |route: &Route, visit: Option<&Visit>, latest: &mut Option<(f32, f32)>| {
        if let (Some(there), Some((x, y))) = (visit, latest.take()) {
            send(
                route,
                &there.device,
                vec![PointerInput::Position { x, y }],
                None,
            );
            true
        } else {
            false
        }
    };
    let resting = |open_until: &mut Option<tokio::time::Instant>, held: u32| {
        *open_until = Some(tokio::time::Instant::now() + if held == 0 { REST } else { HOLD });
    };
    loop {
        let rest_at = open_until;
        let pressure_at = push_until;
        tokio::select! {
            () = async { tokio::time::sleep_until(rest_at.unwrap_or_else(tokio::time::Instant::now)).await }, if rest_at.is_some() => {
                open_until = None;
                if let Some(there) = visit.as_ref() {
                    if held == 0 {
                        send(&current, &there.device, Vec::new(), Some(Ending::Copy));
                    } else if let Some((x, y)) = there.share(&current.plane) {
                        send(&current, &there.device, vec![PointerInput::Position { x, y }], None);
                        open_until = Some(tokio::time::Instant::now() + HOLD);
                    }
                }
            }
            () = async { tokio::time::sleep_until(broken.values().min().map_or_else(tokio::time::Instant::now, |first| *first + BROKEN_GUARD)).await }, if !broken.is_empty() => {
                broken.retain(|_, when| when.elapsed() < BROKEN_GUARD);
                capture.set_barriers(open_edges(&current, &broken, driven || cooling.is_some()));
            }
            () = async { tokio::time::sleep_until(cooling.unwrap_or_else(tokio::time::Instant::now)).await }, if cooling.is_some() => {
                cooling = None;
                capture.set_barriers(open_edges(&current, &broken, driven));
            }
            () = crate::desktop_input::replayed(), if !driven => {
                if crate::desktop_input::replayed_within(DRIVEN_GUARD) {
                    tracing::debug!("A device drives this desktop: its edges are closed");
                    driven = true;
                    driven_check = tokio::time::Instant::now() + DRIVEN_GUARD;
                    if let Some((there, _)) = pushing.take() {
                        push_until = None;
                        give_back(&capture, Some(there.from), &mut cooling);
                    } else {
                        capture.set_barriers(Vec::new());
                    }
                }
            }
            () = tokio::time::sleep_until(driven_check), if driven => {
                if crate::desktop_input::replayed_within(DRIVEN_GUARD) {
                    driven_check = tokio::time::Instant::now() + DRIVEN_GUARD;
                } else {
                    tracing::debug!("No device drives this desktop any more: its edges are open");
                    driven = false;
                    capture.set_barriers(open_edges(&current, &broken, cooling.is_some()));
                }
            }
            Some((device, blocked, ended)) = dead_rx.recv() => {
                let serial = serial.load(std::sync::atomic::Ordering::Relaxed);
                if ended.is_some_and(|ended| ended != serial) {
                    continue;
                }
                if !blocked {
                    broken.insert(device.0.clone(), tokio::time::Instant::now());
                }
                if let Some(there) = visit.take_if(|there| there.device == device.0) {
                    stopped_through.store(serial, std::sync::atomic::Ordering::Relaxed);
                    if blocked {
                        tracing::debug!("The pointer was given back: the visit is no longer accepted");
                    } else {
                        tracing::warn!("The pointer was given back: the device it was on could not be reached");
                    }
                    give_back(&capture, Some(there.from), &mut cooling);
                    push_until = None;
                    open_until = None;
                    held = 0;
                } else if let Some((there, _)) = pushing.take_if(|(there, _)| there.device == device.0) {
                    push_until = None;
                    give_back(&capture, Some(there.from), &mut cooling);
                } else {
                    capture.set_barriers(open_edges(&current, &broken, driven || cooling.is_some()));
                }
            }
            failed = failures.recv() => {
                use tokio::sync::broadcast::error::RecvError;
                let stop = match failed {
                    Ok(notice) if current.carrier.as_ref() == Some(&notice.key.plugin) =>
                        notice.key.device.map(|device| (device, notice.blocked, notice.visit)),
                    Ok(_) => None,
                    // Failures were said faster than they were heard: whichever device is visited is one of them.
                    Err(RecvError::Lagged(_)) => visit.as_ref().map(|there| (PeerId(there.device.clone()), true,
                        Some(serial.load(std::sync::atomic::Ordering::Relaxed)))),
                    Err(RecvError::Closed) => return,
                };
                if let Some(stop) = stop {
                    let _ = failed_later.send(stop);
                }
            }
            changed = snapshots.changed() => {
                if changed.is_err() {
                    return;
                }
                let next = route(&snapshots.borrow_and_update());
                if next != current {
                    peersus_diagnostics::dev_trace!(stage = "capture-route", carrier = next.carrier.is_some(), barriers = next.plane.barriers().len(), "Desktop capture route changed");
                    // The visited device went offline, lost its permission, or its place in the
                    // layout changed: its pointer is withdrawn and this one given back where it
                    // left, never kept captured with no way back.
                    push_until = None;
                    let carrier_changed = current.carrier != next.carrier;
                    if let Some(gone) = visit.take_if(|there| carrier_changed || !next.plane.keeps(there)) {
                        send(&current, &gone.device, Vec::new(), Some(Ending::Visit));
                        give_back(&capture, Some(gone.from), &mut cooling);
                        open_until = None;
                        held = 0;
                    } else if let Some((gone, _)) = pushing.take() {
                        give_back(&capture, Some(gone.from), &mut cooling);
                    } else {
                        if let Some(there) = &mut visit {
                            there.clear_return();
                        }
                        capture.set_barriers(open_edges(&next, &broken, driven || cooling.is_some()));
                    }
                    current = next;
                }
            }
            turn = capture_turn(&mut taken, &mut batch, pressure_at) => {
                match turn {
                    CaptureTurn::Read(0) => return,
                    CaptureTurn::Read(_) => {}
                    CaptureTurn::Idle(at) => {
                        if let Some(there) = expire_pressure(at, &mut push_until, &mut pushing, visit.as_mut()) {
                            tracing::debug!("A push against an edge stood still and the pointer was given back");
                            give_back(&capture, Some(there.from), &mut cooling);
                        }
                        continue;
                    }
                }
                let mut latest = None;
                for Sample { at, event: captured } in batch.drain(..) {
                    if !matches!(captured, Captured::Motion { .. } | Captured::Left)
                        && let Some(there) = expire_pressure(at, &mut push_until, &mut pushing, visit.as_mut())
                    {
                        give_back(&capture, Some(there.from), &mut cooling);
                    }
                    if !matches!(captured, Captured::Motion { .. })
                        && flush_position(&current, visit.as_ref(), &mut latest)
                    {
                        resting(&mut open_until, held);
                    }
                    match captured {
                        Captured::Entered { edge, cursor } => {
                            // A newly closed barrier can still activate before the desktop removes it.
                            let entered = current.plane.enter(edge, cursor).filter(|entered| {
                                !driven
                                    && cooling.is_none()
                                    && !crate::desktop_input::replayed_within(DRIVEN_GUARD)
                                    && !broken.contains_key(&entered.device)
                            });
                            peersus_diagnostics::dev_trace!(stage = "capture-enter", entered = entered.is_some(), "Pointer reached desktop barrier");
                            push_until = None;
                            let Some(entered) = entered else {
                                if let Some(gone) = visit.take() {
                                    send(&current, &gone.device, Vec::new(), Some(Ending::Visit));
                                    open_until = None;
                                    held = 0;
                                }
                                pushing = None;
                                give_back(&capture, Some(let_go(edge, cursor)), &mut cooling);
                                continue;
                            };

                            // A pointer already carried over that is stopped again goes on from there.
                            let ready = if visit.is_some() {
                                Some(entered)
                            } else {
                                begin_departure(&mut pushing, entered, edge, current.pressure)
                            };
                            if let Some(entered) = ready {
                                if let Some((x, y)) = entered.share(&current.plane) {
                                    begin(&current, &entered.device, x, y);
                                    resting(&mut open_until, held);
                                }
                                visit = Some(entered);
                            } else {
                                push_until = Some(at + PUSH_IDLE);
                            }
                        }
                        Captured::Motion { dx, dy } => {
                            let step = follow_motion(
                                &current, &mut pushing, visit.as_mut(), &mut push_until, at, dx, dy,
                            );
                            match step {
                                MotionStep::Unchanged => {}
                                MotionStep::GiveBack(there) => {
                                    give_back(&capture, Some(there.from), &mut cooling);
                                }
                                MotionStep::Enter(entered) => {
                                    if let Some((x, y)) = entered.share(&current.plane) {
                                        begin(&current, &entered.device, x, y);
                                        resting(&mut open_until, held);
                                    }
                                    visit = Some(entered);
                                }
                                MotionStep::Visiting(Some(Step::There { x, y })) => {
                                    latest = Some((x, y));
                                }
                                MotionStep::Visiting(Some(Step::Back { at })) => {
                                    let Some(there) = visit.as_ref() else { continue };
                                    flush_position(&current, Some(there), &mut latest);
                                    tracing::debug!(?at, "Pointer pushed back to this computer");
                                    give_back(&capture, Some(at), &mut cooling);
                                    send(&current, &there.device, Vec::new(), Some(Ending::Visit));
                                    visit = None;
                                    push_until = None;
                                    open_until = None;
                                    held = 0;
                                }
                                MotionStep::Visiting(None) => {
                                    let Some(there) = visit.as_ref() else { continue };
                                    tracing::warn!("The pointer was given back: its place on the device it was on was lost");
                                    give_back(&capture, Some(there.from), &mut cooling);
                                    send(&current, &there.device, Vec::new(), Some(Ending::Visit));
                                    latest = None;
                                    visit = None;
                                    push_until = None;
                                    open_until = None;
                                    held = 0;
                                }
                            }
                        }
                        Captured::Left => {
                            // The desktop let it go on its own (including the two-key escape).
                            push_until = None;
                            if let Some(gone) = visit.take() {
                                give_back(&capture, None, &mut cooling);
                                send(&current, &gone.device, Vec::new(), Some(Ending::Visit));
                                open_until = None;
                                held = 0;
                            } else if pushing.take().is_some() {
                                give_back(&capture, None, &mut cooling);
                            }
                        }
                        other => {
                            // A key, button or wheel cancels a local push; that one input was
                            // already taken by the desktop. In a visit it is the other device's.
                            if let Some((there, _)) = pushing.take() {
                                push_until = None;
                                give_back(&capture, Some(there.from), &mut cooling);
                                continue;
                            }
                            if let (Some(there), Some(input)) = (&visit, passed(&other)) {
                                let pressing = matches!(input, PointerInput::Button { down: true, .. } | PointerInput::KeyCode { down: true, .. });
                                send(&current, &there.device, vec![input], None);
                                // A press held is a drag or a key held down: closing would let go of it.
                                if pressing { held += 1 } else if matches!(other, Captured::Button { down: false, .. } | Captured::Key { down: false, .. }) { held = held.saturating_sub(1) }
                                resting(&mut open_until, held);
                            }
                        }
                    }
                }
                if flush_position(&current, visit.as_ref(), &mut latest) {
                    resting(&mut open_until, held);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pointer_goes_over_only_once_it_is_pushed_against_the_edge() {
        // A touch is not a push: one step against the edge is nothing yet, and enough of them go through.
        let amount = NonZeroU16::new(80).unwrap();
        let mut push = Push::at(Edge::Left, amount);
        assert_eq!(push.moved(-8., 0.), Pushed::More);
        let steps = (0..9).map(|_| push.moved(-8., 1.)).collect::<Vec<_>>();
        assert!(steps[..8].iter().all(|step| *step == Pushed::More));
        assert_eq!(steps[8], Pushed::Through { extra: 0. });
        // Out is away from the screen, on every edge.
        for (edge, dx, dy) in [
            (Edge::Left, -90., 0.),
            (Edge::Right, 90., 0.),
            (Edge::Top, 0., -90.),
            (Edge::Bottom, 0., 90.),
        ] {
            assert_eq!(
                Push::at(edge, amount).moved(dx, dy),
                Pushed::Through { extra: 10. },
                "{edge:?}"
            );
            assert_eq!(
                Push::at(edge, amount).moved(-dx, -dy),
                Pushed::Back,
                "{edge:?}"
            );
        }
        // The jitter of a pushing hand does not let go; pulling back does.
        let mut push = Push::at(Edge::Right, amount);
        assert_eq!(push.moved(30., 2.), Pushed::More);
        assert_eq!(push.moved(-10., -2.), Pushed::More);
        assert_eq!(push.moved(70., 0.), Pushed::Through { extra: 10. });
        let mut push = Push::at(Edge::Right, amount);
        assert_eq!(push.moved(10., 0.), Pushed::More);
        assert_eq!(push.moved(-30., 0.), Pushed::Back);
        // A pointer travelling along the edge is not pushing through it.
        let mut push = Push::at(Edge::Left, amount);
        assert_eq!(push.moved(-2., 40.), Pushed::More);
        assert_eq!(push.moved(-2., 40.), Pushed::Back);
        // An amount no hand makes says nothing.
        let mut push = Push::at(Edge::Left, amount);
        assert_eq!(push.moved(f64::NAN, 0.), Pushed::More);
        assert_eq!(push.moved(f64::NEG_INFINITY, 0.), Pushed::More);
        assert_eq!(push.moved(-80., 0.), Pushed::Through { extra: 0. });
    }

    fn rightward_route() -> Route {
        use crate::pass_through::Plane;
        use peersus_core::layout::{Display, Placement};
        let display = Display {
            id: "screen".into(),
            x: 0,
            y: 0,
            width: 100,
            height: 100,
        };
        Route {
            carrier: None,
            pressure: NonZeroU16::new(80),
            plane: Plane::new(
                std::slice::from_ref(&display),
                &[Placement {
                    device: "neighbor".into(),
                    x: 100,
                    y: 0,
                    relative_to_local: None,
                }],
                &[("neighbor".to_owned(), vec![display.clone()])].into(),
            ),
        }
    }

    fn rightward_visit() -> Visit {
        rightward_route()
            .plane
            .enter(Edge::Right, (99., 50.))
            .unwrap()
    }

    #[test]
    fn departure_restarts_on_a_new_activation_and_crosses_only_once() {
        let mut pending = None;
        let amount = NonZeroU16::new(80);
        let visit = rightward_visit();
        assert_eq!(
            begin_departure(&mut pending, visit.clone(), Edge::Right, amount),
            None
        );
        assert_eq!(push_departure(&mut pending, 79., 0.), None);
        assert_eq!(
            begin_departure(&mut pending, visit.clone(), Edge::Right, amount),
            None
        );
        assert_eq!(push_departure(&mut pending, 1., 0.), None);
        assert_eq!(push_departure(&mut pending, 79., 0.), Some(Ok(visit)));
        assert_eq!(push_departure(&mut pending, 80., 0.), None);
    }

    #[test]
    fn departure_pulled_back_at_the_slack_is_cancelled_and_off_enters_at_once() {
        let mut pending = None;
        let visit = rightward_visit();
        assert_eq!(
            begin_departure(
                &mut pending,
                visit.clone(),
                Edge::Right,
                NonZeroU16::new(80)
            ),
            None
        );
        assert_eq!(push_departure(&mut pending, 10., 0.), None);
        assert_eq!(
            push_departure(&mut pending, -26., 0.),
            Some(Err(visit.clone()))
        );
        assert_eq!(push_departure(&mut pending, 100., 0.), None);
        assert_eq!(
            begin_departure(&mut pending, visit.clone(), Edge::Right, None),
            Some(visit)
        );
        assert!(pending.is_none());
    }

    #[tokio::test]
    async fn capture_turn_bounds_work_without_netting_opposing_samples() {
        let (tx, mut taken) = tokio::sync::mpsc::unbounded_channel();
        let outward = Captured::Motion { dx: 40., dy: 0. };
        let inward = Captured::Motion { dx: -40., dy: 0. };
        let at = Instant::now();
        let push = |event| tx.send(Sample { at, event }).unwrap();
        for _ in 0..CAPTURE_TURN {
            push(outward.clone());
            push(inward.clone());
        }
        let key = Captured::Key {
            code: 30,
            down: true,
        };
        push(key.clone());
        let mut batch = Vec::with_capacity(CAPTURE_TURN);
        for _ in 0..2 {
            assert_eq!(
                capture_turn(&mut taken, &mut batch, None).await,
                CaptureTurn::Read(CAPTURE_TURN)
            );
            assert!(
                batch
                    .chunks_exact(2)
                    .all(|pair| pair[0].event == outward && pair[1].event == inward)
            );
            batch.clear();
        }
        assert_eq!(
            capture_turn(&mut taken, &mut batch, None).await,
            CaptureTurn::Read(1)
        );
        assert_eq!(batch[0].event, key);
    }

    #[tokio::test]
    async fn queued_motion_at_the_deadline_starts_a_new_return_push() {
        let (tx, mut taken) = tokio::sync::mpsc::unbounded_channel();
        let route = rightward_route();
        let mut visit = rightward_visit();
        let mut pushing = None;
        let mut until = None;
        let first = Instant::now() - 2 * PUSH_IDLE;
        let mut batch = Vec::new();
        tx.send(Sample {
            at: first,
            event: Captured::Motion { dx: -80., dy: 0. },
        })
        .unwrap();
        assert_eq!(
            capture_turn(&mut taken, &mut batch, until).await,
            CaptureTurn::Read(1)
        );
        let mut advance = |batch: &mut Vec<Sample>, until: &mut Option<Instant>| {
            let Sample {
                at,
                event: Captured::Motion { dx, dy },
            } = batch.pop().unwrap()
            else {
                panic!("expected a captured motion");
            };
            follow_motion(&route, &mut pushing, Some(&mut visit), until, at, dx, dy)
        };
        assert!(matches!(
            advance(&mut batch, &mut until),
            MotionStep::Visiting(Some(Step::There { .. }))
        ));
        assert_eq!(until, Some(first + PUSH_IDLE));
        // Both capture input and the idle deadline are ready. The sample, not the select, decides.
        tx.send(Sample {
            at: first + PUSH_IDLE,
            event: Captured::Motion { dx: -1., dy: 0. },
        })
        .unwrap();
        assert_eq!(
            capture_turn(&mut taken, &mut batch, until).await,
            CaptureTurn::Read(1)
        );
        assert!(matches!(
            advance(&mut batch, &mut until),
            MotionStep::Visiting(Some(Step::There { .. }))
        ));
        tx.send(Sample {
            at: first + PUSH_IDLE + std::time::Duration::from_millis(1),
            event: Captured::Motion { dx: -79., dy: 0. },
        })
        .unwrap();
        assert_eq!(
            capture_turn(&mut taken, &mut batch, until).await,
            CaptureTurn::Read(1)
        );
        assert!(matches!(
            advance(&mut batch, &mut until),
            MotionStep::Visiting(Some(Step::Back { .. }))
        ));
    }

    #[tokio::test]
    async fn queued_zero_sample_does_not_keep_old_return_pressure_alive() {
        let (tx, mut taken) = tokio::sync::mpsc::unbounded_channel();
        let route = rightward_route();
        let mut visit = rightward_visit();
        let mut pushing = None;
        let mut until = None;
        let first = Instant::now() - PUSH_IDLE;
        for (offset, dx) in [(0, -80.), (299, 0.), (300, -1.)] {
            tx.send(Sample {
                at: first + std::time::Duration::from_millis(offset),
                event: Captured::Motion { dx, dy: 0. },
            })
            .unwrap();
        }
        let mut batch = Vec::new();
        assert_eq!(
            capture_turn(&mut taken, &mut batch, until).await,
            CaptureTurn::Read(3)
        );
        for Sample { at, event } in batch {
            let Captured::Motion { dx, dy } = event else {
                panic!("expected a captured motion")
            };
            let step = follow_motion(
                &route,
                &mut pushing,
                Some(&mut visit),
                &mut until,
                at,
                dx,
                dy,
            );
            if dx == 0. {
                assert_eq!(step, MotionStep::Unchanged);
                assert_eq!(
                    until,
                    Some(first + PUSH_IDLE),
                    "zero motion cannot extend the push"
                );
            } else {
                assert!(matches!(
                    step,
                    MotionStep::Visiting(Some(Step::There { .. }))
                ));
            }
        }
    }

    #[tokio::test]
    async fn an_idle_departure_releases_without_waiting_for_another_sample() {
        let (_tx, mut taken) = tokio::sync::mpsc::unbounded_channel();
        let route = rightward_route();
        let departure = rightward_visit();
        let mut pushing = None;
        let first = Instant::now() - 2 * PUSH_IDLE;
        let mut until = Some(first + PUSH_IDLE);
        assert!(
            begin_departure(&mut pushing, departure.clone(), Edge::Right, route.pressure).is_none()
        );
        assert_eq!(
            follow_motion(&route, &mut pushing, None, &mut until, first, 79., 0.),
            MotionStep::Unchanged
        );
        let CaptureTurn::Idle(at) = capture_turn(&mut taken, &mut Vec::new(), until).await else {
            panic!("the pending departure should have expired");
        };
        assert_eq!(
            expire_pressure(at, &mut until, &mut pushing, None),
            Some(departure)
        );
        assert!(until.is_none());
        assert!(push_departure(&mut pushing, 1., 0.).is_none());
    }

    #[test]
    fn a_captured_input_the_contract_refuses_is_not_passed_on() {
        use crate::desktop_capture::Captured;
        use peersus_api::contributions::PointerInput;
        // A key of a keyboard goes through, pressed and let go.
        assert_eq!(
            passed(&Captured::Key {
                code: 30,
                down: true
            }),
            Some(PointerInput::KeyCode {
                code: 30,
                down: true
            })
        );
        // The range of buttons and tools, and anything past what a keyboard has, would have a whole batch
        // refused: each is left out by itself instead.
        for code in [0, 0x100, 0x113, 0x14b, 0x15f, 0x300, 70_000] {
            assert_eq!(
                passed(&Captured::Key { code, down: true }),
                None,
                "{code:#x}"
            );
        }
        // Everything this passes on is something the contract accepts.
        for captured in [
            Captured::Key {
                code: 0x160,
                down: false,
            },
            Captured::Button {
                code: 0x110,
                down: true,
            },
            Captured::Scroll { dx: 900, dy: -900 },
        ] {
            assert!(passed(&captured).is_some_and(|input| input.validate().is_ok()));
        }
        assert_eq!(
            passed(&Captured::Button {
                code: 0x113,
                down: true
            }),
            None
        );
    }

    #[test]
    fn an_edge_that_cannot_be_entered_now_has_no_barrier() {
        use crate::pass_through::Plane;
        use peersus_core::layout::{Display, Placement};
        let screen = |id: &str, width, height| Display {
            id: id.into(),
            x: 0,
            y: 0,
            width,
            height,
        };
        let place = |device: &str, x| Placement {
            device: device.into(),
            x,
            y: 0,
            relative_to_local: None,
        };
        let route = Route {
            carrier: None,
            pressure: None,
            plane: Plane::new(
                &[screen("a", 1920, 1080)],
                &[place("left", -1280), place("right", 1920)],
                &std::collections::BTreeMap::from([
                    ("left".to_owned(), vec![screen("b", 1280, 1080)]),
                    ("right".to_owned(), vec![screen("c", 1280, 1080)]),
                ]),
            ),
        };
        let toward = |broken: &HashMap<String, tokio::time::Instant>, driven| {
            let mut devices: Vec<String> = open_edges(&route, broken, driven)
                .into_iter()
                .map(|barrier| barrier.device)
                .collect();
            devices.sort();
            devices
        };
        let none = HashMap::new();
        assert_eq!(toward(&none, false), ["left", "right"]);
        // The device that could not be reached is not entered again while its mark stands; the other still is.
        let broken = HashMap::from([("left".to_owned(), tokio::time::Instant::now())]);
        assert_eq!(toward(&broken, false), ["right"]);
        // While a device drives this desktop nothing is taken at any edge.
        assert!(toward(&none, true).is_empty());
        assert!(toward(&broken, true).is_empty());
    }

    type Handed = Arc<std::sync::Mutex<Vec<(Instant, Outgoing)>>>;

    fn batch(inputs: Vec<PointerInput>, end: bool, visit: u64) -> Outgoing {
        (
            PluginId::new("carrier").unwrap(),
            PeerId("far".into()),
            inputs,
            end.then_some(Ending::Visit),
            visit,
        )
    }

    fn place(x: f32, visit: u64) -> Outgoing {
        batch(vec![PointerInput::Position { x, y: 0. }], false, visit)
    }

    fn press(visit: u64) -> Outgoing {
        batch(
            vec![PointerInput::Button {
                button: peersus_api::contributions::Button::Left,
                down: true,
            }],
            false,
            visit,
        )
    }

    /// Runs the forwarder over what `feed` sends, in virtual time, until the capture is gone. Returns when each
    /// thing was handed over, measured from the start.
    async fn handed(
        stopped: u64,
        feed: impl AsyncFnOnce(mpsc::UnboundedSender<Outgoing>),
    ) -> Vec<(Duration, Outgoing)> {
        let (outgoing, queued) = mpsc::unbounded_channel();
        let record: Handed = Arc::default();
        let started = Instant::now();
        let writer = record.clone();
        let forwarder = tokio::spawn(forward_in_order(
            queued,
            Arc::new(AtomicU64::new(stopped)),
            move |batch| {
                let writer = writer.clone();
                async move {
                    writer.lock().unwrap().push((Instant::now(), batch));
                }
            },
        ));
        feed(outgoing).await;
        forwarder.await.unwrap();
        let taken = std::mem::take(&mut *record.lock().unwrap());
        taken
            .into_iter()
            .map(|(at, batch)| (at - started, batch))
            .collect()
    }

    fn at_of(batch: &Outgoing) -> f32 {
        match batch.2.as_slice() {
            [PointerInput::Position { x, .. }] => *x,
            other => panic!("not a place: {other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_mouse_reporting_every_millisecond_is_one_call_a_frame_and_the_newest_place_is_never_lost()
     {
        let handed = handed(0, async |outgoing| {
            for report in 0..200u16 {
                outgoing.send(place(f32::from(report) / 200., 1)).unwrap();
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            // The capture goes on being there: a closed queue would flush the last place early.
            tokio::time::sleep(Duration::from_millis(20)).await;
        })
        .await;
        // The first place goes at once; no two calls are closer than a frame.
        assert_eq!(handed[0].0, Duration::ZERO);
        assert!(
            handed.windows(2).all(|pair| pair[1].0 - pair[0].0 >= FRAME),
            "{:?}",
            handed.iter().map(|(at, _)| at).collect::<Vec<_>>()
        );
        // 200 ms is at most 26 frames, not 200 reports.
        assert!(handed.len() <= 26, "{} calls", handed.len());
        assert!(handed.len() >= 20, "{} calls", handed.len());
        // Places only go forward, and the last one the hand reached is the last one sent.
        let sent: Vec<f32> = handed.iter().map(|(_, batch)| at_of(batch)).collect();
        assert!(sent.windows(2).all(|pair| pair[0] < pair[1]), "{sent:?}");
        assert_eq!(*sent.last().unwrap(), 199. / 200.);
    }

    #[tokio::test(start_paused = true)]
    async fn a_press_is_never_held_or_merged_and_goes_after_the_place_before_it() {
        let handed = handed(0, async |outgoing| {
            outgoing.send(place(0.1, 1)).unwrap();
            tokio::time::sleep(Duration::from_millis(2)).await;
            outgoing.send(place(0.2, 1)).unwrap();
            outgoing.send(place(0.3, 1)).unwrap();
            tokio::time::sleep(Duration::from_millis(1)).await;
            outgoing.send(press(1)).unwrap();
            outgoing.send(place(0.4, 1)).unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        })
        .await;
        // 0.1 at once; 0.3 (which replaced 0.2) and the press the moment the press came, before the frame was over;
        // 0.4 only when a frame has passed since 0.3.
        let told: Vec<_> = handed
            .iter()
            .map(|(at, batch)| (*at, batch.2.clone()))
            .collect();
        assert_eq!(told.len(), 4, "{told:?}");
        assert_eq!(told[0], (Duration::ZERO, place(0.1, 1).2));
        assert_eq!(told[1], (Duration::from_millis(3), place(0.3, 1).2));
        assert_eq!(told[2], (Duration::from_millis(3), press(1).2));
        assert_eq!(told[3], (Duration::from_millis(3) + FRAME, place(0.4, 1).2));
    }

    #[tokio::test(start_paused = true)]
    async fn the_end_of_a_visit_follows_its_last_place_at_once() {
        let handed = handed(0, async |outgoing| {
            outgoing.send(place(0.1, 1)).unwrap();
            tokio::time::sleep(Duration::from_millis(1)).await;
            outgoing.send(place(0.2, 1)).unwrap();
            outgoing.send(batch(Vec::new(), true, 1)).unwrap();
        })
        .await;
        assert_eq!(handed.len(), 3);
        assert_eq!(at_of(&handed[1].1), 0.2);
        assert_eq!(handed[2].1.3, Some(Ending::Visit));
        assert_eq!(handed[1].0, Duration::from_millis(1));
        assert_eq!(handed[2].0, Duration::from_millis(1));
    }

    #[tokio::test(start_paused = true)]
    async fn the_first_place_of_a_new_visit_is_not_held_by_the_frame_of_the_last_one() {
        let handed = handed(0, async |outgoing| {
            outgoing.send(place(0.1, 1)).unwrap();
            tokio::time::sleep(Duration::from_millis(1)).await;
            outgoing.send(place(0.9, 2)).unwrap();
        })
        .await;
        assert_eq!(handed.len(), 2);
        assert_eq!(handed[1].1.4, 2);
        assert_eq!(handed[1].0, Duration::from_millis(1));
    }

    #[tokio::test(start_paused = true)]
    async fn input_of_a_visit_that_was_stopped_is_dropped_and_a_later_one_is_not() {
        let handed = handed(2, async |outgoing| {
            outgoing.send(place(0.1, 1)).unwrap();
            outgoing.send(press(2)).unwrap();
            outgoing.send(place(0.5, 3)).unwrap();
        })
        .await;
        assert_eq!(handed.len(), 1);
        assert_eq!(handed[0].1.4, 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_send_does_not_add_a_frame_on_top_and_what_piled_up_is_one_place() {
        let (outgoing, queued) = mpsc::unbounded_channel();
        let record: Handed = Arc::default();
        let writer = record.clone();
        let started = Instant::now();
        let forwarder = tokio::spawn(forward_in_order(
            queued,
            Arc::new(AtomicU64::new(0)),
            move |batch| {
                let writer = writer.clone();
                async move {
                    writer.lock().unwrap().push((Instant::now(), batch));
                    // The extension takes far longer than a frame to answer.
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
        ));
        for report in 0..50u16 {
            outgoing.send(place(f32::from(report) / 50., 1)).unwrap();
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        drop(outgoing);
        forwarder.await.unwrap();
        let taken = record.lock().unwrap();
        // The first place, then at the moment it was answered the newest of the ones that piled up, and nothing else.
        let told: Vec<(Duration, f32)> = taken
            .iter()
            .map(|(at, batch)| (*at - started, at_of(batch)))
            .collect();
        assert_eq!(
            told,
            [
                (Duration::ZERO, 0.),
                (Duration::from_millis(100), 49. / 50.)
            ]
        );
    }
}
