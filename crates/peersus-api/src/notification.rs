//! A notification: a short text the application shows a person for whoever sent it, with the
//! buttons the sender names. One shape for the application's own and for an extension's, so
//! what one can ask the other can.
//!
//! Where it is drawn, how large and in what order is the application's: a notification has no
//! size, position or colour. The sender says only what it says, how far it reaches, whether it
//! waits, and what the person may answer.

use crate::{Error, Result, name};
use serde::{Deserialize, Serialize};

/// Longest text of a notification, in characters. It is one paragraph: no line breaks.
pub const MAX_TEXT: usize = 500;
/// Longest words on a button, in characters.
pub const MAX_LABEL: usize = 24;
/// Most buttons on one notification.
pub const MAX_BUTTONS: usize = 3;

/// How far a notification reaches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reach {
    /// Only inside the application's window: about something the person is doing there.
    #[default]
    Inside,
    /// Also outside the window, through what the system offers, while the window is not in
    /// front: about something that happens whether the person looks or not.
    Outside,
}

/// One answer the person may give: `id` is what the sender is told, `label` the words on it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Button {
    pub id: String,
    pub label: String,
}

/// What a sender asks the application to show.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Notification {
    pub text: String,
    #[serde(default)]
    pub reach: Reach,
    /// Whether it stays until the person answers or closes it. One that does not stay goes by
    /// itself after the application's own time.
    #[serde(default)]
    pub stays: bool,
    /// The answers offered, in the order shown. None: it only says something.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub buttons: Vec<Button>,
}

impl Notification {
    /// A text that says something and goes by itself, inside the window.
    pub fn saying(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            reach: Reach::Inside,
            stays: false,
            buttons: Vec::new(),
        }
    }

    /// Whether it waits for the person: it was asked to stay, or it offers an answer, which
    /// nobody could give to something that left.
    pub fn waits(&self) -> bool {
        self.stays || !self.buttons.is_empty()
    }

    /// Refuses what cannot be shown as it is: nothing is cut or repaired on the way.
    pub fn validate(&self) -> Result<()> {
        let buttons_fit = self.buttons.len() <= MAX_BUTTONS
            && self.buttons.iter().enumerate().all(|(at, button)| {
                name::valid(&button.id)
                    && name::text(&button.label, MAX_LABEL)
                    && self.buttons[..at]
                        .iter()
                        .all(|before| before.id != button.id)
            });
        if name::text(&self.text, MAX_TEXT) && buttons_fit {
            Ok(())
        } else {
            Err(Error::InvalidPayload)
        }
    }
}

/// What became of a notification that waited, as its sender is told.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "answer", content = "button", rename_all = "kebab-case")]
pub enum Answer {
    /// The person pressed the button of this id.
    Pressed(String),
    /// The person closed it without choosing.
    Dismissed,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn button(id: &str, label: &str) -> Button {
        Button {
            id: id.into(),
            label: label.into(),
        }
    }

    #[test]
    fn a_sender_names_its_own_buttons_and_the_shape_is_bounded() {
        let asked = Notification {
            text: "Trimit fișierul?".into(),
            reach: Reach::Outside,
            stays: false,
            buttons: vec![button("send", "Trimite"), button("keep", "Păstrează-l")],
        };
        assert_eq!(asked.validate(), Ok(()));
        let widest = Notification {
            text: "é".repeat(MAX_TEXT),
            buttons: (0..MAX_BUTTONS)
                .map(|n| button(&format!("b{n}"), &"é".repeat(MAX_LABEL)))
                .collect(),
            ..Notification::saying("")
        };
        assert_eq!(widest.validate(), Ok(()));

        let refused = |change: fn(&mut Notification)| {
            let mut bad = Notification {
                buttons: vec![button("yes", "Yes"), button("no", "No")],
                ..Notification::saying("Question")
            };
            change(&mut bad);
            bad.validate()
        };
        for change in [
            (|n| n.text = "  ".into()) as fn(&mut Notification),
            |n| n.text = "line\nbreak".into(),
            |n| n.text = "x".repeat(MAX_TEXT + 1),
            |n| n.buttons[0].label = String::new(),
            |n| n.buttons[0].label = "x".repeat(MAX_LABEL + 1),
            |n| n.buttons[0].label = "tab\there".into(),
            |n| n.buttons[0].id = String::new(),
            |n| n.buttons[0].id = "two words".into(),
            |n| n.buttons[1].id = "yes".into(),
            |n| n.buttons = vec![button("a", "A"); 2],
            |n| {
                n.buttons = (0..=MAX_BUTTONS)
                    .map(|i| button(&format!("b{i}"), "B"))
                    .collect();
            },
        ] {
            assert_eq!(refused(change), Err(Error::InvalidPayload));
        }
    }

    #[test]
    fn what_offers_an_answer_waits_for_it() {
        let says = Notification::saying("Done");
        assert!(!says.waits());
        assert!(
            Notification {
                stays: true,
                ..says.clone()
            }
            .waits()
        );
        assert!(
            Notification {
                buttons: vec![button("ok", "OK")],
                ..says
            }
            .waits()
        );
    }

    #[test]
    fn the_least_a_sender_writes_is_the_text_and_unknown_fields_are_refused() {
        let least: Notification = serde_json::from_str(r#"{"text":"Hello"}"#).unwrap();
        assert_eq!(least, Notification::saying("Hello"));
        assert_eq!(
            serde_json::to_string(&least).unwrap(),
            r#"{"text":"Hello","reach":"inside","stays":false}"#
        );
        let full: Notification = serde_json::from_str(
            r#"{"text":"Q","reach":"outside","stays":true,"buttons":[{"id":"a","label":"A"}]}"#,
        )
        .unwrap();
        assert_eq!(full.reach, Reach::Outside);
        assert_eq!(full.buttons, [button("a", "A")]);
        for bad in [
            r#"{}"#,
            r#"{"text":"Q","reach":"everywhere"}"#,
            r#"{"text":"Q","width":300}"#,
            r#"{"text":"Q","buttons":[{"id":"a","label":"A","action":"x"}]}"#,
            r#"{"text":"Q","buttons":[{"id":"a"}]}"#,
        ] {
            assert!(serde_json::from_str::<Notification>(bad).is_err(), "{bad}");
        }
        assert_eq!(
            serde_json::to_string(&Answer::Pressed("send".into())).unwrap(),
            r#"{"answer":"pressed","button":"send"}"#
        );
        assert_eq!(
            serde_json::to_string(&Answer::Dismissed).unwrap(),
            r#"{"answer":"dismissed"}"#
        );
    }
}
