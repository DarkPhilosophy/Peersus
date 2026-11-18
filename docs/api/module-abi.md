# Native-library and WebAssembly ABI

The source contract is `crates/peersus-plugin-runtime/sdk/peersus_plugin.h`. It is a C-compatible ABI, not Rust's ABI. Keep language bindings faithful to the integer widths, pointer lifetimes and return conventions in that header.

## Native entry

A native library exports exactly:

```c
int32_t peersus_plugin_main(const PeersusHostV1 *host);
```

The helper supplies the function table for the duration of this call. Check `abi_version` against `PEERSUS_PLUGIN_ABI_VERSION` (currently 1) and check `struct_size` before using appended fields. Append-only additions require size checks; incompatible changes require a new ABI version. Do not retain the table or its context after the entry returns.

The table provides `receive`, `send`, `data_open`, `data_read`, `data_write`, `data_close` and `data_shutdown`. Each native callback receives the host context. Control receive returns a byte count, or zero at EOF; control send returns zero on success. Data reads/writes return byte counts. Operations can return a negative error. Handle partial reads/writes rather than assuming a stream operation transfers the requested length.

The host loads the library in its separate executor process, never in the application owner or UI. A native process boundary alone is not a general guarantee of sandboxing; assess the actual platform isolation and granted broker access.

## WebAssembly entry

A module exports memory and:

```c
int32_t peersus_main(void);
```

It imports functions from the `peersus_v1` module with the names in the SDK header. These have no native context pointer. Build freestanding; the contract does not provide WASI or libc imports. Host handles and bootstrap credentials remain in the host, not in module memory as ambient OS resources.

### 32-bit and 64-bit modules

Both widths use the same module name and the same functions. The host reads the width from the module's own exported `memory`: a 32-bit memory takes 32-bit pointers and lengths, a 64-bit memory (the WebAssembly `memory64` proposal) takes the same functions with every pointer and length as a 64-bit integer. Return values, handles and the `kind` and `number` arguments of `data_open` are unchanged. Nothing is declared and nothing is configured: the module's memory is the single source of truth, so a manifest cannot choose a width, and a module whose imports disagree with its memory does not link.

A 64-bit module is not given more memory. The host's memory and module-size bounds are identical for both widths, and a 64-bit pointer is checked as a whole: a value that does not fit a host address is refused, never truncated to a smaller valid one. Choose 64-bit only if you need it; for a text plugin 32-bit is smaller and no slower. Rust targets are `wasm32-unknown-unknown` and, on nightly with `-Zbuild-std`, `wasm64-unknown-unknown`.

## Control and data are separate

Control buffers contain JSON operations/deliveries **without** the wire framing prefix. The ABI's `PEERSUS_CONTROL_LIMIT` is 65536 bytes; this does not enlarge the broker's stricter 48 KiB serialized control-message admission limit. Both boundaries apply.

`data_open` selects `PEERSUS_DATA_TICKET` (0) or `PEERSUS_DATA_CLAIM` (1) and an unsigned 64-bit number. The number references a hub ticket or an application-issued claim, not an identity supplied by the plugin. Data streams carry raw bytes. Finish an outgoing stream with `data_shutdown`, read to EOF, close it, and then reply; changing that ordering can race the host's data handshake.

For simultaneous streams, `poll(handles, count, timeout_ms)` waits for the control channel and only the selected open data handles, without reading bytes. It returns `0` for control, a positive data handle for readable data or EOF, `-1` for timeout, or `-2` for a transport error. Select at most 32 distinct handles. The timeout is `-1` for an indefinite wait or `0..30000` milliseconds; zero checks readiness without waiting. Pointer and count follow the module's memory width; each selected handle remains a little-endian 32-bit integer. Invalid guest memory or invalid handle selections trap like other invalid ABI arguments. Readiness alone does not renew interpreter fuel.

Only read the selected channel and process a bounded chunk before waiting again. Keep parsing buffers, input ownership and outstanding reply tickets per stream. Readiness is scheduled fairly across control and selected data streams. An open, idle data stream must not block control deliveries or another peer. Cancelled receive tickets must close their stream and release only their owner's input, without a late reply.

The native table appends `poll` after `data_shutdown`; check `struct_size` before using it. Existing modules that do not use polling remain compatible with the extended host. A WASM module importing `poll` requires a host that provides this extension.

The optional WASM `diagnostic(stage: i32, outcome: i32, correlation: i64, actual: i64, expected: i64) -> i32` import carries only the SDK header's closed stage/outcome vocabulary and bounded counters, never guest text or memory. It uses the same scalar signature with either memory width. It never grants capabilities, sends broker operations or renews interpreter fuel. Development hosts admit at most 64 events per second per isolated generation; invalid values return `-1`, excess events `-2`, and production hosts return `1` without emitting. A normal admitted event returns `0`. Correlation must be a local call/ticket/handle, never a device identity or bearer claim. Build guests with this import only for development: old guests still link to the extended host; diagnostic guests require an updated host. Native libraries and process plugins do not gain an arbitrary stderr logging channel.

Imports do not grant capabilities. The same declared services, version checks and user grants apply regardless of native or WASM execution. See [service messages](service-messages.md).

## The Rust guest's two boundary files

A Rust guest does not have to write the imports itself. `templates/guest/host.rs` holds them behind two traits (`Wire` for control messages, `Data` for streams), with the bounded reads and writes every guest needs and a scripted host for its tests; `templates/guest/native.rs` holds the same functions through the native table, for the same guest built as a native library. A guest copies the files it needs into `wasm/src/` and declares the Cargo features they name (`native`, `development-diagnostics`). They are copied, not depended on: an extension is built from its own source and lockfile, and the application's workspace forbids `unsafe` outside its two platform crates, so the boundary cannot be a workspace crate.

A copy is exactly the file: `scripts/check-guest-glue.py <extension>` compares byte for byte, and the source check of a submission (`scripts/check-extension-source.py`) runs it before any submitted command, refusing a guest whose copy differs. A rule of the boundary is therefore changed in `templates/guest/` and copied out, never edited in one extension. A function a guest never calls is not imported by its module (the linker drops it), so carrying the whole file does not make a small guest require `poll` of its host. A guest that speaks to `peersus_v1` in its own few lines, like `templates/hello-guest`, carries neither file and is not concerned.

## Embedded package identity

Use `scripts/embed-metadata.py` and `PEERSUS_METADATA_RECORD` to embed the manifest's package metadata. The host reads the record without loading or running the program. ELF/WASM use `peersus.meta`; Mach-O uses `__DATA,peersus.meta`; PE uses `.psmeta`. This record binds metadata to bytes; the approved catalog digest and signature establish provenance. It is not a self-issued trust certificate. See [the extension-store contract](../extension-store.md).
