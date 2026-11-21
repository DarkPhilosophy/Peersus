//! What counts as a new copy worth sending, and what never does.
//!
//! The same rules as the process variant, written again because that one lives in a crate with an operating system
//! underneath it. They are pure on purpose: no clock, no clipboard, no connection, so every one is a test.
//!
//! The rule behind all of them: an old clipboard must never be sent as if it were just copied. Anything that breaks
//! the chain of what was seen (nothing readable, empty, too large, sensitive) makes the next reading a new baseline.
use crate::wire::MAX_TEXT;

/// State holds at most one bounded text. Unavailable reads break continuity rather than letting a later grant or a
/// regained focus send what happened to be on the clipboard.
#[derive(Default)]
pub struct Observation {
    last: Option<String>,
    initialized: bool,
}

fn usable(text: Option<String>) -> Option<String> {
    text.filter(|text| !text.is_empty() && text.len() <= MAX_TEXT)
}

impl Observation {
    /// A snapshot: the text now, or nothing readable. Returns the text only when it is a copy made since the last one.
    pub fn changed(&mut self, text: Option<String>) -> Option<String> {
        let text = usable(text);
        let changed = self.initialized && self.last != text;
        self.initialized = text.is_some();
        self.last.clone_from(&text);
        if changed { text } else { None }
    }

    /// Text this device itself placed: not a copy of its own, so it is never sent back.
    pub fn received(&mut self, text: &str) {
        self.last = Some(text.to_owned());
        self.initialized = true;
    }

    /// A platform event proves freshness even after unreadable contents: the copy happened after the last baseline.
    pub fn fresh(&mut self, text: String) -> Option<String> {
        if text.is_empty() || text.len() > MAX_TEXT {
            return self.changed(None);
        }
        let duplicate = self.last.as_ref() == Some(&text);
        self.initialized = true;
        self.last = Some(text.clone());
        (!duplicate).then_some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_clipboard_and_reconnect_are_not_sent() {
        let mut state = Observation::default();
        assert_eq!(state.changed(Some("old".into())), None);
        assert_eq!(state.changed(None), None);
        assert_eq!(state.changed(Some("copied while unavailable".into())), None);
        assert_eq!(state.changed(Some("fresh".into())), Some("fresh".into()));
    }

    #[test]
    fn remote_write_and_duplicate_poll_do_not_echo() {
        let mut state = Observation::default();
        state.changed(Some("local".into()));
        state.received("remote");
        assert_eq!(state.changed(Some("remote".into())), None);
        assert_eq!(state.changed(Some("remote".into())), None);
        assert_eq!(
            state.changed(Some("new local".into())),
            Some("new local".into())
        );
    }

    #[test]
    fn empty_oversized_and_sensitive_unavailable_reads_break_continuity() {
        let mut state = Observation::default();
        state.changed(Some("before".into()));
        assert_eq!(state.changed(Some("x".repeat(MAX_TEXT + 1))), None);
        assert_eq!(state.changed(Some("after sensitive".into())), None);
        assert_eq!(state.changed(Some(String::new())), None);
    }

    #[test]
    fn a_proven_new_copy_after_sensitive_contents_is_not_mistaken_for_a_stale_snapshot() {
        let mut state = Observation::default();
        state.received("remote sensitive");
        assert_eq!(state.changed(None), None);
        assert_eq!(
            state.fresh("genuinely copied now".into()),
            Some("genuinely copied now".into())
        );
        assert_eq!(state.fresh("genuinely copied now".into()), None);
    }

    #[test]
    fn received_text_does_not_echo_after_destination_changes_rebaseline() {
        let mut state = Observation::default();
        state.received("remote");
        state = Observation::default();
        assert_eq!(state.changed(Some("remote".into())), None);
        assert_eq!(state.changed(Some("remote".into())), None);
    }

    #[test]
    fn an_oversize_or_empty_event_is_not_a_copy_and_breaks_the_chain() {
        let mut state = Observation::default();
        state.fresh("one".into());
        assert_eq!(state.fresh("x".repeat(MAX_TEXT + 1)), None);
        assert_eq!(state.fresh(String::new()), None);
        // The chain is broken, so what follows is a baseline and not a copy.
        assert_eq!(state.changed(Some("two".into())), None);
    }
}
