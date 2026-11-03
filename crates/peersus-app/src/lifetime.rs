//! When an owner started by a window ends with its windows.
//!
//! A window that finds no owner starts one and marks it as started for a window. Every window then keeps a
//! connection to the owner open for as long as it lives (a lease); the owner counts them. When the last
//! lease is gone and the person has not asked for the application to keep running in the background, the
//! owner ends. An owner that was started any other way (from the command line, as a service, by another
//! program) carries no mark, and nothing here ever stops it.
//!
//! WHY LEASES AND NOT THE PARENT PROCESS. The owner's parent is the first window and only it: a second
//! window is not its child, and once the first has gone the parent says nothing about who is still open. A
//! lease is held by every window, so "the last one went" is something the system tells the owner (the
//! connection closes) when a window is closed, killed or crashes, with no timer and no guess.
//!
//! WHAT THIS COVERS.
//! * The only window closes, or is killed, or crashes: the owner ends (the background option being off).
//! * Two windows are open and one closes: the owner stays for the other, and ends when that one goes.
//! * The background option is turned on and later off with a window open: the owner ends when the last
//!   window goes. Nothing is let go of for good, so turning the option off is never ineffective.
//! * No window ever comes (it died before it could attach): the owner ends after [`STARTUP_GRACE`].
//!
//! The rule is pure ([`Presence`]); the owner holds the leases and looks at it on a timer.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

/// The environment variable by which a window marks the owner it starts as started for a window. Its value
/// is not used: only that it is set to something. An owner without it is never ended by this.
pub const STARTED_FOR_A_WINDOW: &str = "PEERSUS_STARTED_FOR_A_WINDOW";

/// How often the owner looks at who is still attached. Often enough that a closed window's owner is gone
/// in a moment, rarely enough to cost nothing.
pub const WATCH_EVERY: Duration = Duration::from_secs(1);

/// How long an owner started for a window waits for its first window to attach before it gives up. The
/// window that started it may have died on the way, and then no lease will ever come to be lost.
pub const STARTUP_GRACE: Duration = Duration::from_secs(15);

/// Whether an environment value marks an owner as started for a window: anything but unset or empty.
pub fn marked(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

/// Which windows are attached to the owner, and whether any ever was.
#[derive(Debug, Clone, Default)]
pub struct Presence {
    attached: Arc<AtomicU32>,
    ever: Arc<std::sync::atomic::AtomicBool>,
}

/// A window's hold on the owner. While it exists the window counts as attached; dropping it, however it
/// comes to be dropped (the connection closing, the task being cancelled, an error), is the window going.
#[derive(Debug)]
pub struct Lease {
    attached: Arc<AtomicU32>,
}

impl Presence {
    /// A window has attached. The returned lease is the window being there.
    pub fn attach(&self) -> Lease {
        self.ever.store(true, Ordering::SeqCst);
        self.attached.fetch_add(1, Ordering::SeqCst);
        Lease {
            attached: self.attached.clone(),
        }
    }

    /// How many windows are attached now.
    pub fn windows(&self) -> u32 {
        self.attached.load(Ordering::SeqCst)
    }

    /// Whether any window has ever attached.
    pub fn ever_had_a_window(&self) -> bool {
        self.ever.load(Ordering::SeqCst)
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        // Never below zero, whatever the order of events: a lease is dropped once, but a count that went
        // under would read as a huge number of windows and keep an owner for ever.
        let _ = self
            .attached
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1));
    }
}

/// Whether the owner should end now.
///
/// * `started_for_a_window`: it carries the mark. One that does not is never ended here.
/// * `windows`: how many are attached now.
/// * `ever_had_a_window`: whether any has attached since the owner started.
/// * `waited`: how long the owner has been running.
/// * `runs_in_background`: the person's preference, read now, so that turning it on or off while a
///   window is open is honoured when the last window goes.
pub fn should_end(
    started_for_a_window: bool,
    windows: u32,
    ever_had_a_window: bool,
    waited: Duration,
    runs_in_background: bool,
) -> bool {
    if !started_for_a_window || runs_in_background || windows > 0 {
        return false;
    }
    // Nobody attached now. Having had one and lost it is the last window going; never having had one is the
    // window that started the owner dying on the way, which is given [`STARTUP_GRACE`] to appear.
    ever_had_a_window || waited >= STARTUP_GRACE
}

#[cfg(test)]
mod tests {
    use super::*;

