# Plugin service messages

Implementation: `crates/peersus-api/src/message.rs` (the contract) and `crates/peersus-plugin/src/hub.rs` (the hub that routes it). These messages are independent of a specific GUI, transport backend or programming language. Process framing and bootstrap authentication belong to the host layer, not to this JSON schema.

## Participant operations

`Operation` is tagged by `op`, uses kebab-case variant names and rejects unknown fields. A participant may send:

| `op` | Fields | Meaning |
| --- | --- | --- |
| `call` | `id`, `provider`, `service`, `version`, `method`, `args`, `timeout_ms` | Call an exported method |
| `reply` | `ticket`, `result` | Answer a call delivered to this participant |
| `cancel` | `id` | Withdraw this participant's pending call |
| `subscribe` | `provider`, `service`, `version`, `event` | Subscribe to a declared event |
| `unsubscribe` | `provider`, `service`, `version`, `event` | Remove a subscription |
| `publish` | `service`, `version`, `event`, `data` | Publish an event this participant exports |

`id` and `ticket` are unsigned 64-bit integers; service versions and `timeout_ms` are unsigned 32-bit integers. The hub applies manifest, permission, lifecycle and timeout rules in addition to deserialization. Syntactically valid JSON is not necessarily an admitted call.

Example of withdrawing one's own call:

```json
{"op":"cancel","id":1}
```

A sender cannot add `caller` or `owner` to claim another identity: these fields are not accepted on operations.

## Hub deliveries

`Delivery` is tagged by `type`, also kebab-case with unknown fields rejected:

| `type` | Fields | Meaning |
| --- | --- | --- |
| `call` | `ticket`, `caller`, `service`, `version`, `method`, `args` | A routed call; answer using this ticket |
| `result` | `id`, `result` | Outcome for the caller's own request ID |
| `cancelled` | `ticket` | The delivered call was cancelled or expired |
| `event` | `provider`, `service`, `version`, `event`, `data` | An admitted subscription event |

The hub stamps `caller` from the channel that submitted the request. The participant does not get to supply that identity. A ticket identifies the routed call at the hub, not the caller's local request ID.

## Outcomes

`Outcome` is tagged by `status`:

```json
{"status":"ok","value":null}
```

```json
{"status":"error","code":"example-error","message":"Synthetic example only"}
```

The example error code is illustrative, not an enumerated protocol error. Use the actual service's documented codes when handling a method result.

Two results are **blocks**, not errors:

```json
{"status":"missing-permission","permission":{"owner":"host","name":"peers.send"}}
{"status":"not-accepted"}
```

`missing-permission` is an exact local grant checked by the owner. A permission has a bounded identifier for each of `owner` and `name` and carries no message. Show what is missing on **this device**; do not retry a blocked action when a grant changes. `not-accepted` is the neutral outcome of a paired-device refusal. It does not say whether the destination's extension is absent or which grant is missing. It is not a substitute for a timeout, broken stream or invalid response.

For a declared user action, use `{"status":"error","code":"<code>","message":""}` for these outcomes. The application uses its own words; it does not display the guest's `message`.

| `code` | Recorded action state | What the application says |
| --- | --- | --- |
| `not-shown` | `done-unshown` (a completion) | The action finished, but its result could not be shown. Check what happened before repeating it. |
| `rate-limited` | `failed`, reason `rate-limited` | The destination's limit was reached. Try again later. |
| `too-large` | `failed`, reason `too-large` | What was sent is larger than the destination accepts. |
| `refused` | `failed`, reason `refused` | The request was refused |
| `credential-rejected` | `failed`, reason `credential-rejected` | The saved credential was not accepted. Save a valid one. |

Use `not-shown` only when the work is known to have completed, not when the action failed or its external outcome is unknown. Forward a checked block instead of turning it into a `refused` or `denied` error. A guest's reply can set a local action state, but cannot cause the owner to warn of a missing grant or send a paired refusal. Guest-to-guest replies that name a missing permission are reduced to `not-accepted`; the hub does not consult another provider's manifest to interpret the name.

The owner publishes the latest state for the action regardless of which client invoked it. An admitted new run replaces any previous result. A block is `blocked`, not `failed`; a grant change clears the extension's earlier blocked results without running anything. A terminal result expires after 15 seconds. A failed tracked action also produces an in-app notice in the application's own words, whether its action menu is open or closed. The owner remembers up to 64 distinct missing-grant warnings and up to 16 missing-extension warnings. Dismissing a warning does not grant the permission or repeat the warning; resolving its cause clears that entry.

## Bounds and implementation guidance

One serialized control message is limited to **48 KiB** (`MAX_PAYLOAD`), including JSON overhead. The size check is on the serialized representation, not just the application value. Large data belongs on the host's separately admitted data path, not in oversized control messages.

Clipboard read chunks contain at most 8 KiB of decoded UTF-8, and may be shorter so the whole reply, including JSON escapes, fits the broker's limit. Use the returned `next` byte offset for the next chunk.

The hub admits each extension's work at its own rate: about six hundred work-starting operations a second (calls, subscriptions, publications), with a burst of three hundred. Replies, cancellations and unsubscriptions spend nothing. The application itself is never limited.

An extension that keeps asking past its rate is refused with `rate-limited`; each refusal is answered at once so nothing waits on its deadline. Refusals without a quiet second between them add up: after six hundred in a row the extension is suspended for thirty seconds — not restarted, not stopped, just not listened to or called. While it is suspended, a call to it is refused at once with `unavailable`, and its own transport is told how long to leave it unread. The application tells the person once, in fixed words. After the suspension the extension is an ordinary participant again. A removal (`forget`) clears everything held against it, so a different package under the same name starts clean.

These limits exist so one misbehaving extension cannot cost the others: its own writes slow it down, its calls never block a deadline, and every other participant keeps its own rate. A serial caller reading checked blocks is paced on its own link; repeated `missing-permission` or `not-accepted` results are not a reason to flood the broker, log a failure, or schedule retries.

Three more bounds are per extension, for the same reason. One consent prompt of an extension waits for the person at a time (four in all, each from a different extension); a second is refused at once. One extension replays input for at most sixteen devices at once (see Input mode negotiation). Input forwarded to an extension waits for its answer in a bounded queue in which motion is summed and a position replaced by the newer one; past it the batch is refused and the pointer is given back.

A participant's queue at the hub holds at most 64 deliveries, less one slot kept for the result of each call it has pending. An event is delivered to every subscriber that has room for it; a subscriber whose queue is full misses that event, and neither the publisher nor any other subscriber is affected. An event is therefore a notice, not a log: a service that publishes changes also offers a method that says what is true now, and a subscriber that may have fallen behind asks it.

A call the hub does not admit is answered under the id the caller chose. A checked missing guard uses the `missing-permission` outcome above. Other refusals remain error results with one of these codes:

| `code` | The call was refused because |
| --- | --- |
| `denied` | the caller does not import the method or is not among the provider's consumers |
| `unavailable` | the provider is not registered, not connected, or suspended |
| `unknown-method` | the provider exports no such service or method |
| `version` | the versions of the import and the export differ |
| `invalid-deadline` | the id is zero, or the time asked for is zero or longer than the hub allows |
| `too-large` | the operation is larger than one control message may be |
| `busy` | the caller has too many calls outstanding, or a queue is full |
| `rate-limited` | the caller is over its rate |
| `refused` | anything else |

One answer is not a refusal: `stopped` comes to a caller whose call was still outstanding for the provider (handed to it, read or not) when it ended or was restarted, before it answered. The call may have done part of its work.

Operations that are not calls (`subscribe`, `publish`, `cancel`, `unsubscribe`) are not answered: a subscription that was refused is seen by its events never arriving. An operation counts against the rate whether or not it is then admitted. Bulk bytes go over a data connection, which is not counted in operations.

Service versions, declared methods/events and permissions are validated by the shared hub. Keep concrete platform and plugin behavior behind these contracts; adding a service must not require recognizing a plugin display name in the application core. Cancellation is part of the lifecycle: an extension must not treat a cancelled or expired call as continuing permission to cause a late side effect.

## Optional page features

`peersus.config.get` (`{}`) includes `page_features` alongside the caller's settings. These names describe what the caller's current executor and platform support. Use a feature only when it is offered.

