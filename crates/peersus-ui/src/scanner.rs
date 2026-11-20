//! What becomes of the frames of a camera, until an invitation is read in one of them.
//!
//! The camera belongs to the platform (on Android, Camera2, started from Java) and it gives frames at its own pace, on
//! a thread that is not the interface's, thirty or more in a second. Reading a code in a frame is costly, and one that
//! has been read is read for good, so what is decided here is: which frames are looked at, when the scan is over, and
//! what is passed on. The layer that talks to the platform only passes frames in; nothing here knows what a camera is.
//!
//! WHAT IS DECIDED
//! * **Pace.** A frame is looked at only if `LOOK_EVERY` has passed since the last one that was: a phone that gives
//!   thirty frames a second is not asked to read thirty codes a second, which would keep its processor busy for nothing.
//! * **The first invitation ends the scan.** What was read is kept once and the scan is over: later frames are not
//!   looked at, so that a camera that stays on the code does not make the same join many times.
//! * **Only an invitation.** Any other code in view (a link, a ticket, a poster) is passed over by
//!   [`crate::qr::invitation_in_frame`], so that a camera pointed at the wrong thing joins nothing.
//! * **Stopped from outside.** A scan that the person left (backed out, or switched page) takes no more frames, and
//!   what it would have found is not passed on.

use crate::qr;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// How often a frame is looked at. A code that is held in front of the camera is read within a tenth of a second at
/// this pace, which a person does not notice, and a processor is not kept busy reading the same view thirty times.
pub const LOOK_EVERY: Duration = Duration::from_millis(100);

/// A scan in progress. Cheap to clone: the clones share the one scan, so that the thread that gives frames and the
/// interface that waits for the answer see the same state.
#[derive(Clone, Default)]
pub struct Scanner {
    inner: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    /// The last time a frame was looked at, to keep the pace.
    last_look: Option<Instant>,
    /// The invitation that was read, until the interface takes it.
    found: Option<String>,
    /// The scan is over: an invitation was read, or the person left.
    over: bool,
    /// How many frames were really looked at, for the one who wants to know the pace was kept.
    looked_at: u32,
    /// The latest frame the camera gave, kept to be shown; only ever the one, replaced by the next.
    frame: Option<Arc<Frame>>,
    /// Where codes were seen in the frame of the last look, in that frame's pixels.
    corners: Vec<qr::Corners>,
    /// Which frame is the latest, counted up with each, so that what was shown already is not shown again.
    serial: u64,
}

/// What a frame did to the scan.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Not looked at: it came too soon after the last one, or the scan is over.
    Skipped,
    /// Looked at, and there is no invitation in it.
    Nothing,
    /// Looked at, and this is the invitation in it. The scan is over.
    Found(String),
}

