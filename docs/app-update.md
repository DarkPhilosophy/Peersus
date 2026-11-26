# Application updates

How Peersus replaces itself with a newer build, what it trusts while doing so, and what has and has not been observed running. Extensions are updated by the [extension store](extension-store.md); this page is about the application only.

## What exists

| Part | Where | Role |
|---|---|---|
| Decision and verification | `crates/peersus-app/src/app_update.rs` | Signature, channel, rollback floor, release names, artifact choice. Never fetches, installs or restarts. |
| Scheduling | `crates/peersus-app/src/runtime.rs` (`automatic_update`) | One step at a time: restart, else install, else check (every six hours). |
| Desktop installer | `crates/peersus-app/src/update_install.rs` | Replaces the folder of programs the owner runs from. |
| Android installer | `crates/peersus-ui-android/src/update.rs` and each distribution's `AppUpdate.java` | Hands one package to Android, or says the store keeps the installation current. |
| Publisher tools | `cargo xtask update-keygen`, `cargo xtask update-sign` | Make the key; name and sign the files of one release. Nothing is uploaded by them. |

## Trust

- One Ed25519 key, `UPDATE_PUBLIC_KEY`, separate from the extension store's key. What is signed begins with the context line `peersus-sums-v1`, so a signature of an extension catalog is never a signature of an application release.
- A product build has no key (`None`): updates report "not set up", every command refuses and the interface shows no Updates section. There is no fallback.
- A development build made with `--private-store` follows the `dev` channel, verified with a development key whose seed is a local, ignored file. That key signs nothing a product build accepts.
- A private build made with `PEERSUS_UPDATE_CHANNEL=pre` as well follows the `pre` channel (prereleases for testers), verified with its own prerelease key; its seed is the local, ignored `app-update-pre.key`. A development release never verifies on a prerelease build, nor the other way round.
- The channel is in the signed names and fixed when the build is made. A release of `pre` is refused by a build that follows `dev`, even with a valid signature. Moving a device to another channel means installing that channel's build; the application never switches channel itself.
- Three stages: `stable` (a release, once a product key exists), `pre` (a prerelease) and `dev` (development builds, mostly trials). All keep the version `0.1.0`; a build is told apart by its build number, commit and artifact digests, not by a new version.
- Rollback: a release is offered only when its `(version, build)` is newer than the running build. The newest release ever verified is kept in `update-floor` in the application's data folder; an older one is refused, and so is a different list for the same version and build.
- Tags, the release listing and the download location are not trusted: they only say where to look. Every artifact is checked against the SHA-256 the signed list gives before any installer sees it; the size the listing gives only bounds the download.
- Release notes are not signed and are not shown by the application.

## A release

One GitHub release per build, under the tag `v<version>-<channel>.<build>` (for example `v0.1.0-dev.42`), created by CI. The application lists the repository's tags that begin with `v` (`git/matching-refs/tags/v`), takes the newest of its own channel, and reads that release. A tag whose release is not published yet is not an update. The release holds:

- every artifact, named `peersus-<version>-<channel>.<build>-<platform>-<architecture>.<extension>`. The extension says how it is applied: `.tar.gz` a folder of programs, `.AppImage` one file, `.apk` a package. `<architecture>` may be `any`.
- `SHA256SUMS`, in the form `sha256sum` reads: one `<hex>  <file>` line per artifact, and one `<hex>  <archive name without .tar.gz>/<program>` line per program in an archive. These names are the only place the version, channel and build are written; every artifact must name the same ones.
- `SHA256SUMS.sig`: the 64-byte signature over the context line and the list.

The first installation (`site/install.sh`) checks the same list with `openssl` and `sha256sum` before anything of the application runs; the website stages the newest development release's list beside itself for it and for the download page.

The build number is `PEERSUS_BUILD_NUMBER` at compile time (0 when unset); a release's build number is at least 1. It distinguishes two builds of one project version without changing that version; on Android the package's `versionCode` is that number plus one, so Android itself also refuses an older build.

