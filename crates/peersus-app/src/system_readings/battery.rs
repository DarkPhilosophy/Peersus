//! An estimate is derived only from battery requests: no polling or persistent state.
use super::{Battery, BatteryStatus};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

const MAX_ESTIMATE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
// Many counters update in 1,000 µAh or µWh steps. Require several steps and time for
// short bursts of use to average out, not just one change in a coarse gauge.
const MIN_MOVEMENT: u64 = 3_000;
const MIN_SPAN: Duration = Duration::from_secs(2 * 60);
const WINDOW: Duration = Duration::from_secs(10 * 60);
// An idle or suspended interval is not evidence of the current rate of use.
const MAX_GAP: Duration = Duration::from_secs(90);
const SAMPLE_TTL: Duration = Duration::from_secs(1);
const MAX_CHANGES: usize = (WINDOW.as_secs() / SAMPLE_TTL.as_secs() + 1) as usize;
const MIN_STALL: Duration = Duration::from_secs(15);
// Both the combined timing uncertainty and the change to a published countdown are bounded by 1/20.
const LIMIT_DIVISOR: u32 = 20;

/// A platform battery reading, with internal inputs for time estimation. Only `Battery` goes on the wire.
#[derive(Default)]
pub struct BatteryReading {
    pub(super) battery: Battery,
    pub(super) counter: Option<Counter>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Source {
    Android,
    #[cfg(any(target_os = "linux", target_os = "android", test))]
    Energy(u64),
    #[cfg(any(target_os = "linux", target_os = "android", test))]
    Charge(u64),
}

pub(super) struct Counter {
    pub(super) source: Source,
    pub(super) remaining: u64,
    pub(super) full: Option<u64>,
}

/// Android sends its independently reported status and plugged flag, a charge counter in µAh,
/// and two OS-provided durations in milliseconds. No momentary current is used.
pub fn battery_said(
    percent: Option<i32>,
    status: Option<BatteryStatus>,
    plugged: Option<bool>,
    charge_left: Option<i32>,
    discharge_ms: Option<i64>,
    to_full_ms: Option<i64>,
) -> BatteryReading {
    let predicted_ms = match status {
        Some(BatteryStatus::Discharging) => discharge_ms,
        Some(BatteryStatus::Charging) => to_full_ms,
        Some(BatteryStatus::NotCharging | BatteryStatus::Full) | None => None,
    };
    let minutes = predicted_ms
        .and_then(|ms| u64::try_from(ms).ok())
        .map(Duration::from_millis)
        .filter(|duration| !duration.is_zero() && *duration <= MAX_ESTIMATE)
        .map(|duration| (duration.as_secs() / 60) as u32);
    BatteryReading {
        battery: Battery {
            charge: percent
                .filter(|n| (0..=100).contains(n))
                .and_then(|n| u16::try_from(n * 100).ok()),
            status,
            plugged,
            minutes,
        },
        counter: charge_left
            .and_then(|left| u64::try_from(left).ok())
            .map(|remaining| Counter {
                source: Source::Android,
                remaining,
                full: None,
            }),
    }
}

/// Boottime is monotonic but includes suspend, so a read after a long sleep starts a new trend.
pub(super) fn sample_time() -> Option<Duration> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        use rustix::time::{DynamicClockId, clock_gettime_dynamic};
        let now = clock_gettime_dynamic(DynamicClockId::Boottime).ok()?;
        Some(Duration::new(
            now.tv_sec.try_into().ok()?,
            now.tv_nsec.try_into().ok()?,
        ))
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    None
}

#[derive(Clone, Copy)]
struct Sample {
    time: Duration,
    remaining: u64,
}

/// The counter changed between these two adjacent readings.
struct Change {
    before: Duration,
    seen: Sample,
}

impl Change {
    fn rate_since(&self, start: &Self) -> Option<(Duration, u64)> {
        let shortest = self.before.checked_sub(start.seen.time)?;
        let moved = self.seen.remaining.abs_diff(start.seen.remaining);
        let uncertainty = (start.seen.time - start.before) + (self.seen.time - self.before);
        (shortest >= MIN_SPAN && moved >= MIN_MOVEMENT && uncertainty <= shortest / LIMIT_DIVISOR)
            .then(|| (shortest + uncertainty / 2, moved))
    }
}

