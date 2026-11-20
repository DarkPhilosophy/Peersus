//! The system clipboard for the toolkit's own text editing (copy, cut, paste in every input).
//!
//! The toolkit's default clipboard on Android is a no-op: it reads an empty string and writes nothing, so paste and copy in every
//! input silently did nothing. This module is the real one. It is separate from the extension clipboard on purpose: editing text
//! the person typed is an explicit, foreground action of that person, and needs none of the consent an extension needs to read a
//! clipboard in the background.
//!
//! What it never does: log or keep what it reads, read anything but one plain-text item, or read while the activity cannot
//! be asked (the answer is then an error, which the editor shows as nothing pasted).
#![allow(unsafe_code)]
use copypasta::ClipboardProvider;
use jni::objects::{JString, JValue};
use std::fmt;

/// The result type of the toolkit's clipboard provider trait, which the crate does not export under a name.
type ClipboardResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync + 'static>>;

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

impl fmt::Display for Refused {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TooLarge => "the text is too large for the clipboard",
            Self::Unavailable => "the clipboard is unavailable",
        })
    }
}
impl std::error::Error for Refused {}

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
trait Platform: Send {
    fn read(&mut self) -> Result<Option<String>, Refused>;
    fn write(&mut self, text: &str) -> Result<(), Refused>;
}

struct Activity;
impl Platform for Activity {
    fn read(&mut self) -> Result<Option<String>, Refused> {
        crate::bridge::call_activity_string("inputClipboardRead").map_err(|_| Refused::Unavailable)
    }
    fn write(&mut self, text: &str) -> Result<(), Refused> {
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

impl<P: Platform> ClipboardProvider for Provider<P> {
    fn get_contents(&mut self) -> ClipboardResult<String> {
        let found = self.0.read()?;
        Ok(bounded(found)?)
    }
    fn set_contents(&mut self, contents: String) -> ClipboardResult<()> {
        self.0.write(admitted(&contents)?)?;
        Ok(())
    }
}

/// The toolkit's clipboard provider over the Android clipboard.
pub struct InputClipboard(Provider<Activity>);

impl InputClipboard {
    pub fn new() -> Self {
        Self(Provider(Activity))
    }
}
impl Default for InputClipboard {
    fn default() -> Self {
        Self::new()
    }
}
impl ClipboardProvider for InputClipboard {
    fn get_contents(&mut self) -> ClipboardResult<String> {
        self.0.get_contents()
    }
    fn set_contents(&mut self, contents: String) -> ClipboardResult<()> {
        self.0.set_contents(contents)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Phone {
        clip: Option<String>,
        asked: bool,
        fail: bool,
    }
    impl Platform for Phone {
        fn read(&mut self) -> Result<Option<String>, Refused> {
            self.asked = true;
            if self.fail {
                Err(Refused::Unavailable)
            } else {
                Ok(self.clip.clone())
            }
        }
        fn write(&mut self, text: &str) -> Result<(), Refused> {
            if self.fail {
                return Err(Refused::Unavailable);
            }
            self.clip = Some(text.to_owned());
            Ok(())
        }
    }

    #[test]
    fn a_copy_comes_back_whole_as_a_paste_including_text_beyond_ascii() {
        let mut clipboard = Provider(Phone::default());
        clipboard.set_contents("é🙂 λ🌍".to_owned()).unwrap();
        assert_eq!(clipboard.get_contents().unwrap(), "é🙂 λ🌍");
    }

    #[test]
    fn an_empty_or_non_text_clipboard_pastes_nothing_and_is_not_an_error() {
        let mut clipboard = Provider(Phone::default());
        assert_eq!(clipboard.get_contents().unwrap(), "");
    }

    #[test]
    fn a_paste_larger_than_the_bound_is_refused_not_cut() {
        let mut clipboard = Provider(Phone {
            clip: Some("a".repeat(MAX_BYTES + 1)),
            ..Phone::default()
        });
        let error = clipboard.get_contents().unwrap_err();
        assert_eq!(error.to_string(), Refused::TooLarge.to_string());
    }

    #[test]
    fn a_copy_larger_than_the_bound_never_reaches_the_platform() {
        let mut phone = Phone::default();
        phone.clip = Some("kept".to_owned());
        let mut clipboard = Provider(phone);
        assert!(clipboard.set_contents("a".repeat(MAX_BYTES + 1)).is_err());
        assert_eq!(clipboard.get_contents().unwrap(), "kept");
    }

    #[test]
    fn a_text_exactly_at_the_bound_is_allowed_both_ways() {
        let mut clipboard = Provider(Phone::default());
        let text = "a".repeat(MAX_BYTES);
        clipboard.set_contents(text.clone()).unwrap();
        assert_eq!(clipboard.get_contents().unwrap(), text);
    }

    #[test]
    fn an_unavailable_platform_is_an_error_so_that_nothing_is_pasted_silently() {
        let mut clipboard = Provider(Phone {
            fail: true,
            ..Phone::default()
        });
        assert!(clipboard.get_contents().is_err());
        assert!(clipboard.set_contents("x".to_owned()).is_err());
    }
}
