#define _POSIX_C_SOURCE 200809L
#include "../sdk/peersus_plugin.h"
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <limits.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <sys/un.h>
#include <unistd.h>

#define MAX_STREAMS 32
struct Stream { int32_t handle; int fd; };
struct Context {
    int control;
    int32_t next_handle;
    const char *data_socket;
    const char *token;
    struct Stream streams[MAX_STREAMS];
};

static int transfer(int fd, void *buffer, size_t size, int writing) {
    unsigned char *at = buffer;
    size_t done = 0;
    while (done < size) {
        ssize_t amount = writing ? send(fd, at + done, size - done, MSG_NOSIGNAL)
                                 : recv(fd, at + done, size - done, 0);
        if (amount < 0 && errno == EINTR) continue;
        if (amount <= 0) return amount == 0 && !writing && done == 0 ? 0 : -1;
        done += (size_t)amount;
    }
    return 1;
}

static int frame_write(int fd, const void *bytes, uint32_t size) {
    if (!bytes || size == 0 || size > PEERSUS_CONTROL_LIMIT) return -1;
    unsigned char header[4] = { (unsigned char)(size >> 24), (unsigned char)(size >> 16),
                                (unsigned char)(size >> 8), (unsigned char)size };
    return transfer(fd, header, sizeof(header), 1) == 1
        && transfer(fd, (void *)bytes, size, 1) == 1 ? 0 : -1;
}

static int timeout_socket(int fd, int option, int seconds) {
    struct timeval timeout = { .tv_sec = seconds, .tv_usec = 0 };
    return setsockopt(fd, SOL_SOCKET, option, &timeout, sizeof(timeout));
}

static int connect_socket(const char *path) {
    struct sockaddr_un address = { .sun_family = AF_UNIX };
    if (!path || path[0] != '/' || strlen(path) >= sizeof(address.sun_path)) return -1;
    memcpy(address.sun_path, path, strlen(path) + 1);
    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd < 0) return -1;
    if (fcntl(fd, F_SETFD, FD_CLOEXEC) < 0 ||
        connect(fd, (struct sockaddr *)&address, (socklen_t)(offsetof(struct sockaddr_un, sun_path) + strlen(path) + 1)) < 0) {
        close(fd); return -1;
    }
    return fd;
}

static int32_t receive_control(void *context, void *buffer, uint32_t capacity) {
    struct Context *host = context;
    unsigned char header[4];
    if (!buffer || capacity == 0 || capacity > PEERSUS_CONTROL_LIMIT) return -1;
    int status = transfer(host->control, header, sizeof(header), 0);
    if (status <= 0) return status;
    uint32_t size = ((uint32_t)header[0] << 24) | ((uint32_t)header[1] << 16)
                  | ((uint32_t)header[2] << 8) | (uint32_t)header[3];
    if (size == 0 || size > PEERSUS_CONTROL_LIMIT || size > capacity) return -1;
    return transfer(host->control, buffer, size, 0) == 1 ? (int32_t)size : -1;
}

static int32_t send_control(void *context, const void *buffer, uint32_t size) {
    return frame_write(((struct Context *)context)->control, buffer, size);
}

static struct Stream *stream_for(struct Context *host, int32_t handle) {
    if (handle <= 0) return NULL;
    for (size_t i = 0; i < MAX_STREAMS; ++i) {
        if (host->streams[i].handle == handle) return &host->streams[i];
    }
    return NULL;
}

static int32_t open_data(void *context, int32_t kind, uint64_t number) {
    struct Context *host = context;
    if ((kind != PEERSUS_DATA_TICKET && kind != PEERSUS_DATA_CLAIM) || host->next_handle == INT32_MAX) return -1;
    struct Stream *slot = NULL;
    for (size_t i = 0; i < MAX_STREAMS; ++i) {
        if (host->streams[i].handle == 0) { slot = &host->streams[i]; break; }
    }
    if (!slot) return -1;
    int fd = connect_socket(host->data_socket);
    if (fd < 0) return -1;
    char hello[256];
    int length = kind == PEERSUS_DATA_TICKET
        ? snprintf(hello, sizeof(hello), "{\"token\":\"%s\",\"ticket\":%" PRIu64 "}", host->token, number)
        : snprintf(hello, sizeof(hello), "{\"token\":\"%s\",\"ticket\":0,\"claim\":%" PRIu64 "}", host->token, number);
    if (length <= 0 || (size_t)length >= sizeof(hello)
        || timeout_socket(fd, SO_RCVTIMEO, 30) < 0 || timeout_socket(fd, SO_SNDTIMEO, 30) < 0
        || frame_write(fd, hello, (uint32_t)length) != 0) { close(fd); return -1; }
    slot->fd = fd;
    slot->handle = host->next_handle++;
    return slot->handle;
}

