//! The rules for moving clipboard text between a guest and the platform, written once for every platform.
//!
//! A guest never holds text: it holds a short-lived handle, owned by the extension that made it, that it reads in
//! bounded pieces, or builds in bounded pieces and commits whole. What differs between platforms is only the four
//! things in [`Board`]; every rule about handles, bounds, expiry and ownership is here, so a desktop and a phone
//! cannot drift apart on the part that guards the person's clipboard.
//!
//! Nothing here names an extension. Identity arrives already stamped by the application and is only compared.
use crate::platform_services::{CLIPBOARD_CHUNK_BYTES, MAX_CLIPBOARD_BYTES};
use peersus_core::plugins::PluginId;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

/// A captured text or an unfinished replacement is dropped after this long, so an abandoned handle holds nothing.
pub const LIFETIME: Duration = Duration::from_secs(8);
/// Handles alive at once across all callers, and per caller. A bound on what is held, not on what is allowed.
const MOST_HANDLES: usize = 4;

/// What a platform answers when asked to read: the text if it may be read now, and whether it is a new copy.
pub struct Reading {
    pub text: Option<String>,
    pub changed: bool,
}

/// The four things only a platform can do. Everything else about clipboard access is in this module.
///
/// Each future must be cancellation safe: the application drops it when the guest is stopped, and a dropped write
/// must never reach the clipboard.
pub trait Board: Send + Sync {
    /// The text now, or `None` when it cannot be read (sensitive, empty, refused, or not allowed now).
    fn read(&self, observe: bool) -> impl Future<Output = Reading> + Send;
    /// Replace the clipboard text. `false` when the platform refused.
    fn write(&self, text: String) -> impl Future<Output = bool> + Send;
    /// Forget earlier copies so that only later ones are reported.
    fn reset(&self) -> impl Future<Output = bool> + Send;
}

struct Capture {
    caller: PluginId,
    text: String,
    taken: Instant,
}

struct Replacement {
    caller: PluginId,
    size: usize,
    text: String,
    started: Instant,
}

/// Every handle of every caller, with the rules that hold them. The owner supplies identifiers, so one counter
/// serves every facility and a handle of one kind can never be mistaken for another.
pub struct Handles {
    captures: HashMap<String, Capture>,
    replacements: HashMap<String, Replacement>,
    /// The most text one handle may carry on this platform. Never above the application's own bound, and a platform
    /// that can carry less says so here, so a request over it is refused when it begins and not after it was all sent.
    largest: usize,
}

impl Default for Handles {
    fn default() -> Self {
        Self::bounded(MAX_CLIPBOARD_BYTES)
    }
}

impl Handles {
    /// A platform that can carry less than the application allows says so here. It can lower the bound and can never
    /// raise it above the application's own, so no platform can open a wider door than the one the application guards.
    pub fn bounded(largest: usize) -> Self {
        Self {
            captures: HashMap::new(),
            replacements: HashMap::new(),
            largest: largest.min(MAX_CLIPBOARD_BYTES),
        }
    }
}

/// The refusals, in fixed words: nothing of what was read or written is repeated, because they reach screens and logs.
pub type Refusal = &'static str;

fn name(arguments: &Value, key: &str) -> Result<String, Refusal> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or("Missing clipboard handle")
}

fn offset(arguments: &Value) -> Result<usize, Refusal> {
    arguments
        .get("offset")
        .and_then(Value::as_u64)
        .and_then(|offset| usize::try_from(offset).ok())
        .ok_or("Invalid clipboard offset")
}

impl Handles {
    fn busy(&self, caller: &PluginId) -> bool {
        self.captures.len() >= MOST_HANDLES
            || self
                .captures
                .values()
                .any(|capture| capture.caller == *caller)
    }

