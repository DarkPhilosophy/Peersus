# Location extension

One extension identity with one WASM guest for Linux, Android, macOS and Windows. A page, among the application's pages, that
says where this device is and draws, north up, where the paired devices that shared their location with it were: how far
away, in which direction, how sure their system was and how long ago. This device is in the middle of the map when it knows
where it is; when it does not (not asked yet, not allowed, or a system that cannot say, as on macOS and Windows today), one
of the devices that shared with it is in the middle, and each device's line says its coordinates in place of a distance and
direction from here.

## What it asks, and when

- **Where this device is** comes from the application's `peersus.location` service (`current`, the person's `location.read`),
  asked only when the person presses *Share my location* or *Show where this device is*, or every five minutes while the page
  is open when *Keep sharing* is on. The application asks the system each time and keeps nothing: on Linux the desktop's
  location portal, which follows the person's own location setting; on Android the system's location manager, which asks the
  person for its own permission the first time; elsewhere nothing, and the page says so.
- **Sharing** sends that one position, once, to every paired device that takes it now (`peersus.peers` `send`, the person's
  `peers.watch` and `peers.send` for that device): online, allowed here, and with Location there accepting
  `location.position`. Nothing is asked of another device, and nothing answers: a position goes only when its own person
  shares it. A share sends the position it was asked for: looking where this device is afterwards changes nothing of what
  goes, a later share replaces an earlier one that had not left yet, and turning *Keep sharing* off takes back what only
  it asked for and has not left (this device still being found, or the streams not open yet), never a share the person
  pressed for. *Keep sharing* asks again five minutes after it last asked, found or not.
- **Receiving** is the exported `location.position` `receive`, which the application admits only from a device this device's
  person allowed Location for (`location.share`). One position per device is kept, in memory only, and forgotten when the
  extension stops, the person presses *Forget received locations*, or the device is no longer listed as allowed (unpaired,
  or Location no longer allowed for it).

## The message

One stream, one line of JSON, at most 512 bytes:

```text
{"position":{"latitude":44.4268,"longitude":26.1025,"accuracy":12.5,"age":30}}
```

`accuracy` is metres (`null` or left out when the system did not say), `age` the seconds since the system found it when it
left. Anything else (other keys, a key written twice, a latitude off the Earth, an accuracy below zero or over a thousand
kilometres, an age over thirty days, a longer stream) is refused and nothing of it is kept; a position that grew older than
thirty days before it left is not written at all. However many pieces the line arrives in, it is one message. What bounds
it is its 512 bytes and time: a guest has no clock, so each single wait for a piece is 400 milliseconds at most (begun again
when the application delivers something else meanwhile, of which 64 at most are kept), and the whole is ended by the
application, which ends the stream when the time it gives the `receive` call is over. A share counts as delivered only when
the application says that every byte reached the other device: each read of that acknowledgement (a few bytes, in a piece or
two) is waited for three seconds at most, in the same way.

## The map

The map is drawn by the extension from the application's generic drawing element, with no tiles and no online map service:
each position at its true distance and direction from the middle (an azimuthal equidistant map), so the rings (every 1, 2 or
5 times a power of ten metres, at most three) hold at every scale. A shaded circle is how sure that device's system was. The
names are those the person gave the devices, written beside their dots.

## Build and test

```sh
cargo test --manifest-path wasm/Cargo.toml --locked
cargo build --manifest-path wasm/Cargo.toml --locked --release --target wasm32-unknown-unknown
```

`submission.toml` is what the store's source check runs: the WASM guest, the native library and process tiers, and one
package per platform.
