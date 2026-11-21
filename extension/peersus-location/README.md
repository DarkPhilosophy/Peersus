# Location extension

An optional device map for Linux, Android, macOS and Windows. Opening the page shows a globe, without asking for a location.
Devices that have shared a position appear as initials and name badges painted by Location. A device that shares at its
turn says no age; a position older than a turn allows says so under the name (see *How old a position is*).
Tap a marker or its name to centre and zoom. **Locate this device** centres on a successful local fix. Sharing is controlled under **Permissions**, not in the map options.

## What it asks, and when

- **Where this device is** comes from the application's `peersus.location` service (`current`, the person's `location.read`),
  requested when you select *Locate this device*, or when an eligible recipient is ready for sharing. Linux uses the desktop
  location portal; Android uses the system location manager. System permission and a usable fix are separate from the
  application's `location.read` grant. A failed lookup does not place a guessed marker.
- **Sharing**: enable `location.sharing` separately for each paired device. `peers.watch` and `peers.send` are also required,
  and the receiving device must run Location and accept the service from this device. The worker checks for eligible peers
  at startup and periodically even when the map is closed. With an eligible recipient it asks for a fresh position at most
  once per five minutes. Losing eligibility cancels pending deliveries. Old receive-only consent does not enable automatic
  transmission after an update.
- **Asking**: a device of which nothing is known here, or only something more than ten minutes old, is asked where it is
  (the exported `location.wanted` `receive`, under the same `location.sharing` on both sides): at once, then again after
  fifteen seconds and twice as long each time while it has not shared, in the end once per five minutes. The device asked
  answers the device that asked, at most once per ten seconds, with the position it already knows; a question it cannot
  answer yet is kept for a minute and answered by its next share. A question never makes a device ask its system for a
  position sooner than every five minutes. This is what fills the map shortly after the application starts, instead of at
  each device's next turn.
- **Receiving** is the exported `location.position` `receive`, which the application admits only from a device this device's
  person allowed Location for (`location.sharing`). One position per device is kept, in memory only, and forgotten when the
  extension stops, the person presses *Forget received locations*, or the device is no longer listed as allowed (unpaired,
  or Location no longer allowed for it).

## How old a position is