    const LONG: Duration = Duration::from_secs(3600);
    const SOON: Duration = Duration::from_secs(2);

    #[test]
    fn an_owner_ends_when_the_last_window_goes_unless_the_person_wants_it_to_stay() {
        // A window was there and is gone, the background is off: the owner ends.
        assert!(should_end(true, 0, true, SOON, false));
        // Still a window: it does not, whatever the preference.
        assert!(!should_end(true, 1, true, LONG, false));
        assert!(!should_end(true, 3, true, LONG, false));
        // Gone, but the person asked for the background: it stays, as asked.
        assert!(!should_end(true, 0, true, LONG, true));
    }

    #[test]
    fn an_owner_that_was_not_started_for_a_window_is_never_ended_by_this() {
        // From the command line, as a service, by another program: nothing marked it.
        for windows in [0, 1, 5] {
            for ever in [true, false] {
                for waited in [SOON, LONG] {
                    for background in [true, false] {
                        assert!(
                            !should_end(false, windows, ever, waited, background),
                            "an owner nobody marked was ended (windows {windows}, ever {ever}, waited {waited:?}, background {background})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn an_owner_waits_for_its_first_window_for_a_while_and_then_gives_up() {
        // The window that started it has not attached yet: it is given time to.
        assert!(!should_end(true, 0, false, Duration::ZERO, false));
        assert!(!should_end(
            true,
            0,
            false,
            STARTUP_GRACE - Duration::from_millis(1),
            false
        ));
        // It never came: the owner ends at the end of the grace, not before.
        assert!(should_end(true, 0, false, STARTUP_GRACE, false));
        assert!(should_end(true, 0, false, LONG, false));
        // With the background on, it waits for ever, as asked.
        assert!(!should_end(true, 0, false, LONG, true));
    }

    #[test]
    fn turning_the_background_on_and_off_with_a_window_open_ends_the_owner_when_that_window_goes() {
        let presence = Presence::default();
        let lease = presence.attach();
        // On, then off, with the window open: nothing ends.
        assert!(!should_end(
            true,
            presence.windows(),
            presence.ever_had_a_window(),
            LONG,
            true
        ));
        assert!(!should_end(
            true,
            presence.windows(),
            presence.ever_had_a_window(),
            LONG,
            false
        ));
        // The window goes with the option off: the owner ends. Nothing was let go of for good.
        drop(lease);
        assert!(should_end(
            true,
            presence.windows(),
            presence.ever_had_a_window(),
            LONG,
            false
        ));
    }

    #[test]
    fn with_two_windows_the_owner_stays_until_the_second_goes() {
        let presence = Presence::default();
        let a = presence.attach();
        let b = presence.attach();
        assert_eq!(presence.windows(), 2);
        drop(a);
        // A went, B is open: the owner stays for B.
        assert!(!should_end(
            true,
            presence.windows(),
            presence.ever_had_a_window(),
            LONG,
            false
        ));
        drop(b);
        assert!(should_end(
            true,
            presence.windows(),
            presence.ever_had_a_window(),
            LONG,
            false
        ));
    }

    #[test]
    fn a_window_that_comes_back_after_all_went_is_counted_again() {
        let presence = Presence::default();
        drop(presence.attach());
        assert_eq!(presence.windows(), 0);
        assert!(presence.ever_had_a_window());
        let _again = presence.attach();
        assert_eq!(presence.windows(), 1);
        assert!(!should_end(
            true,
            presence.windows(),
            presence.ever_had_a_window(),
            LONG,
            false
        ));
    }

    #[test]
    fn the_count_of_windows_never_goes_below_zero() {
        let presence = Presence::default();
        // A lease made by hand from the same count and dropped twice stands for a double release.
        let lease = presence.attach();
        let twin = Lease {
            attached: lease.attached.clone(),
        };
        drop(lease);
        drop(twin);
        assert_eq!(
            presence.windows(),
            0,
            "the count went under zero and would read as many windows"
        );
        assert!(should_end(
            true,
            presence.windows(),
            presence.ever_had_a_window(),
            LONG,
            false
        ));
    }

    #[test]
    fn only_a_value_that_is_set_marks_an_owner_as_started_for_a_window() {
        assert!(marked(Some("1".as_ref())));
        assert!(marked(Some("anything".as_ref())));
        assert!(!marked(Some("".as_ref())));
        assert!(!marked(None));
    }
}
