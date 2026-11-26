//! The statuses that plugins define for this device and which of them are on now.
//!
//! The application knows no status. A plugin creates the ones it wants while it runs (`define`), says which are true of this device
//! right now (`set`, `clear`), and the application says the whole of it to every paired device. What one status IS, is only ever the
//! pair of the plugin that owns it and the identifier that plugin gave it: that pair is how a status is named here, on the wire, on
//! the screen of another device and in what another plugin reads, and no step in between drops either half.
//!
//! Everything here belongs to a plugin PROCESS. When the plugin stops or starts again, what it defined and what it had on go with it
//! (`forget_plugin`): a status that nobody keeps true any more must not stay on the screen of another device.
use peersus_core::plugins::{
    MAX_ACTIVE_STATUSES, MAX_STATUSES_PER_PLUGIN, PluginId, PluginStateError, StatusId, StatusInfo,
    StatusKey, StatusLabel,
};
use std::collections::BTreeMap;

/// Why a plugin was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// The identifier or the label is not acceptable: not plain text, too long, or one of the application's own words.
    Invalid,
    /// The plugin already has as many statuses as it may have.
    TooManyDefined,
    /// The plugin never defined this one. A status can only be set after its owner created it, and only by that owner.
    Undefined,
    /// More statuses would be on at once than a device may say.
    TooManyActive,
}

impl From<PluginStateError> for Refused {
    fn from(_: PluginStateError) -> Self {
        Self::Invalid
    }
}

#[derive(Debug, Default)]
pub struct Statuses {
    /// What each plugin defined: its identifier and the words its author gave it.
    defined: BTreeMap<StatusKey, StatusLabel>,
    /// Which are on now, in the order they were turned on.
    on: Vec<StatusKey>,
}

impl Statuses {
    /// Create a status of `plugin`, or give an existing one new words. Defining again changes the words and nothing else: it is not
    /// turned on or off by it.
    pub fn define(
        &mut self,
        plugin: &PluginId,
        id: &str,
        label: &str,
    ) -> Result<StatusInfo, Refused> {
        let key = StatusKey {
            plugin: plugin.clone(),
            id: StatusId::new(id)?,
        };
        let label = StatusLabel::new(label)?;
        let owned = self
            .defined
            .keys()
            .filter(|known| &known.plugin == plugin)
            .count();
        if !self.defined.contains_key(&key) && owned >= MAX_STATUSES_PER_PLUGIN {
            return Err(Refused::TooManyDefined);
        }
        self.defined.insert(key.clone(), label.clone());
        Ok(StatusInfo { key, label })
    }

    /// Turn a status of `plugin` on. Only one it defined, and only up to what a device may say at once. Turning on what is on changes
    /// nothing.
    pub fn set(&mut self, plugin: &PluginId, id: &str) -> Result<(), Refused> {
        let key = self.owned(plugin, id)?;
        if self.on.contains(&key) {
            return Ok(());
        }
        if self.on.len() >= MAX_ACTIVE_STATUSES {
            return Err(Refused::TooManyActive);
        }
        self.on.push(key);
        Ok(())
    }

    /// Turn a status of `plugin` off. Turning off what is off changes nothing.
    pub fn clear(&mut self, plugin: &PluginId, id: &str) -> Result<(), Refused> {
        let key = self.owned(plugin, id)?;
        self.on.retain(|on| on != &key);
        Ok(())
    }

    /// The key of a status that `plugin` itself defined: never one of another plugin's, whatever identifier is given.
    fn owned(&self, plugin: &PluginId, id: &str) -> Result<StatusKey, Refused> {
        let key = StatusKey {
            plugin: plugin.clone(),
            id: StatusId::new(id)?,
        };
        self.defined
            .contains_key(&key)
            .then_some(key)
            .ok_or(Refused::Undefined)
    }

    /// Everything on now, with whose it is and the words of its author: what this device says of itself.
    pub fn active(&self) -> Vec<StatusInfo> {
        self.on
            .iter()
            .filter_map(|key| {
                self.defined.get(key).map(|label| StatusInfo {
                    key: key.clone(),
                    label: label.clone(),
                })
            })
            .collect()
    }

    /// Everything `plugin` defined, on or not.
    pub fn defined_by(&self, plugin: &PluginId) -> Vec<StatusInfo> {
        self.defined
            .iter()
            .filter(|(key, _)| &key.plugin == plugin)
            .map(|(key, label)| StatusInfo {
                key: key.clone(),
                label: label.clone(),
            })
            .collect()
    }