| Feature | What it allows |
| --- | --- |
| `pinch` | `"pinch"` in a box's `observe` list, sent as `{"t":"pinch",…}`. On a box with an ID, two touchscreen contacts produce `{"t":"pinch","id":"canvas","x":120000,"y":90000,"scale":10000,"begin":true,"at":123}`. The coordinates are the global midpoint in thousandths of a logical pixel. Later events have `begin:false` and an absolute scale relative to the initial separation (for example `15000` for 1.5×). `at` is monotonic milliseconds, as on `pointer` and `wheel`. The host can drop intermediate samples but does not merge across a new gesture. A mouse wheel is still a `wheel` event |
| `layers` | `layers` on a `drawing`: kept layers placed by id, kept through `peersus.drawings` (see below) |
| `follow` | `style.follow` on a clipped `canvas`: preview drag or zoom on its last bitmap while waiting for a new frame |
| `text_size` | Optional `size` on a `text`: font size in logical pixels (8 to 64). Omit it to use the tone's usual size. The tone still selects colour and weight. For example `{"k":"text","text":"Canada","tone":"title","size":32,"lines":1}` |
| `rotate` | `"rotate"` in a box's `observe` list. Two touchscreen contacts on a box with an ID produce `{"t":"rotate","id":"map","x":120000,"y":90000,"turn":0,"begin":true,"at":123}`, then `begin:false` events whose `turn` is how far the line between the two touches has turned since the second touch, in hundredths of a degree, clockwise on the screen positive, continuous past a half turn (`-4550` is 45.5° anticlockwise). `x`/`y` are the midpoint as for `pinch`. A box may observe `pinch` and `rotate` together; each two-touch sample then sends a `pinch` and then a `rotate` with a later `at`. Nothing is previewed: the extension draws the turn. Ask for it only when this owner lists `rotate`: an older owner refuses a tree that names it |
| `canvas` | A `canvas` element whose finished pixels the extension supplies from its own process; see below |
| `visibility` | A `box` with an ID may include `"observe":["visibility"]`. The mounted box reports `{"t":"visibility","id":"row","visible":true,"foreground":true}` to its own page source. `visible` is intersection with the clipped viewport, not page prefetch; `foreground` uses window focus and the owner's awake state (on Android also a resumed activity with the screen on). False transitions do not open a private-mode visit. The owner only forwards observations for IDs in the current page tree that asked for them, replaces a pending observation for the same ID, and applies the existing bounded page sample queue and cadence. A notice can be lost; never infer a positive observation from a timer or an `open`. |
| `press` | A `box` with an ID may include `"observe":["press"]`. The window then judges each touch or click on it itself and sends no `pointer` events for it: a tap or click sends `{"t":"press","id":"row"}`; a touch kept down for half a second without moving, or a press of the mouse's right button, sends `{"t":"hold","id":"row"}` instead, never both; a drag that starts on the box scrolls the scrolling box around it and sends nothing. A press on a pressed box inside another one goes to the inner box only. Not with `pinch` or `rotate` in the same list. Ask for it only when this owner lists `press`: an older owner refuses a tree that names it |
| `back` | A `box` with an ID may include `"observe":["back"]`. While it is on the page shown, Back (Android's Back button or gesture, or Escape on a desktop when no text field is being edited) sends `{"t":"back","id":"room"}` to it instead of the application going back, but only once whatever the application opened over the page (a list, a menu, the Settings sheet) is closed. When several boxes observe it, the newest one takes it. Every press is sent again while the box keeps observing `back`; stop observing it to give Back to the application again (it then goes to its first screen, and from there Android's Back leaves, by default at a second press). Ask for it only when this owner lists `back` |

`peersus page` asks the owner for the extension's tree, and `page-event` can send explicit actions to that page. Neither selects a page in a window or reports visibility; local CLI requests cannot submit `visibility` samples. If a window is already showing the same page, it can render the changed tree and report its own observations. An observation for a box that the current tree no longer observes is discarded without an error, including if the box disappeared while the sample was queued. Malformed events remain invalid.

## Extension-rendered canvases

`canvas` is offered to WASM, native-library and standalone process extensions wherever that execution kind is available. The extension owns its scene, layout inside the bitmap, and input interpretation. The application's window composites the finished raster and supplies shared controls, raw input, and accessibility; neither the window nor the owner interprets the extension's drawing commands.

```json
{"k":"canvas","id":"scene","observe":["size","wheel"],
 "style":{"width":{"unit":"px","value":640},"height":{"unit":"px","value":400},"clip":true,"follow":{"drag":true,"wheel_doubling":120}},
 "a11y":[{"id":"legend","role":"text","label":"Flow field","rect":{"x":12000,"y":12000,"width":120000,"height":24000}}]}
```

Canvas IDs use the normal identifier alphabet, are at most 64 bytes, and are distinct within a page. A canvas has box sizing and no declarative children. It forwards the existing `pointer`, `wheel`, `pinch` and `rotate` events as applicable. When it observes `size`, the first mounted measurement and changes of size, device scale or theme send `canvas_size`: `id`, `x`, `y`, `width`, `height`, `scale`, `palette`. Bounds are in thousandths of a global logical pixel; `scale` is device pixels per logical pixel times 1000. Compute a pixel dimension from these units as `ceil(dimension * scale / 1_000_000)`. `palette` gives `dark` and straight sRGB RGBA values for `surface`, `raised`, `text`, `muted`, `border`, `scrim`, `accent`, `success`, and `danger`. The owner coalesces continuous measurements and input; `null` is an appropriate page reply when the tree has not changed.

`a11y` is an optional list of up to 512 unique-ID overlays, also subject to the whole page's control-message byte limit. Rectangles are canvas-local in thousandths of a logical pixel. Roles are `button`, `link`, `image`, `text`, and `group`; buttons and links send `press` with the overlay's ID, while the canvas continues to receive its raw input.

### Bitmap gesture preview

Only a clipped `canvas` may use `style.follow`. The window translates or scales the most recent bitmap and its semantic overlays; the viewport's place, size, clip, measurement and input area stay fixed. A declarative `box` does not accept `follow`, even when it contains a `drawing`.

- `drag` moves the bitmap with the pressed pointer.
- `wheel_doubling` is the wheel travel in logical pixels per doubling (`0` disables the preview). With `wheel` observed, the scale factor is `2^(-dy / wheel_doubling)` about the pointer, limited to four doublings in either direction per sample.
- `pinch` previews the change in scale about the touch midpoint when `pinch` is observed.

An uncovered part of the viewport shows its background until the extension renders again. Rotation is sent to the extension and is not previewed. The incoming `PSF1` frame's `settled` field, not the page tree, supplies the last gesture `at` that the frame already shows. The window reapplies only later steps to that frame. Each gesture event on a previewing canvas has a later timestamp than the previous event.

The owner holds continuous events while an answer is outstanding and sends a held one when the answer arrives. It sums wheel `dx`/`dy` only for consecutive events on the same element with the same pointer position and vertical direction, then uses the latest `at`. A different anchor or a reversal keeps its order. Pointer moves, pinch samples and rotate samples keep the latest sample within a gesture.

### Opening and publishing

Import `host` / `peersus.canvas` / `1` / `open`. Once the page includes the canvas and a window is watching it, call `open` with `{"page":"demo","canvas":"scene"}`. The reply is `{"claim":…}`: an opaque, one-use `u64` expiring in five seconds and bound to this extension, page, and canvas. Preserve all 64 bits. Use one persistent connection per canvas. Unmounting the view, removing the declaration, or stopping the extension revokes the lease. Use a fresh claim on the next mount or after a broken stream.

`Open` means the canvas page is shown again, even if the host still has its previous element tree. Retire its old frame stream and discard any pending claim on `Open`. When the mounted view sends its first `CanvasSize`, use those metrics, open a fresh stream, and present the first frame as soon as the claim is ready; do not wait for a timer or for an old stream to fail. The first `CanvasSize` follows `Open` even when the dimensions did not change.

