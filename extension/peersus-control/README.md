# Control extension

The submission builds one WASM guest for Linux, Android, macOS and Windows. Control turns a phone into a touchpad and keyboard for a paired computer or phone. Desktop input and display features depend on the host.

## Platforms and prerequisites

The following describe the host adapters and their prerequisites, not results of device tests:

- **Android:** The Remote page is offered for sending input. The receiving adapter requires the application's accessibility service to be enabled and connected.
- **Linux:** The input adapter provides shared-pointer replay and can provide independent pointers on desktops that draw a pointer per tablet tool.
- **Windows:** The host has a shared-pointer SendInput adapter and publishes monitor geometry for Layout. Outbound capture at display edges is Linux-only.
- **macOS:** The host has a shared-pointer Quartz adapter that requires Accessibility permission. Native display discovery is not wired on macOS.
- Every direction needs the per-device `control.use` permission on both devices, and the receiving device's `input.inject` permission of the application.

The guest needs a host with `peersus_v1.poll`, caller-scoped `peersus.config` readiness and its change event, `peersus.pages`, `peersus.displays`, and `peersus.input`. Update host and guest together. While **Independent pointer per device** is on, a receive also asks `peersus.input.inspect` whether this computer can draw one pointer per device. New connections are refused if the host cannot read its pointer settings; defaults apply only to options absent from a valid settings response.

Control checks local permission readiness before it acts. For example, **Layout** can show the arrangement with `displays.watch` even if it cannot save without `displays.arrange`. A locally blocked option names the missing permission; Control does not poll or retry the refused operation. A remote refusal stops the current gesture without naming the other device's grant or reporting a transfer failure. Withdrawing a grant while controlling a device ends the gesture; restoring it does not resume the old gesture. Choose the device again in **Remote** or deliberately cross the edge again in **Layout**.

The source is Apache-2.0 licensed; retain LICENSE and NOTICE in exported submissions. `submission.toml` declares the build, test and package commands; the WASM guest has its own lockfile.

## Relative layout

In **Layout → Devices**, choose **Refit** for a paired device, then choose a side of this computer. The preview scales that device's monitors together to fit the whole side and labels their real resolutions. Entry and return use the same proportion of the corresponding edge: for example, leaving one quarter of the way down lands one quarter of the way down on the other device. Choose **Use real size** to return the device to its saved free placement. Attaching a device does not change its screen resolution.

## Crossing arranged screen edges

On a Linux desktop with a working InputCapture portal, place a paired device against a screen edge in Layout. Under **Settings → Extensions → Control**, enter **Edge pressure (motion units)** and save. The range is **0–400**, with **80** as the default. A positive value requires an extra push beyond the shared edge, both when you leave and when you return. Back away or stop to cancel that push. **0** uses the first eligible edge crossing without measuring an extra push.

These are relative mouse-motion units, not a physical distance or a dwell time. This is a setting on the Linux desktop that captures the mouse. It applies to a visit to a supported Linux, Windows or Android receiver without a matching setting there, and does not affect the Remote touchpad or ordinary input replay. Windows, Android and macOS cannot initiate arranged edge capture. macOS also does not currently publish the geometry needed for a layout visit. Hold both Control keys to take the pointer back regardless of the pressure setting.

## Choosing the device to control

The Remote page has one layout, with or without a device: the buttons, the pad, the wheel and the keyboard stay where they are, and the pad reaches the edges of the page. The device is chosen in the application's own chooser (the `devices` page element of `peersus-api`), in the pad's lower left corner: the chosen device's dot, picture and name, and, pressed, the application's menu of every paired device Control may see, each with its picture, name and how it is reachable. Control asks it for devices that are online (a sleeping screen has nobody to take input), allowed, and where Control is there and allowed to take this device's input (`accepts` names `control.pointer`); the application dims the others and says why (SLEEPING, OFFLINE, NOT ALLOWED, NOT READY THERE). When the chosen device stops qualifying, or is unpaired, what was held on it is let go and it is unselected; the page then says which device and why beside the chooser, and a press on the pad or a key sends nothing. That same device is chosen again by itself once it can be controlled again, unless another was chosen meanwhile; control is never moved to another device by itself. The list comes from the application's `peersus.peers` `list` (`presence`, `allowed`, `accepts`), read every two seconds; with a host that does not say them yet, a device is offered by `online` alone, as before.

A stationary tap clicks the half the finger pressed; a swipe immediately after a tap can drag with the previous tap's button. A keyboard submission waits for the other device to acknowledge receipt even while a control is held. A checked refusal is shown as a state instead of a delivery error. If a Backspace lift is lost, repeating stops 30 seconds after the press even while the wheel is in use. A pad or wheel gesture with no activity for 30 seconds is also ended.

Desktop forwarding ages a connection from the application's timestamps for that stream. If unrelated work interrupts the wait, the guest may rotate early rather than let the receiver's call expire. A successful rotation preserves held input on the same device. Before rotating on readiness, the guest checks buffered and already-readable control messages for an end or switch that can release the input on the original connection.

## Execution format

The **WASM guest is the official build**. The Linux, Android, macOS and Windows archives contain the same WebAssembly module. It runs in a separate interpreter worker with no direct filesystem, network, process or OS access; external work goes through the brokered services the user granted. The interpreter limits memory and work per turn and reports guest traps without running the guest in the application owner.

The optional Linux archives are `control-linux-native_library.tar.gz` (a `.so` loaded by a separate helper) and `control-linux-process.tar.gz` (the Rust static library linked into the native host). These forms share Control's guest logic and brokered control and stream protocol, not the WASM runtime guarantees. Both run machine code with the application user's OS privileges. Broker permissions still apply to brokered calls, but neither native form is a sandbox or has the interpreter's per-turn fuel limit, linear-memory cap or trap handling. The store installs only WASM; the native archives are for manual installation.

## Build and test

```sh
cargo test --manifest-path wasm/Cargo.toml --locked
cargo build --manifest-path wasm/Cargo.toml --locked --release --target wasm32-unknown-unknown
```

Control keeps `opt-level = 3`: in ten paired runs of the release WASM guest with an in-memory host on Linux x86-64 (rustc 1.96.1, wasmi 2.0.2), a 12-display Layout drag reply took a median 4.18 ms versus 5.20 ms at `"s"`, at the cost of a larger module (464 kB versus 262 kB) and higher peak fuel (38.9M versus 35.0M in that workload).

Unit tests cover the wire format, stream ordering, cancellation, layout geometry and page rendering. The Linux audit smoke-tested startup and a page reply for WASM, native library and linked process using a synthetic broker; it did not drive a real input device. Archive validation covered all four WASM platforms but did not exercise the guest on Android, macOS or Windows.

## Submission

One source directory and one ownership identity. Include this README, the WASM source and Cargo files, the manifest, `submission.toml`, `ownership.json` and license notices. Passing CI is not catalog publication.

## License and third-party notices

This extension is licensed under Apache-2.0 (`LICENSE`, `NOTICE`). The WASM guest is built from these libraries: serde, serde_derive, serde_core, serde_json, itoa, memchr, zmij (MIT OR Apache-2.0, MIT, or Unlicense OR MIT), with build-time proc-macro2, quote, syn and unicode-ident ((MIT OR Apache-2.0) AND Unicode-3.0). 
Every packaged archive carries `LICENSE`, `NOTICE`, `THIRD_PARTY_NOTICES.txt` and the license texts of its exact resolved dependency set under `licenses/`, generated by the packager from the lockfile; keep them when redistributing.
