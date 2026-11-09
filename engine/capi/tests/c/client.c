/* A C client of the Lemnos surface: handshake, list, a call, three subscribed
 * values, what a device says about itself, power, close. It prints what it
 * saw and exits non-zero on the first failure, with the surface's message. */
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
    printf("devices=%d\n", count);

    char name[64];
    size_t len = 0;
    status = lemnos_device_text(client, 0, LEMNOS_DEVICE_NAME, name, sizeof name, &len);
    if (status != LEMNOS_OK) return fail("name", status, "");
    LemnosDeviceInfo info = {sizeof info, 0, 0, 0};
    status = lemnos_device_info(client, 0, &info);
    if (status != LEMNOS_OK) return fail("info", status, "");
    printf("first=%s state=%u\n", name, info.state);

    uint32_t opened = 0;
    status = lemnos_open(client, name, "example.counter/1", &opened, sink);
    if (status != LEMNOS_OK) return fail("open contract", status, message);

    uint8_t reply[16];
    status = lemnos_call(client, opened, 1, NULL, 0, reply, sizeof reply, sink);
    if (status != 4) return fail("call", status, message);
    uint32_t got;
    memcpy(&got, reply, 4);
    printf("got=%u\n", got);

    LemnosSubscription sub = {sizeof sub, 0, 0, 0};
    status = lemnos_subscribe(client, opened, 1, 1000, 0, &sub, sink);
    if (status != LEMNOS_OK) return fail("subscribe", status, message);

    int streamed = 0;
    while (streamed < 3) {
        LemnosValue value = {sizeof value, 0, 0, 0};
        status = lemnos_next_value(client, 2000, &value, reply, sizeof reply, sink);
        if (status < 0) return fail("value", status, message);
        if (status == 1 && value.subscription == sub.id) streamed++;
    }
    printf("streamed %d\n", streamed);
    status = lemnos_unsubscribe(client, sub.id, sink);
    if (status != LEMNOS_OK) return fail("unsubscribe", status, message);

    int32_t items = lemnos_item_count(client, 0, sink);
    if (items < 1) return fail("items", items, message);
    LemnosItemInfo item = {sizeof item, 0, 0, 0, 0, 0, 0, 0};
    status = lemnos_item_info(client, 0, 0, &item);
    if (status != LEMNOS_OK) return fail("item", status, "");
    char label[64], tag[64];
    status = lemnos_item_text(client, 0, 0, LEMNOS_ITEM_NAME, label, sizeof label, &len);
    if (status != LEMNOS_OK) return fail("item name", status, "");
    status = lemnos_item_text(client, 0, 0, LEMNOS_ITEM_TAG, tag, sizeof tag, &len);
    if (status != LEMNOS_OK) return fail("item tag", status, "");
    printf("item=%s method=%u of %s\n", label, item.method, tag);

    status = lemnos_set_power(client, 0, 0, sink);
    if (status != LEMNOS_OK) return fail("off", status, message);
    message[0] = 0;
    status = lemnos_call(client, opened, 1, NULL, 0, reply, sizeof reply, sink);
    if (status >= 0) return fail("call while off", status, "should have failed");
    printf("off status=%d\n", status);
    status = lemnos_set_power(client, 0, 1, sink);
    if (status != LEMNOS_OK) return fail("on", status, message);

    status = lemnos_client_close(client);
    if (status != LEMNOS_OK) return fail("close", status, "");
    status = lemnos_device_count(client);
    if (status != LEMNOS_E_STALE_HANDLE) return fail("stale", status, "");
    printf("closed\n");
    return 0;
}
