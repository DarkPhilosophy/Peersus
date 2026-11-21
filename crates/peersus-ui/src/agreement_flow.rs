//! The rules of the licence-and-notice overlay, with no toolkit: which step is shown, which buttons it has and which of them can be
//! pressed, how long the warning must be held, how big the panel is. The overlay only draws what this says.
//!
//! The overlay belongs to the application. An extension supplies the words and the pictures of its pages and nothing else: it cannot
//! size the panel, add, move, press or hide a button, or accept for the person. The panel's size, the place of `Cancel` at the far
//! left and of the forward button at the right, and the order of the steps are decided here, once, for every extension.
//!
//! A package that did not come through the store has one step more, BEFORE its pages: the application's own warning. It must be
//! read to its end, and then held; only then do the pages come, and only after the pages is `Accept TOS` there to press.

/// How long the warning's button must be held to confirm it, in milliseconds. Longer than a tap or a press that rests by accident,
/// and short enough that it is a decision and not a chore.
pub const RISK_HOLD_MS: u64 = 2_500;

// Far longer than a press that rests by accident (the circle of the Send page is held for 700 ms).
const _: () = assert!(RISK_HOLD_MS >= 2_000);

/// How near the end of the text counts as having read to the end, in pixels: a fraction of a line that cannot be scrolled to.
pub const END_SLACK: f32 = 4.;

/// The panel is never larger than this, whatever the window is, and never closer to the window's edges than [`MARGIN`].
pub const PANEL_MAX: (f32, f32) = (560., 680.);
/// The least the panel leaves between itself and the window's edges, in pixels.
pub const MARGIN: f32 = 16.;

/// The size of the panel for a window of `window` pixels: the one standard size, made smaller only so that it fits. Nothing an extension
/// says reaches this function.
#[must_use]
pub fn panel_size(window: (f32, f32)) -> (f32, f32) {
    // A window that has not been measured yet (not a number, or unbounded) has no room to give.
    let fit = |room: f32, most: f32| {
        if room.is_finite() {
            (room - MARGIN * 2.).clamp(0., most)
        } else {
            0.
        }
    };
    (fit(window.0, PANEL_MAX.0), fit(window.1, PANEL_MAX.1))
}

/// Whether the text has been read to its end: it fits without scrolling, or it has been scrolled to within [`END_SLACK`] of its bottom.
/// `offset` is how far down it is scrolled, `viewport` the height of what is seen and `content` the height of the whole text. Sizes that are
/// not numbers have not been measured yet, which is not the end.
#[must_use]
pub fn scrolled_to_end(offset: f32, viewport: f32, content: f32) -> bool {
    if !(offset.is_finite() && viewport.is_finite() && content.is_finite()) || viewport <= 0. {
        return false;
    }
    content <= viewport + END_SLACK || offset + viewport + END_SLACK >= content
}

/// How full the button is after being held for `held` milliseconds, from 0 to 1.
#[must_use]
pub fn hold_fill(held: u64) -> f32 {
    (held as f32 / RISK_HOLD_MS as f32).clamp(0., 1.)
}

/// Whether holding for `held` milliseconds confirms.
#[must_use]
pub fn hold_confirms(held: u64) -> bool {
    held >= RISK_HOLD_MS
}

/// Where the person is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// The application's warning about a package from outside the store.
    Warning,
    /// A page of the extension's agreement, from 0.
    Page(usize),
}

/// What the forward button at the right is, at this step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Forward {
    /// The warning's button, held to confirm. Pressable only once the whole warning has been read to its end.
    Hold { enabled: bool },
    /// On to the next page.
    Next,
    /// The last page's button: accepts the agreement, and so installs. There is no other way to accept.
    Accept,
    /// The last page of an agreement read again, after installation: nothing to accept, only `Close` at the left.
    None,
}

/// The buttons of a step. `Cancel` is always there, at the far left, and does nothing but give the package up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Buttons {
    /// Back to the page before, at the right beside the forward button. Never on the first thing the person sees.
    pub back: bool,
    pub forward: Forward,
}

/// What the person accepted, which is all the owner is told.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Accepted {
    /// The warning for a package from outside the store was read to its end and held.
    pub risk_acknowledged: bool,
}

/// The course of one agreement through the overlay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flow {
    pages: usize,
    outside_store: bool,
    warned: bool,
    page: usize,
    /// The agreement of an installed extension, read again: no warning, nothing to accept.
    review: bool,
}

impl Flow {
    /// A flow over an agreement of `pages` pages, for a package that did (`outside_store` false) or did not come through the store.
    /// There is no flow over an agreement with no page: there is nothing to accept, so nothing can be.
    #[must_use]
    pub fn new(pages: usize, outside_store: bool) -> Option<Self> {
        (pages > 0).then_some(Self {
            pages,
            outside_store,
            warned: false,
            page: 0,
            review: false,
        })
    }

