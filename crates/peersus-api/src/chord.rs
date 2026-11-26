//! A chord: the keys a person presses together to run an action. Modifiers and one key of a
//! closed list, written one way (`"ctrl+alt+1"`), so the same chord reads the same in a
//! manifest, in the settings and on every device.
//!
//! A chord always holds Control, Alt or the system key: it is never something a person types
//! as text. What a chord runs, and whether it is bound at all, is the application's; an
//! extension only suggests one for an action of its own.

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// Longest written chord (`"ctrl+alt+shift+super+escape"` is 28 bytes).
pub const MAX_CHORD: usize = 32;
/// Function keys every keyboard has.
pub const MAX_FUNCTION_KEY: u8 = 12;

/// The keys held with the chord's key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Modifiers {
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    /// The system key: Super, Windows or Command, whatever the keyboard calls it.
    pub system: bool,
}

/// The one key of a chord, by what is printed on it rather than by a code of some system.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ChordKey {
    /// `a` to `z`: the key, whatever Shift makes of it.
    Letter(char),
    /// `0` to `9` of the row above the letters.
    Digit(u8),
    /// `f1` to `f12`.
    Function(u8),
    Enter,
    Space,
    Tab,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Delete,
}

const NAMED: [(&str, ChordKey); 11] = [
    ("enter", ChordKey::Enter),
    ("space", ChordKey::Space),
    ("tab", ChordKey::Tab),
    ("escape", ChordKey::Escape),
    ("up", ChordKey::Up),
    ("down", ChordKey::Down),
    ("left", ChordKey::Left),
    ("right", ChordKey::Right),
    ("home", ChordKey::Home),
    ("end", ChordKey::End),
    ("delete", ChordKey::Delete),
];

impl ChordKey {
    fn in_range(self) -> bool {
        match self {
            Self::Letter(letter) => letter.is_ascii_lowercase(),
            Self::Digit(digit) => digit <= 9,
            Self::Function(number) => (1..=MAX_FUNCTION_KEY).contains(&number),
            _ => true,
        }
    }

    fn read(word: &str) -> Option<Self> {
        let mut chars = word.chars();
        let key = match (chars.next()?, chars.as_str()) {
            (letter @ 'a'..='z', "") => Self::Letter(letter),
            (digit @ '0'..='9', "") => Self::Digit(digit as u8 - b'0'),
            ('f', number) if number.bytes().all(|b| b.is_ascii_digit()) => {
                Self::Function(number.parse().ok()?)
            }
            _ => NAMED.iter().find(|(name, _)| *name == word)?.1,
        };
        key.in_range().then_some(key)
    }
}

impl fmt::Display for ChordKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Letter(letter) => write!(f, "{letter}"),
            Self::Digit(digit) => write!(f, "{digit}"),
            Self::Function(number) => write!(f, "f{number}"),
            named => f.write_str(
                NAMED
                    .iter()
                    .find(|(_, key)| key == named)
                    .map_or("", |(name, _)| name),
            ),
        }
    }
}

/// Modifiers and one key. Built only through [`Chord::new`] or from its text, so a chord that
/// exists is one the rules allow.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Chord {
    modifiers: Modifiers,
    key: ChordKey,
}

impl Chord {
    /// `None` for what would be typed text (no Control, Alt or system key) and for a key
    /// outside the list.
    pub fn new(modifiers: Modifiers, key: ChordKey) -> Option<Self> {
        ((modifiers.control || modifiers.alt || modifiers.system) && key.in_range())
            .then_some(Self { modifiers, key })
    }

    pub fn modifiers(&self) -> Modifiers {
        self.modifiers
    }

    pub fn key(&self) -> ChordKey {
        self.key
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Modifiers {
            control,
            alt,
            shift,
            system,
        } = self.modifiers;
        for (held, name) in [
            (control, "ctrl"),
            (alt, "alt"),
            (shift, "shift"),
            (system, "super"),
        ] {
            if held {
                write!(f, "{name}+")?;
            }
        }
        self.key.fmt(f)
    }
}

impl FromStr for Chord {
    type Err = Error;

    /// Reads the one spelling [`fmt::Display`] writes: lower case, modifiers in the order
    /// `ctrl`, `alt`, `shift`, `super`, each at most once, then the key.
    fn from_str(text: &str) -> Result<Self> {
        if text.len() > MAX_CHORD {
            return Err(Error::InvalidManifest);
        }
        let (held, key) = text.rsplit_once('+').unwrap_or(("", text));
        let mut modifiers = Modifiers::default();
        for name in held.split('+').filter(|name| !name.is_empty()) {
            match name {
                "ctrl" => modifiers.control = true,
                "alt" => modifiers.alt = true,
                "shift" => modifiers.shift = true,
                "super" => modifiers.system = true,
                _ => return Err(Error::InvalidManifest),
            }
        }
        ChordKey::read(key)
            .and_then(|key| Self::new(modifiers, key))
            // Anything read leniently above (a repeated or misplaced modifier, `f01`, an empty
            // part) is not the spelling this writes, and is refused here.
            .filter(|chord| chord.to_string() == text)
            .ok_or(Error::InvalidManifest)
    }
}

