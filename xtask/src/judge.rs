//! What a benchmark comparison decides (`cargo xtask bench --compare`, `cargo xtask bench-compare`).
//!
//! A metric needs every scheduled sample on both sides. Compare the best sample per side and
//! show the spread and the verdict against the thresholds below. A universally unavailable
//! counter (for example GPU time on the software renderer) is shown, not compared.
//!
//! A verdict is about the numbers the measuring programs produce on synthetic local data, on
//! the machine that ran both sides. It is not a device or acceptance result.

use serde_json::Value;
use std::fmt::Write as _;

/// What one number, and a whole comparison, comes to. Ordered: the worst of many is their sum.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verdict {
    Ok,
    Review,
    Reject,
}

impl Verdict {
    fn word(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Review => "REVIEW",
            Self::Reject => "REJECT",
        }
    }
}

/// How a number is judged.
#[derive(Clone, Copy)]
enum Limit {
    /// By how many percent its cost grew. A change smaller than `floor`, in the number's own
    /// unit, is too small to matter whatever its percentage.
    Percent {
        limits: (f64, f64),
        floor: f64,
        higher_is_better: bool,
    },
    /// By how much more of it there is, in its own unit: for a number that should be nothing,
    /// of which a percentage says nothing.
    More { review: f64, reject: f64 },
}

/// Percent of growth that asks for the cause, and that is refused.
const MEMORY: (f64, f64) = (10.0, 25.0);
const CPU: (f64, f64) = (15.0, 50.0);
const TIME: (f64, f64) = (25.0, 100.0);
const SIZE: (f64, f64) = (5.0, 15.0);

const fn cost(limits: (f64, f64), floor: f64) -> Option<Limit> {
    Some(Limit::Percent {
        limits,
        floor,
        higher_is_better: false,
    })
}

/// The numbers of a report that a comparison shows, by the name of their field (of the field
/// above it for a `median`), and how each is judged; `None` is shown and not judged.
const NUMBERS: &[(&str, Option<Limit>)] = &[
    // Read from outside the measured program, for every program.
    ("peak_resident_kib", cost(MEMORY, 2048.0)),
    ("peak_pss_kib", cost(MEMORY, 2048.0)),
    ("peak_anonymous_kib", cost(MEMORY, 2048.0)),
    ("gpu_memory_kib", cost(MEMORY, 2048.0)),
    ("cpu_ms", cost(CPU, 20.0)),
    ("main_thread_ms", cost(CPU, 20.0)),
    ("gpu_ms", cost(CPU, 20.0)),
    ("threads", None),
    // A product program, or the package of an extension, as a file; and that package unpacked.
    ("file_bytes", cost(SIZE, 16_384.0)),
    ("unpacked_bytes", cost(SIZE, 16_384.0)),
    ("largest_file_bytes", cost(SIZE, 16_384.0)),
    // The measured program's own account.
    ("resident_kib", cost(MEMORY, 2048.0)),
    ("heap_peak_bytes", cost(MEMORY, 262_144.0)),
    ("heap_live_bytes", cost(MEMORY, 262_144.0)),
    ("allocated_bytes", cost(MEMORY, 1024.0)),
    (
        "heap_live_bytes_grown",
        Some(Limit::More {
            review: 1_048_576.0,
            reject: 8_388_608.0,
        }),
    ),
    (
        "retained_bytes_per_open",
        Some(Limit::More {
            review: 1024.0,
            reject: 65_536.0,
        }),
    ),
    (
        "written_bytes",
        Some(Limit::More {
            review: 65_536.0,
            reject: 16_777_216.0,
        }),
    ),
    ("heap_allocated_bytes", None),
    ("voluntary_switches", cost(CPU, 200.0)),
    ("median_us", cost(TIME, 5.0)),
    ("p95_us", None),
    ("first_frame_ms", cost(TIME, 10.0)),
    ("startup_ms", cost(TIME, 20.0)),
    ("first_frame_work_us", cost(TIME, 1000.0)),
    ("connect_ms", cost(TIME, 0.5)),
    ("endpoint_pair_ms", cost(TIME, 0.5)),
    ("update_ms", cost(TIME, 0.01)),
    (
        "encrypted_loopback_mib_s",
        Some(Limit::Percent {
            limits: TIME,
            floor: 1.0,
            higher_is_better: true,
        }),
    ),
    ("frames_per_second", None),
    ("distinct_canvas_frames_presented", None),
    ("cpu_ms_per_presented_frame", cost(TIME, 0.1)),
    (
        "achieved_fps",
        Some(Limit::Percent {
            limits: TIME,
            floor: 2.0,
            higher_is_better: true,
        }),
    ),
    ("involuntary_switches", None),
];

