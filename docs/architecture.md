# Architecture and ownership

## Workspace map

| Component | Responsibility | Boundary |
| --- | --- | --- |
| `peersus-core` | Commands, snapshots, settings patches, pairing rules and per-device plugin grants | No GUI toolkit or concrete extension implementation |
| `peersus-net` | Authenticated transport, invitations, reconnect, file transfer and local persistence | Owned by the application; independent of the renderer |
| `peersus-app` | Single owner, commands, local IPC, plugin lifecycle and store policy; bounded relay of finished plugin canvas frames | It validates headers, not display lists, and never rasterizes a plugin's canvas |
| `peersus-api` | Shared identifiers, manifests, contributions, page elements, canvas wire formats, permissions and service messages | Plain data and validation; no hub, process, OS handle or GUI toolkit |
| `peersus-plugin` | Runs plugins for the application: the hub that routes calls and events, process supervision, control/data framing and platform-provided launchers | Speaks only the `peersus-api` contract; execution is separate from manifest policy |
| `peersus-plugin-runtime` | WASM interpreter, bound canvas handles and frame writers; separate native helper and C SDK | Execution and canvas rendering happen in the extension's process, not in the application |
| `peersus-render` | Validates `PSD1` drawing commands and rasterizes them with Skia CPU | Linked into the extension's own process (desktop WASM helper, Android isolated WASM worker); neither owner nor window runs it |
| `peersus-plugin-android` | Android extension worker entry | Android lifecycle and Binder integration are platform concerns |
| `peersus-clipboard-platform` | System clipboard owner and change observation | Shared by native extensions and the application's clipboard service; itself not an extension |
| `peersus-diagnostics` | Structured diagnostics shared by the desktop and Android processes | Each process initializes it; fields are made safe where they are emitted |
| extensions (not in this workspace) | Concrete functionality: clipboard sharing, cloud upload, remote control | Submitted on `submission/<id>-<date>` branches under `extension/<name>/` against the `extension` base branch and installed through the store; `main` builds and ships none. `plugins/` is an ignored local working copy |
| `peersus-store-fetch` | Desktop catalog/archive download helper | Downloads bytes; the application verifies trust and installs |
| `peersus-ui` | Shared Freya interface and command adapter | Displays shared lightweight controls and composites finished canvas frames; forwards raw input and exposes accessibility overlays |
| `peersus-ui-android` | Android entry, JNI and platform adapters | Owns Android-specific integration, not domain rules |
| `xtask` | Quality gates, builds, native helpers and catalog tooling | Local build preparation is not publication |

### The crates as Cargo declares them (generated)

What each crate says it is, which other crates of this workspace it builds against (test-only dependencies are left out), and its modules. Lowest first: a crate only builds against crates above it in this table, and the generator refuses a circle. Written by `python3 scripts/gen-docs.py` from the manifests and the source folders; the gate refuses it when it is stale. Do not edit it by hand.

