# Upload extension

One extension identity, one source: a WASM guest (official) for Linux, Android and macOS; the same source also builds a native library and a process (not recommended). Every upload requires an explicitly selected file and destination approval. There are two destinations, chosen per file on Send: the account's provider, whose API key the person saves, and Catbox, which needs no account and no key. Uploading sends plaintext content to the approved external provider and may create a public link; peer-to-peer transport encryption does not protect a file after that upload.

## Platforms and prerequisites

- The application's trusted file-selection, HTTPS, encrypted credential configuration and result-presentation services (`peersus.files`, `peersus.http`, `peersus.actions`). The guest receives opaque file and authorization handles, not ambient filesystem access or credential bytes. It validates provider success and its canonical single-file result before requesting presentation.
- Network uploads require HTTPS access to the configured provider. Tests below use synthetic data and do not require an account, a credential or a real upload.

The source is Apache-2.0 licensed; retain LICENSE and NOTICE in exported submissions. `submission.toml` declares build commands; CI resolves and records the trusted host SDK commit. The guest has its own lockfile (`wasm/Cargo.lock`) and depends on no application crate. Host and guest must be updated together when their service contract changes; an older host without `peersus.http.present` is not compatible with the updated guest.

The credential action declares `input: "secret_text"`: the host renders a masked field directly, with the same submit operation for Enter and the trailing Enter icon. The value answers a host-owned credential request and is never included in guest action arguments. Update the host and the WASM package together; older hosts cannot interpret this input declaration.

After a key is saved, the guest asks the host's `peersus.http.check` to read it back against the provider's identity route and reports one of three outcomes: `valid` (a success status), `rejected` (the provider answered 401 or 403, which fails the action so the person is told at once) or `unverified` (anything else, including a host that cannot make the check). Only the status crosses back to the guest; neither the key nor the provider's response body does. A host without `peersus.http.check` reports `unverified`, never `valid`.

The guest keeps the upload honest about the key:

- When it starts, and whenever its settings are opened or a key is saved or removed, it asks the application whether a usable key is saved (`peersus.http` `saved`: yes or no, nothing of the key) and takes "Upload selected file (poo.wang)" back while there is none (`peersus.actions` `withhold`). With no key that action is not offered anywhere; saving one offers it again. If the application will not say (the extension is not allowed the network yet), the action stays and a press says what is missing.
- When an upload to that destination starts it asks the provider about the key first (`check`, as above). A key the provider refuses ends the upload before the file is prepared or the person is asked to allow anything; a provider that could not say does not stop it.
- Its settings show one line, drawn by the application from what the guest answers (a page placed in Settings): no key saved; a key saved and not yet asked about; accepted; refused. "Check now" asks the provider. The line asks to be drawn again every two seconds while it is shown, and drawing asks nobody. The same line names the destination that needs no key, since that is where a person looks for destinations.

The anonymous destination is offered beside the keyed one, always, and touches none of the above: no key is asked about, read or sent. "Upload selected file (Catbox, anonymous)" sends one file to `https://catbox.moe/user/api.php` (`reqtype=fileupload`, the file as `fileToUpload`) in a single multipart request, and the destination answers with plain text: the public link. The guest reads that answer as text (`peersus.http` `request`, `response: "text"`), accepts only `https://files.catbox.moe/<one identifier segment>` and shows it with `peersus.http` `present`; anything else — another address, another path, a query, a fragment, a second line — is refused before anything is presented. That link address is a different origin from the one the file is sent to, so the guest names it when it asks to send (`authorize`, `result_origin`): the person is shown both addresses in the one approval, requests still go to `https://catbox.moe` only, and a request can never be sent to the address approved for the result.

Catbox takes one file of up to 200 MB, but this application reads at most 6 MiB for one request part and does not split a file for a destination that has no part protocol. The guest therefore asks the application for a selection no larger than 6 MiB and says so before anything is sent: a larger file is refused at the selection, before a snapshot, a question or a byte leaves. It is never sent as if part of it were the whole file.

An application that does not know `anonymous`, `result_origin` or `response: "text"` cannot serve the anonymous destination: the old host asks for a key before the consent (so the action fails and says what is missing) and parses the plain-text answer as JSON (so the upload fails with "Invalid JSON response"). Neither ever sends a key to Catbox. Update the application and this package together.

An application without `peersus.http` `saved` or `peersus.actions` does not start this guest: update the application and this package together.

A file larger than one request is sent in parts that carry a session header of the provider's protocol, at the keyed destination only (Catbox has no such protocol). The guest names that header when it asks to send the file (`peersus.http` `authorize`, `headers`), the person is shown it with the file and the address, and the application then lets requests of that authorization carry it. An application older than this refuses the first part of a large file with "Unsupported request header" after nothing of it was sent; a small file is unaffected. Update the application and this package together.

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
