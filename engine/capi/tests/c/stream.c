/* A C consumer of frame streams and the package list. On the first service:
 * frames of dma-bufs read in place, by copy and through each buffer's own
 * handle; what is refused; a stream the consumer fills, with jobs; a stream of
 * shared memory; a client closed with a frame held. On the second: the package
 * list. It prints what it saw and exits non-zero on the first failure. */
#define _POSIX_C_SOURCE 200809L
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

#include "lemnos.h"

#define FRAME_BYTES 5000
#define INPUT_BYTES 256

static void collect(void *context, const uint8_t *text, size_t len) {
    char *out = context;
    size_t used = strlen(out);
    size_t room = 255 - used;
    if (len > room) len = room;
    memcpy(out + used, text, len);
    out[used + len] = 0;
}

static char message[256];

static int fail(const char *what, int32_t status) {
    fprintf(stderr, "%s: status %d %s\n", what, status, message);
    return 1;
}

/* What the producer writes into a frame's buffer. */
static int frame_is_right(const uint8_t *bytes, size_t len, uint64_t sequence) {
    for (size_t i = 0; i < len; i++)
        if (bytes[i] != (uint8_t)(sequence * 7 + i)) return 0;
    return 1;
}

static int32_t open_client(const char *socket, LemnosHandle *client) {
    LemnosMessageSink sink = {message, collect};
    return lemnos_client_open(socket, "c-stream", 0, client, sink);
}

static int frames(LemnosHandle client) {
    LemnosMessageSink sink = {message, collect};
    LemnosHandle stream = 0;
    int32_t status = lemnos_stream_open(client, 0, "frames", 0, &stream, sink);
    if (status != LEMNOS_OK) return fail("open frames", status);
    LemnosStreamInfo info = {sizeof info, 0, 0, 0, 0, 0, 0, 0};
    status = lemnos_stream_info(client, stream, &info);
    if (status != LEMNOS_OK) return fail("info", status);
    printf("frames buffers=%u bytes=%u handles=%d writable=%d open=%d\n", info.buffers,
           info.buffer_bytes, !!(info.flags & LEMNOS_STREAM_BUFFER_HANDLES),
           !!(info.flags & LEMNOS_STREAM_WRITABLE), !!(info.flags & LEMNOS_STREAM_OPEN));

    for (int n = 0; n < 6; n++) {
        LemnosFrame frame = {sizeof frame, 0, 0, 0, 0, 0, 0};
        const uint8_t *data = NULL;
        status = lemnos_stream_next_frame(client, stream, 5000, &frame, &data, sink);
        if (status != 1) return fail("next frame", status);
        if (frame.len != FRAME_BYTES || !frame_is_right(data, frame.len, frame.sequence))
            return fail("frame in place", -1);
        uint8_t copy[16];
        status = lemnos_stream_read(client, stream, frame.buffer, 100, copy, sizeof copy, sink);
        if (status != LEMNOS_OK || memcmp(copy, data + 100, sizeof copy) != 0)
            return fail("frame by copy", status);
        int32_t fd = -1;
        status = lemnos_stream_buffer_handle(client, stream, frame.buffer, &fd, sink);
        if (status != LEMNOS_OK || fd < 0) return fail("buffer handle", status);
        void *map = mmap(NULL, FRAME_BYTES, PROT_READ, MAP_SHARED, fd, 0);
        close(fd);
        if (map == MAP_FAILED) return fail("map the handle", -errno);
        int same = frame_is_right(map, FRAME_BYTES, frame.sequence);
        munmap(map, FRAME_BYTES);
        if (!same) return fail("frame through its handle", -1);
        status = lemnos_stream_release(client, stream, frame.buffer, sink);
        if (status != LEMNOS_OK) return fail("release", status);
    }
    printf("frames 6 in place, by copy and by handle\n");

    uint8_t byte = 1;
    status = lemnos_stream_write(client, stream, 0, 0, &byte, 1, LEMNOS_NO_SINK);
    if (status != -EACCES) return fail("write on a stream taken read-only", status);
    uint8_t two[2];
    status = lemnos_stream_read(client, stream, 0, FRAME_BYTES - 1, two, 2, LEMNOS_NO_SINK);
    if (status != -EINVAL) return fail("read past a buffer", status);
    status = lemnos_stream_read(client, stream, info.buffers, 0, two, 1, LEMNOS_NO_SINK);
    if (status != -EINVAL) return fail("read of no buffer", status);
    status = lemnos_stream_release(client, stream, 0, LEMNOS_NO_SINK);
    if (status != -EINVAL) return fail("release of a frame not held", status);
    int32_t fd = -1;
    status = lemnos_stream_buffer_handle(client, stream, info.buffers, &fd, LEMNOS_NO_SINK);
    if (status != -EINVAL) return fail("handle of no buffer", status);
    printf("refused write=%d past=%d none=%d\n", -EACCES, -EINVAL, -EINVAL);

    status = lemnos_stream_close(client, stream, sink);
    if (status != LEMNOS_OK) return fail("close frames", status);
    status = lemnos_stream_info(client, stream, &info);
    if (status != LEMNOS_E_STALE_HANDLE) return fail("closed stream", status);
    return 0;
}