<!-- CRATES-START -->
| Crate | What it is, in its own manifest | Builds against, of this workspace |
| --- | --- | --- |
| [`peersus-api`](../crates/peersus-api) | The Peersus contract shared by plugins and the application: manifests, contributions, elements and messages | none |
| [`peersus-clipboard-platform`](../crates/peersus-clipboard-platform) | Shared isolated system clipboard owner and change observation | none |
| [`peersus-store-fetch`](../crates/peersus-store-fetch) | Bounded HTTPS download for the extension store, kept out of the application owner | none |
| [`peersus-core`](../crates/peersus-core) | Toolkit-independent Peersus domain: model, commands, settings and mutual pairing | `peersus-api` |
| [`peersus-diagnostics`](../crates/peersus-diagnostics) | Application-owned structured diagnostics for desktop and Android | `peersus-api` |
| [`peersus-render`](../crates/peersus-render) | Bounded CPU rendering for a separately supervised Peersus extension | `peersus-api` |
| [`xtask`](../xtask) | Peersus build and quality gate tooling: checks, builds, catalog packaging | `peersus-api` |
| [`peersus-net`](../crates/peersus-net) | Peersus identity, encrypted transport, mutual pairing, remembered peers and file transfer | `peersus-api`, `peersus-core` |
| [`peersus-plugin`](../crates/peersus-plugin) | Runs Peersus plugins for the application: the hub that routes their calls and each plugin's isolated process | `peersus-api`, `peersus-diagnostics` |
| [`peersus-app`](../crates/peersus-app) | Peersus application owner: runtime, plugin host, command line and local control | `peersus-api`, `peersus-clipboard-platform`, `peersus-core`, `peersus-diagnostics`, `peersus-net`, `peersus-plugin` |
| [`peersus-plugin-runtime`](../crates/peersus-plugin-runtime) | Bounded interpreter and transport for separately supervised plugin modules | `peersus-api`, `peersus-diagnostics`, `peersus-plugin` |
| [`peersus-plugin-canvas`](../crates/peersus-plugin-canvas) | Canvas renderer for dedicated WebAssembly extension workers | `peersus-api`, `peersus-plugin-runtime`, `peersus-render` |
| [`peersus-ui`](../crates/peersus-ui) | Peersus interface: renders the owner's Snapshot and sends it Commands. No business logic. | `peersus-api`, `peersus-app`, `peersus-core`, `peersus-diagnostics`, `peersus-net`, `peersus-plugin` |
| [`peersus-plugin-android`](../crates/peersus-plugin-android) | JNI descriptor adapter for the separately managed Android WASM worker. | `peersus-diagnostics`, `peersus-plugin-canvas`, `peersus-plugin-runtime` |
| [`peersus-ui-android`](../crates/peersus-ui-android) | Peersus on Android: the entry the system calls, and the owner started inside the application. The interface is peersus-ui. | `peersus-api`, `peersus-app`, `peersus-core`, `peersus-diagnostics`, `peersus-net`, `peersus-plugin`, `peersus-ui` |

Modules directly under each crate's `src/`:

