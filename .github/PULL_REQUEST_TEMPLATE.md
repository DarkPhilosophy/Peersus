**Submitting an extension?** Target the `extension` branch and add `template=extension.md` to the pull-request form's URL query. Use the dedicated extension template to describe its purpose, usage, permissions, configuration and verification—not just packaging or CI.

## Problem and scope

Describe the user-visible problem and the evidence for its cause. Identify affected platforms and contracts.

## Change

Describe the behavior change and important tradeoffs. Separate implementation from unverified assumptions.

## Verification

List actual commands/results and runtime checks. State missing prerequisites or untested platforms explicitly. For UI changes, include narrow-layout and long-label evidence using non-sensitive data.

## Before / after

Required for interface or behavior changes. Fill N/A with a reason when nothing visible moved.

| Before | After |
| --- | --- |
| Attach a screenshot, recording or exact rendered description at this change's own base commit | Attach the same capture or description at this change |

Images must come from public synthetic fixtures (the `demo` feature's read-only snapshot, a screenshot harness, `PEERSUS_PAGE`)—never a real peer's device data—and the commit SHA each capture belongs to must be stated, so reviewer and reader can reproduce both sides rather than trusting a summary.

## Performance

Required when the change touches a hot path or makes a performance claim. Write `N/A — <reason>` otherwise (e.g. docs-only, no hot path touched).

| Method | Workload | Baseline | After | Delta |
| --- | --- | --- | --- | --- |
| Tool and flags used (e.g. `replay_bench --iters=200`) | Snapshot size / transfer count / scenario | Median + p95 at base revision | Same harness, this change | Absolute + relative |

State the machine/OS, build profile and feature set, base-commit SHA, warmup and sample counts, and whether the numbers are synthetic or a real device. A claim without a comparable same-harness baseline is not a claim. `cargo run --release -p peersus-ui --example replay_bench --features demo` measures the view-refresh and snapshot-codec path only; real-device network, disk and end-to-end latency are separate measurements and must be labeled as such.

## Verification matrix

| Environment | Exercise | Result |
| --- | --- | --- |
| Real device / OS version | What was actually run | Outcome or UNTESTED |
| Synthetic (demo/replay/simulator) | What was exercised | Outcome |

Mark clearly which results are real-device and which are synthetic. A synthetic check is not device acceptance: say what stays unverified on real hardware (pairing, transfers, suspend/resume, edge cases such as empty/long inputs, permission prompts).

## Risks and blockers

State what could go wrong if this merges: migration, compatibility, security boundary, or behavior regressions—and what blocks completion (unverified platforms, missing prerequisites, follow-ups owned by whom).

## Review checklist

- [ ] Related callers and API documentation match the change.
- [ ] Relevant behavior and failure-path checks ran.
- [ ] No credentials, private device data or generated build artifacts are included.
- [ ] Permission, cancellation and extension trust boundaries are preserved or explicitly explained.
- [ ] Performance claims include comparable measurements, if applicable.

Publication, signing and deployment are separate actions; this change description does not authorize them.
