# Application updates

How Peersus replaces itself with a newer build, what it trusts while doing so, and what has and has not been observed running. Extensions are updated by the [extension store](extension-store.md); this page is about the application only.

## What exists

| Part | Where | Role |
|---|---|---|
| Decision and verification | `crates/peersus-app/src/app_update.rs` | Signature, channel, rollback floor, artifact choice. Never fetches, installs or restarts. |
| Scheduling | `crates/peersus-app/src/runtime.rs` (`automatic_update`) | One step at a time: restart, else install, else check (every six hours). |
| Desktop installer | `crates/peersus-app/src/update_install.rs` | Replaces the folder of programs the owner runs from. |
| Android installer | `crates/peersus-ui-android/src/update.rs` and each distribution's `AppUpdate.java` | Hands one package to Android, or says the store keeps the installation current. |
| Publisher tools | `cargo xtask update-keygen`, `cargo xtask update-sign` | Make the key; describe and sign one release. Nothing is uploaded by them. |

## Trust

- One Ed25519 key, `UPDATE_PUBLIC_KEY`, separate from the extension store's key. What is signed begins with the context line `peersus-app-update-v1`, so a signature of an extension catalog is never a signature of an application release.
- A product build has no key (`None`): updates report "not set up", every command refuses and the interface shows no Updates section. There is no fallback.
- A development build made with `--private-store` verifies the `test` channel with a development key whose seed is a local, ignored file. That key signs nothing a product build accepts.
- The channel is inside the signed document. A release signed for `test` is refused on `stable`.
- Rollback: a release is offered only when its `(version, build)` is newer than the running build. The newest release ever verified is kept in `update-floor` in the application's data folder; an older one is refused, and so is a different document for the same version and build.
- The release listing and the download location are not trusted. Every artifact is checked against the size and SHA-256 the signed document pins before any installer sees it.

## A release

One GitHub release per channel, under the tag `app/<channel>`, holding:

- `update.json`: `schema` (1), `channel`, `version`, `build`, optional `notes`, and `artifacts`, each with `platform`, `architecture` (or `any`), `kind` (`archive`, `image` or `package`), `file`, `size`, `sha256`.
- `update.sig`: the 64-byte signature.
- every file the document names.

The build number is `PEERSUS_BUILD_NUMBER` at compile time (0 when unset). It distinguishes two builds of one project version without changing that version; on Android the package's `versionCode` is that number plus one, so Android itself also refuses an older build.

```sh
# Once: the publisher's key. The public half goes into UPDATE_PUBLIC_KEY.
cargo xtask update-keygen --out update-signing.key

# Per release: build with the new number, then describe and sign.
PEERSUS_BUILD_NUMBER=2 ./build.sh desktop
tar -czf peersus-linux-x86_64.tar.gz -C <folder of programs> \
    peersus peersus-ui peersus-wasm-host peersus-native-host peersus-store-fetch
APP_UPDATE_SIGNING_KEY="$(<update-signing.key)" cargo xtask update-sign \
    --channel test --version 0.1.0 --build 2 --out release/ \
    --artifact linux:x86_64:archive:peersus-linux-x86_64.tar.gz \
    --artifact android:aarch64:package:app-community-arm64-v8a.apk
```

Publishing the files in `release/` under the tag is a separate, deliberate step. No build, test or tool in this repository creates a tag or a release.

The transport carries one artifact in one piece, at most 128 MiB, within the store helper's existing deadline (60 s on desktop, 75 s on Android). Measured on 2026-11-11: the development Android package with both ABIs is 96 MiB and fits; an archive of the five unoptimized desktop programs with their debugging symbols is 378 MiB and does not, the same programs stripped are 50 MiB. Strip the copies that go into an archive, or archive an optimized build.

## What the person controls

Two switches in Device → Updates, both off by default, also the start options `peersus --update-automatically[=false]` and `--restart-after-update[=false]`:

- **Update automatically**: a verified update is downloaded and put in place without asking. Off: the section says a version is available and waits for "Update now".
- **Restart after an update**: the application starts again by itself once an update is in place, never while a transfer or a pairing is under way. Off: "Restart now" is offered instead.

Each extension keeps its own automatic-update switch in the store; an update of an extension resets what it was allowed.

Commands, with the application running: `peersus update`, `update-check`, `update-install`, `update-restart`.

## Per platform

| Installation | What happens | State |
|---|---|---|
| Linux, a writable folder of programs | The archive must be plain files and include the owner. Each file is written beside the one it replaces and moved over it once all are on disk. Running programs keep running; the owner then re-executes itself and the window follows it. | Observed on 2026-11-11 from a published `app/test` release, on a development instance: build 1 found build 2, downloaded and verified it (installed after 7 s), all five programs on disk were the published ones; with "Restart after an update" turned on the owner was back after 6 s and both the owner and the window were running the new programs, extensions restarted, paired devices online. |
| Linux, Flatpak or AppImage, or a folder the user cannot write | Says who keeps the installation current. No switches, nothing offered. | Unit tests. |
| Windows, macOS | "The application cannot replace itself on this system yet". Replacing a running program needs a different mechanism there and none is written. | Not implemented. |
| Android, community distribution | The verified package is streamed to `pm install -r --user <user> --pkg <this application> -S <size>` by a shell-mode Stellar/Shizuku worker: Android asks nothing, and still refuses another package name, another signer or an older `versionCode`. Replacing the package ends the application; with "Restart after an update" on, the same shell command opens it again. Without a running provider, or before the provider allowed Peersus, nothing is installed and the section says which. | Observed on 2026-11-11 on one phone (Android 16, Stellar in shell mode, already allowed) with both switches on: build 1 downloaded build 2 (96 MiB in 8 s), the package was replaced (`versionCode` 2 to 3) and the window was open again on build 2 about 11 s after the switches were turned on. The refusals (no provider, not allowed, another signer) were not provoked from the application; that a package of another name is refused was seen with the same command run by hand. Downloads on Android take turns with the store's (one worker, one download at a time), so a check made while the store refreshes waits for it: about 8 s at start. |
| Android, Play distribution | Never replaces itself. Play's in-app update question is shown once per available version while the window is open; Play downloads, verifies and installs. The Updates section says the installation is updated by Google Play. | Compiles. Cannot be observed without a Play listing. |

Independently of these switches, Android brings the background service back after any update when the person turned background running on (`BootStart`).

## Not done

- No production key exists, so no product build updates itself.
- Windows and macOS installers.
- AppImage replacement of `$APPIMAGE`.
- Artifacts above the transport bound, resumable or streamed downloads.
- The owner's reasons for a failed update are shown in English only.
