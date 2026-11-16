# Upload extension

One extension identity with two execution variants: a native desktop process and an Android WASM guest. Both require an explicitly selected file and destination approval. Uploading sends plaintext content to the approved external provider and may create a public link; peer-to-peer transport encryption does not protect a file after that upload.

## Platforms and prerequisites

- Legacy native variant (not used by the Linux or Android WASM packages): a graphical session, `zenity`, a desktop file portal and a platform credential-store service. Building this variant on Linux does not certify macOS behavior.
- WASM (Linux and Android): the matching application's trusted file-selection, HTTPS, encrypted credential configuration and result-presentation services. The guest receives opaque file and authorization handles, not ambient filesystem access or credential bytes. It validates provider success and its canonical single-file result before requesting presentation.
- Network uploads require HTTPS access to the configured provider. Tests below use synthetic data and do not require an account, a credential or a real upload.

The source is Apache-2.0 licensed; retain LICENSE and NOTICE in exported submissions. `submission.toml` declares build commands; CI resolves and records the trusted host SDK commit and uses its workspace lockfile. The WASM guest also has its own lockfile. Host and guest must be updated together when their service contract changes; an older host without `peersus.http.present` is not compatible with the updated WASM guest.

The credential action declares `input: "secret_text"`: the host renders a masked field directly, with the same submit operation for Enter and the trailing Enter icon. The value answers a host-owned credential request and is never included in guest action arguments. Update the host and the WASM package together; older hosts cannot interpret this input declaration.

After a key is saved, the guest asks the host's `peersus.http.check` to read it back against the provider's identity route and reports one of three outcomes: `valid` (a success status), `rejected` (the provider answered 401 or 403, which fails the action so the person is told at once) or `unverified` (anything else, including a host that cannot make the check). Only the status crosses back to the guest; neither the key nor the provider's response body does. A host without `peersus.http.check` reports `unverified`, never `valid`.

The guest keeps the upload honest about the key:

- When it starts, and whenever its settings are opened or a key is saved or removed, it asks the application whether a usable key is saved (`peersus.http` `saved`: yes or no, nothing of the key) and takes "Upload selected file" back while there is none (`peersus.actions` `withhold`). With no key the action is not offered anywhere; saving one offers it again. If the application will not say (the extension is not allowed the network yet), the action stays and a press says what is missing.
- When an upload starts it asks the provider about the key first (`check`, as above). A key the provider refuses ends the upload before the file is prepared or the person is asked to allow anything; a provider that could not say does not stop it.
- Its settings show one line, drawn by the application from what the guest answers (a page placed in Settings): no key saved; a key saved and not yet asked about; accepted; refused. "Check now" asks the provider. The line asks to be drawn again every two seconds while it is shown, and drawing asks nobody.

An application without `peersus.http` `saved` or `peersus.actions` does not start this guest: update the application and this package together.

A file larger than one request is sent in parts that carry a session header of the provider's protocol. The guest names that header when it asks to send the file (`peersus.http` `authorize`, `headers`), the person is shown it with the file and the address, and the application then lets requests of that authorization carry it. An application older than this refuses the first part of a large file with "Unsupported request header" after nothing of it was sent; a small file is unaffected. Update the application and this package together.

## Execution format

The **WASM guest is the official build** of this extension and is packaged for Linux, Android and macOS; the store refuses a submission without it. Why WASM: one module, byte-identical on every platform, runs in an isolated interpreter that has no files, network, processes or OS calls of its own, only the application's brokered services the user granted, with bounded memory and work; a crash or hang ends only its worker. A native library (`.so`, second tier, not recommended) is only crash-isolated and runs with the application user's OS privileges; a process binary (third tier, not recommended) is an arbitrary program for which the application guarantees nothing beyond starting and stopping it.

The same guest source also builds as a native library (`cargo build --release --features native` in `wasm/`, packaged with `--native-library libpeersus_upload_wasm.so` as `upload-linux-native_library.tar.gz`) and, linked into the native host, as a process (`upload-linux-process.tar.gz`); every submission builds and validates both. They speak the identical protocol through the native host's function table instead of WebAssembly imports, so behaviour is the same; only the isolation is weaker. The store installs the WASM build only: the other two travel with the signed release for a person to install by hand, with limited support.

macOS support is limited: the same WASM package runs, but host services implemented only for Linux report that they are unsupported there. Windows has no extension host yet.

## Build and test

From the application workspace root:

```sh
cargo build -p peersus-upload
cargo test -p peersus-upload --all-targets
cargo test --manifest-path plugin/peersus-upload/wasm/Cargo.toml
cargo test -p peersus-ui-android --test extension_services
```

For the real authenticated sandbox integration, install the `wasm32-unknown-unknown` Rust target in the development environment, then build the production guest and run the explicit integration test:

```sh
cargo build --manifest-path plugin/peersus-upload/wasm/Cargo.toml \
  --release --target wasm32-unknown-unknown
PEERSUS_UPLOAD_WASM="$PWD/plugin/peersus-upload/wasm/target/wasm32-unknown-unknown/release/peersus_upload_wasm.wasm" \
  cargo test -p peersus-plugin-runtime --test process \
  real_upload_guest_uses_host_services_and_returns_the_verified_link -- --ignored
```

The unit and host-adapter tests cover malformed results, snapshot and authorization ownership, bounds and cancellation. The sandbox test executes the compiled production guest through the actual worker/hub ABI with synthetic host replies; it does not open Android dialogs or perform a live upload. Run real UI checks separately and only use a file and credential explicitly approved for that test. Never put credentials or personal test files in a submission.

## Submission

Export one source directory and one ownership identity for both native and WASM variants. Include this README, source and Cargo files, manifests, `submission.toml`, `ownership.json` and license notices. Follow the extension branch's root guide for standalone CI and submission commands. Passing CI is not catalog publication: each update still needs ownership verification and independent maintainer approval.

## License and third-party notices

This extension is licensed under Apache-2.0 (`LICENSE`, `NOTICE`). The WASM guest is built from these libraries: serde, serde_derive, serde_core, serde_json, itoa, memchr, zmij (MIT OR Apache-2.0, MIT, or Unlicense OR MIT), with build-time proc-macro2, quote, syn and unicode-ident ((MIT OR Apache-2.0) AND Unicode-3.0). Legacy native variant (not packaged): arboard, keyring, reqwest, rfd, rpassword, rustix, serde, serde_json, sha2, tempfile, thiserror, tokio, url and zeroize (MIT and/or Apache-2.0).
Every packaged archive carries `LICENSE`, `NOTICE`, `THIRD_PARTY_NOTICES.txt` and the license texts of its exact resolved dependency set under `licenses/`, generated by the packager from the lockfile; keep them when redistributing.