/// The field a number is known by: its own, or the one above a `median`.
fn name_of(path: &str) -> &str {
    let mut fields = path.rsplit('.');
    let leaf = fields.next().unwrap_or(path);
    let name = if leaf == "median" {
        fields.next().unwrap_or(leaf)
    } else {
        leaf
    };
    name.split('[').next().unwrap_or(name)
}

/// Whether a comparison shows the number at `path`, and how it judges it.
fn known(path: &str) -> Option<Option<Limit>> {
    let name = name_of(path);
    NUMBERS
        .iter()
        .find(|(known, _)| *known == name)
        .map(|(_, limit)| *limit)
}

/// Every compared number of a report, by its path in it.
fn numbers(value: &Value, path: &str, into: &mut impl FnMut(&str, Option<f64>)) {
    match value {
        Value::Object(fields) => {
            for (name, value) in fields {
                let path = if path.is_empty() {
                    name.clone()
                } else {
                    format!("{path}.{name}")
                };
                numbers(value, &path, into);
            }
        }
        // A list of records is walked; a list of raw samples is not what is compared.
        Value::Array(items) => {
            for (index, item) in items
                .iter()
                .enumerate()
                .filter(|(_, item)| item.is_object())
            {
                // A history workload is identified by its record count, not its position in a run.
                let key = item
                    .get("records")
                    .and_then(Value::as_u64)
                    .map_or_else(|| index.to_string(), |records| format!("records={records}"));
                numbers(item, &format!("{path}[{key}]"), into);
            }
        }
        _ if known(path).is_some() => into(path, value.as_f64()),
        _ => {}
    }
}

/// The runs of one side for one number: the best of them, and by how many percent the worst
/// differs from it (nothing to say of one run, or around zero).
fn side(values: &[f64], higher_is_better: bool) -> (f64, Option<f64>) {
    let least = values.iter().copied().fold(f64::INFINITY, f64::min);
    let most = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let spread = (values.len() > 1 && least > 0.0).then(|| (most - least) / least * 100.0);
    (if higher_is_better { most } else { least }, spread)
}

fn judged(limit: Limit, base: f64, head: f64, noise: Option<f64>) -> Verdict {
    match limit {
        Limit::More { review, reject } => {
            let more = head - base;
            if more >= reject {
                Verdict::Reject
            } else if more >= review {
                Verdict::Review
            } else {
                Verdict::Ok
            }
        }
        Limit::Percent {
            limits: (review, reject),
            floor,
            higher_is_better,
        } => {
            if (head - base).abs() < floor {
                return Verdict::Ok;
            }
            let (cheap, dear) = if higher_is_better {
                (head, base)
            } else {
                (base, head)
            };
            if cheap <= 0.0 {
                // From nothing to something has no percentage; it is still a new cost.
                return if dear > cheap {
                    Verdict::Review
                } else {
                    Verdict::Ok
                };
            }
            let percent = (dear / cheap - 1.0) * 100.0;
            if percent <= noise.unwrap_or(0.0) {
                Verdict::Ok
            } else if percent >= reject {
                Verdict::Reject
            } else if percent >= review {
                Verdict::Review
            } else {
                Verdict::Ok
            }
        }
    }
}

/// One number both sides report.
pub struct Row {
    pub path: String,
    pub base: f64,
    pub head: f64,
    /// By how many percent the runs of one side differ among themselves, at most.
    pub noise: Option<f64>,
    /// `None` for a number that is shown and not judged.
    pub verdict: Option<Verdict>,
}

impl Row {
    /// The change of the number itself in percent; none from nothing.
    fn change(&self) -> Option<f64> {
        (self.base != 0.0).then(|| (self.head - self.base) / self.base.abs() * 100.0)
    }

    fn change_shown(&self) -> String {
        // A measured zero has an absolute delta, never a fabricated percentage.
        self.change().map_or_else(
            || {
                format!(
                    "{}{}",
                    if self.head > 0.0 { "+" } else { "" },
                    shown(&self.path, self.head)
                )
            },
            |change| format!("{:+.1}%", if change.abs() < 0.05 { 0.0 } else { change }),
        )
    }

    fn noise_shown(&self) -> String {
        self.noise
            .map_or_else(|| "-".to_owned(), |noise| format!("{noise:.1}%"))
    }
}