- **WASM:** open the claim with `data_open(PEERSUS_DATA_CANVAS, claim)`. Use the handle in the display list passed to `canvas_present(ptr,len)`, which rasterizes it with Skia in the WASM helper. These handles do not support raw reads, writes, shutdown or poll. `canvas_present` returns `0` when it writes a complete frame, `-1` for invalid arguments or state, `-2` for a work or rate limit, and `-3` if the stream failed and the helper closed the handle. It never replenishes WASM fuel.
- **Native library or executable using the C SDK:** open with the same `data_open` kind and write finished `PSF1` headers and pixel payloads using `data_write`. Use chunks of at most 65,536 bytes and handle partial writes. These handles do not support reads, shutdown or poll. Native extensions use their own renderer; `canvas_present` is a WASM import only.
- **Standalone executable implementing the broker protocol:** authenticate an ordinary data connection with the claim. On desktop, follow the redirect before writing `PSF1` frames; on Android, use the authenticated data connection itself. See [the process transport](module-abi.md#standalone-process-canvas-transport).

Close with `data_close` or close the raw connection. There is no per-frame acknowledgment: the consumer can drop a frame even after a complete write. If a partial frame cannot be completed, close the stream rather than appending another header to it. Pixels never travel in broker replies or snapshots.

### Display list and frame formats

All multi-byte display-list and frame fields are **little-endian**. A WASM display list starts with `PSD1`, then opcode **1** (`BEGIN`), `id_len:u8`, `id` bytes, `handle:i32`, `seq:u64`, `settled:u64`, `width:u32`, `height:u32`, `scale:u32`, then nine straight RGBA palette values in the order above (without `dark`). Width and height are device pixels. A colour is a `u8` tag: `0` followed by four straight RGBA bytes, `1` for transparent, or `2` through `10` for the palette in order. Drawing coordinates are finite `f32` in logical pixels.

| Opcode | Payload |
| --- | --- |
| 2 `CLEAR` | Colour |
| 3 `PATH` | Mode `u8` (0 fill, 1 stroke), colour, stroke width `f32`, cap `u8` (butt/round/square), join `u8` (miter/round/bevel), opacity `u8` (0–255), verb count `u16`, then verbs. Each verb tag is `u8`: move 0 or line 1 with one pair of `f32` coordinates; quadratic 2 with two pairs; cubic 3 with three pairs; close 4 with no arguments |
| 4 `TEXT` | Size `f32`, colour, X and baseline Y `f32`, alignment `u8` (left/center/right), UTF-8 byte count `u16`, one line of text. The helper shapes text with the bundled Quicksand font and available system fallback fonts |
| 5 `IMAGE` | Previously defined `id:u32`, then X, Y, width, height `f32` |
| 6 / 7 `CLIP_PUSH` / `CLIP_POP` | X, Y, width, height `f32` for push; no arguments for pop |
| 8 / 9 `TRANSFORM_PUSH` / `TRANSFORM_POP` | `a,b,c,d,e,f` as six `f32` for push: `(x,y) → (a*x+c*y+e,b*x+d*y+f)`; no arguments for pop |
| 10 `IMAGE_DATA` | Unique per-frame `id:u32`, image width and height `u16`, then tightly packed straight RGBA |
| 11 `END` | No arguments or trailing bytes; all pushes must be popped in reverse order |

Every finished raster starts with `PSF1`, `id_len:u8`, ID bytes, width, height and row stride (`u32` each), `format:u8` (1 = premultiplied sRGB RGBA8), `seq:u64`, and `settled:u64`. It ends with exactly `stride × height` bytes, followed by the next frame header if there is another frame. There is no separate frame-length prefix. Stride must be a multiple of four and at least `4 × width`; the WASM helper writes tight rows. `seq` is nonzero and increases on the stream. `settled` is zero before any gesture has been rendered. The consumer checks the ID against the claim before allocating pixels.

On desktop, a window listens on a private local endpoint and sends `MountCanvas {plugin, page, canvas, endpoint}` over authenticated local control IPC. The owner responds with `PFB2` followed by a status byte: `0` ready (then a 32-byte one-use token), `1` closed, `2` busy. The connection is a lease, not a pixel channel. The owner makes the same token and endpoint available to the extension's transport only after authenticating its `DataHello` and consuming the claim. The window accepts a matching token once and closes its listener. The direct connection uses a private Unix socket with a peer UID check on Unix and loopback TCP on Windows. The owner never reads a desktop raster. On Android the window and owner share one process: the window watches the owner's slot and holds the same allocation without another pixel IPC hop.

### Bounds

- All execution kinds share the same bounds: at most **4096 × 4096** device pixels and **32 MiB** per raster, **4** active canvases per extension, and **60 frames/s** and **256 MiB/s** per extension per receiving process. Retained raster storage (including a displayed frame and one in transit) is capped at **64 MiB** per extension in that process. The desktop window enforces the size, sequence, byte and rate limits on its direct stream; on Android the shared owner slot enforces them. A receiver can delay reading and drop an over-limit frame without blocking the UI event loop.
- Each WASM display list is at most **4 MiB**, **4,096 commands**, **32** nested clip/transform scopes, **512** verbs per path and **65,536** path verbs across the frame. Text is at most **1,024 bytes** per command, and at most **16** images may be defined. Coordinates and affine components are bounded to ±65,536, strokes to 1,024 logical pixels, and font sizes to 512 logical pixels. `scale` is 1 to 16,000.
- WASM helper rendering has a **120 ms** cooperative deadline, checked between bounded native operations; the time is the extension's own process's. It does not preempt an individual Skia call. Native libraries and process extensions do their own rendering; only the finished-frame bounds apply to what they send.

## The drawings an extension keeps

Use `drawing` for small graphics such as icons, gauges, and sparklines. Use `canvas` for maps or other dense or continuously redrawn scenes. A small drawing that reuses the same outlines, such as a chart grid or retained strokes, can keep them out of each page answer and place them by name. A drawing's inline outlines are capped at 16 KiB and the page tree at 128 KiB.

`host` / `peersus.drawings` / `1` offers `keep` and `forget` to plugins importing those exact methods. It requires no capability: only the caller's own layers are kept, replaced or let go of, drawn only where the caller's pages place them, and none of another extension's can be placed, read or forgotten.

* `keep` (`{"id":"grid/1","shapes":[{"path":"M0 0 L9 9","stroke":"accent","line":10}]}`) keeps the layer `id`, or replaces the one under that id, and answers `{}`. `id` is 1 to 64 characters of `a-z`, `0-9`, `.`, `_`, `/` and `-`. `shapes` is 1 to 64 shapes, each as a drawing's, and their outlines together at most **40 KiB** (40960 characters). Coordinates are the layer's own units. The whole call must also fit one control message (**48 KiB**, `MAX_PAYLOAD`): the outlines must leave room for shape metadata and the call envelope.
* `forget` (`{"ids":["grid/1","grid/2"]}`) lets go of those layers, up to 1024 ids; an id that is not kept is ignored. It answers `{}`.
* An extension keeps at most **1024 layers** and **16 MiB** of outline in all. A `keep` past either is refused (`refused`) and changes nothing. All of an extension's layers are forgotten when it stops, restarts, is turned off or is removed.

A drawing places them with `layers`, drawn in order under its own `shapes`:

```json
{"k":"drawing","width":600,"height":400,
 "layers":[{"id":"grid/1","scale":1000000,"at":[-12000,4500]}],
 "shapes":[{"path":"M0 0 L600 0","stroke":"border","line":10}]}
```

A point `p` of the layer is drawn at `at / 1000 + p * scale / 1_000_000` pixels of the drawing: `scale` is millionths (1..=1 000 000 000) and `at` thousandths of a pixel within ±16 384 000. The stroke's `line` is in drawing pixels and is **not** enlarged by `scale`. At most 256 layers are placed per drawing. The placement records count toward the page tree, but the retained outlines do not count toward the inline drawing or page-tree byte limits. A placed `id` the extension has not kept, or has let go of, draws nothing and is not an error.

What a layer holds never travels in a snapshot: `Snapshot::kept_layers` names each extension's set by an opaque `stamp`, and a window that does not hold it reads the local commands `layer-index` (`{"plugin":…}` → `{stamp, layers:[[id, stamp], …]}`) and `fetch-layers` (`{"plugin":…, "ids":[…]}` → the shapes of those layers). A fetch can be partial; the window asks for the remaining IDs before presenting the new layer set and page together. Each layer therefore reaches a window once for each change, and a window that opens later asks for all of them.

The window may temporarily stop painting an extension's `drawing` nodes if it exceeds its per-extension paint budget for several consecutive seconds. The drawings keep their size and show their background only. The application shows a notice and **Resume**; after repeated pauses it waits for manual resume. The extension keeps running and receiving normal messages and events. There is no protocol message for the paint pause.

## The pictures an extension keeps

A photo in a message, a cover, a preview: an extension shows a picture by making its pixels in its own process and keeping them under an id, which its pages place. The application never decodes a picture for an extension and never reads one from a path; it checks the size of what it keeps and draws it as it is.

**Decoding, in the extension's own process.** A WASM guest that imports `host` / `peersus.pictures` / `1` with the method `decode` runs in the helper that links the decoder (Skia) and may call the guest function `peersus_v1.picture_decode(input, input_len, max_edge, output, output_len) -> i64`; see [module ABI](module-abi.md). `decode` is a declaration and is never answered through the hub (a call is refused as `unsupported`). The input is an encoded PNG, JPEG, WebP, GIF (first frame) or BMP of at most 16 MiB, for example read from the extension's private storage after `peersus.files.stage-selected`. The picture is turned upright by its EXIF orientation and made no larger than `max_edge` (1 to 1024) on its longest side, proportions kept, never enlarged. A JPEG is decoded already smaller; a format that cannot be is refused past 48 million pixels. On success `output` holds `width × height × 4` bytes of unpremultiplied sRGB RGBA and the answer is `width << 16 | height`; otherwise `-1` (no decoder: `decode` was not imported), `-2` (not a picture this reads, or damaged), `-3` (past a bound) or `-4` (`output` too short). One call costs the guest two million units of interpreter fuel, whatever the picture. A damaged or enormous picture costs only the extension's own process.

**Keeping.** The same service offers `keep` and `forget`, no capability required: only the caller's own pictures are kept, placed or let go of.

* `keep` (`{"id":"photo/7","width":256,"height":192,"source":{"kind":"stage","stage":…}}`) reads, once, `width × height × 4` bytes of unpremultiplied sRGB RGBA from the caller's private storage (`source` as in `peersus.storage.read`: a sealed stage, or a committed key at an exact revision, so a picture kept in storage can be kept again after a restart without a new stage) and keeps them under `id`, or replaces the picture under that id. It answers `{}`. `id` follows the drawing layer id grammar; each side is 1 to 640 pixels. A source of any other length is refused as `invalid`. An extension keeps at most 64 pictures and 8 MiB of pixels; past either, `keep` is refused as `refused` and nothing changes. The read runs off the application's dispatch loop and counts against the caller's platform-call slots.
* `forget` (`{"ids":["photo/7"]}`) lets go of those pictures, up to 1024 ids; an id that is not kept is ignored. It answers `{}`.

What an extension keeps goes with it when it stops, restarts, is turned off or is removed; a `keep` still reading at that moment is not let in afterwards.

**Placing.** `{"k":"picture","id":"photo/7","width":240,"height":180,"radius":12}` shows the kept picture in a box `width` by `height` pixels (1 to 4096), covering it with its proportions kept and the overflow cut off, corners rounded by `radius` (at most 2048). A picture that is not kept, or not yet in the window, shows nothing at the same size, so the page does not move when it arrives. The owner writes the kept picture's `digest` into the element; an extension that writes `digest` itself is refused. A window that does not hold that digest asks the owner for the pixels with the local command `fetch-kept-picture` (`{"digest":…}`), once, as it asks for a device's picture; a page never carries them.

## The sound an extension plays

A voice message, a song: an extension plays a sound by decoding it in its own process and handing the finished samples to the application in chunks, which plays them on the device's output. The application never decodes a sound for an extension and never reads one from a path; it checks the size of each chunk and plays it as it is.

**The samples.** Always **48 000** frames a second of **2** interleaved signed 16-bit little-endian samples (4 bytes a frame), whatever the file was: the decoder below gives exactly this, and the application neither resamples nor mixes.

**Decoding, in the extension's own process.** A WASM guest that imports `host` / `peersus.audio` / `1` with the method `decode` runs in the helper that links the decoder and may call the guest functions `peersus_v1.audio_open(input, input_len) -> i64`, `audio_read(handle: i64, output, output_len) -> i64`, `audio_length(handle: i64) -> i64` and `audio_close(handle: i64) -> i64`; see [module ABI](module-abi.md). `decode` is a declaration and is never answered through the hub (a call is refused as `unsupported`). `audio_open` reads an encoded WAV, MP3, FLAC, Ogg Vorbis or AAC in MP4 of at most 24 MiB, keeps its own copy, and answers a positive handle; at most 2 are open at once. `audio_read` writes the next samples, as many whole frames as fit (at most one chunk's worth a call), and answers the bytes written, `0` once the sound ended. A mono sound comes out on both channels, one with more channels keeps its first two, and another rate is turned into 48 000 by linear interpolation. `audio_length` answers the frames of the whole sound at 48 000 when the file says. Negative answers: `-1` no decoder here, `-2` not a sound it reads or damaged, `-3` input past 24 MiB, `-4` two decoders open already, `-5` no such handle, `-6` length unknown.

**Playing.** The same service offers `play`, `queue`, `pause`, `resume`, `stop` and `state`, no capability required. One sound plays in the whole application: a `play` replaces whatever plays, whichever extension it is.