impl Scanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// A frame from the camera: `luma` is its brightness plane, `width` by `height`, one byte for a pixel. `now` is the
    /// time of the frame, given and not read here, so that the pace is tried without waiting.
    pub fn frame(&self, luma: &[u8], width: usize, height: usize, now: Instant) -> Outcome {
        self.frame_coloured(luma, width, height, None, now)
    }

    /// [`frame`](Self::frame) with the colour of the frame's centre square, when the camera gave it.
    pub fn frame_coloured(
        &self,
        luma: &[u8],
        width: usize,
        height: usize,
        colour: Option<Colour>,
        now: Instant,
    ) -> Outcome {
        // Every frame is kept to be shown, and only some are looked at: a frame that is not looked at leaves the
        // corners of the last look as they were.
        let shown = (width.checked_mul(height) == Some(luma.len()) && width > 0 && height > 0)
            .then(|| {
                Arc::new(Frame {
                    luma: luma.to_vec(),
                    width,
                    height,
                    colour,
                })
            });
        if !self.begin(now) {
            if let Some(frame) = shown {
                self.show(frame, None);
            }
            return Outcome::Skipped;
        }
        // Read with no lock held: reading a code costs, and the interface must not wait on it to ask a question.
        let seen = qr::look_in_frame(luma, width, height);
        if let Some(frame) = shown {
            self.show(frame, Some(seen.corners));
        }
        self.finish(seen.invitation)
    }

    /// Whether a frame at `now` is to be looked at: not if the scan is over, and not if it came too soon after the
    /// last one that was. When it is, it is counted and the time is kept.
    pub fn begin(&self, now: Instant) -> bool {
        let Ok(mut state) = self.inner.lock() else {
            return false;
        };
        if state.over {
            return false;
        }
        if state
            .last_look
            .is_some_and(|last| now.saturating_duration_since(last) < LOOK_EVERY)
        {
            return false;
        }
        state.last_look = Some(now);
        state.looked_at = state.looked_at.saturating_add(1);
        true
    }

    /// What a frame that was looked at means, given what was read in it. The person may have left while it was being
    /// read, on another thread: what it found is then not passed on.
    pub fn finish(&self, read: Option<String>) -> Outcome {
        let Ok(mut state) = self.inner.lock() else {
            return Outcome::Skipped;
        };
        if state.over {
            return Outcome::Skipped;
        }
        match read {
            Some(invitation) => {
                state.over = true;
                state.found = Some(invitation.clone());
                Outcome::Found(invitation)
            }
            None => Outcome::Nothing,
        }
    }

    /// The invitation that was read, taken once.
    pub fn take(&self) -> Option<String> {
        self.inner.lock().ok()?.found.take()
    }

    /// Whether the scan is over, by an invitation or by the person leaving.
    pub fn is_over(&self) -> bool {
        self.inner.lock().map_or(true, |state| state.over)
    }

    /// The person left: no more frames are looked at, and nothing is passed on.
    pub fn stop(&self) {
        if let Ok(mut state) = self.inner.lock() {
            state.over = true;
            state.found = None;
        }
    }

    /// How many frames were really looked at.
    pub fn looked_at(&self) -> u32 {
        self.inner.lock().map_or(0, |state| state.looked_at)
    }

    /// Keep the latest frame for the person to see, and say where codes were seen in it. Called for every frame the
    /// camera gives, since a picture that moved a few times a second would be no use for aiming; only the `Arc` is
    /// kept under the lock, never a copy of the frame. `corners` is what the last look found, and `None` leaves what
    /// the last look found as it was: a frame that was not looked at is not evidence that the code went away.
    pub fn show(&self, frame: Arc<Frame>, corners: Option<Vec<qr::Corners>>) {
        let Ok(mut state) = self.inner.lock() else {
            return;
        };
        if state.over {
            return;
        }
        state.serial = state.serial.wrapping_add(1);
        if let Some(corners) = corners {
            state.corners = corners;
        }
        state.frame = Some(frame);
    }

    /// What there is to show, if it is newer than `after` (the serial of what was last shown): the frame, the corners
    /// seen, and the serial to pass next time. `None` when nothing new has come, so that the same picture is not made
    /// and handed over again and again, and when the scan is over, since a picture of a camera that is shut is wrong.
    pub fn view_after(&self, after: u64) -> Option<View> {
        let state = self.inner.lock().ok()?;
        if state.over || state.serial == after {
            return None;
        }
        Some(View {
            frame: state.frame.clone()?,
            corners: state.corners.clone(),
            serial: state.serial,
        })
    }
}

/// One frame of the camera as it came: its brightness plane, one byte to a pixel, `width` bytes to a row, and, when the
/// camera gave them, the colour of its centre square.
#[derive(Debug)]
pub struct Frame {
    pub luma: Vec<u8>,
    pub width: usize,
    pub height: usize,
    pub colour: Option<Colour>,
}

/// The two colour planes (blue and red difference, 128 for none) of the centre square of a frame, a quarter of its pixels:
/// `side` is half the side of that square, and each plane has `side` bytes to a row and `side` rows. Only the centre square
/// is carried because it is all the circle shows, and a camera's colour planes already are at half the resolution of its
/// brightness one, so that this is a quarter of a quarter of the frame to copy thirty times a second.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Colour {
    pub side: usize,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

impl Colour {
    /// Whether these planes are those of a centre square `edge` pixels across: anything else is not used, so that a
    /// plane that does not fit shows grey and never a smeared colour.
    pub fn fits(&self, edge: usize) -> bool {
        let needed = self.side.checked_mul(self.side);
        self.side > 0
            && self.side == edge / 2
            && needed.is_some_and(|needed| self.u.len() >= needed && self.v.len() >= needed)
    }

    /// The colour (BT.601, full range, as a camera's own conversion) of brightness `y` at `column`, `row` of the centre
    /// square, which this must [`fit`](Self::fit).
    pub fn pixel(&self, y: u8, column: usize, row: usize) -> [u8; 3] {
        let at = (row / 2).min(self.side - 1) * self.side + (column / 2).min(self.side - 1);
        let (u, v) = (i32::from(self.u[at]) - 128, i32::from(self.v[at]) - 128);
        let y = i32::from(y);
        let channel = |value: i32| value.clamp(0, 255) as u8;
        [
            channel(y + (1436 * v) / 1024),
            channel(y - (352 * u + 731 * v) / 1024),
            channel(y + (1815 * u) / 1024),
        ]
    }
}

/// What the person is shown of the scan: the latest frame, where codes were seen in it, and which one it is.
#[derive(Debug, Clone)]
pub struct View {
    pub frame: Arc<Frame>,
    pub corners: Vec<qr::Corners>,
    pub serial: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The brightness plane of the code of `text`, as a camera would give it.
    fn frame_of(text: &str) -> (Vec<u8>, usize) {
        let raster = qr::raster(&qr::for_code(text)).expect("a code that can be drawn");
        let side = raster.side as usize;
        (
            (0..side * side)
                .map(|index| raster.rgba[index * 4])
                .collect(),
            side,
        )
    }

