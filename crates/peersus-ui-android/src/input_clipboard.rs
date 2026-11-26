//! The system clipboard for the toolkit's own text editing (copy, cut, paste in every input).
//!
//! Keep input edits on the bounded, plain-text foreground bridge. The toolkit's Android provider
//! does not impose those limits. This is separate from the extension clipboard: editing text is
//! an explicit action and needs none of the consent an extension needs to read in the background.
//!
//! What it never does: log or keep what it reads, read anything but one plain-text item, or read while the activity cannot
//! be asked (the answer is then an error, which the editor shows as nothing pasted).
use freya::clipboard::{
    ClipboardError, ClipboardFuture, ClipboardProvider, DataFormat, DataValue, TransferData,
};
use jni::objects::{JString, JValue};
use std::future::ready;

/// The largest text a paste or a copy moves. The same bound as the extension clipboard, so one limit protects both.
pub const MAX_BYTES: usize = crate::extension_clipboard::MAX_BYTES;

/// Why a clipboard operation did not happen. Never carries the text.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// The text is larger than [`MAX_BYTES`].
    TooLarge,
    /// The platform could not be asked.
    Unavailable,
}

/// What the platform said a paste found. `None` is an empty or non-text clipboard, which is nothing to paste, not an error.
fn bounded(found: Option<String>) -> Result<String, Refused> {
    match found {
        None => Ok(String::new()),
        Some(text) if text.len() > MAX_BYTES => Err(Refused::TooLarge),
        Some(text) => Ok(text),
    }
}

/// Text to be copied, refused before it reaches the platform when it is too large.
fn admitted(text: &str) -> Result<&str, Refused> {
    if text.len() > MAX_BYTES {
        Err(Refused::TooLarge)
    } else {
        Ok(text)
    }
}

/// The platform's side, behind a trait so that the policy above is tried without a phone.
trait Platform {
    fn read(&self) -> Result<Option<String>, Refused>;
    fn write(&self, text: &str) -> Result<(), Refused>;
}

struct Activity;
impl Platform for Activity {
    fn read(&self) -> Result<Option<String>, Refused> {
        crate::bridge::call_activity_string("inputClipboardRead").map_err(|_| Refused::Unavailable)
    }
    fn write(&self, text: &str) -> Result<(), Refused> {
        crate::bridge::with_activity(|env, activity| {
            let text = JString::from(env.new_string(text).map_err(|error| error.to_string())?);
            env.call_method(
                activity,
                "inputClipboardWrite",
                "(Ljava/lang/String;)V",
                &[JValue::Object(&text)],
            )
            .map(|_| ())
            .map_err(|error| {
                let _ = env.exception_clear();
                error.to_string()
            })
        })
        .map_err(|_| Refused::Unavailable)
    }
}

/// The policy over any platform: what a paste may return and what a copy may send. Private; tried without a phone.
struct Provider<P: Platform>(P);

impl<P: Platform> Provider<P> {
    fn read_text(&self) -> Result<String, Refused> {
        bounded(self.0.read()?)
    }

    fn write_text(&self, contents: &str) -> Result<(), Refused> {
        self.0.write(admitted(contents)?)
    }
}

impl<P: Platform> ClipboardProvider for Provider<P> {
    fn get(&self, format: DataFormat) -> ClipboardFuture<DataValue> {
        let result = if format == DataFormat::Text {
            self.read_text()
                .map(DataValue::Text)
                .map_err(|_| ClipboardError::FailedToRead)
        } else {
            Err(ClipboardError::UnsupportedFormat)
        };
        Box::pin(ready(result))
    }

    fn set(&self, data: TransferData) -> ClipboardFuture<()> {
        let result = match data.formats.as_slice() {
            [(DataFormat::Text, DataValue::Text(text))] => self
                .write_text(text)
                .map_err(|_| ClipboardError::FailedToSet),
            _ => Err(ClipboardError::UnsupportedFormat),
        };
        Box::pin(ready(result))
    }
}

pub(super) fn provider() -> impl ClipboardProvider {
    Provider(Activity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Phone {
        clip: RefCell<Option<String>>,
        fail: bool,
    }
    impl Platform for Phone {
        fn read(&self) -> Result<Option<String>, Refused> {
            if self.fail {
                Err(Refused::Unavailable)
            } else {
                Ok(self.clip.borrow().clone())
            }
        }
        fn write(&self, text: &str) -> Result<(), Refused> {
            if self.fail {
                return Err(Refused::Unavailable);
            }
            self.clip.replace(Some(text.to_owned()));
            Ok(())
        }
    }

    #[test]
    fn a_copy_comes_back_whole_as_a_paste_including_text_beyond_ascii() {
        let clipboard = Provider(Phone::default());
        clipboard.write_text("é🙂 λ🌍").unwrap();
        assert_eq!(clipboard.read_text().unwrap(), "é🙂 λ🌍");
    }

    #[test]
    fn an_empty_or_non_text_clipboard_pastes_nothing_and_is_not_an_error() {
        let clipboard = Provider(Phone::default());
        assert_eq!(clipboard.read_text().unwrap(), "");
    }

    #[test]
    fn a_paste_larger_than_the_bound_is_refused_not_cut() {
        let clipboard = Provider(Phone {
            clip: RefCell::new(Some("a".repeat(MAX_BYTES + 1))),
            ..Phone::default()
        });
        assert_eq!(clipboard.read_text(), Err(Refused::TooLarge));
    }

    #[test]
    fn a_copy_larger_than_the_bound_never_reaches_the_platform() {
        let clipboard = Provider(Phone {
            clip: RefCell::new(Some("kept".to_owned())),
            ..Phone::default()
        });
        assert_eq!(
            clipboard.write_text(&"a".repeat(MAX_BYTES + 1)),
            Err(Refused::TooLarge)
        );
        assert_eq!(clipboard.read_text().unwrap(), "kept");
    }

    #[test]
    fn a_text_exactly_at_the_bound_is_allowed_both_ways() {
        let clipboard = Provider(Phone::default());
        let text = "a".repeat(MAX_BYTES);
        clipboard.write_text(&text).unwrap();
        assert_eq!(clipboard.read_text().unwrap(), text);
    }

    #[test]
    fn an_unavailable_platform_is_an_error_so_that_nothing_is_pasted_silently() {
        let clipboard = Provider(Phone {
            fail: true,
            ..Phone::default()
        });
        assert_eq!(clipboard.read_text(), Err(Refused::Unavailable));
        assert_eq!(clipboard.write_text("x"), Err(Refused::Unavailable));
    }
}
