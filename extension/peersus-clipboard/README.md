# Clipboard extension

Explicit text sharing and permission-gated clipboard synchronization between paired devices, shipped as a WASM guest (official) for Linux, Android, macOS and Windows; the same source also builds a native library and a process (not recommended). Peer access is brokered by the application; the extension cannot choose an arbitrary network destination. Outbound success requires the application's matching delivered-byte acknowledgement, not merely a successful local socket write. This confirms transport delivery, not that a remote user has read the text.

## Platforms and dependencies

The extension never touches the system clipboard itself. It asks the application (`peersus.clipboard`), which reads and replaces this device's clipboard only while the person allows it, a permission of the application separate from this extension's `clipboard.use`. The paired devices it may reach come from `peersus.peers`, its settings from `peersus.config`.

A missing local grant is returned by name; a paired device's refusal is only **not accepted**, whether it arrives before the stream or in its acknowledgement. The same copy is never retried. A refused reset or read pauses automatic synchronization without backoff. A relevant grant or consent change can establish a fresh baseline, not send text from before the change. A transient reset failure still uses a bounded backoff. Read and write grants are independent.

- Linux: the application's clipboard owner needs a working X11 or Wayland session; compositor support affects background observation.
- macOS: the application's pasteboard adapter. An X server does not isolate it from the real pasteboard.
- Android: the application's own adapter (foreground reading, the share sheet, and the optional privileged helper of the community build).
- Windows: the application's desktop clipboard adapter reads and writes text.

Build requirements are a stable Rust toolchain with the `wasm32-unknown-unknown` target; the guest has its own lockfile (`wasm/Cargo.lock`) and depends on no application crate. This plugin is licensed under Apache-2.0; keep the exported LICENSE and NOTICE files. `submission.toml` declares the build recipe; CI resolves and records the trusted host SDK commit before executing it.

## Execution format

The **WASM guest is the official build** of this extension and is packaged for Linux, Android, macOS and Windows; the store requires the first three. The same module ships on every platform. It runs in a separate worker under an interpreter that bounds work (fuel) and memory, handles guest traps, and exposes only the application's brokered services. The desktop worker is not itself an OS sandbox.

The same guest source also builds as a Linux native library (`cargo build --release --features native` in `wasm/`, packaged as `clipboard-linux-native_library.tar.gz`) and, linked into the native host, as a Linux executable (`clipboard-linux-process.tar.gz`). Both use the same clipboard logic and brokered control and data protocol through the native function table. Broker permissions and I/O bounds still apply, but neither gets the interpreter's fuel or memory limits, guest-memory checks, or trap handling. The `.so` runs in a separate helper for crash isolation; the executable is supervised as a process. Both run with the application user's OS privileges. These optional formats are not recommended and are for manual installation only; the store installs only WASM.

The macOS and Windows host adapters use periodic clipboard snapshots for automatic synchronization instead of native copy notifications. A copy replaced before the next snapshot can be missed.

## Build and test

In `wasm/`:

```sh
cargo build --release --target wasm32-unknown-unknown --locked
cargo test --locked
```

The guest's unit tests drive its protocol with scripted host answers: framing, bounds, cancellation, permission changes, automatic synchronization and delivery confirmation, without a clipboard.

A Linux smoke test verified startup and a brokered share refusal in all three formats; it did not exercise clipboard synchronization. The Android, macOS and Windows archives were validated as packages without running the guest on those platforms in that check. The Windows x64 host was also type-checked.

The application carries the opt-in end-to-end test of this guest: two real owners on private virtual X displays pair and keep their clipboards in step through it, both ways (`crates/peersus-app/tests/desktop_clipboard_wasm.rs`). **It runs only on Linux**: the test is compiled out on every other system, where the command below passes with zero tests and verifies nothing. It needs **Xvfb**, **xclip**, and the trusted host SDK checkout containing `crates/peersus-app`. Start in this submission's `extension/peersus-clipboard/wasm/` directory (or the `wasm/` directory of a local copy). Set `sdk` to the SDK checkout's absolute path:

```sh
sdk=/absolute/path/to/peersus-sdk
manifest="$PWD/plugin.json"
cargo build --locked --release --target wasm32-unknown-unknown --target-dir "$sdk/target/clip-wasm-rel"
cd "$sdk"
cargo build --locked -p peersus-plugin-runtime --bin peersus-wasm-host --target-dir "$sdk/target"
PEERSUS_DESKTOP_WASM_TEST=1 PEERSUS_CLIPBOARD_MANIFEST="$manifest" \
  cargo test --locked -p peersus-app --features test-plugin,test-identity --target-dir "$sdk/target" \
    --test desktop_clipboard_wasm -- --test-threads=1
```

The owners use the test build's identity (no system keyring), as the application's gate builds its tests; the package is installed as a person installs one from a folder, its licence, notice and the outside-store warning accepted by digest. It never touches the display or data of the person running it; a run without the flag returns at once and is not a clipboard verification.

## Submission

The exported source lives in its own extension directory with `submission.toml`, `ownership.json`, the `wasm/` guest (manifest, sources, lockfile and translations), this README and license notices. Follow the extension branch's root guide for its standalone CI command and permitted source changes. Match the host SDK to the outbound acknowledgement contract. Passing build and artifact validation do not grant catalog admission: each update still requires the independent maintainer review and verified ownership checks.

## License and third-party notices

This extension is licensed under Apache-2.0 (`LICENSE`, `NOTICE`). The guest is built from these libraries: serde, serde_derive, serde_core, serde_json, itoa, memchr, zmij (MIT OR Apache-2.0, MIT, or Unlicense OR MIT), with build-time proc-macro2, quote, syn and unicode-ident ((MIT OR Apache-2.0) AND Unicode-3.0). The native library and the process are the same guest; the process also links the SDK's native host (`crates/peersus-plugin-runtime/native/host.c`, Apache-2.0).
Every packaged archive carries `LICENSE`, `NOTICE`, `THIRD_PARTY_NOTICES.txt` and the license texts of its exact resolved dependency set under `licenses/`, generated by the packager from the lockfile; keep them when redistributing.
