/*
 * The Lemnos C surface, version 1.3: the client of lemnosd.
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
#define LEMNOS_ABI_MINOR 3
#define LEMNOS_LAYOUT_HASH 0x736a24f433432c7ULL

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

/* Package modes, and lemnos_package_text fields. */
#define LEMNOS_PACKAGES_OFF 0
#define LEMNOS_PACKAGES_SIGNED 1
#define LEMNOS_PACKAGES_OPEN 2
#define LEMNOS_PACKAGE_PATH 0
#define LEMNOS_PACKAGE_DRIVER 1
#define LEMNOS_PACKAGE_REASON 2
#define LEMNOS_PACKAGE_NOTE 3

/* lemnos_stream_open flags, and LemnosStreamInfo flags. LEMNOS_STREAM_OPEN_UNMAPPED
 * is an open flag only, and not with LEMNOS_STREAM_WRITABLE. */
#define LEMNOS_STREAM_WRITABLE (1u << 0)
#define LEMNOS_STREAM_OPEN_UNMAPPED (1u << 1)
#define LEMNOS_STREAM_BUFFER_HANDLES (1u << 1)
#define LEMNOS_STREAM_OPEN (1u << 2)
#define LEMNOS_STREAM_UNMAPPED (1u << 3)

/* lemnos_layout_refusal's `which`: the shared structure whose layout a stream open refused. */
#define LEMNOS_LAYOUT_RING 0
#define LEMNOS_LAYOUT_MARKS 1
/* LemnosStreamRegion flags. */
#define LEMNOS_REGION_WRITABLE (1u << 0)
#define LEMNOS_REGION_DMA_BUF (1u << 1)
/* LemnosJobEvent.kind */
#define LEMNOS_JOB_PROGRESS 0
#define LEMNOS_JOB_DONE 1

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

typedef struct LemnosPackageInfo {
    uint32_t struct_size;
    uint32_t loaded;
} LemnosPackageInfo;

typedef struct LemnosStreamInfo {
    uint32_t struct_size;
    uint32_t flags;
    uint32_t buffers;
    uint32_t buffer_bytes;
    uint32_t frame_slots;
    uint32_t job_slots;
    uint32_t job_bytes;
    uint32_t planes; /* memory planes per buffer: 1 unless the board file gives more */
} LemnosStreamInfo;

/* One plane of one buffer, from lemnos_stream_region. fd is a new descriptor the
 * caller closes. */
typedef struct LemnosStreamRegion {
    uint32_t struct_size;
    uint32_t flags;          /* LEMNOS_REGION_WRITABLE, LEMNOS_REGION_DMA_BUF */
    uint32_t index;          /* pool index: buffer * planes + plane */
    int32_t fd;              /* the memory: a memfd of the pool, or a dma-buf */
    uint64_t offset;         /* where the plane starts in fd */
    uint64_t payload_bytes;  /* what the contract may use in the plane */
    uint64_t allocated_bytes;/* what exists from offset; at least payload_bytes */
    uint64_t tail_bytes;     /* slack past the payload for an importer: 0 today */
} LemnosStreamRegion;

/* sequence is the frame counter: the producer's frames from its start, numbered 0, 1, 2, ... The
 * consumer numbers and checks frames by it. identity is what the producer names the frame by: the job
 * identity a block's job carried back, or 0 for a producer that takes no job (a camera receiver, on
 * hardware and in the simulator). Lemnos does not read identity; do not use it as a frame number. */
typedef struct LemnosFrame {
    uint32_t struct_size;
    uint32_t buffer;
    uint32_t len;
    uint32_t reserved;
    uint64_t sequence;
    uint64_t identity;
    uint64_t timestamp_ns; /* CLOCK_MONOTONIC nanoseconds; 0 when the producer had no time */
} LemnosFrame;

/* What happened to a long job. Progress: done of total (total 0 when not known).
 * Done: status is how it ended and len the result's length. */
typedef struct LemnosJobEvent {
    uint32_t struct_size;
    uint32_t kind;
    uint64_t job;
    uint64_t done;
    uint64_t total;
    LemnosStatus status;
    uint32_t len;
} LemnosJobEvent;

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
/* Writes the base hash of the opened contract as the device describes it, and its additions level
 * (how many of its methods are additions). The bind check that a typed client runs is in Rust. */
LemnosStatus lemnos_contract_hash(LemnosHandle client, uint32_t opened, uint64_t *hash,
                                  uint32_t *level, LemnosMessageSink sink);
