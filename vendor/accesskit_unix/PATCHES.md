# Local maintained-bus compatibility port

Upstream: [accesskit_unix 0.24.1](https://crates.io/crates/accesskit_unix/0.24.1).
Source repository: https://github.com/AccessKit/accesskit
Published source revision: `f3026222559c7f5f46d8f25cb7404de68eb149f0` (retained `.cargo_vcs_info.json`).
License: MIT OR Apache-2.0; upstream license and attribution files remain unchanged.
The published crate omitted standalone license files. `LICENSE-MIT` and `LICENSE-APACHE` are copied verbatim from the root of the source repository at the exact revision above.

This port requires Rust 1.89 or newer, matching zbus 6; the application toolchain is newer.

This local port adopts zbus 6.2.1's integrated wire types, names and derive macros instead of the unmaintained standalone serialization packages. Runtime consumers explicitly retain proxy/service support and map the former async-io runtime feature to default-rt. Existing feature selection remains available; no accessibility feature or XML signature validator is removed. Where needed, the maintained zbus_xml 6 release replaces version 5. Both dependency manifests record the migration.

The common accessibility error conversion is consolidated because zbus 6 unifies the former bus, name and wire errors. Name and wire validation failures retain their respective accessibility error categories; connection errors remain bus errors. This applies in atspi-common; other packages retain their existing conversion logic.

zbus 6 owns its connection scheduling through the selected runtime. The old private connection tick loops and executor plumbing are no longer compiled; the application's accessibility event loop and activation/deactivation flow remain. Interface registration treats `None` as successful insertion and `Some(interface)` as an already-registered interface, preserving the prior new-registration boolean semantics.

The five retained cache tests pass. An additional isolated private-bus regression verifies that initial application registration performs exactly one registry Embed handshake, repeat registration does not re-embed, the cache is installed, a new interface reports insertion while a duplicate does not, and another connection can call the registered interface. The root workspace lockfile passes the strict local audit without suppressions. These checks are not full screen-reader/platform verification or hosted CI success.

Remove this override when a compatible upstream release supports zbus 6 and the required accessibility behavior is verified.