/// A number with the unit its name says it has.
fn shown(path: &str, value: f64) -> String {
    let name = name_of(path);
    if name.ends_with("_kib") {
        format!("{:.1} MiB", value / 1024.0)
    } else if name.contains("bytes") {
        if value.abs() >= 1_048_576.0 {
            format!("{:.2} MiB", value / 1_048_576.0)
        } else if value.abs() >= 1024.0 {
            format!("{:.1} KiB", value / 1024.0)
        } else {
            format!("{value:.0} B")
        }
    } else if name.ends_with("_mib_s") {
        format!("{value:.1} MiB/s")
    } else if name.ends_with("_ms") {
        format!("{value:.2} ms")
    } else if name.ends_with("_ms_per_presented_frame") {
        format!("{value:.2} ms/frame")
    } else if name.ends_with("_fps") {
        format!("{value:.2} fps")
    } else if name.ends_with("_us") {
        format!("{value:.1} µs")
    } else {
        format!("{value:.2}")
    }
}

/// The numbers a reader looks at first, in words: the size of each product program or
/// extension package, and what was read from outside each measured program and its heap.
fn headline(path: &str) -> Option<String> {
    let mut fields = path.split('.');
    let first = fields.next().unwrap_or(path);
    let process = path.contains(".process.");
    if path.starts_with("canvas.") && (path.contains(".cpu.") || !process) {
        let workload = fields.next()?;
        let metric = match name_of(path) {
            "cpu_ms" => "CPU time",
            "cpu_ms_per_presented_frame" => "CPU per presented frame",
            "achieved_fps" => "Canvas frame rate",
            "distinct_canvas_frames_presented" => "Distinct canvas frames",
            _ => return None,
        };
        let suffix = path
            .rsplit_once('.')
            .and_then(|(path, _)| path.split_once(".cpu."))
            .map_or_else(String::new, |(_, actor)| format!(" · `{actor}`"));
        return Some(format!("{metric} · canvas `{workload}`{suffix}"));
    }
    if path.starts_with("extension.") {
        let actor = fields.next()?;
        let metric = match name_of(path) {
            "cpu_ms" => "CPU time",
            "peak_resident_kib" => "Peak resident memory (RSS)",
            "peak_pss_kib" => "Peak proportional memory (PSS)",
            "startup_ms" => "Extension startup time",
            _ => return None,
        };
        return Some(format!("{metric} · extension `{actor}`"));
    }
    let label = match name_of(path) {
        "file_bytes" if first == "package" => {
            return Some(format!("Package size · `{}`", fields.next()?));
        }
        "unpacked_bytes" => return Some(format!("Package unpacked · `{}`", fields.next()?)),
        "largest_file_bytes" => {
            return Some(format!(
                "Largest file of the package · `{}`",
                fields.next()?
            ));
        }
        "file_bytes" => return Some(format!("Executable size · `{}`", fields.next()?)),
        "cpu_ms" if process => "CPU time",
        "main_thread_ms" if process => "Window main-thread CPU time",
        "gpu_ms" if process => "GPU time",
        "peak_resident_kib" if process => "Peak resident memory (RSS)",
        "peak_pss_kib" if process => "Peak proportional memory (PSS)",
        "peak_anonymous_kib" if process => "Peak anonymous memory (heap and stacks)",
        "gpu_memory_kib" if process => "GPU memory",
        "heap_peak_bytes" => "Peak Rust heap",
        _ => return None,
    };
    Some(format!("{label} · `{first}`"))
}

pub struct Comparison {
    pub rows: Vec<Row>,
    /// Incomplete metrics are shown with their sample counts, never silently dropped or compared with zero.
    incomplete: Vec<(String, [usize; 2], [Option<f64>; 2])>,
    /// How many runs each side has.
    runs: (usize, usize),
    failures: Vec<String>,
}

fn optional(path: &str) -> bool {
    matches!(name_of(path), "gpu_ms" | "gpu_memory_kib")
        || (path.starts_with("canvas.idle.") && name_of(path) == "cpu_ms_per_presented_frame")
}