/* Returns the reply's length. */
LemnosStatus lemnos_call(LemnosHandle client, uint32_t opened, uint16_t method, const uint8_t *args,
                         size_t args_len, uint8_t *reply, size_t reply_capacity,
                         LemnosMessageSink sink);

/* Starts a long job: a method the description marks as one. Its progress and its end come
 * from lemnos_job_next. */
LemnosStatus lemnos_job_start(LemnosHandle client, uint32_t opened, uint16_t method,
                              const uint8_t *args, size_t args_len, uint64_t *job,
                              LemnosMessageSink sink);
/* The job then ends with -125 (ECANCELED), or with what it reached first. */
LemnosStatus lemnos_job_cancel(LemnosHandle client, uint64_t job, LemnosMessageSink sink);
/* Returns 1 with an event, 0 when timeout_ms passed without one. A job the connection lost
 * ends with -4 (EINTR). A result longer than capacity returns
 * LEMNOS_E_BUFFER_TOO_SMALL with the event filled in and the result dropped. */
LemnosStatus lemnos_job_next(LemnosHandle client, uint32_t timeout_ms, LemnosJobEvent *event,
                             uint8_t *result, size_t capacity);

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

/* What became of the service's driver packages. Fetches the list, sets *mode
 * (LEMNOS_PACKAGES_*) when mode is not NULL, and returns how many files the
 * service looked at, loaded or refused. The calls after it read that list. */
LemnosStatus lemnos_package_list(LemnosHandle client, uint32_t *mode, LemnosMessageSink sink);
LemnosStatus lemnos_package_info(LemnosHandle client, uint32_t package, LemnosPackageInfo *info);
/* LEMNOS_PACKAGE_DRIVER is empty for a refused file, LEMNOS_PACKAGE_REASON for a
 * loaded one. LEMNOS_PACKAGE_NOTE is the list's own: why the package directory
 * could not be used, or empty; package is not looked at for it. */
LemnosStatus lemnos_package_text(LemnosHandle client, uint32_t package, uint32_t field, char *out,
                                 size_t capacity, size_t *len);

/*
 * Frame streams (docs/streams.md). A stream has one consumer and is not taken
 * again after a reconnect: open it again. A stream handle belongs to the client
 * that opened it. Calls on one stream wait for each other, never for the
 * client's other calls or streams.
 */

/* Takes the named stream of a device, by its place in the list. With
 * LEMNOS_STREAM_WRITABLE the buffers are mapped to be written too, for a stream
 * the consumer fills (its contract says which). With LEMNOS_STREAM_OPEN_UNMAPPED
 * none of the buffers is mapped by this process: frames, their lengths
 * (lemnos_stream_plane_len), regions, pool page and generation all work, but
 * *data of lemnos_stream_next_frame is NULL, lemnos_stream_plane gives len 0,
 * and lemnos_stream_read and lemnos_stream_write fail with -EOPNOTSUPP. The
 * writable flag of a region is then read from the descriptor's access mode and
 * seals, not by mapping it (docs/streams.md). Any other combination of flags is
 * -EINVAL. */
LemnosStatus lemnos_stream_open(LemnosHandle client, uint32_t device, const char *name,
                                uint32_t flags, LemnosHandle *stream, LemnosMessageSink sink);
/* After a lemnos_stream_open that returned LEMNOS_E_LAYOUT_MISMATCH: which shared structure refused
 * (LEMNOS_LAYOUT_RING or LEMNOS_LAYOUT_MARKS), the value it found and the value this build reads.
 * -ENOENT when the client's last open refused no layout. The next open on the client replaces it.
 * Writes the three outputs only on LEMNOS_OK. */
LemnosStatus lemnos_layout_refusal(LemnosHandle client, uint32_t *which, uint64_t *found,
                                   uint64_t *expected);
LemnosStatus lemnos_stream_info(LemnosHandle client, LemnosHandle stream, LemnosStreamInfo *info);
/* Returns 1 with a frame, 0 when timeout_ms passed or the host ended the stream
 * (LEMNOS_STREAM_OPEN then clears). 0 ms takes a frame only if one is there.
 * *data, when data is not NULL, is the frame's frame->len bytes, read in place
 * and valid until the frame is released or the stream closed. On a stream whose
 * buffers are in planes (info.planes above 1), or an unmapped one, *data is NULL:
 * read each plane with lemnos_stream_plane. Allocates nothing. */
LemnosStatus lemnos_stream_next_frame(LemnosHandle client, LemnosHandle stream, uint32_t timeout_ms,
                                      LemnosFrame *frame, const uint8_t **data,
                                      LemnosMessageSink sink);
