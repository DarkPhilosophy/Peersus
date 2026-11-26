//! A guest may name only fixed stages/outcomes and bounded counters, never text.
use std::time::{Duration, Instant};

/// Per isolated generation, not per call: repeated guest work cannot reset its quota.
pub(super) struct Budget {
    began: Instant,
    remaining: u32,
}
impl Default for Budget {
    fn default() -> Self {
        Self {
            began: Instant::now(),
            remaining: 64,
        }
    }
}
impl Budget {
    pub(super) fn emit(
        &mut self,
        stage: i32,
        outcome: i32,
        correlation: i64,
        actual: i64,
        expected: i64,
    ) -> i32 {
        let stage = match stage {
            1 => "begin",
            2 => "call",
            3 => "connect",
            4 => "acknowledge",
            5 => "input",
            6 => "receive",
            7 => "switch",
            8 => "release",
            9 => "end",
            10 => "result",
            _ => return -1,
        };
        let outcome = match outcome {
            0 => "ok",
            1 => "refused",
            2 => "cancelled",
            3 => "malformed",
            4 => "stopped",
            5 => "not-replayed",
            6 => "count-mismatch",
            7 => "unavailable",
            8 => "failed",
            _ => return -1,
        };
        if correlation < 0
            || !(0..=i64::from(u32::MAX)).contains(&actual)
            || !(0..=i64::from(u32::MAX)).contains(&expected)
        {
            return -1;
        }
        if !peersus_diagnostics::DEVELOPMENT {
            return 1;
        }
        if self.began.elapsed() >= Duration::from_secs(1) {
            self.began = Instant::now();
            self.remaining = 64;
        }
        if self.remaining == 0 {
            return -2;
        }
        self.remaining -= 1;
        peersus_diagnostics::dev_trace!(
            stage = "guest-decision",
            guest_stage = stage,
            outcome,
            correlation,
            actual,
            expected,
            "Guest decision"
        );
        0
    }
}
