/*
 * The Lemnos C surface, version 1.0: the client of lemnosd.
 *
 * Every call returns an int32_t status: 0 success, -1 to -4095 a negated Linux
 * errno, LEMNOS_E_* (-4096 and below) Lemnos's own codes, and a positive value
 * a count where a call says so. Call lemnos_check_version first with
 * LEMNOS_VERSION_INIT.
 *
 * Output structs start with struct_size: set it to sizeof the struct before
 * the call. A LemnosMessageSink receives an error's text (UTF-8, not
 * NUL-terminated, valid during the callback); pass LEMNOS_NO_SINK to drop it.
 *
 * What a method's arguments and reply mean belongs to its contract; docs/wire.md
 * says how each type is encoded. Kept in step with engine/capi/src/surface.rs;
 * that crate's tests check this header's sizes, offsets and layout hash.
 */
#ifndef LEMNOS_H
#define LEMNOS_H

#include <stddef.h>
#include <stdint.h>

#include "lemnos/abi.h"

#ifdef __cplusplus
extern "C" {
#endif

#define LEMNOS_ABI_MAJOR 1
#define LEMNOS_ABI_MINOR 0
#define LEMNOS_LAYOUT_HASH 0x10174a7e6b2602e2ULL

#define LEMNOS_VERSION_INIT \
    { sizeof(LemnosAbiVersion), LEMNOS_ABI_MAJOR, LEMNOS_ABI_MINOR, LEMNOS_LAYOUT_HASH }

/* Device states and health. */
#define LEMNOS_STATE_RUNNING 0
#define LEMNOS_STATE_BACKING_OFF 1
#define LEMNOS_STATE_OFF 2
#define LEMNOS_HEALTH_OK 0
#define LEMNOS_HEALTH_DEGRADED 1
#define LEMNOS_HEALTH_FAILED 2

/* lemnos_device_text fields. */
#define LEMNOS_DEVICE_NAME 0
#define LEMNOS_DEVICE_DRIVER 1
#define LEMNOS_DEVICE_TAGS 2
#define LEMNOS_DEVICE_REASON 3

/* Item kinds and lemnos_item_text fields. */
#define LEMNOS_ITEM_VALUE 0
#define LEMNOS_ITEM_CONTROL 1
#define LEMNOS_ITEM_ACTION 2
#define LEMNOS_ITEM_NAME 0
#define LEMNOS_ITEM_UNIT 1
#define LEMNOS_ITEM_TAG 2

typedef struct LemnosDeviceInfo {
    uint32_t struct_size;
    uint32_t state;
    uint32_t health;
    uint32_t reserved;
} LemnosDeviceInfo;

typedef struct LemnosSubscription {
    uint32_t struct_size;
    uint32_t reserved;
    uint64_t id;
    uint64_t period_us;
} LemnosSubscription;

typedef struct LemnosValue {
    uint32_t struct_size;
    uint32_t len;
    uint64_t subscription;
    uint64_t stamp_ns;
} LemnosValue;

typedef struct LemnosItemInfo {
    uint32_t struct_size;
    uint32_t kind;
    uint32_t contract;
    uint32_t method;
    int32_t exponent;
    uint32_t has_range;
    int64_t min;
    int64_t max;
} LemnosItemInfo;

LemnosAbiVersion lemnos_version(void);
LemnosStatus lemnos_check_version(const LemnosAbiVersion *caller);

/* timeout_ms applies to each later request too; 0 keeps the default. */
LemnosStatus lemnos_client_open(const char *socket, const char *name, uint32_t timeout_ms,
                                LemnosHandle *client, LemnosMessageSink sink);
LemnosStatus lemnos_client_close(LemnosHandle client);

/* Refreshes the list and returns how many devices it has. */
LemnosStatus lemnos_device_count(LemnosHandle client);
LemnosStatus lemnos_device_info(LemnosHandle client, uint32_t device, LemnosDeviceInfo *info);
/* Writes NUL-terminated text. *len is its length without the NUL, even when it did not fit. */
LemnosStatus lemnos_device_text(LemnosHandle client, uint32_t device, uint32_t field, char *out,
                                size_t capacity, size_t *len);

/* Opens a contract on the named device, or on any device with it when device is "". */
LemnosStatus lemnos_open(LemnosHandle client, const char *device, const char *tag, uint32_t *opened,
                         LemnosMessageSink sink);
/* Returns the reply's length. */
LemnosStatus lemnos_call(LemnosHandle client, uint32_t opened, uint16_t method, const uint8_t *args,
                         size_t args_len, uint8_t *reply, size_t reply_capacity,
                         LemnosMessageSink sink);

LemnosStatus lemnos_subscribe(LemnosHandle client, uint32_t opened, uint16_t method,
                              uint64_t period_us, uint32_t only_changes,
                              LemnosSubscription *subscription, LemnosMessageSink sink);
LemnosStatus lemnos_unsubscribe(LemnosHandle client, uint64_t subscription, LemnosMessageSink sink);
/* Returns 1 with a value, 0 when timeout_ms passed without one. */
LemnosStatus lemnos_next_value(LemnosHandle client, uint32_t timeout_ms, LemnosValue *value,
                               uint8_t *into, size_t capacity, LemnosMessageSink sink);

LemnosStatus lemnos_set_power(LemnosHandle client, uint32_t device, uint32_t on,
                              LemnosMessageSink sink);

/* What a device says about itself. Returns how many items it has. */
LemnosStatus lemnos_item_count(LemnosHandle client, uint32_t device, LemnosMessageSink sink);
LemnosStatus lemnos_item_info(LemnosHandle client, uint32_t device, uint32_t item,
                              LemnosItemInfo *info);
LemnosStatus lemnos_item_text(LemnosHandle client, uint32_t device, uint32_t item, uint32_t field,
                              char *out, size_t capacity, size_t *len);

#ifdef __cplusplus
}
#endif

#endif
