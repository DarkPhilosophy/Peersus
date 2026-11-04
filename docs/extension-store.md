# Extension store

How an extension gets from its author to a person's device, what each step proves, and what it does **not** prove.
The implementation provides package validation, protected approval/signing workflow definitions, catalog verification
and desktop download/install integration. A public store is **not active** merely because those components exist:
the production verification key and public address still require maintainer setup, and publishing is separate.
Local tests are not evidence that GitHub environment protection or a deployed catalog has been configured.

## The chain

```
author ──PR──▶ extension branch ──CI check──▶ maintainer approves ──▶ signed catalog ──▶ Pages ──▶ the app
        (untrusted)   (no secrets)             (protected workflow)      (Ed25519)      (static)   (one click)
```

1. **Submit.** The author opens a pull request against `extension`, changing only
   `extension/<plugin-name>/**`: source, manifest, verified-account `ownership.json` and a `submission.toml` build/test/package recipe.
   First-party source directories are `peersus-clipboard` and `peersus-upload`; their installation IDs are `clipboard` and `upload`.
2. **Check** (`.github/workflows/extension-validate.yml`). Three separate jobs plan the source scope,
   build/test/package untrusted source without credentials, and validate its output as data on a fresh runner.
   Trusted tools come from the default branch, pinned to one SDK commit for the entire run. Validation binds
   all platform artifacts to one plugin ID and emits a digest-bound `review.json`; it does not run package code.
