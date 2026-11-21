# Test extension

A page of its own, drawn by the application from plain elements, for the people who build Peersus and its extensions. It tries what the application offers an extension through its contract, the limits, and what the application must survive when an extension misbehaves. It is not a feature for anyone else.

## What it shows

- **Every element and event.** Text in every tone (title, body, muted, figure) and with a line limit, a button with its count, a switch, a text field whose sent words are shown, boxes in every colour of the contract and in several shapes (half seen, two round corners, scrolling, one laid over another, one that grows in), the application's own pictures, a drawing, rows that are added and cleared, dividers, and lines that ask to be drawn again by themselves (`interval`). A pad (a box with an id) reports raw pointer, size and wheel events, which the drawing follows; the application's device chooser reports the device chosen; a clock ticks only while the page is shown.
- **What the application answers.** One button per call the contract offers an extension, each showing the answer: its own settings (`peersus.config`), whether its licence was accepted and whether it came from outside the store (`peersus.extension`), the system (`peersus.platform`, `platform.read`), the person's language (`peersus.language`, `language.read`), the paired devices it may see with their pictures (`peersus.peers`, `peers.watch`), the other extensions (`peersus.plugins`, `plugins.watch`), a status of its own made, shown, hidden and listed (`peersus.status`, `status.define` and `status.set`), a second copy of its page in Settings (`peersus.pages`) and one of its own actions taken back (`peersus.actions`). Beside each, what the application should answer. A permission the person has not granted is answered `denied`, which is the right answer.
- **Notifications.** "Say something" asks for a notification inside the window; "Ask a question" asks for one that also reaches outside it, with two answers, and the page shows what was answered. Both need `notify.show`.
- **Key bindings.** Five actions a key can run: three only add to a count on the page, one with a long description and one with none, to show both on the Binds page; two ask for the two notifications. Three suggest a chord (`ctrl+alt+k`, `ctrl+alt+n`, `ctrl+alt+q`), bound when the extension is newly installed if the chord is free.
- **The limits.** "Too large" answers with a page nested far deeper than the application draws: it refuses that tree and keeps showing the page as it was.
- **Misbehaving on purpose**, behind the setting "Show the misbehaving buttons" (off by default). Buttons flood the application with operations or with events, answer nonsense, never answer one call, spin without end, fill its memory, or leave; beside each, what the application should do. Whatever it does, only this extension may suffer for it: the window stays responsive, its connection to the other devices stays up, and the other extensions keep running.

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

The tests cover the page (every element kind, tone and colour, and that the fullest page fits in one message), each probe's exact call and how its answer is shown, the setting that shows the misbehaving buttons, the pad, wheel, clock and chooser, notifications and key presses, the loop's answers, and the manifest against the code. They do not prove the application survives the trouble: that is the stress run on the real devices, with the application watched for staying responsive and connected throughout.

## License

Apache-2.0 (`LICENSE`, `NOTICE`), as the manifest's licence page says.
