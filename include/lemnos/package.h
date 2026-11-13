/*
 * The Lemnos driver package boundary, version 2.0: what a compiled driver file
 * (a package) and the service that loads it say to each other. Both are in one
 * process, and nothing but the types here crosses.
 *
 * A package is a shared object that exports one data symbol, lemnos_package, a
 * LemnosPackageDescriptor. The host reads its magic and version from the file
 * before it runs anything of the package's, then calls register_driver once
 * for the driver's manifest and its LemnosDriverTable. When the driver binds,
 * the host hands it a LemnosHostTable: what the driver is given comes back as
 * handles only the host can resolve, used through the table of their kind.
 *
 * Written to match the lemnos-abi crate's package module, and checked against
 * it: the crate's tests compile this header and compare every size, offset and
 * value with the Rust types, and a test loads a package written in C against
 * it. `docs/writing-a-driver.md` describes a package; a driver written in Rust
 * needs none of this, only `lemnos_core::export_package!`.
 *
 * Rules for a package not built from the crate:
 *  - No function may unwind or longjmp across the boundary. A function that
 *    cannot finish returns a status.
 *  - A pointer and length pair must be what it says: readable (or writable,
 *    for a LemnosSliceMut and an out pointer) for that many elements for the
 *    whole call. The receiver trusts them. With a length of 0 the pointer may
 *    be anything, null included, and is not to be read.
 *  - Text is UTF-8, not NUL-terminated.
 *  - Everything the package hands over in register_driver (the manifest, the
 *    table, every string) stays valid while the package is loaded, and a
 *    package is never unloaded.
 *  - The host calls one function at a time on one driver, never after drop.
 */
#ifndef LEMNOS_PACKAGE_H
#define LEMNOS_PACKAGE_H

#include <stddef.h>
#include <stdint.h>

#include "abi.h"

