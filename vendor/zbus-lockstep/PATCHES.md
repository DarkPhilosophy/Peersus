# Local maintained-bus compatibility port

Upstream: [zbus-lockstep 0.5.2](https://crates.io/crates/zbus-lockstep/0.5.2).
Source repository: https://github.com/luukvanderduim/zbus-lockstep
Published source revision: `9bda5264dfe1dc7eaa7738f839ff977315c31b84` (retained `.cargo_vcs_info.json`).
License: MIT; upstream license and attribution files remain unchanged.

This port requires Rust 1.89 or newer, matching zbus 6; the application toolchain is newer.

This local port adopts zbus 6.2.1's integrated wire types, names and derive macros instead of the unmaintained standalone serialization packages. Runtime consumers explicitly retain proxy/service support and map the former async-io runtime feature to default-rt. Existing feature selection remains available; no accessibility feature or XML signature validator is removed. Where needed, the maintained zbus_xml 6 release replaces version 5. Both dependency manifests record the migration.

The common accessibility error conversion is consolidated because zbus 6 unifies the former bus, name and wire errors. Name and wire validation failures retain their respective accessibility error categories; connection errors remain bus errors. This applies in atspi-common; other packages retain their existing conversion logic.

The introspection XML 6 API returns iterators for methods, properties and signals. Lookup uses those iterators directly, retaining missing-interface/member/property errors and signature comparison rather than skipping validation.

The published crate omits its upstream XML test fixture. `xml/test_definition_file.xml` is restored verbatim from the same revision's root `xml/` directory; Git blob identity `7796e237878dbd5bcd63795ff01910e481188bf8` is verified. This allows the original signature lookup tests to run without weakening their assertions.

All 23 retained XML validator tests pass, as do the common AT-SPI protocol/event suite and desktop UI consumer compilation. The root workspace lockfile passes the strict local audit without suppressions. Full screen-reader/platform behavior and hosted CI remain unverified.

Remove this override when a compatible upstream release supports zbus 6 and the required accessibility behavior is verified.
