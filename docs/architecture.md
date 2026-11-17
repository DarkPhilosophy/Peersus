# Architecture and ownership

## Workspace map

| Component | Responsibility | Boundary |
| --- | --- | --- |
| `peersus-core` | Commands, snapshots, settings patches, pairing rules and per-device plugin grants | No GUI toolkit or concrete extension implementation |
| `peersus-net` | Authenticated transport, invitations, reconnect, file transfer and local persistence | Owned by the application; independent of the renderer |
| `peersus-app` | Single application owner, command execution, local IPC, plugin lifecycle, installation and store verification | Coordinates policy; frontends do not bypass it |
| `peersus-api` | The contract plugins and the application share: identifiers, manifests, contributions, page elements, permission declarations and service messages | Plain data and validation; no hub, process, OS handle or GUI toolkit |
| `peersus-plugin` | Runs plugins for the application: the hub that routes calls and events, process supervision, control/data framing and platform-provided launchers | Speaks only the `peersus-api` contract; execution is separate from manifest policy |
| `peersus-plugin-runtime` | WASM interpreter and native module helper; C SDK | Helpers execute payloads outside the desktop owner process |
| `peersus-plugin-android` | Android extension worker entry | Android lifecycle and Binder integration are platform concerns |
| `peersus-clipboard-platform` | System clipboard owner and change observation | Shared by native extensions and the application's clipboard service; itself not an extension |
| `peersus-diagnostics` | Structured diagnostics shared by the desktop and Android processes | Each process initializes it; fields are made safe where they are emitted |
| extensions (not in this workspace) | Concrete functionality: clipboard sharing, cloud upload, remote control | Submitted on `submission/<id>-<date>` branches under `extension/<name>/` against the `extension` base branch and installed through the store; `main` builds and ships none. `plugins/` is an ignored local working copy |
| `peersus-store-fetch` | Desktop catalog/archive download helper | Downloads bytes; the application verifies trust and installs |
| `peersus-ui` | Shared Freya interface and command adapter | Displays application state and submits commands |
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
| [`xtask`](../xtask) | Peersus build and quality gate tooling: checks, builds, catalog packaging | `peersus-api` |
| [`peersus-net`](../crates/peersus-net) | Peersus identity, encrypted transport, mutual pairing, remembered peers and file transfer | `peersus-api`, `peersus-core` |
| [`peersus-plugin`](../crates/peersus-plugin) | Runs Peersus plugins for the application: the hub that routes their calls and each plugin's isolated process | `peersus-api`, `peersus-diagnostics` |
| [`peersus-app`](../crates/peersus-app) | Peersus application owner: runtime, plugin host, command line and local control | `peersus-api`, `peersus-clipboard-platform`, `peersus-core`, `peersus-diagnostics`, `peersus-net`, `peersus-plugin` |
| [`peersus-plugin-runtime`](../crates/peersus-plugin-runtime) | Bounded interpreter and transport for separately supervised plugin modules | `peersus-diagnostics`, `peersus-plugin` |
| [`peersus-plugin-android`](../crates/peersus-plugin-android) | JNI descriptor adapter for the separately managed Android WASM worker. | `peersus-diagnostics`, `peersus-plugin-runtime` |
| [`peersus-ui`](../crates/peersus-ui) | Peersus interface: renders the owner's Snapshot and sends it Commands. No business logic. | `peersus-api`, `peersus-app`, `peersus-core`, `peersus-diagnostics`, `peersus-net`, `peersus-plugin` |
| [`peersus-ui-android`](../crates/peersus-ui-android) | Peersus on Android: the entry the system calls, and the owner started inside the application. The interface is peersus-ui. | `peersus-api`, `peersus-app`, `peersus-core`, `peersus-diagnostics`, `peersus-net`, `peersus-plugin`, `peersus-ui` |

Modules directly under each crate's `src/`:

- `peersus-api`: `agreement`, `chord`, `contributions`, `elements`, `error`, `language`, `manifest`, `message`, `name`, `notification`, `package`, `permission`, `platform`
- `peersus-clipboard-platform`: `desktop`, `owner`, `watch`
- `peersus-store-fetch`: none
- `peersus-core`: `address`, `agreements`, `bindings`, `config`, `layout`, `model`, `nearby`, `pairing`, `peer`, `plugins`, `private_store`, `profile`, `relay`, `secret`
- `peersus-diagnostics`: none
- `xtask`: `bench`, `catalog`, `languages`, `manifest`, `update`
- `peersus-net`: `admission`, `announce`, `atomic`, `ceremony`, `file_session`, `folder_picked`, `folder_sender`, `grants`, `handshake`, `held`, `history`, `invitation`, `invitations`, `key_file`, `network_host`, `pairing_tasks`, `peers`, `picked`, `picture_owners`, `pictures`, `profile_exchange`, `reconnect`, `relay_policy`, `relay_probe`, `settings`, `streams`, `token_file`, `transfer`, `transfer_tasks`, `transport`, `walk`
- `peersus-plugin`: `data`, `frame`, `hub`, `link`, `module`, `pump`, `service`, `supervisor`, `transport`
- `peersus-app`: `aliases`, `app_update`, `archive`, `binary_metadata`, `board`, `catalog`, `cli`, `clipboard_handles`, `desktop_capture`, `desktop_clipboard`, `desktop_input`, `desktop_services`, `discovery`, `display_service`, `displays`, `extension_prompts`, `identity`, `identity_unlock`, `in_flight`, `input_follow`, `instance`, `ipc`, `languages`, `lifetime`, `local_pipe`, `nearby`, `notifications`, `offer_panel`, `package_languages`, `page_offers`, `page_views`, `parked`, `pass_through`, `platform_calls`, `platform_services`, `plugin_agreements`, `plugin_approval`, `plugin_folder`, `plugin_host`, `plugin_manager`, `plugin_state`, `private_fs`, `private_store`, `receive`, `relay_token`, `running`, `runtime`, `sleep`, `status_service`, `statuses`, `store`, `store_branch`, `store_runtime`, `store_service`, `store_trust`, `surfaces`, `system_readings`, `transfer_events`, `update_install`
- `peersus-plugin-runtime`: `diagnostic`, `socket`
- `peersus-plugin-android`: none
- `peersus-ui`: `about`, `accent_picker`, `agreement_flow`, `agreement_overlay`, `appearance`, `avatar`, `avatar_file`, `avatar_view`, `back`, `binds_page`, `bound_keys`, `chords`, `clipboard_files`, `contributed_page`, `demo`, `demo_owner`, `devices_page`, `field`, `font`, `glyph`, `header`, `headless`, `i18n`, `icon`, `intake`, `interface_size`, `language_picker`, `languages_page`, `layout_regression`, `link`, `motion`, `navigation`, `notices`, `offer_window`, `outgoing`, `outside_chords`, `plugin_controls`, `plugin_manager`, `portal`, `preview`, `private_store`, `probe`, `qr`, `qr_reader`, `receive_page`, `relay_choice`, `scanner`, `send_page`, `settings_index`, `settings_navigation`, `settings_page`, `settings_search`, `shelf`, `sidebar`, `sliding_line`, `support`, `system_theme`, `text`, `theme`, `top_sheet`, `transfers_page`, `view`, `window_chrome`
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

Not recovered by anything today: an owner that dies while no window is open (the next window starts it), a window that dies, and an owner that runs but does not answer. An extension that fails is restarted by its supervisor a bounded number of times and then left stopped with a Restart button for the person; it never takes the owner or another extension with it.

## Extensions and backend independence

A host assigns each extension ID. The manifest declares capabilities, versioned service imports/exports, actions and typed settings. User-facing labels are presentation, not identifiers or authorization. Core behavior must not select a concrete plugin implementation by its display name.

`ModuleFactory` supplies execution launchers at the platform boundary. The desktop implementation resolves trusted helpers beside the application executable, never from a downloaded package or `PATH`. Packages cannot choose their own interpreter executable. Android supplies its own managed service lifecycle instead of assuming desktop subprocess behavior.

Manifest platform compatibility is a declaration, not an OS permission grant or evidence of runtime support. A new backend must implement the actual launch, transport and filesystem contracts and pass behavioral checks; changing a platform list is insufficient.

Native extensions run with the desktop user's or Android application's privileges. A separate process provides crash isolation, **not an adversarial security sandbox**. WASM has interpreter limits, but host services and permission checks remain security-relevant.

## Input carried across screen edges