    /// Hold `text` for `caller` and tell it how to ask for it. `fresh` says whether it is a new copy.
    fn capture(
        &mut self,
        caller: &PluginId,
        handle: String,
        text: String,
        fresh: bool,
    ) -> Result<Value, Refusal> {
        // The owner's counter never repeats a name. If one ever did, replacing the earlier capture would silently
        // take another extension's text away, so it is refused instead.
        if self.captures.contains_key(&handle) {
            return Err("That clipboard handle is already in use");
        }
        let bytes = text.len();
        self.captures.insert(
            handle.clone(),
            Capture {
                caller: caller.clone(),
                text,
                taken: Instant::now(),
            },
        );
        Ok(json!({"available":true,"clip":handle,"bytes":bytes,"changed":fresh}))
    }

    /// Redeem a text the application itself was given for this caller, as one capture. The caller has already proven
    /// the text is its own and still valid; this only holds it under the same bounds as a read.
    pub fn hold_selected(
        &mut self,
        caller: &PluginId,
        handle: String,
        text: String,
    ) -> Result<Value, Refusal> {
        if self.busy(caller) {
            return Err("Release the captured text before capturing another");
        }
        if text.is_empty() || text.len() > self.largest {
            return Err("Selected text is outside its bound");
        }
        self.capture(caller, handle, text, false)
    }

    /// Whether `caller` may capture now. Checked before the platform is asked, so a refused caller costs nothing.
    pub fn may_capture(&self, caller: &PluginId) -> Result<(), Refusal> {
        if self.busy(caller) {
            Err("Release the captured text before capturing another")
        } else {
            Ok(())
        }
    }

    /// Hold what the platform read, or say nothing was readable.
    pub fn held(
        &mut self,
        caller: &PluginId,
        handle: String,
        reading: Reading,
    ) -> Result<Value, Refusal> {
        let Some(text) = reading.text.filter(|text| !text.is_empty()) else {
            return Ok(json!({"available":false}));
        };
        if text.len() > self.largest {
            return Ok(json!({"available":false}));
        }
        self.capture(caller, handle, text, reading.changed)
    }

    /// One bounded piece of a captured text, never splitting a character.
    pub fn chunk(&mut self, caller: &PluginId, arguments: &Value) -> Result<Value, Refusal> {
        let handle = name(arguments, "clip")?;
        let offset = offset(arguments)?;
        let capture = self
            .captures
            .get(&handle)
            .filter(|capture| capture.caller == *caller)
            .ok_or("Unknown captured text")?;
        if capture.taken.elapsed() > LIFETIME {
            self.captures.remove(&handle);
            return Err("The captured text expired");
        }
        let text = &capture.text;
        if offset > text.len() || !text.is_char_boundary(offset) {
            return Err("Invalid clipboard offset");
        }
        let mut end = (offset + CLIPBOARD_CHUNK_BYTES).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        Ok(json!({"text":&text[offset..end],"next":end}))
    }

    /// Begin a replacement of a declared size. Nothing reaches the clipboard until it is committed whole.
    pub fn begin(
        &mut self,
        caller: &PluginId,
        handle: String,
        arguments: &Value,
    ) -> Result<Value, Refusal> {
        let size = arguments
            .get("bytes")
            .and_then(Value::as_u64)
            .and_then(|size| usize::try_from(size).ok())
            .filter(|size| *size > 0 && *size <= self.largest)
            .ok_or("Invalid clipboard size")?;
        if self.replacements.len() >= MOST_HANDLES
            || self
                .replacements
                .values()
                .any(|replacement| replacement.caller == *caller)
        {
            return Err("Finish the pending replacement before starting another");
        }
        if self.replacements.contains_key(&handle) {
            return Err("That clipboard handle is already in use");
        }
        self.replacements.insert(
            handle.clone(),
            Replacement {
                caller: caller.clone(),
                size,
                text: String::new(),
                started: Instant::now(),
            },
        );
        Ok(json!({"put":handle}))
    }

