# Local compatibility patch

Upstream: [mundy 0.2.3](https://crates.io/crates/mundy/0.2.3), [source](https://github.com/tautropfli/mundy/tree/608fe75c6a5eb82b14ac4539455c00ce01e774fb).
Revision: `608fe75c6a5eb82b14ac4539455c00ce01e774fb`, as recorded in the published crate's `.cargo_vcs_info.json`.
License: Apache-2.0; the upstream license and attribution files are retained unchanged.

This port requires Rust 1.89 or newer, matching zbus 6; the application toolchain is newer.

The Linux dependency moves from zbus 5 to zbus 6.2.1 with the explicit proxy feature. The existing `async-io` feature selects zbus's maintained `default-rt`; the `tokio` feature still selects zbus's Tokio integration. The portal implementation imports the integrated variant types under its existing local names and matches the renamed nested-variant case. Portal namespaces, preference subscriptions, requested preference features, and non-Linux implementations are unchanged. Both dependency manifests reflect this patch. Local regression tests exercise nested portal variant unwrapping and integrated wire-format roundtripping.

This patch does not disable preference support or suppress audit findings. All seven unit tests, including nested-variant wire encoding/decoding regressions, pass, and the desktop UI consumer compiles. The root workspace lockfile passes the strict local audit without suppressions. Actual desktop portal preference behavior and hosted CI remain unverified.

Remove this override when an upstream release supports zbus 6 and preserves the preference behavior required by this application.
