# Extension manifests

Authoritative definitions: `peersus-api/src/manifest.rs`, `contributions.rs`, `platform.rs` and `package.rs`. The portable manifest version and module execution ABI are both currently 1, but they are independent versioned contracts.

## Identity and declarations

A plugin does not choose its broker identity in the manifest. The host assigns it from the installed package/channel. Imports select a **named provider**, service and exact service version; another plugin exporting the same service cannot substitute for that provider.

The manifest describes:

| Field | Role |
| --- | --- |
| `manifest_version` | Portable schema version |
| `capabilities` | Capability names mapped to meaningful human descriptions |
| `imports` | Provider/service/version plus requested method/event names |
| `exports` | Service/version/description, consumers and method/event declarations |
| `contributions` | Declarative settings and actions for generic frontends |
| `files` | Optional explicit package file list |
| `program` | Optional program filename; defaults to `plugin` |
| `platforms` | Optional OS compatibility declaration; explicit empty lists are invalid |
| `execution` | Executor kind and, for modules, ABI version |

Unknown fields are rejected by the typed manifest contract. Capability, export and member descriptions are bounded to 280 characters and must be meaningful nonblank text. `MAX_PARTICIPANTS`, `MAX_MEMBERS` and the validation routines define additional admission bounds; do not assume a deserialized manifest is already valid.

An export's consumer list and the user's grants are separate checks. An export with no consumers is private. Declaring an import or a capability never grants it. A platform declaration describes compatibility, not access to OS resources.

## Contextual actions

An action may declare `"input":"selected_file"`; omission retains the existing no-input behavior. Such actions must not also set `needs_device`. On Android, generic Send controls expose them only for exactly one staged file and pass a host-issued context in the invocation's `args.context`. The extension redeems it through `peersus.files.selected`; it receives neither a filesystem path nor a raw descriptor. Selected-file actions do not appear among global Settings actions. Separate no-input actions can expose configuration there.

## Execution

The `execution.kind` values use **snake_case**, unlike the kebab-case service-message variants:

```json
{"kind":"native_library","abi":1}
```

```json
{"kind":"wasm","abi":1}
```

### Support tiers

| Tier | Kind | Status | What is guaranteed |
|---|---|---|---|
| 1 | `wasm` | **Official.** Every extension must ship and test it. The store refuses a submission without WASM builds for Linux, Android and macOS. | Same module on every platform; isolated interpreter with no files, network, processes or OS calls of its own; only the brokered services the user granted; bounded memory and fuel; a crash ends only that worker. |
| 2 | `native_library` | Supported, **not recommended**. Optional extra artifact, tested separately; never replaces the WASM build. | Crash isolation only: it is loaded by a separate helper, but it runs machine code with the application user's OS privileges. One ELF build per OS and architecture. |
| 3 | `process` | Limited support, **not recommended, no guarantees**. Optional extra artifact. | Only that the host starts, supervises and stops it. It is an arbitrary program with the application user's privileges; behaviour, portability and safety are its author's responsibility. Linux/Android ELF only. |

Why WASM: one portable module is the same bytes on Linux, Android and macOS, so behaviour does not drift between per-OS native builds, and the host can admit it on a new platform without a new compiler toolchain. Its only reach outside itself is the brokered service contract, so a capability the user did not grant does not exist for it. A native library or process can read files, open sockets and call the OS directly, whatever its manifest says; the host can only crash-isolate it, not confine it.

Platform scope: WASM extensions run on Linux and Android. macOS receives the same packages with **limited** support: the module runs, but host services implemented only for Linux (`peersus.input`, `peersus.displays`) report that they are unsupported there. Windows has no extension host yet: the application builds and runs, but installing or running any extension is refused explicitly.

Omitting execution preserves the legacy process executor for installed/manual packages. New Store submissions select WASM, optionally plus native-library or process artifacts. Process admission requires a verifiable executable, matching embedded package identity, signed archive bytes and an independently approved source build. File type alone must not change the permission or service contract.

## File ownership

A program name is one plain ASCII filename, up to 64 bytes, not a path, hidden filename, option or manifest filename. The host still validates its existence, ownership and execution suitability.

When `files` is present, it lists additional installed files; the manifest and program are already included. The maximum list length is 512. Paths must remain inside the package; traversal, backslashes, control characters and ambiguous spellings are refused. A package must not contain two competing manifest files. Without the optional list, legacy package installation takes the whole package subject to installer checks. Store validation has additional stricter packaging rules.

## Package metadata

Human metadata lives in a separate `package` block in the manifest file. The parser removes and validates it before deserializing the capability/service manifest. It cannot confer privileges or change participant identity.

Package versions are three unsigned decimal components (`major.minor.patch`), without leading zeroes, prerelease or build suffixes. A newer version compares those numeric components. Names are limited to 80 characters, descriptions to 1000 and changelog text to 4000. An optional public link must be plain HTTPS without credentials, query or fragment. Store packages additionally require author/link metadata and an embedded identity record that matches the manifest and signed catalog.

Use the real bundled manifests as complete examples. A partial JSON excerpt in this reference is not an installable manifest. Follow [the store contract](../extension-store.md) for archive and provenance requirements, and [service messages](service-messages.md) for runtime operations.
