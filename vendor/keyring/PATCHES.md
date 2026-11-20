# Internal synchronous-backend compatibility patch

## Provenance and licensing

- Package: `keyring` 3.6.3, copied from the published crates.io source package.
- Upstream: https://github.com/hwchen/keyring-rs
- Published source revision: `315cbdf6c6a9153d8c9f88b56568f29862d3e39d` (the retained `.cargo_vcs_info.json`).
- License: MIT OR Apache-2.0. Original licenses and copyright notices are retained unchanged.
- Modified files: `Cargo.toml`, `Cargo.toml.orig`, `src/lib.rs`, `src/secret_service.rs`. Each carries a modification notice. Upstream README and remaining sources are retained for provenance; the feature contract of this internal copy is described here and in the adjusted crate documentation.

## Scope

The application enables `apple-native`, `windows-native`, `sync-secret-service`, `crypto-rust`, and `vendored`. These features, their native backends and the public credential API are retained. Synchronous Secret Service transport encryption, lookup attributes, target-first matching, default-collection legacy fallback, ambiguity handling and update-before-create logic are unchanged.

This internal copy intentionally removes the unused `async-secret-service`, `linux-native-async-persistent`, `async-io`, and `tokio` features, their optional `secret-service`/`zbus` dependencies, their weak crypto-feature edges, and their unreachable conditional imports. It is not a general-purpose replacement for every upstream keyring configuration. No application backend is switched, no keychain migration is performed, and no credential is read or rewritten by applying this patch.

A supported `cargo update --workspace` on the unmodified manifest left the lockfile unchanged and all three legacy wire-type advisories present. Removing these optional dependency declarations addresses the graph itself, not advisory suppression or a hand-edited lockfile. The retained `crypto-rust` implementation is not itself one of those three audit findings.

## Verification and limitations

The regenerated root lockfile passes both `cargo audit --deny warnings --json` (zero vulnerabilities and warnings) and the actual `cargo xtask audit` gate locally. Targeted application regressions pass: two mock identity tests, five relay-token tests, sixteen upload tests, ten About/license tests and four locale/catalog tests. The permission-gated native credential test remains ignored; no actual OS credential access, Apple/Windows runtime test or hosted CI success is claimed. Source comparison confirms exactly the four declared modified upstream files; both original license files remain byte-identical. Regenerate the root lockfile through Cargo; do not edit dependency edges manually.

## Removal

Remove this local patch once an upstream supported release avoids the unused legacy dependency chain while preserving the application's credential lookup contract, or after a separately verified backend migration. Recheck strict audit, mock credential regressions, installed credential compatibility, desktop and Android builds before changing the credential backend.
