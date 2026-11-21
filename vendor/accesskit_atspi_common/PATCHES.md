# Local maintained-bus compatibility port

Upstream: [accesskit_atspi_common 0.21.1](https://crates.io/crates/accesskit_atspi_common/0.21.1).
Source repository: https://github.com/AccessKit/accesskit
Published source revision: `f3026222559c7f5f46d8f25cb7404de68eb149f0` (retained `.cargo_vcs_info.json`).
License: MIT OR Apache-2.0; upstream license and attribution files remain unchanged.
The published crate omitted standalone license files. `LICENSE-MIT` and `LICENSE-APACHE` are copied verbatim from the root of the source repository at the exact revision above.

This port requires Rust 1.89 or newer, matching zbus 6; the application toolchain is newer.

This local port adopts zbus 6.2.1's integrated wire types, names and derive macros instead of the unmaintained standalone serialization packages. Runtime consumers explicitly retain proxy/service support and map the former async-io runtime feature to default-rt. Existing feature selection remains available; no accessibility feature or XML signature validator is removed. Where needed, the maintained zbus_xml 6 release replaces version 5. Both dependency manifests record the migration.

The common accessibility error conversion is consolidated because zbus 6 unifies the former bus, name and wire errors. Name and wire validation failures retain their respective accessibility error categories; connection errors remain bus errors. This applies in atspi-common; other packages retain their existing conversion logic.

All seven retained adapter tests and desktop UI consumer compilation pass. The root workspace lockfile passes the strict local audit without suppressions. Full screen-reader/platform behavior and hosted CI remain unverified; compilation is not substituted for those checks.

Remove this override when a compatible upstream release supports zbus 6 and the required accessibility behavior is verified.
