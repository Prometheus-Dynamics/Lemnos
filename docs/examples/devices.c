/* Lists the devices a Lemnos service has, through the C API.
 *
 *   cc devices.c $(pkg-config --cflags --libs lemnos-capi) -o devices
 *   ./devices SOCKET
 */
#include <stdio.h>
#include <string.h>

#include "lemnos.h"

/* Receives an error's text. It is not NUL-terminated. */
static void collect(void *context, const uint8_t *text, size_t len) {
    char *out = context;
    if (len > 255) len = 255;
    memcpy(out, text, len);
    out[len] = 0;
}

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: devices SOCKET\n");
        return 2;
    }
    char message[256] = {0};
    LemnosMessageSink sink = {message, collect};

    /* First, check that this program and the library agree on the interface. */
    LemnosAbiVersion mine = LEMNOS_VERSION_INIT;
    int32_t status = lemnos_check_version(&mine);
    if (status != LEMNOS_OK) {
        fprintf(stderr, "version: status %d\n", status);
        return 1;
    }

    LemnosHandle client = 0;
    status = lemnos_client_open(argv[1], "devices-example", 0, &client, sink);
    if (status != LEMNOS_OK) {
        fprintf(stderr, "open: status %d %s\n", status, message);
        return 1;
    }

    /* A count is a positive status. */
    int32_t count = lemnos_device_count(client);
    if (count < 0) {
        fprintf(stderr, "list: status %d\n", count);
        return 1;
    }
    for (int32_t device = 0; device < count; device++) {
        char name[64], driver[64], tags[256];
        size_t len = 0;
        LemnosDeviceInfo info = {sizeof info, 0, 0, 0};
        if (lemnos_device_text(client, device, LEMNOS_DEVICE_NAME, name, sizeof name, &len) != LEMNOS_OK ||
            lemnos_device_text(client, device, LEMNOS_DEVICE_DRIVER, driver, sizeof driver, &len) != LEMNOS_OK ||
            lemnos_device_text(client, device, LEMNOS_DEVICE_TAGS, tags, sizeof tags, &len) != LEMNOS_OK ||
            lemnos_device_info(client, device, &info) != LEMNOS_OK) {
            fprintf(stderr, "device %d: could not be read\n", device);
            return 1;
        }
        printf("%s (%s): state %u, health %u, provides %s\n", name, driver, info.state, info.health, tags);
    }
    return lemnos_client_close(client) == LEMNOS_OK ? 0 : 1;
}
