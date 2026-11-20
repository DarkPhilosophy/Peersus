//! The pointer taken at this desktop's edges and what is done beyond them carried to the device there:
//! the capture ([`crate::desktop_capture`]) driven by where the layout says the pointer goes
//! ([`crate::pass_through`]), for as long as the owner runs. The one place that knows both; neither
//! of them knows this.
use crate::pass_through::{Barrier, Route, Step, Visit, let_go, route};
use std::collections::HashMap;

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

/// Take the pointer at the layout's edges and carry what is done on the other side to the extension
/// that sends it there, for as long as the owner runs.
pub async fn follow(client: crate::runtime::Client, data: std::path::PathBuf) {
    use crate::desktop_capture::{Capture, Captured};
    use peersus_api::contributions::PointerInput;
    use peersus_core::model::{Command, PeerId};
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
    // A captured event read ahead while gathering motion, handled next, in order.
    let mut deferred: Option<Captured> = None;
    // How long after the last input a device replayed here this desktop still counts as driven by it.
    const DRIVEN_GUARD: std::time::Duration = std::time::Duration::from_secs(2);
    let mut driven = crate::desktop_input::replayed_within(DRIVEN_GUARD);
    let mut driven_check = tokio::time::Instant::now() + DRIVEN_GUARD;
    // Sending waits for the owner and, through it, for the device: on a slow or failing link that is seconds. The capture loop never
    // waits for it, so the pointer comes back the moment it is pushed back, whatever the link does. One task sends, in order; positions
    // that piled up behind a slow send are one position, the newest, since each is absolute.
    type Outgoing = (
        peersus_core::plugins::PluginId,
        PeerId,
        Vec<serde_json::Value>,
        bool,
    );
    let (outgoing, mut queued) = tokio::sync::mpsc::unbounded_channel::<Outgoing>();
    // A device whose forward just failed: the pointer is given back at once and the edges toward it are closed for a short
    // while — a link that is down must not hold the person's pointer, and must not be entered again at every push while it is.
    const BROKEN_GUARD: std::time::Duration = std::time::Duration::from_secs(10);
    let mut broken: HashMap<String, tokio::time::Instant> = HashMap::new();
    // Until when the edges are closed because the pointer was just given back.
    let mut cooling: Option<tokio::time::Instant> = None;
    capture.set_barriers(open_edges(&current, &broken, driven));
    let (dead, mut dead_rx) = tokio::sync::mpsc::unbounded_channel::<PeerId>();
    // A batch is accepted long before it is known whether it arrived: the extension answers later, or
    // not at all within its time. That later failure is heard here, so the pointer is given back then
    // too, and not only when a command is refused outright.
    let mut failures = client.surface_failures();
    let failed_later = dead.clone();
    let sender_client = client.clone();
    tokio::spawn(async move {
        let mut ahead: Option<Outgoing> = None;
        loop {
            let Some(mut next) = (match ahead.take() {
                Some(next) => Some(next),
                None => queued.recv().await,
            }) else {
                return;
            };
            let only_position = |batch: &Outgoing| {
                !batch.3
                    && batch.2.len() == 1
                    && batch.2[0].get("t").and_then(|t| t.as_str()) == Some("position")
            };
            while only_position(&next) {
                match queued.try_recv() {
                    Ok(later)
                        if only_position(&later) && later.0 == next.0 && later.1 == next.1 =>
                    {
                        next = later
                    }
                    Ok(later) => {
                        ahead = Some(later);
                        break;
                    }
                    Err(_) => break,
                }
            }
            let (plugin, peer, events, end) = next;
            // What a refused batch was, for the log: how many inputs and whether it was the pointer's place. Never which key or button.
            let count = events.len();
            let place = events
                .iter()
                .all(|event| event.get("t").and_then(|t| t.as_str()) == Some("position"));
            let result = sender_client
                .command(Command::ForwardInput {
                    plugin,
                    peer: peer.clone(),
                    events,
                    end,
                })
                .await;
            peersus_diagnostics::dev_trace!(
                stage = "capture-forward",
                end,
                outcome = if result.is_ok() { "ok" } else { "failed" },
                "Captured input dispatched"
            );
            // The device could not be reached: the follower is told, so the pointer it holds
            // is given back instead of disappearing into a link that is down.
            if let Err(reason) = result {
                tracing::debug!(reason, count, place, end, "Captured input was refused");
                let _ = dead.send(peer);
            }
        }
    });
    let send = |route: &Route, device: &str, events: Vec<PointerInput>, end: bool| {
        let carrier = route.carrier.clone();
        let peer = PeerId(device.to_owned());
        let outgoing = outgoing.clone();
        async move {
            let Some(plugin) = carrier else {
                return;
            };
            let events = events
                .iter()
                .filter_map(|event| serde_json::to_value(event).ok())
                .collect();
            // Queued in capture order, including the end before another device is entered.
            let _ = outgoing.send((plugin, peer, events, end));
        }
    };
    loop {
        let rest_at = open_until;
        tokio::select! {
            () = async { tokio::time::sleep_until(rest_at.unwrap_or_else(tokio::time::Instant::now)).await }, if rest_at.is_some() => {
                open_until = None;
                if let Some(there) = visit.as_mut() {
                    if held == 0 {
                        send(&current, &there.device, Vec::new(), true).await;
                    } else if let Some(Step::There { x, y }) = there.moved(&current.plane, 0., 0.) {
                        send(&current, &there.device, vec![PointerInput::Position { x, y }], false).await;
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
                    capture.set_barriers(Vec::new());
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
            Some(device) = dead_rx.recv() => {
                // The device the pointer is visiting could not be reached: the capture is given
                // back in from where it left this desktop, and the edges toward that device are
                // closed until its broken mark ages out — otherwise every push of the pointer
                // would bounce through the same dead link.
                broken.insert(device.0.clone(), tokio::time::Instant::now());
                if let Some(there) = visit.take_if(|there| there.device == device.0) {
                    tracing::warn!("The pointer was given back: the device it was on could not be reached");
                    give_back(&capture, Some(there.from), &mut cooling);
                    open_until = None;
                    held = 0;
                } else {
                    capture.set_barriers(open_edges(&current, &broken, driven || cooling.is_some()));
                }
            }
            failed = failures.recv() => {
                use tokio::sync::broadcast::error::RecvError;
                let device = match failed {
                    Ok(key) if current.carrier.as_ref() == Some(&key.plugin) => key.device,
                    Ok(_) => None,
                    // Failures were said faster than they were heard: whichever device is visited is one of them.
                    Err(RecvError::Lagged(_)) => visit.as_ref().map(|there| PeerId(there.device.clone())),
                    Err(RecvError::Closed) => return,
                };
                if let Some(device) = device {
                    let _ = failed_later.send(device);
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
                    if let Some(gone) = visit.take_if(|there| !next.plane.keeps(there)) {
                        send(&current, &gone.device, Vec::new(), true).await;
                        give_back(&capture, Some(gone.from), &mut cooling);
                    } else {
                        capture.set_barriers(open_edges(&next, &broken, driven || cooling.is_some()));
                    }
                    current = next;
                }
            }
            captured = async { match deferred.take() { Some(next) => Some(next), None => taken.recv().await } } => {
                let Some(captured) = captured else { return };
                // After anything sent: the connection is closed once the pointer rests, or kept alive while something is held.
                let resting = |open_until: &mut Option<tokio::time::Instant>, held: u32| {
                    *open_until = Some(tokio::time::Instant::now() + if held == 0 { REST } else { HOLD });
                };
                match captured {
                    Captured::Entered { edge, cursor } => {
                        // An edge closed a moment ago can still be reached before the desktop has taken its barrier away, and a
                        // layout that changed can leave a barrier that leads nowhere: the pointer is given back in from there,
                        // never kept with no device to go to.
                        let entered = current.plane.enter(edge, cursor).filter(|entered| {
                            !driven
                                && cooling.is_none()
                                && !crate::desktop_input::replayed_within(DRIVEN_GUARD)
                                && !broken.contains_key(&entered.device)
                        });
                        peersus_diagnostics::dev_trace!(stage = "capture-enter", entered = entered.is_some(), "Pointer reached desktop barrier");
                        let Some(entered) = entered else {
                            tracing::debug!("The pointer reached an edge it cannot be carried through and was given back");
                            give_back(&capture, Some(let_go(edge, cursor)), &mut cooling);
                            continue;
                        };
                        if let Some((x, y)) = entered.share(&current.plane) {
                            send(&current, &entered.device, vec![PointerInput::Position { x, y }], false).await;
                            resting(&mut open_until, held);
                        }
                        visit = Some(entered);
                    }
                    Captured::Motion { mut dx, mut dy } => {
                        let Some(there) = visit.as_mut() else { continue };
                        // Motion that arrived while the last position was on its way to a slow device is one move, not a queue of
                        // old positions replayed late: the pointer would trail the hand by seconds and only then come back.
                        while let Ok(next) = taken.try_recv() {
                            match next {
                                Captured::Motion { dx: more_x, dy: more_y } => {
                                    dx += more_x;
                                    dy += more_y;
                                }
                                other => {
                                    deferred = Some(other);
                                    break;
                                }
                            }
                        }
                        match there.moved(&current.plane, dx, dy) {
                            Some(Step::There { x, y }) => {
                                send(&current, &there.device, vec![PointerInput::Position { x, y }], false).await;
                                resting(&mut open_until, held);
                            }
                            Some(Step::Back { at }) => {
                                tracing::debug!(?at, "Pointer pushed back to this computer");
                                give_back(&capture, Some(at), &mut cooling);
                                send(&current, &there.device, Vec::new(), true).await;
                                visit = None;
                                open_until = None;
                                held = 0;
                            }
                            None => {
                                // Where the pointer is on that device is no longer known (its screens changed under it): it
                                // is given back here, never left taken with every motion going nowhere.
                                tracing::warn!("The pointer was given back: its place on the device it was on was lost");
                                let (device, from) = (there.device.clone(), there.from);
                                give_back(&capture, Some(from), &mut cooling);
                                send(&current, &device, Vec::new(), true).await;
                                visit = None;
                                open_until = None;
                                held = 0;
                            }
                        }
                    }
                    Captured::Left => {
                        // The desktop let the pointer go by itself (the way out with two keys, or its own reasons), where it
                        // held it: on the barrier. Only a visit is ended this way: the same word comes when taking the pointer
                        // could not start at all, and that is not tried again here.
                        if let Some(gone) = visit.take() {
                            give_back(&capture, None, &mut cooling);
                            send(&current, &gone.device, Vec::new(), true).await;
                            open_until = None;
                            held = 0;
                        }
                    }
                    other => {
                        if let (Some(there), Some(input)) = (&visit, passed(&other)) {
                            let pressing = matches!(input, PointerInput::Button { down: true, .. } | PointerInput::KeyCode { down: true, .. });
                            send(&current, &there.device, vec![input], false).await;
                            // A press held is a drag or a key held down: closing would let go of it.
                            if pressing { held += 1 } else if matches!(other, Captured::Button { down: false, .. } | Captured::Key { down: false, .. }) { held = held.saturating_sub(1) }
                            resting(&mut open_until, held);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        };
        let route = Route {
            carrier: None,
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
}
