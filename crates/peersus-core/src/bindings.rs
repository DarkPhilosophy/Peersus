//! Key bindings: a person's chords on an action that already exists. A chord has one meaning; an action has up to
//! [`MAX_PER_TARGET`] chords (for an action that needs a device, that many per device). What can be bound is checked by the owner
//! against what is installed and paired; this module only keeps the map's own rules, which hold whatever is installed.

use crate::{peer::PeerId, plugins::PluginId};
use peersus_api::chord::Chord;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The application's own actions a chord can run. Each is what its button in the window does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AppAction {
    /// Accept the newest waiting offer of files.
    AcceptOffer,
    /// Decline the newest waiting offer of files.
    DeclineOffer,
    /// Show the Send page.
    ShowSend,
    /// Show the Receive page.
    ShowReceive,
}

impl AppAction {
    /// Every one, in the order Settings lists them.
    pub const ALL: [Self; 4] = [
        Self::AcceptOffer,
        Self::DeclineOffer,
        Self::ShowSend,
        Self::ShowReceive,
    ];

    /// Its English words, a message id of the window's table.
    pub const fn label(self) -> &'static str {
        match self {
            Self::AcceptOffer => "Accept incoming files",
            Self::DeclineOffer => "Decline incoming files",
            Self::ShowSend => "Show Send",
            Self::ShowReceive => "Show Receive",
        }
    }

    /// What it does, in a sentence, under its label on the Binds page: an English message id of the window's table.
    pub const fn help(self) -> &'static str {
        match self {
            Self::AcceptOffer => {
                "Accepts the newest offer of files waiting for you, saved where your rule for received files says."
            }
            Self::DeclineOffer => "Declines the newest offer of files waiting for you.",
            Self::ShowSend => "Opens the Send page, to choose files and the device they go to.",
            Self::ShowReceive => {
                "Opens the Receive page, with its pairing code and the files received."
            }
        }
    }
}

/// What a chord runs.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum BindTarget {
    /// One of the application's own actions.
    App { action: AppAction },
    /// An extension's action, run as its button runs it; `peer` for an action that needs a device.
    Plugin {
        plugin: PluginId,
        action: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        peer: Option<PeerId>,
    },
}

impl BindTarget {
    fn is_plugin(&self, id: &PluginId) -> bool {
        matches!(self, Self::Plugin { plugin, .. } if plugin == id)
    }

    fn is_for_peer(&self, id: &PeerId) -> bool {
        matches!(self, Self::Plugin { peer: Some(peer), .. } if peer == id)
    }
}

/// The person's bindings: chord → what it runs.
pub type Bindings = BTreeMap<Chord, BindTarget>;

/// The most chords one action (for one device) can have.
pub const MAX_PER_TARGET: usize = 3;

/// Why a chord was not bound.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The chord already runs something else: this.
    Taken(BindTarget),
    /// The action has [`MAX_PER_TARGET`] chords already.
    Full,
}

/// Add `chord` to the chords of `target`. Refused when the chord already runs something else, or the target has as many chords as
/// it may; nothing changes then. A chord the target has already is no change.
pub fn bind(bindings: &mut Bindings, chord: Chord, target: BindTarget) -> Result<(), Refused> {
    match bindings.get(&chord) {
        Some(held) if *held == target => return Ok(()),
        Some(held) => return Err(Refused::Taken(held.clone())),
        None => {}
    }
    if chords_of(bindings, &target).count() >= MAX_PER_TARGET {
        return Err(Refused::Full);
    }
    bindings.insert(chord, target);
    Ok(())
}

/// The chords of `target`, in the map's order.
pub fn chords_of<'a>(
    bindings: &'a Bindings,
    target: &'a BindTarget,
) -> impl Iterator<Item = Chord> + 'a {
    bindings
        .iter()
        .filter(move |(_, held)| *held == target)
        .map(|(chord, _)| *chord)
}

/// Whether every target of a whole map keeps to [`MAX_PER_TARGET`]: what the owner checks of a map the window sends. (A chord
/// with two meanings cannot be written in a map at all.)
pub fn within_limits(bindings: &Bindings) -> bool {
    let mut counts: BTreeMap<&BindTarget, usize> = BTreeMap::new();
    bindings.values().all(|target| {
        let count = counts.entry(target).or_default();
        *count += 1;
        *count <= MAX_PER_TARGET
    })
}

/// Bind each chord a newly installed extension suggests to its action (one that needs no device), where the chord is free and the
/// action has room: what an extension suggests never takes a key from anything, and gives way where the key is taken.
pub fn bind_suggested(
    bindings: &mut Bindings,
    plugin: &PluginId,
    suggested: impl IntoIterator<Item = (String, Chord)>,
) -> bool {
    let mut changed = false;
    for (action, chord) in suggested {
        let target = BindTarget::Plugin {
            plugin: plugin.clone(),
            action,
            peer: None,
        };
        changed |= !bindings.contains_key(&chord) && bind(bindings, chord, target).is_ok();
    }
    changed
}

/// Every binding of an extension that is removed goes with it.
pub fn forget_plugin(bindings: &mut Bindings, plugin: &PluginId) -> bool {
    let before = bindings.len();
    bindings.retain(|_, target| !target.is_plugin(plugin));
    bindings.len() != before
}

