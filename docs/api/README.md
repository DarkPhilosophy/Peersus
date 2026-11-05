# API reference

These are development contracts, not a stable external SDK promise. The Rust types and executable tests are authoritative. A plugin's service version, its execution ABI and the local application's IPC protocol are separate boundaries.

| Interface | Reference | Source |
| --- | --- | --- |
| Local application IPC | [Local control](local-control.md) | `peersus-app/src/ipc.rs`, `peersus-core/src/model.rs` |
| Plugin service calls and events | [Service messages](service-messages.md) | `peersus-api/src/message.rs`, `peersus-plugin/src/hub.rs` |
| Extension manifest and contributions | [Manifests](manifests.md) | `peersus-api/src/manifest.rs`, `contributions.rs`, `elements.rs` |
| Module host ABI | [Native/WASM ABI](module-abi.md) | `peersus-plugin-runtime/sdk/peersus_plugin.h` |
| Approved extension distribution | [Extension store](../extension-store.md) | `peersus-app/src/catalog.rs`, `store_service.rs` |

## Generate Rust API documentation

From the workspace, with the declared toolchain and platform build prerequisites installed:

```sh
cargo doc --locked --workspace --exclude peersus-ui-android --no-deps
```

The Android entry requires the Android target, NDK and renderer prerequisites; it is not a desktop rustdoc target. Excluding it does not assert Android API validation. Dependencies may need to be fetched before an offline build.

## Security boundaries

- Local application control is same-user control, not a remote HTTP API.
- Plugin identity comes from its authenticated channel; a message cannot name its own sender.
- Manifest declarations do not grant access. The application checks user grants and target devices separately.
- Credentials are not API documentation examples. Never include real bootstrap tokens, relay tokens or pairing invitations in logs or examples.
- A lost command reply is an unknown outcome. Do not automatically retry a command with side effects.
