# Local application control

Implementation: `crates/peersus-app/src/ipc.rs`. This is the local frontend-to-owner protocol, **not** the plugin protocol and not a public network listener.

## Connection and identity

The launcher creates and owns the local socket and its cleanup. On Unix, both accepting and connecting sides check the socket peer UID against the effective UID. Same-user authorization means another process running as that user is within the trust boundary; the socket is not protection against a compromised user account.

## Framing

Each frame is a four-byte unsigned **big-endian** byte length followed by that many UTF-8 JSON bytes. Zero-length and oversized frames are refused before allocating the payload. Requests are bounded by `REQUEST_LIMIT` in the implementation, derived from the maximum profile picture plus command overhead. Replies are bounded to 4 MiB.

The request discriminator is `operation`. Its values are Rust variant names, with their case preserved:

```json
{"operation":"Snapshot"}
```

```json
{"operation":"Lease"}
```

A command request is `{"operation":"Command","command":...}`. The command's shape comes from `peersus_core::model::Command`; do not infer it from the CLI spelling. Unknown request fields are rejected. Boxing the command in Rust does not add a wrapper to its JSON representation.

The response has `result` and `snapshot` fields. `result` is the serde representation of `Result<Option<Reply>, String>`; snapshot and lease requests return `Ok(null)`, while a command returns a reply or an error. A response example showing only the result portion is:

```json
{"Ok":null}
```

This is **not** a complete response: a real response also includes the complete `snapshot` object.

A command request may carry two optional booleans, each left out when false:

- `"quiet": true` asks for an answer without the `snapshot` field. A window follows the state with `{"operation":"Watch","since":N}` instead, which is answered when the state's generation is not `N`, after ten seconds unchanged, or when the owner stops, with the state and its `generation`.
- `"keep": true` asks the owner to leave the connection open after the answer, so that the next command is sent on it instead of on a new connection. Only commands with `"keep": true` may follow on such a connection; anything else closes it. The owner does not close a kept connection for being idle: it lasts until the client closes it, a frame on it fails, or the owner stops.

An owner that does not know one of these fields closes the connection without an answer and without running the command; the window then asks the same command again without the field. A connection that was already answered on and then ends without an answer is not that case: the outcome is unknown and the command is not asked again.

## Leases, concurrency and deadlines

A lease keeps a window attached while its connection remains open. It receives its initial response, then does not require periodic client messages. EOF, connection failure or owner shutdown releases it.

