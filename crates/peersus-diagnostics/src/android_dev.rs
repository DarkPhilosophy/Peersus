//! Project reviewed TRACE events onto INFO in the dedicated Logcat sink. Android normally
//! discards VERBOSE before logcat can display it. The subscriber and its filters still see TRACE.
use std::{
    fmt::{self, Write as _},
    sync::Mutex,
};
use tracing::{
    Dispatch, Event, Level, Subscriber,
    callsite::DefaultCallsite,
    field::{Field, Visit},
};
use tracing_subscriber::layer::{Context, Layer};

pub(crate) const TARGET: &str = "peersus::dev";
#[cfg(target_os = "android")]
pub(crate) const TAG: &str = "peersus-dev";

static LOGCAT_INFO: tracing::Metadata<'static> = tracing::metadata! {
    name: "peersus reviewed Android trace",
    target: TARGET,
    level: Level::INFO,
    fields: &["message"],
    callsite: &LOGCAT_CALLSITE,
    kind: tracing::metadata::Kind::EVENT,
};
static LOGCAT_CALLSITE: DefaultCallsite = DefaultCallsite::new(&LOGCAT_INFO);

pub(crate) fn reviewed(event: &tracing::Metadata<'_>) -> bool {
    event.is_event() && event.target() == TARGET && *event.level() == Level::TRACE
}

#[derive(Default)]
struct Budget {
    start: u64,
    sent: u8,
}

impl Budget {
    fn admit(&mut self, now: u64, limit: u8) -> bool {
        if now.saturating_sub(self.start) >= 1000 {
            self.start = now;
            self.sent = 0;
        }
        if self.sent == limit {
            return false;
        }
        self.sent += 1;
        true
    }
}

pub(crate) struct Sink {
    role: &'static str,
    dispatch: Dispatch,
    monotonic_ms: fn() -> u64,
    ordinary: Mutex<Budget>,
    milestones: Mutex<Budget>,
}

impl Sink {
    pub(crate) fn new(role: &'static str, dispatch: Dispatch, monotonic_ms: fn() -> u64) -> Self {
        Self {
            role,
            dispatch,
            monotonic_ms,
            ordinary: Mutex::default(),
            milestones: Mutex::default(),
        }
    }
}

impl<S: Subscriber> Layer<S> for Sink {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        if !reviewed(event.metadata()) {
            return;
        }
        let mut stage = Stage::default();
        event.record(&mut stage);
        if !stage.present {
            return;
        }
        let (budget, limit) = if stage.milestone {
            (&self.milestones, 8)
        } else {
            (&self.ordinary, 64)
        };
        let now = (self.monotonic_ms)();
        // Logging is lossy on contention: never wait on the budget lock.
        if !budget
            .try_lock()
            .is_ok_and(|mut budget| budget.admit(now, limit))
        {
            return;
        }
        let mut line = Fields(String::with_capacity(256));
        let _ = write!(&mut line.0, "role={} mono_ms={now}", self.role);
        event.record(&mut line);
        // The sink has its own registry and never observes spans or their fields. In particular
        // the native formatter can't prepend a path or inherited span data to this projection.
        let message = line.0.as_str();
        let values: [Option<&dyn tracing::field::Value>; 1] = [Some(&message)];
        self.dispatch.event(&Event::new(
            &LOGCAT_INFO,
            &LOGCAT_INFO.fields().value_set_all(&values),
        ));
    }
}

#[derive(Default)]
struct Stage {
    present: bool,
    milestone: bool,
}

impl Visit for Stage {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "stage" && word(value) {
            self.present = true;
            self.milestone = matches!(value, "canvas-mount" | "canvas-first-frame");
        }
    }
    fn record_debug(&mut self, _: &Field, _: &dyn fmt::Debug) {}
}

fn word(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 48
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

struct Fields(String);

impl Fields {
    fn number(&mut self, field: &Field, value: impl fmt::Display) {
        if matches!(
            field.name(),
            "process"
                | "child_process"
                | "call"
                | "ticket"
                | "number"
                | "handle"
                | "bytes"
                | "display_list_bytes"
                | "elapsed_ms"
                | "delay_ms"
                | "timeout_ms"
                | "connection"
                | "generation"
                | "caller"
                | "caller_generation"
                | "provider"
                | "provider_generation"
                | "service_slot"
                | "method_slot"
                | "publisher"
                | "subscriber"
                | "cancelled"
                | "open"
                | "correlation"
                | "actual"
                | "expected"
                | "showing"
                | "refused_rasters"
                | "read_errors"
        ) {
            let _ = write!(&mut self.0, " {}={value}", field.name());
        }
    }
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        if matches!(
            field.name(),
            "stage"
                | "outcome"
                | "direction"
                | "kind"
                | "method"
                | "operation"
                | "step"
                | "guest_stage"
        ) && word(value)
        {
            let _ = write!(&mut self.0, " {}={value}", field.name());
        }
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.number(field, value);
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.number(field, value);
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.number(field, value);
    }
    fn record_debug(&mut self, _: &Field, _: &dyn fmt::Debug) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tracing_subscriber::{EnvFilter, prelude::*};

    #[derive(Clone, Default)]
    struct Recorded(Arc<Mutex<Vec<String>>>);
    impl<S: Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>> Layer<S> for Recorded {
        fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
            assert_eq!(*event.metadata().level(), Level::INFO);
            assert!(ctx.event_scope(event).is_none());
            struct Message<'a>(&'a mut Vec<String>);
            impl Visit for Message<'_> {
                fn record_str(&mut self, field: &Field, value: &str) {
                    assert_eq!(field.name(), "message");
                    self.0.push(value.to_owned());
                }
                fn record_debug(&mut self, _: &Field, _: &dyn fmt::Debug) {
                    panic!("expected one string");
                }
            }
            event.record(&mut Message(&mut self.0.lock().unwrap()));
        }
    }

    #[test]
    fn output_is_fixed_field_info_and_a_flood_does_not_hide_the_canvas_milestone() {
        let record = Recorded::default();
        let sink = Dispatch::new(tracing_subscriber::registry().with(record.clone()));
        let subscriber = tracing_subscriber::registry()
            .with(EnvFilter::new("warn,peersus::dev=trace"))
            .with(Sink::new("wasm-worker", sink, || 1234));
        tracing::subscriber::with_default(subscriber, || {
            let _span = tracing::warn_span!("fixture", payload = "private-span").entered();
            for _ in 0..200 {
                crate::dev_trace!(
                    stage = "canvas-present",
                    outcome = "limited",
                    x = 5,
                    reason = "/private/path",
                    "private-message"
                );
            }
            crate::dev_trace!(
                stage = "canvas-first-frame",
                refused_rasters = 3u64,
                "first"
            );
            tracing::trace!(target: "peersus::dev", stage = "/private/path", "never");
        });
        let lines = record.0.lock().unwrap();
        assert_eq!(lines.len(), 65);
        assert!(lines[0].contains("role=wasm-worker mono_ms=1234 process="));
        assert!(lines[0].ends_with("stage=canvas-present outcome=limited"));
        assert!(lines[64].ends_with("stage=canvas-first-frame refused_rasters=3"));
    }

    #[test]
    fn a_budget_reopens_only_after_its_window() {
        let mut budget = Budget::default();
        assert!(budget.admit(12_345, 1));
        assert!(!budget.admit(13_344, 1));
        assert!(budget.admit(13_345, 1));
    }
}
