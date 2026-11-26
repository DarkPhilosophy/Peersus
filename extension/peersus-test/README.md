# Test extension

Open Test in the extension's settings. Turn on **Add Test to page** to show the same page in the main navigation as well; turning it off leaves the settings page available. Test exercises the extension contract for developers, not as an end-user feature.

## What it shows

- **Elements and their effects together.** Text in every tone, buttons and switches with local feedback, a submitted field, colour and box samples, icons and an animated reveal. Added rows appear beside their Add button. The touch pad and its pointer/size/wheel words sit next to their drawing on a wide screen and above it on a narrow screen; the ring follows touch and the line follows scrolling (the triangle is a static sample). The device chooser and its answer share a section, as does the clock.
- **What the application answers.** In-place probes cover settings, agreement and installation source, platform and language, peers and plugins, and one status (`peersus.status`). Type and submit any allowed status label (default `Testing`) to define or reword it, show or hide it, reset its label, and read its on/off state on this page. An active status appears on paired devices' Devices pages. Interactive buttons that modify another page or the Binds screen are intentionally absent.
- **Device location.** Press **Check device location** to ask the application once for this device's current position. It needs `location.read` and a working system location service. Test validates the answer, then shows whether a position was received and its accuracy (if supplied), never the coordinates. It does not save, log, or send the position. The operating system decides how to find it and may use its own online location service.
- **Notifications.** "Say something" stays inside the app; "Ask a question" is in the app when it is foreground, or shown by the platform when the app is backgrounded and that platform permits it. Both need `notify.show` and the page shows the answer.
- **Permission outcomes.** A probe whose local grant is missing names it without calling the guarded service. Checked `missing-permission` and `not-accepted` answers are identified by name, not reported as a failed probe. A grant change updates readiness but does not rerun any probe.
- **Key bindings.** Five actions a key can run: three only add to a count on the page, one with a long description and one with none, to show both on the Binds page; two ask for the two notifications. Three suggest a chord (`ctrl+alt+k`, `ctrl+alt+n`, `ctrl+alt+q`), bound when the extension is newly installed if the chord is free.
- **The limits.** "Too large" answers with a page nested far deeper than the application draws: it refuses that tree and keeps showing the page as it was.
- **Misbehaving on purpose**, behind the setting "Show the misbehaving buttons" (off by default). These buttons test how the application handles floods, malformed messages, missing replies, and the extension leaving. **Spin** exhausts the WebAssembly interpreter's per-turn fuel; **Fill memory** tests its linear-memory limit. The native library and process have neither interpreter limit, so both buttons refuse to run those cases there and the page says why.

## Execution format

The **WASM guest is the official build**, declared for Linux, Android, macOS and Windows, and the one the store installs. The same source also builds as a native library (`cargo build --release --features native` in `wasm/`, packaged as `test-linux-native_library.tar.gz`) and, linked into the native host, as a process (`test-linux-process.tar.gz`); both are built and validated with every package, run with the application user's OS privileges and travel with the signed release for a person to install by hand, with limited support. `src/host.rs` and `src/native.rs` are the shared guest glue (`templates/guest/`), compared byte for byte by `scripts/check-guest-glue.py`.

## Build and test

From `wasm/` (`env -u CARGO_BUILD_TARGET` only where the environment exports a build target):

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release --target wasm32-unknown-unknown
cargo build --locked --release --features native
```

`submission.toml` is the full recipe: the WASM build for four platforms, the native library and the process, each validated on its own.

The tests cover the page (including its size limit), service probes, settings, input, notifications, key presses, and the guest loop. Native-target tests confirm that both interpreter-only cases return a refusal on the page and the guest still answers the next press. The tests do not prove that the application stays responsive and connected during deliberate WebAssembly stress; that requires running those cases on the real devices.

## License

Apache-2.0 (`LICENSE`, `NOTICE`), as the manifest's licence page says.
