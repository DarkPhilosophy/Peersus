//! How a text is made to fit the box it is drawn in.
//!
//! A window is as wide as the screen divided by the size the person chose for the interface, in any of twelve languages, so no
//! text has a width that can be known where it is written. What a text does when its box is narrower than it would like is
//! therefore decided here, once, by what kind of text it is, and never by a size written for one screen at one scale:
//!
//! * **Running text** (a sentence, a note) wraps at its spaces. That is what a plain `label()` does, and nothing is needed.
//! * **A caption of a control** (a button, a chip, a value at the end of a row) is one line and is never broken inside a word:
//!   [`caption`]. Controls that stand side by side are put in a row that wraps (`Content::wrap_spacing`), so that the one that
//!   no longer fits goes under the others whole, instead of being squeezed into what is left.
//! * **A text in a box of a fixed share of the window** (a place in the bottom bar, a title) is made as large as fits, down to a
//!   size below which it is cut with an ellipsis: [`Fitted`], sized by the rule [`crate::view::Fit`], which knows no toolkit and
//!   is tested on its own.
//!
//! Every size here is in logical pixels, which is what makes one rule do for a resized window, a small phone and a larger
//! interface alike: a larger interface is the same window with fewer logical pixels across.
//!
//! `headless::tests::no_text_is_broken_inside_a_word_at_any_size_in_any_language` lays every screen out at the narrowest widths
//! in every language and fails on a word broken over two lines, which is what a text that was given none of these looks like.
use std::borrow::Cow;

use freya::prelude::*;

use crate::view::Fit;

/// The caption of a control: one line, whole. It is as wide as its words, so a row that wraps moves the control and does not
/// squeeze it.
pub fn caption(text: impl Into<Cow<'static, str>>) -> Label {
    label().text(text).max_lines(1)
}

/// One line of text in a box it does not choose the width of: as large as fits, and cut with an ellipsis only when it does not
/// fit at its smallest. It measures the room it is given; until that is known it is at its smallest, and the first
/// measurement, a frame later, puts it right.
#[derive(PartialEq)]
pub struct Fitted {
    pub text: Cow<'static, str>,
    pub fit: Fit,
    pub colour: (u8, u8, u8),
    /// The face, when it is not the interface's own.
    pub family: Option<&'static str>,
}

impl Component for Fitted {
    fn render(&self) -> impl IntoElement {
        let mut room = use_state(|| 0.0_f32);
        let size = self.fit.size_in(room(), self.text.chars().count());
        let mut words = label()
            .text(self.text.clone())
            .color(self.colour)
            .font_size(size)
            .width(Size::fill())
            .text_align(TextAlign::Center)
            .max_lines(1)
            .text_overflow(TextOverflow::Ellipsis);
        if let Some(family) = self.family {
            words = words.font_family(family);
        }
        rect()
            .width(Size::fill())
            .on_sized(move |e: Event<SizedEventData>| room.set_if_modified(e.area.width()))
            .child(words)
    }
}