- `peersus-api`: `agreement`, `canvas`, `chord`, `contributions`, `elements`, `error`, `language`, `manifest`, `message`, `name`, `notification`, `package`, `permission`, `platform`
- `peersus-clipboard-platform`: `desktop`, `owner`, `watch`
- `peersus-store-fetch`: none
- `peersus-core`: `address`, `agreements`, `bindings`, `config`, `layout`, `model`, `nearby`, `pairing`, `peer`, `plugins`, `private_store`, `profile`, `relay`, `secret`
- `peersus-diagnostics`: none
- `peersus-render`: `display`
- `xtask`: `bench`, `catalog`, `judge`, `languages`, `manifest`, `update`
- `peersus-net`: `admission`, `announce`, `atomic`, `ceremony`, `file_session`, `folder_picked`, `folder_sender`, `grants`, `handshake`, `held`, `history`, `invitation`, `invitations`, `key_file`, `network_host`, `pairing_tasks`, `peers`, `picked`, `picture_owners`, `pictures`, `profile_exchange`, `reconnect`, `relay_policy`, `relay_probe`, `settings`, `streams`, `token_file`, `transfer`, `transfer_tasks`, `transport`, `walk`
- `peersus-plugin`: `data`, `frame`, `hub`, `link`, `module`, `pump`, `service`, `supervisor`, `transport`
- `peersus-app`: `aliases`, `app_update`, `archive`, `binary_metadata`, `board`, `catalog`, `cli`, `clipboard_handles`, `desktop_capture`, `desktop_clipboard`, `desktop_input`, `desktop_services`, `discovery`, `display_service`, `displays`, `drawing_layers`, `extension_prompts`, `identity`, `identity_unlock`, `in_flight`, `input_follow`, `instance`, `ipc`, `languages`, `lifetime`, `local_pipe`, `nearby`, `notifications`, `offer_panel`, `package_languages`, `page_frames`, `page_offers`, `page_views`, `parked`, `pass_through`, `permits`, `platform_calls`, `platform_services`, `plugin_agreements`, `plugin_approval`, `plugin_folder`, `plugin_host`, `plugin_manager`, `plugin_state`, `private_fs`, `private_store`, `receive`, `relay_token`, `running`, `runtime`, `service`, `sleep`, `status_service`, `statuses`, `store`, `store_branch`, `store_runtime`, `store_service`, `store_trust`, `surfaces`, `system_readings`, `transfer_events`, `update_install`
- `peersus-plugin-runtime`: `desktop`, `diagnostic`, `socket`
- `peersus-plugin-canvas`: `streams`
- `peersus-ui`: `about`, `accent_picker`, `agreement_flow`, `agreement_overlay`, `appearance`, `avatar`, `avatar_file`, `avatar_view`, `back`, `binds_page`, `bound_keys`, `canvas_view`, `chords`, `clipboard_files`, `contributed_page`, `demo`, `demo_owner`, `devices_page`, `field`, `follow`, `font`, `glyph`, `header`, `headless`, `i18n`, `icon`, `intake`, `interface_size`, `language_picker`, `languages_page`, `layers`, `layout_regression`, `link`, `motion`, `navigation`, `notices`, `offer_window`, `outgoing`, `outside_chords`, `plugin_controls`, `plugin_manager`, `portal`, `preview`, `private_store`, `probe`, `qr`, `qr_reader`, `receive_page`, `relay_choice`, `render_budget`, `renderer_bench`, `renderer_settings`, `replay_fixture`, `scanner`, `send_page`, `settings_index`, `settings_navigation`, `settings_page`, `settings_search`, `shelf`, `sidebar`, `sliding_line`, `startup_row`, `support`, `system_theme`, `text`, `theme`, `top_sheet`, `transfers_page`, `view`, `window_chrome`
- `peersus-plugin-android`: none
- `peersus-ui-android`: `bridge`, `camera_bridge`, `extension_authorization`, `extension_clipboard`, `extension_files`, `extension_services`, `input_clipboard`, `nearby`, `notifications`, `plugins`, `store`, `text_input`, `tls_init`, `update`
<!-- CRATES-END -->

## Command path

```text
Desktop GUI / CLI / Android UI
             |
             v
  core Command + SettingsPatch
             |
             v
  application Runtime (single owner)
       |             |             |
       v             v             v
  network host   plugin broker   persistence
       |             |
       v             v
 authenticated   supervised extension
 peer transport  + declared services
             |
             v
   snapshot and command reply
```

CLI preference flags become the same typed settings patches as other frontends. `cli::Control` exposes state and command submission; local IPC adapts that interface. The GUI is not a second owner of network or plugin policy.

### What belongs in an extension

An extension can **provide** a versioned service as well as consume one. The host admits and routes calls; it does not implement the provider's feature. A recipient gets a broker-stamped caller and a ticket, owns the work for that ticket, and can return a result or publish an event it declared. Another extension can consume that contract with an explicit import and the required grants. A page is one possible client, not the lifetime of the service.

For example, a guest can carry geographic outlines, calculate the projection and decide how dragging or zooming changes its view. The host need only validate and draw the resulting generic elements. Asking the operating system for a device's current position is a different operation and stays behind a host permission. Drawing a map needs neither that permission nor network access.

**Extension drawing isolation.** The extension computes its content and decides how to respond to input; the window paints the resulting primitives and kept layers. It measures synchronous paint time on the UI thread by host-assigned extension ID. Sustained over-budget paint suspends that extension's drawings, which keep their size and show only their background, with an application-owned notice and **Resume** control. A temporary pause expires after 10 seconds; three pauses within two minutes require manual resume. The window holds bounded accounts only for extensions with available pages; switching tabs does not clear a pause. This limits sustained painting, not a single synchronous renderer call or GPU work outside the paint callback.

Keep domain state and transitions in ordinary code in the guest; translate host deliveries and platform capabilities at its boundary. A guest restart loses its in-memory state. Do not persist sensitive observations merely to survive a restart, treat a saved preference as authorization, or replay an external operation whose outcome is unknown. See the [module ABI](api/module-abi.md) for the transport and its limits.