    /// The agreement an installed extension was accepted with, read again: its pages only, and nothing to accept at the end.
    #[must_use]
    pub fn review(pages: usize) -> Option<Self> {
        Self::new(pages, false).map(|flow| Self {
            review: true,
            ..flow
        })
    }

    #[must_use]
    pub fn step(&self) -> Step {
        if self.outside_store && !self.warned {
            Step::Warning
        } else {
            Step::Page(self.page)
        }
    }

    /// Which step this is and how many there are, both counted from 1, the warning being the first for a package from outside the store.
    #[must_use]
    pub fn position(&self) -> (usize, usize) {
        let before = usize::from(self.outside_store);
        let total = self.pages + before;
        match self.step() {
            Step::Warning => (1, total),
            Step::Page(page) => (page + 1 + before, total),
        }
    }

    /// The buttons for this step. `read_to_end` is whether the text on show has been read to its end ([`scrolled_to_end`]); it matters
    /// only on the warning.
    #[must_use]
    pub fn buttons(&self, read_to_end: bool) -> Buttons {
        match self.step() {
            Step::Warning => Buttons {
                back: false,
                forward: Forward::Hold {
                    enabled: read_to_end,
                },
            },
            Step::Page(page) => Buttons {
                back: page > 0,
                forward: if page + 1 < self.pages {
                    Forward::Next
                } else if self.review {
                    Forward::None
                } else {
                    Forward::Accept
                },
            },
        }
    }

    /// The warning was held to the end. Only counts at the warning, and only once the text was read to its end: a hold on a button that is
    /// not pressable confirms nothing.
    pub fn confirm_warning(&mut self, read_to_end: bool) -> bool {
        if self.step() == Step::Warning && read_to_end {
            self.warned = true;
            self.page = 0;
            true
        } else {
            false
        }
    }

    /// On to the next page. Not from the warning, not past the last page.
    pub fn advance(&mut self) -> bool {
        if matches!(self.step(), Step::Page(page) if page + 1 < self.pages) {
            self.page += 1;
            true
        } else {
            false
        }
    }

    /// Back to the page before. Not before the first page, and never back to the warning once it was held.
    pub fn back(&mut self) -> bool {
        if matches!(self.step(), Step::Page(page) if page > 0) {
            self.page -= 1;
            true
        } else {
            false
        }
    }