A device with an eligible recipient shares again every five minutes, so a position younger than that is simply where the
device is, and no age is said. A position six minutes old or older (the turn, and a minute for the time a turn takes) is
said, in **Options** and under the device's name on the map, as its **last location** with its age: `last location 12 min
ago`. It stays on the map until a newer one arrives or it is forgotten (see **Receiving** above).

A guest has no clock. While a page is open its events say the time. With no page open, time is counted from idle waits of
five seconds: one that ran out surely took five seconds; one that a delivery cut short took an unknown part of them. That
part is never added to an age as if it were known. It is kept apart, and once it may add up to a whole unit of what is said
(a minute of an age said in minutes, an hour of one said in hours), the age is the least it can be and says so with a plus:
`last location 12+ min ago`. The age sent to another device is that least age too. Turns and retries are counted on the same sure time, so a share is never sooner than five minutes after the last and
may be later by what was not counted.

## The message

One stream, one line of JSON, at most 512 bytes:

```text
{"position":{"latitude":44.4268,"longitude":26.1025,"accuracy":12.5,"age":30}}
```

`accuracy` is metres (`null` or left out when the system did not say), `age` the seconds since the system found it when it
left, at least (see *How old a position is*). Anything else (other keys, a key written twice, a latitude off the Earth, an accuracy below zero or over a thousand
kilometres, an age over thirty days, a longer stream) is refused and nothing of it is kept; a position that grew older than
thirty days before it left is not written at all. However many pieces the line arrives in, it is one message. What bounds
it is its 512 bytes and time: a guest has no clock, so each single wait for a piece is 400 milliseconds at most (begun again
when the application delivers something else meanwhile, of which 64 at most are kept), and the whole is ended by the
application, which ends the stream when the time it gives the `receive` call is over. A share counts as delivered only when
the application says that every byte reached the other device: each read of that acknowledgement (a few bytes, in a piece or
two) is waited for three seconds at most, in the same way.

The question is one stream with exactly this line, and nothing answers on it:

```text
{"wanted":"position"}
```

## The map

The compiled extension carries Natural Earth's public-domain coastlines, land borders, regional roads and populated places.
The globe uses the same orthographic projection and hides the far hemisphere at every zoom level. Coastlines, roads, city
labels and device markers share that camera; zoom changes its scale, not the projection.
Roads and boundaries are indexed in five-degree cells. At regional scales only intersecting cells are visited; major roads
appear first and town labels are ranked and spaced apart. Named local streets are available inside the bundled Bucharest
and Monaco areas (OpenStreetMap, ODbL 1.0); other parts of the world have regional detail only without enabling online data.
The data and reproducible generators are listed in `NOTICE`. No map requests are made until the map provider is approved.

**Online street detail.** In Location’s extension settings, **OpenStreetMap street detail** is off until the person turns it on, with Public HTTPS allowed and OpenFreeMap approved; the approval is reused after restarting. Regional zoom shows vector roads, water and built-up areas. Closer zoom adds local streets, place labels, parks and buildings where the source provides them. The map uses one tile level at a time and keeps a covering level visible until its replacement is ready. Decoded tiles are held in at most 8 MiB; when the level the view asks for needs the room the level on screen takes, that level is given up and the offline map stands in until the new one is whole. The provider sees the address of every tile requested, so the areas shown, and the network address; no GPS fix or paired-device identity is included in a request. If the source is unavailable, the offline globe remains usable. Online data is from © OpenMapTiles and © OpenStreetMap contributors via OpenFreeMap.

**Kept map areas.** With **Keep viewed map areas** on (the default), the application keeps the map index and downloaded tiles on this device, up to 128 MB. Saved areas show immediately without waiting for a download, even offline. Location refreshes older copies in the background and continues showing the saved map until replacement tiles are ready. **Options** on the map shows how many areas are kept and their size; **Clear saved map areas** removes them. Removing Location also removes them. With this setting off, Location neither reads nor saves copies on disk; it holds the current view’s tiles in memory and leaves already saved areas untouched until cleared.

The right-side zoom scale can be tapped or dragged. Its caption is an estimated camera height above the surface, not GPS altitude.
The orthographic view has no physical altitude, so the estimate uses a downward-looking 60° perspective camera with the same
scale at the centre: `elevation = (R / r) × min(width, height) / (2 × tan(30°))`, where `R` is Earth's mean radius and `r` is the
projected globe radius in logical pixels. The location icon centres on a known device and restores north-up.

The horizontal scale at the bottom is a ground-distance ruler calibrated at the map centre. It chooses a fitting 1, 2 or 5 times
a power of ten and draws its length as `2r × sin(distance / (2R))` logical pixels. The same scale is valid at any bearing.
Labels avoid the ruler's backing, and the ruler stays hidden until there is enough measured room to show it whole.

At country and wider views, dragging rotates the globe through its poles and keeps a point under the finger on the visible hemisphere. At regional and closer scales the rectangular crop works as a local map. A two-finger twist turns either view: about the centre of the globe or about the gesture's midpoint on the local map.

Location draws the entire map, including offline and online features, labels, device markers, and both scales, into a single Canvas. The supervised WASM helper renders the display list and the window composites the completed raster. The window reports the logical size, device scale, palette, and gestures.
Location uses the current WASM Canvas contract on all supported platforms.

## Build and test

```sh
cargo test --manifest-path wasm/Cargo.toml --locked
cargo build --manifest-path wasm/Cargo.toml --locked --release --target wasm32-unknown-unknown
```

From the Peersus checkout root, the replay suite (the real interpreter and renderer, including rotation) uses the same
release guest. One of its tests also needs `PEERSUS_TEST_LOCATION_TILES`, a folder of real saved tiles:

```sh
PEERSUS_TEST_LOCATION_MODULE="$PWD/plugins/peersus-location/wasm/target/wasm32-unknown-unknown/release/peersus_location_wasm.wasm" \
  cargo test --locked -p peersus-plugin-canvas --test location -- --ignored --nocapture
```

`submission.toml` builds and tests the WASM guest and produces one WASM package per platform.
Before building, the source recipe checks that `wasm/src/canvas_sdk.rs` is a byte-for-byte copy of the SDK's
`templates/guest/canvas.rs`. Update the SDK file first, then refresh the vendored copy.
