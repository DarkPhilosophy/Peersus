# Extension store

How an extension gets from its author to a person's device, what each step proves, and what it does **not** prove.
The application discovers source manifests from the existing `extension` branch and obtains independently signed,
per-plugin/version approval records and packages from GitHub Releases. `catalog.json` is an application-local cache,
not a required remote file. The production verification key still requires maintainer setup; local verification
does not establish that signing protection, release publication or a real authenticated phone session is configured.

## The chain

```
author ──PR──▶ extension branch ──CI check──▶ maintainer approves ──▶ versioned release ──▶ the app
        (untrusted)   (sources)                (protected workflow)    (signed packages)   (local cache)
```

1. **Submit.** The author opens a pull request against `extension`, changing only
   `extension/<plugin-name>/**`: source, manifest, verified-account `ownership.json` and a `submission.toml` build/test/package recipe.
   First-party source directories are `peersus-<id>` (`peersus-clipboard`, `peersus-control`, `peersus-monitor`, `peersus-test`, `peersus-upload`); their installation IDs are the `<id>` part.
2. **Check** (`.github/workflows/extension-validate.yml`). Starts when a maintainer adds the `run:ci` label to the PR
   (contributors cannot add labels; remove and add it again after a push). Before anything is built, the plan job runs
   `scripts/verify-review-readiness.py`: the required reviewers (repository variable `REQUIRED_REVIEWERS`, or the
   script's default) must have reviewed this exact head commit, and no review thread on the current code may be
   unresolved (an outdated thread was answered by changing its code). A maintainer may add `review:waived` when a
   required reviewer is down or never reviews the branch: it skips the reviewers only, never an unresolved current
   thread, and it approves, signs or publishes nothing. Then three separate jobs plan the source scope,
   build/test/package untrusted source without credentials, and validate its output as data on a fresh runner.
   Trusted tools come from the default branch, pinned to one SDK commit for the entire run. The data-only validation job
   binds all platform artifacts to one plugin ID and emits `review.json`. A subsequent resource job measures only reviewed Linux WASM.
3. **Approve** (`.github/workflows/extension-approve.yml`). Manual (`workflow_dispatch`), inside the protected
   `extension-approval` environment. Dispatch requires the candidate (`id@version`), PR, successful source-check run, current head commit and SHA-256 of `review.json`. The gate independently verifies the changed paths, run association and
   a current, independent human maintainer approval (for a repository admin's own submission, which no one else can review while there is one maintainer: an admin's approval of the admission run's `extension-approval` environment, read back from that run's approval history). It downloads those exact reviewed artifacts, revalidates
   them, compares the review digest and signs one approval record containing the plugin's platform variants.
   No contributor source is executed with signing credentials. Before anyone is asked, a first job without the
   environment binds the version transition, PR and source commit, successful validation run, exact archive inventory
   and digests, benchmark verdict, and whether the licence and notice changed. A stale or missing benchmark cannot
   reach the protected job; that job rechecks the pinned evidence before signing. The run title shows the requested
   `id@version`, PR, commit and CI run, and the protected job name uses the verified transition. Open the run summary
   before deciding. **Approval signs and stages; it does not merge, publish, install or grant permissions.** A historical
   deployment without this preview cannot be identified from its run title or number alone.
   Each submission supplies WASM for Linux, Android and macOS; it can also offer WASM for Windows. The same extension ID and version apply to every artifact.
   The signed approval lists only the plugin's WebAssembly artifacts: that is what the store installs.
   An author may supply an optional native library (`.so`, the second choice, not recommended),
   an optional standalone `process` executable (not recommended, unsupported), both or neither.
   Any supplied archive is validated and digest-checked and can travel with the release for manual installation
   where the host can launch it. Native artifacts stay out of the catalog because the app prefers an exact
   architecture over the portable build; listing them would make native execution the default.
   Duplicate platform/architecture/execution targets are refused.
   Merely naming that environment in YAML does **not** configure required reviewers.
   Configure and verify its approval protection in GitHub before treating a dispatched job as approval-gated.
   GitHub currently limits required reviewers on Free, Pro and Team plans to public repositories
   ([availability](https://docs.github.com/en/actions/reference/workflows-and-actions/deployments-and-environments#required-reviewers)).
   Do not assume a private repository has this gate, change repository visibility implicitly,
   or install signing credentials before verifying the intended approval controls.
4. **Publish.** Use one release tag `extension/<plugin-id>/v<version>`, containing the generated platform
   archives, `approval.json`, `approval.sig` and `review.json`. The approval workflow stages these files as an
   `approved-release-<pr>-<commit>` artifact; actual release publication remains a separate authorized action.
   No global remote catalog, catalog branch or Pages deployment is required by the application.
5. **Install.** In the app, *Settings → Extensions*: search the unified list, then choose Install. Catalog entries carry a green 🛒 Store badge and keep their list position whether installed or removed. The app downloads through a
   separate helper process, verifies, and installs through the same installer as a manual install. The extension starts
   with **every permission refused**.

### Private development catalog (no public publication)

The explicit `private-store` development feature reads the same branch and release API with a read-only GitHub token (`Contents: read`). Authentication changes access, not the approval or package-integrity rules. It does not require Pages, change visibility, approve a submission or activate the production key.

Build both transport and frontend through the supported launcher:

```sh
./dev.sh desktop --container YOUR_EXISTING_BUILDER --private-store
./dev.sh android --container YOUR_EXISTING_BUILDER --private-store
```

Omit `--container` when the host already has the build dependencies. The launcher enables the desktop owner, UI and separate helper together. Android enables the native owner/UI feature and the matching debug-only Java transport; release variants keep private mode disabled.

In **Settings → Extensions → Private development catalog**, enter only the read-only GitHub token. The repository,
source directory, `extension` revision and verification public key are fixed in the development build. Save applies
the source and starts background discovery. Cancel does not save; a secure-save failure leaves the running source
unchanged. Unreadable saved configuration does not prevent opening the app to repair it.

The owner immediately restores its last authenticated local `catalog.json` cache, rechecking the original signed
approval bytes. It discovers source manifests through one branch-tree snapshot and immutable blob IDs, then looks
up each plugin/version release. Unchanged manifests reuse previously authenticated proofs. A source with no approved
release is not presented as installable. A failed refresh preserves the last valid visible listing and cache.
Startup, timer and manual refresh share a five-minute success budget; failures back off from one minute to one hour.
Independent signed version floors prevent rollback of one plugin even when another plugin updates.

Desktop stores configuration in the system credential store and passes it to the helper through a bounded anonymous
stdin pipe, never argv, environment variables or a plaintext scratch file. Android encrypts it with Android Keystore
AES-GCM in no-backup storage. Neither platform exposes it in preferences or snapshots. API discovery never follows
redirects. Repository-bound release-asset downloads support a direct response or one HTTPS redirect to GitHub asset
hosts; the redirected request has no authorization header. Both paths enforce streamed byte caps and disable proxies.
Token authentication is not approval: archive size/hash and manifest/embedded identity checks remain mandatory.

Exercised on 2026-11-11 with real credentials, in the private repository and with a development key whose seed is a
local, ignored file: a test extension published under `extension/test/v0.1.0` was listed on Linux and on one Android
phone, installed from the store on the phone, and, with its automatic-update switch on, replaced by `v0.1.1` on both
devices at their next refresh ([engineering log](engineering-log.md), E-029). This says nothing about the production
key, which still does not exist, or about the hosted checks and the approval workflow, which did not run: the test
approval was signed locally with the same tools. The isolated Echo demonstration below remains separate.

### A local store UX demonstration

The development-only `store_demo` example serves the existing public, signed Echo fixture to a real application owner. Only the fetcher is in memory: catalog signatures, package digests, embedded metadata checks, installation, listing and removal use the normal implementation. It does not change `MAINTAINER_PUBLIC_KEY`, contact the public store, generate signing credentials or use personal application data. It is excluded from normal builds by the `test-plugin` feature.

Use the root launcher rather than assembling separate build and owner/UI commands:

```sh
./store-demo.sh
```

It builds the required release programs and native hosts, resolves the triple-specific artifact directory through the shared build layout, creates a fresh data root, starts the isolated owner and attaches the real UI. It uses the same prerequisites as the desktop build.

The launcher chooses a new session rather than deleting or reusing personal data. In **Settings → Extensions**, refresh, search for **Echo**, inspect its author/version/description, install it, then remove it from the same card and confirm removal. Check that the card remains in its original list position with the green 🛒 Store badge and offers Install again after removal. Installation starts with no permissions. This fixture demonstrates installation, listing and removal, not functional file-transfer or clipboard actions; its validator-only program exits immediately, so a failed runtime status is expected and does not establish a module-host defect. The example attaches the normal module executors from the release application directory. Closing the UI or interrupting the launcher stops its own processes; its data remains under `target/store-demo/`.

This desktop demonstration does not configure the Android production store or prove HTTPS download operation. Android store UX requires its own explicitly isolated development setup; do not replace the production trust key with a test key to make a normal phone build accept the fixture.

### Branches, pull requests and CI

The roles are different:

* The default branch holds trusted application code, validators and workflow definitions, not a remote application catalog.
* `extension` is a lean source branch containing the two plugin directories, dedicated developer/submission README and submission workflow—not a mirror of the application. Validation tools and the SDK come from the trusted default branch.
* An author works on a separate feature branch, usually in a fork, and opens a PR **into `extension`**. The trusted planner enforces that every changed path belongs to exactly one `extension/<plugin-name>/` directory; changing workflows or another plugin cannot produce an admissible build.
* Submit source with its manifest and explicit build/test/package recipe. CI builds the binaries; contributors do not submit prebuilt archives as a substitute for reviewable source. The extension-branch README documents local checks against a committed SDK checkout.
* `ownership.json` binds the plugin owner and contributors to verified GitHub account IDs. The current accepted registry authorizes updates; only the owner can authorize contributor changes, and ownership transfer also needs the receiving owner's consent. The same independent maintainer gate applies to first submissions, updates and roster changes. Neither display author metadata nor a newly self-declared role grants authority.
* PR CI produces reviewed build artifacts, not approval, installation, a merge or publication. A green result does not prove that arbitrary native code is confined or works on every platform.
* The last job compares all package sizes and takes three interleaved startup/idle samples per version on the same trusted Linux window, scratch owner and WASM interpreter. It stages only the reviewed Linux WASM manifest and module from the published and submitted archives; native variants are never executed. The job summary and raw datasets report CPU, window main-thread time, helper time and RSS/PSS, without running extensions' actions or granting permissions. A first release has no predecessor: the report is **INCONCLUSIVE**, and admission can proceed only after all three submitted samples completed. Updates with missing samples cannot pass. `impact:accepted` can waive a measured blocking regression, not missing evidence. See [Resource impact](development.md#resource-impact).
* The maintainer reviews the exact source revision and build report. The protected workflow requires independent human approval for that commit, not author self-approval or a bot review; only a repository admin's own submission may instead be admitted by an admin's approval of the admission run in its protected environment (Review deployments). It stages one signed plugin-version release bundle; it does not merge the PR or publish the release.
* The website is a separate preview/presentation surface. Its existing staged-catalog preview is not the application's discovery source and does not activate a public store.

Creating a remote branch, pushing a submission, opening a PR, configuring a signing secret and publishing Pages are separate operations. The local demonstration requires none of them.


## What a package must be

Each CI-produced `<name>-<platform>.tar.gz` (WASM), `<name>-<platform>-native_library.tar.gz` or
`<name>-<platform>-process.tar.gz` contains one folder `<name>/` with:

| File | Rule |
|---|---|
| `plugin.json` | JSON only for the store. A `package` block with `name`, `version` (`major.minor.patch`), `author`, `link` (https, no credentials, query or fragment) and optionally `description`, `changelog`. |
| `execution` in `plugin.json` | Required WASM coverage: Linux, Android and macOS; Windows may be included. A `.so` or standalone executable may accompany WASM but neither is required. The validator checks each supplied archive's actual contents, not its filename. Only WASM enters the catalog. Neither native alternative is a sandbox. See [support tiers](api/manifests.md#support-tiers). |
| the program | The file `program` names. Must carry the **embedded record** below. |

**The id is the folder name**, never a field of the package. The host assigns it; the catalog pins it; a mismatch is refused.

### Licence and notice (required)

Every extension — through the store or installed from a file — carries an `agreements` block in
its manifest: the licence(s) the person accepts and a notice that says what the extension does.
A manifest without one is never shown or installed; the checker is `cargo xtask check-manifest
<package-folder>`, which the submission check, the packaging gate and the approval workflow run
on the same bytes that ship.

```json
"agreements": {
  "pages": [
    {
      "kind": "licence",
      "title": "Apache License 2.0",
      "explains": "This extension is licensed under Apache-2.0: you may use, copy, change and share it.",
      "changes": "Accepting lets the application install and run this extension; it grants no extra rights over your files, devices or data.",
      "spdx": "Apache-2.0",
      "links": [{"label": "Licence text", "url": "https://www.apache.org/licenses/LICENSE-2.0"}]
    },
    {
      "kind": "notice",
      "title": "What this extension does",
      "explains": "What it does, what it uses and who made what it uses, in plain words.",
      "changes": "What changes for the person, their files and their devices once they accept."
    }
  ]
}
```

The same manifest written as TOML:

```toml
[[agreements.pages]]
kind = "licence"
title = "Apache License 2.0"
explains = "This extension is licensed under Apache-2.0: you may use, copy, change and share it."
changes = "Accepting lets the application install and run this extension; it grants no extra rights over your files, devices or data."
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

At least one licence page and one notice page are required; a licence page names an SPDX
expression and at least one `https` link to the full text. Page kinds, blocks, counts and text
limits are defined in `crates/peersus-api/src/agreement.rs`; the application draws the overlay at
a fixed size with `Cancel` at the far left and `Accept` at the right of the last page — an
extension cannot place, size, press or hide those buttons. It learns only `extension.tos.accepted`
(service `peersus.extension`, method `tos`, answer `{"accepted": true|false}`). Acceptance is per
user and asked again when any word of the agreement changes. A package installed from outside
the store additionally shows the application's own warning, which must be scrolled to the end
and held to confirm; the extension cannot change it.


### Manual download versus Store identity

The stable IDs for the first-party extensions are `clipboard` and `upload`. Keep
that top-level archive directory unchanged across manual downloads, catalog
submission and updates. `package.name` is display metadata, not a UUID or a
second registration key.

The unified list joins an installed package and a catalog listing by this ID,
so installing `clipboard` manually does not create a second Store card for
`clipboard`. Installing identical package contents again is a no-op; different
contents under an occupied ID are refused by the command-line installer rather
than silently replacing it (remove first). The window and the Store replace an
installed extension only through the one replacement path, and what that means
for its permissions is one rule (`peersus_core::plugins::Installation`):

| Case | What it was allowed |
|---|---|
| **Install**: nothing under the ID (also after a removal) | nothing; it starts with no permissions |
| **Store update**: strictly newer version from the signed Store, over a sound extension that was itself installed from the Store | kept where the new version still asks for it; forgotten where it no longer does (asking again later asks the person again); anything newly asked starts off |
| **Replacement**: from outside the Store, over an extension installed from outside the Store, over a damaged one, or not newer | nothing; it starts with no permissions |

The owner records a Store installation in the extension's folder (`from-store`,
one of the application's own files that a package cannot bring). Only the
maintainer's review and the catalog's binding of the ID to its owner make two
versions the same extension; a shared ID from anywhere else proves nothing, so
it carries no consent. Renaming the archive's root to a different valid ID
changes its identity.

This join is **not provenance verification**. The current Installed/Update decision
compares the declared installed version with the catalog version. A manual package
with the same ID/version is not thereby proven to be the signed catalog's bytes.
The archive digest and metadata checks run when installing from the signed Store;
the green Store badge describes the listing, not certification of arbitrary
manually installed files. Never infer automatic approval from a shared display name.

### First-party package eligibility

The upload WASM packaging gate now embeds `package` metadata into the module's
`peersus.meta` custom section and validates the finished archive with the same
data-only submission validator before replacing its output. Its mandatory worker
test executes that metadata-bearing module with synthetic host services; it does
not access a real credential or upload to a provider.

`scripts/package-native-extension.py` packages one compiled artifact per invocation. A recipe
that chooses an optional `.so` or standalone executable declares that archive alongside its
WASM outputs. Optional archives still carry matching embedded identity, declared companions
and license notices and pass independent validation; admitting an archive is not a runtime
support guarantee. The isolated Echo demo is not a functional test of either first-party
plugin, and successful local validation is not a live PR or deployment.

### The embedded record

Text in a manifest is easy to edit, so the same author and link must also be compiled **into the program**, where the
host reads them *without loading or running it*:

| Format | Where |
|---|---|
| ELF (Linux, Android `.so`) | section `peersus.meta` |
| Mach-O | section `peersus.meta` |
| PE (Windows) | section `.psmeta` (a PE image allows eight characters) |
| WebAssembly | custom section `peersus.meta` |

Layout: 8 bytes `PSUSMETA`, `u32` little-endian length, then the JSON of the `package` block, optionally zero-padded.
Generate it from the manifest so the two cannot drift:

```
python3 scripts/embed-metadata.py header plugin.json > metadata.h
# in the plugin's C source, once:
#   #include "peersus_plugin.h"
#   #include "metadata.h"
#   PEERSUS_METADATA_RECORD(PEERSUS_METADATA_BYTES);
```

The host requires the record to equal the manifest's `package` block, and both to equal the signed catalog entry.
Check a package locally exactly as CI does: `python3 scripts/validate-extension.py extensions/<name>.tar.gz`.
For a compiled WebAssembly module, use
`python3 scripts/embed-metadata.py wasm plugin.json module.wasm > packaged.wasm`.
The tool refuses an existing metadata section instead of appending a duplicate.

## What each layer proves

Every layer was disabled in turn and the test that guards it was shown to **fail** (and was restored).

| Layer | Refuses |
|---|---|
| Approval signature (Ed25519, pinned public key) | unapproved bytes or edits to a per-version approval record |
| Per-plugin version floor (authenticated records kept on disk) | replay of an older signed version, including after restart or alongside another plugin's update |
| Archive size + SHA-256 pinned in the signed entry | any other archive, including a well-formed one |
| Binding: catalog ↔ manifest ↔ compiled-in record | an edited manifest author or link, even if the catalog was made to agree with the edit |
| Isolation gate | a legacy process package |
| Same installer as a manual install | links, devices, oversized or unsafe archives; the digest check before install |

**What this does not prove.** The signature and the checks prove *who approved exactly these bytes*. They do not make an
extension harmless. An approved extension still runs only as safely as the process boundary it runs behind, and it
starts with nothing allowed. A maintainer who approves a malicious package approves it; the pipeline protects the
identity and provenance of what is approved, not the judgement of the person approving.

An **update** from the Store keeps what the installed version was allowed that the new version still asks for, when the
installed version also came from the Store; any other replacement forgets it (see the table above). The Store says which
next to the button.

## Maintainer action (one time)

The application cannot sign and holds no key. **Until these are done every build reports the store as "not set up"
and refuses every store command** — there is no fallback that trusts an unsigned catalog.

1. Make the key, on a trusted machine:
   `cargo xtask catalog-keygen --out ~/peersus-catalog-seed` — writes the seed (mode 0600, never printed) and prints
   the **public** key as a Rust array.
2. Commit that array as `MAINTAINER_PUBLIC_KEY` in `crates/peersus-app/src/store_trust.rs`. A public key is not secret.
3. Store the seed's contents as the secret `CATALOG_SIGNING_KEY` of the protected environment `extension-approval`,
   and require yourself as its reviewer. Keep an offline copy. Anyone who obtains it can sign a catalog the app trusts.
4. Confirm the fixed repository and `extension` source revision in both transports. The application reads branch
   manifests and release metadata directly, not a Pages catalog.
5. After reviewing the exact approved release artifact, publish its files under the tag recorded in `release-tag`:
   `extension/<plugin-id>/v<version>`. Keep WASM and native packages under that one plugin/version release.
   Do not overwrite an already admitted version with different bytes: the client refuses same-version equivocation.
   No `catalog.json` or catalog archives need to be committed to the default branch. Releases, tags, environment
   protection and signing credentials are external writes and are not created by local application tests.

### Store links

A web page opens one extension of the store in Peersus with the address `peersus://store/<id>` (the site's **Install**).
The desktop entry registers `x-scheme-handler/peersus` and starts the window with the address (`peersus-ui %u`); Android
declares a `VIEW` intent filter for that scheme and host. The window opens Settings on Extensions, goes to that extension's
row once the store has answered and, when it is listed and neither installed nor waiting, starts its Install: the package
is downloaded and checked, and the licence and notice are shown to accept. A link installs nothing by itself, and nothing
of the address but the extension's id is read (`peersus_core::model::store_link_plugin`). A link that reaches an
application already running (Android's `onNewIntent`, or `peersus open-link <address>`) goes through the owner
(`show-store-entry`), which tells every open window once. On a desktop a link opens a new window even when one is open.

### Pictures in the store

An extension's source may give the store pictures to show before anyone installs it, in a `store/` folder beside its
`plugin.json`: `icon.png` (or `.webp`) and up to eight screenshots, `screenshot-1.png` … `screenshot-8.png` (or `.webp`),
each at most 1 MiB. `scripts/check-extension-source.py` refuses anything else in that folder, a file whose first bytes are
not the kind its name says, or two pictures of one name. The site gathers them from the same source as the approved version
(`fetch-store-approvals.py`), checks them again (`site/build.py`) and lists them in `store/index.json`; the store shows the
icon on the extension's tile, the first screenshot as its banner and every screenshot in its details. The pictures are what
the author shows, reviewed with the source; they are not part of the signed approval, and the application does not show
them yet.

### Website data and offline operation

`scripts/fetch-store-approvals.py` gathers, when the site is staged, the signed approval of the version each extension
source on the `extension` branch names, and the application's signed development prerelease manifest (`app/test`).
`site/build.py --approvals DIR` checks their shape and stages them same-origin: `store/index.json` lists the extensions,
`store/<id>/approval.json` and `approval.sig` hold each approval, `app/update.json` and `update.sig` the prerelease. The
browser reads only those staged files (or, in an explicitly marked development preview, `store-test/catalog.json`); it
does not query the GitHub API. Download links point at the release each file was published on; **Install** is the link
`peersus://store/<id>`, which opens that entry in Peersus. Approving an extension does not update a deployed site by
itself: the site is staged and deployed again.

A local HTTP server can serve the complete staged folder without internet access (downloads then fail, the list does
not). Opening HTML directly through `file://` renders the static page but does not reliably permit the fetches; the
tested browser shows the store-unavailable message. There is no service worker or offline cache.

## Languages: the same fetcher, NOT signed

The application's language files travel the store's road without its proof. The `languages` branch holds
`languages/<release>/<tag>.po` and the offer `languages/<release>/languages.json`; the owner fetches the branch's listing
and each file by its blob id through the same fetcher as the store (the helper on a desktop, the `:store` service on
Android), bounded in size. Nothing is signed: a language file changes only the words shown, never what anything does, and
what does not fit its English text (placeholders, plural forms, length, control characters) is dropped where it is read,
leaving English. A shared translation is uploaded into the release's directory as a pull request; a maintainer reviews it
and runs `cargo xtask check-languages`, and `cargo xtask languages-offer` writes the offer. See [development](development.md#languages).

## Limits, stated plainly

* **Android** attaches the same store verifier and installer as desktop. Downloads run in a private, non-exported `:store` service using platform HTTPS and a bounded file descriptor, with a 75-second caller deadline. The service is a separate same-UID process, not an adversarial security sandbox. A missing production verification key still reports "not set up". Local transport and Binder tests do not establish public-store deployment or complete phone UX verification.
* **Windows**: the store and download helper use Unix package/file semantics; Windows operation is not verified.
* **AppImage**: `scripts/build-appimage.py` packages the staged Linux binaries, including helpers. That is packaging
  support, not proof of store operation on every target distribution; certificate discovery and helper execution need
  runtime validation on the intended host.
* **NixOS and similar**: the download helper runs with an **empty environment** on purpose (no proxy variables, no
  credentials). Systems that need `SSL_CERT_FILE`-style variables to find certificates would need the helper to be
  given exactly those, not the whole environment.
* The embedded-record tool in `scripts/validate-extension.py` reads ELF and WebAssembly. The host also reads PE and
  Mach-O; CI does not yet validate those formats, so a package in them is refused by the CI check until it does.
* The signing key does not exist yet. The fixtures under `crates/peersus-app/tests/fixtures/catalog` are signed with a
  throwaway test seed and carry no project key.
