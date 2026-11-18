//! The announcement outside the window, on a desktop: of an offer, and of a notification that reaches outside.
//!
//! A file offered to this computer waits for an answer for a bounded time. With the window in front the
//! window itself shows it. With the window behind another, minimised, or closed while the owner runs in the
//! background, nothing did: the offer ran out unseen. This shows it then, as a small panel at the edge of the
//! screen the person chose, with the same answers the window gives. A notification whose sender asked for it
//! to reach outside the window (`peersus_api::notification::Reach::Outside`) is shown by the same panel.
//!
//! WHO DRAWS IT. The owner has no window and no toolkit. The panel is the window program, started beside the
//! owner with [`ARGUMENT`]: it attaches to the owner like any window (without counting as one, so that it never
//! keeps an owner alive that should end), draws the one offer, and answers through the same commands. Nothing
//! about an offer is passed to it: it reads the state the owner publishes.
//!
//! WHEN. The rule is [`wanted`]: an offer waits, and no window says it is in front. The owner starts the panel
//! when the rule becomes true and stops it when it becomes false; the panel never decides for itself. While
//! nothing waits nothing is looked at on a clock: the follower sleeps on the state.
//!
//! WHERE IT IS. Linux only, for now: the owner of no other system starts one, and their Settings do not offer
//! its place. A panel is placed by the program, which a Wayland compositor does not allow a client to do; it is
//! therefore drawn through the X server (the one Wayland sessions run for older programs). A session with no X
//! display has no panel, which is said once in the log and is not an error of the offer: the window still
//! shows it.

use std::{path::Path, time::Duration};

use peersus_core::model::Snapshot;

use crate::{lifetime::Presence, runtime::Client};

/// The argument by which the window program is asked to be the panel and nothing else.
pub const ARGUMENT: &str = "--offer-panel";

/// How often the owner looks at whether a window is in front, and only while an offer waits.
const LOOK_EVERY: Duration = Duration::from_millis(500);

/// The least time between two starts of the panel.
const BETWEEN_STARTS: Duration = Duration::from_secs(5);

/// Whether something is to be shown outside the window: a file offered to this device waits for an answer, or a
/// notification that reaches outside is on show.
fn waits(snapshot: &Snapshot) -> bool {
    snapshot
        .transfers
        .iter()
        .any(|transfer| transfer.awaits_answer())
        || snapshot
            .notifications
            .iter()
            .any(|posted| posted.notification.reach == peersus_api::notification::Reach::Outside)
}

/// Whether the panel should be shown now: something waits to be shown, and no window is in front to show it.
pub fn wanted(snapshot: &Snapshot, window_in_front: bool) -> bool {
    !window_in_front && waits(snapshot)
}

/// Whether the panel may be started now. Not within [`BETWEEN_STARTS`] of the last start, and not again after one ended by
/// itself while the same things wait: a panel that cannot draw (no display it may use, a program that cannot start) ends at
/// once, and each start of it is one more error put in front of the person. What waits next is shown by a new one.
fn may_start(since_last: Option<Duration>, ended_by_itself: bool) -> bool {
    !ended_by_itself && since_last.is_none_or(|waited| waited >= BETWEEN_STARTS)
}

/// The window program, beside the owner and nowhere else: never looked for on the search path.
fn program(owner: &Path) -> Option<std::path::PathBuf> {
    let directory = owner
        .parent()
        .filter(|directory| !directory.as_os_str().is_empty())?;
    let program = directory.join(format!("peersus-ui{}", std::env::consts::EXE_SUFFIX));
    program.is_file().then_some(program)
}

/// Whether this session has an X display, which is where a panel can be placed. The owner asks before it starts one, and
/// the window asks the same of the same session before it offers the choice of an edge.
pub fn has_display(display: Option<&std::ffi::OsStr>) -> bool {
    display.is_some_and(|value| !value.is_empty())
}

/// Why no panel can be started here, as a sentence for the log; `None` when one can.
fn unavailable(owner: &Path, display: Option<&std::ffi::OsStr>) -> Option<&'static str> {
    if program(owner).is_none() {
        return Some("The window program is not beside the owner");
    }
    if !has_display(display) {
        return Some("This session has no X display to place a panel on");
    }
    None
}

