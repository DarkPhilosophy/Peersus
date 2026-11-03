# Extension manifests

Authoritative definitions: `peersus-plugin-api/src/manifest.rs`, `contributions.rs`, `platform.rs` and `package.rs`. The portable manifest version and module execution ABI are both currently 1, but they are independent versioned contracts.

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

## Execution

The `execution.kind` values use **snake_case**, unlike the kebab-case service-message variants:

```json
{"kind":"native_library","abi":1}
```

```json
{"kind":"wasm","abi":1}
```

Omitting execution selects the legacy process executor. Store approval deliberately refuses those legacy process packages; manual installation compatibility is not store eligibility. Native libraries are loaded in a separate helper, not in the application owner or UI. File type alone must not change the permission or service contract.

## File ownership

A program name is one plain ASCII filename, up to 64 bytes, not a path, hidden filename, option or manifest filename. The host still validates its existence, ownership and execution suitability.

When `files` is present, it lists additional installed files; the manifest and program are already included. The maximum list length is 512. Paths must remain inside the package; traversal, backslashes, control characters and ambiguous spellings are refused. A package must not contain two competing manifest files. Without the optional list, legacy package installation takes the whole package subject to installer checks. Store validation has additional stricter packaging rules.

## Package metadata

Human metadata lives in a separate `package` block in the manifest file. The parser removes and validates it before deserializing the capability/service manifest. It cannot confer privileges or change participant identity.

Package versions are three unsigned decimal components (`major.minor.patch`), without leading zeroes, prerelease or build suffixes. A newer version compares those numeric components. Names are limited to 80 characters, descriptions to 1000 and changelog text to 4000. An optional public link must be plain HTTPS without credentials, query or fragment. Store packages additionally require author/link metadata and an embedded identity record that matches the manifest and signed catalog.

Use the real bundled manifests as complete examples. A partial JSON excerpt in this reference is not an installable manifest. Follow [the store contract](../extension-store.md) for archive and provenance requirements, and [service messages](service-messages.md) for runtime operations.