### Work per broker call

A reply to an extension and a change to application state are separate events. Reading configuration, language, status or a transfer list replies only to the caller. Listing extension statuses projects the registered IDs and supervisor statuses, not a copy of every manifest. A successful change to page placement, action availability or status can require publication; restating the same placement or withholding the same action does not. A restart is a separate effect performed after the reply.

This matters even when no window redraws: the owner compares a *completed* snapshot with the previous snapshot, so suppressing an equal snapshot at the last step does not save its construction, peer admission calculations or chooser updates. Broker queues and per-extension rate limits bound pending work; they do not by themselves bound the cost of doing one accepted operation for many callers.

## The owner, its windows, and recovery (desktop)

The owner (`peersus`) holds the network, the saved state and the extensions; a window (`peersus-ui`) draws its state and sends it commands over the local control channel. A window that finds no owner starts one. Every window keeps a lease on the owner for as long as it lives (`peersus-app/src/lifetime.rs`), and an owner started for a window ends when its last window goes unless the person asked for the background.

How an owner ended decides what its windows do, and the owner says which: as it ends because it was told to (`peersus stop`, a signal, its last window going) it writes one byte on every lease (`ipc::TOLD`).

| The owner | The window |
| --- | --- |
| was told to stop | closes with it |
| restarts for an update | waits for the new one and takes the new window's place |
| ended in any other way | starts it again by replacing itself with a new window, at most three times in five minutes |
| keeps ending, or could not be started again | says so, offers Restart, and rejoins an owner that appears |
| could not start (the identity is not to be had, preferences, another reason) | shows what the owner said; never retried without a person, since a retry asks the system for the keyring again |

A window that dies and an owner that no longer answers are not automatically recovered. If an owner dies without a window, the next window can start it; a Linux service owner is supervised as described below. An extension that fails has its own bounded restart budget independent of the owner and other extensions.

### Starting without opening a window on Linux

“Run in the background” controls what happens when the last window closes. “Start when you sign in” controls a separate systemd **user** unit. Enabling it requests future activation, not a second owner immediately. Disabling it prevents future starts without stopping a running transfer. Settings reads the manager's actual state when the row opens and after a change; it keeps no second copy of the switch.

The unit starts the same headless executable with `--service`. Ordinarily the user manager starts at login. If the administrator already enabled lingering, it may start at boot; this is not a guarantee of connectivity before login. The service waits for Secret Service to become available and unlocked and for the existing owner to release its lock. It does not enable lingering, request root, export an identity, or restart a healthy owner when a window appears. A service reported as active may still be waiting rather than connected.

### Native desktop session startup

On macOS 13 or later the signed-in window registers the bundle's optional Aqua LaunchAgent using `SMAppService`. The OS may require approval in Login Items. Its `BundleProgram` is the same owner the window uses; the agent refuses root and disallows Keychain prompts. Registration, permission to launch, a running process and a connected device are four distinct states. Unregistering a running macOS agent can terminate it.

On Windows, `peersus --install-session-template` registers a native `SERVICE_USER_OWN_PROCESS` template from the machine installation under Program Files. This is an administrator operation, does not replace existing services and opts in no accounts. Windows creates a distinct instance at sign-in. An instance starts an owner only when the current account opted in; it uses that account's data directory and Credential Manager. An SCM stop requests ordinary owner shutdown rather than stopping a different window's owner. A console invocation of `--service` cannot impersonate a service. Neither native session service creates a replacement identity.

Neither path provides access to the login screen. A macOS LaunchDaemon is a distinct system registration that requires a notarized bundle and administrator approval; the development bundle is ad hoc. A Windows machine service runs outside the interactive desktop. Running the extension host under either system identity is not a supported substitute for a session owner.

## Extensions and backend independence

