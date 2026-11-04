# Clipboard extension

A supervised native-process extension for explicit text sharing and permission-gated clipboard synchronization. Peer access is brokered by the application; the extension cannot choose an arbitrary network destination. Outbound success requires the application's matching delivered-byte acknowledgement, not merely a successful local socket write. This confirms transport delivery, not that a remote user has read the text.

## Platforms and dependencies

- Linux: a working X11 or Wayland clipboard session. X11 and Wayland use their platform adapters; compositor support affects background observation.
- macOS: the native pasteboard adapter. An X server does not isolate this adapter from the real pasteboard.
- Android: the application provides its platform adapter; this native process package is not an Android installer.

Build requirements are the workspace's pinned Rust toolchain and SDK dependencies. This plugin is licensed under Apache-2.0; keep the exported LICENSE and NOTICE files. `submission.toml` declares the build recipe; CI resolves and records the trusted host SDK commit before executing it, using the SDK's lockfile. The source archive does not contain a standalone workspace or pin an SDK revision itself.

## Build and test

From the application workspace root:

```sh
cargo build -p peersus-clipboard
cargo test -p peersus-clipboard --all-targets
```

Unit tests exercise framing, bounds, cancellation, permission changes and delivery confirmation without changing the desktop clipboard. Linux full-runtime clipboard tests need **Xvfb**, **xvfb-run**, **xauth** and **xclip**. Their ordinary invocation returns without exercising the real clipboard unless the isolation flag is set. Run them explicitly in a private X server:

```sh
PEERSUS_ISOLATED_CLIPBOARD=1 xvfb-run -a env -u WAYLAND_DISPLAY \
  cargo test -p peersus-clipboard --test runtime_end_to_end -- --test-threads=1
```

Never set the isolation flag against a personal desktop session. The runtime tests install the actual extension into temporary application state, pair a synthetic local peer and check permitted and refused network transfers. Do not interpret a skipped/non-isolated run as desktop clipboard verification.

## Submission

The exported source lives in its own extension directory with `plugin.json`, `submission.toml`, `ownership.json`, Cargo files, this README and license notices. Follow the extension branch's root guide for its standalone CI command and permitted source changes. Match the host SDK to the outbound acknowledgement contract. Passing build and artifact validation do not grant catalog admission: each update still requires the independent maintainer review and verified ownership checks.