struct Forecast {
    at: Duration,
    left: Duration,
}

struct Trend {
    source: Source,
    status: BatteryStatus,
    /// Unchanged readings narrow the next change's bracket, but are not rate anchors.
    last: Sample,
    changes: VecDeque<Change>,
    forecast: Option<Forecast>,
}

impl Trend {
    fn stalled(&self, now: Duration) -> bool {
        let Some((first, latest)) = self.changes.front().zip(self.changes.back()) else {
            return false;
        };
        let intervals = (self.changes.len() - 1) as u32;
        intervals != 0
            && now - latest.seen.time
                >= ((latest.seen.time - first.seen.time) * 3 / intervals).clamp(MIN_STALL, WINDOW)
    }

    fn observe(&mut self, sample: Sample, left: u64) -> Option<Duration> {
        let previous = std::mem::replace(&mut self.last, sample);
        let now = sample.time;
        let changed = previous.remaining != sample.remaining;
        if !changed && self.stalled(now) {
            self.changes.clear();
            self.forecast = None;
        }
        let countdown = self.forecast.as_ref().and_then(|forecast| {
            let elapsed = now - forecast.at;
            (elapsed <= WINDOW && elapsed < forecast.left).then(|| forecast.left - elapsed)
        });
        // An expired countdown is no longer a valid baseline for a percentage revision.
        let expired = countdown.is_none() && self.forecast.take().is_some();
        if !changed {
            return countdown;
        }

        while self
            .changes
            .front()
            .is_some_and(|old| now - old.before > WINDOW)
        {
            self.changes.pop_front();
        }
        if self.changes.len() == MAX_CHANGES {
            self.changes.pop_front();
        }
        let latest = Change {
            before: previous.time,
            seen: sample,
        };
        let rate = self
            .changes
            .iter()
            .find_map(|start| latest.rate_since(start));
        self.changes.push_back(latest);
        if expired {
            return None;
        }
        let Some((span, moved)) = rate else {
            return countdown;
        };

        // Divide once using the midpoint span, without rounding the rate. The product
        // fits in u128 because the counter is u64 and the span is at most the window.
        let nanos = u128::from(left) * span.as_nanos() / u128::from(moved);
        if nanos > MAX_ESTIMATE.as_nanos() {
            self.forecast = None;
            return None;
        }
        let measured = Duration::from_nanos(nanos as u64);
        let left = countdown.map_or(measured, |current| {
            let revision = current / LIMIT_DIVISOR;
            measured.clamp(current - revision, current + revision)
        });
        self.forecast = Some(Forecast { at: now, left });
        Some(left)
    }
}

#[derive(Default)]
pub(crate) struct BatteryHistory {
    trend: Option<Trend>,
    sampled_at: Option<Instant>,
    cached: Battery,
}

impl BatteryHistory {
    /// One on-demand read shared by all callers. Only a fresh platform reading updates the trend.
    pub(super) fn sample(
        &mut self,
        clock: impl Fn() -> Instant,
        read: impl FnOnce() -> (Option<Duration>, BatteryReading),
    ) -> &Battery {
        let fresh = self
            .sampled_at
            .and_then(|at| clock().checked_duration_since(at))
            .is_some_and(|age| age < SAMPLE_TTL);
        if !fresh {
            let (now, reading) = read();
            self.cached = self.observe(now, reading);
            self.sampled_at = Some(clock());
        }
        &self.cached
    }

    pub(super) fn observe(&mut self, now: Option<Duration>, reading: BatteryReading) -> Battery {
        let mut battery = reading.battery;
        let measured = match (battery.status, reading.counter, now) {
            (
                Some(status @ (BatteryStatus::Charging | BatteryStatus::Discharging)),
                Some(counter),
                Some(now),
            ) => self.measure(status, counter, now),
            _ => {
                self.trend = None;
                None
            }
        };
        // Continue sampling even while the platform can predict, so a missing prediction can use recent history.
        battery.minutes = battery.minutes.or(measured);
        battery
    }