A host assigns each extension ID. The manifest declares capabilities, versioned service imports/exports, actions and typed settings. User-facing labels are presentation, not identifiers or authorization. Core behavior must not select a concrete plugin implementation by its display name.

`ModuleFactory` supplies execution launchers at the platform boundary. The desktop implementation resolves trusted helpers beside the application executable, never from a downloaded package or `PATH`. Packages cannot choose their own interpreter executable. Android supplies its own managed service lifecycle instead of assuming desktop subprocess behavior.

Manifest platform compatibility is a declaration, not an OS permission grant or evidence of runtime support. A new backend must implement the actual launch, transport and filesystem contracts and pass behavioral checks; changing a platform list is insufficient.

Native extensions run with the desktop user's or Android application's privileges. A separate process provides crash isolation, **not an adversarial security sandbox**. WASM has interpreter limits, but host services and permission checks remain security-relevant.

### Extension-rendered canvases

The extension computes and renders its own scene. A WASM extension submits a bounded display list to `canvas_present`, which rasterizes it with Skia CPU inside its own helper or Android isolated worker. Native libraries and standalone executables render with their own engine. All three send finished `PSF1` frames; the desktop owner never receives those frames or rasterizes them.

```text
WASM extension + its process: PSD1 list → Skia CPU → PSF1 RGBA
native library or executable: its own renderer → PSF1 RGBA
desktop owner: authenticate the claim → authorize a one-use connection to the mounted window
desktop extension → private local socket → window: validate → one image draw + accessibility overlays
Android extension → in-process owner slot: validate → window shares the latest raster allocation
window input → page events → extension
```

One desktop window at a time holds a canvas lease. Another window waits without getting pixels until the first unmounts. The lease is also revoked when the declaration disappears or the extension stops. Desktop windows enforce frame and byte limits before reading pixels; Android's in-process owner enforces the same limits and shares the raster with the compositor. Both paths reuse at most two CPU buffers per mounted canvas and apply backpressure before reading another raster when both are held. No pixels travel inside snapshots.

