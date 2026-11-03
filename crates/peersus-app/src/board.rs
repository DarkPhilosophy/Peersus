//! What became of each action a user started, for the screen to show beside its button.
//!
//! One entry per plugin, action and device: the latest state and nothing older. It holds
//! only what the application itself decided to say (`ActionState` is a closed vocabulary), so
//! nothing a plugin wrote can sit in here. It is bounded, and the bound never throws away an
//! action that is still running, because a screen that stopped showing "running" for
//! something that is running would be telling the user something false.
use crate::in_flight::Key;
use peersus_core::model::{ActionState, ActionStatus};

/// Most actions remembered at once. Past this the oldest finished one is forgotten.
pub const MAX_BOARD: usize = 32;

/// The latest state of each action, oldest first.
#[derive(Debug, Default)]
pub struct Board {
    entries: Vec<(Key, ActionState)>,
}

impl Board {
    /// The action was sent and has not been answered.
    pub fn running(&mut self, key: Key) {
        self.set(key, ActionState::Running);
    }

    /// The action ended. Recorded only for an action the board knows: an answer to something
    /// that was withdrawn, or to a call of a plugin that was restarted, has nothing to update.
    pub fn finished(&mut self, key: &Key, state: ActionState) {
        if let Some(entry) = self.entries.iter_mut().find(|(k, _)| k == key) {
            entry.1 = state;
        }
    }

    /// Forget every action of `plugin`, as when its process is restarted: whatever it was
    /// doing is gone, and "running" would no longer be true of any of it.
    pub fn forget_plugin(&mut self, plugin: &peersus_core::plugins::PluginId) {
        self.entries.retain(|(key, _)| &key.plugin != plugin);
    }

    /// Forget what is recorded for `device`, for one plugin or (with `None`) for every plugin,
    /// as when a permission is withdrawn or the device is forgotten. What was started for it
    /// is withdrawn and will never be answered, so leaving it recorded would show "running"
    /// for something that is not.
    pub fn forget_for(
        &mut self,
        device: &peersus_core::model::PeerId,
        plugin: Option<&peersus_core::plugins::PluginId>,
    ) {
        self.entries.retain(|(key, _)| {
            let hit = key.device.as_ref() == Some(device)
                && plugin.is_none_or(|plugin| plugin == &key.plugin);
            !hit
        });
    }

    /// Every action's latest state, oldest first, in the shape the screen is given.
    pub fn statuses(&self) -> Vec<ActionStatus> {
        self.entries
            .iter()
            .map(|(key, state)| ActionStatus {
                plugin: key.plugin.clone(),
                action: key.action.clone(),
                peer: key.device.clone(),
                state: *state,
            })
            .collect()
    }

