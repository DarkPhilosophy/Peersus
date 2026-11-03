#include "../sdk/peersus_plugin.h"
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int32_t peersus_plugin_main(const PeersusHostV1 *host) {
    if (!host || host->abi_version != PEERSUS_PLUGIN_ABI_VERSION
        || host->struct_size < offsetof(PeersusHostV1, data_shutdown) + sizeof(host->data_shutdown)) return 1;
    char incoming[PEERSUS_CONTROL_LIMIT + 1], reply[256];
    for (;;) {
        int32_t length = host->receive(host->context, incoming, PEERSUS_CONTROL_LIMIT);
        if (length == 0) return 0;
        if (length < 0 || length > (int32_t)PEERSUS_CONTROL_LIMIT) return 1;
        incoming[length] = 0;
        const char *at = strstr(incoming, "\"ticket\":");
        if (!at) return 1;
        unsigned long long ticket = strtoull(at + 9, NULL, 10);
        int32_t stream = host->data_open(host->context, PEERSUS_DATA_TICKET, ticket);
        if (stream < 0) return 1;
        if (host->data_write(host->context, stream, "real data", 9) != 9
            || host->data_shutdown(host->context, stream) != 0
            || host->data_read(host->context, stream, incoming, 1) != 0
            || host->data_close(host->context, stream) != 0) return 1;
        int written = snprintf(reply, sizeof(reply), "{\"op\":\"reply\",\"ticket\":%llu,\"result\":{\"status\":\"ok\",\"value\":\"actual wasm action\"}}", ticket);
        if (written <= 0 || (size_t)written >= sizeof(reply)
            || host->send(host->context, reply, (uint32_t)written) != 0) return 1;
    }
}