/// Every binding for a device that is forgotten goes with it.
pub fn forget_peer(bindings: &mut Bindings, peer: &PeerId) -> bool {
    let before = bindings.len();
    bindings.retain(|_, target| !target.is_for_peer(peer));
    bindings.len() != before
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(text: &str) -> Chord {
        text.parse().unwrap()
    }

    fn plugin(id: &str, action: &str, peer: Option<PeerId>) -> BindTarget {
        BindTarget::Plugin {
            plugin: PluginId::new(id).unwrap(),
            action: action.into(),
            peer,
        }
    }

    #[test]
    fn a_chord_has_one_meaning_and_an_action_up_to_three_chords() {
        let mut bindings = Bindings::new();
        let accept = BindTarget::App {
            action: AppAction::AcceptOffer,
        };
        assert_eq!(
            bind(&mut bindings, chord("ctrl+alt+a"), accept.clone()),
            Ok(())
        );
        // The same again changes nothing.
        assert_eq!(
            bind(&mut bindings, chord("ctrl+alt+a"), accept.clone()),
            Ok(())
        );
        // Another action on a held chord is refused with what holds it, and nothing changes.
        let go = plugin("control", "go", None);
        assert_eq!(
            bind(&mut bindings, chord("ctrl+alt+a"), go.clone()),
            Err(Refused::Taken(accept.clone()))
        );
        assert_eq!(bindings.len(), 1);
        // More chords for one action are added beside the first, up to three; a fourth is refused.
        for more in ["ctrl+alt+b", "ctrl+alt+c"] {
            assert_eq!(bind(&mut bindings, chord(more), accept.clone()), Ok(()));
        }
        assert_eq!(
            bind(&mut bindings, chord("ctrl+alt+d"), accept.clone()),
            Err(Refused::Full)
        );
        assert_eq!(
            chords_of(&bindings, &accept).collect::<Vec<_>>(),
            [
                chord("ctrl+alt+a"),
                chord("ctrl+alt+b"),
                chord("ctrl+alt+c")
            ]
        );
        assert!(within_limits(&bindings));
        // Another action is not counted against it.
        assert_eq!(bind(&mut bindings, chord("ctrl+alt+d"), go.clone()), Ok(()));
        // A whole map with a fourth chord for one action is outside the limits.
        bindings.insert(chord("ctrl+alt+e"), accept);
        assert!(!within_limits(&bindings));
    }

    #[test]
    fn a_device_action_has_one_chord_per_device_and_goes_with_its_device_or_extension() {
        let one = PeerId("one".into());
        let two = PeerId("two".into());
        let mut bindings = Bindings::new();
        bind(
            &mut bindings,
            chord("ctrl+alt+1"),
            plugin("control", "take", Some(one.clone())),
        )
        .unwrap();
        bind(
            &mut bindings,
            chord("ctrl+alt+2"),
            plugin("control", "take", Some(two.clone())),
        )
        .unwrap();
        bind(
            &mut bindings,
            chord("ctrl+alt+s"),
            BindTarget::App {
                action: AppAction::ShowSend,
            },
        )
        .unwrap();
        assert_eq!(bindings.len(), 3);
        assert!(forget_peer(&mut bindings, &one));
        assert!(!bindings.contains_key(&chord("ctrl+alt+1")));
        assert!(!forget_peer(&mut bindings, &one));
        assert!(forget_plugin(
            &mut bindings,
            &PluginId::new("control").unwrap()
        ));
        assert_eq!(
            bindings.keys().copied().collect::<Vec<_>>(),
            [chord("ctrl+alt+s")]
        );
    }

    #[test]
    fn an_extensions_suggested_key_is_bound_only_where_it_is_free() {
        let mut bindings = Bindings::new();
        let send = BindTarget::App {
            action: AppAction::ShowSend,
        };
        bind(&mut bindings, chord("ctrl+alt+s"), send.clone()).unwrap();
        let test = PluginId::new("test").unwrap();
        let suggested = [
            ("count".to_owned(), chord("ctrl+alt+k")),
            ("taken".to_owned(), chord("ctrl+alt+s")),
        ];
        assert!(bind_suggested(&mut bindings, &test, suggested.clone()));
        assert_eq!(
            bindings.get(&chord("ctrl+alt+k")),
            Some(&plugin("test", "count", None))
        );
        // The key something else held stays with it: the extension gives way.
        assert_eq!(bindings.get(&chord("ctrl+alt+s")), Some(&send));
        // Again, nothing changes.
        assert!(!bind_suggested(&mut bindings, &test, suggested));
    }

    #[test]
    fn bindings_cross_as_a_map_keyed_by_the_written_chord() {
        let mut bindings = Bindings::new();
        bind(
            &mut bindings,
            chord("ctrl+shift+f1"),
            BindTarget::App {
                action: AppAction::ShowReceive,
            },
        )
        .unwrap();
        bind(
            &mut bindings,
            chord("alt+3"),
            plugin("control", "take", None),
        )
        .unwrap();
        let text = serde_json::to_string(&bindings).unwrap();
        assert_eq!(
            text,
            r#"{"alt+3":{"kind":"plugin","plugin":"control","action":"take"},"ctrl+shift+f1":{"kind":"app","action":"show-receive"}}"#
        );
        assert_eq!(serde_json::from_str::<Bindings>(&text).unwrap(), bindings);
        // What is not understood is refused, not dropped.
        assert!(
            serde_json::from_str::<Bindings>(
                r#"{"alt+3":{"kind":"app","action":"show-send","extra":1}}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<Bindings>(r#"{"shift+3":{"kind":"app","action":"show-send"}}"#)
                .is_err()
        );
    }
}
