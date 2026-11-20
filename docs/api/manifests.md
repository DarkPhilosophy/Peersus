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
| `execution` | Required executor kind and, for modules, ABI version |

Unknown fields are rejected by the typed manifest contract. Capability, export and member descriptions are bounded to 280 characters and must be meaningful nonblank text. `MAX_PARTICIPANTS`, `MAX_MEMBERS` and the validation routines define additional admission bounds; do not assume a deserialized manifest is already valid.

An export's consumer list and the user's grants are separate checks. An export with no consumers is private. Declaring an import or a capability never grants it. A platform declaration describes compatibility, not access to OS resources.

## Public HTTPS settings

A single global toggle with `default: false` may declare `"public_read":{"origin":"https://example.org","root":"/public"}`. The extension imports `peersus.http` version 1 and restores the exact scope with `restore-read`. The host shows the destination and disclosure alongside the switch, checks the current agreement, and saves approval only when the person enables the switch. The effective setting is off without approval, including after removal or permission revocation. Older hosts refuse this manifest field rather than silently enabling access.

## Contextual actions

An action may declare `"input":"selected_file"`; omission retains the existing no-input behavior. Such actions must not also set `needs_device`. On Android, generic Send controls expose them only for exactly one staged file and pass a host-issued context in the invocation's `args.context`. The extension redeems it through `peersus.files.selected`; it receives neither a filesystem path nor a raw descriptor. Selected-file actions do not appear among global Settings actions. Separate no-input actions can expose configuration there.

## Suggested chords

