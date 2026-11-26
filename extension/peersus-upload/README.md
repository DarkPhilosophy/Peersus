# Upload extension

One extension identity, one source: the official WASM guest, plus an optional native library and process (not recommended). On Send, choose one file and approve sending its name and contents to one destination: poo.wang (with a saved API key or anonymously) or Catbox (always anonymous). Only the connection to the provider is encrypted; the provider receives the file as it is and the result link may be public.

## Platforms and prerequisites

- The application's trusted file-selection, HTTPS, encrypted credential configuration and result-presentation services (`peersus.files`, `peersus.http`, `peersus.actions`). The guest receives opaque file and authorization handles, not ambient filesystem access or credential bytes. It validates provider success and its canonical single-file result before requesting presentation.
- Network uploads require HTTPS access to the configured provider. Tests below use synthetic data and do not require an account, a credential or a real upload.

The source is Apache-2.0 licensed; retain LICENSE and NOTICE in exported submissions. `submission.toml` declares build commands; CI resolves and records the trusted host SDK commit. The guest has its own lockfile (`wasm/Cargo.lock`) and depends on no application crate. Host and guest must be updated together when their service contract changes; an older host without `peersus.http.present` is not compatible with the updated guest.

The credential action declares `input: "secret_text"`: the host renders a masked field directly, with the same submit operation for Enter and the trailing Enter icon. The value answers a host-owned credential request and is never included in guest action arguments. Update the host and the WASM package together; older hosts cannot interpret this input declaration.

## Destinations

**poo.wang (optional API key).** Use **API key…** in this extension's settings to save or remove a key. On Send, **poo.wang (API key)** means a saved key will be used; **poo.wang (anonymous)** is offered only when the host confirms no saved key. If the host cannot determine key state, neither row is offered and **Destinations** explains why; use **Refresh key state** there after fixing the store. An out-of-date row never silently changes the credential mode. Before a keyed upload, after saving a key, and when you press **Check now**, the host checks the bearer at `https://poo.wang/api/me`. A 401 or 403 stops a keyed upload before file selection or consent; a rejected key never falls back to anonymous. An inconclusive provider check still permits the keyed attempt.

The host must confirm a save or removal before any follow-up key check. If a save has already completed, cancelling stops remaining work but does not undo that save. While another action is pending, passive settings redraws can use the last page; opening settings or pressing **Check now** reports that the extension is busy rather than claiming to have refreshed.

After consent, the host sends the file as a multipart `files` field to `https://poo.wang/api/upload`. For files over 3 MiB it starts a session at `https://poo.wang/api/upload/session` with the file name and size. Only the upload or session start carries the saved bearer key, or the keyless opt-in below. The `/api/upload/session/part` and `/api/upload/session/complete` requests use opaque session authorization, not the saved API key. The extension limits a keyed upload to 250 MiB; the service also enforces its account limits. Both modes request a 24-hour expiry, never publish to Showcase, and require one result link matching `https://poo.wang/f/<id>`.

**Keyless poo.wang.** Only a confirmed absence of a saved key enables this mode; an unreadable key state stops the upload. The upload or session start carries `X-Poo-Upload-Client: native` and no `Origin`, `Cookie`, or `Authorization`. This requires the service's native anonymous support; it is not a browser session. The limit is **25 MiB per file** and **10 admitted files per UTC day per connecting network address**, including admissions for uploads that do not finish. Links expire after **24 hours**.

From the command line, use `peersus run-file upload upload <file>` for the keyed action or `peersus run-file upload upload_keyless <file>` for the anonymous action. Only the action offered for the current key state is accepted.

**Catbox (anonymous).** The Catbox action never asks about, reads or sends a saved API key, even if a poo.wang key is saved. Selecting Catbox as the only destination also skips poo.wang key lookups in startup and settings. After consent, the host sends a multipart POST to `https://catbox.moe/user/api.php` with `reqtype=fileupload` and the file in `fileToUpload`, without `userhash` or a bearer. Catbox allows uploads up to 200 MB, but the app permits only 6 MiB per request part and Catbox does not accept split uploads. Larger files are refused before a snapshot or consent. The host must identify its HTTPS client: Catbox can close the connection before returning an HTTP response when `User-Agent` is absent. The extension cannot set that header.

Catbox replies with a plain-text link. The guest accepts only `https://files.catbox.moe/<one identifier segment>` with optional trailing line endings, without a query, fragment or other content. Consent names both the upload and result origins; the extension never sends file bytes to the result origin. Catbox's [FAQ](https://catbox.moe/faq.php) says anonymous uploads are removed after two years of inactivity.

