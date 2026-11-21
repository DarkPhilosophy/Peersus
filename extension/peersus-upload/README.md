# Upload extension

One extension identity, one source: a WASM guest (official) for Linux, Android and macOS; the same source also builds a native library and a process (not recommended). On Send, choose one file and approve sending its name and contents to one destination: poo.wang with a saved API key, or Catbox without one. Only the connection to the provider is encrypted; the provider receives the file as it is and the result link may be public.

## Platforms and prerequisites

- The application's trusted file-selection, HTTPS, encrypted credential configuration and result-presentation services (`peersus.files`, `peersus.http`, `peersus.actions`). The guest receives opaque file and authorization handles, not ambient filesystem access or credential bytes. It validates provider success and its canonical single-file result before requesting presentation.
- Network uploads require HTTPS access to the configured provider. Tests below use synthetic data and do not require an account, a credential or a real upload.

The source is Apache-2.0 licensed; retain LICENSE and NOTICE in exported submissions. `submission.toml` declares build commands; CI resolves and records the trusted host SDK commit. The guest has its own lockfile (`wasm/Cargo.lock`) and depends on no application crate. Host and guest must be updated together when their service contract changes; an older host without `peersus.http.present` is not compatible with the updated guest.

The credential action declares `input: "secret_text"`: the host renders a masked field directly, with the same submit operation for Enter and the trailing Enter icon. The value answers a host-owned credential request and is never included in guest action arguments. Update the host and the WASM package together; older hosts cannot interpret this input declaration.

## Destinations

**poo.wang (API key required).** Use **API key…** in this extension's settings to save a key. On startup and when these settings are opened or a key is changed, the host reports whether a usable key is saved; the guest hides the poo.wang action when the answer is no. Saving a key, starting a poo.wang upload, or clicking **Check now** asks the host to send the bearer key to `https://poo.wang/api/me`. Only the HTTP status reaches the guest. A 401 or 403 before upload means the key was rejected and stops the upload before a file snapshot or consent; an inconclusive check does not block the attempt.

After consent, the host sends the file as a multipart `files` field to `https://poo.wang/api/upload` with the bearer key. For files over 3 MiB, it starts a session at `https://poo.wang/api/upload/session` with the key and file name and size. The requests under `/api/upload/session/part` and `/api/upload/session/complete` use opaque session authorization, not the saved API key. The app limits the selected file to 250 MiB and the service also enforces the account's own limits. Both paths request expiration after 24 hours and do not publish to Showcase. The result must be one link matching `https://poo.wang/f/<id>`.

The poo.wang website supports anonymous uploads up to 25 MiB per file and 10 per day per anonymous session, with the default link expiry of 24 hours. Its upload and session-start API routes reject an anonymous request without an approved browser `Origin`, and the host cannot supply that header or browser cookies. This extension does not attempt keyless uploads to poo.wang.

**Catbox (anonymous).** The Catbox action never asks about, reads or sends a saved API key, even if a poo.wang key is saved. Selecting Catbox as the only destination also skips poo.wang key lookups in startup and settings. After consent, the host sends a multipart POST to `https://catbox.moe/user/api.php` with `reqtype=fileupload` and the file in `fileToUpload`, without `userhash` or a bearer. Catbox allows uploads up to 200 MB, but the app permits only 6 MiB per request part and Catbox does not accept split uploads. Larger files are refused before a snapshot or consent. The host must identify its HTTPS client: Catbox can close the connection before returning an HTTP response when `User-Agent` is absent. The extension cannot set that header.

Catbox replies with a plain-text link. The guest accepts only `https://files.catbox.moe/<one identifier segment>` with optional trailing line endings, without a query, fragment or other content. Consent names both the upload and result origins; the extension never sends file bytes to the result origin. Catbox's [FAQ](https://catbox.moe/faq.php) says anonymous uploads are removed after two years of inactivity.

Provider refusals and malformed results never trigger a retry or show a link. A disabled destination, missing selection, missing or rejected poo.wang key, or disallowed file size reports the upload as unavailable. A missing host permission still reports a permission denial, and a cancelled selection or consent reports cancellation.

## Execution format

The **WASM guest is the official build** of this extension and is packaged for Linux, Android and macOS; the store refuses a submission without it. Why WASM: one module, byte-identical on every platform, runs in an isolated interpreter that has no files, network, processes or OS calls of its own, only the application's brokered services the user granted, with bounded memory and work; a crash or hang ends only its worker. A native library (`.so`, second tier, not recommended) is only crash-isolated and runs with the application user's OS privileges; a process binary (third tier, not recommended) is an arbitrary program for which the application guarantees nothing beyond starting and stopping it.

The same guest source also builds as a native library (`cargo build --release --features native` in `wasm/`, packaged with `--native-library libpeersus_upload_wasm.so` as `upload-linux-native_library.tar.gz`) and, linked into the native host, as a process (`upload-linux-process.tar.gz`); every submission builds and validates both. They speak the identical protocol through the native host's function table instead of WebAssembly imports, so behaviour is the same; only the isolation is weaker. The store installs the WASM build only: the other two travel with the signed release for a person to install by hand, with limited support.

The portable WASM module is packaged for Linux, Android, macOS and Windows. This recipe's shared library and process variants are Linux-only. Packaging a Windows or macOS archive is not a test of the file picker, credentials or upload on that operating system.

## Build and test

In `wasm/`:

```sh
cargo build --release --target wasm32-unknown-unknown --locked
cargo test --locked
```

The guest's unit tests drive its protocol with scripted host answers: malformed results, snapshot and authorization ownership, bounds, the key's three outcomes, parts and cancellation.

The application carries the tests that run the compiled guest itself. From the application workspace root, with the guest built as above:

```sh
PEERSUS_UPLOAD_WASM="$PWD/plugins/peersus-upload/wasm/target/wasm32-unknown-unknown/release/peersus_upload_wasm.wasm" \
PEERSUS_UPLOAD_MANIFEST="$PWD/plugins/peersus-upload/wasm/plugin.json" \
  cargo test -p peersus-plugin-runtime --test process \
  real_upload_guest_uses_host_services_and_returns_the_verified_link -- --ignored
```

That test executes the production guest through the actual worker and hub with synthetic host replies; `upload_wasm_pipeline.rs` in `peersus-app` (also `--ignored`, with `PEERSUS_WASM_HOST`) sends it through a local TLS server, whole and in parts. Neither opens a dialog or performs a live upload. Run real UI checks separately and only use a file and credential explicitly approved for that test. Never put credentials or personal test files in a submission.

## Submission

Export one source directory and one ownership identity. Include this README, the `wasm/` guest (manifest, sources and lockfile), `submission.toml`, `ownership.json` and license notices. Follow the extension branch's root guide for standalone CI and submission commands. Passing CI is not catalog publication: each update still needs ownership verification and independent maintainer approval.

## License and third-party notices

This extension is licensed under Apache-2.0 (`LICENSE`, `NOTICE`). The guest is built from these libraries: serde_json, serde_core, itoa, memchr and zmij (MIT OR Apache-2.0, MIT, or Unlicense OR MIT). The native library and the process are the same guest; the process also links the SDK's native host (`crates/peersus-plugin-runtime/native/host.c`, Apache-2.0).
Every packaged archive carries `LICENSE`, `NOTICE`, `THIRD_PARTY_NOTICES.txt` and the license texts of its exact resolved dependency set under `licenses/`, generated by the packager from the lockfile; keep them when redistributing.