    fn invitation() -> String {
        format!("peersus:1:{}", "7b226164647265737322".repeat(28))
    }

    #[test]
    fn the_first_invitation_in_view_ends_the_scan_and_later_frames_are_not_looked_at() {
        let scanner = Scanner::new();
        let (frame, side) = frame_of(&invitation());
        let start = Instant::now();
        assert_eq!(
            scanner.frame(&frame, side, side, start),
            Outcome::Found(invitation())
        );
        assert!(scanner.is_over());
        // A camera that stays on the code: the frames that follow are not looked at, and nothing is joined twice.
        for n in 1..=20 {
            let later = start + LOOK_EVERY * n;
            assert_eq!(scanner.frame(&frame, side, side, later), Outcome::Skipped);
        }
        assert_eq!(
            scanner.looked_at(),
            1,
            "a frame was looked at after the scan was over"
        );
        assert_eq!(scanner.take(), Some(invitation()));
        assert_eq!(scanner.take(), None, "the invitation was handed out twice");
    }

    #[test]
    fn frames_that_come_faster_than_the_pace_are_skipped_and_the_ones_at_the_pace_are_looked_at() {
        let scanner = Scanner::new();
        let blank = vec![128u8; 64 * 64];
        let start = Instant::now();
        assert_eq!(scanner.frame(&blank, 64, 64, start), Outcome::Nothing);
        // Thirty frames a second: the next one at 33 ms is too soon.
        assert_eq!(
            scanner.frame(&blank, 64, 64, start + Duration::from_millis(33)),
            Outcome::Skipped
        );
        assert_eq!(
            scanner.frame(&blank, 64, 64, start + Duration::from_millis(66)),
            Outcome::Skipped
        );
        // At the pace it is looked at again.
        assert_eq!(
            scanner.frame(&blank, 64, 64, start + LOOK_EVERY),
            Outcome::Nothing
        );
        assert_eq!(scanner.looked_at(), 2);
        // A clock that goes back (a frame stamped earlier) is not a reason to read: it is skipped, not a panic.
        assert_eq!(scanner.frame(&blank, 64, 64, start), Outcome::Skipped);
    }

    #[test]
    fn a_code_that_is_not_an_invitation_is_passed_over_and_the_scan_goes_on() {
        let scanner = Scanner::new();
        let start = Instant::now();
        let (link, side) = frame_of("https://example.com/pay?to=someone");
        assert_eq!(scanner.frame(&link, side, side, start), Outcome::Nothing);
        assert!(!scanner.is_over(), "a link ended the scan");
        // And the real invitation, a moment later, is still found.
        let (frame, side) = frame_of(&invitation());
        assert_eq!(
            scanner.frame(&frame, side, side, start + LOOK_EVERY),
            Outcome::Found(invitation())
        );
    }

    #[test]
    fn a_scan_that_the_person_left_takes_no_more_frames_and_hands_nothing_on() {
        let scanner = Scanner::new();
        let (frame, side) = frame_of(&invitation());
        let start = Instant::now();
        scanner.stop();
        assert!(scanner.is_over());
        assert_eq!(
            scanner.frame(&frame, side, side, start),
            Outcome::Skipped,
            "a frame was read after the person left"
        );
        assert_eq!(scanner.take(), None);
        assert_eq!(scanner.looked_at(), 0);
    }

    #[test]
    fn leaving_after_an_invitation_was_read_withdraws_it_so_that_it_is_not_joined_late() {
        let scanner = Scanner::new();
        let (frame, side) = frame_of(&invitation());
        assert!(matches!(
            scanner.frame(&frame, side, side, Instant::now()),
            Outcome::Found(_)
        ));
        scanner.stop();
        assert_eq!(
            scanner.take(),
            None,
            "an invitation was joined after the person had left"
        );
    }

    #[test]
    fn the_clones_are_one_scan() {
        let camera = Scanner::new();
        let interface = camera.clone();
        let (frame, side) = frame_of(&invitation());
        camera.frame(&frame, side, side, Instant::now());
        assert!(
            interface.is_over(),
            "the interface did not see the scan end"
        );
        assert_eq!(interface.take(), Some(invitation()));
    }

    #[test]
    fn a_frame_that_finishes_being_read_after_the_person_left_is_not_passed_on() {
        let scanner = Scanner::new();
        // The camera's thread starts to read a frame that holds the invitation...
        assert!(scanner.begin(Instant::now()));
        // ...and while it reads, on the interface's thread, the person leaves.
        scanner.stop();
        // The reading ends with the invitation found: it must not become a join the person did not ask for.
        assert_eq!(scanner.finish(Some(invitation())), Outcome::Skipped);
        assert_eq!(
            scanner.take(),
            None,
            "an invitation read after the person left was handed on"
        );
        assert!(scanner.is_over());
    }