/* Gives a held frame's buffer back to the producer, by its number. */
LemnosStatus lemnos_stream_release(LemnosHandle client, LemnosHandle stream, uint32_t buffer,
                                   LemnosMessageSink sink);
/* Copies len bytes at offset of a buffer out, or in (a stream opened
 * LEMNOS_STREAM_WRITABLE only). A buffer or range the stream doesn't have is
 * refused and nothing is copied. Write a buffer before posting the job that
 * names it, and not again until that job is done. */
LemnosStatus lemnos_stream_read(LemnosHandle client, LemnosHandle stream, uint32_t buffer,
                                size_t offset, uint8_t *out, size_t len, LemnosMessageSink sink);
LemnosStatus lemnos_stream_write(LemnosHandle client, LemnosHandle stream, uint32_t buffer,
                                 size_t offset, const uint8_t *data, size_t len,
                                 LemnosMessageSink sink);
/* Returns 1 when posted, 0 when too many jobs are waiting. */
LemnosStatus lemnos_stream_post_job(LemnosHandle client, LemnosHandle stream, const uint8_t *job,
                                    size_t len, LemnosMessageSink sink);
/* A new descriptor of a buffer's dma-buf, which the caller owns and closes:
 * -EOPNOTSUPP on a stream of shared memory (no LEMNOS_STREAM_BUFFER_HANDLES).
 * It keeps the memory past the stream's close; who may touch it when is as for
 * lemnos_stream_write and frames. */
LemnosStatus lemnos_stream_buffer_handle(LemnosHandle client, LemnosHandle stream, uint32_t buffer,
                                         int32_t *fd, LemnosMessageSink sink);
/* The part of the held frame in buffer that is in plane plane: *data and *len,
 * read in place and valid as lemnos_stream_next_frame's data. The planes hold the
 * frame's bytes end to end, so a plane past its end is empty. A buffer not held
 * or a plane the stream doesn't have is -EINVAL. Allocates nothing. */
LemnosStatus lemnos_stream_plane(LemnosHandle client, LemnosHandle stream, uint32_t buffer,
                                 uint32_t plane, const uint8_t **data, size_t *len);
/* The length of plane plane of the held frame in buffer: what lemnos_stream_plane
 * would give as *len, with no mapping of the buffers, so it works on an unmapped
 * stream. Same errors: -EINVAL for a buffer not held or a plane the stream doesn't
 * have. */
LemnosStatus lemnos_stream_plane_len(LemnosHandle client, LemnosHandle stream, uint32_t buffer,
                                     uint32_t plane, size_t *len);
/* A new descriptor of plane plane of a buffer, as lemnos_stream_buffer_handle
 * gives plane 0. */
LemnosStatus lemnos_stream_plane_handle(LemnosHandle client, LemnosHandle stream, uint32_t buffer,
                                        uint32_t plane, int32_t *fd, LemnosMessageSink sink);
/* Where plane plane of buffer is: fills *region (struct_size set). A buffer or
 * plane the stream doesn't have is -EINVAL. Valid for the stream's life; see
 * docs/streams.md for who may touch the memory and when. */
LemnosStatus lemnos_stream_region(LemnosHandle client, LemnosHandle stream, uint32_t buffer,
                                  uint32_t plane, LemnosStreamRegion *region,
                                  LemnosMessageSink sink);
/* A new read-only descriptor of the pool page, for a second reader of the pool. It
 * holds no reader slot and must not mark frames; see docs/streams.md. */
LemnosStatus lemnos_stream_pool_page(LemnosHandle client, LemnosHandle stream, int32_t *fd,
                                     LemnosMessageSink sink);
/* The pool's generation: it changes whenever the host makes a new pool for the
 * stream, so an older lease is on a pool that is gone. */
LemnosStatus lemnos_stream_generation(LemnosHandle client, LemnosHandle stream, uint32_t *generation);
/* How many frames the stream's reader was too late for, in total: frames overwritten, or whose
 * buffers the producer took back, before a lemnos_stream_next_frame read them. A reader that keeps
 * up never counts one; a lost frame never stops the frame behind it from being returned. */
LemnosStatus lemnos_stream_skipped(LemnosHandle client, LemnosHandle stream, uint64_t *skipped);
/* Releases every frame still held and gives the stream back. */
LemnosStatus lemnos_stream_close(LemnosHandle client, LemnosHandle stream, LemnosMessageSink sink);

#ifdef __cplusplus
}
#endif

#endif