    fn set(&mut self, key: Key, state: ActionState) {
        // Running again replaces what was recorded and counts as the newest.
        self.entries.retain(|(k, _)| k != &key);
        if self.entries.len() >= MAX_BOARD {
            // Make room by forgetting the oldest FINISHED action. If every entry is still
            // running there is nothing honest to forget, and the table may exceed the bound
            // by what is running, which `InFlight` already limits.
            if let Some(oldest) = self
                .entries
                .iter()
                .position(|(_, state)| *state != ActionState::Running)
            {
                self.entries.remove(oldest);
            }
        }
        self.entries.push((key, state));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peersus_core::{
        model::{ActionFailure, PeerId},
        plugins::PluginId,
    };

    fn key(plugin: &str, action: &str, device: Option<&str>) -> Key {
        Key {
            plugin: PluginId::new(plugin).unwrap(),
            action: action.into(),
            device: device.map(|d| PeerId(d.into())),
        }
    }
    const DONE: ActionState = ActionState::Done;
    const FAILED: ActionState = ActionState::Failed {
        reason: ActionFailure::Other,
    };

    fn states(board: &Board) -> Vec<(String, ActionState)> {
        board
            .statuses()
            .into_iter()
            .map(|s| (format!("{}/{}", s.plugin.as_str(), s.action), s.state))
            .collect()
    }

    #[test]
    fn an_action_is_running_until_it_is_answered_and_then_shows_how_it_ended() {
        let mut board = Board::default();
        board.running(key("a", "share", Some("p1")));
        assert_eq!(
            states(&board),
            [("a/share".to_owned(), ActionState::Running)]
        );
        board.finished(&key("a", "share", Some("p1")), DONE);
        assert_eq!(states(&board), [("a/share".to_owned(), DONE)]);
    }

    #[test]
    fn only_the_latest_state_of_an_action_is_kept_and_it_counts_as_the_newest() {
        let mut board = Board::default();
        board.running(key("a", "one", None));
        board.running(key("a", "two", None));
        board.finished(&key("a", "one", None), FAILED);
        // Run `one` again: it replaces its failure and moves to the end.
        board.running(key("a", "one", None));
        assert_eq!(
            states(&board),
            [
                ("a/two".to_owned(), ActionState::Running),
                ("a/one".to_owned(), ActionState::Running)
            ]
        );
    }

    #[test]
    fn the_same_action_for_two_devices_is_two_entries() {
        let mut board = Board::default();
        board.running(key("a", "share", Some("p1")));
        board.running(key("a", "share", Some("p2")));
        board.finished(&key("a", "share", Some("p2")), DONE);
        let all = board.statuses();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].state, ActionState::Running, "p1 is untouched");
        assert_eq!(all[1].state, DONE);
    }

    #[test]
    fn an_answer_to_an_action_the_board_never_knew_adds_nothing() {
        let mut board = Board::default();
        board.finished(&key("a", "ghost", None), DONE);
        assert!(board.statuses().is_empty());
    }

    #[test]
    fn the_board_is_bounded_and_forgets_the_oldest_finished_action_first() {
        let mut board = Board::default();
        for n in 0..MAX_BOARD {
            let k = key("a", &format!("act{n}"), None);
            board.running(k.clone());
            board.finished(&k, DONE);
        }
        assert_eq!(board.statuses().len(), MAX_BOARD);
        board.running(key("a", "newest", None));
        let all = board.statuses();
        assert_eq!(all.len(), MAX_BOARD);
        assert!(all.iter().all(|s| s.action != "act0"), "the oldest went");
        assert_eq!(all.last().unwrap().action, "newest");
    }

    #[test]
    fn making_room_never_forgets_an_action_that_is_still_running() {
        let mut board = Board::default();
        // The OLDEST entry is the one still running, then finished ones after it.
        board.running(key("a", "long", None));
        for n in 1..MAX_BOARD {
            let k = key("a", &format!("act{n}"), None);
            board.running(k.clone());
            board.finished(&k, DONE);
        }
        board.running(key("a", "newest", None));
        let all = board.statuses();
        assert!(
            all.iter().any(|s| s.action == "long"),
            "a running action was thrown away to make room"
        );
        assert!(
            all.iter().all(|s| s.action != "act1"),
            "the oldest FINISHED went"
        );
    }

    #[test]
    fn restarting_a_plugin_forgets_its_actions_and_only_its_actions() {
        let mut board = Board::default();
        board.running(key("a", "share", Some("p1")));
        board.running(key("b", "share", Some("p1")));
        board.forget_plugin(&PluginId::new("a").unwrap());
        let all = board.statuses();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].plugin.as_str(), "b");
    }

    #[test]
    fn forgetting_a_device_forgets_what_was_recorded_for_it_and_only_that() {
        let mut board = Board::default();
        board.running(key("a", "share", Some("p1")));
        board.running(key("a", "share", Some("p2")));
        board.running(key("a", "status", None));
        board.forget_for(&PeerId("p1".into()), None);
        let all = board.statuses();
        assert_eq!(all.len(), 2);
        assert!(all.iter().all(|s| s.peer != Some(PeerId("p1".into()))));
    }

    #[test]
    fn withdrawing_one_plugin_for_a_device_forgets_only_that_pair() {
        let mut board = Board::default();
        board.running(key("a", "share", Some("p1")));
        board.running(key("b", "share", Some("p1")));
        board.running(key("a", "share", Some("p2")));
        board.forget_for(&PeerId("p1".into()), Some(&PluginId::new("a").unwrap()));
        let left: Vec<(String, Option<String>)> = board
            .statuses()
            .into_iter()
            .map(|s| (s.plugin.as_str().to_owned(), s.peer.map(|p| p.0)))
            .collect();
        assert_eq!(
            left,
            [
                ("b".to_owned(), Some("p1".to_owned())),
                ("a".to_owned(), Some("p2".to_owned()))
            ]
        );
    }

    #[test]
    fn withdrawing_with_no_plugin_named_forgets_the_device_for_every_plugin() {
        let mut board = Board::default();
        board.running(key("a", "share", Some("p1")));
        board.running(key("b", "share", Some("p1")));
        board.running(key("a", "status", None));
        board.forget_for(&PeerId("p1".into()), None);
        assert_eq!(board.statuses().len(), 1);
        assert_eq!(board.statuses()[0].action, "status");
    }
}