* `play` (`{"sound":"voice/1","bytes":384000,"source":{"kind":"stage","stage":…}}`) stops whatever plays and starts the caller's sound with one chunk read, once, from its private storage (`source` as in `peersus.storage.read`), exactly `bytes` long: a whole number of frames, at most 8 seconds (1 536 000 bytes). `sound` is the caller's own name for it and follows the drawing layer id grammar. It answers `{}` once the chunk is queued; the read runs off the dispatch loop.
* `queue` (same arguments) adds the next chunk to the sound the caller plays, named by the same `sound`. A caller holds at most 30 seconds not yet played; past it, or for a sound it is not playing, `queue` is refused as `refused` and nothing changes.
* `pause`, `resume` and `stop` (`{}`) pause the caller's sound keeping what is queued, go on with it, or stop it and let go of what is queued. They answer `{}` and change nothing when the caller plays nothing.
* `state` (`{}`) answers `{"sound":"voice/1","playing":true,"position_ms":12000,"queued_ms":3000}`: the caller's sound, whether it plays now, how far the device has played it since its first chunk, and how much is queued. `sound` is `null` once it stopped, ended or another sound replaced it. A sound whose queue ran dry plays silence for 2 seconds, in case the next chunk is late, and then has ended.

The device is opened only while a sound is there. What an extension plays stops when it stops, restarts, is turned off or is removed; a `play` or `queue` still reading at that moment is not let in afterwards.

**Recording.** With `audio.record` (granted once, for no device) the same service records from the device's microphone. One recording runs in the whole application.

* `record` (`{"max_seconds":120}`, 1 to 120) starts it and answers `{}`. On Android the system's own microphone question is asked first, from the open window only; refused, or still unanswered after 15 seconds, the call is refused as `refused`. Another extension's recording makes it `busy`; the caller's own is replaced.
* `finish` (`{}`) stops it and answers `{"stage":…,"bytes":…,"sha256":…,"duration_ms":…,"levels":[…]}`: a sealed stage of the caller's own storage holding a WAV of 16 000 frames a second, one signed 16-bit channel, which the caller commits or aborts like any stage (`peersus-api::audio::{Recorded, wav}`), and how loud each tenth of a second was (0 silence to 255, at most 1 200, the square root of the peak so that a quiet voice shows; `audio::level`). Nothing heard, or no microphone, is `unavailable` with the reason.
* `levels` (`{}`) answers `{"elapsed_ms":…,"levels":[…],"bands":[…]}`: how long the caller's recording has run; the loudness of its newest 48 tenths of a second, oldest first; and `bands`, how strong each of 32 ranges of frequencies (about 100 Hz to 8 kHz, lowest first, wider as they rise) is in the newest 1024 samples, 0 to 255 on a scale of decibels, for an equaliser shown while recording. `unavailable` when it records nothing.
* `cancel` (`{}`) stops it and keeps nothing.

The microphone stops by itself after `max_seconds`, whether or not the device gave samples; `finish` then hands over what was heard. A recording ends, unkept, when its extension stops or `audio.record` is taken back; one ended and neither finished nor cancelled for a minute no longer keeps another extension from recording. The device is opened only while a recording runs.

## Additional placements of declared pages

An extension's pages are drawn in its settings, whatever placement its manifest names: no page is among the application's pages (the main navigation, or **Extensions**) because a manifest says so. An extension that wants a page there gives the person an option of its own for it, a global toggle setting it declares with its own label and help (for example "Add Layout to page"), and places a copy of the page here while the person has that option on. An extension without such an option has its pages in its settings only.

`host` / `peersus.pages` / `1` offers `set` to plugins importing that exact method. It requires no additional capability: it can only offer copies of the caller's existing manifest pages, with unchanged sources, capabilities, device requirements and platform restrictions.

Call `set` with `{"pages":[{"id":"editor-main","page":"editor","placement":"primary"}]}`. `page` names a page in this caller's manifest; `id` is a new caller-local contribution ID. Only the copy's ID and placement change. The original page remains in the extension's settings, and both placements use the same exported source. Supported placements are `primary`, `settings` and `status`. A `primary` copy's `id` must be the `id` of the global toggle setting that is its option, and the person must have saved that toggle as on (a declared default does not count); otherwise the call is refused ("A page is placed among the application's pages only by its option, turned on by the person") and the previous set is kept. The application reads nothing else of the setting: its label, help and meaning are the extension's.

Each successful call atomically replaces this caller's additional placements and returns `{}`. `{"pages":[]}` removes them, not the manifest pages, and is always accepted. Unknown fields, missing templates, invalid or duplicate IDs, and more than eight total manifest-plus-additional pages are refused without changing the previous set. A caller cannot specify another owner or introduce an undeclared source. Removing or replacing a page withdraws its queued input, outstanding calls and cached view. Offers are transient: stopping, disabling or restarting the plugin removes them.

The plugin interprets its own settings and calls this API accordingly. Saving plugin settings restarts that plugin, so its new process republishes the desired placements. Frontends use the existing generic page rendering and routing, without setting-specific widgets or visibility conditions. A plugin importing this service requires a host implementing it; update the host and plugin together.

Where `primary` pages appear is the window's choice, not the extension's: by default each has a place of its own in the main navigation; the person's Appearance setting "Extension pages in one place" (`settings.extension_pages_together`) puts them all in one place, **Extensions**, whose top row switches between them (the row is shown from two pages on). An extension cannot ask for either and learns nothing of the choice. The pages of an extension whose agreement the person has not accepted are not drawn anywhere.

## Actions an extension takes back for now

`host` / `peersus.actions` / `1` offers `withhold` to plugins importing that exact method. It requires no additional capability: it can only take back actions the caller's own manifest declares, never add or change one.

Call `withhold` with `{"actions":["send"]}`: the set of this caller's action IDs that are not to be offered now. Each call replaces the whole set and returns `{}`; `{"actions":[]}` offers all declared actions, even those withheld initially. While an action is withheld the application omits it from the offered actions and refuses invocation, including presses already on their way from a window or another client. An undeclared ID, unknown field or wrong shape is refused without changing the last set. An identical set needs no new snapshot.

An already-running selected-file action remains visible on Send while withheld so the person can cancel it. This is not an offer to start it again; the cancel control goes away when that run ends.

`contributions.initially_withheld_actions` is an optional set of declared action IDs, used before the caller's first update and again after stop, disable or restart. An omitted set is empty. The extension can safely declare several static action labels for different modes without offering them before it knows which one applies. Action labels and short labels are nonblank, at most 80 characters and single-line (no control, line-separator or paragraph-separator characters).

An override applies only to the authenticated extension instance that set it. A new instance starts from the manifest default even when the host has not yet reported the restart, and an outstanding call from the previous instance cannot change the new offers.

If the instance whose offer the application checked is replaced before the broker admits a press, the broker refuses the submission rather than routing it to the replacement.

The plugin decides, by its own rules, when an action of its own makes sense (a destination not yet set up, a device that cannot do it); the application never reads a plugin's state to hide one. A plugin importing this service requires a host implementing it; update the host and plugin together.

## Caller readiness

An extension may import `host` / `peersus.config` / `1`, method `get` with `{}` and event `changed`. No separate grant is required. The answer names only the caller (`plugin`) and includes its effective global `settings`, `missing_permissions` on this device, and `peers`: `{device, online, settings, permissions}` for each device it may see with `peers.watch`. Each per-device `permissions` list is the caller's actual grant for that alias. Root `missing_permissions` includes its missing standalone permissions and includes the coarse `peers.watch` gate if it asks for it and no device is visible.

`changed` carries `{}` only when this subscriber's settings, grants, visible devices, or the routes it may see change. Another extension's untouched projection is not woken. Read `get` again to learn the latest scoped state. The event does not identify the change, grant or device and never authorizes replay. Check the permissions needed for the particular operation; the owner still checks each call.

### Permission readiness in a guest

Use the canonical `templates/guest/host.rs` in the pinned SDK; the copy under the guest's
`wasm/src/` must be byte-identical. Keep native glue byte-identical too when it is present. The
shared helper handles the wire contract; the extension decides which grants each operation needs.

1. Declare exact service imports and capability guards. Import `host / peersus.config / 1` with
   method `get` and event `changed`. Send `subscribe_config()` at start and read `get({})`.
2. Feed the answer to `Readiness::update`. On a matching `config_changed` event, read it again.
   The event carries no grants and does not restart a refused operation.
3. Before calling a guarded method, check `Readiness::missing(device, owner, name)` for each grant
   needed by that operation. Use no alias for standalone grants and the coarse list gate; use the
   caller-scoped alias for a per-device grant. No known miss is not a grant: the owner still authorizes.
4. If a local miss is known, do not call. Show “**{operation or destination}: missing permission
   {name} on this device.**” Leave unrelated granted operations active.
5. For a checked `Blocked::MissingPermission` that races a readiness read, remember the local
   denial with `Readiness::deny` until a fresh readiness answer, and stop the current operation.
6. For `Blocked::NotAccepted`, or `DeliveryAck::NotAccepted` from `read_ack`, stop without
   retrying or reporting a failure. At most show that the destination is not accepting it, without
   naming its grant. A malformed or absent acknowledgement is still a failure.

Control is the worked example: a stopped gesture is discarded and never resumed by a grant change
or timer. A new action starts only when the person deliberately chooses the destination again or
begins a new desktop crossing.

## The paired devices, as one extension may know them

`host` / `peersus.peers` / `1` offers `list` and `send` to an extension that imports them. Both are granted per device: `list` needs `peers.watch` and `send` needs `peers.send` for the device concerned. An extension granted `peers.watch` for no device gets `missing-permission`; otherwise `list` returns one entry for each paired device the person allowed it to see, and a device it may not see is not in the answer at all:

