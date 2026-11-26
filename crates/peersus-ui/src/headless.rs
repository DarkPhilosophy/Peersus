//! The read-only demo's window with no display behind it.
//!
//! The same root component the desktop program opens ([`crate::App`]), on the same demo
//! session ([`crate::link::Session::demo`]), run by the toolkit's own headless runner: it is
//! laid out, takes pointer and keyboard events, and is drawn by the CPU into memory. Nothing
//! is shown and no compositor, GPU or window system is needed, so it runs in a test and in CI.
//!
//! What it is evidence of, and what it is not:
//!
//! * It IS the real component tree, the real layout and the real drawing code, on the real
//!   font. A screen that panics, lays out to nothing or stops answering a press fails here.
//! * It is NOT the desktop window: no GPU surface, no window system, no real frame pacing, no
//!   platform dialog, no owner process. A frame time measured here is CPU layout and raster
//!   work on this fixture; it compares revisions and proves nothing about a device.
//!
//! Development only, behind the `headless` feature. The runner uses an in-memory clipboard,
//! independent of the desktop session.

use crate::{App, link::Session};
use freya::{engine::prelude::SkParagraph, prelude::*};
use freya_testing::{
    TestingNode, TestingRunner,
    prelude::{DragEventName, KeyboardEventName, PlatformEvent},
};
use std::{
    borrow::Cow,
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

/// The desktop window's opening size (see `window_config`).
pub const DESKTOP: (f32, f32) = (1100., 720.);
/// A phone held upright, in logical pixels: the narrow layout.
pub const NARROW: (f32, f32) = (412., 915.);
/// The narrowest phones it runs on, upright, in logical pixels (1116 pixels at a density of 533).
pub const PHONE: (f32, f32) = (335., 744.);

/// One headless demo window.
pub struct Window {
    // Declared first so that it is dropped first: the window's tasks end before the session's
    // executor they talk to.
    runner: TestingRunner,
    _session: Arc<Session>,
    /// Device pixels to each logical pixel: layout is in device pixels, and a pointer is placed in logical ones.
    scale: f32,
}

impl Window {
    /// Open the demo the same arguments would open on the desktop (see [`crate::demo`]), at
    /// `size` logical pixels. `Err` carries the sentence the program would print.
    pub fn open(args: &[String], size: (f32, f32)) -> Result<Self, String> {
        Self::open_scaled(args, size, 1.0)
    }

    /// [`Window::open`] drawn at `scale` device pixels to each logical pixel, as on a high-density screen: what a picture of
    /// the application shown on such a screen is made from.
    pub fn open_scaled(args: &[String], size: (f32, f32), scale: f64) -> Result<Self, String> {
        let spec = crate::demo::parse(args)?;
        let session = Session::demo(&spec).map_err(str::to_owned)?;
        Ok(Self::on_scaled(session, size, scale))
    }

    /// The window on `session`, whatever is behind it: the demo's fixed pictures, or, in a test, an owner that runs in
    /// the test's own process (see [`Session::paired_for_test`]).
    #[cfg(test)]
    pub(crate) fn on(session: Arc<Session>, size: (f32, f32)) -> Self {
        Self::on_scaled(session, size, 1.0)
    }

    fn on_scaled(session: Arc<Session>, size: (f32, f32), scale: f64) -> Self {
        let root = session.clone();
        let (mut runner, ()) = TestingRunner::new(
            move || App {
                session: root.clone(),
            },
            size.into(),
            |_| {
                GlobalContexts::get().insert_context(crate::edit_labels());
            },
            scale,
        );
        runner.set_fonts(HashMap::from([(crate::font::FAMILY, crate::font::BYTES)]));
        runner.set_default_fonts(&[Cow::Borrowed(crate::font::FAMILY)]);
        // Every animation ends on its next tick instead of running on the wall clock: a screen
        // is asserted and drawn as it comes to rest, and two runs draw the same picture.
        runner.animation_clock().disable();
        let mut window = Self {
            runner,
            _session: session,
            scale: scale as f32,
        };
        window.settle();
        window
    }

    /// Let what is pending happen: tasks the screens started, the events they led to, the
    /// layout that follows. A bounded number of turns, never "until quiet": a screen that
    /// animates forever must not hang a test.
    pub fn settle(&mut self) {
        self.runner.poll_n(Duration::from_millis(2), 12);
    }

    /// One frame's work, as the window does it for every frame: apply what changed, lay out,
    /// and draw into the in-memory surface.
    pub fn frame(&mut self) {
        self.runner.sync_and_update();
        self.runner.render_to_surface();
    }

    /// The current frame as PNG bytes.
    pub fn png(&mut self) -> Vec<u8> {
        self.runner.sync_and_update();
        self.runner.render().as_bytes().to_vec()
    }

    /// Write the current frame as a PNG file.
    pub fn save(&mut self, path: &Path) -> std::io::Result<()> {
        std::fs::write(path, self.png())
    }

    /// Every text on the screen that is laid out inside the window and not clipped away, in
    /// tree order: what a person could read, which is what a screen test should assert.
    pub fn texts(&self) -> Vec<String> {
        self.runner.find_many(|node, element| {
            let text = text_of!(element)?;
            (!text.is_empty() && node.is_visible()).then_some(text)
        })
    }

    /// Whether `text` is one of [`Window::texts`], exactly.
    pub fn shows(&self, text: &str) -> bool {
        self.texts().iter().any(|shown| shown == text)
    }
    /// Every visible text that is laid out with a line ending inside a word, with the two pieces it was broken into: what a
    /// box too narrow for its words looks like ("DISPOZITI" over "VE"). Read from the paragraph the toolkit laid out, so it is what
    /// is drawn and not an estimate. A line that ends at a space, a hyphen or any other punctuation is how text is meant to
    /// wrap and is not reported, nor is one between characters of a script written without spaces (Chinese, Japanese), which
    /// wraps between any two of them.
    pub fn broken_words(&self) -> Vec<String> {
        self.runner.find_many(|node, element| {
            let text = text_of!(element)?;
            if !node.is_visible() {
                return None;
            }
            let layout = node.layout();
            let paragraph = layout.data.as_ref()?.downcast_ref::<SkParagraph>()?;
            // The paragraph counts in UTF-16 units, as the engine does; a text is not sliced by them as bytes.
            let units: Vec<u16> = text.encode_utf16().collect();
            let piece = |from: usize, to: usize| {
                String::from_utf16_lossy(units.get(from..to.min(units.len())).unwrap_or_default())
            };
            // A letter or digit of a script that puts spaces between its words.
            let letter = |at: usize| {
                units
                    .get(at)
                    .and_then(|&unit| char::from_u32(u32::from(unit)))
                    .is_some_and(|c| c.is_alphanumeric() && !wraps_anywhere(c))
            };
            let lines = paragraph.get_line_metrics();
            lines.windows(2).find_map(|pair| {
                let (end, next) = (pair[0].end_index, pair[1].start_index);
                // The break took no white space with it, and both sides of it are letters or digits.
                let inside = !pair[0].hard_break
                    && pair[0].end_excluding_whitespaces == end
                    && end == next
                    && end > 0
                    && letter(end - 1)
                    && letter(next);
                inside.then(|| {
                    format!(
                        "{text:?} broken as {:?} | {:?}",
                        piece(pair[0].start_index, end),
                        piece(next, pair[1].end_index)
                    )
                })
            })
        })
    }

    /// Every visible text with any part of it right of the window's edge (`width` logical pixels across): what a row of cards wider than
    /// the window looks like, a card cut off at the side with the next one out of sight. Read from where the toolkit laid each text out.
    pub fn cut_off_at_the_side(&self, width: f32) -> Vec<String> {
        self.runner.find_many(|node, element| {
            let text = text_of!(element)?;
            (node.is_visible() && node.layout().area.max_x() > width + 0.5).then_some(text)
        })
    }

    /// Where the first visible text that reads exactly `text` is laid out, in logical pixels: `(left, top, right, bottom)`.
    /// `None` when no such text is on the screen. For a test to say that a name lies inside the window, and where.
    pub fn text_area(&self, text: &str) -> Option<(f32, f32, f32, f32)> {
        let node: TestingNode = self.runner.find(|node, element| {
            (text_of!(element).as_deref() == Some(text) && node.is_visible()).then_some(node)
        })?;
        let area = node.layout().area;
        Some((
            area.min_x() / self.scale,
            area.min_y() / self.scale,
            area.max_x() / self.scale,
            area.max_y() / self.scale,
        ))
    }

    /// Every visible text laid out narrower than the line it is (`max_intrinsic_width` of its paragraph): one that ends in an ellipsis
    /// or goes on under itself because its box is too narrow for the whole of it, as a name in a list as wide as its button is.
    pub fn narrower_than_itself(&self) -> Vec<String> {
        self.runner.find_many(|node, element| {
            let text = text_of!(element)?;
            if !node.is_visible() {
                return None;
            }
            let layout = node.layout();
            let paragraph = layout.data.as_ref()?.downcast_ref::<SkParagraph>()?;
            (paragraph.max_intrinsic_width() > layout.area.width() + 0.5).then_some(text)
        })
    }

    /// Press and release the primary button at a point of the window, in logical pixels (the runner takes device pixels, so a
    /// window drawn at more than one device pixel to a logical one is pressed where its person would point).
    pub fn click(&mut self, x: f64, y: f64) {
        let scale = f64::from(self.scale);
        self.runner.click_cursor((x * scale, y * scale));
        self.settle();
    }

    /// Click the middle of the first visible text that reads exactly `text`. `false` when no
    /// such text is on the screen, so that a test says which control it could not find.
    pub fn click_text(&mut self, text: &str) -> bool {
        let found: Option<TestingNode> = self.runner.find(|node, element| {
            (text_of!(element).as_deref() == Some(text) && node.is_visible()).then_some(node)
        });
        let Some(node) = found else {
            return false;
        };
        let middle = node.layout().area.center();
        self.click(
            f64::from(middle.x / self.scale),
            f64::from(middle.y / self.scale),
        );
        true
    }

    /// Turn the wheel by `(x, y)` with the pointer at a point of the window.
    pub fn scroll(&mut self, at: (f64, f64), by: (f64, f64)) {
        self.runner.scroll(at, by);
        self.settle();
    }

    /// Drop files on a point of the window, as a file manager does.
    pub fn drop_files(&mut self, at: (f64, f64), paths: Vec<PathBuf>) {
        let data = DragDataHandle::new(vec![DataFormat::Files]);
        let decision = DragDecision::default();
        self.runner.send_event(PlatformEvent::Drag {
            name: DragEventName::Over,
            cursor: at.into(),
            data: data.clone(),
            decision: Some(decision.clone()),
            target: None,
        });
        self.runner.sync_and_update();
        let Some((target, _)) = decision.accepted() else {
            return;
        };
        self.runner.send_event(PlatformEvent::Drag {
            name: DragEventName::Drop,
            cursor: at.into(),
            data: data.clone(),
            decision: None,
            target: Some(target),
        });
        self.settle();
        if let Some(request) = data
            .take_read_requests()
            .expect("test drag data remains available")
            .into_iter()
            .next()
        {
            let _ = request.response.send(Ok(DataValue::Files(paths)));
        }
        self.settle();
    }

    /// Type into whatever holds the keyboard.
    pub fn write(&mut self, text: &str) {
        self.runner.write_text(text);
        self.settle();
    }

    /// Press one key that is not a character (an arrow, Backspace) in whatever holds the keyboard.
    pub fn key(&mut self, key: NamedKey) {
        self.runner.press_key(Key::Named(key));
        self.settle();
    }

    /// Press and let go one key with `modifiers` held, as the platform reports it: the logical key, its place, the modifiers.
    pub fn chord(&mut self, key: Key, code: Code, modifiers: Modifiers) {
        for name in [KeyboardEventName::KeyDown, KeyboardEventName::KeyUp] {
            self.runner.send_event(PlatformEvent::Keyboard {
                name,
                key: key.clone(),
                code,
                modifiers,
            });
        }
        self.settle();
    }

    /// Press one key and keep it down, with `modifiers` held, as the platform reports it.
    pub fn key_down(&mut self, key: Key, code: Code, modifiers: Modifiers) {
        self.runner.send_event(PlatformEvent::Keyboard {
            name: KeyboardEventName::KeyDown,
            key,
            code,
            modifiers,
        });
        self.settle();
    }
}

/// The words of a text element, whichever of the toolkit's two kinds it is. A macro and not a
/// function because the trait object the runner hands over is not one the toolkit lets an
/// application name.
macro_rules! text_of {
    ($element:expr) => {
        match Label::try_downcast($element) {
            Some(label) => Some(label.text.into_owned()),
            None => Paragraph::try_downcast($element).map(|paragraph| {
                paragraph
                    .spans
                    .iter()
                    .map(|span| span.text.as_ref())
                    .collect::<String>()
            }),
        }
    };
}
use text_of;

/// A character of a script written without spaces between words, so that a line may end after any one of them: Han
/// ideographs, kana and the full-width forms set with them.
fn wraps_anywhere(c: char) -> bool {
    matches!(
        u32::from(c),
        0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF | 0x20000..=0x3134F
    )
}

#[cfg(test)]
mod tests;