static int32_t read_data(void *context, int32_t handle, void *buffer, uint32_t capacity) {
    struct Stream *stream = stream_for(context, handle);
    if (!stream || !buffer || capacity > PEERSUS_CONTROL_LIMIT) return -1;
    ssize_t amount;
    do { amount = recv(stream->fd, buffer, capacity, 0); } while (amount < 0 && errno == EINTR);
    return (int32_t)amount;
}

static int32_t write_data(void *context, int32_t handle, const void *buffer, uint32_t length) {
    struct Stream *stream = stream_for(context, handle);
    if (!stream || !buffer || length > PEERSUS_CONTROL_LIMIT) return -1;
    ssize_t amount;
    do { amount = send(stream->fd, buffer, length, MSG_NOSIGNAL); } while (amount < 0 && errno == EINTR);
    return (int32_t)amount;
}

static int32_t close_data(void *context, int32_t handle) {
    struct Stream *stream = stream_for(context, handle);
    if (!stream) return -1;
    int fd = stream->fd;
    stream->handle = 0;
    stream->fd = -1;
    return close(fd) == 0 ? 0 : -1;
}

static int32_t shutdown_data(void *context, int32_t handle) {
    struct Stream *stream = stream_for(context, handle);
    return stream && shutdown(stream->fd, SHUT_WR) == 0 ? 0 : -1;
}

/* Called only inside an already separately managed native worker, never in the owner/UI.
 * Native code has application-UID privileges; this is crash isolation, not a sandbox.
 */
int peersus_native_run(const char *module, const char *socket, const char *data_socket, const char *token) {
    if (!module || module[0] != '/' || !token || strlen(token) != 64
        || strspn(token, "0123456789abcdef") != 64) return 1;
    struct Context host = { .control = -1, .next_handle = 1, .data_socket = data_socket, .token = token };
    host.control = connect_socket(socket);
    if (host.control < 0) return 1;
    char hello[96];
    int length = snprintf(hello, sizeof(hello), "{\"token\":\"%s\"}", token);
    if (length <= 0 || (size_t)length >= sizeof(hello) || timeout_socket(host.control, SO_SNDTIMEO, 5) < 0
        || frame_write(host.control, hello, (uint32_t)length) != 0) { close(host.control); return 1; }
    void *library = dlopen(module, RTLD_NOW | RTLD_LOCAL);
    int32_t (*entry)(const PeersusHostV1 *) = library ? (int32_t (*)(const PeersusHostV1 *))dlsym(library, "peersus_plugin_main") : NULL;
    int result = 1;
    if (entry) {
        const PeersusHostV1 api = {
            .abi_version = PEERSUS_PLUGIN_ABI_VERSION, .struct_size = sizeof(PeersusHostV1), .context = &host,
            .receive = receive_control, .send = send_control, .data_open = open_data,
            .data_read = read_data, .data_write = write_data, .data_close = close_data, .data_shutdown = shutdown_data,
        };
        result = entry(&api) == 0 ? 0 : 1;
    }
    for (size_t i = 0; i < MAX_STREAMS; ++i) {
        if (host.streams[i].handle != 0) close(host.streams[i].fd);
    }
    close(host.control);
    if (library) dlclose(library);
    return result;
}

#ifndef __ANDROID__
int main(int argc, char **argv) {
    if (argc != 2) return 1;
    int result = peersus_native_run(argv[1], getenv("PEERSUS_PLUGIN_SOCKET"), getenv("PEERSUS_PLUGIN_DATA_SOCKET"), getenv("PEERSUS_PLUGIN_TOKEN"));
    if (result != 0) fputs("Native extension host failed\n", stderr);
    return result;
}
#endif