impl Comparison {
    /// Compare the runs of a base with the runs of a change.
    pub fn of(base: &[Value], head: &[Value]) -> Self {
        let mut samples = std::collections::BTreeMap::<String, [Vec<f64>; 2]>::new();
        for (side, runs) in [base, head].into_iter().enumerate() {
            for run in runs {
                // One sample at most per reported metric, even if two JSON paths spell
                // the same metric (a dotted key is legal in an example report).
                let mut seen = std::collections::BTreeMap::new();
                numbers(run, "", &mut |path, value| {
                    seen.entry(path.to_owned())
                        .and_modify(|sample| *sample = None)
                        .or_insert(value);
                });
                for (path, value) in seen {
                    samples.entry(path).or_default()[side].extend(value);
                }
            }
        }
        let mut incomplete = Vec::new();
        let rows = samples
            .into_iter()
            .filter_map(|(path, [earlier, later])| {
                let limit = known(&path)?;
                let higher_is_better = matches!(
                    limit,
                    Some(Limit::Percent {
                        higher_is_better: true,
                        ..
                    })
                );
                if earlier.len() != base.len()
                    || later.len() != head.len()
                    || earlier.is_empty()
                    || later.is_empty()
                {
                    let best = |samples: &[f64]| {
                        (!samples.is_empty()).then(|| side(samples, higher_is_better).0)
                    };
                    incomplete.push((
                        path,
                        [earlier.len(), later.len()],
                        [best(&earlier), best(&later)],
                    ));
                    return None;
                }
                let (base, base_spread) = side(&earlier, higher_is_better);
                let (head, head_spread) = side(&later, higher_is_better);
                let noise = match (base_spread, head_spread) {
                    (Some(base), Some(head)) => Some(base.max(head)),
                    (spread, None) | (None, spread) => spread,
                };
                Some(Row {
                    verdict: limit.map(|limit| judged(limit, base, head, noise)),
                    path,
                    base,
                    head,
                    noise,
                })
            })
            .collect();
        Self {
            rows,
            incomplete,
            runs: (base.len(), head.len()),
            failures: Vec::new(),
        }
    }

    /// The worst verdict of any number.
    pub fn verdict(&self) -> Verdict {
        self.rows
            .iter()
            .filter_map(|row| row.verdict)
            .max()
            .unwrap_or(Verdict::Ok)
    }

    fn count(&self, verdict: Verdict) -> usize {
        self.rows
            .iter()
            .filter(|row| row.verdict == Some(verdict))
            .count()
    }

    pub fn with_failures(mut self, failures: Vec<String>) -> Self {
        self.failures = failures;
        self
    }

    fn missing_metrics(&self) -> usize {
        self.incomplete
            .iter()
            .filter(|(path, counts, _)| *counts != [0, 0] || !optional(path))
            .count()
    }

    fn inconclusive(&self) -> bool {
        !self.failures.is_empty()
            || self.missing_metrics() > 0
            || !self.rows.iter().any(|row| row.verdict.is_some())
    }

    pub fn blocks(&self) -> bool {
        self.inconclusive() || self.verdict() == Verdict::Reject
    }

    /// One sentence: what the comparison comes to.
    pub fn summary(&self) -> String {
        let (reject, review) = (self.count(Verdict::Reject), self.count(Verdict::Review));
        if self.inconclusive() {
            return format!(
                "INCONCLUSIVE — {} sample set error(s), {} incomplete metric(s), {} blocking and {} review regression(s) in available data",
                self.failures.len(),
                self.missing_metrics(),
                reject,
                review
            ) + if self.rows.iter().any(|row| row.verdict.is_some()) {
                ""
            } else {
                "; no complete judged metrics"
            };
        }
        if reject + review == 0 {
            "PASS — all scheduled metrics with applicable counters are within limits".to_owned()
        } else {
            format!("REGRESSION — {reject} blocking, {review} review")
        }
    }

    /// How the sides were run, and what that makes of the noise.
    fn method(&self) -> String {
        let (base, head) = self.runs;
        if base == 0 || head == 0 {
            format!(
                "{base} baseline and {head} after runs scheduled; one side has no comparable runs."
            )
        } else if base > 1 && head > 1 {
            format!(
                "{base} baseline runs and {head} after runs. Each metric uses its best sample. \
                 For relative limits, a delta within either side's observed spread is not flagged; \
                 absolute growth limits still apply. Spread is not a confidence interval."
            )
        } else {
            "One run of a side: a size is exact, but for a number that differs from run to run \
             (time, CPU, memory) the noise is not known, so every change past a limit is \
             flagged. Repeat the runs before reading a small change of those as real."
                .to_owned()
        }
    }