| Field | What it says |
| --- | --- |
| `device` | The name this extension uses for the device: what `send` takes back, and what an action or a receive hands over. Another extension is given another name for the same device; it is never the device's key |
| `name` | What the person called the device |
| `online` | Whether there is a connection to it now. True for a device that sleeps |
| `presence` | `online`, `sleeping` or `offline`: the application's own three words. A device that is connected and has said it sleeps is `sleeping`; one that is not connected is `offline` whatever it last said |
| `allowed` | Whether the person has allowed this extension, for this device, every permission it can be allowed for a device. It is this side's consent only: the other device decides for itself and may still refuse |
| `missing_permissions` | Per-device permissions for this caller that are not allowed here, as `{owner,name}` objects. Does not reveal the destination's grants |
| `accepts` | Which of THIS extension's own services (those it exports with `receive`) the device said it takes a stream to from this device now, by name: the same extension is there, running, and its person allowed this device. `[]`: it takes none (the extension is not there, not running, or not allowed for this device there). Absent: not known (an older device, or one not connected), never a guess. Another extension's services are never named |

A paired device advertises only changes to the routes this peer can use. It does not advertise an installation or grant change that leaves those routes unchanged. A reasonless refusal is kept in memory for the exact caller, peer, service and version under the authenticated, visible connection, even before its first advertisement. A change in acceptance of **that route**, loss of visibility or a new connection invalidates it; other routes do not. Up to 128 destination decisions are kept per owner, 16 per extension. Capacity is reserved before a new destination can be tried; filling it refuses new destinations rather than evicting a current refusal. A cache hit opens no stream, including for an older claim that has not yet started opening.

An extension that offers devices as destinations offers as ready the ones that are `online` by `presence`, `allowed`, and, for what it sends as a stream, whose `accepts` names the service it sends to; `accepts: []` is not ready (the extension is not there, not running or not allowed there). It shows the others as they are. `accepts` absent is not known: an older host says none, so there a device that is `online` and `allowed` is offered as before. What a sleeping device does with what it is sent is the extension's own matter (a file waits for it; input meant for a person at its screen does not belong there). Statuses that other extensions give a device are not in this list: they are read through `peersus.status`, behind its own permission. An older host sends `device`, `name` and `online` only; an extension that must run on one treats a missing `presence` as `online` or `offline` by `online`, and a missing `allowed` as true. A page can instead let the application choose the device for it, with the [`devices` element](#choosing-a-device-on-a-page), which applies these same rules.

### Private mode

A person can keep their device to itself (Settings › Private mode). It stays connected and sees its paired devices as before, but tells them nothing of itself, so on them it is `offline` (`online: false`, no statuses, no `accepts`) although a connection exists, and it takes no stream and no file from them. A device is shown as there only after it has said how it is over the connection, and stops being shown when it says it withdraws.

For an extension on the private device this means:

- `send` to a device the person started nothing for is answered `refused` ("Private mode is on: …"). Nothing an extension does by itself opens a device: asking, a timer, a clipboard change.
- What the person does opens one device: choosing it in the application's device chooser on the extension's page, running one of the extension's actions for it, input forwarded to it, or sending it a file. From then that device sees this one as it is and may start streams here, and the extension's `send` to it is admitted.
- The opening is that extension's for that device. It lasts while the person stays on the extension's page (its events, the ticks of an `interval` included, arrive only then) or the extension still carries a stream to the device, and for 30 seconds after; then the device is told nothing again and what it had started here is ended. What the extension sends and what the other device asks of it renews nothing. Another extension may send to a device while it is open, and does not keep it open.
- Opening a page with a device already chosen from an earlier visit opens nothing: the person chooses again.

## Choosing a device on a page

A page lets the application choose a device for it with the element `{"k":"devices","id":..}`: the application draws the chosen device's picture and name in a frame and, once it is pressed, its own menu of the devices the extension may see (the same devices `list` gives), each with its picture and name. Choosing one sends `{"t":"choose","id":..,"device":..}`, with `device` the alias `list` gives; choosing this device (offered only with `this_device`) sends `{"t":"choose","id":..}` with no `device`. That is all the extension learns.

| Field | What it says |
| --- | --- |
| `chosen` | The alias of the device chosen now; absent, nothing is chosen, or, with `this_device`, this device. An alias the extension may no longer name is shown as not chosen |
| `this_device` | This device is offered first, by its own picture and name. Default false |
| `pick.presence` | How a device may be reachable to be chosen: any of `online`, `sleeping`, `offline`. Default `["online"]` |
| `pick.allowed` | Only a device that is `allowed` by `list`. Default true |
| `pick.accepts` | Only a device whose `accepts` names this service, which must be one of the extension's own; another extension's is refused on every device. A device that said nothing is offered by the other conditions |
| `pick.status` | Only a device that shows a status of `plugin` (`{"plugin":..}`), or exactly that one (`{"plugin":..,"id":..}`), as `peersus.status` names it. Read only when the extension holds `status.watch`; without it no device is chosen |
| `show.dot`, `show.presence` | A dot in the colour of how it is reachable, and the word for it. Default true |
| `show.statuses` | The statuses it shows, in their authors' words, only with `status.watch`. Default false |
| `above` | The menu opens above the frame: a chooser at the foot of a page |

A device the conditions refuse is listed dimmed with the application's word for why (how it is reachable, `NOT ALLOWED`, `NOT READY THERE`, `NO STATUS`, `STATUS NOT ALLOWED`) and cannot be chosen; the owner refuses a choice of a device the chooser does not offer as it is shown. The devices themselves (`entries`) are filled in by the application: a tree that writes one is refused whole.

If the window still reports a choice for a chooser that has since disappeared, the owner discards it without forwarding it. A device choice is checked again when dispatched after a previous page call, so a removed chooser or device cannot grant access or receive delayed input.

## What paired devices say of themselves

`host` / `peersus.status` / `1` offers `define`, `set`, `clear`, `own`, `list` and the event `changed`. `define` needs `status.define`, `set` and `clear` need `status.set`, `list` and `changed` need `status.watch`, each granted once; `own` is the caller's own and needs nothing. `list` answers, for each paired device this extension may also see (`peers.watch`), `{device, name, presence, statuses: [{plugin, id, label}]}`, with `device` the same alias `peersus.peers` gives. `changed` carries `{}`: one payload goes to every listener, so it names no device; an extension reads `list` again to learn what changed.

## Arranging desktops

`host` / `peersus.displays` / `1` offers `inspect({})` with `displays.watch` and `arrange` with `displays.arrange`. The arrangement includes this device's display geometry and only the paired devices visible to the caller (`peers.watch`), identified by their caller-scoped aliases.

`inspect` returns `{devices, placements}`. A placement is `{device, x, y}` for a free, unscaled position. A placement may instead include `relative_to_local: "left" | "right" | "top" | "bottom"` to attach that device to the whole indicated side of this device. The stored `x` and `y` stay unchanged when it is attached and are used again when the field is removed. A present field must name a side (`null`, other values and unknown placement fields are refused). At most one relative desktop can use each side.

For an attached peer, its `devices` row includes `relative_to_local` and `relative_displays`: one `[x, y, width, height]` in shared-plane pixels per entry of the unchanged real `displays` array, in the same order. The owner scales all rectangles by one ratio (local bounding height / peer bounding height for left or right; bounding widths for top or bottom), rounds their positions and dimensions, and keeps each dimension at least one pixel. Without usable geometry, `relative_displays` is empty and there is no pointer route. In a visit, the ratio along the whole edge determines where entry and return land; replay uses the visited device's real geometry.

`arrange({expected, placements})` compares `expected` with the last visible placements, including `relative_to_local`. It validates and replaces those placements atomically, keeps hidden devices' placements, and returns the same shape as `inspect`. Submit the exact `placements` last read as `expected`; a differing arrangement is refused, not overwritten.

## Input mode negotiation

`host` / `peersus.input` / `1` offers `inspect`, `inject`, and `release`, each requiring the existing `input.inject` capability and an exact manifest import. `inspect` accepts only `{}` and returns `{"independent_pointer":true|false}`. It reports the adapter's supported mode without opening an input device, replaying input, or granting permission. The desktop adapter reports independent pointers only when a pen backend is configured; Windows and Android report false. OS policy or backend failure can still refuse a later injection.

An extension requesting independent pointers must inspect the receiving host first. A false response means it should explicitly use the shared-pointer contract (omit `pointer` and `show_name`); a missing method or malformed response is an incompatibility, not a successful negotiation. This additive method does not change existing `inject`/`release` calls. New extensions importing it require an updated host; old extensions remain supported by the host.

`inject` takes these arguments (declared once in `crates/peersus-app/src/platform_services.rs`, `input_arg`):

| Argument | Meaning |
|---|---|
| `events` | The batch to replay, at most the contract's batch size; all of it is checked before any of it is replayed |
| `owner` | The alias of the device replayed for; an input carrier must name an authenticated sender with a claimed receive. Extensions replaying input not bound to a device may omit it if they are not input carriers. What one device holds is released without clearing another's held input |
| `pointer` | `"own"` asks for a pointer of that device's own; absent for the shared one. Needs `owner` |
| `show_name` | `true` asks for the device's name beside its own pointer. The application decides the words; a `label` an extension writes is dropped |

One extension replays for at most sixteen devices at once. A device that has gone silent and holds nothing gives up its place; past the limit an `inject` for a new `owner` is refused.

## One extension carries input between devices

The manifest's `input` contribution names the extension's method that takes input captured on this desktop to another device. One extension carries it. A second extension that declares an `input` contribution is not started beside the first: the person is told, and chooses by turning one off. Nothing is refused at installation, since which of them runs is the person's choice.

If that same service exports `receive` and the extension imports `peersus.input.inject`, the owner also checks the standalone `input.inject` grant before admitting or advertising the route. The guest's reply is never evidence that the owner checked a grant. `inject` and `release` for a paired controller are both bound to the live receive that admitted them; a queued operation cannot affect a newer controller. Only an authenticated live visit can retain held input across completed copies.