static int jobs(LemnosHandle client) {
    LemnosMessageSink sink = {message, collect};
    LemnosHandle stream = 0;
    int32_t status = lemnos_stream_open(client, 0, "input", LEMNOS_STREAM_WRITABLE, &stream, sink);
    if (status != LEMNOS_OK) return fail("open input", status);
    LemnosStreamInfo info = {sizeof info, 0, 0, 0, 0, 0, 0, 0};
    status = lemnos_stream_info(client, stream, &info);
    if (status != LEMNOS_OK) return fail("input info", status);
    if (!(info.flags & LEMNOS_STREAM_WRITABLE)) return fail("input writable", -1);
    for (uint32_t round = 0; round < 6; round++) {
        uint32_t buffer = round % info.buffers;
        uint8_t image[INPUT_BYTES];
        for (int i = 0; i < INPUT_BYTES; i++) image[i] = (uint8_t)(round + i * 3);
        status = lemnos_stream_write(client, stream, buffer, 0, image, sizeof image, sink);
        if (status != LEMNOS_OK) return fail("write", status);
        uint8_t job[4];
        memcpy(job, &buffer, 4);
        status = lemnos_stream_post_job(client, stream, job, sizeof job, sink);
        if (status != 1) return fail("post job", status);
        LemnosFrame frame = {sizeof frame, 0, 0, 0, 0, 0, 0};
        const uint8_t *data = NULL;
        status = lemnos_stream_next_frame(client, stream, 5000, &frame, &data, sink);
        if (status != 1) return fail("job's frame", status);
        if (frame.buffer != buffer || frame.len != INPUT_BYTES) return fail("job's buffer", -1);
        for (int i = 0; i < INPUT_BYTES; i++)
            if (data[i] != (uint8_t)~image[i]) return fail("what the producer made", -1);
        status = lemnos_stream_release(client, stream, frame.buffer, sink);
        if (status != LEMNOS_OK) return fail("release job frame", status);
    }
    printf("jobs 6 written, posted and made\n");
    uint8_t too_long[INPUT_BYTES + 1] = {0};
    status = lemnos_stream_write(client, stream, 0, 0, too_long, sizeof too_long, LEMNOS_NO_SINK);
    if (status != -EINVAL) return fail("write past a buffer", status);
    status = lemnos_stream_close(client, stream, sink);
    if (status != LEMNOS_OK) return fail("close input", status);

    status = lemnos_stream_open(client, 0, "plain", 0, &stream, sink);
    if (status != LEMNOS_OK) return fail("open plain", status);
    status = lemnos_stream_info(client, stream, &info);
    if (status != LEMNOS_OK || (info.flags & LEMNOS_STREAM_BUFFER_HANDLES))
        return fail("plain info", status);
    int32_t fd = -1;
    status = lemnos_stream_buffer_handle(client, stream, 0, &fd, LEMNOS_NO_SINK);
    if (status != -EOPNOTSUPP) return fail("handle on shared memory", status);
    printf("plain handle status=%d\n", status);
    status = lemnos_stream_close(client, stream, sink);
    if (status != LEMNOS_OK) return fail("close plain", status);
    status = lemnos_stream_open(client, 0, "nothing", 0, &stream, LEMNOS_NO_SINK);
    if (status != -ENOENT) return fail("open a stream there isn't", status);
    status = lemnos_stream_open(client, 0, "frames", 1u << 7, &stream, LEMNOS_NO_SINK);
    if (status != -EINVAL) return fail("open with an unknown flag", status);
    return 0;
}

