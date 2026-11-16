/* The C surface, once: a C program that includes include/lemnos.h, links the library and calls it
 * the way a C caller does. It opens a client on the stream service (argv[1]), reads the device,
 * opens and closes one frame stream, takes an error, then lists the packages on the second service
 * (argv[2]). It prints one line per step and exits non-zero on the first failure. The rest of the
 * surface is covered by c_abi.rs, which calls the same functions from Rust. */
#include <stdio.h>
#include <string.h>

#include "lemnos.h"

static char message[256];

static void collect(void *context, const uint8_t *text, size_t len) {
    char *out = context;
    size_t used = strlen(out);
    size_t room = sizeof message - 1 - used;
    if (len > room) len = room;
    memcpy(out + used, text, len);
    out[used + len] = 0;
}

static int fail(const char *what, int32_t status) {
    fprintf(stderr, "%s: status %d %s\n", what, status, message);
    return 1;
}

static int32_t open_client(const char *socket, LemnosHandle *client) {
    LemnosMessageSink sink = {message, collect};
    return lemnos_client_open(socket, "c-smoke", 0, client, sink);
}

int main(int argc, char **argv) {
    if (argc < 3) return 2;
    LemnosMessageSink sink = {message, collect};

    LemnosAbiVersion mine = LEMNOS_VERSION_INIT;
    int32_t status = lemnos_check_version(&mine);
    if (status != LEMNOS_OK) return fail("version", status);

    LemnosHandle client = 0;
    status = open_client(argv[1], &client);
    if (status != LEMNOS_OK) return fail("open", status);

    int32_t count = lemnos_device_count(client);
    if (count < 1) return fail("devices", count);
    char name[64];
    size_t len = 0;
    status = lemnos_device_text(client, 0, LEMNOS_DEVICE_NAME, name, sizeof name, &len);
    if (status != LEMNOS_OK) return fail("name", status);
    LemnosDeviceInfo info = {sizeof info, 0, 0, 0};
    status = lemnos_device_info(client, 0, &info);
    if (status != LEMNOS_OK) return fail("info", status);
    printf("device=%s state=%u\n", name, info.state);

    LemnosHandle stream = 0;
    status = lemnos_stream_open(client, 0, "frames", 0, &stream, sink);
    if (status != LEMNOS_OK) return fail("open frames", status);
    LemnosStreamInfo stream_info = {sizeof stream_info, 0, 0, 0, 0, 0, 0, 0};
    status = lemnos_stream_info(client, stream, &stream_info);
    if (status != LEMNOS_OK) return fail("stream info", status);
    printf("stream buffers=%u bytes=%u\n", stream_info.buffers, stream_info.buffer_bytes);
    status = lemnos_stream_close(client, stream, sink);
    if (status != LEMNOS_OK) return fail("close frames", status);

    /* An error the caller can read: the stream is closed, so its handle is stale. */
    status = lemnos_stream_info(client, stream, &stream_info);
    if (status != LEMNOS_E_STALE_HANDLE) return fail("stale stream", status);
    printf("stale stream status=%d\n", status);

    status = lemnos_client_close(client);
    if (status != LEMNOS_OK) return fail("close", status);

    LemnosHandle packages = 0;
    status = open_client(argv[2], &packages);
    if (status != LEMNOS_OK) return fail("open packages", status);
    uint32_t mode = 99;
    int32_t listed = lemnos_package_list(packages, &mode, sink);
    if (listed < 0) return fail("package list", listed);
    printf("packages=%d\n", listed);
    status = lemnos_client_close(packages);
    if (status != LEMNOS_OK) return fail("close packages", status);
    printf("done\n");
    return 0;
}