For a desktop crossing the sending owner keeps one `plugin-visit` stream for that service and version. Its own QUIC stream number, scoped to the authenticated connection, is the visit ID named by subsequent extension data streams. The controlled owner admits the exact input route, returns the usual terminal frame with `received(0)` or reasonless `not-accepted`, and watches the open forward half for the visit's lifetime. A clean rotation carries no release and does not depend on when a receipt's transport acknowledgement is polled. Closure, reset, loss of the QUIC connection, or withdrawal of the controlled owner's prerequisite invalidates that visit and releases only its input, even with no data stream open. A vanished peer is bounded by the connection's negotiated QUIC idle timeout; ordinary stationary input on a live connection does not run a second idle timer.

The owner supplies `{peer, events, end, at}` with a caller-scoped `peer`, a checked input batch, and monotonic `at` in milliseconds. Only the first nonempty batch of a deliberate new desktop crossing adds `begin:true`. Ordinary input, rotation, rest and clean ends do not carry it.

A visit stopped by a checked refusal or grant withdrawal gets one `{peer, events:[], end:true, at, stop}`. `stop` is `{"status":"not-accepted"}` for a remote no, or `{"status":"missing-permission","permission":{"owner":…,"name":…}}` for a checked local miss. This callback discards the gesture locally; it must not open another stream to the destination. The owner itself takes down the paired visit. Stopped samples are not replayed. A later `begin:true` starts a new operation, subject to current grants.

Every input carrier declares an `edge_pressure` policy in its manifest. The Linux capture owner uses the running carrier's own global number, or `off`, in both directions. A positive number is the additional motion against an eligible edge before crossing; at zero an eligible activation or return overshoot crosses immediately. For departure, the compositor must first take the pointer at an arranged edge. For return, the source counts only the part of a motion past the edge, including its sideways component, not the approach. Backing off, sliding sideways or pausing for 300 ms clears the push. Idle time is measured at capture; zero-length samples do not reset it. Emergency or failure releases never wait for pressure.

The host refuses invalid bindings and out-of-range or nonnumeric setting writes, including through whole-preferences updates. This parameter belongs to the capture contribution, not to `peersus.input.inspect`, `inject`, `release`, or a network input frame. A receiver replays the resulting positions without measuring its own threshold. Origin capture needs a supported Linux InputCapture portal; other platforms do not offer it.

## Notifications an extension asks for

`host` / `peersus.notify` / `1` offers two methods, `show` and `opened`, and the event `opened`, to an extension that imports them and was granted `notify.show` by the person (granted once, not per device). The arguments of `show` are a notification exactly as the contract describes it (`peersus_api::notification`), and nothing else:

```json
{"text": "The backup is ready. Send it now?", "reach": "outside", "stays": false,
 "buttons": [{"id": "send", "label": "Send it"}, {"id": "later", "label": "Not now"}]}
```

- `text` is one paragraph of at most 500 characters. `reach` is `inside` (the default: only in the application's window) or `outside` (also outside the window, where the platform has a way, while the window is not in front). `stays` (default false) keeps one without buttons until the person closes it. `buttons` is at most three, each with an `id` of the extension's own and a `label` of at most 24 characters; the words are the extension's. `opens` (optional) is a word of the extension's own (an identifier: ASCII letters, digits, `.`, `_`, `-`, at most 128) that makes a tap on the notification itself, not on a button, bring the window forward on the extension's page: a page of its in the navigation, else its row in Settings › Extensions, where its settings pages are.
- The extension chooses what is said and what may be answered. It does not choose where the notification is drawn, its size, its colours or its order, and it is always shown under the extension's installed name: there are no fields for those, and an unknown field is refused.
- One that neither stays nor has a button is answered at once with `{}` and leaves by itself after seven seconds. One that waits is answered when the person answers: `{"answer": "pressed", "button": "<id>"}`, `{"answer": "dismissed"}` when they closed it, or `{"answer": "opened", "button": "<word>"}` when they tapped one that opens. The call's own deadline (at most ten minutes) is how long it may wait; when it passes, or the extension cancels the call or stops, the notification leaves the screen.
- One that opens and has no buttons does not make its call wait, even when it stays: it is answered at once with `{}`. When the person taps it, it leaves, and the extension receives the event `opened`, which carries nothing and goes to that extension alone; `opened` takes `{}` and answers `{"opened": ["<word>", …]}`, the words of its notifications tapped since it last asked, oldest first (at most eight are kept). A newer one with the same word takes the place of the one on show, and when two of the extension's are on show, the oldest of those that open and do not wait makes room for a new one, so a notification per conversation, say, never ends in `busy`.
- Refusals show nothing: `refused` for what is not a notification the contract allows (nothing is cut or repaired), `rate-limited` above six requests a minute (a refused request counts), `busy` while two of the extension's notifications are already on show and none of them can make room, `cancelled` when it left without an answer, and `denied` from the hub without the permission.

The application's own questions about an extension (the consent before a file leaves, below) are shown the same way under the extension's name, and count toward the two on show but not toward its rate.

## The person's language

`host` / `peersus.language` / `1` offers `get` and the event `changed` to an extension that imports them, both behind `language.read` (granted once). `get` takes `{}` and answers `{"language": "<tag>"}`: the language the person chose, else the device's, else `en`, as a lowercase tag with `-` between subtags (`ro`, `pt-br`). `changed` carries the same object when that language changes; an extension that cannot keep up misses it, and `get` always answers what is true now.

The extension translates its own words: the pages it sets, its notifications, what its program shows. It carries its translations itself, in whatever form it likes, and shows what it does not translate as it wrote it. The words its manifest declares (the labels and help of its pages, actions and settings, which `Contributions::words` lists, and its package's `description`) it may translate in its package: `languages/<tag>.po`, gettext files of the same format as the application's, each translating those English texts (`msgid "Share"` / `msgstr "Partajează"`). The window shows them in the person's language, else the primary language (`pt` for `pt-br`), else as written; a file whose header names another language than its name, or a translation that would not be a valid label, help or description, is left out. The window never looks up extension words in the application's catalog, even when the same English occurs in both. Ids, choices and every other value are never translated, nor are the licences and notice, nor what each capability lets it do: a person grants a permission on the manifest's own English, which the store reviews, never on words only its language files carry. A package whose manifest lists its `files` names them there too, or they are not installed. At most 64 language files are read, each no larger than an application language file. The SDK's packager (`scripts/package-native-extension.py`) takes them from `languages/` beside the manifest it packages (`<source>/languages/*.po`), puts them in the archive and lists them in `files`; anything else in that folder is refused. Before any submitted command runs, the store's check (`cargo xtask check-manifest`, through `scripts/check-extension-manifest.py`) refuses a file there that is not `<tag>.po`, cannot be read or names another language than its name, and prints as notes the translations the window would leave out and the declared words a file leaves in English; an extension that wants those refused too runs `cargo xtask check-manifest --complete-translations` in its own recipe (Clipboard does).

## How this device is doing

`host` / `peersus.system` / `1` offers one method per part of this device, each behind its own permission, granted once: the person decides part by part what an extension may see. Each takes `{}` and answers that part from what the operating system publishes now, each field `null` where it cannot be determined:

| Method | Permission | Answer |
| --- | --- | --- |
| `processor` | `device.info.cpu` | `{name, cores, mhz, busy, total, celsius}`: `mhz` is the cores' clock now, on average; `busy` and `total` are ticks since the system started; the share busy between two answers is the difference of `busy` over the difference of `total` |
| `graphics` | `device.info.gpu` | `{name, mhz, memory_used, memory_total, busy, celsius}`: its own memory in bytes, `busy` in hundredths of a percent |
| `memory` | `device.info.ram` | `{total, available}` in bytes |
| `storage` | `device.info.storage` | `{device, total, available}`: `device` is the system's name of the disk (`nvme0n1p2`), absent where it has none; sizes in bytes, of the volume received files go to, else of the application's data; no file or folder names |
| `network` | `device.info.network` | `{received, sent}` in bytes since the system started, every interface but the loopback; no networks or addresses |
| `battery` | `device.info.battery` | `{charge, status, plugged, minutes}`: `charge` in hundredths of a percent; `status` is `"charging"`, `"discharging"`, `"not_charging"`, `"full"`, or `null`; `plugged` is `true`, `false`, or `null`, independently of `status` |

The battery's `minutes` estimates time to empty while `"discharging"` or time to full while `"charging"`; it is `null` until an estimate exists. Android uses its system prediction for discharge when positive and at most seven days, otherwise it uses the same recent average as Linux. For charging, Android keeps the system's time to full, and Linux estimates from the measured rate if it knows the full capacity. Full, not charging, and unknown statuses have no estimate.

The measured estimate uses only samples requested by callers: Linux reads remaining energy (µWh) or charge (µAh), and Android reads remaining charge. The owner shares one sample per second across callers and keeps a ten-minute window without polling on its own. It measures between counter changes at least two minutes and 3,000 units apart, only when the two changes' timing uncertainties together fit within 5% of the shortest possible span. Sparse requests can therefore leave the estimate unknown. Between revisions the estimate counts down, and a revision can change that countdown by at most 5% before rounding to whole minutes. If the counter does not move for three times its recent average interval between changes (at least 15 seconds and at most ten minutes), the measured estimate clears. A battery or status change, a counter reversal, a missed counter, or a gap over 90 seconds starts a new history. No estimate exceeds seven days.

On Linux, `plugged` uses online mains, wireless, or USB system inputs. A peripheral supply or USB source does not count, and an ambiguous USB port does not establish incoming power. Android reads `status` and `plugged` independently from the battery-changed broadcast. Missing or unrecognised values are not inferred from each other.

A part not granted is answered `denied`, and the others still answer. On Linux and Android these come from `/proc` and `/sys` where readable; elsewhere only `cores` and `storage` are known today. Other rates and history are the extension's to make.

Android closes some of those files to an application, and then the application reads the same thing elsewhere: the processor's temperature is the hottest of the thermal zones that say they are a core's (the cooling devices beside them are not read), the graphics come from the `kgsl` driver (its model, how busy it was over its last window, the hottest of its own zones; its clock and memory are closed and stay `null`), and the network comes from `TrafficStats`. The battery's charge and charging time come from `BatteryManager`; its discharge prediction comes from `PowerManager`. `busy` and `total` of the processor come from `/proc/stat`, which only the shell may read: the community distribution reads that one line through a shell-mode Stellar/Shizuku provider the person already allowed for this application, and without one they stay `null`. Nothing is asked of the person for it.

## Guarded methods belong to the application

A method's `capability` is granted by the person to the application, for a device or outright, and is what lets the application call that method. There is no grant between two extensions: an extension that imports another extension's guarded method is answered `denied` whatever the person has allowed, while its unguarded methods are open to the extensions its `consumers` names. This is decided, not an omission: nothing shown at consent today says "this extension may use that extension", and no extension needs it. An extension that wants to offer something to other extensions exports it unguarded and names its consumers.

## Android file-selection and HTTPS host services

Implementation: `crates/peersus-app/src/platform_services.rs` and `crates/peersus-ui-android/src/extension_services.rs`. These are application-owned services, not provider-specific upload logic. A manifest must import the exact service/version/methods, and the user must separately grant the host capability. Installing or starting the extension does not grant either capability.

| Provider / service / version | Methods | Host capability |
| --- | --- | --- |
| `host` / `peersus.files` / `1` | `choose`, `selected`, `release` | `files.select` |
| `host` / `peersus.http` / `1` | `configure`, `saved`, `check`, `authorize`, `request`, `present`, `release` | `network.http` |
| `host` / `peersus.location` / `1` | `current` | `location.read` |

Every outbound request through `peersus.http`, including a credential check or public-resource fetch, sends `User-Agent: Peersus/<version>`, using the application's build-time crate version. The header contains only the product name and version; no operating system, device, extension, or user information. Extensions cannot declare, override, or remove it.

Arguments and successful `Outcome.value` objects:

| Method | Arguments | Successful value |
| --- | --- | --- |
| `peersus.files.choose` | Optional `max_bytes`: positive integer, at most 250 MiB; optional `kind`: `any` (default), `image`, `audio` or `video`, which picker the platform opens (on Android the photo picker for `image`, otherwise the document picker for that type; on a computer a dialog filtered to those files). It narrows what is offered, not what is accepted | `file`: opaque handle; `name`, `bytes`, `sha256`: selected-file metadata |
| `peersus.files.selected` | `context`: nonzero host-issued action context; optional `max_bytes` as above; no `kind` (the file is already chosen) | Same snapshot metadata as `choose`, using the existing Send selection without opening another picker |
| `peersus.files.release` | `file`: this caller's selection handle | `{"released":true}` |
| `peersus.http.configure` | `origin`: HTTPS origin, with the same restrictions as `authorize` | `{"configured":true}` only after a successful save or removal; not an upload authorization or credential-presence assertion |
| `peersus.http.saved` | `origin`: HTTPS origin, with the same restrictions as `authorize` | `{"saved":true}` when the store confirms a credential for this caller and origin, or `{"saved":false}` only on confirmed absence. If the store cannot confirm either, the host returns `unavailable` instead of a boolean. No destination is contacted and no credential data is returned. |
| `peersus.http.check` | `origin` as above; `path`: a path within that origin | `{"status":N}`: the status code the origin answered one `GET` of that path with, sent with the saved credential as the bearer. No file, no person asked; the response body is not returned. Refused when no credential is saved |
| `peersus.http.authorize` | `file`: this caller's selection; `origin`: HTTPS origin with no user information, query, fragment or non-root path; optional `headers`: at most eight lower-case names of request headers of its own the extension will send with this authorization. The person is shown them with the file and the address. Names the transport or the application writes, or that a server or proxy reads as where a request came from (`host`, `authorization`, `cookie`, the lengths and encodings, `proxy-…`, `sec-…`, `x-forwarded-…` and the like: a browser's forbidden request headers and the forwarding ones) refuse the call before the person is asked. Optional `anonymous: true` skips reading a saved key. Optional `result_origin` names an HTTPS origin for result links only, never for requests; a different origin is shown in the same consent | `authorization`: opaque handle, only after explicit file/destination consent and (unless anonymous) saved-credential use |
| `peersus.http.request` | `authorization`, `method`, `path`, and the bounded options below | For 2xx: `status` and parsed `json` by default, or `text` when explicitly requested; otherwise only `status` |
| `peersus.http.present` | `authorization`: this caller's authorization handle; `link`: provider-validated, result URL at `result_origin` if supplied, otherwise at the request origin | `{"presented":true}` after native presentation succeeds |
| `peersus.http.release` | `authorization`: this caller's authorization handle | `{"released":true}` |
| `peersus.location.current` | none | `{latitude, longitude, accuracy, age}`: degrees, metres (`null` when the system does not say how sure it is) and seconds since the system found it. Refused as `unavailable` when the system says nothing (no location service, location off, no fix within 20 seconds) or the person's setting refuses (location turned off on the device, or not allowed to the application by the desktop's location portal); on Android the system asks the person for its own permission the first time, when the window is open. The position is the system's, found its own way (it may ask an online service of its own with the wireless networks it sees); the application keeps none |

