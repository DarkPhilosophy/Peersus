# Documentation

Peersus is development-stage software for consent-based transfers between devices. Linux and Android have device-level evidence; a successful cross-build is not a promise of desktop or mobile runtime support on another platform.

## Start here

- [Architecture and ownership](architecture.md): workspace boundaries, commands, networking and extension execution.
- [Extension store](extension-store.md): package provenance, catalog verification and maintainer setup.
- [API reference](api/README.md): local application control, plugin service messages and source contracts.
- [Development and verification](development.md): toolchain, build outputs and platform-specific checks.
- [Website preparation](website.md): static staging, catalog activation and publication boundaries.
- [Support](support.md): voluntary support and public acknowledgements.
- [Release-readiness evidence](release-readiness.md): historical checks, known limitations and unverified paths. Dates and the tested revision matter; earlier passing checks do not validate later changes.

## Source of truth

The implementation, executable tests and package manifests define the current contract. This documentation distinguishes intended behavior from demonstrated behavior. The extension manifest and message types live in `crates/peersus-plugin-api`; user commands and snapshots live in `crates/peersus-core`; CLI syntax lives in `crates/peersus-app/src/cli.rs`.

Never put pairing invitations, device identities, relay tokens, signing material or private diagnostic captures in an issue or pull request. Use synthetic examples when reporting a problem.
