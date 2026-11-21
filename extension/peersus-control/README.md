# Control extension

One extension identity with one WASM guest for Linux, Android, macOS and Windows. A phone becomes a touchpad and keyboard for a paired computer or another paired phone. Desktop input and display features depend on the host platform.

## Platforms and prerequisites

- **Android (sending):** the Remote page sends pointer, button, scroll and text input to the chosen paired device.
- **Android (receiving):** the application's accessibility service replays input on the phone. It must be on and connected; the application turns it on and keeps it connected through its own switch.
- **Linux (receiving):** the application's input service replays input; with "Independent pointer per device", each controlling device gets its own pointer on desktops that draw one per tablet tool (such as GNOME), elsewhere the shared pointer moves.
- **Windows (receiving):** the host replays input through SendInput on the shared mouse pointer. The Layout editor can read and save the display arrangement; Windows does not capture outbound input at display edges.
- **macOS (receiving):** the host replays input on the shared pointer through Quartz, with macOS Accessibility permission. Native display discovery is not wired on macOS.
- Every direction needs the per-device `control.use` permission on both devices, and the receiving device's `input.inject` permission of the application.

The guest needs a host with `peersus_v1.poll`, `peersus.pages`, `peersus.displays` and `peersus.input`. While "Independent pointer per device" is on, a receive also asks `peersus.input`'s `inspect` whether this computer can draw a pointer per device; a host without the method refuses the receive instead of pretending. An older host without these is not compatible: update host and guest together.
New incoming connections are refused if the host cannot read the pointer settings; defaults apply only to options absent from a valid settings response. If a replacement is refused and no accepted connection can take over after rotation, Control asks the host to release the input held by that controller.

The source is Apache-2.0 licensed; retain LICENSE and NOTICE in exported submissions. `submission.toml` declares the build, test and package commands; the WASM guest has its own lockfile.

## Choosing the device to control

The Remote page has one layout, with or without a device: the buttons, the pad, the wheel and the keyboard stay where they are, and the pad reaches the edges of the page. The device is chosen in the application's own chooser (the `devices` page element of `peersus-api`), in the pad's lower left corner: the chosen device's dot, picture and name, and, pressed, the application's menu of every paired device Control may see, each with its picture, name and how it is reachable. Control asks it for devices that are online (a sleeping screen has nobody to take input), allowed, and where Control is there and allowed to take this device's input (`accepts` names `control.pointer`); the application dims the others and says why (SLEEPING, OFFLINE, NOT ALLOWED, NOT READY THERE). When the chosen device stops qualifying, or is unpaired, what was held on it is let go and it is unselected; the page then says which device and why beside the chooser, and a press on the pad or a key sends nothing. That same device is chosen again by itself once it can be controlled again, unless another was chosen meanwhile; control is never moved to another device by itself. The list comes from the application's `peersus.peers` `list` (`presence`, `allowed`, `accepts`), read every two seconds; with a host that does not say them yet, a device is offered by `online` alone, as before.

A stationary tap clicks the half the finger pressed; a swipe immediately after a tap can drag with the previous tap's button. A keyboard submission that closes its connection reports an error if the other device does not confirm delivery. While the Remote page is open, a lost lift clears the held control and its highlight after 30 seconds without gesture activity.

Desktop forwarding ages a connection from the application's timestamps for that stream. If unrelated work interrupts the wait, the guest may rotate early rather than let the receiver's call expire. A successful rotation preserves held input on the same device. Before rotating on readiness, the guest checks buffered and already-readable control messages for an end or switch that can release the input on the original connection.

## Execution format

The **WASM guest is the official build** of this extension. The submission packages it for Linux, Android, macOS and Windows (`control-windows.tar.gz`) from the same WebAssembly binary. The module runs in an isolated interpreter that has no files, network, processes or OS calls of its own, only the application's brokered services the user granted, with bounded memory and work; a crash or hang ends only its worker.

The same guest source also builds as a native library (`cargo build --release --features native` in `wasm/`, packaged with `--native-library libpeersus_control_wasm.so` as `control-linux-native_library.tar.gz`) and, linked into the native host, as a process (`control-linux-process.tar.gz`); every submission builds and validates both. They speak the identical protocol through the native host's function table instead of WebAssembly imports, so behaviour is the same; only the isolation is weaker. The store installs the WASM build only: the other two travel with the signed release for a person to install by hand, with limited support.

## Build and test

```sh
cargo test --manifest-path wasm/Cargo.toml --locked
cargo build --manifest-path wasm/Cargo.toml --locked --release --target wasm32-unknown-unknown
```

The tests cover the wire format, stream ordering across several controlling devices, cancellation, layout geometry and page rendering. They do not drive a real device; check input on the real devices separately.

## Submission

One source directory and one ownership identity. Include this README, the WASM source and Cargo files, the manifest, `submission.toml`, `ownership.json` and license notices. Passing CI is not catalog publication.

## License and third-party notices

This extension is licensed under Apache-2.0 (`LICENSE`, `NOTICE`). The WASM guest is built from these libraries: serde, serde_derive, serde_core, serde_json, itoa, memchr, zmij (MIT OR Apache-2.0, MIT, or Unlicense OR MIT), with build-time proc-macro2, quote, syn and unicode-ident ((MIT OR Apache-2.0) AND Unicode-3.0). 
Every packaged archive carries `LICENSE`, `NOTICE`, `THIRD_PARTY_NOTICES.txt` and the license texts of its exact resolved dependency set under `licenses/`, generated by the packager from the lockfile; keep them when redistributing.