/// The authority file the panel needs to reach `display`, when this process was given none: an owner started from a Wayland
/// session may have inherited no XAUTHORITY, and the session's Xwayland refuses a client without its cookie. `None` when one
/// was given (the panel inherits it) or the display is not this user's own local server.
fn authority(display: &std::ffi::OsStr) -> Option<std::path::PathBuf> {
    if std::env::var_os("XAUTHORITY").is_some_and(|given| !given.is_empty()) {
        return None;
    }
    let number = x11rb::reexports::x11rb_protocol::parse_display::parse_display(display.to_str())
        .ok()?
        .display;
    crate::desktop_input::authority_file(number)
}

fn start(program: &Path, authority: Option<&Path>) -> std::io::Result<tokio::process::Child> {
    let mut command = tokio::process::Command::new(program);
    command
        .arg(ARGUMENT)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        // Through the X server, where a program may place its own window: see the module's words.
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("WAYLAND_SOCKET");
    if let Some(authority) = authority {
        command.env("XAUTHORITY", authority);
    }
    command.spawn()
}

/// Show the panel while [`wanted`] holds, for as long as the owner runs. `owner` is the owner's own program.
///
/// Ends when the owner's state ends. Dropping the future stops the panel (it is killed with its handle).
pub async fn follow(client: Client, presence: Presence, owner: std::path::PathBuf) {
    let mut states = client.subscribe();
    let mut panel: Option<tokio::process::Child> = None;
    let mut last_start: Option<std::time::Instant> = None;
    let mut ended_by_itself = false;
    let mut said_unavailable = false;
    loop {
        // Read once: whether to look again on a clock depends on an offer waiting, whoever shows it.
        let (waits, show) = {
            let state = states.borrow_and_update();
            (waits(&state), wanted(&state, presence.in_front()))
        };
        // A panel that ended by itself (it could not draw, its display went, the system closed it) is not one that is shown.
        if let Some(child) = &mut panel
            && matches!(child.try_wait(), Ok(Some(_)) | Err(_))
        {
            panel = None;
            ended_by_itself = true;
            tracing::warn!(
                "The panel of an offer ended by itself; it is not started again for what still waits"
            );
        }
        if !waits {
            ended_by_itself = false;
        }
        match (show, panel.is_some()) {
            (true, false) if may_start(last_start.map(|at| at.elapsed()), ended_by_itself) => {
                let display = std::env::var_os("DISPLAY");
                if let Some(reason) = unavailable(&owner, display.as_deref()) {
                    if !said_unavailable {
                        said_unavailable = true;
                        tracing::warn!("An offer cannot be announced outside the window: {reason}");
                    }
                } else if let (Some(program), Some(display)) = (program(&owner), display) {
                    last_start = Some(std::time::Instant::now());
                    match start(&program, authority(&display).as_deref()) {
                        Ok(child) => panel = Some(child),
                        Err(_) => tracing::warn!("The panel of an offer could not be started"),
                    }
                }
            }
            (false, true) => {
                if let Some(mut child) = panel.take() {
                    let _ = child.kill().await;
                }
            }
            _ => {}
        }
        // With nothing waiting, only a change of the state can make the rule true: nothing is asked on a clock.
        if waits {
            tokio::select! {
                changed = states.changed() => if changed.is_err() { break },
                _ = tokio::time::sleep(LOOK_EVERY) => {}
            }
        } else if states.changed().await.is_err() {
            break;
        }
    }
    if let Some(mut child) = panel {
        let _ = child.kill().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panel_is_not_started_again_at_once_nor_after_it_ended_by_itself() {
        assert!(may_start(None, false));
        assert!(!may_start(Some(Duration::from_secs(1)), false));
        assert!(may_start(Some(BETWEEN_STARTS), false));
        assert!(
            !may_start(Some(BETWEEN_STARTS * 10), true),
            "one that ended by itself waits for the next thing to show"
        );
        assert!(!may_start(None, true));
    }

    #[test]
    fn no_panel_without_the_window_program_or_a_display() {
        let nowhere = Path::new("/nonexistent-directory-of-a-test/peersus");
        assert!(unavailable(nowhere, Some(std::ffi::OsStr::new(":0"))).is_some());
        let directory = tempfile::tempdir().unwrap();
        let owner = directory.path().join("peersus");
        std::fs::write(
            directory
                .path()
                .join(format!("peersus-ui{}", std::env::consts::EXE_SUFFIX)),
            b"",
        )
        .unwrap();
        assert_eq!(unavailable(&owner, Some(std::ffi::OsStr::new(":0"))), None);
        assert!(unavailable(&owner, None).is_some());
        assert!(unavailable(&owner, Some(std::ffi::OsStr::new(""))).is_some());
    }
}
