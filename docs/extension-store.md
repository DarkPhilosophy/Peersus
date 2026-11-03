# Extension store

How an extension gets from its author to a person's device, what each step proves, and what it does **not** prove.
Everything below is implemented and tested in this repository except the two steps marked **maintainer action**, which
need something only the maintainer can do (a signing key, a public address).

## The chain

```
author ──PR──▶ extension branch ──CI check──▶ maintainer approves ──▶ signed catalog ──▶ Pages ──▶ the app
        (untrusted)   (no secrets)             (protected workflow)      (Ed25519)      (static)   (one click)
```

1. **Submit.** The author opens a pull request against the `extension` branch adding one file:
   `extensions/<name>.tar.gz`. The pull request may change nothing else.
2. **Check** (`.github/workflows/extension-validate.yml`). Runs on `pull_request`, never `pull_request_target`, with a
   read-only token and no secrets. The validator comes from the **base** branch, so a pull request cannot weaken the
   rule that judges it. It reads the archive and never runs anything in it. It prints a review report with the
   archive's SHA-256 — the value the maintainer approves.
3. **Approve** (`.github/workflows/extension-approve.yml`). Manual (`workflow_dispatch`), inside the protected
   `extension-approval` environment, which holds the signing key and requires the maintainer's approval. The
   maintainer gives the commit and the SHA-256 they reviewed; the job refuses if the file at that commit hashes to
   anything else. All code it runs comes from the default branch; the submission is data.
4. **Publish.** The signed `catalog.json` + `catalog.sig` and the approved archives are served as static files
   (GitHub Pages). Publishing them is a separate, deliberate step that is **not automated here**.
5. **Install.** In the app, *Settings → Extensions → Store*: search, then one click. The app downloads through a
   separate helper process, verifies, and installs through the same installer as a manual install. The extension starts
   with **every permission refused**.

## What a package must be

`extensions/<name>.tar.gz`, containing one folder `<name>/` with:

| File | Rule |
|---|---|
| `plugin.json` | JSON only for the store. A `package` block with `name`, `version` (`major.minor.patch`), `author`, `link` (https, no credentials, query or fragment) and optionally `description`, `changelog`. |
| `execution` in `plugin.json` | `{"kind": "native_library" or "wasm", "abi": 1}`. A legacy `process` package is **refused**: it would run with the user's own rights. |
| the program | The file `program` names. Must carry the **embedded record** below. |

**The id is the folder name**, never a field of the package. The host assigns it; the catalog pins it; a mismatch is refused.

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

## Limits, stated plainly

* **Android** has the install machinery, but the Android entry does not attach a store, so it reports "not set up" and
  refuses store commands. Wiring it needs a way to download that fits Android's process rules (the desktop path runs a
  separate helper program beside the application), which has not been designed or tested. Manual install on Android is unchanged.
* **Windows and AppImage**: the store and the helper are Unix-only today (`archive` and the package manager use Unix
  file semantics), and no AppImage or Windows packaging exists in this repository. Nothing here claims they work.
* **NixOS and similar**: the download helper runs with an **empty environment** on purpose (no proxy variables, no
  credentials). Systems that need `SSL_CERT_FILE`-style variables to find certificates would need the helper to be
  given exactly those, not the whole environment.
* The embedded-record tool in `scripts/validate-extension.py` reads ELF and WebAssembly. The host also reads PE and
  Mach-O; CI does not yet validate those formats, so a package in them is refused by the CI check until it does.
* The signing key does not exist yet. The fixtures under `crates/peersus-app/tests/fixtures/catalog` are signed with a
  throwaway test seed and carry no project key.
