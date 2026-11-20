//! A key pressed in the window, read as a chord of the contract (`peersus_api::chord`). The one place the toolkit's key events
//! become chords: the Binds page records them with it, and the window runs a bound action by it.

use freya::prelude::{Code, Key, Modifiers};
use peersus_api::chord::{Chord, ChordKey, Modifiers as Held};

/// The chord a key press makes, or `None` for a press that is no chord: a modifier alone, a key outside the contract's list, or a key
/// with no Control, Alt or system key held (that is typing). A letter is read by what the layout prints on it, so a chord is the same
/// on a keyboard laid out otherwise; digits, function and named keys by their place.
pub fn of(key: &Key, code: Code, modifiers: Modifiers) -> Option<Chord> {
    let held = Held {
        control: modifiers.contains(Modifiers::CONTROL),
        alt: modifiers.contains(Modifiers::ALT),
        shift: modifiers.contains(Modifiers::SHIFT),
        system: modifiers.contains(Modifiers::META),
    };
    Chord::new(held, key_of(key, code)?)
}

/// The modifiers held now, written as a chord begins (`ctrl+alt+…`), for the Binds page while it records; `None` when none is held.
/// `key` is the key of the event: a modifier going down counts as held and one going up does not, whatever the platform reported
/// with it (the state it sends may be from before or after the key).
pub fn held_words(key: &Key, modifiers: Modifiers, down: bool) -> Option<String> {
    use freya::prelude::NamedKey;
    let mut held = modifiers;
    let own = match key {
        Key::Named(NamedKey::Control) => Some(Modifiers::CONTROL),
        Key::Named(NamedKey::Alt) => Some(Modifiers::ALT),
        Key::Named(NamedKey::Shift) => Some(Modifiers::SHIFT),
        Key::Named(NamedKey::Meta) => Some(Modifiers::META),
        _ => None,
    };
    if let Some(own) = own {
        held.set(own, down);
    }
    let words: Vec<&str> = [
        (Modifiers::CONTROL, "ctrl"),
        (Modifiers::ALT, "alt"),
        (Modifiers::SHIFT, "shift"),
        (Modifiers::META, "super"),
    ]
    .into_iter()
    .filter(|(one, _)| held.contains(*one))
    .map(|(_, word)| word)
    .collect();
    (!words.is_empty()).then(|| format!("{}+…", words.join("+")))
}