```sh
# Once: the publisher's key. The public half goes into UPDATE_PUBLIC_KEY.
cargo xtask update-keygen --out update-signing.key

# Per release: build with the new number, then name and sign.
PEERSUS_BUILD_NUMBER=2 ./build.sh desktop
tar -czf programs.tar.gz -C <folder of programs> \
    peersus peersus-ui peersus-wasm-host peersus-wasm-canvas-host peersus-native-host peersus-store-fetch peersus-diag
APP_UPDATE_SIGNING_KEY="$(<update-signing.key)" cargo xtask update-sign \
    --channel dev --version 0.1.0 --build 2 --out release/ \
    --artifact linux:x86_64:programs.tar.gz \
    --artifact android:aarch64:app-community-arm64-v8a.apk
```

`release/` then holds the renamed artifacts, `SHA256SUMS` and `SHA256SUMS.sig`.

### The development release workflow

`.github/workflows/release.yml` (Actions → Run workflow, choosing the channel `dev` or `pre` and a build number greater than every published build of any channel) does the steps above in CI and publishes the GitHub prerelease `v<version>-<channel>.<build>`; nothing runs on push, tag or schedule. One build sequence serves every channel because Android takes the build number as the package version (`versionCode` is the build plus one) and installs a package only over a lower one. It builds the optimized desktop programs and the Android package with `PEERSUS_PRIVATE_STORE_BUILD=1` and `PEERSUS_UPDATE_CHANNEL` set to the channel, so what it publishes verifies its updates with that channel's key, archives the stripped programs, optionally builds an AppImage (when the runtime's published SHA-256 is given), signs with `cargo xtask update-sign` and runs `gh release create`.

Three repository secrets hold the keys, and nothing else reads them:

- `APP_UPDATE_SIGNING_KEY`: the development update seed (base64 of 32 bytes), the same seed as the local, ignored `app-update-test.key`.
- `APP_UPDATE_SIGNING_KEY_PRE`: the prerelease update seed, the same seed as the local, ignored `app-update-pre.key`.
- `ANDROID_SIGNING_KEYSTORE`: the fixed Android signing key, a base64 keystore. It is the key Gradle's development signer uses on the publisher's workstation (`~/.android/debug.keystore`, alias `androiddebugkey`), so a package from `./dev.sh deploy` and one from a release have the same signer and Android accepts either as an update of the other, without removing the application or its data. The workflow puts it where Gradle looks, checks the package's certificate (SHA-256 `e8002af8…44e250`) and deletes it again.

A build made by `./dev.sh deploy` without `--private-store` follows `stable`, which has no key yet: it never updates itself. Deploy with `--private-store` to put a development-channel build on a device.

The transport carries one artifact in one piece, at most 128 MiB, within the store helper's existing deadline (60 s on desktop, 75 s on Android). Measured on 2026-11-11: the development Android package with both ABIs was 96 MiB and fit; on 2026-11-26 it was past 128 MiB, so the release workflow publishes one package per ABI (`android-aarch64`, `android-x86_64`, built with `PEERSUS_ANDROID_ABI_SPLIT=1`) and each device takes its own architecture's; an archive of the five unoptimized desktop programs with their debugging symbols is 378 MiB and does not, the same programs stripped are 50 MiB. Strip the copies that go into an archive, or archive an optimized build.

## Putting damaged programs back

An installation that came from a release keeps that release's `SHA256SUMS` and `SHA256SUMS.sig` beside its programs (`lib/peersus/` on Linux). They are written by the first installation (`site/install.sh` hands them to `peersus install`, which checks every program against them first) and by every in-app update (which also refuses an archive that holds anything other than exactly the programs the list signs, each with its signed digest). A development deploy writes no list and removes one an earlier installation left, so local programs are never "repaired" back to a release.

Once per start, the owner reads every program in a background task and compares it with the list, after verifying the list as an update is verified. A program that is missing or holds other bytes makes it look for the release of the running build at once and put it back without waiting for the switches: this changes nothing but what was damaged, and needs no restart, because what runs is already that build. When a newer release is the newest one, it is offered as an ordinary update instead. The names of the damaged programs are written to the owner's log. A list this build cannot verify, or no list, means nothing is inspected.

