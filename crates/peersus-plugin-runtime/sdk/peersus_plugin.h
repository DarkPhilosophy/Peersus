#ifndef PEERSUS_PLUGIN_V1_H
#define PEERSUS_PLUGIN_V1_H

/* Embedded identity. The host reads this record WITHOUT loading or running the program and requires
 * it to equal the manifest's `package` block. Generate the record with
 * `python3 scripts/embed-metadata.py header plugin.json > metadata.h`, which emits the exact bytes
 * (8-byte magic, u32 LE length, JSON) as PEERSUS_METADATA_BYTES, then write
 * `PEERSUS_METADATA_RECORD(PEERSUS_METADATA_BYTES)` once. Section names are per format because a
 * PE image allows eight characters: `.psmeta` (PE), `__DATA,peersus.meta` (Mach-O), `peersus.meta`
 * (ELF and WebAssembly). This is an identity check; trust comes from the maintainer-signed catalog
 * digest over the whole package. */
#if defined(_WIN32)
#define PEERSUS_METADATA_SECTION __attribute__((section(".psmeta"), used))
#elif defined(__APPLE__)
#define PEERSUS_METADATA_SECTION __attribute__((section("__DATA,peersus.meta"), used))
#else
#define PEERSUS_METADATA_SECTION __attribute__((section("peersus.meta"), used))
#endif
#define PEERSUS_METADATA_RECORD(...) \
    PEERSUS_METADATA_SECTION static const unsigned char peersus_metadata_record[] = { __VA_ARGS__ }

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define PEERSUS_PLUGIN_ABI_VERSION 1u
#define PEERSUS_CONTROL_LIMIT 65536u
#define PEERSUS_DATA_TICKET 0
#define PEERSUS_DATA_CLAIM 1

/* JSON operations and deliveries retain the ordinary broker protocol. Imports never
 * grant capabilities. Bootstrap tokens and operating-system handles remain in the host.
 * A data number names a hub ticket or an application-issued claim, never an identity.
 * Control receive returns its byte count (zero at EOF); send returns zero on success.
 * Data read/write return byte counts, and all methods may return a negative error.
 * Receive/send buffers exclude the wire framing prefix; data streams contain raw bytes.
 * Finish an outgoing stream with data_shutdown, read to EOF, then close and Reply.
 * This preserves the existing native protocol without racing the host's data handshake.
 * poll waits without consuming bytes for control (0) or one selected data handle (>0),
 * including EOF. It returns -1 on timeout and -2 on transport failure. Select at most
 * 32 distinct open handles; timeout_ms is -1 (wait), or 0..30000. Handles are i32,
 * even in memory64. Read only the returned stream; other streams may still block.
 *
 * Width. The protocol is `peersus_v1` whatever a module's pointer width. The host reads the width from the
 * module's own exported memory: a 32-bit memory takes the types written below, a 64-bit memory (memory64) takes
 * the same functions with every pointer and length as a 64-bit integer. Return values, handles and the `kind` and
 * `number` of data_open do not change. The module is the only source of truth: a manifest cannot choose a width,
 * and a module whose imports disagree with its memory does not link. Wide addressing is not more memory, which
 * the host bounds the same for both.
 */
#if defined(__wasm__)
#define PEERSUS_IMPORT(name) __attribute__((import_module("peersus_v1"), import_name(name)))
PEERSUS_IMPORT("receive") int32_t peersus_receive(void *buffer, uint32_t capacity);
PEERSUS_IMPORT("send") int32_t peersus_send(const void *buffer, uint32_t length);
PEERSUS_IMPORT("data_open") int32_t peersus_data_open(int32_t kind, uint64_t number);
PEERSUS_IMPORT("data_read") int32_t peersus_data_read(int32_t handle, void *buffer, uint32_t capacity);
PEERSUS_IMPORT("data_write") int32_t peersus_data_write(int32_t handle, const void *buffer, uint32_t length);
PEERSUS_IMPORT("data_close") int32_t peersus_data_close(int32_t handle);
PEERSUS_IMPORT("data_shutdown") int32_t peersus_data_shutdown(int32_t handle);
PEERSUS_IMPORT("poll") int32_t peersus_poll(const int32_t *handles, uintptr_t count, int32_t timeout_ms);
/* Compile freestanding, export memory and this entry, and do not import WASI or libc. */
int32_t peersus_main(void);
#else
/* The table is valid for the duration of peersus_plugin_main. Append-only additions
 * require checking struct_size; incompatible changes require a new ABI version.
 */
typedef struct PeersusHostV1 {
    uint32_t abi_version;
    uint32_t struct_size;
    void *context;
    int32_t (*receive)(void *context, void *buffer, uint32_t capacity);
    int32_t (*send)(void *context, const void *buffer, uint32_t length);
    int32_t (*data_open)(void *context, int32_t kind, uint64_t number);
    int32_t (*data_read)(void *context, int32_t handle, void *buffer, uint32_t capacity);
    int32_t (*data_write)(void *context, int32_t handle, const void *buffer, uint32_t length);
    int32_t (*data_close)(void *context, int32_t handle);
    int32_t (*data_shutdown)(void *context, int32_t handle);
    /* Optional append-only extension: check struct_size before accessing this member. */
    int32_t (*poll)(void *context, const int32_t *handles, uint32_t count, int32_t timeout_ms);
} PeersusHostV1;

#if defined(_WIN32)
#define PEERSUS_EXPORT __declspec(dllexport)
#else
#define PEERSUS_EXPORT __attribute__((visibility("default")))
#endif
/* Export exactly this C symbol; never export a Rust ABI or load it in an owner/UI. */
PEERSUS_EXPORT int32_t peersus_plugin_main(const PeersusHostV1 *host);
#endif
#ifdef __cplusplus
}
#endif
#endif
