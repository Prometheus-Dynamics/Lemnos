/*
 * Lemnos C surface, version 1.0: the lemnosd device client.
 *
 * Every call returns an int32_t status: 0 success, -1 to -4095 a negated
 * Linux errno, LEMNOS_E_* (-4096 and below) Lemnos's own codes, and a
 * positive value a count where a call says so. Call lemnos_check_version
 * first with LEMNOS_VERSION_INIT.
 *
 * Output structs start with struct_size: set it to sizeof the struct before
 * the call. Devices are named by their index in the client's list
 * (lemnos_device_count), so a read passes no strings. A LemnosMessageSink
 * receives an error's text (UTF-8, not NUL-terminated, valid during the
 * callback); pass LEMNOS_NO_SINK to drop it.
 *
 * Kept in step with crates/lemnos-capi/src/surface.rs and lemnos-abi; the
 * crate's tests check this header's sizes, offsets and layout hash.
 */
#ifndef LEMNOS_H
#define LEMNOS_H

#include <stddef.h>
#include <stdint.h>

/* Status codes, handles, the handshake and message sinks: shared by every surface. */
#include "lemnos/abi.h"

#ifdef __cplusplus
extern "C" {
#endif

#define LEMNOS_ABI_MAJOR 1
#define LEMNOS_ABI_MINOR 0
#define LEMNOS_LAYOUT_HASH 0xfd630e784f3059feULL

#define LEMNOS_MAX_CHANNELS 16
#define LEMNOS_NO_VALUE INT32_MIN

/* Device statuses, classes, quantities and axes: the IPC wire's numbers. */
#define LEMNOS_STATUS_AVAILABLE 0
#define LEMNOS_STATUS_DEGRADED 1
#define LEMNOS_STATUS_FAULTED 2
#define LEMNOS_STATUS_MISSING 3

#define LEMNOS_CLASS_IMU 0
#define LEMNOS_CLASS_ACCELEROMETER 1
#define LEMNOS_CLASS_GYROSCOPE 2
#define LEMNOS_CLASS_MAGNETOMETER 3
#define LEMNOS_CLASS_POWER_MONITOR 4
#define LEMNOS_CLASS_TEMPERATURE 5
#define LEMNOS_CLASS_FAN 6
#define LEMNOS_CLASS_LENS 7
#define LEMNOS_CLASS_LIGHT 8
#define LEMNOS_CLASS_GPIO 9
#define LEMNOS_CLASS_ORIENTATION 10
#define LEMNOS_CLASS_OTHER 11
#define LEMNOS_CLASS_POWER_SWITCH 12

#define LEMNOS_QUANTITY_ACCELERATION 0
#define LEMNOS_QUANTITY_ANGULAR_RATE 1
#define LEMNOS_QUANTITY_MAGNETIC_FIELD 2
#define LEMNOS_QUANTITY_VOLTAGE 3
#define LEMNOS_QUANTITY_CURRENT 4
#define LEMNOS_QUANTITY_POWER 5
#define LEMNOS_QUANTITY_TEMPERATURE 6
#define LEMNOS_QUANTITY_ROTATIONAL_SPEED 7
#define LEMNOS_QUANTITY_RATIO 8
#define LEMNOS_QUANTITY_POSITION 9
#define LEMNOS_QUANTITY_LEVEL 10
#define LEMNOS_QUANTITY_MODE 11
#define LEMNOS_QUANTITY_COLOR 12
#define LEMNOS_QUANTITY_ANGLE 13
#define LEMNOS_QUANTITY_PRESSURE 14
#define LEMNOS_QUANTITY_FREQUENCY 15

#define LEMNOS_AXIS_NONE 0
#define LEMNOS_AXIS_X 1
#define LEMNOS_AXIS_Y 2
#define LEMNOS_AXIS_Z 3

#define LEMNOS_VERSION_INIT \
    { sizeof(LemnosAbiVersion), LEMNOS_ABI_MAJOR, LEMNOS_ABI_MINOR, LEMNOS_LAYOUT_HASH }

typedef struct LemnosDeviceInfo {
    uint32_t struct_size;
    uint32_t device_class;
    uint32_t status;
    uint32_t channel_count;
    uint32_t control_count;
    uint32_t pixels;
} LemnosDeviceInfo;

/* A raw value v is v * 10^exponent in the quantity's unit. */
typedef struct LemnosChannelInfo {
    uint32_t struct_size;
    uint32_t quantity;
    uint32_t axis;
    int32_t exponent;
} LemnosChannelInfo;

typedef struct LemnosReading {
    uint32_t struct_size;
    uint32_t status;
    /* Microseconds on CLOCK_BOOTTIME when the read started. */
    uint64_t timestamp_us;
    uint32_t channel_count;
    uint32_t reserved;
    int32_t values[LEMNOS_MAX_CHANNELS];
} LemnosReading;

LemnosAbiVersion lemnos_version(void);
int32_t lemnos_check_version(const LemnosAbiVersion *caller);

/* timeout_ms bounds each request (0: the default). */
int32_t lemnos_client_open(const char *socket, const char *name, uint32_t timeout_ms,
                           LemnosHandle *client, LemnosMessageSink sink);
int32_t lemnos_client_close(LemnosHandle client);

/* The number of devices (a positive status). */
int32_t lemnos_device_count(LemnosHandle client);
/* The id is NUL-terminated; *id_len is its length without the NUL.
 * LEMNOS_E_BUFFER_TOO_SMALL when it does not fit (info is written either way). */
int32_t lemnos_device_info(LemnosHandle client, uint32_t device, LemnosDeviceInfo *info,
                           uint8_t *id, size_t id_capacity, size_t *id_len);
int32_t lemnos_channel_info(LemnosHandle client, uint32_t device, uint32_t channel,
                            LemnosChannelInfo *info, uint8_t *name, size_t name_capacity,
                            size_t *name_len);

/* A one-shot read. */
int32_t lemnos_read(LemnosHandle client, uint32_t device, LemnosReading *reading,
                    LemnosMessageSink sink);
/* period_ms 0 ends the subscription; *granted_ms may be NULL. */
int32_t lemnos_subscribe(LemnosHandle client, uint32_t device, uint32_t period_ms,
                         uint32_t *granted_ms, LemnosMessageSink sink);
/* LEMNOS_OK with *device and *reading; -ETIMEDOUT when none came within timeout_ms;
 * -EAGAIN when something else came first (call again). */
int32_t lemnos_next_reading(LemnosHandle client, uint32_t timeout_ms, uint32_t *device,
                            LemnosReading *reading, LemnosMessageSink sink);

#ifdef __cplusplus
}
#endif

#endif /* LEMNOS_H */