    /// What a plugin defined and had on goes with its process. Returns whether the set this device says changed.
    pub fn forget_plugin(&mut self, plugin: &PluginId) -> bool {
        let before = self.on.len();
        self.on.retain(|key| &key.plugin != plugin);
        self.defined.retain(|key, _| &key.plugin != plugin);
        self.on.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(id: &str) -> PluginId {
        PluginId::new(id).unwrap()
    }
    fn ids(list: &[StatusInfo]) -> Vec<(String, String, String)> {
        list.iter()
            .map(|s| {
                (
                    s.key.plugin.as_str().into(),
                    s.key.id.as_str().into(),
                    s.label.as_str().into(),
                )
            })
            .collect()
    }

    #[test]
    fn a_plugin_creates_its_own_status_with_its_own_words_and_turns_it_on_and_off() {
        let mut statuses = Statuses::default();
        let player = plugin("player");
        let made = statuses
            .define(&player, "watching", "Watching a video")
            .unwrap();
        assert_eq!(
            (made.key.id.as_str(), made.label.as_str()),
            ("watching", "Watching a video")
        );
        // Created is not on: nothing is said of the device until the plugin says it is true.
        assert!(statuses.active().is_empty());
        statuses.set(&player, "watching").unwrap();
        assert_eq!(
            ids(&statuses.active()),
            [(
                "player".into(),
                "watching".into(),
                "Watching a video".into()
            )]
        );
        statuses.clear(&player, "watching").unwrap();
        assert!(statuses.active().is_empty());
        // It is still defined: turning it off is not forgetting it.
        statuses.set(&player, "watching").unwrap();
    }

    #[test]
    fn a_plugin_can_only_turn_on_what_it_created_and_never_what_another_plugin_created() {
        let mut statuses = Statuses::default();
        statuses
            .define(&plugin("player"), "watching", "Watching")
            .unwrap();
        // Not defined at all, and defined by someone else: both are refused, and neither is told apart from the other.
        assert_eq!(
            statuses.set(&plugin("player"), "missing"),
            Err(Refused::Undefined)
        );
        assert_eq!(
            statuses.set(&plugin("intruder"), "watching"),
            Err(Refused::Undefined)
        );
        assert_eq!(
            statuses.clear(&plugin("intruder"), "watching"),
            Err(Refused::Undefined)
        );
        assert!(statuses.active().is_empty());
        // And the owner of a status is the one the HOST gave: the same identifier from two plugins is two different statuses.
        statuses
            .define(&plugin("timer"), "watching", "A timer")
            .unwrap();
        statuses.set(&plugin("timer"), "watching").unwrap();
        assert_eq!(
            ids(&statuses.active()),
            [("timer".into(), "watching".into(), "A timer".into())]
        );
    }

    #[test]
    fn the_applications_own_words_are_never_a_status_whatever_way_they_are_written() {
        let mut statuses = Statuses::default();
        let player = plugin("player");
        for system in ["online", "sleeping", "offline", "Offline", "ONLINE"] {
            assert_eq!(
                statuses.define(&player, system, "Fine"),
                Err(Refused::Invalid),
                "id {system}"
            );
            assert_eq!(
                statuses.define(&player, "ok", system),
                Err(Refused::Invalid),
                "label {system}"
            );
        }
        assert!(statuses.defined_by(&player).is_empty());
    }

    #[test]
    fn text_that_hides_or_redirects_or_is_too_long_is_refused_and_creates_nothing() {
        let mut statuses = Statuses::default();
        let player = plugin("player");
        for label in [
            "",
            "  ",
            "a\u{202E}b",
            "zero\u{200B}width",
            "two\nlines",
            &"x".repeat(81),
        ] {
            assert_eq!(
                statuses.define(&player, "ok", label),
                Err(Refused::Invalid),
                "{label:?}"
            );
        }
        for id in ["", "a b", "é", "a:b", &"x".repeat(129)] {
            assert_eq!(
                statuses.define(&player, id, "Fine"),
                Err(Refused::Invalid),
                "{id:?}"
            );
        }
        assert!(
            statuses.defined_by(&player).is_empty(),
            "a refused one still created something"
        );
    }

    #[test]
    fn a_plugin_has_a_limited_number_of_statuses_but_may_reword_one_it_has() {
        let mut statuses = Statuses::default();
        let player = plugin("player");
        for i in 0..MAX_STATUSES_PER_PLUGIN {
            statuses.define(&player, &format!("s{i}"), "x").unwrap();
        }
        assert_eq!(
            statuses.define(&player, "one-more", "x"),
            Err(Refused::TooManyDefined)
        );
        // Giving new words to one it already has is not one more, and it does not turn it on or off.
        statuses.set(&player, "s0").unwrap();
        statuses.define(&player, "s0", "New words").unwrap();
        assert_eq!(statuses.active()[0].label.as_str(), "New words");
        assert_eq!(statuses.defined_by(&player).len(), MAX_STATUSES_PER_PLUGIN);
        // The limit is each plugin's own: another plugin is not held back by this one.
        statuses.define(&plugin("other"), "free", "x").unwrap();
    }

    #[test]
    fn only_so_many_may_be_on_at_once_whoever_owns_them_and_the_refusal_changes_nothing() {
        let mut statuses = Statuses::default();
        for i in 0..MAX_ACTIVE_STATUSES {
            let owner = plugin(&format!("p{i}"));
            statuses.define(&owner, "on", "x").unwrap();
            statuses.set(&owner, "on").unwrap();
        }
        statuses.define(&plugin("late"), "on", "x").unwrap();
        assert_eq!(
            statuses.set(&plugin("late"), "on"),
            Err(Refused::TooManyActive)
        );
        assert_eq!(statuses.active().len(), MAX_ACTIVE_STATUSES);
        // Turning on what is on is not one more.
        statuses.set(&plugin("p0"), "on").unwrap();
        assert_eq!(statuses.active().len(), MAX_ACTIVE_STATUSES);
    }

    #[test]
    fn what_a_plugin_defined_and_had_on_goes_when_the_plugin_does_and_nobody_elses_does() {
        let mut statuses = Statuses::default();
        for name in ["gone", "stays"] {
            statuses.define(&plugin(name), "busy", "Busy").unwrap();
            statuses.set(&plugin(name), "busy").unwrap();
        }
        assert!(
            statuses.forget_plugin(&plugin("gone")),
            "it had one on, so what the device says changed"
        );
        assert_eq!(
            ids(&statuses.active()),
            [("stays".into(), "busy".into(), "Busy".into())]
        );
        assert!(statuses.defined_by(&plugin("gone")).is_empty());
        // A plugin that had nothing on changes nothing, and one that was never there is no error.
        assert!(!statuses.forget_plugin(&plugin("stays-not")));
        // What it had is no longer defined, so it cannot be turned on again without creating it again.
        assert_eq!(
            statuses.set(&plugin("gone"), "busy"),
            Err(Refused::Undefined)
        );
    }
}
