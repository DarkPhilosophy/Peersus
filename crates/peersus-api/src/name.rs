//! One identifier grammar for every addressable thing (plugins, services, methods,
//! capabilities, pages, actions, settings) and the bounded-text rule for labels.

pub const MAX_NAME: usize = 128;

/// Whether every byte is one an identifier is written in: an ASCII letter or digit, `.`, `_` or `-`. Only the alphabet: what else a
/// name must be (not empty, how long, how it may begin) is the rule of whoever names the thing.
pub fn plain(value: &str) -> bool {
    value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// ASCII alphanumerics plus `.`, `_` and `-`; non-empty and bounded.
pub fn valid(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_NAME && plain(value)
}

/// Human-readable text: non-blank, bounded by characters, no control characters.
pub fn text(value: &str, max_chars: usize) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= max_chars
        && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_strictly_ascii_and_bounded() {
        assert!(valid("clipboard.read"));
        assert!(valid("a-b_c.1"));
        for bad in ["", "a b", "a/b", "é", "a\n", &"x".repeat(MAX_NAME + 1)] {
            assert!(!valid(bad), "{bad:?}");
        }
        assert!(valid(&"x".repeat(MAX_NAME)));
    }

    #[test]
    fn text_rejects_blank_control_and_oversize() {
        assert!(text("Sync clipboard", 80));
        assert!(text("Acțiune", 80));
        assert!(!text("   ", 80));
        assert!(!text("bad\u{7}", 80));
        assert!(!text(&"é".repeat(81), 80));
        assert!(text(&"é".repeat(80), 80));
    }
}