#ifdef __cplusplus
extern "C" {
#endif

/* The version of the boundary (lemnos_abi::package::PACKAGE_VERSION). */
#define LEMNOS_PACKAGE_MAJOR 2
#define LEMNOS_PACKAGE_MINOR 0
/*
 * The layout hash follows the target, because the types hold pointers. It is
 * given here for 64-bit targets. For a 32-bit target take it from the crate
 * built for that target (lemnos_abi::package::PACKAGE_LAYOUT_HASH).
 */
#if UINTPTR_MAX == 0xffffffffffffffffULL
#define LEMNOS_PACKAGE_LAYOUT_HASH 0x171f8780472f4930ULL
#endif

/* The first field of a descriptor: the bytes "LEMNOSPK". */
#define LEMNOS_PACKAGE_MAGIC 0x4b50534f4e4d454cULL
/* The one symbol a package exports. */
#define LEMNOS_PACKAGE_ENTRY "lemnos_package"

#ifdef LEMNOS_PACKAGE_LAYOUT_HASH
#define LEMNOS_PACKAGE_VERSION_INIT \
    { sizeof(LemnosAbiVersion), LEMNOS_PACKAGE_MAJOR, LEMNOS_PACKAGE_MINOR, LEMNOS_PACKAGE_LAYOUT_HASH }
#endif

/* ---- Pointers across the boundary ---- */

/* Text: UTF-8, not NUL-terminated. */
typedef struct LemnosStr {
    const uint8_t *ptr;
    size_t len;
} LemnosStr;

/* Values the receiver only reads. `len` counts elements. */
typedef struct LemnosSlice {
    const void *ptr;
    size_t len;
} LemnosSlice;

/* Room the receiver fills. `len` counts elements. */
typedef struct LemnosSliceMut {
    void *ptr;
    size_t len;
} LemnosSliceMut;

/* A file descriptor's number, lent: -1 is none. */
typedef int32_t LemnosFd;
#define LEMNOS_FD_NONE (-1)

/* ---- The manifest ---- */

/* LemnosPackageNeed.kind and LemnosHostTable.take's kind. */
#define LEMNOS_TAKE_DEVICE 0
#define LEMNOS_TAKE_I2C 1
#define LEMNOS_TAKE_SPI 2
#define LEMNOS_TAKE_LINE 3
#define LEMNOS_TAKE_PWM 4
#define LEMNOS_TAKE_UART 5
#define LEMNOS_TAKE_USB_DEVICE 6
#define LEMNOS_TAKE_USB_FUNCTION 7
#define LEMNOS_TAKE_REGISTERS 8
#define LEMNOS_TAKE_INTERRUPT 9
#define LEMNOS_TAKE_DMA 10
#define LEMNOS_TAKE_CLOCKS 11
#define LEMNOS_TAKE_RESET 12
#define LEMNOS_TAKE_STREAM 13
/* take only: an SPI device with per-transfer settings. Its need is LEMNOS_TAKE_SPI. */
#define LEMNOS_TAKE_RAW_SPI 14

/* One thing the driver needs, by the name a board file gives it. */
typedef struct LemnosPackageNeed {
    LemnosStr name;
    /* LEMNOS_TAKE_DEVICE to LEMNOS_TAKE_STREAM. */
    uint32_t kind;
    /* Not 0: the driver binds without it. */
    uint32_t optional;
    /* The contract a needed device provides, as name/major. Empty for a bus. */
    LemnosStr tag;
} LemnosPackageNeed;

/*
 * What a value looks like on the wire. kind: 0 unit, 1 bool, 2 u8, 3 u16,
 * 4 u32, 5 u64, 6 i8, 7 i16, 8 i32, 9 i64, 10 f32, 11 f64, 12 text, 13 bytes,
 * 14 option (of: one LemnosPackageShape), 15 array (of, count), 16 record
 * (fields: LemnosPackageField).
 */
typedef struct LemnosPackageShape {
    uint32_t kind;
    uint32_t count;
    LemnosSlice of;
    LemnosSlice fields;
} LemnosPackageShape;

typedef struct LemnosPackageField {
    LemnosStr name;
    LemnosPackageShape shape;
} LemnosPackageField;

typedef struct LemnosPackageMethod {
    uint16_t id;
    /* Not 0: the method also fills a caller-supplied byte buffer. */
    uint16_t has_out_buffer;
    uint32_t reserved;
    LemnosStr name;
    /* LemnosPackageField. */
    LemnosSlice params;
    LemnosPackageShape returns;
} LemnosPackageMethod;

/* One contract the driver provides: its id and version as name/major. */
typedef struct LemnosPackageContract {
    LemnosStr tag;
    /* LemnosPackageMethod. */
    LemnosSlice methods;
} LemnosPackageContract;

typedef struct LemnosPackageManifest {
    LemnosStr name;
    /* LemnosPackageNeed. */
    LemnosSlice needs;
    /* LemnosPackageContract. */
    LemnosSlice provides;
} LemnosPackageManifest;

/* ---- What a driver says about itself ---- */

#define LEMNOS_HEALTH_OK 0
#define LEMNOS_HEALTH_DEGRADED 1
#define LEMNOS_HEALTH_FAILED 2
/* What the driver was bound with is dead: the host drops it and binds again. */
#define LEMNOS_HEALTH_GONE 3
typedef struct LemnosPackageHealth {
    uint32_t kind;
    uint32_t reserved;
    /* Valid while the package is loaded. */
    LemnosStr reason;
} LemnosPackageHealth;

#define LEMNOS_ITEM_HAS_MIN 1
#define LEMNOS_ITEM_HAS_MAX 2
#define LEMNOS_ITEM_HAS_FIELD 4
/* One thing a device offers, tied to the method that does it. */
typedef struct LemnosPackageItem {
    /* 0 value, 1 control, 2 action. */
    uint8_t kind;
    uint8_t unit;
    int8_t exponent;
    uint8_t flags;
    /* Position of the method's contract in the manifest. */
    uint8_t contract;
    uint8_t reserved;
    uint16_t method;
    int64_t min;
    int64_t max;
    LemnosStr name;
    LemnosStr field;
} LemnosPackageItem;

#define LEMNOS_SETTING_INT 0
#define LEMNOS_SETTING_FLOAT 1
#define LEMNOS_SETTING_BOOL 2
#define LEMNOS_SETTING_TEXT 3
/* One setting from the device's board entry, lent for the bind. */
typedef struct LemnosPackageSetting {
    LemnosStr key;
    uint32_t kind;
    uint32_t reserved;
    /* The integer, or 0 or 1 for a boolean. */
    int64_t int_value;
    double float_value;
    LemnosStr text;
} LemnosPackageSetting;

/* ---- What the resource tables carry ---- */

/* The most steps one I2C or SPI transaction carries, and segments one raw SPI transfer. */
#define LEMNOS_MAX_TRANSFER_STEPS 16
/* The most packets one isochronous transfer carries. */
#define LEMNOS_MAX_ISO_PACKETS 64
/* A timeout that never comes. */
#define LEMNOS_NO_TIMEOUT UINT64_MAX
/* stream.next_buffer when the consumer has returned none. */
#define LEMNOS_NO_BUFFER UINT32_MAX
/* stream.next_job when the consumer has sent none. */
#define LEMNOS_NO_JOB SIZE_MAX
/* stream.buffer_kind: plain shared memory, or every buffer behind a handle. */
#define LEMNOS_BUFFERS_PLAIN 0
#define LEMNOS_BUFFERS_HANDLES 1

#define LEMNOS_TRANSFER_WRITE 0
#define LEMNOS_TRANSFER_READ 1
/* SPI: sends `write` while `read` fills. */
#define LEMNOS_TRANSFER_BOTH 2
/* SPI: sends what `read` holds and fills it with what came back. */
#define LEMNOS_TRANSFER_IN_PLACE 3
/* SPI: waits delay_ns. */
#define LEMNOS_TRANSFER_DELAY 4
/* One step of an I2C or SPI transaction. I2C takes only write and read. */
typedef struct LemnosPackageTransfer {
    uint32_t kind;
    uint32_t delay_ns;
    /* Bytes. */
    LemnosSlice write;
    LemnosSliceMut read;
} LemnosPackageTransfer;

/* One transfer of a raw SPI transaction: tx is clocked out while rx fills. */
typedef struct LemnosPackageSpiSegment {
    /* Clock polarity and phase, 0 to 3. */
    uint8_t mode;
    /* 0: 8. */
    uint8_t bits_per_word;
    /* Not 0: deselect the chip after this transfer. */
    uint8_t cs_change;
    uint8_t reserved;
    /* 0: the device's default. */
    uint32_t speed_hz;
    uint16_t delay_us;
    uint16_t reserved2[3];
    LemnosSlice tx;
    LemnosSliceMut rx;
} LemnosPackageSpiSegment;

typedef struct LemnosPackageLineConfig {
    /* 0 input, 1 output. */
    uint8_t direction;
    uint8_t initial;
    uint8_t active_low;
    /* 0 as it is, 1 pull up, 2 pull down, 3 disabled. */
    uint8_t bias;
    /* 0 push-pull, 1 open drain, 2 open source. */
    uint8_t drive;
    /* 0 none, 1 rising, 2 falling, 3 both. */
    uint8_t edge;
    uint16_t reserved;
    uint32_t debounce_us;
} LemnosPackageLineConfig;

typedef struct LemnosPackageEdge {
    /* 0: there was no edge. */
    uint32_t present;
    uint32_t rising;
    uint32_t seq;
    uint32_t reserved;
    uint64_t timestamp_ns;
} LemnosPackageEdge;

typedef struct LemnosPackagePwmConfig {
    uint64_t period_ns;
    uint64_t duty_ns;
    /* 0 normal, 1 inversed. */
    uint32_t polarity;
    uint32_t enabled;
} LemnosPackagePwmConfig;

typedef struct LemnosPackageInterruptEvent {
    /* 0: the timeout passed first. */
    uint32_t present;
    uint32_t count;
    uint64_t timestamp_ns;
} LemnosPackageInterruptEvent;

typedef struct LemnosPackageDmaBuffer {
    uint32_t id;
    uint32_t reserved;
    uint64_t len;
    /* The address as the block sees it. */
    uint64_t device_address;
} LemnosPackageDmaBuffer;

typedef struct LemnosPackageFrame {
    uint32_t buffer;
    uint32_t bytes;
    uint64_t sequence;
    uint64_t identity;
    uint64_t timestamp_ns;
} LemnosPackageFrame;

typedef struct LemnosPackageControlSetup {
    uint8_t request_type;
    uint8_t request;
    uint16_t value;
    uint16_t index;
    uint16_t reserved;
} LemnosPackageControlSetup;

typedef struct LemnosPackageIsoPacket {
    /* Bytes this packet may carry, set by the caller. */
    uint32_t capacity;
    /* Bytes it did carry, set by the transfer. */
    uint32_t received;
    uint32_t ok;
} LemnosPackageIsoPacket;

#define LEMNOS_USB_EVENT_NONE 0
#define LEMNOS_USB_EVENT_CONFIGURED 1
#define LEMNOS_USB_EVENT_DECONFIGURED 2
#define LEMNOS_USB_EVENT_SUSPENDED 3
#define LEMNOS_USB_EVENT_RESUMED 4
#define LEMNOS_USB_EVENT_SETUP 5
typedef struct LemnosPackageUsbEvent {
    uint32_t kind;
    /* A setup's wLength. */
    uint32_t length;
    LemnosPackageControlSetup setup;
} LemnosPackageUsbEvent;

/* ---- One table for each kind of resource ---- */

/*
 * Every function takes the resource's handle first and returns a status. A
 * handle of another kind is -22 (invalid argument), one released or never
 * given LEMNOS_E_STALE_HANDLE, and a function the host does not serve
 * -95 (not supported). A resource's own error is one of: -2 not found, -19
 * unavailable, -16 busy, -13 permission denied, -110 timeout, -22 invalid
 * input, -95 unsupported, -71 configuration, -121 not acknowledged, -75
 * overrun, -5 failed.
 */

/* Another device, taken with LEMNOS_TAKE_DEVICE. */
typedef struct LemnosDeviceTable {
    LemnosStatus (*call)(LemnosHandle device, uint16_t method, LemnosSlice args,
                         LemnosSliceMut reply, size_t *len);
    LemnosStatus (*health)(LemnosHandle device, LemnosPackageHealth *health);
} LemnosDeviceTable;

typedef struct LemnosI2cTable {
    /* steps: LemnosPackageTransfer, at most LEMNOS_MAX_TRANSFER_STEPS. */
    LemnosStatus (*transaction)(LemnosHandle bus, uint16_t address, LemnosSliceMut steps);
} LemnosI2cTable;

typedef struct LemnosSpiTable {
    /* On a device taken with LEMNOS_TAKE_SPI. steps: LemnosPackageTransfer. */
    LemnosStatus (*transaction)(LemnosHandle device, LemnosSliceMut steps);
    /* On a device taken with LEMNOS_TAKE_RAW_SPI. segments: LemnosPackageSpiSegment. */
    LemnosStatus (*transfer)(LemnosHandle device, LemnosSliceMut segments);
} LemnosSpiTable;

typedef struct LemnosLineTable {
    LemnosStatus (*configure)(LemnosHandle line, LemnosPackageLineConfig config);
    LemnosStatus (*get)(LemnosHandle line, uint32_t *value);
    LemnosStatus (*set)(LemnosHandle line, uint32_t value);
    LemnosStatus (*read_edge)(LemnosHandle line, LemnosPackageEdge *edge);
} LemnosLineTable;

typedef struct LemnosPwmTable {
    LemnosStatus (*configure)(LemnosHandle pwm, LemnosPackagePwmConfig config);
    LemnosStatus (*config)(LemnosHandle pwm, LemnosPackagePwmConfig *config);
} LemnosPwmTable;

typedef struct LemnosUartTable {
    LemnosStatus (*set_baud)(LemnosHandle uart, uint32_t baud);
    LemnosStatus (*write)(LemnosHandle uart, LemnosSlice data, size_t *len);
    LemnosStatus (*read)(LemnosHandle uart, LemnosSliceMut out, uint64_t timeout_ns, size_t *len);
} LemnosUartTable;

typedef struct LemnosRegistersTable {
    LemnosStatus (*size)(LemnosHandle registers, size_t *size);
    LemnosStatus (*read32)(LemnosHandle registers, size_t offset, uint32_t *value);
    LemnosStatus (*write32)(LemnosHandle registers, size_t offset, uint32_t value);
    LemnosStatus (*modify32)(LemnosHandle registers, size_t offset, uint32_t clear, uint32_t set);
    /* values: uint32_t. */
    LemnosStatus (*write_block)(LemnosHandle registers, size_t offset, LemnosSlice values);
} LemnosRegistersTable;

typedef struct LemnosInterruptTable {
    LemnosStatus (*enable)(LemnosHandle interrupt);
    LemnosStatus (*disable)(LemnosHandle interrupt);
    LemnosStatus (*wait)(LemnosHandle interrupt, uint64_t timeout_ns,
                         LemnosPackageInterruptEvent *event);
} LemnosInterruptTable;

typedef struct LemnosDmaTable {
    LemnosStatus (*allocate)(LemnosHandle dma, uint64_t len, LemnosPackageDmaBuffer *buffer);
    LemnosStatus (*release)(LemnosHandle dma, uint32_t id);
    LemnosStatus (*write)(LemnosHandle dma, uint32_t id, uint64_t offset, LemnosSlice data);
    LemnosStatus (*read)(LemnosHandle dma, uint32_t id, uint64_t offset, LemnosSliceMut out);
    LemnosStatus (*sync_for_device)(LemnosHandle dma, uint32_t id);
    LemnosStatus (*sync_for_cpu)(LemnosHandle dma, uint32_t id);
} LemnosDmaTable;

typedef struct LemnosClocksTable {
    LemnosStatus (*enable)(LemnosHandle clocks, uint32_t clock);
    LemnosStatus (*disable)(LemnosHandle clocks, uint32_t clock);
    LemnosStatus (*rate_hz)(LemnosHandle clocks, uint32_t clock, uint64_t *hz);
    LemnosStatus (*set_rate_hz)(LemnosHandle clocks, uint32_t clock, uint64_t hz,
                                uint64_t *settled);
} LemnosClocksTable;

typedef struct LemnosResetTable {
    LemnosStatus (*assert_reset)(LemnosHandle reset, uint32_t line);
    LemnosStatus (*deassert_reset)(LemnosHandle reset, uint32_t line);
} LemnosResetTable;

/* The producer's end of a stream of buffers. */
typedef struct LemnosStreamTable {
    LemnosStatus (*buffer_count)(LemnosHandle stream, uint32_t *count);
    LemnosStatus (*publish)(LemnosHandle stream, LemnosPackageFrame frame);
    LemnosStatus (*next_buffer)(LemnosHandle stream, uint32_t *buffer);
    LemnosStatus (*next_job)(LemnosHandle stream, LemnosSliceMut out, size_t *len);
    /* attached: 0 when no consumer is. */
    LemnosStatus (*write)(LemnosHandle stream, uint32_t buffer, size_t offset, LemnosSlice data,
                          uint32_t *attached);
    LemnosStatus (*read)(LemnosHandle stream, uint32_t buffer, size_t offset, LemnosSliceMut out,
                         uint32_t *attached);
    /*
     * The buffer's memory as a descriptor, open in this process until the
     * stream's next call, or LEMNOS_FD_NONE. Duplicate it to hold the memory
     * longer; never close it.
     */
    LemnosStatus (*buffer_handle)(LemnosHandle stream, uint32_t buffer, LemnosFd *fd);
    /* current: 0 when the buffer was not taken under the consumer attached now. */
    LemnosStatus (*buffer_current)(LemnosHandle stream, uint32_t buffer, uint32_t *current);
    /* kind: LEMNOS_BUFFERS_PLAIN or LEMNOS_BUFFERS_HANDLES. It never changes for a stream. */
    LemnosStatus (*buffer_kind)(LemnosHandle stream, uint32_t *kind);
    /* Ends the attached consumer's stream as faulted. */
    LemnosStatus (*fault_consumer)(LemnosHandle stream);
} LemnosStreamTable;

typedef struct LemnosUsbDeviceTable {
    LemnosStatus (*config_descriptor)(LemnosHandle device, uint8_t index, LemnosSliceMut out,
                                      size_t *len);
    LemnosStatus (*reset)(LemnosHandle device);
    LemnosStatus (*detach_kernel_driver)(LemnosHandle device, uint8_t interface);
    LemnosStatus (*claim_interface)(LemnosHandle device, uint8_t interface);
    LemnosStatus (*release_interface)(LemnosHandle device, uint8_t interface);
    LemnosStatus (*set_alternate)(LemnosHandle device, uint8_t interface, uint8_t alternate);
    LemnosStatus (*control_in)(LemnosHandle device, LemnosPackageControlSetup setup,
                               LemnosSliceMut data, uint64_t timeout_ns, size_t *len);
    LemnosStatus (*control_out)(LemnosHandle device, LemnosPackageControlSetup setup,
                                LemnosSlice data, uint64_t timeout_ns, size_t *len);
    LemnosStatus (*bulk_in)(LemnosHandle device, uint8_t endpoint, LemnosSliceMut data,
                            uint64_t timeout_ns, size_t *len);
    LemnosStatus (*bulk_out)(LemnosHandle device, uint8_t endpoint, LemnosSlice data,
                             uint64_t timeout_ns, size_t *len);
    LemnosStatus (*interrupt_in)(LemnosHandle device, uint8_t endpoint, LemnosSliceMut data,
                                 uint64_t timeout_ns, size_t *len);
    /* A stream of `transfers` transfers of `packets` packets (at most LEMNOS_MAX_ISO_PACKETS)
     * of `packet_bytes` each, kept queued. */
    LemnosStatus (*iso_in_start)(LemnosHandle device, uint8_t endpoint, uint8_t transfers,
                                 uint16_t packets, uint32_t packet_bytes);
    /* The oldest done transfer; packets: LemnosPackageIsoPacket, packet i at i * packet_bytes. */
    LemnosStatus (*iso_in_next)(LemnosHandle device, LemnosSliceMut data, LemnosSliceMut packets,
                                uint64_t timeout_ns);
    LemnosStatus (*iso_in_stop)(LemnosHandle device);
} LemnosUsbDeviceTable;

typedef struct LemnosUsbFunctionTable {
    LemnosStatus (*enable)(LemnosHandle function, LemnosSlice descriptors);
    LemnosStatus (*disable)(LemnosHandle function);
    LemnosStatus (*next_event)(LemnosHandle function, LemnosPackageUsbEvent *event);
    LemnosStatus (*control_in)(LemnosHandle function, LemnosSlice data, uint64_t timeout_ns,
                               size_t *len);
    LemnosStatus (*control_out)(LemnosHandle function, LemnosSliceMut data, uint64_t timeout_ns,
                                size_t *len);
    LemnosStatus (*control_stall)(LemnosHandle function);
    LemnosStatus (*set_halt)(LemnosHandle function, uint8_t endpoint, uint32_t halted);
    LemnosStatus (*read)(LemnosHandle function, uint8_t endpoint, LemnosSliceMut data,
                         uint64_t timeout_ns, size_t *len);
    LemnosStatus (*write)(LemnosHandle function, uint8_t endpoint, LemnosSlice data,
                          uint64_t timeout_ns, size_t *len);
} LemnosUsbFunctionTable;

/* ---- What a package's driver calls on the host ---- */

/*
 * The table lives as long as the process. A handle from take is the driver's
 * until it calls release, or until the host lets go of the driver, which
 * releases whatever it still holds.
 */
typedef struct LemnosHostTable {
    uint32_t struct_size;
    uint32_t reserved;
    /* Whether the board file supplies `need`. Not 0 for yes. Only during bind. */
    uint32_t (*provided)(void *binding, LemnosStr need);
    /* kind: a LEMNOS_TAKE_* value; tag: a device's contract. Only during bind. */
    LemnosStatus (*take)(void *binding, uint32_t kind, LemnosStr need, LemnosStr tag,
                         LemnosHandle *resource);
    LemnosStatus (*release)(LemnosHandle resource);
    LemnosDeviceTable device;
    LemnosI2cTable i2c;
    LemnosSpiTable spi;
    LemnosLineTable line;
    LemnosPwmTable pwm;
    LemnosUartTable uart;
    LemnosRegistersTable registers;
    LemnosInterruptTable interrupt;
    LemnosDmaTable dma;
    LemnosClocksTable clocks;
    LemnosResetTable reset;
    LemnosStreamTable stream;
    LemnosUsbDeviceTable usb_device;
    LemnosUsbFunctionTable usb_function;
} LemnosHostTable;

/* ---- What the host calls on a package's driver ---- */

/* LemnosDriverTable.flags: the host should call tick. */
#define LEMNOS_DRIVER_TICKS 1
/* LemnosDriverTable.flags: the host should call caller and caller_gone. */
#define LEMNOS_DRIVER_TRACKS_CALLERS 2
/* tick's answer when the driver wants no tick. */
#define LEMNOS_NO_TICK UINT64_MAX

/* `driver` is what bind wrote, and belongs to the package. */
typedef struct LemnosDriverTable {
    uint32_t struct_size;
    uint32_t flags;
    /*
     * Builds the driver and writes it to `driver`. `binding` is the host's,
     * valid for this call only: pass it to host->provided and host->take.
     * settings: LemnosPackageSetting. `link` counts only when has_link is not 0.
     */
    LemnosStatus (*bind)(const LemnosHostTable *host, void *binding, LemnosSlice settings,
                         LemnosStr link, uint32_t has_link, void **driver);
    /*
     * Fills `items` (LemnosPackageItem) and writes how many the device has in
     * all to `count`: more than `items` holds means some were left out.
     */
    LemnosStatus (*describe)(void *driver, LemnosSliceMut items, uint32_t *count);
    LemnosStatus (*health)(void *driver, LemnosPackageHealth *health);
    /* Runs a method. The reply's length goes to `len` whatever the status. */
    LemnosStatus (*dispatch)(void *driver, uint8_t contract, uint16_t method, LemnosSlice args,
                             LemnosSliceMut reply, size_t *len);
    /* Does what is due at now_ns and writes when to be called next, or LEMNOS_NO_TICK. */
    LemnosStatus (*tick)(void *driver, uint64_t now_ns, uint64_t *next_ns);
    LemnosStatus (*caller)(void *driver, uint64_t caller);
    LemnosStatus (*caller_gone)(void *driver, uint64_t caller);
    /* Lets go of the driver and everything it holds. */
    LemnosStatus (*drop)(void *driver);
} LemnosDriverTable;

/* What register_driver hands the host: one driver. */
typedef struct LemnosPackageDriver {
    LemnosPackageManifest manifest;
    LemnosDriverTable table;
} LemnosPackageDriver;

/* What a package's lemnos_package symbol is. */
typedef struct LemnosPackageDescriptor {
    /* LEMNOS_PACKAGE_MAGIC. */
    uint64_t magic;
    /* LEMNOS_PACKAGE_VERSION_INIT. */
    LemnosAbiVersion version;
    /*
     * Called once. Writes the package's driver: `driver` points to a
     * LemnosSlice whose ptr is one LemnosPackageDriver and whose len is 1.
     */
    LemnosStatus (*register_driver)(LemnosSlice *driver);
} LemnosPackageDescriptor;

#ifdef __cplusplus
}
#endif

#endif /* LEMNOS_PACKAGE_H */
