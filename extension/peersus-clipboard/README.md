# Clipboard extension

Explicit text sharing and permission-gated clipboard synchronization between paired devices, shipped as a WASM guest (official) for Linux, Android, macOS and Windows; the same source also builds a native library and a process (not recommended). Peer access is brokered by the application; the extension cannot choose an arbitrary network destination. Outbound success requires the application's matching delivered-byte acknowledgement, not merely a successful local socket write. This confirms transport delivery, not that a remote user has read the text.

## Platforms and dependencies

The extension never touches the system clipboard itself. It asks the application (`peersus.clipboard`), which reads and replaces this device's clipboard only while the person allows it, a permission of the application separate from this extension's `clipboard.use`. The paired devices it may reach come from `peersus.peers`, its settings from `peersus.config`.

- Linux: the application's clipboard owner needs a working X11 or Wayland session; compositor support affects background observation.
- macOS: the application's pasteboard adapter. An X server does not isolate it from the real pasteboard.
- Android: the application's own adapter (foreground reading, the share sheet, and the optional privileged helper of the community build).
- Windows: the application's desktop clipboard adapter reads and writes text.

Build requirements are a stable Rust toolchain with the `wasm32-unknown-unknown` target; the guest has its own lockfile (`wasm/Cargo.lock`) and depends on no application crate. This plugin is licensed under Apache-2.0; keep the exported LICENSE and NOTICE files. `submission.toml` declares the build recipe; CI resolves and records the trusted host SDK commit before executing it.

## Execution format

The **WASM guest is the official build** of this extension and is packaged for Linux, Android, macOS and Windows; the store requires the first three. Why WASM: one module, byte-identical on every platform, runs in an isolated interpreter that has no files, network, processes or OS calls of its own, only the application's brokered services the user granted, with bounded memory and work; a crash or hang ends only its worker. A native library (`.so`, second tier, not recommended) is only crash-isolated and runs with the application user's OS privileges; a process binary (third tier, not recommended) is an arbitrary program for which the application guarantees nothing beyond starting and stopping it.

The same guest source also builds as a native library (`cargo build --release --features native` in `wasm/`, packaged with `--native-library libpeersus_clipboard_wasm.so` as `clipboard-linux-native_library.tar.gz`) and, linked into the native host, as a process (`clipboard-linux-process.tar.gz`); every submission builds and validates both. They speak the identical protocol through the native host's function table instead of WebAssembly imports, so behaviour is the same; only the isolation is weaker. The store installs the WASM build only: the other two travel with the signed release for a person to install by hand, with limited support.

On macOS and Windows, the host uses periodic clipboard snapshots for automatic synchronization instead of native copy notifications. A copy replaced before the next snapshot can be missed. The Windows package is the same module as on the other platforms and the application's Windows path compiles; it has not been run on a Windows device.

## Build and test

In `wasm/`:

```sh
cargo build --release --target wasm32-unknown-unknown --locked
cargo test --locked
```

The guest's unit tests drive its protocol with scripted host answers: framing, bounds, cancellation, permission changes, automatic synchronization and delivery confirmation, without a clipboard.

The application carries the opt-in end-to-end test of this guest: two real owners on private virtual X displays pair and keep their clipboards in step through it, both ways (`crates/peersus-app/tests/desktop_clipboard_wasm.rs`). It needs **Xvfb**, **xclip**, and the trusted host SDK checkout containing `crates/peersus-app`. Start in this submission's `extension/peersus-clipboard/wasm/` directory (or the `wasm/` directory of a local copy). Set `sdk` to the SDK checkout's absolute path:

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