On Linux the owner takes this computer's pointer and keyboard at the edges of the layout (`desktop_capture.rs`, the desktop's InputCapture portal) and forwards them to the device placed there through whichever extension declares an input transport (`input_follow.rs`, which drives the capture by the layout's geometry in `pass_through.rs`; neither of those two needs it); no host code names that extension. The rules that keep a person from being locked out are the host's, not the extension's:

- The pointer comes back when it is pushed back, when the visited device goes offline, sleeps or loses its permission, when a forwarded batch is refused, and when a batch that was accepted fails or runs out of time later. A device whose forward failed cannot take the pointer again for ten seconds. After a failure, input that waited behind it is dropped, never replayed late.
- **Both Control keys held together give the pointer back at once.** The capture session does this itself, with no word from the visited device, its extension or the network.
- On the receiving computer, whatever a controller that has sent nothing for thirty seconds still holds (a button, a key) is let go (`desktop_input.rs`, `HELD_LIMIT`, the same time a silent connection is given).

## API expansion (TODO)

**Telemetry is never part of the application.** The application has none and no advertising, and that is the privacy promise: nothing leaves the device that the person did not send themselves. Measuring how the application is used, to improve it, is a job for an *extension*, which only works on the data the person chooses to give it and can do nothing without the permissions they grant. Nothing of that extension exists. This item is to make sure the application, which knows no extension by name, already offers what such an extension would need, and nothing more. A first reading of what is missing or unproven:

- **Data classes, one permission each.** An extension that wants to observe the application asks for named, narrow read capabilities (for example transfer counts and sizes, extension health, errors without file names or device names, application and platform version), each described in words for the person who grants it. The services that exist today (`peersus.platform`, `peersus.transfers`, `peersus.status`, `peersus.plugins`) say what is already readable; the gaps are to be listed against that. A person can grant some and refuse others, and the application must work whichever they choose. This is the property that makes the design unlike a general consent switch: the choice is per kind of datum, never all or nothing.
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

Manual and store installation share the package installer. Store installation additionally checks the maintainer signature, catalog rollback floor, archive digest and size, and agreement between catalog, manifest and embedded program metadata. A trusted signature attests approval of bytes, not harmlessness of code. New installations start without grants; replacement resets previous permissions.

**Licence and notice.** Every extension carries an agreement in its manifest (`agreements`, contract in `peersus-api::agreement`): at least one licence page and one notice page, each saying what it is and what changes for the person, in plain words, with plain content blocks and `https` links. A package without one is refused at staging, at confirmation and by the installer itself, so no install path (window, store, command line) skips it. The application, never the extension, draws the one standard overlay: the panel's size, `Cancel` at the far left, `Accept TOS` at the right of the last page, and the order of the pages are fixed in `peersus-ui::agreement_flow`, and nothing in an agreement can size, place, press or hide them. Installing is one button: it opens the overlay, and the package installs when the agreement is accepted; the owner installs only if the digest it is given is the agreement of the very package it holds. The acceptance is per user (`agreements.json` in the person's data directory), outlives updates that change no word of the agreement, is asked again when any word changes, and goes when the extension is removed. An extension learns one thing of it: `extension.tos.accepted`, a boolean (`peersus.extension`, method `tos`). A package from outside the store has one step more, before its pages: the application's own warning, which must be scrolled to its end and then held, and which the owner requires (`risk_acknowledged`) before it installs such a package.

The production store is disabled while no maintainer public key is configured. Desktop downloads use a dedicated helper; Android uses platform HTTPS in a private, non-exported `:store` service and shares the catalog verifier and installer. The Android service has the same UID as the application: process separation is not an adversarial sandbox. Local tests do not establish production-store deployment or complete phone UX verification.

## Reading and validating changes

Start from `crates/peersus-core/src/model.rs` for commands and snapshots, `crates/peersus-app/src/runtime.rs` for execution, and `crates/peersus-api/src/manifest.rs` for extension declarations. Follow the platform adapter only after understanding the common policy path.

`cargo xtask gate` is the shared local/CI entrypoint. Desktop builds use `cargo xtask build`; Android release preparation uses `cargo xtask android`. These commands build or test artifacts: neither proves visual correctness, device lifecycle behavior or successful publication. See the dated [readiness evidence](release-readiness.md) for those distinctions.