    /// One more piece, at exactly the offset reached: no gaps, no overlap, no more than was declared.
    pub fn append(&mut self, caller: &PluginId, arguments: &Value) -> Result<Value, Refusal> {
        let handle = name(arguments, "put")?;
        let piece = arguments
            .get("text")
            .and_then(Value::as_str)
            .filter(|piece| piece.len() <= CLIPBOARD_CHUNK_BYTES)
            .ok_or("Invalid clipboard piece")?;
        let offset = arguments
            .get("offset")
            .and_then(Value::as_u64)
            .ok_or("Missing clipboard offset")?;
        let replacement = self
            .replacements
            .get_mut(&handle)
            .filter(|replacement| replacement.caller == *caller)
            .ok_or("Unknown pending replacement")?;
        if replacement.started.elapsed() > LIFETIME
            || offset != replacement.text.len() as u64
            || replacement.text.len() + piece.len() > replacement.size
        {
            return Err("Invalid clipboard piece");
        }
        replacement.text.push_str(piece);
        Ok(json!({"next":replacement.text.len()}))
    }

    /// Take a finished replacement out, for the platform to write. Only a complete one, within its lifetime.
    pub fn finish(&mut self, caller: &PluginId, arguments: &Value) -> Result<String, Refusal> {
        let handle = name(arguments, "put")?;
        let replacement = self
            .replacements
            .get(&handle)
            .filter(|replacement| replacement.caller == *caller)
            .ok_or("Unknown pending replacement")?;
        if replacement.started.elapsed() > LIFETIME || replacement.text.len() != replacement.size {
            return Err("The replacement is incomplete");
        }
        self.replacements
            .remove(&handle)
            .map(|replacement| replacement.text)
            .ok_or("Unknown pending replacement")
    }

    /// Give back everything `caller` holds. Also what ends a generation.
    pub fn release(&mut self, caller: &PluginId) {
        self.captures.retain(|_, capture| capture.caller != *caller);
        self.replacements
            .retain(|_, replacement| replacement.caller != *caller);
    }
}

/// What a call is, once its arguments are read. The owner dispatches on this and not on strings.
pub enum Method {
    Read,
    Observe,
    Chunk,
    Write,
    Append,
    Commit,
    Reset,
    Release,
}

impl Method {
    /// `selected` is not here: redeeming staged input depends on state the owner holds for every facility.
    pub fn parse(method: &str) -> Option<Self> {
        Some(match method {
            "read" => Self::Read,
            "observe" => Self::Observe,
            "chunk" => Self::Chunk,
            "write" => Self::Write,
            "append" => Self::Append,
            "commit" => Self::Commit,
            "reset" => Self::Reset,
            "release" => Self::Release,
            _ => return None,
        })
    }
}

/// Write a finished replacement through the platform, with a fixed refusal when it declines.
pub async fn commit<B: Board>(board: &B, text: String) -> Result<Value, Refusal> {
    if board.write(text).await {
        Ok(json!({"written":true}))
    } else {
        Err("The platform refused the clipboard write")
    }
}