What this does not cover: a missing or damaged owner cannot start to repair anything, and a missing window stops `peersus` from opening it (the message says to start `peersus owner` once, or to install again). Nothing here has been observed against a published release yet: unit tests cover the decisions, the installer and the inspection.

## What the person controls

Two switches in Device → Updates, both off by default, also available as flags before the desktop `owner` subcommand: `--update-automatically[=false]` and `--restart-after-update[=false]`:

- **Update automatically**: a verified update is downloaded and put in place without asking. Off: the section says a version is available and waits for "Update now".
- **Restart after an update**: the application starts again by itself once an update is in place, never while a transfer or a pairing is under way. Off: "Restart now" is offered instead.

Each extension keeps its own automatic-update switch in the store. A Store update of an extension that was installed from the Store keeps what it was allowed that the new version still asks for; any other replacement resets it (see [extension-store.md](extension-store.md#manual-download-versus-store-identity)).

Commands, with the application running: `peersus update`, `update-check`, `update-install`, `update-restart`.

## Per platform

| Installation | What happens | State |
|---|---|---|
| Linux, a writable folder of programs | The archive must be plain files and include the owner. Each file is written beside the one it replaces and moved over it once all are on disk. Running programs keep running; the owner then re-executes itself and the window follows it. | Observed on 2026-11-11 from a published release in the earlier format (one signed JSON document per channel), on a development instance: build 1 found build 2, downloaded and verified it (installed after 7 s), all five programs on disk were the published ones; with "Restart after an update" turned on the owner was back after 6 s and both the owner and the window were running the new programs, extensions restarted, paired devices online. The current format is covered by unit and integration tests; no release in it has been published yet. |
| Linux, Flatpak or AppImage, or a folder the user cannot write | Says who keeps the installation current. No switches, nothing offered. | Unit tests. |
| Windows, macOS | "The application cannot replace itself on this system yet". Replacing a running program needs a different mechanism there and none is written. | Not implemented. |
| Android, community distribution | The verified package is streamed to `pm install -r --user <user> --pkg <this application> -S <size>` by a shell-mode Stellar/Shizuku worker: Android asks nothing, and still refuses another package name, another signer or an older `versionCode`. Replacing the package ends the application; with "Restart after an update" on, the same shell command opens it again. Without a running provider, or before the provider allowed Peersus, nothing is installed and the section says which. | Observed on 2026-11-11 on one phone (Android 16, Stellar in shell mode, already allowed) with both switches on: build 1 downloaded build 2 (96 MiB in 8 s), the package was replaced (`versionCode` 2 to 3) and the window was open again on build 2 about 11 s after the switches were turned on. The refusals (no provider, not allowed, another signer) were not provoked from the application; that a package of another name is refused was seen with the same command run by hand. Downloads on Android take turns with the store's (one worker, one download at a time), so a check made while the store refreshes waits for it: about 8 s at start. |
| Android, Play distribution | Never replaces itself. Play's in-app update question is shown once per available version while the window is open; Play downloads, verifies and installs. The Updates section says the installation is updated by Google Play. | Compiles. Cannot be observed without a Play listing. |

On a Linux user installation, the updater resolves the caller's executable to `lib/peersus` and replaces programs there, leaving the `bin/peersus` symlink in place. Restart uses the explicit `owner` subcommand, with `--service` for a login-service owner.

This describes updates between builds using the current launch contract. Move development desktops from an older launch layout with `./dev.sh deploy`, not an in-app update; the old owner would restart using its old arguments.

Independently of these switches, Android brings the background service back after any update when the person turned background running on (`BootStart`).

## Not done

- No production key exists, so no product (`stable`) build updates itself; the development channel is published by the release workflow.
- Windows and macOS installers.
- AppImage replacement of `$APPIMAGE`.
- Artifacts above the transport bound, resumable or streamed downloads.
- The owner's reasons for a failed update are shown in English only.
- The Updates section shows a repair of the running build as an ordinary available version while it runs.
