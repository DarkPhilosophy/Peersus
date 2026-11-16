# Monitor extension

One extension identity with one WASM guest for Linux, Android, macOS and Windows. A page that says how a device is doing —
processor, graphics, memory, storage, network and battery, each in a card of its own — for this device, or for a paired
device that allows it. Every number is shown only after the person allowed it, part by part and device by device; a number
the system does not publish is shown as not available and is never estimated.

## What it shows

The page lives in this extension's settings; the setting "Add Monitor to page" also places it among the application's pages.
At the top is the application's own device chooser (the `devices` page element of `peersus-api`): "This device" first, by its
own picture and name, then every paired device Monitor may see. Monitor asks it for devices that are online or sleeping (a
sleeping device still answers), allowed, and where Monitor is there to answer (`accepts` names `monitor.readings`); the
application dims the others and says why. A chosen device that goes offline, or where Monitor stops being there, is unselected
and Monitor says why.

| Card | Shows |
|---|---|
| CPU | Its name (one line, cut with an ellipsis), how busy it is in a tile, a graph of the last minute, its clock and cores, its temperature over a bar coloured by warmth |
| GPU | Who made it, how busy it is, a graph of the last minute, its clock and its own memory used (VRAM), its temperature |
| Memory | An open ring of how much is used, the share large in it, used of total under it |
| Storage | The same, for the volume received files go to, else the application's own data, with the system's name of its disk where it has one |
| Network | A graph of the last minute, download and upload per second, total since the system started |
| Battery | A card like the others: its title at the top, the charge, "Charging" or "Discharging" and how long it will last at the bottom, and the whole card filled from its bottom as far as it is charged. A device with no battery (a desktop, which answers that it knows neither its charge nor whether it charges) has no battery card, and the other cards take its room; a battery not allowed still says "Not allowed" |

The application answers counters (processor ticks, network bytes); the shares and speeds are made here from two answers in a
row, so a first answer shows no rate. The page is read again once a second while it is open and stops when it is closed. A
card is never a fixed width: it is its share of the row it is in, so the rows always fill the page; the page's measured width
only decides how many cards a row holds and how wide the graphs are drawn. On a page at least 720 pixels wide the battery
stands tall at the right; beside it the processor and graphics, and under them storage, network and memory, every card of a
row as tall as the tallest. On a narrower page the cards go in rows of as many as fit, two on a phone held upright, drawn
compact (a smaller picture and number tile, storage's name over its ring).

What goes wrong (a chosen device unselected and why, one that does not answer, paired devices not shown at all) is told once
through the application's notifications (`peersus.notify`, the person's `notify.show`), and again only if it goes and comes
back. Without that permission the page writes it instead.

## Platforms and what each gives

One guest for every platform; what it can show is what the application's `peersus.system` service reads on that platform
(`crates/peersus-app/src/system_readings.rs`):

| Part | Linux | Android | macOS, Windows |
|---|---|---|---|
| Processor | `/proc/stat` ticks, name from `/proc/cpuinfo`, clock as the average of its cores (`cpu MHz` there, else each core's `cpufreq/scaling_cur_freq`), temperature from the processor's hwmon sensor, else its package thermal zone, else the hottest core's zone; cores from the system | clock and the hottest core's thermal zone as on Linux; name from the platform (`QTI SM8650`); busy only through a shell-mode Stellar/Shizuku provider the person already allowed (community distribution), since `/proc/stat` is closed to an application | cores only |
| Graphics | The first card of `/sys/class/drm` that names its maker: NVIDIA, AMD or Intel as its name; busy only if its driver publishes `gpu_busy_percent`; clock from its hwmon `freq1_input`, else the level its driver marks current in `pp_dpm_sclk`; its memory from `mem_info_vram_used` and `_total`; temperature from its hwmon | the `kgsl` driver: its model (`Adreno750v2`), how busy over its last window, the hottest of its thermal zones; clock and memory are closed | **nothing** |
| Memory | `/proc/meminfo` | the same | nothing |
| Storage | Total and free of the volume received files go to, else the application's data; the disk's name from the mount table (`/proc/self/mountinfo`), none for a volume in memory | the same | total and free |
| Network | `/proc/net/dev`, every interface but the loopback | the platform's `TrafficStats` | nothing |
| Battery | The first `Battery` of `/sys/class/power_supply`: charge, charging, minutes | the platform's `BatteryManager`: charge, charging, minutes from the charge left and the current, or the time to full it computed | nothing |

So the graphics answer something only on Linux and Android, only where a driver publishes it. The GPU card says so instead of
leaving empty rings: when nothing is published it says that this device's (or the chosen device's) system does not publish its
graphics; when only how busy it is is missing, it says that the driver does not publish that. A card is explained only by an
answer that came: before the first one, nothing is known to be missing.