/// Ask the platform to forget earlier copies.
pub async fn reset<B: Board>(board: &B) -> Value {
    json!({"reset":board.reset().await})
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn who(name: &str) -> PluginId {
        PluginId::new(name).unwrap()
    }
    fn reading(text: &str) -> Reading {
        Reading {
            text: Some(text.into()),
            changed: false,
        }
    }
    fn held(handles: &mut Handles, caller: &PluginId, handle: &str, text: &str) -> Value {
        handles.held(caller, handle.into(), reading(text)).unwrap()
    }

    /// A platform that records what it was asked to do, and answers as scripted.
    struct Script {
        text: Mutex<Option<String>>,
        written: Mutex<Vec<String>>,
        accept: bool,
    }
    impl Board for Script {
        async fn read(&self, _: bool) -> Reading {
            Reading {
                text: self.text.lock().unwrap().clone(),
                changed: true,
            }
        }
        async fn write(&self, text: String) -> bool {
            self.written.lock().unwrap().push(text);
            self.accept
        }
        async fn reset(&self) -> bool {
            true
        }
    }

    #[test]
    fn full_size_unicode_survives_bounded_pieces_both_ways() {
        // Nine bytes a repeat, so 900 000: larger than a control message, below the bound, and made of characters that
        // must never be split.
        let text = "\0\"\\🙂é".repeat(100_000);
        assert!(text.len() < MAX_CLIPBOARD_BYTES && text.len() > 1024 * 400);
        let mut handles = Handles::default();
        let caller = who("one");
        let shown = held(&mut handles, &caller, "clip-1", &text);
        let mut got = String::new();
        loop {
            let piece = handles
                .chunk(&caller, &json!({"clip":shown["clip"],"offset":got.len()}))
                .unwrap();
            assert!(serde_json::to_vec(&piece).unwrap().len() < 65536);
            let part = piece["text"].as_str().unwrap();
            got.push_str(part);
            assert_eq!(piece["next"].as_u64().unwrap() as usize, got.len());
            if got.len() == text.len() {
                break;
            }
            assert!(
                !part.is_empty(),
                "a piece that makes no progress would loop forever"
            );
        }
        assert_eq!(got, text);
        let begun = handles
            .begin(&caller, "put-1".into(), &json!({"bytes":got.len()}))
            .unwrap();
        let mut offset = 0;
        while offset < got.len() {
            let mut end = (offset + CLIPBOARD_CHUNK_BYTES).min(got.len());
            while !got.is_char_boundary(end) {
                end -= 1;
            }
            handles
                .append(
                    &caller,
                    &json!({"put":begun["put"],"offset":offset,"text":&got[offset..end]}),
                )
                .unwrap();
            offset = end;
        }
        assert_eq!(
            handles
                .finish(&caller, &json!({"put":begun["put"]}))
                .unwrap(),
            text
        );
    }

    #[test]
    fn a_repeated_handle_name_is_refused_and_never_replaces_another_callers_text() {
        let mut handles = Handles::default();
        let (one, two) = (who("one"), who("two"));
        let shown = held(&mut handles, &one, "clip-1", "mine");
        assert_eq!(
            handles.held(&two, "clip-1".into(), reading("theirs")),
            Err("That clipboard handle is already in use")
        );
        let piece = handles
            .chunk(&one, &json!({"clip":shown["clip"],"offset":0}))
            .unwrap();
        assert_eq!(
            piece["text"], "mine",
            "the earlier capture is exactly as it was"
        );
        handles
            .begin(&one, "put-1".into(), &json!({"bytes":1}))
            .unwrap();
        assert_eq!(
            handles.begin(&two, "put-1".into(), &json!({"bytes":1})),
            Err("That clipboard handle is already in use")
        );
    }

    #[test]
    fn one_extensions_handles_are_invisible_and_unusable_to_another() {
        let mut handles = Handles::default();
        let (one, two) = (who("one"), who("two"));
        let shown = held(&mut handles, &one, "clip-1", "private");
        assert_eq!(
            handles.chunk(&two, &json!({"clip":shown["clip"],"offset":0})),
            Err("Unknown captured text")
        );
        let begun = handles
            .begin(&one, "put-1".into(), &json!({"bytes":1}))
            .unwrap();
        handles
            .append(&one, &json!({"put":begun["put"],"offset":0,"text":"x"}))
            .unwrap();
        assert_eq!(
            handles.append(&two, &json!({"put":begun["put"],"offset":1,"text":"y"})),
            Err("Unknown pending replacement")
        );
        assert_eq!(
            handles.finish(&two, &json!({"put":begun["put"]})),
            Err("Unknown pending replacement")
        );
        // Both still belong to their owner, untouched by the attempts.
        assert!(
            handles
                .chunk(&one, &json!({"clip":shown["clip"],"offset":0}))
                .is_ok()
        );
        assert_eq!(
            handles.finish(&one, &json!({"put":begun["put"]})).unwrap(),
            "x"
        );
    }

    #[test]
    fn a_caller_holds_one_capture_and_one_replacement_and_all_callers_together_four() {
        let mut handles = Handles::default();
        let one = who("one");
        held(&mut handles, &one, "clip-1", "a");
        assert!(
            handles.may_capture(&one).is_err(),
            "a second capture waits for the first to be released"
        );
        for n in 0..3 {
            held(
                &mut handles,
                &who(&format!("c{n}")),
                &format!("clip-c{n}"),
                "a",
            );
        }
        assert!(
            handles.may_capture(&who("late")).is_err(),
            "four in all is the bound"
        );
        handles.release(&one);
        assert!(
            handles.may_capture(&who("late")).is_ok(),
            "a release frees room for others"
        );
        handles
            .begin(&one, "put-1".into(), &json!({"bytes":1}))
            .unwrap();
        assert_eq!(
            handles.begin(&one, "put-2".into(), &json!({"bytes":1})),
            Err("Finish the pending replacement before starting another")
        );
    }

    #[test]
    fn a_replacement_commits_only_complete_and_in_order() {
        let mut handles = Handles::default();
        let caller = who("one");
        let begun = handles
            .begin(&caller, "put-1".into(), &json!({"bytes":3}))
            .unwrap();
        let put = &begun["put"];
        // Too early, a gap, an overlap, too much, and a piece over the bound.
        assert!(handles.finish(&caller, &json!({"put":put})).is_err());
        for (offset, text) in [(1, "a"), (0, "abcd")] {
            assert!(
                handles
                    .append(&caller, &json!({"put":put,"offset":offset,"text":text}))
                    .is_err()
            );
        }
        assert!(
            handles
                .append(
                    &caller,
                    &json!({"put":put,"offset":0,"text":"x".repeat(CLIPBOARD_CHUNK_BYTES + 1)})
                )
                .is_err()
        );
        handles
            .append(&caller, &json!({"put":put,"offset":0,"text":"ab"}))
            .unwrap();
        assert!(
            handles
                .append(&caller, &json!({"put":put,"offset":0,"text":"zz"}))
                .is_err(),
            "no overlap"
        );
        assert!(
            handles.finish(&caller, &json!({"put":put})).is_err(),
            "two of three is not complete"
        );
        handles
            .append(&caller, &json!({"put":put,"offset":2,"text":"c"}))
            .unwrap();
        assert_eq!(handles.finish(&caller, &json!({"put":put})).unwrap(), "abc");
        assert!(
            handles.finish(&caller, &json!({"put":put})).is_err(),
            "a finished replacement is gone"
        );
    }

    #[test]
    fn sizes_and_offsets_outside_their_bounds_are_refused() {
        let mut handles = Handles::default();
        let caller = who("one");
        for bytes in [
            json!(0),
            json!(MAX_CLIPBOARD_BYTES + 1),
            json!(-1),
            json!("5"),
            json!(null),
        ] {
            assert_eq!(
                handles.begin(&caller, "put-1".into(), &json!({"bytes":bytes})),
                Err("Invalid clipboard size"),
                "{bytes}"
            );
        }
        let shown = held(&mut handles, &caller, "clip-1", "héllo");
        // An offset past the end, and one inside a multi-byte character.
        for offset in [json!(100), json!(2), json!(-1), json!("0")] {
            assert!(
                handles
                    .chunk(&caller, &json!({"clip":shown["clip"],"offset":offset}))
                    .is_err(),
                "{offset}"
            );
        }
    }

    #[test]
    fn nothing_readable_holds_nothing_and_an_oversize_text_is_never_held() {
        let mut handles = Handles::default();
        let caller = who("one");
        for text in [
            None,
            Some(String::new()),
            Some("x".repeat(MAX_CLIPBOARD_BYTES + 1)),
        ] {
            let shown = handles
                .held(
                    &caller,
                    "clip-1".into(),
                    Reading {
                        text,
                        changed: true,
                    },
                )
                .unwrap();
            assert_eq!(shown, json!({"available":false}));
        }
        assert!(
            handles.may_capture(&caller).is_ok(),
            "nothing was held, so nothing blocks the next capture"
        );
    }

    #[test]
    fn selected_text_is_held_under_the_same_bounds_and_never_marked_as_a_new_copy() {
        let mut handles = Handles::default();
        let caller = who("one");
        let shown = handles
            .hold_selected(&caller, "clip-1".into(), "shared".into())
            .unwrap();
        assert_eq!(shown["changed"], false);
        assert_eq!(
            handles.hold_selected(&caller, "clip-2".into(), "again".into()),
            Err("Release the captured text before capturing another")
        );
        handles.release(&caller);
        for text in [String::new(), "x".repeat(MAX_CLIPBOARD_BYTES + 1)] {
            assert_eq!(
                handles.hold_selected(&caller, "clip-3".into(), text),
                Err("Selected text is outside its bound")
            );
        }
    }

    #[test]
    fn a_platform_can_lower_the_bound_and_never_raise_it_and_the_refusal_comes_at_the_start() {
        let caller = who("one");
        let mut small = Handles::bounded(64 * 1024);
        // Over the platform's bound, refused when the replacement begins, before anything was sent.
        assert_eq!(
            small.begin(&caller, "put-1".into(), &json!({"bytes":64 * 1024 + 1})),
            Err("Invalid clipboard size")
        );
        assert!(
            small
                .begin(&caller, "put-1".into(), &json!({"bytes":64 * 1024}))
                .is_ok(),
            "exactly the bound is fine"
        );
        // A text over it is not held when read either, and selected text obeys the same bound.
        let mut small = Handles::bounded(64 * 1024);
        let big = "x".repeat(64 * 1024 + 1);
        assert_eq!(
            small.held(&caller, "clip-1".into(), reading(&big)).unwrap(),
            json!({"available":false})
        );
        assert_eq!(
            small.hold_selected(&caller, "clip-1".into(), big),
            Err("Selected text is outside its bound")
        );
        // Asking for more than the application allows is clamped to what the application allows.
        let mut wide = Handles::bounded(usize::MAX);
        assert_eq!(
            wide.begin(
                &caller,
                "put-1".into(),
                &json!({"bytes":MAX_CLIPBOARD_BYTES + 1})
            ),
            Err("Invalid clipboard size")
        );
        assert!(
            wide.begin(
                &caller,
                "put-2".into(),
                &json!({"bytes":MAX_CLIPBOARD_BYTES})
            )
            .is_ok()
        );
    }

    #[test]
    fn releasing_a_caller_removes_everything_it_holds_and_nothing_of_anyone_else() {
        let mut handles = Handles::default();
        let (one, two) = (who("one"), who("two"));
        held(&mut handles, &one, "clip-1", "mine");
        let begun = handles
            .begin(&one, "put-1".into(), &json!({"bytes":1}))
            .unwrap();
        let theirs = held(&mut handles, &two, "clip-2", "theirs");
        handles.release(&one);
        assert!(
            handles
                .chunk(&one, &json!({"clip":"clip-1","offset":0}))
                .is_err()
        );
        assert!(
            handles
                .append(&one, &json!({"put":begun["put"],"offset":0,"text":"x"}))
                .is_err()
        );
        assert!(
            handles
                .chunk(&two, &json!({"clip":theirs["clip"],"offset":0}))
                .is_ok()
        );
    }

    #[tokio::test]
    async fn the_platform_decides_what_is_written_and_a_refusal_is_reported_in_fixed_words() {
        let accepting = Script {
            text: Mutex::new(Some("now".into())),
            written: Mutex::default(),
            accept: true,
        };
        assert_eq!(
            commit(&accepting, "text".into()).await.unwrap(),
            json!({"written":true})
        );
        assert_eq!(accepting.written.lock().unwrap().as_slice(), ["text"]);
        let refusing = Script {
            text: Mutex::new(None),
            written: Mutex::default(),
            accept: false,
        };
        assert_eq!(
            commit(&refusing, "text".into()).await,
            Err("The platform refused the clipboard write")
        );
        assert_eq!(reset(&refusing).await, json!({"reset":true}));
        assert!(refusing.read(false).await.text.is_none());
    }

    #[test]
    fn only_the_methods_this_module_owns_are_recognised() {
        for known in [
            "read", "observe", "chunk", "write", "append", "commit", "reset", "release",
        ] {
            assert!(Method::parse(known).is_some(), "{known}");
        }
        // Redeeming staged input is the owner's, because it shares state with every other facility.
        for other in ["selected", "", "READ", "request", "chunk "] {
            assert!(Method::parse(other).is_none(), "{other:?}");
        }
    }
}