An action may carry `"chord"`, the keys its extension suggests for it, as one text: the modifiers `ctrl`, `alt`, `shift`, `super` in that order, each at most once, then one key, joined by `+` and in lower case (`"ctrl+alt+1"`). The key is a letter `a`–`z`, a digit `0`–`9`, `f1`–`f12`, or one of `enter`, `space`, `tab`, `escape`, `up`, `down`, `left`, `right`, `home`, `end`, `delete`. A chord holds `ctrl`, `alt` or `super`, so it is never something typed; any other spelling is refused with the manifest. Only an action a key can run as it is may suggest one: no `input`, not `destructive`; and one manifest does not suggest one chord twice. An application built before the field existed refuses a manifest that has it, like any field it does not know. When the extension is newly installed, the application binds each suggested chord of an action that needs no device (`settings.bindings`, see [local control](local-control.md#key-bindings-desktop)) if the key is free; a key something else holds stays with it, so an extension never takes a key from anything. An update binds nothing new, keeping what the person made of the bindings; removing the extension drops them.

An action may also carry `"help"`: what it does exactly, one line of at most 280 characters, shown under its label on the Binds page (a long one scrolls inside a short box). It is optional; an action without it shows its label alone.

## Execution

Every `plugin.json` or `plugin.toml` must explicitly declare `execution`. A missing declaration is
refused at discovery, installation and submission; no executor is inferred from the program or its
file name. For a standalone executable, declare `{"kind":"process"}` in JSON or
`execution = { kind = "process" }` in TOML. It does not take an `abi` field.

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
| 1 | `wasm` | **Official and recommended.** The store refuses a submission without WASM builds for Linux, Android and macOS. | Same module on every platform; isolated interpreter with no files, network, processes or OS calls of its own; only the brokered services the user granted; bounded memory and fuel; a crash ends only that worker. |
| 2 | `native_library` | Optional second choice, **not recommended**. The reviewed `.so` can accompany a release for manual installation; the store never installs it. | Crash isolation only: a separate helper loads it with the application's OS privileges. One ELF build per OS and architecture. |
| 3 | `process` | An optional standalone executable, **not recommended and unsupported**. A reviewed archive may accompany a release, but the store never installs it. | Where the host can launch it, it starts, supervises and stops an arbitrary program with the application user's privileges. Behaviour, portability and safety are the author's responsibility. The submission validator accepts Linux/Android ELF only. |

Why WASM: one portable module is the same bytes on Linux, Android and macOS, so behaviour does not drift between per-OS native builds, and the host can admit it on a new platform without a new compiler toolchain. Its only reach outside itself is the brokered service contract, so a capability the user did not grant does not exist for it. A native library or process can read files, open sockets and call the OS directly, whatever its manifest says; the host can only crash-isolate it, not confine it.

Platform scope: WASM extensions run on Linux, Android and Windows. On Windows the application talks to an extension's worker over a loopback TCP connection on a port the system assigns, which every connection must open with the worker's 256-bit bootstrap token (named pipes would restrict who may connect, but have no half-close, which every data exchange depends on); plugin folders rely on the access list the user's profile gives them, and links and junctions are refused. Each worker runs in a Windows job object, so processes it starts end with it. `peersus.input` replays on Windows through SendInput on the shared pointer and keyboard; Windows draws one pointer, so a device's own pointer is refused there. `peersus.displays` answers on every desktop system from the monitors the application has enumerated: it enumerates them on Linux and on Windows, and on macOS nothing does, so the device has no displays of its own to arrange; there is no refusal that says so. `peersus.input` on macOS refuses every replay as unsupported. macOS receives the same packages with **limited** support. An extension reaches a system only if its manifest's `platforms` lists it (or omits `platforms`).

Store submissions must provide WASM for Linux, Android and macOS. Neither native alternative is mandatory, and a submission may include either one without the other. Each optional archive must still pass binary, embedded identity and digest checks and independent review. Building an archive does not grant permission to install or run it.

## File ownership

A program name is one plain ASCII filename, up to 64 bytes, not a path, hidden filename, option or manifest filename. The host still validates its existence, ownership and execution suitability.

When `files` is present, it lists additional installed files; the manifest and program are already included. The maximum list length is 512. Paths must remain inside the package; traversal, backslashes, control characters and ambiguous spellings are refused. A package must not contain two competing manifest files. Without the optional list, legacy package installation takes the whole package subject to installer checks. Store validation has additional stricter packaging rules.

## Package metadata

Human metadata lives in a separate `package` block in the manifest file. The parser removes and validates it before deserializing the capability/service manifest. It cannot confer privileges or change participant identity.

Package versions are three unsigned decimal components (`major.minor.patch`), without leading zeroes, prerelease or build suffixes. A newer version compares those numeric components. Names are limited to 80 characters, descriptions to 1000 and changelog text to 4000. An optional public link must be plain HTTPS without credentials, query or fragment. Store packages additionally require author/link metadata and an embedded identity record that matches the manifest and signed catalog.

## Licence and notice (required)

The `agreements` field carries what a person reads and accepts before the extension may be
installed: at least one `licence` page and one `notice` page. It is optional in the type — an
older manifest still parses — but a package without it is never shown or installed, and the
store's tooling refuses it. `cargo xtask check-manifest <package-folder>` runs the application's
own check; `crates/peersus-api/src/agreement.rs` defines the page kinds, blocks, counts and text
limits.

```json
"agreements": {
  "pages": [
    {"kind": "licence", "title": "Apache License 2.0",
     "explains": "This extension is licensed under Apache-2.0: you may use, copy, change and share it.",
     "changes": "Accepting lets the application install and run this extension; it grants no extra rights.",
     "spdx": "Apache-2.0",
     "links": [{"label": "Licence text", "url": "https://www.apache.org/licenses/LICENSE-2.0"}]},
    {"kind": "notice", "title": "What this extension does",
     "explains": "What it does, what it uses and who made what it uses, in plain words.",
     "changes": "What changes for the person, their files and their devices once they accept."}
  ]
}
```

The same block as TOML:

```toml
[[agreements.pages]]
kind = "licence"
title = "Apache License 2.0"
explains = "This extension is licensed under Apache-2.0: you may use, copy, change and share it."
changes = "Accepting lets the application install and run this extension; it grants no extra rights."
spdx = "Apache-2.0"
[[agreements.pages.links]]
label = "Licence text"
url = "https://www.apache.org/licenses/LICENSE-2.0"

[[agreements.pages]]
kind = "notice"
title = "What this extension does"
explains = "What it does, what it uses and who made what it uses, in plain words."
changes = "What changes for the person, their files and their devices once they accept."
```

A licence page names an SPDX expression and at least one `https` link to the full text. The
application draws the agreement overlay at a fixed size with `Cancel` at the far left and
`Accept` at the right of the last page; an extension cannot place, size, press or hide those
buttons. It learns of its own agreement only `extension.tos.accepted` — service `peersus.extension`, method `tos`,
answer `{"accepted": true|false}` — true when this user accepted exactly the agreement installed
now. Acceptance is per user and is asked again when any word of the agreement changes. A second
boolean belongs to the person and not to the extension: `extension.tos.outside_store` — same service,
method `outside_store`, answer `{"accepted": true|false}` — true once this user accepted the
application's own warning about installing an extension from outside the store. That warning is
accepted once by the person, before the file is chosen (or at its first step, where a package was
chosen another way), and is not shown again for the next extension; each extension's licence and
notice are a separate agreement and are read for each. An extension cannot change either answer.


Use the real bundled manifests as complete examples. A partial JSON excerpt in this reference is not an installable manifest. Follow [the store contract](../extension-store.md) for archive and provenance requirements, and [service messages](service-messages.md) for runtime operations.
