# Peersus review guide

## Where things belong

| Path | Responsibility |
|---|---|
| `crates/peersus-api` | Shared extension/app contract: data and validation only (no broker, processes, toolkit, OS, network, product concepts). |
| `crates/peersus-plugin*` | Extension runtime: broker, process supervision, frames, data connections. Speaks only the `peersus-api` contract. |
| `crates/peersus-core` | Product model, commands, pairing, settings. Knows no concrete extension. |
| `crates/peersus-net` | Identity, encrypted transport, transfer, QR codes. |
| `crates/peersus-app` | Runtime owner, CLI, local IPC, single instance, extension host. |
| `crates/peersus-ui` | Freya UI library. Renders `Snapshot` and contributions; display rules live in `src/view.rs`, which is toolkit-independent. |
| `crates/peersus-ui-android`, `crates/peersus-plugin-android` | Narrow, audited JNI adapters; the only crates allowed `unsafe`. |
| `android/` | Android app shell and platform adapters. |
| `extension/<name>/` (on `extension`) | One extension's source submission. |

## What to check

- **Agnostic host.** The host never branches on an extension's name, ID or settings. A capability an extension needs is a reusable API that any future extension could use, not renamed extension logic.
- **Hard isolation.** An extension can never stop, block, slow or disconnect the app. Its data is validated at the boundary with fixed user-facing messages; a failing extension is stopped or restarted alone.
- **Pairing contract.** Both people choose the same numbered circle independently; a mismatch refuses pairing. A file transfer never pairs a device implicitly.
- **Security semantics hold through every layer.** Masking text is not secret input; secrets stay out of logs, accessibility and clipboard.
- **Compatibility.** Changes to serialized forms keep older peers, older extensions and older app versions working or refuse them explicitly.
- **Honest scope.** Flag code paths with no consumer, duplicated helpers, and claims in READMEs or docs that the code does not implement.

## What not to flag

Formatting, naming taste, or suggestions to add features, abstractions or tests unrelated to the change.