### Public binary HTTPS resources (`network.public`)

These `peersus.http` methods use a separate, outright permission from `network.http`. They never use a file, body, saved key, cookie, or caller-provided request header. The person grants access by enabling a declared `public_read` toggle in the extension's Settings. The switch names the exact destination before activation; the host binds the decision to the extension, installed agreement, origin, and path root. A guest cannot initiate a confirmation. Disabling the setting, removing the extension, or revoking the permission invalidates the authorization.

| Method | Arguments | Successful value |
| --- | --- | --- |
| `restore-read` | `origin` (HTTPS hostname at port 443), `root` (canonical absolute path) | `{"authorization":"handle"}` for the approved path; otherwise refused silently |
| `authorize-read` | Same origin and root as `restore-read` | Compatibility for earlier modules; it restores existing approval only and never opens a notification |
| `get-resource` | `authorization`, `path` (the root or a descendant; no query or fragment); optional `keep: false` | `{"resource":"handle","bytes":N}`; at most 2 MiB on the wire and 4 MiB after host-side gzip decoding |
| `read-resource` | `resource`, `offset` starting at zero | `{"hex":"...","end":true/false}`, at most 16 KiB decoded; the final chunk releases the buffer |
| `release-read` | `authorization` | `{"released":true}` |
| `resource-cache` | none | `{"resources":N,"bytes":N,"most_bytes":N}`: the copies this device keeps for the caller |
| `clear-resource-cache` | none | The same, after every copy kept for the caller is removed |

The host pins each request to resolved public addresses, disables redirects and retries, and bounds the entire download in time. The handles belong to the caller and its current generation. Each caller can hold one origin authorization and the latest response, with a 240 ms minimum interval between downloads. Public reads have their own bounded pool and do not use the upload workers.

A downloaded resource is kept on this device, in the application's private data, one file per address and per extension, unless the call said `keep: false`, which neither reads nor writes a copy. A kept copy younger than 30 days answers `get-resource` for the same address without the network and without the download interval. An extension keeps at most 128 MiB and 16 384 copies; past either, the oldest copies are removed. `resource-cache` and `clear-resource-cache` see and remove only the caller's own copies and need no authorization; removing the extension removes them with its approval. An address that names data which changes (an index rather than a versioned file) should be read with `keep: false`.

File selection uses the Android document picker or the trusted Send selection and a private immutable snapshot, not ambient filesystem access or a path passed by the guest. Selected-action contexts are random, caller/generation scoped, expiring and single-use; revocation invalidates them. Redeeming a context does not consume the original Send selection. There are at most two live snapshots per caller and four globally. Releasing a file also invalidates its associated authorizations. Authorization binds one snapshot, caller generation and origin; credential bytes are not returned to the guest.

Configuration is separate from file selection and upload consent. Its native dialog can save or remove the origin-scoped encrypted API key and show whether a record exists without decrypting it. Successful writes close the dialog with explicit native confirmation; storage failures keep it open with an error. Cancel returns `cancelled`, never a successful save. Configuration creates neither a file snapshot nor an HTTPS authorization; each upload still needs explicit file/destination consent.

On Linux, `saved` searches the Secret Service collections without reading the key or asking for an unlock. A confirmed match wins even if another collection is locked or unreadable. If there is no confirmed match, an inconclusive search leaves presence unknown rather than reporting absence. `check` and keyed `authorize` still use the platform's normal credential read and may require an unlock. If that read fails, the host returns `unavailable` with “The credential could not be read”; a check does not contact the destination and authorization does not ask for consent in this case. An explicitly anonymous authorization does not look up a credential.

