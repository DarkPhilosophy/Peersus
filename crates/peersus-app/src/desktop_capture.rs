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
}

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
            Err(error) => {
                tracing::debug!(%error, "Input capture session ended");
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

async fn session(
    barriers: &[Barrier],
    orders: &mut mpsc::UnboundedReceiver<Order>,
    events: &mpsc::UnboundedSender<Captured>,
    token: &PathBuf,
) -> Result<Ended, Box<dyn std::error::Error>> {
    let portal = InputCapture::new().await?;
    let session = open(&portal, token).await?;
    let zones = portal
        .zones(&session, Default::default())
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
        .set_pointer_barriers(&session, &set, zones.zone_set(), Default::default())
        .await?
        .response()?;
    let fd = portal.connect_to_eis(&session, Default::default()).await?;
    let context = ei::Context::new(UnixStream::from(fd))?;
    let (_connection, mut input) = context
        .handshake_tokio("io.peersus.Peersus", ei::handshake::ContextType::Receiver)
        .await?;
    // Listened to before enabling: the desktop may hand the pointer over at once.
    let mut activated = portal.receive_activated().await?;
    let mut deactivated = portal.receive_deactivated().await?;
    portal.enable(&session, Default::default()).await?;
    let mut active: Option<Option<u32>> = None;
    // New barriers while the pointer is taken: a new session now would drop the pointer on the floor in the middle of a visit (the
    // desktop forgets the capture, the visit goes on), which is the pointer that seems to be on both computers at once. They wait
    // until the pointer is given back.
    let mut pending: Option<Vec<Barrier>> = None;
    let ended = loop {
        tokio::select! {
            signal = activated.next() => {
                let Some(signal) = signal else { break Err("the desktop stopped saying when it takes the pointer".into()) };
                let edge = match signal.barrier_id() {
                    Some(ActivatedBarrier::Barrier(id)) => edges.get(&id).copied(),
                    _ => None,
                };
                let (Some(edge), Some(cursor)) = (edge, signal.cursor_position()) else { continue };
                active = Some(signal.activation_id());
                let _ = events.send(Captured::Entered { edge, cursor: (f64::from(cursor.0), f64::from(cursor.1)) });
            }
            signal = deactivated.next() => {
                if signal.is_none() { break Err("the desktop stopped saying when it gives the pointer back".into()) }
                if active.take().is_some() {
                    let _ = events.send(Captured::Left);
                }
                if let Some(next) = pending.take() {
                    break Ok(Ended::Barriers(next));
                }
            }
            event = input.next() => {
                let Some(event) = event else { break Err("the input connection closed".into()) };
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
                if let Some(captured) = translate(&event) {
                    let _ = events.send(captured);
                }
            }
            order = orders.recv() => match order {
                None => break Ok(Ended::Stopped),
                Some(Order::Barriers(next)) if next.as_slice() != barriers => {
                    if active.is_some() {
                        pending = Some(next);
                    } else {
                        break Ok(Ended::Barriers(next));
                    }
                }
                Some(Order::Barriers(_)) => pending = None,
                Some(Order::Release { at }) => {
                    if let Some(activation) = active.take() {
                        let options = ReleaseOptions::default()
                            .set_activation_id(activation)
                            .set_cursor_position(Some(at));
                        portal.release(&session, options).await?;
                    }
                    if let Some(next) = pending.take() {
                        break Ok(Ended::Barriers(next));
                    }
                }
            },
        }
    };
    let _ = session.close().await;
    ended
}

fn translate(event: &EiEvent) -> Option<Captured> {
    Some(match event {
        EiEvent::PointerMotion(motion) => Captured::Motion {
            dx: f64::from(motion.dx),
            dy: f64::from(motion.dy),
        },
        EiEvent::Button(button) => Captured::Button {
            code: button.button,
            down: button.state == ei::button::ButtonState::Press,
        },
        EiEvent::ScrollDiscrete(scroll) => Captured::Scroll {
            dx: scroll.discrete_dx / 120,
            dy: scroll.discrete_dy / 120,
        },
        EiEvent::KeyboardKey(key) => Captured::Key {
            code: key.key,
            down: key.state == ei::keyboard::KeyState::Press,
        },
        _ => return None,
    })
}