fn key_of(key: &Key, code: Code) -> Option<ChordKey> {
    if let Key::Character(text) = key {
        let mut letters = text.chars();
        if let (Some(letter), None) = (letters.next(), letters.next())
            && letter.is_ascii_alphabetic()
        {
            return Some(ChordKey::Letter(letter.to_ascii_lowercase()));
        }
    }
    Some(match code {
        Code::KeyA => ChordKey::Letter('a'),
        Code::KeyB => ChordKey::Letter('b'),
        Code::KeyC => ChordKey::Letter('c'),
        Code::KeyD => ChordKey::Letter('d'),
        Code::KeyE => ChordKey::Letter('e'),
        Code::KeyF => ChordKey::Letter('f'),
        Code::KeyG => ChordKey::Letter('g'),
        Code::KeyH => ChordKey::Letter('h'),
        Code::KeyI => ChordKey::Letter('i'),
        Code::KeyJ => ChordKey::Letter('j'),
        Code::KeyK => ChordKey::Letter('k'),
        Code::KeyL => ChordKey::Letter('l'),
        Code::KeyM => ChordKey::Letter('m'),
        Code::KeyN => ChordKey::Letter('n'),
        Code::KeyO => ChordKey::Letter('o'),
        Code::KeyP => ChordKey::Letter('p'),
        Code::KeyQ => ChordKey::Letter('q'),
        Code::KeyR => ChordKey::Letter('r'),
        Code::KeyS => ChordKey::Letter('s'),
        Code::KeyT => ChordKey::Letter('t'),
        Code::KeyU => ChordKey::Letter('u'),
        Code::KeyV => ChordKey::Letter('v'),
        Code::KeyW => ChordKey::Letter('w'),
        Code::KeyX => ChordKey::Letter('x'),
        Code::KeyY => ChordKey::Letter('y'),
        Code::KeyZ => ChordKey::Letter('z'),
        Code::Digit0 => ChordKey::Digit(0),
        Code::Digit1 => ChordKey::Digit(1),
        Code::Digit2 => ChordKey::Digit(2),
        Code::Digit3 => ChordKey::Digit(3),
        Code::Digit4 => ChordKey::Digit(4),
        Code::Digit5 => ChordKey::Digit(5),
        Code::Digit6 => ChordKey::Digit(6),
        Code::Digit7 => ChordKey::Digit(7),
        Code::Digit8 => ChordKey::Digit(8),
        Code::Digit9 => ChordKey::Digit(9),
        Code::F1 => ChordKey::Function(1),
        Code::F2 => ChordKey::Function(2),
        Code::F3 => ChordKey::Function(3),
        Code::F4 => ChordKey::Function(4),
        Code::F5 => ChordKey::Function(5),
        Code::F6 => ChordKey::Function(6),
        Code::F7 => ChordKey::Function(7),
        Code::F8 => ChordKey::Function(8),
        Code::F9 => ChordKey::Function(9),
        Code::F10 => ChordKey::Function(10),
        Code::F11 => ChordKey::Function(11),
        Code::F12 => ChordKey::Function(12),
        Code::Enter | Code::NumpadEnter => ChordKey::Enter,
        Code::Space => ChordKey::Space,
        Code::Tab => ChordKey::Tab,
        Code::Escape => ChordKey::Escape,
        Code::ArrowUp => ChordKey::Up,
        Code::ArrowDown => ChordKey::Down,
        Code::ArrowLeft => ChordKey::Left,
        Code::ArrowRight => ChordKey::Right,
        Code::Home => ChordKey::Home,
        Code::End => ChordKey::End,
        Code::Delete => ChordKey::Delete,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use freya::prelude::NamedKey;

    fn chord(text: &str) -> Option<Chord> {
        Some(text.parse().unwrap())
    }

    #[test]
    fn a_press_with_control_alt_or_the_system_key_is_a_chord_and_typing_is_not() {
        let letter = Key::Character("a".into());
        assert_eq!(
            of(&letter, Code::KeyA, Modifiers::CONTROL | Modifiers::ALT),
            chord("ctrl+alt+a")
        );
        assert_eq!(of(&letter, Code::KeyA, Modifiers::META), chord("super+a"));
        // Shift alone is typing; a modifier alone is no chord.
        assert_eq!(
            of(&Key::Character("A".into()), Code::KeyA, Modifiers::SHIFT),
            None
        );
        assert_eq!(
            of(
                &Key::Named(NamedKey::Control),
                Code::ControlLeft,
                Modifiers::CONTROL
            ),
            None
        );
        // A key outside the list makes nothing.
        assert_eq!(
            of(
                &Key::Named(NamedKey::PageUp),
                Code::PageUp,
                Modifiers::CONTROL
            ),
            None
        );
    }

    #[test]
    fn a_letter_is_what_the_layout_prints_and_a_digit_is_its_place() {
        // The key in the place of Q on a keyboard laid out to print A there.
        assert_eq!(
            of(&Key::Character("a".into()), Code::KeyQ, Modifiers::CONTROL),
            chord("ctrl+a")
        );
        // Shift with a digit prints a sign; the chord keeps the digit.
        assert_eq!(
            of(
                &Key::Character("!".into()),
                Code::Digit1,
                Modifiers::CONTROL | Modifiers::SHIFT
            ),
            chord("ctrl+shift+1")
        );
        assert_eq!(
            of(&Key::Named(NamedKey::F5), Code::F5, Modifiers::ALT),
            chord("alt+f5")
        );
        assert_eq!(
            of(
                &Key::Named(NamedKey::Enter),
                Code::NumpadEnter,
                Modifiers::CONTROL
            ),
            chord("ctrl+enter")
        );
    }

    #[test]
    fn what_is_held_while_recording_is_shown_as_the_start_of_a_chord() {
        let alt = Key::Named(NamedKey::Alt);
        // The platform may report Alt with its own press or only after it: either way it is held.
        assert_eq!(
            held_words(&alt, Modifiers::empty(), true).as_deref(),
            Some("alt+…")
        );
        assert_eq!(
            held_words(&Key::Named(NamedKey::Control), Modifiers::ALT, true).as_deref(),
            Some("ctrl+alt+…")
        );
        // Let go, it is no longer held, whatever state came with it.
        assert_eq!(held_words(&alt, Modifiers::ALT, false), None);
        assert_eq!(
            held_words(&Key::Character("a".into()), Modifiers::empty(), true),
            None
        );
    }
}