/* A client closed with a stream open and a frame held gives the stream up. */
static int closed_holding(const char *socket) {
    LemnosMessageSink sink = {message, collect};
    LemnosHandle client = 0;
    int32_t status = open_client(socket, &client);
    if (status != LEMNOS_OK) return fail("open client", status);
    LemnosHandle stream = 0;
    status = lemnos_stream_open(client, 0, "frames", 0, &stream, sink);
    if (status != LEMNOS_OK) return fail("open frames again", status);
    LemnosFrame frame = {sizeof frame, 0, 0, 0, 0, 0, 0};
    status = lemnos_stream_next_frame(client, stream, 5000, &frame, NULL, sink);
    if (status != 1) return fail("a frame to hold", status);
    status = lemnos_client_close(client);
    if (status != LEMNOS_OK) return fail("close holding", status);

    status = open_client(socket, &client);
    if (status != LEMNOS_OK) return fail("open client", status);
    /* The service lets go of the stream when it sees the connection end. */
    for (int tries = 0;; tries++) {
        status = lemnos_stream_open(client, 0, "frames", 0, &stream, LEMNOS_NO_SINK);
        if (status != -EBUSY || tries == 200) break;
        struct timespec pause = {0, 10 * 1000 * 1000};
        nanosleep(&pause, NULL);
    }
    if (status != LEMNOS_OK) return fail("taken again", status);
    status = lemnos_stream_next_frame(client, stream, 5000, &frame, NULL, sink);
    if (status != 1) return fail("a frame after", status);
    printf("taken again after a close holding a frame\n");
    lemnos_client_close(client);
    return 0;
}

static int packages(const char *socket) {
    LemnosMessageSink sink = {message, collect};
    LemnosHandle client = 0;
    int32_t status = open_client(socket, &client);
    if (status != LEMNOS_OK) return fail("open package client", status);
    uint32_t mode = 99;
    int32_t count = lemnos_package_list(client, &mode, sink);
    if (count < 0) return fail("package list", count);
    char note[128];
    size_t len = 0;
    status = lemnos_package_text(client, 0, LEMNOS_PACKAGE_NOTE, note, sizeof note, &len);
    if (status != LEMNOS_OK) return fail("note", status);
    printf("packages=%d mode=%u note=%s\n", count, mode, note);
    for (int32_t i = 0; i < count; i++) {
        LemnosPackageInfo info = {sizeof info, 0};
        status = lemnos_package_info(client, (uint32_t)i, &info);
        if (status != LEMNOS_OK) return fail("package info", status);
        char path[128], driver[64], reason[128];
        status = lemnos_package_text(client, (uint32_t)i, LEMNOS_PACKAGE_PATH, path, sizeof path, &len);
        if (status != LEMNOS_OK) return fail("path", status);
        status = lemnos_package_text(client, (uint32_t)i, LEMNOS_PACKAGE_DRIVER, driver, sizeof driver, &len);
        if (status != LEMNOS_OK) return fail("driver", status);
        status = lemnos_package_text(client, (uint32_t)i, LEMNOS_PACKAGE_REASON, reason, sizeof reason, &len);
        if (status != LEMNOS_OK) return fail("reason", status);
        printf("package %s loaded=%u driver=%s reason=%s\n", path, info.loaded, driver, reason);
    }
    LemnosPackageInfo past = {sizeof past, 0};
    status = lemnos_package_info(client, (uint32_t)count, &past);
    if (status != -ENOENT) return fail("package past the list", status);
    lemnos_client_close(client);
    return 0;
}

int main(int argc, char **argv) {
    if (argc < 3) return 2;
    LemnosAbiVersion mine = LEMNOS_VERSION_INIT;
    int32_t status = lemnos_check_version(&mine);
    if (status != LEMNOS_OK) return fail("version", status);

    LemnosHandle client = 0;
    status = open_client(argv[1], &client);
    if (status != LEMNOS_OK) return fail("open", status);
    if (lemnos_device_count(client) < 1) return fail("devices", -1);
    if (frames(client) || jobs(client)) return 1;
    status = lemnos_client_close(client);
    if (status != LEMNOS_OK) return fail("close", status);
    if (closed_holding(argv[1]) || packages(argv[2])) return 1;
    printf("done\n");
    return 0;
}