    fn measure(&mut self, status: BatteryStatus, counter: Counter, now: Duration) -> Option<u32> {
        let left = if status == BatteryStatus::Discharging {
            Some(counter.remaining)
        } else {
            counter
                .full
                .and_then(|full| full.checked_sub(counter.remaining))
        };
        let Some(left) = left else {
            self.trend = None;
            return None;
        };
        let sample = Sample {
            time: now,
            remaining: counter.remaining,
        };
        let trend = self.trend.as_mut().filter(|trend| {
            trend.source == counter.source
                && trend.status == status
                && now
                    .checked_sub(trend.last.time)
                    .is_some_and(|gap| gap <= MAX_GAP)
                && if status == BatteryStatus::Discharging {
                    sample.remaining <= trend.last.remaining
                } else {
                    sample.remaining >= trend.last.remaining
                }
        });
        let Some(trend) = trend else {
            self.trend = Some(Trend {
                source: counter.source,
                status,
                last: sample,
                changes: VecDeque::new(),
                forecast: None,
            });
            return None;
        };
        trend
            .observe(sample, left)
            .map(|time| (time.as_secs() / 60) as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use BatteryStatus::{Charging, Discharging, Full, NotCharging};

    fn reading(status: BatteryStatus, source: Source, remaining: u64) -> BatteryReading {
        BatteryReading {
            battery: Battery {
                status: Some(status),
                ..Battery::default()
            },
            counter: Some(Counter {
                source,
                remaining,
                full: Some(4_000_000),
            }),
        }
    }

    fn discharging(history: &mut BatteryHistory, time: Duration, left: u64) -> Option<u32> {
        history
            .observe(Some(time), reading(Discharging, Source::Charge(1), left))
            .minutes
    }

    fn idle_gauge(second: u64) -> u64 {
        900_000 - second / 60 * 1_000 // 60 mA, with one 1 mAh step per minute
    }

    #[test]
    fn rapid_battery_requests_share_one_sample_until_the_second_expires() {
        let mut history = BatteryHistory::default();
        let start = Instant::now();
        let reads = std::cell::Cell::new(0);
        for (milliseconds, expected) in [(0, 1), (300, 1), (600, 1), (900, 1), (1000, 2)] {
            let elapsed = Duration::from_millis(milliseconds);
            let battery = history.sample(
                || start + elapsed,
                || {
                    reads.set(reads.get() + 1);
                    let mut sample = reading(Discharging, Source::Charge(1), 4_000_000);
                    sample.battery.charge = Some(reads.get());
                    (Some(elapsed), sample)
                },
            );
            assert_eq!(battery.charge, Some(expected));
            assert_eq!(
                history.trend.as_ref().unwrap().last.time,
                Duration::from_secs(u64::from(expected - 1))
            );
        }
    }

    #[test]
    fn the_reviewers_sparse_120_second_sequence_does_not_publish_an_uncertain_rate() {
        let mut history = BatteryHistory::default();
        for (ms, remaining) in [
            (0, 4_000_000),
            (60_000, 3_999_000),
            (120_000, 3_997_000),
            (120_300, 3_996_000),
        ] {
            assert_eq!(
                discharging(&mut history, Duration::from_millis(ms), remaining),
                None
            );
        }
    }

    #[test]
    fn the_reviewers_step_at_120_3_seconds_cannot_erase_a_quarter_of_an_existing_estimate() {
        let mut history = BatteryHistory::default();
        // The clock starts two seconds before the 4,000,000 reading, so that change is bracketed.
        let mut before = None;
        let mut left = 4_001_000;
        for second in 0..=122 {
            if matches!(second, 1 | 42 | 82 | 122) {
                left -= 1_000;
            }
            before = discharging(&mut history, Duration::from_secs(second), left);
        }
        assert_eq!(left, 3_997_000);
        let before = before.expect("bracketed forecast at 120 seconds");
        let after = discharging(&mut history, Duration::from_millis(122_300), 3_996_000)
            .expect("bounded revision at 120.3 seconds");
        let revision = before - after;
        assert!(revision.abs_diff(before / 20) <= 1, "{before} → {after}");
    }

    #[test]
    fn a_stalled_counter_counts_down_and_clears_after_three_expected_intervals() {
        let mut history = BatteryHistory::default();
        for second in 0..=240 {
            let minutes = discharging(
                &mut history,
                Duration::from_secs(second),
                idle_gauge(second),
            );
            assert_eq!(minutes, (second == 240).then_some(896));
        }
        for second in 241..420 {
            let minutes = discharging(&mut history, Duration::from_secs(second), idle_gauge(240));
            assert_eq!(minutes, Some(((896 * 60 - (second - 240)) / 60) as u32));
        }
        assert_eq!(
            discharging(&mut history, Duration::from_secs(420), idle_gauge(240)),
            None
        );
        assert_eq!(
            discharging(&mut history, Duration::from_secs(421), idle_gauge(300)),
            None
        );
    }

    #[test]
    fn sustained_tenfold_discharge_is_followed_without_a_large_single_revision() {
        let mut history = BatteryHistory::default();
        let mut left = 4_000_000;
        let mut minutes = None;
        for second in 0..=600 {
            if second > 0 && second % 60 == 0 {
                left -= 1_000;
            }
            minutes = discharging(&mut history, Duration::from_secs(second), left);
        }
        let baseline = minutes.expect("forecast before the load increase");
        let mut previous = baseline;
        let mut reacted = None;
        let mut caught_up = None;
        for seconds_since_change in 1..=600 {
            if seconds_since_change % 6 == 0 {
                left -= 1_000;
            }
            let estimate = discharging(
                &mut history,
                Duration::from_secs(600 + seconds_since_change),
                left,
            )
            .expect("a moving counter retains a forecast");
            assert!(estimate.abs_diff(previous) <= previous.div_ceil(20) + 1);
            if estimate + 1 < baseline {
                reacted.get_or_insert(seconds_since_change);
            }
            // At the new rate, one minute costs 10,000 counter units.
            let ideal = (left / 10_000) as u32;
            if estimate.abs_diff(ideal) <= ideal / 20 {
                caught_up.get_or_insert(seconds_since_change);
            } else {
                caught_up = None;
            }
            previous = estimate;
        }
        let reacted = reacted.expect("forecast responded to the extra load");
        let caught_up = caught_up.expect("forecast reached within 5% of the new rate");
        assert!(reacted <= 6);
        println!("10× discharge: first material revision at {reacted}s; within 5% at {caught_up}s");
    }

    #[test]
    fn thirty_second_reads_delay_the_first_estimate_until_changes_are_better_bracketed() {
        let mut history = BatteryHistory::default();
        for second in (0..=600).step_by(30) {
            assert_eq!(
                discharging(
                    &mut history,
                    Duration::from_secs(second),
                    idle_gauge(second)
                ),
                None
            );
        }
        for second in 601..=840 {
            let minutes = discharging(
                &mut history,
                Duration::from_secs(second),
                idle_gauge(second),
            );
            assert_eq!(minutes, (second == 840).then_some(886));
        }
    }

    #[test]
    fn alternating_rates_twenty_times_apart_are_averaged_and_history_stays_bounded() {
        let mut history = BatteryHistory::default();
        let mut remaining = 20_000_000;
        // A twenty-fold swing between successive readings: 100 vs 2,000 micro-units per second.
        let low = 100;
        let high = 20 * low;
        assert_eq!(
            history
                .observe(
                    Some(Duration::ZERO),
                    reading(Discharging, Source::Energy(1), remaining)
                )
                .minutes,
            None
        );
        let mut previous: Option<u32> = None;
        for second in 1..=4_000 {
            remaining -= if second % 2 == 0 { high } else { low };
            let battery = history.observe(
                Some(Duration::from_secs(second)),
                reading(Discharging, Source::Energy(1), remaining),
            );
            // The first and last one-second brackets leave a shortest span of second - 2.
            if second < MIN_SPAN.as_secs() + 2 {
                assert_eq!(battery.minutes, None);
            } else {
                let estimate = battery.minutes.expect("enough observed discharge");
                let average = remaining / ((high + low) / 2 * 60);
                assert!(u64::from(estimate).abs_diff(average) <= 3);
                if let Some(last) = previous {
                    assert!(estimate.abs_diff(last) <= 3);
                }
                previous = Some(estimate);
            }
            assert!(history.trend.as_ref().unwrap().changes.len() <= MAX_CHANGES);
        }
    }

    #[test]
    fn a_gap_reversal_status_or_battery_change_discards_old_samples() {
        for (time, status, source, remaining) in [
            (213, Discharging, Source::Energy(1), 3_877_000),
            (123, Discharging, Source::Energy(1), 3_878_001),
            (123, Charging, Source::Energy(1), 3_878_001),
            (123, Discharging, Source::Energy(2), 3_877_000),
            (123, Discharging, Source::Charge(1), 3_877_000),
            (121, Discharging, Source::Energy(1), 3_877_000),
        ] {
            let mut history = BatteryHistory::default();
            for second in 0..=122 {
                let battery = history.observe(
                    Some(Duration::from_secs(second)),
                    reading(Discharging, Source::Energy(1), 4_000_000 - second * 1_000),
                );
                assert_eq!(battery.minutes.is_some(), second == 122);
            }
            let clock = Some(Duration::from_secs(time));
            assert_eq!(
                history
                    .observe(clock, reading(status, source, remaining))
                    .minutes,
                None
            );
            assert_eq!(
                history
                    .observe(
                        Some(Duration::from_secs(time + 1)),
                        reading(status, source, remaining),
                    )
                    .minutes,
                None
            );
        }
    }

    #[test]
    fn an_android_prediction_takes_priority_and_invalid_predictions_use_the_same_estimate() {
        let mut history = BatteryHistory::default();
        let mut sample = |second, prediction| {
            history
                .observe(
                    Some(Duration::from_secs(second)),
                    battery_said(
                        Some(81),
                        Some(Discharging),
                        Some(false),
                        i32::try_from(idle_gauge(second)).ok(),
                        prediction,
                        None,
                    ),
                )
                .minutes
        };
        let max_ms = MAX_ESTIMATE.as_millis() as i64;
        for second in 0u64..=420 {
            let (prediction, expected) = match second {
                60 => (Some(180 * 60_000), Some(180)),
                120 => (Some(0), None),
                300 => (Some(0), Some(895)),
                360 => (Some(-1), Some(894)),
                361 => (Some(max_ms), Some(7 * 24 * 60)),
                420 => (Some(max_ms + 1), Some(893)),
                second if second >= 240 => (None, Some(900 - (second.div_ceil(60) as u32))),
                _ => (None, None),
            };
            assert_eq!(sample(second, prediction), expected, "at {second}s");
        }
    }

    #[test]
    fn time_to_full_uses_either_the_platform_or_the_same_measured_rate() {
        let mut history = BatteryHistory::default();
        for now in 0..=122 {
            let left = 3_970_000 + now * 100;
            let minutes = (now == 122).then_some(2);
            assert_eq!(
                history
                    .observe(
                        Some(Duration::from_secs(now)),
                        reading(Charging, Source::Charge(1), left)
                    )
                    .minutes,
                minutes
            );
        }
        for status in [Some(Charging), Some(Full), Some(NotCharging), None] {
            for plugged in [Some(true), Some(false), None] {
                let battery = history.observe(
                    None,
                    battery_said(
                        Some(81),
                        status,
                        plugged,
                        Some(400),
                        Some(45 * 60_000),
                        Some(90 * 60_000),
                    ),
                );
                assert_eq!(
                    battery,
                    Battery {
                        charge: Some(8100),
                        status,
                        plugged,
                        minutes: (status == Some(Charging)).then_some(90)
                    }
                );
            }
        }
    }
}
