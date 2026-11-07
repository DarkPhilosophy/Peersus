# Control extension

One extension identity with one WASM guest for Linux and Android. A phone becomes a touchpad and keyboard for a paired computer or another paired phone; a computer arranges its displays and the paired devices around them on a Layout page.

## Platforms and prerequisites

- **Android (sending):** the Remote page sends pointer, button, scroll and text input to the chosen paired device.
- **Android (receiving):** the application's accessibility service replays input on the phone. It must be on and connected; the application turns it on and keeps it connected through its own switch.
- **Linux (receiving):** the application's input service replays input; with "Independent pointer per device", each controlling device gets its own pointer on desktops that draw one per tablet tool (such as GNOME), elsewhere the shared pointer moves.
- Every direction needs the per-device `control.use` permission on both devices, and the receiving device's `input.inject` permission of the application.

The guest needs a host with `peersus_v1.poll`, `peersus.pages`, `peersus.displays` and `peersus.input`. An older host without them is not compatible: update host and guest together.

The source is Apache-2.0 licensed; retain LICENSE and NOTICE in exported submissions. `submission.toml` declares the build, test and package commands; the WASM guest has its own lockfile.

## Build and test

```sh
cargo test --manifest-path wasm/Cargo.toml --locked
cargo build --manifest-path wasm/Cargo.toml --locked --release --target wasm32-unknown-unknown
```

The tests cover the wire format, stream ordering across several controlling devices, cancellation, layout geometry and page rendering. They do not drive a real device; check input on the real devices separately.

## Submission

One source directory and one ownership identity. Include this README, the WASM source and Cargo files, the manifest, `submission.toml`, `ownership.json` and license notices. Passing CI is not catalog publication.