Provider refusals (rate limit, file size, or another rejection) and malformed results never trigger a retry or show a link. A disabled destination, unavailable selection, or unknown key state stops the upload. Upload requires `files.select` and `network.http`; a missing local grant or a neutral refusal stops the attempt without retrying. **Destinations** distinguishes a refused key lookup or check from a provider that could not verify a key. A grant change refreshes readiness without repeating a key check or upload. Withdrawing the action or declining consent still reports cancellation. If the file was accepted and validated but its link could not be shown, the result says it was uploaded; it is not sent again.

Send shows the host's observed file payload progress for both destinations, regardless of which client starts the action. **Finishing…** means the HTTP client has consumed all the file bytes; the result still depends on the provider response and link presentation. A terminal action label clears after 15 seconds or on the next run.

If startup cannot read the configured destination or update the available actions, it reports the failure without proceeding with uploads or key changes. A missing or invalid destination is never treated as **Choose on Send**, and no key lookup is made. The next action or active settings request makes one new startup attempt; idle time and passive redraws never trigger a retry. No restart is needed after the problem is fixed.

## Execution format

The **WASM guest is the official build**. The store requires Linux, Android and macOS WASM packages; Windows WASM is optional and included by this recipe. One module, byte-identical on every platform, runs in an isolated interpreter that has no files, network, processes or OS calls of its own, only the application's brokered services the user granted, with bounded memory and work; a crash or hang ends only its worker. A native library (`.so`, second tier, not recommended) is only crash-isolated and runs with the application user's OS privileges; a process binary (third tier, not recommended) is an arbitrary program for which the application guarantees nothing beyond starting and stopping it.

The same guest source also builds as a native library (`cargo build --release --features native` in `wasm/`, packaged with `--native-library libpeersus_upload_wasm.so` as `upload-linux-native_library.tar.gz`) and, linked into the native host, as a process (`upload-linux-process.tar.gz`); every submission builds and validates both. They speak the identical protocol through the native host's function table instead of WebAssembly imports, so behaviour is the same; only the isolation is weaker. The store installs the WASM build only: the other two travel with the signed release for a person to install by hand, with limited support.

The manifest lists Linux, Android, macOS and Windows because this submission packages the WASM guest for all four. The shared library and process variants are Linux-only. The complete file-picker, credential and live upload workflow has not been verified here on macOS or Windows; a WASM archive is not evidence of end-to-end support.

## Build and test

In `wasm/`:

```sh
cargo build --release --target wasm32-unknown-unknown --locked --target-dir target
cargo test --locked
```

The guest's unit tests use scripted host answers to cover startup recovery, keyed and keyless request shapes, bounds, provider refusals, malformed results, failed presentation, and cancellation.

The application carries the tests that run the compiled guest itself. Run this from the **application workspace root**, pointing `guest` to the submission's `wasm/` directory. Use an absolute path if the submission is checked out separately, or `"$PWD/plugins/peersus-upload/wasm"` for the local working copy:

```sh
guest="$PWD/extension/peersus-upload/wasm"
PEERSUS_UPLOAD_WASM="$guest/target/wasm32-unknown-unknown/release/peersus_upload_wasm.wasm" \
PEERSUS_UPLOAD_MANIFEST="$guest/plugin.json" \
  cargo test -p peersus-plugin-runtime --test process \
  real_upload_guest_uses_host_services_and_returns_the_verified_link -- --ignored
```

That test executes the production guest through the actual worker and hub with synthetic host replies; `upload_wasm_pipeline.rs` in `peersus-app` (also `--ignored`, with `PEERSUS_WASM_HOST`) sends it through a local TLS server, whole and in parts. Neither opens a dialog or performs a live upload. Run real UI checks separately and only use a file and credential explicitly approved for that test. Never put credentials or personal test files in a submission.

## Submission

Export one source directory and one ownership identity. Include this README, the `wasm/` guest (manifest, sources and lockfile), `submission.toml`, `ownership.json` and license notices. Follow the extension branch's root guide for standalone CI and submission commands. Passing CI is not catalog publication: each update still needs ownership verification and independent maintainer approval.

## License and third-party notices

This extension is licensed under Apache-2.0 (`LICENSE`, `NOTICE`). The guest is built from these libraries: serde_json, serde_core, itoa, memchr and zmij (MIT OR Apache-2.0, MIT, or Unlicense OR MIT). The native library and the process are the same guest; the process also links the SDK's native host (`crates/peersus-plugin-runtime/native/host.c`, Apache-2.0).
Every packaged archive carries `LICENSE`, `NOTICE`, `THIRD_PARTY_NOTICES.txt` and the license texts of its exact resolved dependency set under `licenses/`, generated by the packager from the lockfile; keep them when redistributing.
