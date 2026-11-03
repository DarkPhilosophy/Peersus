# Contributing

The project is in development. Keep changes focused and distinguish implementation, automated checks and actual platform runtime evidence.

## Before changing code

Read the [architecture](docs/architecture.md), [development guide](docs/development.md) and applicable [API contracts](docs/api/README.md). Reproduce defects at their real entry point where practical. Preserve unrelated work in a non-clean checkout; do not reset or reformat third-party sources as part of a local fix.

Platform-specific execution belongs behind the existing platform/host contracts. Frontends should submit shared application commands and render snapshots. Plugin-specific controls should derive from manifests and contributions, not plugin names. A new abstraction should remove a demonstrated duplication or boundary problem, not create a parallel framework.

## Verification

Use the declared Rust toolchain and locked dependencies. Run focused behavioral checks while developing, then the relevant gate:

```sh
cargo fmt --all
cargo xtask gate
```

The gate requires system metadata validators and `cargo-audit`; see the development guide and CI workflow. Android and visual behavior need their own build/run checks. Do not claim device or desktop runtime validation from compilation alone. Report missing prerequisites and failing checks rather than weakening assertions or disabling the gate.

For UI changes, inspect narrow layouts, long labels/translations, keyboard interaction, disabled/loading/error states and touch targets. For API changes, update consumers and documentation together. For performance changes, provide a measured baseline and the same workload after the change; fewer lines are not a performance measurement.

## Change description

Explain the user-visible problem, the cause supported by evidence, the chosen change, verification commands/results and any remaining limits. Include screenshots only with synthetic or appropriately redacted data. Do not commit local application data, device identifiers, credentials, APK signing material or generated build directories.

A pull request does not authorize a deployment, release or signature. Website artifacts, signed extension approval and publication are separate operations.

## Generated documentation

`README.md` and `CHANGELOG.md` mix manual text with generated regions delimited by `<!-- NAME-START -->` and `<!-- NAME-END -->`. Keep exactly one pair per region. Do not edit text inside a generated region: the next run replaces it.

- Workspace version, toolchain and license come from `Cargo.toml` and `rust-toolchain.toml`.
- The bundled-extension table comes from workspace plugin manifests.
- The workflow list comes from workflow definitions; it is not a live CI status or a claim that checks passed.
- Changelog entries and the README's latest changes come from `docs/changes.json`. Add user-visible changes there. `unreleased` is not a release; only add an entry to `releases` with its actual version/date when preparing that release. Git tags and commit messages do not silently publish release notes.

Run `python3 scripts/gen-docs.py` after changing a source, then `python3 scripts/gen-docs.py --check`. The generator is deterministic and works offline without a Git checkout. Its regression tests protect manual text, reject damaged markers and verify that dry-run checks do not write.

The documentation workflow automatically regenerates and uploads both files as an artifact on relevant pushes/PRs and manual dispatch. It fails if tracked generated sections are stale; the metadata gate checks the same invariant. It intentionally has read-only repository access and does not commit, push, sign or publish. Include regenerated files in the change through the normal authorized Git workflow.

## Extensions

Extension submissions use a separate archive-only workflow; follow [the extension-store contract](docs/extension-store.md). Never include signing secrets in a submission. Validator success is input validation, not maintainer approval or a promise that the package is safe.

## Security reports

Follow [SECURITY.md](SECURITY.md). Public bug reports are not an appropriate place for unmitigated exploit details or sensitive diagnostics.