Seen on two phones (RedMagic NX769J and ROG Phone, both Snapdragon 8 Gen 3): temperatures, graphics model and busy, battery and
network; the processor's share busy on the one where a shell provider is allowed.

## Permissions: per part and per device

Nothing reaches the extension without a permission the person gave.

| Permission | Granted | What it lets Monitor do |
|---|---|---|
| `device.info.cpu`, `.gpu`, `.ram`, `.storage`, `.network`, `.battery` | once, for each part | Read that part of this device. A part not allowed says "Not allowed" in its card; the others still show |
| `peers.watch` | for each paired device | See the device in the bar |
| `peers.send` | for each paired device | Send to it: the question it asks, the answer it gives |
| `monitor.share` | for each paired device | Show its numbers here, and let it see the numbers of this device. The application applies it to what Monitor sends to the device and to what the device sends Monitor, so one switch covers both directions |

Showing a paired device takes the three per-device permissions on the device that asks (`peers.watch`, `peers.send`,
`monitor.share` for the other device), `peers.send` and `monitor.share` for the asker on the device that answers, and the
`device.info.*` of the parts shown on the device that answers. The device that answers decides part by part: it answers only
with the parts its own person allowed Monitor to read there, and says "denied" for the others. What it answers with is
read when the question comes and is kept nowhere, so a permission withdrawn is gone from the next answer.

## Showing a paired device

The application carries a stream one way only (`peersus.peers` `send` writes it, the exported `receive` reads it), so a
question and its answer are two streams of one service, each sent to the device that is spoken to, by the name the application
gave this extension for it (a device is never named by anything it wrote):

- Service `monitor.readings`, version 1; method `receive`, guarded by the capability `monitor.share`; the application is its
  only consumer. The application hands a stream to `receive` only from a device whose person allowed `monitor.share`, and says
  whose it is.
- The question, one short line asked of the chosen device: `{"ask":{"seq":7,"parts":["processor","graphics","memory",
  "storage","network","battery"]}}`.
- The answer, sent back to the device that asked: `{"answer":{"seq":7,"parts":{"processor":{"status":"ok","value":{…}},
  "graphics":{"status":"error","code":"denied"}, …}}}`. A part is what the application said to the answering Monitor, in the
  application's own outcome words: `ok` with the value, or `error` with `denied` (the person there did not allow it) or
  `unavailable` (the application there could not say it).

What is accepted, on both sides, is judged in one place (`src/wire.rs`) and nowhere else:

- Only exactly these words: the fields of this version and no other, `seq` a whole number from one, each part named once and
  a part of a device. A question over 512 bytes, an answer over 8 KiB, or anything nested deeper than the reader follows, is
  not understood and nothing of it is used.
- A value keeps only the fields its part has, each within what it can hold (names of at most 128 characters without control
  characters, temperatures from -40 to 150 °C, shares to 100 %, minutes to a week); anything else is not known (`null`), never
  repaired. The same table decides what leaves a device and what is taken from another.
- An answer is taken only for the question that is out and only from the device that was chosen and asked. Late, repeated,
  unasked or foreign answers are let go, and nothing of them is shown.

How it is asked, so as to be polite to the other device and the application:

- Only while the page is open and a paired device is chosen. At most one question is out at a time, and a new one goes no
  sooner than 0.9 s after the last by the page's own clock: about once a second. Closing the page stops the clock and so the
  questions; choosing another device, or this one, lets go of the question that is out and of the stream it was waiting for.
- A question not answered in 3 s is lost and what was shown of the device is dropped; after three lost in a row the device is
  not asked again until it is chosen again or the page is opened again. The stream of a question that is not acknowledged
  by the application counts as lost.
- Every wait is bounded (400 ms for a stream to be readable, at most 12 reads of a stream, 2 s for the application to answer a
  call) and gives way to what the application delivers meanwhile, which is kept in order and handled afterwards (at most 64;
  more ends the wait). A call cancelled meanwhile gets no reply and causes no send.

What the person sees, in Monitor's words:

| Situation | Monitor says |
|---|---|
| The chosen device has not been asked yet, or has not answered yet | "Asking Desk…" on the page |
| This device's person did not allow Monitor to see it, send to it or share with it | One card, "Not allowed", saying where to allow it and that the other device must allow the same |
| The other device did not answer three questions | A notification: "Desk did not answer. Check that Monitor is installed there and allows this device, then choose it again." |
| The other device does not allow a part | That card says "Not allowed" and that Desk does not let Monitor share it; the others show |
| The other device could not read a part | "Not available" in its card |

## Execution format

The **WASM guest is the official build**: one module, byte-identical on every platform, in an isolated interpreter with no
files, network, processes or OS calls of its own, reaching the application only through the services it imports
(`peersus.peers` `list` and `send`, `peersus.system`, `peersus.config`, `peersus.pages`, `peersus.notify`). It is what the
store installs. It needs a host with `peersus_v1.poll` and data connections, as Control does; an older host is not
compatible, and host and guest are updated together. `src/host.rs` is the one copy of the shared guest glue
(`templates/guest/`), compared byte for byte by `scripts/check-guest-glue.py`.

The same source also builds, for validation and limited support, as a native library (`cargo build --release --features
native` in `wasm/`, packaged with `--native-library libpeersus_monitor_wasm.so` as `monitor-linux-native_library.tar.gz`) and
as a process (that library linked into the native host, `monitor-linux-process.tar.gz`). Both speak the same protocol and
behave the same, but run with the application user's OS privileges: the store does not install them; they travel with the
signed release for a person to install by hand ([support
tiers](https://github.com/DarkPhilosophy/Peersus/blob/main/docs/api/manifests.md#support-tiers)).

## Build and test

From `wasm/` (`env -u CARGO_BUILD_TARGET` only where the environment exports a build target):

```sh
cargo test --offline
cargo clippy --offline --all-targets -- -D warnings
cargo build --offline --release --target wasm32-unknown-unknown
cargo build --offline --release --features native
```

From the root of a Peersus source checkout, with `EXT` the path of this extension's source (`extension/peersus-monitor` in
the `extension` branch):

```sh
cargo run --offline -q -p xtask --target-dir target/ui/xtask -- check-manifest "$EXT/wasm/plugin.json"
python3 scripts/check-guest-glue.py "$EXT"
python3 scripts/module-cycles.py "$EXT/wasm"
python3 scripts/package-native-extension.py --source "$EXT/wasm" --sdk . \
  --target-dir "$EXT/wasm/target" --out target/extension-preview --platform linux --id monitor
```

`submission.toml` is the full recipe CI runs: the WASM build for Linux, Android, macOS and Windows, the native library and the
process, each validated on its own.

The tests cover the words of the wire, what is refused and cut at its size, what is kept of a part, asking and answering
through a scripted host (and two Monitors answering each other), every refusal on the way, the bounds of the waits and of what
is kept, choosing and unchoosing a device, the manifest against the code, the guest's whole loop, and the page: the cards, the
widths of the cards and graphs for a phone, a tablet and a desktop, the words of "Not allowed" and of the GPU, the chooser it
asks the application for, and the problems it tells as notifications. They do not drive a real device or a window: the page
was looked at in the application's demo window and on two phones, and the exchange between two devices was seen between them.

It draws with page elements of the application's contract that came with this version (`figure` text, `lines` on a text,
cards of a row as tall as the tallest, the `devices` chooser): an older application refuses its page, with its own message.

Known limits of this version:

- The guest has no clock, so the device that answers cannot meter how often a device asks; it answers one question at a time
  and relies on the application's own limits for an extension.
- When the other device's application refuses a stream (Monitor is not allowed there, or not installed), this device is not
  told why: it sees no answer. The other device's application says so to its person for each stream it refuses, so a device
  chosen that did not allow Monitor causes up to three such notices there before it is let be.

## License and third-party notices

This extension is licensed under Apache-2.0. The archive the packager makes carries `LICENSE` and `NOTICE` (those of the
Peersus source), `THIRD_PARTY_NOTICES.txt` and the license texts of the exact dependency set of `wasm/Cargo.lock` under
`licenses/`; keep them when redistributing. The WASM guest links serde_json and what it needs (itoa, memchr, serde_core,
zmij; MIT OR Apache-2.0, MIT, or Unlicense OR MIT); the lockfile also lists serde, serde_derive, proc-macro2, quote, syn and
unicode-ident ((MIT OR Apache-2.0) AND Unicode-3.0), which are attributed with it.
