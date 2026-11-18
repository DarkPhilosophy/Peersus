# Documentation

Peersus is development-stage software for consent-based transfers between devices. Linux and Android have device-level evidence; a successful cross-build is not a promise of desktop or mobile runtime support on another platform.

## Start here

- [Architecture and ownership](architecture.md): workspace boundaries, commands, networking and extension execution.
- [Extension store](extension-store.md): package provenance, catalog verification and maintainer setup.
- [Application updates](app-update.md): how the application replaces itself, the key and channel it trusts, and what each platform does.
- [API reference](api/README.md): local application control, plugin service messages and source contracts.
- [Development and verification](development.md): toolchain, build outputs and platform-specific checks.
- [Test strategy](qa-strategy.md): what each kind of test is evidence of, what the gate runs, what needs a device or a person, and the known gaps.
- [Engineering log](engineering-log.md): findings, fixes and measurements, each marked verified, fixed, open or unverified.
- [Go/No-Go evaluation](go-no-go.md): the current verdict and what blocks a release, each pointing at its evidence in the log.
- [macOS evaluation](macos-evaluation.md): what the source does and does not do for macOS, read and not run, and how it could be tested.
- [Devices nearby](nearby-send.md): an experiment behind a switch that is off by default: a paired device brought near is proposed on Send, and devices nearby are listed on Peers to pair with. Linux and Android.
- [Website preparation](website.md): static staging, catalog activation and publication boundaries.
- [Support](support.md): voluntary support and public acknowledgements.
- [Release-readiness evidence](release-readiness.md): historical checks, known limitations and unverified paths. Dates and the tested revision matter; earlier passing checks do not validate later changes.

## Source of truth

The implementation, executable tests and package manifests define the current contract. This documentation distinguishes intended behavior from demonstrated behavior. The extension manifest and message types live in `crates/peersus-api`; user commands and snapshots live in `crates/peersus-core`; CLI syntax lives in `crates/peersus-app/src/cli.rs`.

Never put pairing invitations, device identities, relay tokens, signing material or private diagnostic captures in an issue or pull request. Use synthetic examples when reporting a problem.
