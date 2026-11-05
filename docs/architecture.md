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
| `peersus-clipboard` | Included clipboard extension | Concrete functionality stays outside the core |
| `peersus-upload` | Explicit cloud upload extension | Upload is not device-to-device end-to-end encryption |
| `peersus-store-fetch` | Desktop catalog/archive download helper | Downloads bytes; the application verifies trust and installs |
| `peersus-ui` | Shared Freya interface and command adapter | Displays application state and submits commands |
| `peersus-ui-android` | Android entry, JNI and platform adapters | Owns Android-specific integration, not domain rules |
| `xtask` | Quality gates, builds, native helpers and catalog tooling | Local build preparation is not publication |

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

## Extensions and backend independence

A host assigns each extension ID. The manifest declares capabilities, versioned service imports/exports, actions and typed settings. User-facing labels are presentation, not identifiers or authorization. Core behavior must not select a concrete plugin implementation by its display name.

`ModuleFactory` supplies execution launchers at the platform boundary. The desktop implementation resolves trusted helpers beside the application executable, never from a downloaded package or `PATH`. Packages cannot choose their own interpreter executable. Android supplies its own managed service lifecycle instead of assuming desktop subprocess behavior.

Manifest platform compatibility is a declaration, not an OS permission grant or evidence of runtime support. A new backend must implement the actual launch, transport and filesystem contracts and pass behavioral checks; changing a platform list is insufficient.

Native extensions run with the desktop user's or Android application's privileges. A separate process provides crash isolation, **not an adversarial security sandbox**. WASM has interpreter limits, but host services and permission checks remain security-relevant.

## Transfers and relay configuration

The authenticated QUIC transport establishes peer identity. Pairing and transfer consent are separate decisions. The receive path validates portable names and sizes, uses private staging and verifies the digest before publishing without overwriting an existing destination. Reconnecting is not file-transfer resume.

Direct IP paths always take priority. New installations enable the project relay followed by the public n0 pool; the shared desktop/Android Network editor can disable, re-enable and reorder fallbacks without losing their saved addresses or positions, and edit project or custom relay addresses. Advanced options retain the direct-path, failover and preferred-tier retry deadlines and the separate custom-relay credential controls. Only one enabled fallback tier is configured as the home relay at a time, so a latency probe cannot silently reorder the user's list. The owner skips disabled tiers, advances an unavailable tier after its configured deadline, advances an explicit authentication refusal immediately, and only retries the preferred enabled tier while application traffic is idle. Existing relay-only settings migrate without silently enabling additional services.

Relay signalling may already participate in NAT traversal before a direct path is established. After the handshake, application streams wait a bounded opportunity for a selected direct path; if that opportunity expires, end-to-end encrypted application traffic may use the relay. Relays are therefore **not connection-establishment-only**: they cannot read file contents, but can observe connection metadata. Turning relays off does not prevent direct connections between networks that permit them. The application does not send telemetry.

`RelayPolicy` and its timing bounds live in `peersus-core/src/relay.rs`; the owner-supervised coordinator lives in `peersus-net/src/relay_policy.rs`. The policy orders this device's home-relay services, not a firewall allowlist for the peer's discovered relay addresses. A custom relay token is not a field in serialized settings or snapshots and is not accepted as a CLI flag. Desktop storage uses the credential store; Android uses a private token file. It is attached only to the custom target, never to the project relay or public pool. See [local settings control](api/local-control.md#relay-policy-settings) for the serialized contract.

## Trust and installation

Manual and store installation share the package installer. Store installation additionally checks the maintainer signature, catalog rollback floor, archive digest and size, and agreement between catalog, manifest and embedded program metadata. A trusted signature attests approval of bytes, not harmlessness of code. New installations start without grants; replacement resets previous permissions.

The production store is disabled while no maintainer public key is configured. Desktop downloads use a dedicated helper; Android uses platform HTTPS in a private, non-exported `:store` service and shares the catalog verifier and installer. The Android service has the same UID as the application: process separation is not an adversarial sandbox. Local tests do not establish production-store deployment or complete phone UX verification.

## Reading and validating changes

Start from `crates/peersus-core/src/model.rs` for commands and snapshots, `crates/peersus-app/src/runtime.rs` for execution, and `crates/peersus-api/src/manifest.rs` for extension declarations. Follow the platform adapter only after understanding the common policy path.

`cargo xtask gate` is the shared local/CI entrypoint. Desktop builds use `cargo xtask build`; Android release preparation uses `cargo xtask android`. These commands build or test artifacts: neither proves visual correctness, device lifecycle behavior or successful publication. See the dated [readiness evidence](release-readiness.md) for those distinctions.
