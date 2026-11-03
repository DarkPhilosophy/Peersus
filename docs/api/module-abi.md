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

## Control and data are separate

Control buffers contain JSON operations/deliveries **without** the wire framing prefix. The ABI's `PEERSUS_CONTROL_LIMIT` is 65536 bytes; this does not enlarge the broker's stricter 48 KiB serialized control-message admission limit. Both boundaries apply.

`data_open` selects `PEERSUS_DATA_TICKET` (0) or `PEERSUS_DATA_CLAIM` (1) and an unsigned 64-bit number. The number references a hub ticket or an application-issued claim, not an identity supplied by the plugin. Data streams carry raw bytes. Finish an outgoing stream with `data_shutdown`, read to EOF, close it, and then reply; changing that ordering can race the host's data handshake.

Imports do not grant capabilities. The same declared services, version checks and user grants apply regardless of native or WASM execution. See [service messages](service-messages.md).

## Embedded package identity

Use `scripts/embed-metadata.py` and `PEERSUS_METADATA_RECORD` to embed the manifest's package metadata. The host reads the record without loading or running the program. ELF/WASM use `peersus.meta`; Mach-O uses `__DATA,peersus.meta`; PE uses `.psmeta`. This record binds metadata to bytes; the approved catalog digest and signature establish provenance. It is not a self-issued trust certificate. See [the extension-store contract](../extension-store.md).
