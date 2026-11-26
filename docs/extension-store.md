# Extension Store: source review and admission

Use the [submission guide](../README.md) for the recipe and local commands. The
[Store specification](https://github.com/axxa-dev/Peersus/blob/main/docs/extension-store.md)
on `main` is the source of truth for the admission and installation pipeline.

## Source review

- A submission changes one `extension/<name>/` directory. Review the source,
  `submission.toml`, the manifest, and the ownership record; the CI build, not a
  contributor-supplied archive, produces the packages. A change to the ownership
  record does not grant itself authority.
- Check that the manifest describes the actual capabilities and versioned
  service imports and exports. Require both licence and notice in `agreements`;
  archive attribution files do not replace those pages.
- Keep product-specific behavior in the guest. Check validation of untrusted
  inputs, bounded work and data, explicit consent and current grants, and
  truthful handling of failure, cancellation and restart. A native library or
  standalone executable has ambient OS access; process separation alone is
  not a security sandbox.
- Check that the recipe covers the required WASM variants (Linux, Android and
  macOS; Windows is optional). A declared optional native library or executable
  should implement the same extension contract. Only WASM is distributed by
  the Store; native variants are for manual installation.

## Evidence and acceptance

Review findings should identify the file, behavior and impact. Source review is
not evidence that binaries were built or tested. The `run:ci` label starts
validation for the exact PR head: build and test, independent package validation,
then resource measurements against the published Linux WASM baseline when one
exists. CI binds the reviewed archives to one extension ID and records their
digests and source revision in `review.json`. Native variants are not executed
by the benchmark.

A passing run is neither maintainer approval nor publication. At admission,
check the exact source revision and reviewed artifacts, not older checkmarks or
a build for a different revision. A first release has no baseline; completed
samples do not demonstrate absence of a regression against a prior version.
