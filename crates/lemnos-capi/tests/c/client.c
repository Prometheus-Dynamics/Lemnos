/* A C client of the Lemnos surface: handshake, list, one-shot read, three
 * streamed readings, close. Prints what it read; exits non-zero on the first
 * failure, with the surface's message. */
#include <math.h>
#include <stdio.h>
#include <string.h>

#include "lemnos.h"

static void collect(void *context, const uint8_t *text, size_t len) {
    char *out = context;
    size_t used = strlen(out);
    size_t room = 255 - used;
    if (len > room) len = room;
    memcpy(out + used, text, len);
    out[used + len] = 0;
}

static int fail(const char *what, int32_t status, const char *message) {
    fprintf(stderr, "%s: status %d %s\n", what, status, message);
    return 1;
}

int main(int argc, char **argv) {
    if (argc < 2) return 2;
    char message[256] = {0};
    LemnosMessageSink sink = {message, collect};

    LemnosAbiVersion mine = LEMNOS_VERSION_INIT;
    int32_t status = lemnos_check_version(&mine);
    if (status != LEMNOS_OK) return fail("version", status, "");

    LemnosHandle client = 0;
    status = lemnos_client_open(argv[1], "c-client", 0, &client, sink);
    if (status != LEMNOS_OK) return fail("open", status, message);

    int32_t count = lemnos_device_count(client);
    if (count < 1) return fail("count", count, "");
    int32_t imu = -1;
    uint32_t channel_count = 0;
    for (int32_t i = 0; i < count; i++) {
        LemnosDeviceInfo info = {.struct_size = sizeof info};
        uint8_t id[64];
        size_t len = 0;
        status = lemnos_device_info(client, (uint32_t)i, &info, id, sizeof id, &len);
        if (status != LEMNOS_OK) return fail("device_info", status, "");
        if (strcmp((const char *)id, "imu") == 0) {
            imu = i;
            channel_count = info.channel_count;
        }
    }
    if (imu < 0) return fail("no imu", 0, "");

    int32_t x = -1, exponent = 0;
    for (uint32_t c = 0; c < channel_count; c++) {
        LemnosChannelInfo info = {.struct_size = sizeof info};
        uint8_t name[64];
        size_t len = 0;
        status = lemnos_channel_info(client, (uint32_t)imu, c, &info, name, sizeof name, &len);
        if (status != LEMNOS_OK) return fail("channel_info", status, "");
        if (strcmp((const char *)name, "acceleration.x") == 0) {
            x = (int32_t)c;
            exponent = info.exponent;
        }
    }
    if (x < 0) return fail("no acceleration.x", 0, "");

    LemnosReading reading = {.struct_size = sizeof reading};
    status = lemnos_read(client, (uint32_t)imu, &reading, sink);
    if (status != LEMNOS_OK) return fail("read", status, message);
    if (reading.values[x] == LEMNOS_NO_VALUE) return fail("no value", 0, "");
    printf("read acceleration.x=%g status=%u\n", reading.values[x] * pow(10, exponent),
           reading.status);

    uint32_t granted = 0;
    status = lemnos_subscribe(client, (uint32_t)imu, 10, &granted, sink);
    if (status != LEMNOS_OK) return fail("subscribe", status, message);
    int streamed = 0;
    for (int tries = 0; tries < 50 && streamed < 3; tries++) {
        uint32_t device = 0;
        status = lemnos_next_reading(client, 200, &device, &reading, sink);
        if (status == LEMNOS_OK && device == (uint32_t)imu) streamed++;
        else if (status != LEMNOS_OK && status != -110 && status != -11)
            return fail("next_reading", status, message);
    }
    printf("streamed %d granted_ms=%u\n", streamed, granted);
    if (streamed < 3) return fail("streamed too few", streamed, "");

    status = lemnos_client_close(client);
    if (status != LEMNOS_OK) return fail("close", status, "");
    status = lemnos_device_count(client);
    if (status != LEMNOS_E_STALE_HANDLE) return fail("stale", status, "");
    printf("closed\n");
    return 0;
}
