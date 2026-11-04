# Upload extension

One extension identity with two execution variants: a native desktop process and an Android WASM guest. Both require an explicitly selected file and destination approval. Uploading sends plaintext content to the approved external provider and may create a public link; peer-to-peer transport encryption does not protect a file after that upload.

## Platforms and prerequisites

- Linux desktop: a graphical session, `zenity`, a working desktop file portal for the picker, and the platform credential-store service when saved credentials are used. The manifest declares the supported native variant; a successful Linux build does not certify macOS behavior.
- Android WASM: the matching application's trusted file-selection, HTTPS, encrypted credential configuration and result-presentation services. The guest receives opaque file and authorization handles, not ambient filesystem access or credential bytes. It validates provider success and its canonical single-file result before requesting presentation.
- Network uploads require HTTPS access to the configured provider. Tests below use synthetic data and do not require an account, a credential or a real upload.

The source is Apache-2.0 licensed; retain LICENSE and NOTICE in exported submissions. `submission.toml` declares build commands; CI resolves and records the trusted host SDK commit and uses its workspace lockfile. The WASM guest also has its own lockfile. Host and guest must be updated together when their service contract changes; an older host without `peersus.http.present` is not compatible with the updated WASM guest.

## Build and test

From the application workspace root:

```sh
cargo build -p peersus-upload
cargo test -p peersus-upload --all-targets
cargo test --manifest-path crates/peersus-upload/wasm/Cargo.toml
cargo test -p peersus-ui-android --test extension_services
```

For the real authenticated sandbox integration, install the `wasm32-unknown-unknown` Rust target in the development environment, then build the production guest and run the explicit integration test:

```sh
cargo build --manifest-path crates/peersus-upload/wasm/Cargo.toml \
  --release --target wasm32-unknown-unknown
PEERSUS_UPLOAD_WASM="$PWD/crates/peersus-upload/wasm/target/wasm32-unknown-unknown/release/peersus_upload_wasm.wasm" \
  cargo test -p peersus-plugin-runtime --test process \
  real_upload_guest_uses_host_services_and_returns_the_verified_link -- --ignored
```

The unit and host-adapter tests cover malformed results, snapshot and authorization ownership, bounds and cancellation. The sandbox test executes the compiled production guest through the actual worker/hub ABI with synthetic host replies; it does not open Android dialogs or perform a live upload. Run real UI checks separately and only use a file and credential explicitly approved for that test. Never put credentials or personal test files in a submission.

## Submission

Export one source directory and one ownership identity for both native and WASM variants. Include this README, source and Cargo files, manifests, `submission.toml`, `ownership.json` and license notices. Follow the extension branch's root guide for standalone CI and submission commands. Passing CI is not catalog publication: each update still needs ownership verification and independent maintainer approval.
