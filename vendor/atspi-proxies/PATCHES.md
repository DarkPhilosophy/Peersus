# Local maintained-bus compatibility port

Upstream: [atspi-proxies 0.13.0](https://crates.io/crates/atspi-proxies/0.13.0).
Source repository: https://github.com/odilia-app/atspi
Published source revision: `f486755b9bf993613f63a779133aafc54925004a` (retained `.cargo_vcs_info.json`).
License: Apache-2.0 OR MIT; upstream license and attribution files remain unchanged.

This port requires Rust 1.89 or newer, matching zbus 6; the application toolchain is newer.

This local port adopts zbus 6.2.1's integrated wire types, names and derive macros instead of the unmaintained standalone serialization packages. Runtime consumers explicitly retain proxy/service support and map the former async-io runtime feature to default-rt. Existing feature selection remains available; no accessibility feature or XML signature validator is removed. Where needed, the maintained zbus_xml 6 release replaces version 5. Both dependency manifests record the migration.

The common accessibility error conversion is consolidated because zbus 6 unifies the former bus, name and wire errors. Name and wire validation failures retain their respective accessibility error categories; connection errors remain bus errors. This applies in atspi-common; other packages retain their existing conversion logic.

The retained proxy test and desktop UI consumer compilation pass. An isolated AccessKit private-bus test also verifies registration and a real cross-connection method call. The root workspace lockfile passes the strict local audit without suppressions. Full screen-reader/platform behavior and hosted CI remain unverified.

Remove this override when a compatible upstream release supports zbus 6 and the required accessibility behavior is verified.
