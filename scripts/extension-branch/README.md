# Peersus extensions

This branch is the source-submission and review home for Peersus plugins. It is **not the application branch**. Application code, the SDK and trusted CI tools belong on `main`; this branch contains plugin sources, this guide and the small workflow definitions needed to review submissions.

## The plugins

| Plugin | Stable installation ID | Submitted packages | Purpose |
| --- | --- | --- | --- |
| `peersus-clipboard` | `clipboard` | WASM for Linux, Android, macOS; Linux `.so` and Linux native process (not recommended) | Explicit text sharing and opt-in clipboard synchronization between permitted paired devices |
| `peersus-upload` | `upload` | WASM for Linux, Android, macOS; Linux `.so` (not recommended) | Upload one selected file to an approved destination and obtain a shareable link |
| `peersus-control` | `control` | WASM for Linux, Android, macOS; Linux `.so` (not recommended) | A phone as touchpad and keyboard for a paired computer |

Each plugin has one stable ID, one submission and one catalog listing. A platform artifact may be named `upload-android.tar.gz`, but its internal root must remain `upload/`; changing that root creates a different plugin identity and is refused.

**WASM is mandatory.** Every submission must build, test and package its WebAssembly guest for Linux, Android and macOS; validation refuses one without them. A `native_library` (`.so`) build is a second, not recommended tier and a `process` binary a third, with limited support and no guarantees; either may only accompany the WASM build, as `<id>-<platform>-<kind>.tar.gz`. Why: see [support tiers](https://github.com/DarkPhilosophy/Peersus/blob/main/docs/api/manifests.md#support-tiers). macOS support is limited; Windows has no extension host yet.

Presence in this branch is not by itself an installation, a security certification or permission to read a person's files or clipboard. The signed catalog identifies the versions the maintainer has admitted. Installation and device permissions remain explicit user choices.

## Submit a new plugin

For example, to propose `fly-me`:

1. Fork this repository and create a feature branch **from `extension`**, not from `main`.
2. Add the complete source under `extension/fly-me/`. Include a README, a license, the plugin manifest, dependency lockfiles, tests, `ownership.json` and `submission.toml`.
3. Make one PR **targeting `extension`**, changing only `extension/fly-me/**`. Do not change the workflows, another plugin or the application as part of that PR.
4. Complete the [extension PR template](.github/PULL_REQUEST_TEMPLATE.md): lead with what the plugin does and a concrete usage example, then explain platforms, permissions, data disclosure, configuration and actual test results. Include a plugin-local README. Do not include credentials, personal files or private service configuration.
5. Inspect the source-build and package-validation checks. Fix failures on the same feature branch. A new revision invalidates approval of the previous revision.
6. Respond to maintainer triage and review. A passing build is necessary, **not approval**. The maintainer decides whether the behavior, permissions, dependencies and maintenance plan are acceptable.
7. After the exact revision and its built package digests have passed review, the maintainer admits that revision to the official signed catalog. Merely opening a PR, obtaining a label or passing CI does not publish a plugin.

For an update, change the existing plugin directory. Keep the same stable ID, increase its version, describe changes and migrations, and use the same build → test → validation → review → admission route. Do not register platform builds as new plugins.

### Ownership and contributors

Each plugin contains an `ownership.json` file. It is an authorization registry, separate from the display-only `package.author` field:

```json
{
  "schema": 1,
  "owner": {"login": "your-github-login", "id": 12345},
  "contributors": [
    {"login": "contributor-login", "id": 67890}
  ]
}
```

The account values above are examples, not accounts to copy. Use each person's real public GitHub login and numeric account ID. CI checks that the login resolves to that immutable ID and a human account; names alone cannot grant ownership. The owner must not also appear in the contributors list, and duplicate contributors are rejected.

- **First submission:** the authenticated PR author becomes the proposed owner by declaring their own verified account. A PR submitted on their behalf instead needs that declared owner's approval of the exact head commit. A maintainer still gives final admission approval.
- **Updates:** the existing owner or an existing contributor can submit changes. Anyone else can propose a change, but an existing owner/contributor must approve that exact revision before it is authorized. The authorization check uses the current accepted registry on `extension`, never the roles newly claimed by the PR.
- **Add/remove contributors:** the owner edits `ownership.json` in a normal plugin PR. A contributor can prepare the edit, but the current owner must approve its exact head commit. Contributors cannot promote themselves, add another contributor or remove the owner; a store maintainer's approval alone does not substitute for owner consent.
- **Transfer ownership:** change the owner record through the same PR route. Both the current and receiving owner must consent, either as authenticated author or through approval of the exact revision. A new owner cannot claim an existing plugin unilaterally.
- **Final decision:** every update, contributor change and transfer still requires the same build, independent package validation and final human maintainer approval as the first submission. Plugin ownership does not grant repository write access, store signing permission or a review bypass.

An updated commit invalidates earlier consent. Dismissed, stale and bot reviews do not count. If owner consent arrives after the ownership check failed, rerun the source checks for that same head commit. Ownership is checked again against the current accepted registry immediately before catalog admission, so a revoked contributor cannot rely on an old green run.

## Build your plugin

Start with the behavior and trust boundary, not with an archive:

1. Choose one stable ID and the operating systems you actually implement. Keep all platform implementations under the same source directory. A source directory can be named `fly-me` or `peersus-fly-me`; both map to installation ID `fly-me`, which must also be the recipe ID and archive root.
2. Choose a supervised process, an ABI-1 native module or an ABI-1 WASM module. Use the pinned SDK's process implementation or module header and examples for the wire contract; renaming an ordinary executable does not implement the broker protocol.
3. Define your services, methods and events in `plugin.json`. Declare imports and capabilities truthfully. UI actions must target a service and method your implementation actually handles. Start from the first-party manifests, but remove services, permissions, companions and UI contributions your plugin does not implement.
4. Implement startup, request handling, bounded data transfer, cancellation and shutdown. Separate normal cancellation, denied permission and a real failure; do not return success merely because a dialog closed.
5. Test the real behavior without personal files or production credentials. Document required external programs, platform limitations and any third-party destination in your own README.
6. Write your build/test/package recipe below. Package the actual compiled program, matching embedded metadata, declared support files and licenses. Do not ship caches, a full application checkout, build credentials or dependency source trees instead of the required runtime files.

A typical source submission looks like this; the implementation language and source layout are yours to choose:

```text
extension/fly-me/
  README.md
  LICENSE
  plugin.json
  submission.toml
  <source, dependency manifest and lockfiles>
  <tests and the packaging implementation>
```

The archive layout is different from this source layout: it contains `fly-me/` and the deployable runtime files described below, not `extension/fly-me/` or a copy of this repository.

## Source and build contract

`submission.toml` describes how to build, test and package your source. Commands are argument arrays, **not shell command strings**. The build runner provides these placeholders:

- `{source}`: this plugin's source directory in a disposable build workspace;
- `{sdk}`: the trusted application/SDK revision selected by CI;
- `{out}`: the directory in which the packaging command must produce its declared archives.

A self-contained plugin can use its own build system. It does not need to be written in Rust. A source submission may run arbitrary build code, which is why build jobs have **no signing secrets, no write token and no persisted Git credentials**. Do not rely on privileged installation, personal accounts or a developer workstation.

A Rust example for `fly-me`:

```toml
schema = 1
id = "fly-me"
build = [["cargo", "build", "--locked", "--release"]]
test = [["cargo", "test", "--locked"]]
package = [["python3", "{source}/package.py", "--out", "{out}"]]
artifacts = ["fly-me-linux.tar.gz"]
```

`package.py` is your real packaging implementation, not an installer. It must write the archive listed in `artifacts`. Include every platform implementation in the same source directory and declare all output archives in this same recipe. Existing bundled Rust plugins can use the explicitly declared `workspace_member` integration with the pinned SDK workspace; do not assume a workspace-inheriting Cargo manifest builds standalone.

CI runs build, test and packaging in order. A failed command fails the submission. A **different fresh job** validates the resulting archives as data; it never loads a native library, executes a package or trusts a validation report written by the build job.

### Test the submission locally

Use a checkout of the application branch as the SDK. Run these commands from that checkout, replacing the source path and output directory with your own **new** temporary directories:

```sh
python3 scripts/check-extension-source.py \
  --source /path/to/extension/fly-me \
  --sdk . \
  --out /path/to/new-candidate
python3 scripts/validate-extension-build.py \
  --directory /path/to/new-candidate \
  --id fly-me \
  --commit "$(git -C /path/to/plugin-checkout rev-parse HEAD)" \
  --sdk "$(git rev-parse HEAD)" \
  --out /path/to/new-review
```

The build helper exports the committed SDK into a disposable workspace; it does not build against uncommitted SDK changes or replace files in the application checkout. Independent submissions are staged under `plugin/<source-name>/` in that disposable SDK, so their own workspace can reference the pinned host SDK without becoming an application workspace member. Application `main` contains no extension source. The legacy `workspace_member` form is accepted only when that exact member exists in the pinned SDK; new independent submissions do not need it.

These commands execute your plugin's build recipe as your local account. Only run them for source you trust. CI uses an expendable build runner and then a separate validation runner; a local shell is not a security sandbox.

Successful validation writes `review.json` and the exact validated archives into the review directory. Its SHA-256 covers the source commit, SDK commit, every archive digest and every platform mapping. Keep that review document with the archives; a report for one build must never be reused to approve another build.

## Package and runtime rules

Each archive has exactly one top-level directory named after the stable installation ID:

```text
fly-me/
  plugin.json
  <the program declared by plugin.json>
  <declared support files and license notices>
```

The manifest describes exported/imported services, execution ABI, supported platforms, contributions and package metadata. `package.name` is a display name, **not another installation ID**. Metadata embedded in the actual program must agree with the manifest. Include upstream license notices and respect all declared bounds.

Store admission supports **three execution forms**: an explicitly declared `{"kind":"process"}`, a native library with ABI 1, or a WASM module with ABI 1. A process declaration has no `abi` field. Processes and native libraries run with the application's operating-system user privileges; a separate process provides crash isolation, **not a security sandbox**. A catalog signature authenticates reviewed package bytes, not confinement.

The source-platform table above does not promise that every platform build is already admitted. The current independent native-package validator handles ELF builds for Linux and Android; WASM modules are architecture-independent and explicitly declare their supported operating systems. Native macOS/Windows packages require their corresponding binary-format validation before admission. Do not label a Linux shared library as a macOS artifact, or use `architecture = "any"` for a native binary.

For process submissions, the validator requires a genuine ELF executable or PIE with an entry point inside an executable, file-backed load segment, supported machine architecture and explicit Linux/Android platform declarations. A shared library, object file or WASM module cannot pass by changing its manifest to `process`. The first-party Linux packaging helper selects `platforms: ["linux"]` for the Linux build; source support for macOS does not mislabel that archive as a macOS build.

The first-party recipes build and test the real clipboard and upload programs. `scripts/package-native-extension.py` packages the compiled process, preserves declared companions, embeds matching package metadata and includes license notices. Upload's recipe additionally packages the real Android WASM implementation under the same `upload/` root. Neither recipe signs, installs or publishes anything.

Processes use the SDK's existing supervisor bootstrap and framed broker protocol. Never print bootstrap values or persist them in a package. Broker imports and permissions govern broker-mediated operations; they do not remove a native process's ambient operating-system access. Implement explicit consent and truthful failure/cancellation handling for that access.

### Implement the chosen runtime contract

The language-neutral interface is defined in the application's SDK at `crates/peersus-plugin-runtime/sdk/peersus_plugin.h`. Use that pinned SDK as the source of truth:

- WASM modules export memory and `peersus_main`; host calls are imported from `peersus_v1`. This is a freestanding module interface, not WASI.
- Native modules export the C symbol `peersus_plugin_main` and accept the versioned `PeersusHostV1` callback table. Check both ABI version and structure size; do not expose a Rust ABI.
- Control messages are the broker's bounded JSON operations and deliveries. Host callbacks carry message payloads without the socket framing prefix.
- Bulk bytes use host-issued tickets or claims through `data_open`, `data_read`, `data_write`, `data_shutdown` and `data_close`. A ticket is not a device identity or a filesystem path.
- A module never receives bootstrap tokens or owner-owned operating-system handles through this interface. Imports describe dependencies, not granted capabilities.

The SDK's `echo.c` is a small protocol example, not a substitute for testing your real behavior. Include tests for cancellation, denied permissions, malformed inputs, bounded payloads and platform failures that your plugin actually handles. Never report success after an upload, clipboard write or configuration operation failed.

Do not embed API keys or give a plugin automatic access to files, networking, clipboard, paired devices or another plugin. Use the SDK's host-service and permission contracts. Uploading to a third-party service must disclose the destination and obtain the required user consent; it is not the same as encrypted device-to-device transfer.

## Triage, bots and maintainer approval

Automation helps classify PRs, report check results and highlight what needs maintainer attention. It must not approve its own source, grant device permissions or treat a green check as an official listing. Dependency-update bots propose updates through PRs; they do not bypass review.

PR labels make review queues searchable:

| Labels | Meaning |
| --- | --- |
| `platform:windows`, `platform:android`, `platform:macos`, `platform:linux` | Platforms declared in the submitted manifests or directly affected platform directories; not proof that a build was tested there |
| `artifact:binary`, `artifact:shared-library`, `artifact:wasm`, `artifact:apk` | Implementation or package category |
| `format:elf`, `format:exe`, `format:so`, `format:dll`, `format:dylib` | Native formats implied by the declared execution form and platforms |
| `ci:pending`, `ci:failed`, `ci:passed` | Checks and commit statuses for the **current PR head**, refreshed on source changes and CI completion |
| `scope:extension`, `scope:application`, `kind:dependency`, `review:maintainer` | Review queue and required human gate |
| `priority:critical`, `priority:high`, `priority:normal`, `priority:low` | Maintainer-assigned urgency; automation creates the available labels but does not invent priority from a title or description |

For example, search `is:pr is:open label:platform:android label:artifact:wasm label:ci:passed`. A new revision removes stale CI-success labels; an absent check or all-skipped checks do not mean a passed build. Priority and other human labels are preserved. No label grants admission or replaces maintainer review.

The same separation applies to PRs targeting `main`: automated checks and triage support human review, but application changes are not plugin submissions.

The maintainer reviews the **exact source revision and exact validated build artifacts**, not a later rebuild. Signing and publication are separate privileged jobs. They must not check out or execute contributor code with signing or publication credentials. The official catalog must preserve one entry per stable plugin ID and select the appropriate platform artifact within that entry.

The admission workflow takes four explicit values: the submission PR number, successful source-check run ID, exact source head commit, and the SHA-256 of `review.json`. It downloads `extension-reviewed-<PR>-<commit>`, never a contributor-provided release or a fresh privileged rebuild. The trusted gate checks that the PR changes only its own plugin directory, still has that head commit, and has an independent human maintainer approval for it (a repository admin's own submission: an admin's approval of the admission run's protected environment instead, since GitHub lets no author approve their own PR). A bot approval, author self-approval, dismissed review, stale approval or green run from a PR that modified workflow files is insufficient.

Every archive is independently revalidated before signing. `catalog-sign --review ...` then emits a schema-2 catalog with **one entry per plugin** and platform/architecture variants inside that entry. Clients still read legacy schema-1 catalogs; unsupported variants do not become duplicate listings. Signing a catalog and staging a website are not the same as deploying that website or configuring production trust.

If required CI, reviewer protection, signing or publication is unavailable, the submission remains unadmitted. Documentation and workflow definitions alone are not evidence that the live store is configured or that a plugin has been published.