The window updates one texture per canvas while its size and graphics context remain unchanged. A window without GPU rendering draws the same raster directly. The main window limits Skia's budgeted GPU resource cache to 64 MiB on Android and desktop; this is not a limit on driver memory or resources in use. Leaving a canvas drops its texture and purges unlocked resources on the window's render pass. Opt-in pan and zoom preview transforms only the last bitmap and its overlays, then retires the steps acknowledged by the new frame. The [canvas contract](api/service-messages.md#extension-rendered-canvases) specifies the wire fields and budgets.

Desktop canvas routing uses the same-user trust boundary of local control IPC. A random, one-use token binds the direct connection to the admitted extension and mounted view. It is not isolation from native code already running with that user's rights.

Declarative `drawing` nodes remain for small graphics such as icons, gauges and sparklines. `peersus.drawings` remains a caller-scoped, bounded cache for reusing strokes. These drawings never use gesture preview; dense and continuously redrawn scenes belong in an extension-rendered canvas.

## Input carried across screen edges

On Linux the owner takes this computer's pointer and keyboard at the edges of the layout (`desktop_capture.rs`, the desktop's InputCapture portal) and forwards them to the device placed there through whichever extension declares an input transport (`input_follow.rs`, which drives the capture by the layout's geometry in `pass_through.rs`; neither of those two needs it); no host code names that extension. The rules that keep a person from being locked out are the host's, not the extension's:

- The pointer comes back when it is pushed back, when the visited device goes offline, sleeps or loses its permission, when a forwarded batch is refused, and when a batch that was accepted fails or runs out of time later. A device whose forward failed cannot take the pointer again for ten seconds. After a failure, input that waited behind it is dropped, never replayed late.
- **Both Control keys held together give the pointer back at once.** The capture session does this itself, with no word from the visited device, its extension or the network.
- On the receiving computer, whatever a controller that has sent nothing for thirty seconds still holds (a button, a key) is let go (`desktop_input.rs`, `HELD_LIMIT`, the same time a silent connection is given).
- A touch is not a crossing. The pointer stopped at an edge is carried over only after it is pushed further against it (`EDGE_PRESSURE`); pulled back, slid along the edge, left still, or followed by a key, a button or the wheel, it is given back where it was stopped. While the push is measured nothing is sent to the other device.
- A scroll is passed on in whole notches: the capture sums what the desktop says during one frame, a wheel's 120ths or a smooth scroll's distance, and keeps what is short of a notch for the next frame of the same scroll.
- A device with a pointer of its own (a pen on the receiving desktop) has no wheel there: its notch goes to the shared mouse, which turns what is under the desktop's own pointer. That pointer is brought under the device's pointer before each notch (`desktop_input.rs`, `Pen::under`).
- The desktop decides when the pointer is stopped. Where something else on the desktop keeps a pointer barrier of its own on the same edge (a shell extension that switches workspaces at the screen edges does), the desktop may stop the pointer at that one and reach the capture's only at the first motion away from the edge. The portal accepts a capture barrier only on a screen edge, so this cannot be worked around here.

## API expansion (TODO)

**Telemetry is never part of the application.** The application has none and no advertising, and that is the privacy promise: nothing leaves the device that the person did not send themselves. Measuring how the application is used, to improve it, is a job for an *extension*, which only works on the data the person chooses to give it and can do nothing without the permissions they grant. Nothing of that extension exists. This item is to make sure the application, which knows no extension by name, already offers what such an extension would need, and nothing more. A first reading of what is missing or unproven:

- **Data classes, one permission each.** An extension that wants to observe the application asks for named, narrow read capabilities (for example transfer counts and sizes, extension health, errors without file names or device names, application and platform version), each described in words for the person who grants it. The services that exist today (`peersus.platform`, `peersus.transfers`, `peersus.status`, `peersus.plugins`) say what is already readable; the gaps are to be listed against that. A person can grant some and refuse others, and the application must work whichever they choose. This is the property that makes the design unlike a general consent switch: the choice is per kind of datum, never all or nothing.
- **Where the device is** is its own class, `location.read`, granted outright to one extension: the application asks the system once per call (the desktop's location portal on Linux, which follows the person's own location setting, the location manager on Android, nothing elsewhere) and keeps nothing. The system may look the position up with an online service of its own; the application sends nothing for it, and what the extension does with the position (sending it to a paired device) needs that device's own permission.
- **Never by default, never by name.** Names of devices, file names and paths, addresses, keys and the contents of anything are never offered as a class a telemetry extension can ask for; the application has no "everything" permission to reach them with.
- **What leaves is what was shown.** An extension that sends data out needs a network permission of its own (today an extension reaches the network only through host services that are guarded); the open question is whether the person is shown the exact payload, or a fixed schema of it, before it is sent, and can see a history of what was sent. The agreement every extension must carry (see "Trust and installation") already requires it to say what it does and how things change; a telemetry extension would also have to say where the data goes and how long it is kept.
- **Revocation and deletion.** Taking a permission away stops that data at once; removing the extension removes its record of acceptance, so installing it again asks again.
- **Store declarations follow what the extension does, not the application.** Google Play's Data safety form and Apple's privacy label describe the data the application collects; an optional extension that is not part of the store build does not change them, and one that ships inside a store build does. Keep telemetry out of store builds unless the declarations are made first.
- **To check before building one:** which events the owner can already publish to an extension without a name in them; whether per-class grants exist for the events in `peersus.transfers` and `peersus.status`; the network egress path an extension has today and what stops it sending more than it was granted; and how a person sees, exports and deletes what an extension holds.

## Transfers and relay configuration

The authenticated QUIC transport establishes peer identity. Pairing and transfer consent are separate decisions. The receive path validates portable names and sizes, uses private staging and verifies the digest before publishing without overwriting an existing destination. Reconnecting is not file-transfer resume.

A files code and a paired device are two ways to send the same selection, and only one is used. While a code nobody has scanned is shown, choosing a device asks the owner to take the code back (`withdraw-files-invitation`, by the code's generation); the owner refuses if the code was scanned or is no longer the newest, and then the code stays. Once taken back, the code admits nobody and the files it held are staged again for the hold-to-send circle. A selection too large to be held twice (more than the 64 files the window may keep open at once) cannot be staged again and the code stays.

Accepting an offer takes the window to Receive and scrolls to the list of what is being received: the rule (`view::accepted_between`) compares two consecutive snapshots and fires only for a file that waited and now moves or has arrived, so a refusal, a timeout or history loaded at start moves nothing. An offer is also announced while no window is in front, at the edge of the screen the person chose (`settings.notice_place`): how each platform does it, and where it does not, is in [local control](api/local-control.md#an-offer-announced-outside-the-window). The owner decides when; the surface that draws it only reads state and submits the ordinary accept and cancel commands.

How a transfer that did not complete is recorded tells who ended it. An offer nobody accepted (refused, cancelled by either person, or run out) is `cancelled`. A file that was moving and that the other device let go of is `failed` with "the other device stopped the transfer"; only a connection that itself broke is a network failure. None of this is a message of its own: a device that lets go of a transfer ends its streams, and the other reads that while the connection stands.

Completed pairings have no expiry: restarting, sleeping or losing a route does not revoke them. Invitation and consent deadlines apply only while establishing a pairing. On desktop, an absent credential lookup is not first use when the saved peer store contains pairings; startup refuses to generate a replacement identity, leaving the credential and pairing records untouched. Invalid pairing state also cannot authorize identity creation. This safeguard prevents replacement after lookup failure; it does not recover an already-mismatched identity.

Behind one switch that is off by default, on Linux and Android, Bluetooth Low Energy announcements are used for three things and for nothing else ([devices nearby](nearby-send.md), experimental): a paired device held right next to this one is *proposed* on Send; devices nearby that also have a window open are *listed* on Peers to pair with; and files listed on Send are *offered* to a device that is not paired when it is brought near, each file being its person's to accept. No Bluetooth connection is made and nothing is paired at the Bluetooth level. The rules (what is announced, what an announcement that was heard means, when a device counts as near, and the threshold a person's measurement gives) are pure functions in `peersus-core::nearby`; `peersus-app::nearby` joins them to a platform adapter that holds no rule (`nearby::linux` for BlueZ, `NearbyRadio` on Android) and tells the owner what was found with commands that no window, guest or socket client can send. A proposal changes how a destination is chosen and never sends; a pairing begun from the list uses a pairing invitation with a gate of its own (`Invitations::create_nearby`) and is followed by the same ceremony as a code; files for a device that is not paired go through a second gate made from the same invitation (`Purpose::NearbyFiles`), which admits file offers and nothing else and pairs nobody, and the owner refuses them for a device it does not hold as near. An invitation announced this way is read by every radio in range, which a code on a screen is not, and a device counted as near is whichever was heard strongest, which nothing verifies: what both mean is said in that document.

On Linux, an already-paired desktop profile resolves Secret Service access before loading its identity. Locked identity items and collections are unlocked through the provider's native prompt; the application never asks for or handles the keyring password. Collections must be checked because some providers hide locked entries from search. Cancelling, refusing or timing out leaves credentials and pairings unchanged and is reported as an identity startup failure, not peer presence. A provider that fails to show its prompt is reported as exactly that, distinct from a keyring that cannot be reached: the application cannot make it show one, and the collection has to be unlocked by the desktop's own means. More than one record matching the identity is refused, never chosen between. First use does not run this paired-profile recovery. The prompt is bounded to 60 seconds, and the desktop startup wait allows it to finish.

Direct IP paths always take priority. New installations enable the project relay and public n0 pool in Automatic mode: the transport measures relay latency and selects a live candidate, using hysteresis to avoid oscillating over small differences. The shared desktop/Android Network editor can disable, re-enable and reorder candidates without losing their saved addresses or positions. Priority and Sequential instead configure one enabled fallback tier at a time and advance after its failure deadline or immediately on authentication refusal; Priority also retries the preferred tier while application traffic is idle, whereas Sequential retains a working fallback. Advanced options retain the direct-path, failover and preferred-tier retry deadlines and separate custom-relay credential controls. Existing relay-only settings migrate without silently enabling additional services.

Relay signalling may already participate in NAT traversal before a direct path is established. After the handshake, application streams wait a bounded opportunity for a selected direct path; if that opportunity expires, end-to-end encrypted application traffic may use the relay. Relays are therefore **not connection-establishment-only**: they cannot read file contents, but can observe connection metadata. Turning relays off does not prevent direct connections between networks that permit them. The application does not send telemetry.

`RelayPolicy` and its timing bounds live in `peersus-core/src/relay.rs`; the owner-supervised coordinator lives in `peersus-net/src/relay_policy.rs`. The policy orders this device's home-relay services, not a firewall allowlist for the peer's discovered relay addresses. A custom relay token is not a field in serialized settings or snapshots and is not accepted as a CLI flag. Desktop storage uses the credential store; Android uses a private token file. It is attached only to the custom target, never to the project relay or public pool. See [local settings control](api/local-control.md#relay-policy-settings) for the serialized contract.

## Trust and installation

Manual and store installation share the package installer. Store installation additionally checks the maintainer signature, catalog rollback floor, archive digest and size, and agreement between catalog, manifest and embedded program metadata. A trusted signature attests approval of bytes, not harmlessness of code. What a package under a name means for what the person allowed is one rule, `peersus_core::plugins::Installation`: an **install** (nothing under the name, including after a removal) starts without grants; a **store update** (a strictly newer version from the signed store, over a sound package that was itself installed from the store, recorded by the owner in its folder as `from-store`) keeps the grants the new version still asks for and forgets the rest, and anything newly asked starts off; every other **replacement** (from outside the store, over an outside-store or damaged package, or not newer) resets them. The id alone never carries consent: only the store vouches that two versions are the same extension.

**Licence and notice.** Every extension carries an agreement in its manifest (`agreements`, contract in `peersus-api::agreement`): at least one licence page and one notice page, each saying what it is and what changes for the person, in plain words, with plain content blocks and `https` links. A package without one is refused at staging, at confirmation and by the installer itself, so no install path (window, store, command line) skips it. The application, never the extension, draws the one standard overlay: the panel's size, `Cancel` at the far left, `Accept TOS` at the right of the last page, and the order of the pages are fixed in `peersus-ui::agreement_flow`, and nothing in an agreement can size, place, press or hide them. Installing is one button: it opens the overlay, and the package installs when the agreement is accepted; the owner installs only if the digest it is given is the agreement of the very package it holds. The acceptance is per user (`agreements.json` in the person's data directory), outlives updates that change no word of the agreement, is asked again when any word changes, and goes when the extension is removed. An extension learns one thing of it: `extension.tos.accepted`, a boolean (`peersus.extension`, method `tos`). A package from outside the store has one step more, before its pages: the application's own warning, which must be scrolled to its end and then held, and which the owner requires (`risk_acknowledged`) before it installs such a package.

The production store is disabled while no maintainer public key is configured. Desktop downloads use a dedicated helper; Android uses platform HTTPS in a private, non-exported `:store` service and shares the catalog verifier and installer. The Android service has the same UID as the application: process separation is not an adversarial sandbox. Local tests do not establish production-store deployment or complete phone UX verification.

## Reading and validating changes

Start from `crates/peersus-core/src/model.rs` for commands and snapshots, `crates/peersus-app/src/runtime.rs` for execution, and `crates/peersus-api/src/manifest.rs` for extension declarations. Follow the platform adapter only after understanding the common policy path.

`cargo xtask gate` is the shared local/CI entrypoint. Desktop builds use `cargo xtask build`; Android release preparation uses `cargo xtask android`. These commands build or test artifacts: neither proves visual correctness, device lifecycle behavior or successful publication. See the dated [readiness evidence](release-readiness.md) for those distinctions.