    /// The comparison for a terminal: a line a number, then the verdict.
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "{:<62} {:>14} {:>14} {:>9} {:>7} {}",
            "metric / method", "baseline", "after", "delta", "noise", "verdict"
        )];
        for row in &self.rows {
            lines.push(format!(
                "{:<62} {:>14.2} {:>14.2} {:>9} {:>7} {}",
                row.path,
                row.base,
                row.head,
                row.change_shown(),
                row.noise_shown(),
                row.verdict.map_or("", Verdict::word),
            ));
        }
        for failure in &self.failures {
            lines.push(format!("Unavailable: {failure}"));
        }
        lines.push(self.summary());
        lines.push(self.method());
        lines
    }

    fn program_metric(&self, path: &str, label: &str, unavailable: &str) -> String {
        if let Some(row) = self.rows.iter().find(|row| row.path == path) {
            let difference = row.head - row.base;
            let delta = format!(
                "{}{}",
                if difference > 0.0 { "+" } else { "" },
                shown(path, difference)
            );
            let percent = row
                .change()
                .map_or_else(|| "N/A %".to_owned(), |p| format!("{p:+.1}%"));
            let verdict = row.verdict.map_or("shown", Verdict::word);
            return format!(
                "{label}: {} → {}; {delta} ({percent}); {verdict}",
                shown(path, row.base),
                shown(path, row.head),
            );
        }
        if let Some((_, counts, values)) = self.incomplete.iter().find(|(name, _, _)| name == path)
            && (*counts != [0, 0] || !optional(path))
        {
            let [base, head] =
                values.map(|value| value.map_or_else(|| "not measured".into(), |n| shown(path, n)));
            return format!(
                "{label}: {base} → {head}; no delta or %; INCONCLUSIVE (samples {}/{} · {}/{})",
                counts[0], self.runs.0, counts[1], self.runs.1,
            );
        }
        format!("{label}: not measured — {unavailable}")
    }

    fn program_table(&self, text: &mut String) {
        text.push_str(
            "### Program resources\n\nValues vary by runner; compare only the two sides of this run. Each measured entry shows baseline → head, absolute delta, percent and verdict. App sizes are the ordinary shipped binaries. The extension replay's instrumented demo window/owner are not substituted for these executables.\n\n\
             | Program | RAM | CPU | GPU | App size |\n|---|---|---|---|---|\n",
        );
        for program in [
            "peersus-ui",
            "peersus",
            "peersus-wasm-host",
            "peersus-wasm-canvas-host",
        ] {
            let (binary, suffix) = if program == "peersus" {
                (
                    "peersus-test-identity",
                    "<br>Runtime: `peersus-test-identity`",
                )
            } else {
                (program, "")
            };
            let metric = |field, title, missing| {
                self.program_metric(&format!("programs.{binary}.{field}"), title, missing)
            };
            let missing = "this executable was not sampled by the workload";
            let rss = metric("peak_resident_kib", "RSS", missing);
            let pss = metric("peak_pss_kib", "PSS", missing);
            let mut cpu = metric("cpu_ms", "Time", missing);
            if program == "peersus-ui" {
                let main = metric("main_thread_ms", "Main thread", missing);
                let _ = write!(cpu, "<br>{main}");
            }
            let unavailable = "no GPU driver counters for this workload (CI uses software Xvfb)";
            let gpu_ram = metric("gpu_memory_kib", "Memory", unavailable);
            let gpu_time = metric("gpu_ms", "Time", unavailable);
            let size = self.program_metric(
                &format!("size.{program}.file_bytes"),
                "Executable",
                "no app-size sample",
            );
            let _ = writeln!(
                text,
                "| `{program}`{suffix} | {rss}<br>{pss} | {cpu} | {gpu_ram}<br>{gpu_time} | {size} |"
            );
        }
        text.push('\n');
    }

    /// The comparison as a document for a pull request: the verdict, the numbers that decide
    /// it, then every number.
    pub fn markdown(&self) -> String {
        let cell = |verdict: Option<Verdict>| match verdict {
            None => "shown".to_owned(),
            Some(Verdict::Ok) => Verdict::Ok.word().to_owned(),
            Some(flagged) => format!("**{}**", flagged.word()),
        };
        let table = |rows: &[&Row], named: bool| {
            let mut table = String::from(
                "| Metric / method | Baseline | After | Delta | Noise | Verdict |\n\
                 |---|---:|---:|---:|---:|---|\n",
            );
            for row in rows {
                let name = headline(&row.path)
                    .filter(|_| named)
                    .unwrap_or_else(|| format!("`{}`", row.path));
                let _ = writeln!(
                    table,
                    "| {name} | {} | {} | {} | {} | {} |",
                    shown(&row.path, row.base),
                    shown(&row.path, row.head),
                    row.change_shown(),
                    row.noise_shown(),
                    cell(row.verdict),
                );
            }
            table
        };
        let flagged = |row: &&Row| row.verdict.is_some_and(|verdict| verdict > Verdict::Ok);
        let mut deciding: Vec<&Row> = self.rows.iter().filter(flagged).collect();
        deciding.sort_by_key(|row| std::cmp::Reverse(row.verdict));
        let mut resources: Vec<&Row> = self
            .rows
            .iter()
            .filter(|row| headline(&row.path).is_some())
            .collect();
        // Sizes first: they are exact, and the first thing a reader asks of a change.
        resources.sort_by_key(|row| {
            !name_of(&row.path).ends_with("_bytes") || row.path.contains("heap")
        });
        let every: Vec<&Row> = self.rows.iter().collect();

        let mut text = format!("## Resource impact: {}\n\n", self.summary());
        let _ = writeln!(text, "{}\n", self.method());
        if !self.failures.is_empty() {
            text.push_str("### Sample set errors\n\n");
            for failure in &self.failures {
                let _ = writeln!(text, "- {failure}");
            }
            text.push('\n');
        }
        if self.rows.is_empty() && self.incomplete.is_empty() {
            // Every sample failed: an empty table and four rows of "not measured" say nothing.
            text.push_str(
                "### No numbers\n\nNo sample on either side produced a measurement, so nothing was compared. The cause is under *Sample set errors* above and in the sampling step's log.\n\n",
            );
            text.push_str(&limits());
            return text;
        }
        self.program_table(&mut text);
        if !deciding.is_empty() {
            let _ = writeln!(
                text,
                "### Past a limit\n\n{}",
                table(&deciding, false).trim_end()
            );
            text.push('\n');
        }
        if resources.is_empty() {
            text.push_str(
                "### Size, CPU, memory and frames\n\nNo size, CPU, memory or frame number was measured on both sides.\n",
            );
        } else {
            let _ = writeln!(
                text,
                "### Size, CPU, memory and frames\n\n{}",
                table(&resources, true).trim_end()
            );
        }
        if !self.incomplete.is_empty() {
            text.push_str("\n### Not comparable\n\n| Metric / method | Baseline | After | Delta | Samples (base / after) |\n|---|---:|---:|---:|---|\n");
            for (path, counts, values) in &self.incomplete {
                let name = headline(path).unwrap_or_else(|| format!("`{path}`"));
                let [base, head] =
                    values.map(|value| value.map_or_else(|| "N/A".into(), |v| shown(path, v)));
                let _ = writeln!(
                    text,
                    "| {name} | {base} | {head} | N/A | {}/{} · {}/{} |",
                    counts[0], self.runs.0, counts[1], self.runs.1
                );
            }
            text.push_str("\nN/A means unavailable, not zero. A scheduled metric missing any sample is inconclusive. GPU counters unavailable on both sides and CPU per frame in a zero-frame idle phase do not apply.\n");
        }
        let _ = writeln!(
            text,
            "\n<details><summary>Every compared number ({})</summary>\n\n{}\n</details>\n",
            every.len(),
            table(&every, false).trim_end()
        );
        text.push_str(
            "\nDeltas from a measured zero are shown in the metric's units instead of percent.\n\n",
        );
        text.push_str(&limits());
        text
    }
}