3. **Approve** (`.github/workflows/extension-approve.yml`). Manual (`workflow_dispatch`), inside the protected
   `extension-approval` environment. Inputs identify the PR, successful source-check run, current head commit
   and review-document SHA-256. The gate independently verifies the changed paths, run association and
   a current, independent human maintainer approval. It downloads those exact reviewed artifacts, revalidates
   them, compares the review digest and signs a catalog with one plugin entry containing its platform variants.
   No contributor source is executed with signing credentials.
   Merely naming that environment in YAML does **not** configure required reviewers.
   Configure and verify its approval protection in GitHub before treating a dispatched job as approval-gated.
   GitHub currently limits required reviewers on Free, Pro and Team plans to public repositories
   ([availability](https://docs.github.com/en/actions/reference/workflows-and-actions/deployments-and-environments#required-reviewers)).
   Do not assume a private repository has this gate, change repository visibility implicitly,
   or install signing credentials before verifying the intended approval controls.
4. **Publish.** The signed `catalog.json` + `catalog.sig` and the approved archives are served as static files
   (GitHub Pages). Publishing them is a separate, deliberate step that is **not automated here**.
5. **Install.** In the app, *Settings → Extensions*: search the unified list, then choose Install. Catalog entries carry a green 🛒 Store badge and keep their list position whether installed or removed. The app downloads through a
   separate helper process, verifies, and installs through the same installer as a manual install. The extension starts
   with **every permission refused**.

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

* The default branch holds trusted application code, validators, workflow definitions and the previously approved catalog.
* `extension` is a lean source branch containing the two plugin directories, dedicated developer/submission README and submission workflow—not a mirror of the application. Validation tools and the SDK come from the trusted default branch.
* An author works on a separate feature branch, usually in a fork, and opens a PR **into `extension`**. The trusted planner enforces that every changed path belongs to exactly one `extension/<plugin-name>/` directory; changing workflows or another plugin cannot produce an admissible build.
* Submit source with its manifest and explicit build/test/package recipe. CI builds the binaries; contributors do not submit prebuilt archives as a substitute for reviewable source. The extension-branch README documents local checks against a committed SDK checkout.
* `ownership.json` binds the plugin owner and contributors to verified GitHub account IDs. The current accepted registry authorizes updates; only the owner can authorize contributor changes, and ownership transfer also needs the receiving owner's consent. The same independent maintainer gate applies to first submissions, updates and roster changes. Neither display author metadata nor a newly self-declared role grants authority.
* PR CI produces reviewed build artifacts, not approval, installation, a merge or publication. A green result does not prove that arbitrary native code is confined or works on every platform.
* The maintainer reviews the exact source revision and build report. The protected workflow requires an independent human approval for that commit, not author self-approval or a bot review; it emits a signed-catalog artifact but does not merge the PR or deploy Pages.
* The website workflow currently creates a preview artifact only. GitHub Pages publication and the production signing environment must be configured separately before clients can use a public store.

Creating a remote branch, pushing a submission, opening a PR, configuring a signing secret and publishing Pages are separate operations. The local demonstration requires none of them.


## What a package must be

CI-produced `<name>-<platform>.tar.gz`, containing one folder `<name>/` with:

| File | Rule |
|---|---|
| `plugin.json` | JSON only for the store. A `package` block with `name`, `version` (`major.minor.patch`), `author`, `link` (https, no credentials, query or fragment) and optionally `description`, `changelog`. |
| `execution` in `plugin.json` | Explicit `{"kind":"process"}`, `{"kind":"native_library","abi":1}`, or `{"kind":"wasm","abi":1}`. Native processes and libraries have application-user OS privileges; neither is a security sandbox. ELF process artifacts require an executable entry point, supported architecture and explicit Linux/Android platforms. |
| the program | The file `program` names. Must carry the **embedded record** below. |

**The id is the folder name**, never a field of the package. The host assigns it; the catalog pins it; a mismatch is refused.

### Manual download versus Store identity

The stable IDs for the first-party extensions are `clipboard` and `upload`. Keep
that top-level archive directory unchanged across manual downloads, catalog
submission and updates. `package.name` is display metadata, not a UUID or a
second registration key.

The unified list joins an installed package and a catalog listing by this ID,
so installing `clipboard` manually does not create a second Store card for
`clipboard`. Installing identical package contents again is a no-op; different
contents under an occupied ID are refused by normal install rather than silently
replacing it. Store updates use the explicit replacement path and reset grants.
Renaming the archive's root to a different valid ID changes its identity.

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

Clipboard and desktop upload use explicit native-process recipes. The first-party
packager builds Linux artifacts with matching embedded metadata, declared companion
programs and upstream license notices; independent validation checks actual executable
entry points and platform identity. The upload submission also carries its Android WASM
artifact under the same plugin ID. Source support for macOS is not evidence that a macOS
artifact has been validated or published. The isolated Echo demo is not a functional test
of either first-party plugin, and successful local validation is not a live PR or deployment.

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
| Catalog signature (Ed25519, key compiled in) | a catalog the maintainer did not sign, or one edited after signing |
| Rollback floor (kept on disk) | an older, validly signed catalog, also after a restart |
| Archive size + SHA-256 pinned in the signed entry | any other archive, including a well-formed one |
| Binding: catalog ↔ manifest ↔ compiled-in record | an edited manifest author or link, even if the catalog was made to agree with the edit |
| Isolation gate | a legacy process package |
| Same installer as a manual install | links, devices, oversized or unsafe archives; the digest check before install |

**What this does not prove.** The signature and the checks prove *who approved exactly these bytes*. They do not make an
extension harmless. An approved extension still runs only as safely as the process boundary it runs behind, and it
starts with nothing allowed. A maintainer who approves a malicious package approves it; the pipeline protects the
identity and provenance of what is approved, not the judgement of the person approving.

An **update** replaces the installed extension and forgets what the old version was allowed to do. This is the existing
replacement behaviour, and the Store says so next to the button.

## Maintainer action (one time)

The application cannot sign and holds no key. **Until these are done every build reports the store as "not set up"
and refuses every store command** — there is no fallback that trusts an unsigned catalog.

1. Make the key, on a trusted machine:
   `cargo xtask catalog-keygen --out ~/peersus-catalog-seed` — writes the seed (mode 0600, never printed) and prints
   the **public** key as a Rust array.
2. Commit that array as `MAINTAINER_PUBLIC_KEY` in `crates/peersus-app/src/store_trust.rs`. A public key is not secret.
3. Store the seed's contents as the secret `CATALOG_SIGNING_KEY` of the protected environment `extension-approval`,
   and require yourself as its reviewer. Keep an offline copy. Anyone who obtains it can sign a catalog the app trusts.
4. Confirm the address the app fetches from: `STORE_BASE` in `crates/peersus-store-fetch/src/lib.rs`. It is currently
   an assumption (`https://darkphilosophy.github.io/Peersus/store/`); at the time of writing that address and the
   repository page both answered 404 from the build environment.
5. Publish when you choose to. Download the `signed-catalog` artifact the approval workflow produced, then stage the site
   with it (this validates the catalog by the application's own rules and refuses tampered or malformed ones):

   ```
   python3 site/build.py --output /new/empty/dir --catalog /path/to/signed-catalog
   ```

   The result is a static folder: the site plus `store/catalog.json`, `store/catalog.sig` and the archives. Uploading it is
   the deploy step, which nothing in this repository does for you. The site's page lists what the catalog says; it is
   display only. The application verifies the signature itself and never trusts the page. To keep earlier extensions
   listed, the approval workflow signs on top of the previous catalog, which it reads as TRUSTED data from the default
   branch: after each approval, **commit the produced `catalog.json` and the archives to `catalog/` on the default
   branch** (`catalog.sig` is not committed: it is regenerated). Without that file every approval would start a catalog of
   one entry at sequence 1, which the application refuses as a rollback. This step is yours; nothing automates it.

### Website data and offline operation

The browser fetches the staged, same-origin `store/catalog.json` (or explicitly
marked `store-test/catalog.json` in development). It does not query the GitHub API.
When served by GitHub Pages those bytes come from the last deployed site snapshot;
approving an extension does not independently update a deployed preview.

A local HTTP server can serve the complete staged folder, including its catalog,
without internet access. Opening HTML directly through `file://` renders the static
page but does not reliably permit the catalog fetch; the tested browser shows the
catalog-unavailable message. There is no service worker or guaranteed cold-start
offline cache. A remote page cannot obtain newer catalog data while disconnected.

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
