//! Window-thread drawing budget, independent of the renderer and its clock.

use peersus_core::plugins::PluginId;
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

/// Account for paint in consecutive one-second windows of the main thread's time.
pub(crate) const WINDOW: Duration = Duration::from_secs(1);
/// Twice the measured peak of the software map workload, rounded up for headroom.
pub(crate) const LIMIT: Duration = Duration::from_millis(500);
/// Require five complete expensive windows, not a short interaction or one slow frame.
pub(crate) const SUSTAIN_WINDOWS: u32 = 5;
/// An extension can try painting again after this long without painting its drawings.
const COOLDOWN: Duration = Duration::from_secs(10);
/// Pause until a person intervenes after this many pauses within the repeat window.
const REPEAT_COUNT: usize = 3;
const REPEAT_WITHIN: Duration = Duration::from_secs(2 * 60);
/// A product window cannot see more extensions than the owner can admit.
const MAX_TRACKED_PLUGINS: usize = peersus_app::discovery::MAX_PLUGINS;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Phase {
    #[default]
    Active,
    CoolingUntil(Instant),
    UntilResume,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Paint {
    Draw,
    Skip,
    JustPaused,
}

struct Window {
    start: Instant,
    /// Multiple callbacks in one frame all count in the same window, even if it was very slow.
    frame: u64,
    spent: Duration,
    over: u32,
}

#[derive(Default)]
struct Account {
    phase: Phase,
    window: Option<Window>,
    recent_pauses: [Option<Instant>; REPEAT_COUNT - 1],
}

impl Account {
    fn begin(&mut self, frame: u64, now: Instant) -> Paint {
        if self.phase != Phase::Active {
            return Paint::Skip;
        }
        let window = self.window.get_or_insert(Window {
            start: now,
            frame,
            spent: Duration::ZERO,
            over: 0,
        });
        if frame == window.frame {
            return Paint::Draw;
        }
        window.frame = frame;
        let age = now.saturating_duration_since(window.start);
        if age < WINDOW {
            return Paint::Draw;
        }

        if age < WINDOW * 2 {
            window.start += WINDOW;
        } else {
            // One delayed frame cannot supply multiple credits. Count it once if its paint
            // covered enough of the gap to leave a full second over budget; otherwise these
            // missing seconds break the streak.
            window.spent = window.spent.saturating_sub(age - WINDOW);
            window.start = now;
        }
        window.over = if window.spent > LIMIT {
            window.over + 1
        } else {
            0
        };
        window.spent = Duration::ZERO;
        if window.over < SUSTAIN_WINDOWS {
            return Paint::Draw;
        }

        let repeated = self.recent_pauses[0]
            .is_some_and(|oldest| now.saturating_duration_since(oldest) < REPEAT_WITHIN);
        self.recent_pauses.rotate_left(1);
        self.recent_pauses[REPEAT_COUNT - 2] = Some(now);
        self.phase = if repeated {
            Phase::UntilResume
        } else {
            Phase::CoolingUntil(now + COOLDOWN)
        };
        self.window = None;
        Paint::JustPaused
    }
}

/// A bounded account per extension, shared by all of its pages in a window.
#[derive(Default)]
pub(crate) struct RenderBudgets(BTreeMap<PluginId, Account>);

impl RenderBudgets {
    /// Start tracking an extension's first page, without resetting existing accounting.
    pub(crate) fn track(&mut self, plugin: &PluginId) -> bool {
        if self.0.contains_key(plugin) {
            return true;
        }
        if self.0.len() >= MAX_TRACKED_PLUGINS {
            return false;
        }
        self.0.insert(plugin.clone(), Account::default());
        true
    }

    /// Leave only accounts whose extensions still have eligible pages in the owner's view.
    pub(crate) fn retain(&mut self, mut has_page: impl FnMut(&PluginId) -> bool) -> bool {
        let before = self.0.len();
        self.0.retain(|id, _| has_page(id));
        self.0.len() != before
    }

    pub(crate) fn phase(&self, plugin: &PluginId) -> Option<Phase> {
        self.0.get(plugin).map(|account| account.phase)
    }

    /// Observe the frame boundary before doing any painting for this callback.
    pub(crate) fn begin(&mut self, plugin: &PluginId, frame: u64, now: Instant) -> Paint {
        self.0
            .get_mut(plugin)
            .map_or(Paint::Skip, |account| account.begin(frame, now))
    }

    pub(crate) fn charge(&mut self, plugin: &PluginId, elapsed: Duration) {
        if let Some(window) = self
            .0
            .get_mut(plugin)
            .and_then(|account| account.window.as_mut())
        {
            window.spent = window.spent.saturating_add(elapsed);
        }
    }

    /// A stale timer must not lift a later pause.
    pub(crate) fn expire(&mut self, plugin: &PluginId, until: Instant, now: Instant) -> bool {
        if let Some(account) = self.0.get_mut(plugin)
            && account.phase == Phase::CoolingUntil(until)
            && now >= until
        {
            account.phase = Phase::Active;
            return true;
        }
        false
    }

    /// Resume an extension early, or give an extension paused indefinitely a new trial.
    pub(crate) fn resume(&mut self, plugin: &PluginId) -> bool {
        let Some(account) = self.0.get_mut(plugin) else {
            return false;
        };
        match account.phase {
            Phase::Active => return false,
            Phase::CoolingUntil(_) => {}
            Phase::UntilResume => account.recent_pauses = Default::default(),
        }
        account.phase = Phase::Active;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin() -> PluginId {
        PluginId::new("fixture").unwrap()
    }

    fn draw_until_paused(
        budgets: &mut RenderBudgets,
        plugin: &PluginId,
        start: Instant,
    ) -> Instant {
        for frame in 0..SUSTAIN_WINDOWS {
            assert_eq!(
                budgets.begin(plugin, u64::from(frame), start + WINDOW * frame),
                Paint::Draw
            );
            budgets.charge(plugin, LIMIT + Duration::from_millis(1));
        }
        let pause = start + WINDOW * SUSTAIN_WINDOWS;
        assert_eq!(
            budgets.begin(plugin, u64::from(SUSTAIN_WINDOWS), pause),
            Paint::JustPaused
        );
        pause
    }

    #[test]
    fn sustained_cost_pauses_only_the_extension_that_drew() {
        let (first, other) = (plugin(), PluginId::new("other").unwrap());
        let mut budgets = RenderBudgets::default();
        assert!(budgets.track(&first));
        assert!(budgets.track(&other));
        let now = draw_until_paused(&mut budgets, &first, Instant::now());
        assert_eq!(
            budgets.phase(&first),
            Some(Phase::CoolingUntil(now + COOLDOWN))
        );
        assert_eq!(budgets.begin(&first, 3, now), Paint::Skip);
        assert_eq!(budgets.begin(&other, 3, now), Paint::Draw);
    }

    #[test]
    fn one_long_frame_counts_toward_only_one_second() {
        let mut budgets = RenderBudgets::default();
        let plugin = plugin();
        assert!(budgets.track(&plugin));
        let start = Instant::now();
        // Even several callbacks of one frame spanning the whole grace period count only once.
        for second in 0..SUSTAIN_WINDOWS {
            let now = start + WINDOW * second;
            assert_eq!(budgets.begin(&plugin, 0, now), Paint::Draw);
            budgets.charge(&plugin, WINDOW);
        }
        assert_eq!(
            budgets.begin(&plugin, 1, start + WINDOW * SUSTAIN_WINDOWS),
            Paint::Draw
        );
    }

    #[test]
    fn sustained_map_paint_fits_but_three_times_its_measured_cost_does_not() {
        // The peak observed in the native software stress trace (234.448 ms in one second).
        const MAP_PAINT: Duration = Duration::from_millis(235);
        const FRAMES_PER_SECOND: u32 = 60;
        let plugin = plugin();
        let start = Instant::now();
        for (paint_per_second, expected) in
            [(MAP_PAINT, Paint::Draw), (MAP_PAINT * 3, Paint::JustPaused)]
        {
            let mut budgets = RenderBudgets::default();
            assert!(budgets.track(&plugin));
            let frames = SUSTAIN_WINDOWS * FRAMES_PER_SECOND;
            for frame in 0..frames {
                let at = start + WINDOW * frame / FRAMES_PER_SECOND;
                assert_eq!(budgets.begin(&plugin, u64::from(frame), at), Paint::Draw);
                budgets.charge(&plugin, paint_per_second / FRAMES_PER_SECOND);
            }
            assert_eq!(
                budgets.begin(&plugin, u64::from(frames), start + WINDOW * SUSTAIN_WINDOWS),
                expected
            );
        }
    }

    #[test]
    fn three_seconds_of_fast_interaction_do_not_pause_drawing() {
        let mut budgets = RenderBudgets::default();
        let plugin = plugin();
        assert!(budgets.track(&plugin));
        let start = Instant::now();
        for frame in 0..3 * 60 {
            assert_eq!(
                budgets.begin(&plugin, u64::from(frame), start + WINDOW * frame / 60),
                Paint::Draw
            );
            budgets.charge(&plugin, WINDOW / 60);
        }
        assert_eq!(
            budgets.begin(&plugin, 3 * 60, start + WINDOW * 3),
            Paint::Draw
        );
        assert_eq!(budgets.phase(&plugin), Some(Phase::Active));
    }

    #[test]
    fn a_second_at_budget_or_an_idle_gap_breaks_the_streak() {
        for quiet in [LIMIT, Duration::ZERO] {
            let mut budgets = RenderBudgets::default();
            let plugin = plugin();
            assert!(budgets.track(&plugin));
            let start = Instant::now();
            let cost = [WINDOW; SUSTAIN_WINDOWS as usize - 1];
            for (frame, cost) in cost
                .into_iter()
                .chain([quiet])
                .chain(cost)
                .chain([LIMIT])
                .enumerate()
            {
                if cost.is_zero() {
                    continue;
                }
                assert_eq!(
                    budgets.begin(&plugin, frame as u64, start + WINDOW * frame as u32),
                    Paint::Draw
                );
                budgets.charge(&plugin, cost);
            }
        }
    }

    #[test]
    fn repeated_multi_second_frames_are_still_sustained_cost() {
        let mut budgets = RenderBudgets::default();
        let plugin = plugin();
        assert!(budgets.track(&plugin));
        let start = Instant::now();
        let spend = WINDOW * 3;
        for frame in 0..SUSTAIN_WINDOWS {
            assert_eq!(
                budgets.begin(&plugin, u64::from(frame), start + spend * frame),
                Paint::Draw
            );
            budgets.charge(&plugin, spend);
        }
        assert_eq!(
            budgets.begin(
                &plugin,
                u64::from(SUSTAIN_WINDOWS),
                start + spend * SUSTAIN_WINDOWS
            ),
            Paint::JustPaused
        );
    }

    #[test]
    fn cooldown_ends_at_the_deadline_with_a_fresh_drawing_window() {
        let mut budgets = RenderBudgets::default();
        let plugin = plugin();
        assert!(budgets.track(&plugin));
        let pause = draw_until_paused(&mut budgets, &plugin, Instant::now());
        let until = pause + COOLDOWN;
        assert!(!budgets.expire(&plugin, until, until - Duration::from_nanos(1)));
        assert_eq!(
            budgets.begin(&plugin, 4, until - Duration::from_millis(1)),
            Paint::Skip
        );
        assert!(budgets.expire(&plugin, until, until));
        assert_eq!(budgets.begin(&plugin, 5, until), Paint::Draw);
    }

    #[test]
    fn the_third_recent_pause_waits_for_resume_even_if_the_first_ended_early() {
        let mut budgets = RenderBudgets::default();
        let plugin = plugin();
        assert!(budgets.track(&plugin));
        let first = draw_until_paused(&mut budgets, &plugin, Instant::now());
        assert!(budgets.resume(&plugin));
        let second = draw_until_paused(&mut budgets, &plugin, first);
        assert!(budgets.expire(&plugin, second + COOLDOWN, second + COOLDOWN));
        let third = draw_until_paused(&mut budgets, &plugin, second + COOLDOWN);
        assert_eq!(budgets.phase(&plugin), Some(Phase::UntilResume));
        assert!(!budgets.expire(&plugin, third + COOLDOWN, third + REPEAT_WITHIN));
        assert_eq!(
            budgets.begin(&plugin, 4, third + REPEAT_WITHIN),
            Paint::Skip
        );
        assert!(budgets.resume(&plugin));
        let fresh = draw_until_paused(&mut budgets, &plugin, third + REPEAT_WITHIN);
        assert_eq!(
            budgets.phase(&plugin),
            Some(Phase::CoolingUntil(fresh + COOLDOWN))
        );
    }

    #[test]
    fn a_pause_outside_the_repeat_window_does_not_count() {
        let mut budgets = RenderBudgets::default();
        let plugin = plugin();
        assert!(budgets.track(&plugin));
        let first = draw_until_paused(&mut budgets, &plugin, Instant::now());
        assert!(budgets.expire(&plugin, first + COOLDOWN, first + COOLDOWN));
        let second = draw_until_paused(&mut budgets, &plugin, first + COOLDOWN);
        assert!(budgets.expire(&plugin, second + COOLDOWN, second + COOLDOWN));
        let third = draw_until_paused(&mut budgets, &plugin, first + REPEAT_WITHIN);
        assert_eq!(
            budgets.phase(&plugin),
            Some(Phase::CoolingUntil(third + COOLDOWN))
        );
    }

    #[test]
    fn the_account_survives_navigation_and_is_forgotten_when_its_pages_disappear() {
        let mut budgets = RenderBudgets::default();
        let plugin = plugin();
        assert!(budgets.track(&plugin));
        let pause = draw_until_paused(&mut budgets, &plugin, Instant::now());
        assert!(budgets.track(&plugin));
        assert_eq!(budgets.begin(&plugin, 4, pause), Paint::Skip);
        assert!(budgets.retain(|_| false));
        assert_eq!(budgets.phase(&plugin), None);
        assert_eq!(budgets.begin(&plugin, 5, pause), Paint::Skip);
        assert!(budgets.track(&plugin));
        assert_eq!(budgets.begin(&plugin, 6, pause), Paint::Draw);
    }

    #[test]
    fn a_full_book_refuses_new_ids_and_reuses_a_slot_after_removal() {
        let mut budgets = RenderBudgets::default();
        let plugin = plugin();
        let ids = (0..MAX_TRACKED_PLUGINS)
            .map(|n| PluginId::new(format!("fixture-{n}")).unwrap())
            .collect::<Vec<_>>();
        for id in &ids {
            assert!(budgets.track(id));
        }
        assert!(budgets.track(&ids[0]));
        assert!(!budgets.track(&plugin));
        assert_eq!(budgets.begin(&plugin, 0, Instant::now()), Paint::Skip);
        assert!(budgets.retain(|id| *id != ids[0]));
        assert!(budgets.track(&plugin));
    }
}