    #[test]
    fn what_was_read_is_judged_only_for_a_frame_that_was_looked_at_and_only_once() {
        let scanner = Scanner::new();
        let start = Instant::now();
        assert!(scanner.begin(start));
        // Nothing in the frame: the scan goes on, and a second frame at the pace is looked at.
        assert_eq!(scanner.finish(None), Outcome::Nothing);
        assert!(!scanner.is_over());
        assert!(scanner.begin(start + LOOK_EVERY));
        assert_eq!(
            scanner.finish(Some(invitation())),
            Outcome::Found(invitation())
        );
        // Over: a frame is not even begun, and a second finding is not accepted.
        assert!(!scanner.begin(start + LOOK_EVERY * 5));
        assert_eq!(
            scanner.finish(Some("peersus:1:ff".to_owned())),
            Outcome::Skipped
        );
        assert_eq!(
            scanner.take(),
            Some(invitation()),
            "the second finding replaced the first"
        );
    }

    fn blank(side: usize) -> Vec<u8> {
        vec![128; side * side]
    }

    #[test]
    fn every_frame_is_kept_to_be_shown_but_only_a_newer_one_than_the_last_shown_is_given() {
        let scanner = Scanner::new();
        let start = Instant::now();
        // Nothing yet: nothing to show.
        assert!(scanner.view_after(0).is_none());
        // A frame comes, looked at or not, and is shown once; asked again with its serial there is nothing new.
        scanner.frame(&blank(32), 32, 32, start);
        let first = scanner
            .view_after(0)
            .expect("a frame came and none was to be shown");
        assert_eq!((first.frame.width, first.frame.height), (32, 32));
        assert!(
            scanner.view_after(first.serial).is_none(),
            "the same frame was given twice"
        );
        // A frame too soon to be looked at is still shown: the picture must move for the person to aim by it.
        scanner.frame(&blank(32), 32, 32, start + Duration::from_millis(10));
        let second = scanner
            .view_after(first.serial)
            .expect("a frame that was not looked at was not shown");
        assert!(second.serial != first.serial);
    }

    #[test]
    fn the_corners_of_the_last_look_stay_until_the_next_look_and_are_cleared_by_one_that_finds_nothing()
     {
        let scanner = Scanner::new();
        let start = Instant::now();
        let corners = vec![[(1, 1), (9, 1), (9, 9), (1, 9)]];
        // A look that found a code, as the camera's thread would have recorded it.
        scanner.show(
            Arc::new(Frame {
                luma: blank(16),
                width: 16,
                height: 16,
                colour: None,
            }),
            Some(corners.clone()),
        );
        assert_eq!(scanner.view_after(0).unwrap().corners, corners);
        // A frame that was not looked at leaves them: it is no evidence the code went away.
        scanner.show(
            Arc::new(Frame {
                luma: blank(16),
                width: 16,
                height: 16,
                colour: None,
            }),
            None,
        );
        assert_eq!(
            scanner.view_after(1).unwrap().corners,
            corners,
            "the corners were lost by a frame not looked at"
        );
        // A real look that finds nothing clears them, or a mark would stay on something that is no longer there.
        scanner.frame(&blank(32), 32, 32, start);
        assert!(
            scanner.view_after(2).unwrap().corners.is_empty(),
            "a mark was left where nothing is seen"
        );
    }

    #[test]
    fn nothing_is_shown_once_the_scan_is_over_and_a_frame_that_is_not_what_it_says_is_never_shown()
    {
        let scanner = Scanner::new();
        let start = Instant::now();
        // A frame that does not hold what it says is looked at as nothing and kept as nothing.
        scanner.frame(&[1, 2, 3], 4, 4, start);
        assert!(scanner.view_after(0).is_none(), "a short frame was shown");
        scanner.frame(&[], 0, 0, start + Duration::from_millis(200));
        assert!(scanner.view_after(0).is_none(), "an empty frame was shown");
        // A good one is shown; the person leaves; nothing is, and a late frame is not kept.
        scanner.frame(&blank(32), 32, 32, start + Duration::from_millis(400));
        assert!(scanner.view_after(0).is_some());
        scanner.stop();
        assert!(
            scanner.view_after(0).is_none(),
            "a picture was shown for a scan that is over"
        );
        scanner.show(
            Arc::new(Frame {
                luma: blank(16),
                width: 16,
                height: 16,
                colour: None,
            }),
            None,
        );
        assert!(
            scanner.view_after(0).is_none(),
            "a late frame was kept after the scan was over"
        );
    }
}