Consent is asked by the service itself, the same on every platform, as an in-app notification under the extension's installed name: the file's name and size, the address, that the file goes there as it is, and the request headers the extension named, with the buttons `Allow` and `Don't allow`. Every word of it is the application's; the extension supplies none. For a keyed request a key must already be saved for the address, or the call is refused before the person is asked anything. An anonymous request states that the application will not use a saved key. A distinct result origin is also shown; it never authorizes sending the file there. `Don't allow`, or closing the notification, returns `cancelled`; so does no answer within 300 seconds, and so does the extension stopping, which also takes the notification off the screen. A name longer than 60 characters is shown with its middle left out; the address never is, and a request whose description does not fit a notification (500 characters) is refused with `platform-refused` and nothing is sent. The extension's own limit of notifications on show applies: with two already waiting, the call is refused.

HTTPS request options:

- `method` is `GET`, `POST`, `PUT` or `DELETE`. Resolving `path` must stay within the approved HTTPS origin; user information and fragments are refused.
- `bearer: true` asks the host to add the approved credential as a sensitive bearer header; anonymous authorizations reject this option. `headers` holds at most eight entries: `content-type`, and the request headers this authorization was asked with (see `authorize`). Any other is refused before anything is sent. The application knows no header by a destination's name for it.
- At most one body may be supplied: `json`, `part` or `multipart`. Serialized JSON is at most 64 KiB, still subject to the smaller overall control-message bound. File parts use `offset` and `length` within the approved snapshot and are bounded to 6 MiB.
- `multipart` also requires a `field` containing 1–64 ASCII letters, digits, `_` or `-`. Optional `fields` contains at most eight string values, each at most 1024 bytes, with keys at most 64 bytes. The requested file-field name is retained.
- Optional `capture_secrets` is an array of at most four JSON pointers, each at most 256 characters. The named response strings are kept in the host and replaced with `{"secret":"opaque-handle"}`. These authorization-scoped references can be used in subsequent JSON bodies or permitted headers; they cannot be borrowed by another authorization. Response capture is explicit, not a claim that arbitrary unknown provider fields are automatically classified as secrets.
- HTTP requests never display links. After validating the provider's success response, call `peersus.http.present` with the caller-owned `authorization` and a `link` of at most 2048 characters. The URL must stay on the approved result origin (`result_origin`, or the request origin when omitted), contain no user information or fragment, and must not echo known authorization secrets. The host shows the link as an in-app notification under the extension's installed name, with one button, `Copy link`, and returns `{"presented":true}` once the person pressed it or closed the notification; the link is copied to the clipboard only when they press the button, whole even when it is shown with its middle left out (over 160 characters), and is never sent to a peer. The former inline `present_link` request option is refused before any network request.
- `response: "text"` accepts at most 4 KiB of printable ASCII plus tabs and line endings; it cannot be combined with `capture_secrets`. The guest must validate the content before using it. The default `response: "json"` keeps the existing behavior.

For a selected-file action, the owner observes the distinct file payload bytes consumed by the HTTP client across its raw parts or multipart file fields. Headers, multipart framing and JSON do not count. The window renders the owner's `[sent,total]` snapshot, coalesced to at most four numeric updates per second per action. This is not a broker event or a new guest permission. Sending every byte does not confirm storage or success; only the action's terminal reply says how it ended.

The client uses Android platform certificate verification, no ambient proxy, no redirects and no automatic retry, with a 15-second connection timeout and a 120-second request timeout. Successful JSON responses are bounded to 32 KiB; known credential/session-secret echoes are refused. At most 128 captured secrets are retained per authorization, each bounded to 16 KiB.

Picker or consent cancellation returns the service error code `cancelled`; a chosen file that is empty, not a regular file or larger than the call's `max_bytes` returns `too-large`; other adapter refusals use `platform-refused`. Hub-level admission failures still follow the common permission/lifecycle rules. Releasing or revoking an authorization does not undo a request already accepted by an external service: inspect any external result before retrying after an interruption.

Ordinary broker calls remain bounded to 30 seconds. The authenticated application can invoke an explicitly declared interactive action up to its declared bound of 600 seconds. Separately, only the live host can configure bounded interactive deadlines for its own exported picker/consent/HTTP methods; an extension cannot extend those deadlines through a wire operation or by changing its manifest.

## Outbound data completion

An admitted `peersus.peers.send` returns a one-use `claim`, not delivery success. A cached refusal instead returns `{"status":"not-accepted"}` without a claim. After authenticating its data connection with a claim, the sender writes its raw payload and half-closes its write side. Read the outcome from that same connection even if writing or half-close stopped early. It is one length-prefixed JSON frame (maximum 1024 bytes): either an unsigned 64-bit byte count, or `{"status":"not-accepted"}`. The latter is a neutral stop, not a confirmed byte count or a failure; do not retry.

An issued, unexpired claim has room reserved for its neutral answer even if permission or route refusal retires it before the guest connects. Until it is collected or expires that reservation is not available for a new claim.

Report success only when the count matches the complete payload. The owner requires both the matching application receipt and bounded confirmation that the forward QUIC stream acknowledged all its bytes. A successful local write or half-close is not confirmation. EOF, missing transport confirmation, mismatched counts, malformed replies and expiry are failures. Even a valid delivery receipt does not prove the receiving guest applied the content or a remote user accepted it.

On the paired connection, the receiving owner sends one version-1 terminal frame on the reverse half of the existing extension stream: a big-endian 32-bit length and at most 512 JSON bytes, then EOF. It returns `{"status":"received","version":1,"bytes":N}` after a complete copy, or exactly `{"status":"not-accepted","version":1}`. The same refusal carries no identity, reason or grant name, even to the sending owner. Only the receiving owner can warn about a missing extension or grant it checked itself. A refusal can arrive before the payload ends. Unrecognized versions, extra fields, trailing bytes and missing confirmation are transport failures. Once the frame begins or the forward copy ends, the terminal read is bounded to five seconds; forward acknowledgement is also bounded. This protocol is for matching owners and applies neither to ordinary file streams nor pairing.

Withdrawing a grant used by a parked or live extension transfer ends it without waiting for its next sample. When the receiver withdraws, it returns the same reasonless refusal. When the sender withdraws, its extension-only reset lets the receiver cancel the announced call, including an unclaimed one, and release the exact controller's held input. The person deliberately switching it off does not get a failure or warning for that withdrawal. Granting it again only makes a new operation possible.

## Private extension data and stable references

These services use caller-authenticated hub registration and exact imports. They grant no ambient filesystem, network, credential, or window access. `peersus-api::extension_data` defines the typed requests, limits and validation.

- `peersus.peers` v1 `new-scope({})` returns `{"scope":"…"}`, 32 random bytes as lowercase hex. It is not an authority. `references` takes `{devices:[alias,…],scope?,include_self?}`: at most 64 distinct currently visible aliases, guarded by `peers.watch`. It returns `{peers:[{device,local_ref,scoped_ref?},…],local?}`. `include_self: true` requires a scope; `local` has `{scoped_ref,name}`. Local references survive an owner restart but change after forgetting and pairing again. Only these local references and the returned `device` alias can be correlated by this caller; a reference neither reveals a key nor works as a route. Scoped references agree across devices for the same scope and extension; use a fresh scope for each independent group or activity.
- `peersus.clock` v1 `now({})` answers `{monotonic_ms}` in 10 ms increments. This is elapsed owner time, not a timestamp to send to another device, compare with frontend events or keep over owner restarts. The broker rates calls. `wall({})`, guarded by `clock.wall` (granted once, for no device), answers `{unix_minute, utc_offset_minutes}`: the minute it is, in whole minutes since 1970-01-01 UTC, and how far this device's time zone is from UTC then, in minutes east (`peersus-api::extension_data::WallTime`, with `valid()` and `local_day_and_minute()`). Nothing finer than a minute and no zone name is given; the answer is this device's clock, which may be wrong or change.
- `peersus.storage` v1 uses private on-device namespaces. `list{prefix?,after?,limit}` returns `{revision,keys,more}`; for a consistent scan, start again if a page changes revision. A key has ASCII letters, digits, `-` and `_` separated by `/`, at most 224 bytes; no paths are exposed. `read{source,offset,length}` requests up to 64 KiB from an exact committed revision or a caller-owned sealed stage. `source` is `{kind:\"key\",key,revision}` or `{kind:\"stage\",stage}`. The reply `{claim,bytes}` opens an existing one-use data stream with exactly that much data and EOF. The guest cannot read another caller's claims or the original file.
- `stage{bytes}` creates a private `{stage}` for at most 8 MiB. `append{stage,offset,length}` returns a one-use `{claim,bytes}` for up to 64 KiB. Write exactly the returned count, half-close, require one byte `0x01` followed by EOF, then close. Short/excess writes publish nothing; a length or offset mismatch never advances the stage. `seal{stage}` requires the exact promised length and returns `{stage,bytes,sha256}`. `abort{stage}` discards an unpublished object. Outstanding claims expire after five seconds. Temporary stages have a five-minute ceiling, separate 24 MiB budget, and no authority after the guest generation stops.
- `commit{if_revision,put?,remove?,remove_prefix?,remove_from?,receive_ticket?}` atomically replaces a namespace revision and returns `{revision}`. A put supplies `{key,text}` (up to 8 KiB, subject to the control frame limit) or `{key,stage}` for a sealed object. The regular batch has at most 64 keys/prefixes. `remove_from` can optionally point to one sealed, at most 512 KiB UTF-8 JSON array of at most 2,048 additional exact keys. Namespace limit: 8,192 objects, 96 MiB live bytes. Wrong revision, invalid overlap or exhausted quota must not publish a subset. A receive ticket must belong to this caller's claimed, live peer stream; cancellation or device setting/grant withdrawal invalidates its fence. After a lost commit reply, re-read the revision and affected records; do not blindly replay.
- `peersus.files` v1 `stage-selected{file}` (`files.select`) accepts a caller-bound snapshot from `choose`/`selected` and returns the sealed storage stage. Release the file selection independently. `save{key,revision,suggested_name}` (`files.export`) offers a system dialog for an exact object revision. It reports `{saved:true}` only after complete destination I/O. It never accepts a guest path, opens an external viewer, or creates a reusable URI grant. An aborted OS/provider write may leave a partial destination; it is not success. Exported copies are no longer part of extension storage.

The per-extension payload quota is **120 MiB**: at most **96 MiB committed** and **24 MiB reserved for staged objects** (at most 10 stages and 8 MiB per object). Index and atomic-publication scratch are bounded separately. A clear transaction unlinks the removed committed objects; an extension explicitly aborts unpublished stages belonging to the cleared data. A stopped generation cannot reopen its claims or stages; restart recovery removes abandoned temporary files. On package removal the owner retires the whole private namespace (including staged bytes and its reference binding) before allowing the same package name to be installed again. If the filesystem refuses retirement, removal is refused rather than handing the old namespace to a replacement; if the filesystem refuses final cleanup of an already retired directory, it stays inaccessible to extensions and the owner reports a fixed warning. This is not physical flash erasure, a cloud-backup removal, or deletion of the user's exported copy.
