//! Bindings that work while the window is not in front (Linux), through the desktop's GlobalShortcuts portal. The desktop
//! decides: it shows the person its own dialog to approve the keys, may give a key other than the one asked for, and tells
//! which binding was pressed. Only the person's switch on the Binds page starts this; the window's own keys work without it.
//!
//! A binding is known to the desktop by its chord written as text (`ctrl+alt+1`), so what comes back is read as the chord the
//! window already looks up. The portal runs on a thread of its own: a desktop that is slow to answer never holds the window.
#![cfg(target_os = "linux")]

use ashpd::desktop::{
    CreateSessionOptions, Session,
    global_shortcuts::{BindShortcutsOptions, GlobalShortcuts, NewShortcut},
};
use futures_lite::StreamExt;
use peersus_api::chord::{Chord, ChordKey};
use tokio::sync::mpsc;

/// One binding asked of the desktop: its chord and the words its dialog shows for it.
pub type Asked = Vec<(Chord, String)>;

/// The portal while it runs; dropping it ends the session and the desktop forgets the keys.
pub struct Outside {
    orders: mpsc::UnboundedSender<Asked>,
}

impl Outside {
    /// Start asking the desktop; every press of a binding it approved comes out of `heard` as its chord.
    pub fn start(heard: mpsc::UnboundedSender<Chord>) -> Self {
        let (orders, receive) = mpsc::unbounded_channel();
        std::thread::Builder::new()
            .name("peersus-outside-keys".into())
            .spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                runtime.block_on(async move {
                    if let Err(error) = run(receive, heard).await {
                        tracing::warn!(%error, "the desktop's shortcuts portal could not be used");
                    }
                });
            })
            .ok();
        Self { orders }
    }

    /// The bindings to ask for now; the ones asked before and missing here are given back.
    pub fn ask(&self, bindings: Asked) {
        let _ = self.orders.send(bindings);
    }
}

async fn run(
    mut orders: mpsc::UnboundedReceiver<Asked>,
    heard: mpsc::UnboundedSender<Chord>,
) -> Result<(), ashpd::Error> {
    // A program that is not in a sandbox is known to the portal by the id its desktop entry and window carry.
    if let Ok(id) = crate::APPLICATION_ID.parse() {
        let _ = ashpd::register_host_app(id).await;
    }
    let portal = GlobalShortcuts::new().await?;
    let mut activated = portal.receive_activated().await?;
    let mut session: Option<Session<GlobalShortcuts>> = None;
    let mut asked = Asked::new();
    loop {
        tokio::select! {
            order = orders.recv() => {
                let Some(bindings) = order else { break };
                if bindings == asked {
                    continue;
                }
                // A new session for each list: the desktop is asked for exactly these keys, and approves again only what is new.
                if let Some(old) = session.take() {
                    let _ = old.close().await;
                }
                asked = bindings;
                if asked.is_empty() {
                    continue;
                }
                let new = portal.create_session(CreateSessionOptions::default()).await?;
                let shortcuts: Vec<NewShortcut> = asked
                    .iter()
                    .map(|(chord, words)| {
                        NewShortcut::new(chord.to_string(), words.as_str())
                            .preferred_trigger(trigger(*chord).as_str())
                    })
                    .collect();
                match portal
                    .bind_shortcuts(&new, &shortcuts, None, BindShortcutsOptions::default())
                    .await
                    .and_then(|request| request.response())
                {
                    Ok(bound) => tracing::info!(
                        asked = asked.len(),
                        bound = bound.shortcuts().len(),
                        "the desktop answered for the outside keys"
                    ),
                    Err(error) => tracing::warn!(%error, "the desktop did not bind the outside keys"),
                }
                session = Some(new);
            }
            pressed = activated.next() => {
                let Some(pressed) = pressed else { break };
                if session.is_none() {
                    continue;
                }
                if let Ok(chord) = pressed.shortcut_id().parse::<Chord>()
                    && asked.iter().any(|(held, _)| *held == chord)
                {
                    let _ = heard.send(chord);
                }
            }
        }
    }
    if let Some(old) = session {
        let _ = old.close().await;
    }
    Ok(())
}

/// A chord as the shortcuts specification writes a trigger: its modifiers, then the key's XKB name.
fn trigger(chord: Chord) -> String {
    let held = chord.modifiers();
    let mut parts: Vec<String> = [
        (held.control, "CTRL"),
        (held.alt, "ALT"),
        (held.shift, "SHIFT"),
        (held.system, "LOGO"),
    ]
    .into_iter()
    .filter(|(on, _)| *on)
    .map(|(_, name)| name.to_owned())
    .collect();
    parts.push(match chord.key() {
        ChordKey::Letter(letter) => letter.to_string(),
        ChordKey::Digit(digit) => digit.to_string(),
        ChordKey::Function(number) => format!("F{number}"),
        ChordKey::Enter => "Return".to_owned(),
        ChordKey::Space => "space".to_owned(),
        ChordKey::Tab => "Tab".to_owned(),
        ChordKey::Escape => "Escape".to_owned(),
        ChordKey::Up => "Up".to_owned(),
        ChordKey::Down => "Down".to_owned(),
        ChordKey::Left => "Left".to_owned(),
        ChordKey::Right => "Right".to_owned(),
        ChordKey::Home => "Home".to_owned(),
        ChordKey::End => "End".to_owned(),
        ChordKey::Delete => "Delete".to_owned(),
    });
    parts.join("+")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(text: &str) -> String {
        trigger(text.parse().unwrap())
    }

    #[test]
    fn a_chord_is_asked_for_in_the_words_of_the_shortcuts_specification() {
        assert_eq!(written("ctrl+alt+1"), "CTRL+ALT+1");
        assert_eq!(written("shift+super+a"), "SHIFT+LOGO+a");
        assert_eq!(written("alt+f5"), "ALT+F5");
        assert_eq!(written("ctrl+enter"), "CTRL+Return");
        assert_eq!(written("ctrl+space"), "CTRL+space");
    }
}
