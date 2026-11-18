# Test strategy

What is tested, by what, where it runs, and what no test here shows. It describes the repository as it is; a kind of test that does not exist yet is listed under "Gaps", not described as if it ran. Findings and their evidence are in the [engineering log](engineering-log.md).

## Levels

| Level | What it is evidence of | Where it lives | Runs in the gate |
| --- | --- | --- | --- |
| Unit | One rule: validation, a state transition, a parser, a view decision. No process, socket or display. | `#[cfg(test)]` modules beside the code, in every crate | yes |
| Integration | Real parts together: a settings store on disk, two endpoints on the loopback, a broker with a real extension process, the owner with a test identity. | `crates/*/tests/` | yes, except the tests marked ignored (below) |
| Interface, no display | The real root component, layout, drawing and input handling on the demo's synthetic snapshot, drawn by the CPU into memory. Includes the guard that no text is broken inside a word on any screen, at the narrowest widths, in every language (the rules it enforces are in `peersus-ui/src/text.rs`). | `crates/peersus-ui/src/headless/tests.rs`, `demo` tests | yes |
| Interface on a real owner, no display | The same window on an owner that runs in the test's own process with an identity of its own, paired with a second one (`Session::paired_for_test`), where the demo cannot go because it hands nothing over: files dropped on the window, what the page then does, and what the two owners record. The window runs on a thread of its own and is given a time, so a window that stops answering fails the test instead of hanging it. | `crates/peersus-ui/src/headless/tests.rs` (`files_listed_on_send_…`) | yes |
| Tooling | The build, packaging, deploy, catalog and extension-submission scripts, on fixtures. | `scripts/test-*.py` | yes (`metadata`) |
| Pictures | Every screen and Settings tab as a PNG at desktop and phone width, for a person to compare before and after an interface change. | `cargo run -p peersus-ui --example demo_shots --features headless -- DIR` | no: a person looks |
| Measurement | Replay cost, first frame, redraws, CPU and memory of the demo window, against a kept baseline. | `./dev.sh bench` (see [development](development.md), "Measuring") | no: on request |
| Device and platform | The installed application on a real phone, a real desktop session, the Windows machine. | by hand, recorded in the engineering log | no |

The gate is `./check.sh` (`cargo xtask gate`): formatting, tooling tests and generated-documentation check, `cargo check`, `clippy -D warnings`, the workspace tests, the headless interface tests, the owner's integration tests with a test identity, and the dependency audit. CI runs the same command on Linux.

## Regression

A defect that is fixed gets a test that fails without the fix, at the lowest level that can show it, named for the behavior and not for the defect's number. Where the defect cannot be reproduced on demand (a failure seen once under load), the entry in the engineering log says so and the test is described as what it is: evidence that the new code holds, not that the old code failed.

An interface defect that the headless window can show (a control that does nothing, a text that is not said, a state that is not put back) is tested there. One that only a real compositor, GPU, keyboard or phone can show is not claimed as tested.

## Extension isolation and stress

The host must stay responsive whatever an extension does. What exists:

- A real extension process that aborts, exits with an error, hangs, ignores a restart, or leaves children behind is contained, restarted within a budget or given up on, and nothing else is affected (`peersus-plugin/tests/isolation.rs`, `external_lifecycle.rs`).
- Time limits a plugin cannot extend for itself; a grant withdrawn while bytes are moving; a stream from a device without the grant; an action pressed twice (`peersus-plugin/tests/host_deadlines.rs`, `peersus-app/tests/plugins.rs`).
- One subscriber that stops reading does not cost the others their events; a write to an extension that does not read ends within a limit (engineering log E-009).
- The interpreter bounds a guest's memory and fuel (`peersus-plugin-runtime`).

## What needs something the gate does not have

These tests exist and are marked ignored, each with its reason in the source. They are run on purpose, with what they need, and the run is recorded:

| Needs | Tests |
| --- | --- |
| A dedicated Android emulator and a person to consent in its interface | `peersus-app/tests/emulator_e2e.rs` (pairing, nested folders, many files, background receive and notifications) |
| The public or the project relay | `peersus-net/tests/public_relay.rs`, one test of `relay_policy.rs` |
| A built extension guest named by an environment variable | upload and control guests in `peersus-plugin-runtime` and `peersus-app` |
| An interactive desktop, `/dev/uinput`, an isolated X server, a desktop D-Bus session | desktop input, the drawn cursor, the system theme |
| An interactive Windows desktop | `displays.rs` |
| Permission to use the system credential store | `identity.rs` |

## Devices

- A change to the extension host, the protocol or the interface is installed and tried on the desktop and on each available phone before it is called working. Installing is `./dev.sh deploy`, after a fresh `./dev.sh android`; it replaces the application and never removes it or its data.
- What is tried on a phone for such a change: the application with no extension and with each one installed; permissions switched repeatedly; restart; the phone locked and in the background; an extension newer than the application and the reverse where both builds exist; an extension that floods, answers late, answers nonsense or dies. The application must stay responsive and connected throughout.
- The level reached is written exactly: compiled, installed, checksum verified, behavior observed, or blocked. A result on one phone is not a result on the other.

## Gaps

Known, and not covered by anything above:

- No rate limit or temporary suspension per extension exists, so there is no test of one (engineering log A-04); the flood case on a device is therefore an observation of the present behavior, not a pass.
- No end-to-end test runs without a person: the emulator tests need consent in the interface.
- Nothing in the repository measures a GPU surface, a paced display, or allocations. A phone was measured by hand with the system's sampling profiler (engineering log E-019 … E-023); no command here repeats that.
- Windows and macOS have no automated test in the gate; Windows is built and tried by hand in a virtual machine, macOS was built and its tests run by hand in a virtual machine (2026-11-12, see [macOS evaluation](macos-evaluation.md)): the window was seen there once, drawn by the software renderer, and nothing ran on a Mac.
- The hosted gate has not finished a run (engineering log E-025): what the gate shows, it shows on a workstation.
- Translations are checked for presence and placeholders, not for meaning.
- The file dialogs, the system clipboard and the credential store are not driven by any automated test.