    /// The person pressed `Accept TOS`. It exists only on the last page, after the warning when there is one; anywhere else it is nothing.
    #[must_use]
    pub fn accept(&self) -> Option<Accepted> {
        (self.buttons(false).forward == Forward::Accept && matches!(self.step(), Step::Page(_)))
            .then_some(Accepted {
                risk_acknowledged: self.outside_store && self.warned,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_panel_has_one_standard_size_and_only_shrinks_to_fit() {
        assert_eq!(panel_size((1600., 1000.)), PANEL_MAX);
        // A phone: the panel leaves its margin on every side, and is the standard size where the window allows it.
        assert_eq!(
            panel_size((400., 600.)),
            (400. - MARGIN * 2., 600. - MARGIN * 2.)
        );
        assert_eq!(panel_size((400., 1000.)), (400. - MARGIN * 2., PANEL_MAX.1));
        // A window smaller than the margins has no negative panel.
        assert_eq!(panel_size((10., 10.)), (0., 0.));
        for window in [(0., 0.), (f32::NAN, 500.), (500., f32::INFINITY)] {
            let (w, h) = panel_size(window);
            assert!(
                w.is_finite() && h.is_finite() && w >= 0. && h >= 0.,
                "{window:?}"
            );
            assert!(w <= PANEL_MAX.0 && h <= PANEL_MAX.1);
        }
    }

    #[test]
    fn text_is_read_to_its_end_when_it_fits_or_has_been_scrolled_to_the_bottom() {
        // Fits: nothing to scroll, so it has been seen whole.
        assert!(scrolled_to_end(0., 400., 300.));
        assert!(scrolled_to_end(0., 400., 400. + END_SLACK));
        // Longer: not at the top, not part way, yes at the bottom and within the slack of it.
        assert!(!scrolled_to_end(0., 400., 1000.));
        assert!(!scrolled_to_end(300., 400., 1000.));
        assert!(!scrolled_to_end(600. - 10., 400., 1000.));
        assert!(scrolled_to_end(600. - END_SLACK, 400., 1000.));
        assert!(scrolled_to_end(600., 400., 1000.));
        // Not measured yet is not the end.
        for (offset, viewport, content) in [
            (0., 0., 0.),
            (0., -1., 10.),
            (f32::NAN, 400., 1000.),
            (0., f32::NAN, 1000.),
            (0., 400., f32::NAN),
            (0., 400., f32::INFINITY),
        ] {
            assert!(
                !scrolled_to_end(offset, viewport, content),
                "{offset} {viewport} {content}"
            );
        }
    }

    #[test]
    fn the_warning_is_held_to_confirm_and_a_short_hold_is_nothing() {
        assert_eq!(hold_fill(0), 0.);
        assert_eq!(hold_fill(RISK_HOLD_MS / 2), 0.5);
        assert_eq!(hold_fill(RISK_HOLD_MS), 1.);
        assert_eq!(hold_fill(u64::MAX), 1.);
        assert!(!hold_confirms(0) && !hold_confirms(RISK_HOLD_MS - 1));
        assert!(hold_confirms(RISK_HOLD_MS) && hold_confirms(u64::MAX));
        // Far longer than a press that rests by accident: see the assertion beside the constant.
    }

    #[test]
    fn an_agreement_of_one_page_from_the_store_has_only_accept_at_the_right() {
        let mut flow = Flow::new(1, false).unwrap();
        assert_eq!(flow.step(), Step::Page(0));
        assert_eq!(flow.position(), (1, 1));
        assert_eq!(
            flow.buttons(false),
            Buttons {
                back: false,
                forward: Forward::Accept
            }
        );
        assert!(!flow.advance() && !flow.back());
        assert_eq!(
            flow.accept(),
            Some(Accepted {
                risk_acknowledged: false
            })
        );
    }

    #[test]
    fn several_pages_go_forward_and_back_and_only_the_last_can_be_accepted() {
        let mut flow = Flow::new(3, false).unwrap();
        assert_eq!(
            flow.buttons(true),
            Buttons {
                back: false,
                forward: Forward::Next
            }
        );
        assert_eq!(flow.accept(), None, "accepted on the first of three pages");
        assert!(flow.advance());
        assert_eq!(flow.position(), (2, 3));
        assert_eq!(
            flow.buttons(true),
            Buttons {
                back: true,
                forward: Forward::Next
            }
        );
        assert_eq!(flow.accept(), None);
        assert!(flow.back());
        assert!(!flow.back(), "back before the first page");
        assert!(flow.advance() && flow.advance());
        assert_eq!(flow.position(), (3, 3));
        assert_eq!(
            flow.buttons(true),
            Buttons {
                back: true,
                forward: Forward::Accept
            }
        );
        assert!(!flow.advance(), "past the last page");
        assert_eq!(
            flow.accept(),
            Some(Accepted {
                risk_acknowledged: false
            })
        );
    }

    #[test]
    fn an_agreement_read_again_has_its_pages_only_and_nothing_to_accept() {
        let mut flow = Flow::review(2).unwrap();
        assert_eq!(
            flow.step(),
            Step::Page(0),
            "no warning, whatever the package came from"
        );
        assert_eq!(flow.position(), (1, 2));
        assert_eq!(flow.buttons(false).forward, Forward::Next);
        assert!(flow.advance());
        assert_eq!(
            flow.buttons(true),
            Buttons {
                back: true,
                forward: Forward::None
            }
        );
        assert_eq!(
            flow.accept(),
            None,
            "an installed extension is not accepted again"
        );
        assert!(Flow::review(0).is_none());
    }

    #[test]
    fn a_package_from_outside_the_store_meets_the_warning_first_and_cannot_skip_it() {
        let mut flow = Flow::new(2, true).unwrap();
        assert_eq!(flow.step(), Step::Warning);
        assert_eq!(flow.position(), (1, 3));
        // Nothing but the held button, which cannot be pressed before the end of the text; no back, no next, no accept.
        assert_eq!(
            flow.buttons(false),
            Buttons {
                back: false,
                forward: Forward::Hold { enabled: false }
            }
        );
        assert_eq!(
            flow.buttons(true),
            Buttons {
                back: false,
                forward: Forward::Hold { enabled: true }
            }
        );
        assert!(!flow.advance() && !flow.back());
        assert_eq!(flow.accept(), None, "accepted without the warning");
        // A hold before the end of the text confirms nothing.
        assert!(!flow.confirm_warning(false));
        assert_eq!(flow.step(), Step::Warning);
        // At the end it does, and the pages follow, the warning counted as the first of three.
        assert!(flow.confirm_warning(true));
        assert_eq!(flow.step(), Step::Page(0));
        assert_eq!(flow.position(), (2, 3));
        // The warning is not confirmed twice and not gone back to.
        assert!(!flow.confirm_warning(true));
        assert!(!flow.back());
        assert!(flow.advance());
        assert_eq!(flow.position(), (3, 3));
        assert_eq!(
            flow.accept(),
            Some(Accepted {
                risk_acknowledged: true
            })
        );
    }

    #[test]
    fn there_is_no_flow_over_nothing_and_a_store_package_never_claims_the_warning() {
        assert_eq!(Flow::new(0, false), None);
        assert_eq!(Flow::new(0, true), None);
        let mut store = Flow::new(1, false).unwrap();
        assert!(
            !store.confirm_warning(true),
            "a package from the store has no warning to confirm"
        );
        assert_eq!(
            store.accept(),
            Some(Accepted {
                risk_acknowledged: false
            })
        );
    }
}
