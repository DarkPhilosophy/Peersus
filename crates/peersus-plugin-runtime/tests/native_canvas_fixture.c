#include "../sdk/peersus_plugin.h"
#include <inttypes.h>
#include <string.h>

static int write_all(const PeersusHostV1 *host, int32_t stream, const unsigned char *bytes, uint32_t size) {
    while (size != 0) {
        int32_t written = host->data_write(host->context, stream, bytes, size);
        if (written <= 0 || (uint32_t)written > size) return -1;
        bytes += written;
        size -= (uint32_t)written;
    }
    return 0;
}

static int frame(const PeersusHostV1 *host, int32_t stream, unsigned char seq,
                 unsigned char settled, const unsigned char color[4]) {
    enum { ROWS = 65, STRIDE = 1024 };
    const unsigned char header[] = {
        'P', 'S', 'F', '1', 3, 'm', 'a', 'p',
        0, 1, 0, 0, ROWS, 0, 0, 0, 0, STRIDE / 256, 0, 0, 1, /* 256 x 65, RGBA8 */
        seq, 0, 0, 0, 0, 0, 0, 0, settled, 0, 0, 0, 0, 0, 0, 0,
    };
    unsigned char row[STRIDE];
    for (size_t at = 0; at < sizeof(row); at += 4) memcpy(row + at, color, 4);
    if (write_all(host, stream, header, sizeof(header)) != 0) return -1;
    for (int y = 0; y < ROWS; ++y) {
        if (write_all(host, stream, row, sizeof(row)) != 0) return -1;
    }
    return 0;
}

int32_t peersus_plugin_main(const PeersusHostV1 *host) {
    if (!host || host->abi_version != PEERSUS_PLUGIN_ABI_VERSION
        || host->struct_size < sizeof(*host) || !host->poll) return 1;
    static const char request[] =
        "{\"op\":\"call\",\"id\":1,\"provider\":\"host\",\"service\":\"peersus.canvas\",\"version\":1,"
        "\"method\":\"open\",\"args\":{\"page\":\"demo\",\"canvas\":\"map\"},\"timeout_ms\":5000}";
    if (host->send(host->context, request, sizeof(request) - 1) != 0) return 1;
    char reply[1024];
    int32_t length = host->receive(host->context, reply, sizeof(reply) - 1);
    if (length <= 0 || (size_t)length >= sizeof(reply)) return 1;
    reply[length] = '\0';
    const char *at = strstr(reply, "\"claim\":");
    if (!at) return 1;
    uint64_t claim = strtoumax(at + sizeof("\"claim\":") - 1, NULL, 10);
    int32_t stream = host->data_open(host->context, PEERSUS_DATA_CANVAS, claim);
    if (stream <= 0) return 1;
    if (host->poll(host->context, &stream, 1, 0) != -2
        || host->data_shutdown(host->context, stream) != -1
        || host->data_read(host->context, stream, reply, 1) != -1) return 1;
    const unsigned char first[] = { 20, 40, 60, 255 }, second[] = { 20, 0, 10, 128 };
    if (frame(host, stream, 1, 42, first) != 0 || frame(host, stream, 2, 43, second) != 0
        || host->data_close(host->context, stream) != 0) return 1;
    return host->receive(host->context, reply, sizeof(reply)) == 0 ? 0 : 1;
}