impl TryFrom<String> for Chord {
    type Error = Error;

    fn try_from(text: String) -> Result<Self> {
        text.parse()
    }
}

impl From<Chord> for String {
    fn from(chord: Chord) -> Self {
        chord.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn every_key() -> Vec<ChordKey> {
        ('a'..='z')
            .map(ChordKey::Letter)
            .chain((0..=9).map(ChordKey::Digit))
            .chain((1..=MAX_FUNCTION_KEY).map(ChordKey::Function))
            .chain(NAMED.iter().map(|(_, key)| *key))
            .collect()
    }

    #[test]
    fn every_chord_is_written_one_way_and_read_back_the_same() {
        let mut written = std::collections::BTreeSet::new();
        for bits in 0..16u8 {
            let modifiers = Modifiers {
                control: bits & 1 != 0,
                alt: bits & 2 != 0,
                shift: bits & 4 != 0,
                system: bits & 8 != 0,
            };
            for key in every_key() {
                let typed = !(modifiers.control || modifiers.alt || modifiers.system);
                let Some(chord) = Chord::new(modifiers, key) else {
                    assert!(typed, "{modifiers:?} {key:?} refused");
                    continue;
                };
                assert!(!typed, "{modifiers:?} {key:?} is something typed");
                let text = chord.to_string();
                assert!(text.len() <= MAX_CHORD, "{text}");
                assert_eq!(text.parse::<Chord>(), Ok(chord), "{text}");
                assert!(written.insert(text), "two chords written alike");
            }
        }
        // Sixteen sets of modifiers, less the two that only type: none, and Shift alone.
        assert_eq!(written.len(), 14 * every_key().len());
        assert_eq!(
            "ctrl+alt+1".parse::<Chord>().unwrap(),
            Chord::new(
                Modifiers {
                    control: true,
                    alt: true,
                    ..Default::default()
                },
                ChordKey::Digit(1)
            )
            .unwrap()
        );
        assert_eq!(
            "ctrl+alt+shift+super+escape"
                .parse::<Chord>()
                .unwrap()
                .to_string(),
            "ctrl+alt+shift+super+escape"
        );
    }

    #[test]
    fn what_is_typed_misspelled_or_unknown_is_refused() {
        for bad in [
            "",
            "a",
            "shift+a",
            "f5",
            "ctrl",
            "ctrl+",
            "+a",
            "ctrl++a",
            "ctrl+ctrl+a",
            "alt+ctrl+a",
            "Ctrl+a",
            "ctrl+A",
            "ctrl + a",
            "ctrl+ab",
            "ctrl+é",
            "ctrl+f0",
            "ctrl+f13",
            "ctrl+f01",
            "ctrl+f+1",
            "ctrl+10",
            "ctrl+meta+a",
            "control+a",
            "ctrl+return",
            "ctrl+a\n",
            "ctrl+alt+shift+super+escape+escape",
        ] {
            assert_eq!(bad.parse::<Chord>(), Err(Error::InvalidManifest), "{bad:?}");
        }
        let held = Modifiers {
            control: true,
            ..Default::default()
        };
        for key in [
            ChordKey::Letter('A'),
            ChordKey::Letter('é'),
            ChordKey::Letter('1'),
            ChordKey::Digit(10),
            ChordKey::Function(0),
            ChordKey::Function(MAX_FUNCTION_KEY + 1),
        ] {
            assert_eq!(Chord::new(held, key), None, "{key:?}");
        }
    }

    #[test]
    fn a_chord_crosses_as_text_and_can_name_an_entry_of_a_map() {
        let chord: Chord = serde_json::from_str(r#""ctrl+alt+f4""#).unwrap();
        assert_eq!(chord.key(), ChordKey::Function(4));
        assert!(chord.modifiers().control && chord.modifiers().alt);
        assert_eq!(serde_json::to_string(&chord).unwrap(), r#""ctrl+alt+f4""#);
        for bad in [r#""alt+ctrl+f4""#, r#"{"key":"a"}"#, "1", "null"] {
            assert!(serde_json::from_str::<Chord>(bad).is_err(), "{bad}");
        }
        let bound = BTreeMap::from([(chord, 7u8)]);
        let wire = serde_json::to_string(&bound).unwrap();
        assert_eq!(wire, r#"{"ctrl+alt+f4":7}"#);
        assert_eq!(
            serde_json::from_str::<BTreeMap<Chord, u8>>(&wire).unwrap(),
            bound
        );
    }
}
