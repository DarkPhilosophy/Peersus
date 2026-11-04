/* Instrumentation sends one real Delivery::Call with ticket 7, using the shared C ABI. */
#include "../sdk/peersus_plugin.h"

static unsigned char incoming[PEERSUS_CONTROL_LIMIT];
#if defined(__wasm__)
#define DATA_OPEN peersus_data_open
#define DATA_WRITE peersus_data_write
#define DATA_SHUTDOWN peersus_data_shutdown
#define DATA_READ peersus_data_read
#define DATA_CLOSE peersus_data_close
#define RECEIVE peersus_receive
#define SEND peersus_send
#else
#define DATA_OPEN(kind, number) host->data_open(host->context, kind, number)
#define DATA_WRITE(handle, bytes, size) host->data_write(host->context, handle, bytes, size)
#define DATA_SHUTDOWN(handle) host->data_shutdown(host->context, handle)
#define DATA_READ(handle, bytes, size) host->data_read(host->context, handle, bytes, size)
#define DATA_CLOSE(handle) host->data_close(host->context, handle)
#define RECEIVE(bytes, size) host->receive(host->context, bytes, size)
#define SEND(bytes, size) host->send(host->context, bytes, size)
#endif
static int32_t work(
#if defined(__wasm__)
    void
#else
    const PeersusHostV1 *host
#endif
) {
    if (RECEIVE(incoming, sizeof(incoming)) <= 0) return 1;
    int32_t stream = DATA_OPEN(PEERSUS_DATA_TICKET, 7);
    if (stream < 0 || DATA_WRITE(stream, "real data", 9) != 9
        || DATA_SHUTDOWN(stream) != 0
        || DATA_READ(stream, incoming, 1) != 0
        || DATA_CLOSE(stream) != 0) return 1;
    static const unsigned char reply[] = "{\"op\":\"reply\",\"ticket\":7,\"result\":{\"status\":\"ok\",\"value\":\"real descriptor data\"}}";
    return SEND(reply, sizeof(reply) - 1) == 0 ? 0 : 1;
}
#if defined(__wasm__)
__attribute__((export_name("peersus_main"))) int32_t peersus_main(void) { return work(); }
#else
int32_t peersus_plugin_main(const PeersusHostV1 *host) {
    if (!host || host->abi_version != PEERSUS_PLUGIN_ABI_VERSION || host->struct_size < sizeof(PeersusHostV1)) return 1;
    return work(host);
}
#endif