/// The limits, said from the values that judge, so that the words cannot differ from them.
fn limits() -> String {
    format!(
        "Limits (growth of the cost that asks for the cause / that is refused): memory \
         +{:.0}% / +{:.0}%, executable size +{:.0}% / +{:.0}%, CPU time, GPU time and wake-ups \
         +{:.0}% / +{:.0}%, times +{:.0}% / +{:.0}%. \
         A number that should stay nothing (heap kept after a screen is closed, heap growth \
         while the window runs, bytes written) is judged by how much more of it there is. \
         Synthetic local workloads on the machine that ran both sides: not a device or \
         acceptance result.\n",
        MEMORY.0, MEMORY.1, SIZE.0, SIZE.1, CPU.0, CPU.1, TIME.0, TIME.1
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row<'a>(comparison: &'a Comparison, path: &str) -> &'a Row {
        comparison
            .rows
            .iter()
            .find(|row| row.path == path)
            .unwrap_or_else(|| panic!("no row {path}"))
    }

    #[test]
    fn shared_numbers_are_compared_and_samples_and_context_are_not() {
        let before = json!({
            "replay": {"metrics": {"view.of": {"count": 200, "median_us": 10.0}}},
            "transport": {
                "encrypted_loopback_mib_s": {"samples": [1.0, 2.0], "median": 100.0},
                "history": [{"records": 1, "update_ms": {"median": 0.0}}],
            },
            "gone": {"median_us": 1.0},
        });
        let now = json!({
            "replay": {"metrics": {"view.of": {"count": 300, "median_us": 12.0}}},
            "transport": {
                "encrypted_loopback_mib_s": {"samples": [3.0], "median": 50.0},
                "history": [{"records": 1, "update_ms": {"median": 2.0}}],
            },
            "new": {"median_us": 1.0},
        });
        let comparison = Comparison::of(&[before], &[now]);
        assert_eq!(comparison.rows.len(), 3);
        let view = row(&comparison, "replay.metrics.view.of.median_us");
        assert_eq!(view.change_shown(), "+20.0%");
        // 20% slower is under the limit for a time.
        assert_eq!(view.verdict, Some(Verdict::Ok));
        // Half the throughput is twice the cost: refused, though the number itself fell.
        let rate = row(&comparison, "transport.encrypted_loopback_mib_s.median");
        assert_eq!(rate.change_shown(), "-50.0%");
        assert_eq!(rate.verdict, Some(Verdict::Reject));
        // A change from nothing has no percentage and is still a new cost.
        let update = row(&comparison, "transport.history[records=1].update_ms.median");
        assert_eq!(update.change_shown(), "+2.00 ms");
        assert_eq!(update.verdict, Some(Verdict::Review));
        assert_eq!(comparison.verdict(), Verdict::Reject);
    }

    #[test]
    fn memory_that_doubles_is_refused_and_a_tenth_more_asks_for_the_cause() {
        let run = |resident: u64, anonymous: u64| {
            json!({"window_quiet": {"process": {
                "peak_resident_kib": resident,
                "peak_anonymous_kib": anonymous,
            }}})
        };
        let comparison = Comparison::of(&[run(100_000, 40_000)], &[run(200_000, 45_000)]);
        assert_eq!(
            row(&comparison, "window_quiet.process.peak_resident_kib").verdict,
            Some(Verdict::Reject)
        );
        assert_eq!(
            row(&comparison, "window_quiet.process.peak_anonymous_kib").verdict,
            Some(Verdict::Review)
        );
        assert_eq!(comparison.verdict(), Verdict::Reject);
        let markdown = comparison.markdown();
        assert!(markdown.starts_with("## Resource impact: REGRESSION"));
        assert!(
            markdown
                .contains("| `window_quiet.process.peak_resident_kib` | 97.7 MiB | 195.3 MiB | +100.0% | - | **REJECT** |")
        );
        // Less memory is never flagged, and neither is a large percentage of very little.
        let less = Comparison::of(&[run(200_000, 1000)], &[run(100_000, 2900)]);
        assert_eq!(less.verdict(), Verdict::Ok);
    }

    #[test]
    fn a_change_within_the_noise_of_the_runs_is_not_flagged() {
        let run = |cpu: f64| json!({"replay": {"process": {"cpu_ms": cpu}}});
        // The base's own runs differ by 40%: 30% more says nothing.
        let noisy = Comparison::of(&[run(1000.0), run(1400.0)], &[run(1300.0), run(1350.0)]);
        let cpu = row(&noisy, "replay.process.cpu_ms");
        assert_eq!((cpu.base, cpu.head), (1000.0, 1300.0));
        assert_eq!(cpu.verdict, Some(Verdict::Ok));
        // Steady runs: the same 30% asks for the cause, and 60% is refused.
        let steady = Comparison::of(&[run(1000.0), run(1010.0)], &[run(1300.0), run(1310.0)]);
        assert_eq!(
            row(&steady, "replay.process.cpu_ms").verdict,
            Some(Verdict::Review)
        );
        let refused = Comparison::of(&[run(1000.0), run(1010.0)], &[run(1600.0), run(1650.0)]);
        assert_eq!(refused.verdict(), Verdict::Reject);
    }

    #[test]
    fn memory_that_is_not_given_back_is_judged_by_its_amount() {
        let run = |kept: f64| {
            json!({"replay": {"metrics": {"screen.open_and_draw.peers": {
                "retained_bytes_per_open": kept,
            }}}})
        };
        let path = "replay.metrics.screen.open_and_draw.peers.retained_bytes_per_open";
        for (kept, verdict) in [
            (0.0, Verdict::Ok),
            (512.0, Verdict::Ok),
            (4096.0, Verdict::Review),
            (100_000.0, Verdict::Reject),
        ] {
            let comparison = Comparison::of(&[run(0.0)], &[run(kept)]);
            assert_eq!(row(&comparison, path).verdict, Some(verdict), "{kept}");
        }
    }

    #[test]
    fn an_absent_counter_is_neither_zero_nor_a_comparable_sample() {
        let run = |gpu: Option<u64>| json!({"window_quiet":{"process":{"gpu_ms":gpu,"cpu_ms":10}}});
        let result = Comparison::of(&[run(Some(0)), run(None)], &[run(Some(4)), run(Some(8))]);
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.incomplete[0].1, [1, 2]);
        assert_eq!(result.incomplete[0].2, [Some(0.0), Some(4.0)]);
        let absent = Comparison::of(&[run(None)], &[run(None)]).markdown();
        assert!(absent.contains("GPU time · `window_quiet` | N/A | N/A | N/A"));
    }

    #[test]
    fn an_incomplete_scheduled_metric_prevents_a_pass_even_with_other_complete_metrics() {
        let run = |pss: Option<u64>| json!({"window_quiet": {"process": {"cpu_ms": 10, "peak_pss_kib": pss}}});
        let comparison = Comparison::of(
            &[run(Some(80_000)), run(None)],
            &[run(Some(81_000)), run(Some(82_000))],
        );
        let report = comparison.markdown();
        assert!(report.starts_with("## Resource impact: INCONCLUSIVE"));
        assert!(report.contains("Peak proportional memory (PSS) · `window_quiet`"));
        assert!(report.contains("| N/A | 1/2 · 2/2 |"));
        assert!(
            Comparison::of(&[run(None)], &[run(None)])
                .summary()
                .starts_with("INCONCLUSIVE")
        );
    }

    #[test]
    fn an_unavailable_optional_gpu_counter_does_not_make_a_software_run_incomplete() {
        let run = json!({"window_quiet": {"process": {"cpu_ms": 10, "gpu_ms": null}}});
        let report =
            Comparison::of(std::slice::from_ref(&run), std::slice::from_ref(&run)).markdown();
        assert!(report.starts_with("## Resource impact: PASS"));
        assert!(report.contains("GPU time · `window_quiet` | N/A | N/A | N/A"));
    }

    #[test]
    fn no_judged_samples_are_inconclusive() {
        let run = json!({"window_quiet": {"steady": {"frames_per_second": 30}}});
        assert!(
            Comparison::of(std::slice::from_ref(&run), std::slice::from_ref(&run))
                .summary()
                .starts_with("INCONCLUSIVE")
        );
    }

    #[test]
    fn canvas_frame_cost_and_rate_are_compared_in_the_right_direction() {
        let run = |fps, helper| {
            json!({"canvas": {"dense": {
                "achieved_fps": fps,
                "cpu": {"wasm_helper": {"cpu_ms_per_presented_frame": helper}},
            }}})
        };
        let result = Comparison::of(&[run(60, 0.5)], &[run(25, 1.5)]);
        assert_eq!(result.verdict(), Verdict::Reject);
        assert_eq!(
            row(&result, "canvas.dense.achieved_fps").verdict,
            Some(Verdict::Reject)
        );
        assert_eq!(
            row(
                &result,
                "canvas.dense.cpu.wasm_helper.cpu_ms_per_presented_frame"
            )
            .verdict,
            Some(Verdict::Reject)
        );
        assert!(
            result
                .markdown()
                .starts_with("## Resource impact: REGRESSION")
        );
    }

    #[test]
    fn the_program_table_uses_exact_process_data_and_reports_unavailable_gpu() {
        let run = |window_ms| {
            json!({
                "programs": {"peersus-ui": {"cpu_ms": window_ms, "peak_pss_kib": 40_000, "gpu_ms": null}},
                "size": {"peersus-ui": {"file_bytes": 1_048_576}},
                "window_replay": {"process": {"cpu_ms": 9_999}},
            })
        };
        let report = Comparison::of(&[run(100.0)], &[run(120.0)]).markdown();
        let program = report
            .lines()
            .find(|line| line.starts_with("| `peersus-ui` |"))
            .expect("one row for the window program");
        assert!(program.contains("100.00 ms → 120.00 ms"));
        assert!(program.contains("+20.00 ms (+20.0%)"));
        assert!(program.contains("not measured — no GPU driver counters"));
        assert!(program.contains("1.00 MiB → 1.00 MiB"));
        assert!(!program.contains("9999"));
        for name in ["peersus", "peersus-wasm-host", "peersus-wasm-canvas-host"] {
            assert!(
                report
                    .lines()
                    .any(|line| line.starts_with(&format!("| `{name}`")))
            );
        }
        assert!(report.contains("vary by runner"));
    }

    #[test]
    fn a_number_that_is_only_shown_never_decides() {
        let run = |frames: f64| json!({"window_replay": {"steady": {"frames_per_second": frames}}});
        let comparison = Comparison::of(&[run(10.0)], &[run(1000.0)]);
        assert_eq!(comparison.rows[0].verdict, None);
        assert_eq!(comparison.verdict(), Verdict::Ok);
        assert!(comparison.markdown().contains("| shown |"));
    }
}
