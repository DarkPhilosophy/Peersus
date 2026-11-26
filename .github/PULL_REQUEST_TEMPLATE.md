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

## Resource impact

Required for every change to code, dependencies or build settings. Write `N/A — <reason>` only when nothing that is compiled or shipped changed (documentation, translations, workflow text).

Before CI, name the paths you expect to change and why (for example startup CPU, PSS, window main-thread time, helper or Canvas cost). Record the current-head `Resource impact` run link and overall verdict after the comparison; the job summary is authoritative. CI uses the exact base and head SHAs and runs three interleaved samples per side on one runner. Forks need a maintainer's `run:ci` label.

| Metric / method | Baseline | After | Delta | Verdict |
| --- | --- | --- | --- | --- |
| Copy every **REVIEW** or **REJECT** row, or report **PASS** or **INCONCLUSIVE** with the reason | | | | |

- **REVIEW**: name the cause of each row: what allocates, links, runs or wakes more, and why this change needs it. Growth nobody can explain is not accepted.
- **REJECT**: the change is not accepted as it is. Find the cause and remove it; memory that is not given back is a defect, never a cost to explain. Only a maintainer who read the cause here can accept it, with the label `impact:accepted`.
- **INCONCLUSIVE**: say which measurement or baseline is missing. Do not claim a pass or fill a missing metric with zero. Reproduction and limits: [Resource impact](../docs/development.md#resource-impact).

The workloads are synthetic and local. A claim about anything they do not exercise (a real device, the network, an extension, end-to-end latency) needs its own comparable measurement: state the machine/OS, build profile and feature set, base-commit SHA, warmup and sample counts, and whether it is synthetic or a real device. A claim without a same-harness baseline is not a claim.

<!-- peersus-benchmark:start -->
<!-- CI replaces this section with measurements for the current head. Keep your own notes outside it. -->
<!-- peersus-benchmark:end -->

## Verification matrix

| Device/model, OS version or synthetic setup | Build/revision and scenario exercised | Evidence/result |
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
- [ ] The resource report is for this head. Every flagged metric or inconclusive result is explained; any blocking regression has been resolved or explicitly accepted.

Publication, signing and deployment are separate actions; this change description does not authorize them.