After its initial response a lease may carry one kind of client message: a single unframed byte, `1` when the window comes in front of the person (it has the system's focus) and `0` when it no longer does. Any other byte is ignored. A window that never writes one counts as in front, and an owner made before this existed reads and ignores the bytes, so neither side needs the other to be new. The owner uses it for one decision: [announcing an offer outside the window](#an-offer-announced-outside-the-window).

The implementation allows 16 ordinary concurrent requests, up to 8 leases and up to 8 kept command connections. A lease or a kept connection adds room for ordinary requests rather than permanently consuming their capacity; a ninth kept connection is closed without an answer and with its command not run. Request reading and response writing have 15-second deadlines; waiting for the next command on a kept connection has none. The client wraps the exchange in a 15-second deadline; that deadline does not prove that a submitted command did not execute.

**Timeout or disconnect after sending a command means the outcome may be unknown.** Query current state before deciding whether a retry is safe. Do not turn transport retries into duplicate installations, transfers or settings changes.

## Preferred client interface

Rust consumers should use the existing IPC client and `cli::Control` adapter instead of implementing a second framing stack. In-process consumers submit the same `Command` values through `runtime::Client` and observe application snapshots. Neither path bypasses application validation.

## Graphics preference

`settings.renderer` is the preference for the next window, not a claim about the live graphics driver. `patch-settings` takes `renderer: "auto" | "vulkan" | "opengl" | "software"` inside `patch`; the CLI uses the same values (`peersus --renderer software settings --save`). Default `auto` tries the native backends in platform order. The window reports the one that actually started in Settings → Appearance; the comparison and its results are local to that window, not part of the owner snapshot.

## Desktop window bar

`settings.desktop_bar_position` is `"top"` (the default when absent) or `"bottom"`. Change it with `{"command":"patch-settings","patch":{"desktop_bar_position":"bottom"}}` or Settings → Appearance → Window bar position. This moves the bar in open landscape or square desktop windows and is saved for the next launch. The handheld and compact layouts do not apply the preference.

## An offer announced outside the window

A file offered to this device waits for an answer for a bounded time (`offer_timeout_seconds`). The window shows it; where the platform has a way, it is also announced while no window is in front:

- **Android:** the platform adapter draws a panel over other applications when the person has allowed that, and posts a notification with the same answers when they have not or the screen is locked. This is in the Android application, not on this socket.
- **Linux:** while an offer waits and no lease says it is in front (none attached, or every one wrote `0`), the owner starts the window program beside it with the single argument `--offer-panel`, and stops it when that is no longer so. The panel attaches like a window but takes **no lease**: it is never what keeps an owner running and is never a window in front. It reads the same snapshot and answers with the same `accept-transfer` and `cancel-transfer` commands. It is a window the window manager does not manage, placed by the program, which needs an X display (XWayland on a Wayland session); with none the owner says so once in its log and the offer is still shown by the window.
  It always stands on the primary screen, clear of the edge by a fixed margin (the desktop's free area is not asked for), and is as tall as what it shows. It can be carried with the pointer; where it was put lasts as long as that panel is up and is not saved, so the next one stands at the chosen edge again.
- **Windows, macOS:** not implemented. Nothing is announced outside the window.

A notification whose sender asked for `"reach": "outside"` (see [Notifications](#notifications)) is announced outside the window too. On Linux, while one is on show and no window is in front, the owner starts the same panel, which draws it under the offer, if there is one, with its buttons and answers with `answer-notification`. On Android, while the window is not in front, each is a system notification with its buttons; a button answers with its id, and swiping it away closes it, both through `answer-notification`; a tap on one that opens brings the window forward and sends `open-notification`. On Windows and macOS such a notification is shown only in the window for now.

`settings.notice_place` (`top`, `bottom`, `left`, `right`; default `bottom`, set with `SettingsPatch.notice_place`) is the edge of the screen the announcement stands at. A phone has a top and a bottom and draws at the bottom for a side saved on a desktop.

## Notifications

`snapshot.notifications` is what the application shows a person now, for itself or for an extension, oldest first. Each is `{"id": N, "source": "app" | {"plugin": "<id>"}, "notification": {"text": …, "reach": "inside" | "outside", "stays": bool, "buttons": [{"id": …, "label": …}], "opens": "<word>"}}`; `buttons` is left out when there are none, `opens` when a tap on it opens nothing. The shape and its limits are the contract's (`peersus_api::notification`): one paragraph of at most 500 characters, at most three buttons, each label at most 24 characters. The words of a button are the sender's; where, how large and in what order a notification is drawn is the application's. One that neither stays nor has a button leaves the list by itself after seven seconds. At most eight of the application's and two of each extension are on show; an extension's leave with it when it stops.

One command answers them: `{"command": "answer-notification", "id": N, "button": "<button id>"}` presses a button, and without `button` it closes the notification. Either takes it off the list and tells whoever waits for it. A number that is not on show, or a button the notification does not have, is refused and changes nothing. A window sends the same command; a script reads the list from `peersus status` and answers with `peersus call`.

`{"command": "open-notification", "id": N}` is a tap on a notification that opens (in a window, the words of its card): it leaves the list, its sender is told its word (see [service messages](service-messages.md#notifications-an-extension-asks-for)), and `snapshot.opened` becomes `{"serial": S, "plugin": "<id>"}` with a new number, which every window acts on once: it comes forward on that extension's page in the navigation, or at its row in Settings › Extensions when it has none there. A number that is not on show, or one that opens nothing, is refused and changes nothing.

## Key bindings (desktop)

`settings.bindings` maps a chord (the spelling of [suggested chords](manifests.md#suggested-chords), `"ctrl+alt+1"`) to what it runs: `{"kind": "app", "action": "accept-offer" | "decline-offer" | "show-send" | "show-receive"}` or `{"kind": "plugin", "plugin": "<id>", "action": "<action id>", "peer": "<device id>"}`, with `peer` exactly when the action needs a device. One chord runs one thing; a thing has up to three chords (an action that needs a device, up to three per device). One command replaces them all: `{"command": "set-bindings", "bindings": {…}}` (an empty map clears them). It is refused, and nothing changes, when a thing would have more than three chords, when an extension's action is not installed, cannot be run by a key (it has `input` or is `destructive`), or names a device it does not need or one that is not paired. `update-settings` keeps the bindings as they are; `patch-settings` has no field for them. Removing an extension drops its bindings; forgetting a device drops those for it. Android refuses the command.

`settings.bindings_outside` (`patch-settings` field `bindings_outside`, off by default) asks the desktop for the bindings while the window is not in front too. On Linux the window asks the GlobalShortcuts portal for them, which shows the desktop's own approval dialog and may give another key where one is taken; it does so only while the switch is on and something is bound, and the session ends when either goes. A press reported by both the window and the desktop runs once.

## Devices nearby (experimental)

`settings.nearby_send` (default `false`, set with `SettingsPatch.nearby_send` or `--nearby-send`) is the person's switch for [devices nearby](../nearby-send.md). While it is on, the snapshot's `nearby` says what the platform's Bluetooth found: `near`, the identifiers of paired devices held right next to this one; `receiving`, those of them that say they take files now (their person has Receive open); `strangers`, the devices nearby that are not paired, each as `{"id": …, "name": …}` with the name that device gave itself, plus `"receives": true` when what it said is that it takes files (without it, it said that it is there to be paired with) and `"near": true` when it is brought near; and `trouble` (`no-radio`, `off`, `not-allowed`, `failed`) when the platform said why nothing can be found. While a person measures, `measuring` is `true` and `strongest` is the strength in dBm of the device of this application heard strongest. All are absent while there is nothing to say.

`settings.nearby_near_at` (absent by default; `SettingsPatch.nearby_near_at` or `--nearby-near-at`, in dBm between −85 and −30, `0` to forget it) is the strength at or above which a device counts as brought near, when the person set one; without it the application's own −55 dBm is used.

Four commands belong to devices nearby:

- `{"command": "seek-nearby"}` says a window is open. Only for 10 seconds after the last one does this device listen for what devices nearby announce and fill `receiving` and `strangers`; a client says it again every few seconds, and one that stops simply stops. With `"takes": "pairing"` it also says that the window shows the devices nearby to pair with, and with `"takes": "files"` that it shows Receive: only then, and only for that one thing, does this device announce an offer to others. The two are different things and never announced together; what was announced for the one admits nobody once the window says the other, or neither. With the switch off it does nothing.
- `{"command": "measure-nearby"}` says a person is setting how near near is. For 10 seconds after the last one the snapshot carries `measuring` and `strongest`. With the switch off it does nothing.
- `{"command": "pair-nearby", "peer": "<id>"}` begins pairing with a device of `strangers`. What follows is what follows `join-invitation`: a pairing request in the snapshot, answered with `choose-pairing` or `reject-pairing` on both devices. It is refused with `That device is no longer nearby` when the device left the list or what it announced ran out.
- `queue-files` and `queue-held` take the destination `{"kind": "nearby", "peer": "<id>"}`: the files, sixteen at most, are offered to that device without pairing, and each is its person's to accept. The owner refuses with `That device is no longer nearby` unless the device is in `strangers` with both `receives` and `near`, and what it announced is still live; what it announced is used once. The reply names the transfers the files will be; they appear in the snapshot once that device has admitted this one, and never when it does not.

The invitation a stranger announced is kept by the owner and is in no snapshot and no reply. What a platform's Bluetooth adapter tells the owner is not accepted on this socket: no client can say that a device is near.

## Relay policy settings

The shared Network editor submits `patch-settings` with the existing `SettingsPatch.relay_policy` field. Direct connection priority is not a reorderable row. A complete local request, using an illustrative project address, is:

```json
{
  "operation": "Command",
  "command": {
    "command": "patch-settings",
    "patch": {
      "bind": {"mode": "automatic"},
      "relay_policy": {
        "targets": [
          {"kind": "project", "url": "https://relay.example.org"},
          {"kind": "public"}
        ],
        "disabled": [],
        "direct_wait_ms": 3000,
        "failover_wait_ms": 5000,
        "retry_primary_ms": 60000
      }
    }
  }
}
```

Check `BindChange` in `peersus-core/src/config.rs` when implementing a client; enum shapes are part of the typed contract. Target kinds are `project`, `custom` (both with `url`) and `public` (the n0 pool, without a URL). List order is priority. An empty list disables relays; at most eight targets, one custom credential-bearing target, one public pool, and no duplicate URLs are accepted. Addresses must pass the existing HTTPS relay URL validator; credentials belong in the separate relay-token command, never in a URL or settings.

The optional `disabled` array contains zero-based positions in `targets`, for example `"disabled": [1]` retains the second fallback but skips it. Missing or empty means every listed target is enabled. Disabled entries retain their addresses and priority; they still count toward the size and duplicate limits, and out-of-range positions are rejected. When reordering, move the disabled flag with its target. An empty list or all targets disabled means direct-only routing. Diagnostics refer to the original configured positions, not a compressed list of enabled targets.

Clients sending `disabled` require an owner that understands this field. Upgrade and restart an older desktop owner before using the new editor; building or replacing the UI executable does not upgrade an already-running owner. Older clients may omit `disabled` and retain their previous all-enabled list semantics with the new owner.

Timing ranges are 100–30,000 ms for the initial direct-path opportunity, 500–60,000 ms for tier failure, and 5,000–3,600,000 ms for retrying the preferred tier. These are deadlines, not guarantees of connectivity. A preferred-tier retry does not preempt an active fallback transfer. The public pool may select a server within that pool; the ordering applies to tiers rather than individual n0 servers.

New settings use the project-then-public default. Old settings without `relay_policy` retain their prior public/custom/direct-only selection; they do not gain a project relay during migration. Legacy `relay` patches remain accepted for existing clients and explicitly select their legacy mode. Do not combine legacy `relay` and `relay_policy` fields in a new client. The GUI submits the complete ordered policy and uses automatic binding when enabling relays, because a fixed local bind and relay routing are incompatible.

New policies default to `order: "automatic"`: direct paths remain preferred, while every enabled relay is offered to the transport for measured-latency selection with anti-oscillation hysteresis. List order does not override latency in this mode; use `priority` for strict ordered fallback and periodic recovery of the preferred tier, or `sequential` to retain a working fallback until it fails. Official automatic candidates are displayed as project/public, moving disabled flags with their targets. Personal relay lists and explicit `priority`/`sequential` lists retain their ordering. A missing `order` in an older policy still means `priority`. The former `latency` value loads as `automatic` and is written back canonically; the duplicate TCP-based selection mode is no longer offered. Disabled relays are never enabled by normalization.

The former project default is migrated to the current project relay address; user-edited project addresses are preserved. Turning all fallbacks off keeps their saved order and addresses, so enabling one later restores it in place.

`snapshot.settings.network.relay_policy` describes configured policy, not proof that a server is reachable. `snapshot.relay` separately reports sanitized live home-relay diagnostics: `{"active_target":1,"health":"connected"}` means zero-based fallback position 1 is connected. Health values are `disabled`, `connecting`, `connected`, `unavailable` and `refused`; disabled has a null target. An absent/null diagnostic is an older owner, not evidence that relays are disabled. Health changes publish snapshots even when the active tier does not change. The shared Network editor shows the state and one-based fallback position. No raw server errors, addresses or credentials are added to diagnostics.

Home-relay health is not an individual peer's selected traffic path. See [the transport limitations](../architecture.md#transfers-and-relay-configuration): relay signalling and encrypted fallback traffic can expose connection metadata, and home-relay ordering is not a peer-relay firewall.

## What extensions keep for their drawings

`snapshot.kept_layers` lists each extension that keeps layers for its drawings (`peersus.drawings`), by `plugin` and an opaque `stamp`. The shapes never travel in a snapshot. A window that does not hold the stamp asks for the set with local commands, both quiet (see the `keep`/`quiet` request options) and never refused:

- `{"command":"layer-index","plugin":"<id>"}` answers, as the reply, `{stamp, layers: [[id, stamp], …]}`: every layer the extension keeps now and the stamp of its content, with `stamp` zero and no layers when it keeps nothing.
- `{"command":"fetch-layers","plugin":"<id>","ids":["<id>", …]}` answers the shapes of those layers, in the order asked: `[{id, stamp, shapes}, …]`, as many as one answer carries (about 1 MiB of layers) and none of the ones no longer kept. A client asks again for the rest.

A layer whose stamp changed is fetched again; a layer no longer in the index is let go of. What an extension keeps is forgotten when it stops, restarts, is turned off or is removed, and the snapshot then stops naming it. See [The drawings an extension keeps](service-messages.md#the-drawings-an-extension-keeps).

## What extensions keep for their pages' pictures

A page that places a kept picture (`{"k":"picture",…}`, `peersus.pictures`) carries, in `snapshot.page_views`, the `digest` the owner wrote into it, never the pixels. A window that does not hold that digest asks once:

- `{"command":"fetch-kept-picture","digest":"<64 hex>"}` answers `{width, height, rgba}` (`rgba` as lowercase hex, at most 640 × 640 × 4 bytes), the same form as `fetch-picture`. A digest no extension keeps is refused.

A window lets go of a picture once no page places it. See [The pictures an extension keeps](service-messages.md#the-pictures-an-extension-keeps).

## Application update commands

Three commands, none with a field: `{"command":"update-check"}`, `{"command":"update-install"}` and `{"command":"update-restart"}`. They name nothing: what is fetched and installed is decided by the signed release, never by the client. Each returns at once; progress is read from `snapshot.update`:

- `status`: `not-configured`, `unsupported`, `idle`, `checking`, `current`, `available`, `downloading`, `installing`, `installed`, `restarting` or `failed`.
- `current`: the running build as it names itself; `offered`: the newer build a verified release offers, or null; `notes`: the publisher's words about it; `error`: a sentence chosen by the application when the status is `failed` or `unsupported`.

`update-install` is refused unless a verified release is waiting, and, where installing ends the application (Android), while a transfer or a pairing is under way. `update-restart` is refused unless an update is in place and nothing is being transferred. A client that held a lease when the owner restarts sees the connection end and connects again. The two preferences are `update_automatically` and `restart_after_update` in `SettingsPatch`. See [Application updates](../app-update.md).
