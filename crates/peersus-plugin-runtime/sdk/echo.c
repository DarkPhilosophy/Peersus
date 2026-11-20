/* A freestanding example: no libc, WASI, filesystem, network, or Rust ABI.
 * The host serializes Delivery::Call with its type and numeric ticket before args.
 * Echo answers those calls; all permission decisions remain in the existing broker.
 */
#include "peersus_plugin.h"

static unsigned char incoming[PEERSUS_CONTROL_LIMIT];
static unsigned char outgoing[256];

#if defined(__wasm__)
#define RECEIVE(buffer, size) peersus_receive(buffer, size)
#define SEND(buffer, size) peersus_send(buffer, size)
#else
#define RECEIVE(buffer, size) host->receive(host->context, buffer, size)
#define SEND(buffer, size) host->send(host->context, buffer, size)
#endif

static int32_t work(
#if !defined(__wasm__)
    const PeersusHostV1 *host
#else
    void
#endif
) {
    static const unsigned char prefix[] = "{\"op\":\"reply\",\"ticket\":";
    static const unsigned char suffix[] = ",\"result\":{\"status\":\"ok\",\"value\":\"portable echo\"}}";
    static const unsigned char call[] = "{\"type\":\"call\",\"ticket\":";
    for (;;) {
        int32_t size = RECEIVE(incoming, PEERSUS_CONTROL_LIMIT);
        if (size == 0) return 0;
        if (size < 0 || (uint32_t)size > PEERSUS_CONTROL_LIMIT) return 1;
        if ((uint32_t)size <= sizeof(call) - 1) continue;
        uint32_t at = 0;
        while (at < sizeof(call) - 1 && incoming[at] == call[at]) ++at;
        if (at != sizeof(call) - 1) continue;
        uint32_t out = 0, digits = 0;
        while (out < sizeof(prefix) - 1) { outgoing[out] = prefix[out]; ++out; }
        while (at < (uint32_t)size && incoming[at] >= '0' && incoming[at] <= '9' && digits < 20) {
            outgoing[out++] = incoming[at++];
            ++digits;
        }
        if (digits == 0 || at >= (uint32_t)size || incoming[at] != ',') return 1;
        for (uint32_t i = 0; i < sizeof(suffix) - 1; ++i) outgoing[out++] = suffix[i];
        if (SEND(outgoing, out) != 0) return 1;
    }
}

#if defined(__wasm__)
__attribute__((export_name("peersus_main"))) int32_t peersus_main(void) { return work(); }
#else
int32_t peersus_plugin_main(const PeersusHostV1 *host) {
    if (!host || host->abi_version != PEERSUS_PLUGIN_ABI_VERSION || host->struct_size < sizeof(PeersusHostV1)) return 1;
    return work(host);
}
#endif
