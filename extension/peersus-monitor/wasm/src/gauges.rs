//! The numbers of one device: each part as it was last told, the rates made of two answers in a row, and the last minute
//! of the two parts that have a graph. This device has one, and so does the paired device whose numbers are shown; the
//! same rules make both, so a number is never worked out two ways.
use crate::Part;
use serde_json::Value;

/// Most points a graph keeps: the last minute.
pub const HISTORY: usize = 60;

/// What was said of one part.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Reading {
    /// Not answered yet.
    #[default]
    Unknown,
    /// The person did not allow this extension to see it (on this device, or on the one that answered).
    Denied,
    /// The device that should have said it could not read it.
    Unavailable,
    Known(Value),
}

/// The last counters, to make a rate of the next ones: processor ticks busy and in all, network bytes in and out
/// with the moment they were asked for.
#[derive(Default)]
struct Counters {
    processor: Option<(u64, u64)>,
    network: Option<(u64, u64, u64)>,
}

#[derive(Default)]
pub struct Gauges {
    pub parts: [Reading; 6],
    counters: Counters,
    /// Share of the processor busy between the last two readings, hundredths of a percent.
    pub busy: Option<u16>,
    /// Bytes per second received and sent between the last two readings.
    pub speed: Option<(u64, u64)>,
    /// The processor's busy share, the graphics' and the network's total speed, oldest first.
    pub busy_history: Vec<u64>,
    pub graphics_history: Vec<u64>,
    pub speed_history: Vec<u64>,
}

impl Gauges {
    /// A part that may not be seen keeps nothing of what was seen of it.
    pub fn deny(&mut self, part: Part) {
        self.forget(part);
        self.parts[part as usize] = Reading::Denied;
    }

    /// A part the device could not read keeps nothing either, and says so.
    pub fn unavailable(&mut self, part: Part) {
        self.forget(part);
        self.parts[part as usize] = Reading::Unavailable;
    }

    fn forget(&mut self, part: Part) {
        match part {
            Part::Processor => {
                self.counters.processor = None;
                self.busy = None;
                self.busy_history.clear();
            }
            Part::Graphics => self.graphics_history.clear(),
            Part::Network => {
                self.counters.network = None;
                self.speed = None;
                self.speed_history.clear();
            }
            _ => {}
        }
    }

    /// A new reading of a part, taken at `at` (the page's clock, in milliseconds): for the processor and the network,
    /// the rate since the last one, and one more point of its graph.
    pub fn read(&mut self, part: Part, value: Value, at: u64) {
        fn keep(history: &mut Vec<u64>, point: u64) {
            if history.len() == HISTORY {
                history.remove(0);
            }
            history.push(point);
        }
        match part {
            Part::Processor => {
                let now = (value["busy"].as_u64(), value["total"].as_u64());
                self.busy = match (now, self.counters.processor) {
                    ((Some(busy), Some(total)), Some((was_busy, was_total)))
                        if total > was_total =>
                    {
                        crate::share(busy.saturating_sub(was_busy), total - was_total)
                    }
                    _ => None,
                };
                self.counters.processor = now.0.zip(now.1);
                if let Some(busy) = self.busy {
                    keep(&mut self.busy_history, u64::from(busy));
                }
            }
            Part::Graphics => {
                if let Some(busy) = value["busy"].as_u64() {
                    keep(&mut self.graphics_history, busy);
                }
            }
            Part::Network => {
                let now = (value["received"].as_u64(), value["sent"].as_u64());
                self.speed = match (now, self.counters.network) {
                    ((Some(received), Some(sent)), Some((was_received, was_sent, was_at)))
                        if at > was_at =>
                    {
                        let seconds = (at - was_at) as f64 / 1000.;
                        let rate =
                            |now: u64, was: u64| (now.saturating_sub(was) as f64 / seconds) as u64;
                        Some((rate(received, was_received), rate(sent, was_sent)))
                    }
                    _ => None,
                };
                self.counters.network = now
                    .0
                    .zip(now.1)
                    .map(|(received, sent)| (received, sent, at));
                if let Some((received, sent)) = self.speed {
                    keep(&mut self.speed_history, received.saturating_add(sent));
                }
            }
            _ => {}
        }
        self.parts[part as usize] = Reading::Known(value);
    }
}
