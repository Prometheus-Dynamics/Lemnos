/* SPDX-License-Identifier: GPL-2.0 WITH Linux-syscall-note */
/*
 * The Lemnos control node, /dev/lemnos: one file, one opener at a time.
 *
 * A block is a platform device bound to the module through driver_override.
 * Blocks have numbers that are not reused while the module is loaded, so a
 * number held across an unbind fails with ENODEV and never reaches another
 * block. LEMNOS_IOC_NEXT walks them. Every request for a block starts with
 * its number.
 *
 * - LEMNOS_IOC_INFO: the ABI, the register regions and interrupts, the DMA
 *   address width, and whether the registers may be mapped, and if so whether
 *   only because the module was told to (unconfined=).
 * - LEMNOS_IOC_REGION: one register region's physical range and the offset to
 *   give mmap(), which maps it when LEMNOS_INFO_MMIO_MAPPABLE is set and the
 *   caller has CAP_SYS_RAWIO. Mappings must be shared. They fault once the
 *   block is unbound.
 * - LEMNOS_IOC_ALLOC: a DMA buffer for the block, exported as a dma-buf (mmap
 *   it for the CPU), with its device address. Unbinding the block waits until
 *   every such buffer is closed.
 * - LEMNOS_IOC_IMPORT / LEMNOS_IOC_RELEASE: a dma-buf mapped on the block,
 *   contiguous in its address space, held until released, the block is
 *   unbound, or the file closes.
 * - Interrupts arrive on the event stream, one per interrupt that fired since
 *   its last enable. An interrupt is masked from when it fires until
 *   LEMNOS_IOC_IRQ_ENABLE: the driver clears the cause, then enables it. All
 *   start masked, and closing the file masks them again.
 * - LEMNOS_IOC_POWER: a block's clocks, supplies and resets, by the names its
 *   firmware description gives them.
 *
 * The event stream is read() and poll() on this file: struct lemnos_event,
 * for interrupts, blocks bound and unbound, supply faults and temperature
 * limits crossed. It starts empty at open.
 *
 * Claims are separate from blocks. LEMNOS_IOC_CLAIM takes a resource by name,
 * exclusively, and returns a file: whoever holds that file holds the claim,
 * and it ends when the last copy closes, however its holder ended. The name
 * means nothing to the module. A name already held is refused with EBUSY.
 *
 * Frame streams. LEMNOS_IOC_STREAM_CREATE sets one up on a block and returns a
 * file: the stream lasts as long as that file, or until the block is unbound.
 * mmap() it at offset 0, shared, for the ring (see below); its size is
 * ring_bytes. poll() is readable while frames wait and on the end of the
 * stream, and the eventfd given at creation is signalled for each frame.
 *
 * The ring is the shared-memory protocol of the socket interface's streams,
 * word for word (LEMNOS_RING_*): the module is the producer and the file's
 * mapper the consumer. Everything in it is untrusted, so each word is read
 * once and checked against what the module holds. Buffers are at multiples of
 * the stride in the pool, the dma-buf named at creation, and consumers map
 * that dma-buf to read them; or, for a stream made with
 * LEMNOS_STREAM_CREATE_BUFFERS, each buffer is a dma-buf of its own (see
 * "Buffers of their own" below). The module publishes frames, takes returned
 * buffers (their fence must match), and takes jobs: a job says which buffer
 * the next frame fills and which device-address ranges the block may use for
 * it, as struct lemnos_stream_job. Any range outside the job's buffer or the
 * registered dma-bufs is refused, and any consumer breaking the rules ends the
 * stream with state 2 and an errno. Nothing else is touched.
 *
 * A consumer that never posts a job gets a frame on each free buffer. Once one
 * job has been accepted the stream is job-driven for the rest of its life: no
 * frame is made without a queued job, so a buffer returned and then named by
 * the next job can't be taken in between. Until then the source may take any
 * free buffer, so a job on a particular buffer can lose it and be refused: a
 * consumer that uses jobs posts (and KICKs) its first before frames start.
 *
 * - LEMNOS_STREAM_IOC_KICK: process what the consumer has posted now. Jobs are
 *   also collected as the block needs them, so this is only needed to learn a
 *   refusal synchronously. Its return is the errno (see "refusals" below).
 * - LEMNOS_STREAM_IOC_REGISTER: pin a dma-buf the block may be sent to.
 * - LEMNOS_STREAM_IOC_UNREGISTER: release one; refused (EBUSY) while a job
 *   uses it.
 * - LEMNOS_STREAM_IOC_STATUS: how the stream is doing.
 * - LEMNOS_STREAM_IOC_BUFFER: gives one buffer its dma-buf, on a stream made
 *   with LEMNOS_STREAM_CREATE_BUFFERS.
 *
 * Buffers of their own. A stream made with LEMNOS_STREAM_CREATE_BUFFERS has no
 * pool (pool_fd is -1, at most LEMNOS_STREAM_MAX_OWN_BUFFERS buffers): it is
 * for a consumer whose frame memory already exists as one dma-buf per buffer,
 * so the block writes where that memory's reader will look. Every buffer
 * starts with no memory, and one without memory is never filled: no frame
 * takes it, and a job naming it is refused (EFAULT). LEMNOS_STREAM_IOC_BUFFER
 * gives buffer n a dma-buf, pinned and mapped for the block exactly as a pool
 * is, and from then on buffer n's device address is that dma-buf's start; only
 * its first buffer_bytes are reachable, however big it is. Called again for a
 * buffer that has memory, it swaps the memory: the old dma-buf is unpinned
 * and the new one takes its place, with the buffer's state and fence as they
 * were. That is allowed whenever the block can't be writing the buffer: while
 * it is free or with the consumer. Naming the dma-buf the buffer already has
 * changes nothing and succeeds, which is how a consumer asks whether buffer n
 * is still the memory it thinks it is. Everything else about the stream is the
 * same: the ring, jobs, registered dma-bufs, and the end of the stream, which
 * unpins every buffer (or none, if the block's DMA could not be seen to stop).
 *
 * LEMNOS_STREAM_IOC_BUFFER's refusals, none of which ends the stream:
 *   EOPNOTSUPP the stream has a pool
 *   EINVAL     buffer number too large; not a dma-buf; not contiguous for the
 *              block; smaller than buffer_bytes; not aligned for the block
 *   EBADF      not an open file
 *   EBUSY      a job names the buffer or the block is filling it; or the
 *              dma-buf is another buffer's or registered, or overlaps one
 *   EIO        the block's DMA could not be seen to stop: nothing changes hands
 *   ENODEV     the stream is over
 *
 * A job's refusals (the errno of KICK and of STATUS.fault):
 *   EINVAL     buffer number too large, zero length, unknown count, non-zero spare
 *   EMSGSIZE   a job length other than sizeof(struct lemnos_stream_job)
 *   ERANGE     a ring head or tail that can't be (more than the capacity away)
 *   EOVERFLOW  addr + len wraps
 *   EFAULT     a range outside every registered region; a buffer with no memory yet
 *   EACCES     a range in another buffer of the pool, or a buffer the consumer holds
 *   EBUSY      the buffer is already posted (double posting)
 *   EPERM      a returned buffer that isn't out, or with the wrong fence
 * A malformed consumer faults only its own stream.
 *
 * Buses go through the kernel's own I2C and SPI cores. Every transfer names a
 * claim, and the claim's resource must be the target's: "i2c-<bus>:0x<addr>"
 * in lower-case hex, or "spi-<bus>.<cs>". An I2C address a kernel driver holds
 * is refused with EBUSY. A SPI device is bound to the module the same way a
 * block is, through driver_override and owned=. A transfer is all or nothing
 * as the kernel's core defines it, and carries at most the module's
 * bus_buffer bytes each way (EMSGSIZE beyond that).
 *
 * GPIO lines and PWM channels go through gpiolib and the PWM core, and belong
 * to a claim named "gpio-<chip>:<offset>" (gpiochip number) or
 * "pwm-<provider>:<channel>" (the PWM chip's parent device name). A request
 * binds the line or channel to the claim's file; later calls just name the
 * claim, and read() and poll() on that file give a line's edges. Closing the
 * claim lets go of the line or channel and leaves its output as it was.
 *
 * PIO, on a kernel that has the RP1 PIO driver, belongs to a claim on "pio-0".
 * LEMNOS_IOC_PIO_OPEN binds a client to the claim and LEMNOS_IOC_PIO_CALL runs
 * one allowed operation; closing the claim ends the client, which frees its
 * state machines and programs. Raw register access isn't offered. DMA data
 * goes through kernel buffers the module owns, so no caller names an address.
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#ifndef _UAPI_LEMNOS_H
#define _UAPI_LEMNOS_H

#include <linux/ioctl.h>
#include <linux/types.h>

#define LEMNOS_ABI 1
#define LEMNOS_MAX_REGIONS 8
#define LEMNOS_MAX_IRQS 4
#define LEMNOS_NAME_MAX 64
#define LEMNOS_POWER_NAME 32
#define LEMNOS_MAX_POWER 12
#define LEMNOS_MAX_THERMAL_WATCHES 8

/* The registers may be mapped (see above). */
#define LEMNOS_INFO_MMIO_MAPPABLE	(1u << 0)
/*
 * They may be mapped only because the block is listed in unconfined=: no
 * IOMMU confines its DMA, so the driver in user space alone keeps it in bounds.
 */
#define LEMNOS_INFO_MMIO_UNCONFINED	(1u << 1)

struct lemnos_next {
	__u32 after;		/* in: a block number, or 0 for the first */
	__u32 block;		/* out: the next block, ENOENT past the last */
	char name[LEMNOS_NAME_MAX]; /* out: its device name */
};

struct lemnos_info {
	__u32 block;		/* in */
	__u32 abi;		/* LEMNOS_ABI */
	__u32 flags;		/* LEMNOS_INFO_* */
	__u32 regions;		/* register regions, up to LEMNOS_MAX_REGIONS */
	__u32 irqs;		/* interrupts forwarded, up to LEMNOS_MAX_IRQS */
	__u32 dma_bits;		/* device addresses are below 2^dma_bits */
	__u32 reserved[2];
};

struct lemnos_region {
	__u32 block;		/* in */
	__u32 index;		/* in */
	__u64 start;		/* physical address */
	__u64 size;		/* bytes */
	__u64 offset;		/* what to give mmap() */
};

struct lemnos_alloc {
	__u32 block;		/* in */
	__u32 flags;		/* in: zero */
	__u64 size;		/* in: bytes, rounded up to pages */
	__u64 addr;		/* out: the device address */
	__s32 fd;		/* out: the dma-buf, close-on-exec */
	__u32 reserved;
};

struct lemnos_import {
	__u32 block;		/* in */
	__s32 fd;		/* in: a dma-buf */
	__u64 addr;		/* out: the device address */
	__u64 size;		/* out: bytes mapped */
	__u32 handle;		/* out: for LEMNOS_IOC_RELEASE, nonzero */
	__u32 reserved;
};

struct lemnos_release {
	__u32 block;		/* in */
	__u32 handle;		/* in */
};

struct lemnos_irq {
	__u32 block;		/* in */
	__u32 index;		/* in: below lemnos_info.irqs */
};

#define LEMNOS_EVENT_IRQ		0	/* index: the interrupt; count: times it fired */
#define LEMNOS_EVENT_BLOCK_BOUND	1
#define LEMNOS_EVENT_BLOCK_UNBOUND	2
#define LEMNOS_EVENT_SUPPLY_FAULT	3	/* index: the supply's handle; value: REGULATOR_EVENT_* */
#define LEMNOS_EVENT_TEMPERATURE	4	/* index: the watch; value: millidegrees */

/*
 * count on the non-interrupt kinds numbers them, so a gap means some were
 * lost to a full queue.
 */
struct lemnos_event {
	__u32 block;
	__u32 index;
	__u32 count;
	__u32 kind;		/* LEMNOS_EVENT_* */
	__u64 ns;		/* ktime_get_ns() when it happened */
	__s64 value;
};

struct lemnos_claim {
	char resource[LEMNOS_NAME_MAX];	/* in: NUL-terminated, not empty */
	__u32 flags;		/* in: zero */
	__s32 fd;		/* out: the claim, close-on-exec */
};

/* The stream's limits, the same as the socket protocol's. */
#define LEMNOS_STREAM_MAX_BUFFERS	4096u
#define LEMNOS_STREAM_MAX_BUFFER_BYTES	(1u << 28)
#define LEMNOS_STREAM_MAX_POOL_BYTES	(1ull << 34)
#define LEMNOS_STREAM_MAX_JOB_SLOTS	64u
#define LEMNOS_STREAM_MAX_JOB_BYTES	4096u
#define LEMNOS_STREAM_BUFFER_ALIGN	4096u
#define LEMNOS_STREAM_FRAME_WORDS	5
/* Buffers a stream made with LEMNOS_STREAM_CREATE_BUFFERS may have. */
#define LEMNOS_STREAM_MAX_OWN_BUFFERS	64u
#define LEMNOS_STREAM_JOB_RANGES	4

/* Words from the start of the ring. Host-endian on every target, which is little. */
#define LEMNOS_RING_MAGIC		0
#define LEMNOS_RING_VERSION		1
#define LEMNOS_RING_BUFFERS		2
#define LEMNOS_RING_BUFFER_BYTES	3
#define LEMNOS_RING_FRAME_SLOTS		4
#define LEMNOS_RING_JOB_SLOTS		5
#define LEMNOS_RING_JOB_BYTES		6
#define LEMNOS_RING_STATE		8
#define LEMNOS_RING_FRAMES_HEAD		16
#define LEMNOS_RING_FRAMES_TAIL		24
#define LEMNOS_RING_RETURNS_HEAD	32
#define LEMNOS_RING_RETURNS_TAIL	40
#define LEMNOS_RING_JOBS_HEAD		48
#define LEMNOS_RING_JOBS_TAIL		56
#define LEMNOS_RING_FRAMES		64

#define LEMNOS_RING_MAGIC_VALUE		0x474e4952534e4d4cull	/* "LMNSRING" */
#define LEMNOS_RING_LAYOUT_VERSION	1ull
#define LEMNOS_RING_STATE_OPEN		0ull
#define LEMNOS_RING_STATE_CLOSED	1ull
#define LEMNOS_RING_STATE_FAULT		2ull

/*
 * lemnos_stream_create.flags: no pool. Each buffer is a dma-buf of its own,
 * given with LEMNOS_STREAM_IOC_BUFFER; pool_fd must be -1 and pool_addr reads 0.
 */
#define LEMNOS_STREAM_CREATE_BUFFERS	(1u << 0)
/*
 * The frame ring is the shared-memory ring of shm/README.md (layout 2), not the
 * LEMNOS_RING_* layout: one field, one sample per frame, each sample's entry
 * naming its buffer and ticket (shm/ring-buffers/src/entry.rs). The pool page
 * (shm/ring-buffers/src/page.rs) follows the ring in the mapping, then the
 * returns and jobs words. Returns are not read in this form: a buffer goes back
 * when no reader holds it, from the reader's marks page (LEMNOS_STREAM_IOC_READER).
 * Jobs are still the old words for now. Needs frame_slots of at least 2.
 */
#define LEMNOS_STREAM_CREATE_SHM	(1u << 1)

struct lemnos_stream_create {
	__u32 block;		/* in */
	__u32 flags;		/* in: LEMNOS_STREAM_CREATE_* */
	__u32 buffers;		/* in: up to LEMNOS_STREAM_MAX_BUFFERS */
	__u32 buffer_bytes;	/* in: up to LEMNOS_STREAM_MAX_BUFFER_BYTES */
	__u32 frame_slots;	/* in: a power of two, at least buffers */
	__u32 job_slots;	/* in: a power of two, up to LEMNOS_STREAM_MAX_JOB_SLOTS */
	__u32 job_bytes;	/* in: at least sizeof(struct lemnos_stream_job) */
	__s32 pool_fd;		/* in: a dma-buf holding buffers * stride bytes; -1 without a pool */
	__s32 doorbell_fd;	/* in: an eventfd, or -1 */
	__s32 fd;		/* out: the stream, close-on-exec */
	__u64 ring_bytes;	/* out: what to mmap() */
	__u64 pool_addr;	/* out: buffer 0's device address */
};

/* One range the block may use for a job's frame, as a device address. */
struct lemnos_stream_range {
	__u64 addr;
	__u64 len;
};

/* A job, in a job slot. Exactly this many bytes, or it's refused. */
struct lemnos_stream_job {
	__u64 identity;			/* carried to the frame */
	__u32 buffer;			/* the pool buffer the frame fills */
	__u32 count;			/* ranges used; the rest must be zero */
	struct lemnos_stream_range range[LEMNOS_STREAM_JOB_RANGES];
};

struct lemnos_stream_register {
	__s32 fd;		/* in: a dma-buf, kept pinned until unregistered */
	__u32 region;		/* out: for LEMNOS_STREAM_IOC_UNREGISTER */
	__u64 addr;		/* out: its device address */
	__u64 size;		/* out: bytes */
};

struct lemnos_stream_unregister {
	__u32 region;		/* in */
	__u32 reserved;
};

/* One buffer's memory, on a stream made with LEMNOS_STREAM_CREATE_BUFFERS. */
struct lemnos_stream_buffer {
	__u32 buffer;		/* in: below the stream's buffers */
	__s32 fd;		/* in: a dma-buf of at least buffer_bytes, kept pinned */
	__u64 addr;		/* out: the buffer's device address */
	__u64 size;		/* out: bytes of it the block may reach: buffer_bytes */
};

/*
 * LEMNOS_STREAM_IOC_READER, on a stream made with LEMNOS_STREAM_CREATE_SHM: gives
 * the module a reader's marks page (shm/ring-holds/src/marks.rs). The module
 * pins the page and reads it read-only; it never writes it. A buffer is held
 * while any reader marks it, and the module reuses it only when none does. A reader
 * over max_held, or holding a buffer longer than max_hold_ns, is cut off: its marks stop
 * holding, and its word after the tickets on the pool page is set (page::is_cut_off).
 * The page must be a sealed memfd (F_SEAL_SHRINK and F_SEAL_GROW) of at least
 * 8 * (1 + 2 * buffers) bytes. The reader table has LEMNOS_STREAM_MAX_READERS slots. A
 * registration lasts while its reader_fd is open: closing that file (an exit does too) or
 * LEMNOS_READER_IOC_UNREGISTER frees the slot, its marks hold nothing from then on, and the page is
 * unpinned. The stream's end frees whatever is left.
 */
#define LEMNOS_STREAM_MAX_READERS	64u

struct lemnos_stream_reader {
	__s32 fd;		/* in: the marks page, a memfd */
	__u32 slot;		/* out: the reader slot the module gave it, 0 to 63 */
	__u32 max_held;		/* in: buffers it may hold at once; 0 for no limit */
	__s32 reader_fd;	/* out: a file that holds the registration; see LEMNOS_READER_IOC_UNREGISTER */
	__u64 max_hold_ns;	/* in: how long one hold may last, ns; 0 for no limit */
};

struct lemnos_stream_status {
	__u32 state;		/* LEMNOS_RING_STATE_*, held in the kernel */
	__s32 fault;		/* the errno that ended it, or 0 */
	__u64 frames;		/* published */
	__u64 dropped;		/* no room on the ring */
	__u64 returned;		/* buffers given back */
	__u64 jobs;		/* jobs accepted */
};

/* Test only, with the module's test_stream_source. */
struct lemnos_stream_test_source {
	__u32 period_us;	/* between frames */
	__u32 frames;		/* to make: 0 stops, ~0u never stops */
};

/* On /dev/lemnos. */
#define LEMNOS_IOC_STREAM_CREATE       _IOWR('L', 0x80, struct lemnos_stream_create)
/* On the stream's file. */
#define LEMNOS_STREAM_IOC_KICK         _IO('L', 0x81)
#define LEMNOS_STREAM_IOC_REGISTER     _IOWR('L', 0x82, struct lemnos_stream_register)
#define LEMNOS_STREAM_IOC_UNREGISTER   _IOW('L', 0x83, struct lemnos_stream_unregister)
#define LEMNOS_STREAM_IOC_STATUS       _IOR('L', 0x84, struct lemnos_stream_status)
#define LEMNOS_STREAM_IOC_TEST_SOURCE  _IOW('L', 0x85, struct lemnos_stream_test_source)
#define LEMNOS_STREAM_IOC_BUFFER       _IOWR('L', 0x86, struct lemnos_stream_buffer)
#define LEMNOS_STREAM_IOC_READER       _IOWR('L', 0x87, struct lemnos_stream_reader)

/*
 * LEMNOS_READER_IOC_UNREGISTER, on the reader_fd that LEMNOS_STREAM_IOC_READER returned: frees the
 * reader's slot now (its marks stop holding; the next frame that needs a buffer can take it). The
 * registration is also freed when that file is closed, by the caller or by its exit. Returns
 * -ENOENT once the registration is gone.
 */
#define LEMNOS_READER_IOC_UNREGISTER   _IO('L', 0x88)

/*
 * LEMNOS_STREAM_IOC_BELL, on a stream made with LEMNOS_STREAM_CREATE_SHM: the consumer's doorbell
 * (shm/ring-doorbell's Doorbell::bell_fds: an eventfd it sleeps on, and its page, read-only). The
 * page's word 0 is sleeps << 1 | asleep, and only the consumer writes it. After each publish the
 * module reads it, and when the consumer is asleep, signals the eventfd once for that sleep. Once
 * per stream: -EBUSY after. Both descriptors are the caller's to keep; the module takes its own.
 * Additive: a stream without a bell is unchanged.
 */
struct lemnos_stream_bell {
	__s32 eventfd;		/* in: an eventfd the consumer waits on */
	__s32 page;		/* in: the doorbell page, read-only */
};

#define LEMNOS_STREAM_IOC_BELL         _IOW('L', 0x89, struct lemnos_stream_bell)

#define LEMNOS_BUS_MAX_SEGMENTS 16

/* lemnos_i2c_segment.flags */
#define LEMNOS_I2C_READ		(1u << 0)
/* lemnos_i2c_transfer.flags */
#define LEMNOS_I2C_TEN_BIT	(1u << 0)

struct lemnos_i2c_segment {
	__u64 buf;		/* user pointer */
	__u32 len;		/* bytes, at most 65535 */
	__u32 flags;		/* LEMNOS_I2C_* */
};

/* Repeated starts between segments, one stop at the end, to one target. */
struct lemnos_i2c_transfer {
	__s32 claim;		/* a claim on this target */
	__u32 bus;
	__u16 addr;
	__u16 flags;		/* LEMNOS_I2C_TEN_BIT */
	__u32 count;		/* up to LEMNOS_BUS_MAX_SEGMENTS */
	__u64 segments;		/* user pointer to lemnos_i2c_segment[count] */
};

/* One SMBus command, for adapters without plain transfers. */
struct lemnos_i2c_smbus {
	__s32 claim;		/* a claim on this target */
	__u32 bus;
	__u16 addr;
	__u16 flags;		/* LEMNOS_I2C_TEN_BIT */
	__u8 read;		/* 1 to read, 0 to write */
	__u8 command;
	__u16 protocol;		/* I2C_SMBUS_QUICK and friends, from <linux/i2c.h> */
	__u8 data[34];		/* union i2c_smbus_data */
	__u8 reserved[6];
};

struct lemnos_spi_segment {
	__u64 tx;		/* user pointer, or 0 */
	__u64 rx;		/* user pointer, or 0 */
	__u32 len;		/* bytes each way */
	__u32 speed_hz;		/* 0: the device's */
	__u16 delay_us;		/* after this segment */
	__u8 bits;		/* 0: the device's */
	__u8 cs_change;
	__u32 reserved;
};

/* Mode bits are SPI_CPHA, SPI_CPOL, SPI_CS_HIGH and SPI_LSB_FIRST. */
struct lemnos_spi_transfer {
	__s32 claim;		/* a claim on this device */
	__u32 bus;
	__u32 cs;
	__u32 mode;
	__u32 count;		/* up to LEMNOS_BUS_MAX_SEGMENTS */
	__u32 reserved;
	__u64 segments;		/* user pointer to lemnos_spi_segment[count] */
};

/* Whether a SPI device is bound to the module (ENODEV if not), and its mode bits. */
struct lemnos_spi_info {
	__u32 bus;
	__u32 cs;
	__u32 mode;		/* out */
	__u32 reserved;
};

#define LEMNOS_POWER_CLOCK	0
#define LEMNOS_POWER_SUPPLY	1
#define LEMNOS_POWER_RESET	2

#define LEMNOS_POWER_ENABLE	0	/* clock, supply; deasserts a reset */
#define LEMNOS_POWER_DISABLE	1	/* clock, supply; asserts a reset */
#define LEMNOS_POWER_STATUS	2	/* value: 1 if on, or asserted for a reset */
#define LEMNOS_POWER_GET	3	/* value: hertz, or microvolts */
#define LEMNOS_POWER_SET	4	/* value: hertz, or minimum microvolts; value2: maximum */
#define LEMNOS_POWER_PULSE	5	/* reset only */

struct lemnos_power {
	__u32 block;		/* in */
	__u32 kind;		/* in: LEMNOS_POWER_CLOCK, SUPPLY or RESET */
	__u32 op;		/* in */
	__u32 handle;		/* out: which handle this is, for events */
	char name[LEMNOS_POWER_NAME];	/* in: empty for a block's only clock */
	__u64 value;		/* in, out */
	__u64 value2;		/* in */
};

struct lemnos_thermal {
	char zone[LEMNOS_NAME_MAX];	/* in: the thermal zone's type */
	__s64 temperature;	/* out: millidegrees, now */
};

/*
 * LEMNOS_EVENT_TEMPERATURE when the zone rises to high_mc, and again once it
 * has fallen to low_mc. Ends when the file closes.
 */
struct lemnos_thermal_watch {
	char zone[LEMNOS_NAME_MAX];	/* in */
	__s32 low_mc;		/* in */
	__s32 high_mc;		/* in */
	__u32 period_ms;	/* in */
	__u32 watch;		/* out: the event's index */
};

/* LEMNOS_PIO_* : each one's args are the same-named rp1_pio_*_args from rp1_pio_if.h. */
enum lemnos_pio_op {
	LEMNOS_PIO_ADD_PROGRAM,
	LEMNOS_PIO_REMOVE_PROGRAM,
	LEMNOS_PIO_SM_CLAIM,
	LEMNOS_PIO_SM_UNCLAIM,
	LEMNOS_PIO_SM_INIT,
	LEMNOS_PIO_SM_SET_CONFIG,
	LEMNOS_PIO_SM_EXEC,
	LEMNOS_PIO_SM_CLEAR_FIFOS,
	LEMNOS_PIO_SM_SET_CLKDIV,
	LEMNOS_PIO_SM_SET_PINS,
	LEMNOS_PIO_SM_SET_PINDIRS,
	LEMNOS_PIO_SM_SET_ENABLED,
	LEMNOS_PIO_SM_RESTART,
	LEMNOS_PIO_SM_PUT,
	LEMNOS_PIO_SM_GET,
	LEMNOS_PIO_SM_FIFO_STATE,
	LEMNOS_PIO_SM_DRAIN_TX,
	LEMNOS_PIO_SM_GET_FLAGS,
	LEMNOS_PIO_GPIO_INIT,
	LEMNOS_PIO_GPIO_SET_FUNCTION,
	LEMNOS_PIO_GPIO_SET_PULLS,
	LEMNOS_PIO_GPIO_SET_OUTOVER,
	LEMNOS_PIO_GPIO_SET_INPUT_ENABLED,
	LEMNOS_PIO_OP_COUNT,
};

#define LEMNOS_PIO_ARGS 72
#define LEMNOS_PIO_TO_SM 0
#define LEMNOS_PIO_FROM_SM 1

struct lemnos_pio_open {
	__s32 claim;		/* a claim on "pio-0" */
};

struct lemnos_pio_call {
	__s32 claim;
	__u32 op;		/* enum lemnos_pio_op */
	__s64 result;		/* out: the operation's return value */
	__u8 args[LEMNOS_PIO_ARGS];	/* in, out */
};

/* Sets a machine's DMA up. Bytes per buffer, at most 256 KiB, and how many buffers. */
struct lemnos_pio_config_xfer {
	__s32 claim;
	__u32 sm;
	__u32 dir;		/* LEMNOS_PIO_TO_SM or LEMNOS_PIO_FROM_SM */
	__u32 buf_size;
	__u32 buf_count;
	__u32 reserved;
};

/* Moves `len` bytes, split into buf_size pieces. Returns when all have moved. */
struct lemnos_pio_xfer {
	__s32 claim;
	__u32 sm;
	__u32 dir;
	__u32 len;
	__u64 data;		/* user pointer */
	__u32 timeout_ms;	/* 0: one second */
	__u32 reserved;
};

#define LEMNOS_IOC_PIO_OPEN   _IOW('L', 0x90, struct lemnos_pio_open)
#define LEMNOS_IOC_PIO_CALL   _IOWR('L', 0x91, struct lemnos_pio_call)
#define LEMNOS_IOC_PIO_CONFIG_XFER _IOW('L', 0x92, struct lemnos_pio_config_xfer)
#define LEMNOS_IOC_PIO_XFER   _IOW('L', 0x93, struct lemnos_pio_xfer)

#define LEMNOS_IOC_THERMAL_READ _IOWR('L', 0xa0, struct lemnos_thermal)
#define LEMNOS_IOC_THERMAL_WATCH _IOWR('L', 0xa1, struct lemnos_thermal_watch)
#define LEMNOS_IOC_POWER      _IOWR('L', 0xd6, struct lemnos_power)

#define LEMNOS_GPIO_OUTPUT	(1u << 0)
#define LEMNOS_GPIO_ACTIVE_LOW	(1u << 1)
#define LEMNOS_GPIO_PULL_UP	(1u << 2)
#define LEMNOS_GPIO_PULL_DOWN	(1u << 3)
#define LEMNOS_GPIO_BIAS_OFF	(1u << 4)
#define LEMNOS_GPIO_OPEN_DRAIN	(1u << 5)
#define LEMNOS_GPIO_OPEN_SOURCE	(1u << 6)
#define LEMNOS_GPIO_RISING	(1u << 7)
#define LEMNOS_GPIO_FALLING	(1u << 8)

struct lemnos_gpio_request {
	__s32 claim;		/* a claim on this line */
	__u32 chip;		/* N in gpiochipN */
	__u32 offset;
	__u32 reserved;
};

struct lemnos_gpio_config {
	__s32 claim;
	__u32 flags;		/* LEMNOS_GPIO_* */
	__u32 value;		/* for an output, 0 or 1 */
	__u32 debounce_us;
};

struct lemnos_gpio_value {
	__s32 claim;
	__u32 value;		/* 0 or 1, after LEMNOS_GPIO_ACTIVE_LOW */
};

/* What read() on a line's claim returns. seq counts every edge, so a gap is a loss. */
struct lemnos_gpio_event {
	__u64 ns;		/* ktime_get_ns() at the edge */
	__u32 seq;
	__u32 rising;		/* 1 for rising, 0 for falling, in line level */
};

struct lemnos_pwm_request {
	__s32 claim;		/* a claim on this channel */
	__u32 channel;
	char provider[LEMNOS_NAME_MAX - 16];	/* the PWM chip's parent device name */
};

struct lemnos_pwm_state {
	__s32 claim;
	__u32 enabled;
	__u32 inverted;
	__u32 reserved;
	__u64 period_ns;
	__u64 duty_ns;
};

#define LEMNOS_IOC_GPIO_REQUEST _IOW('L', 0x98, struct lemnos_gpio_request)
#define LEMNOS_IOC_GPIO_CONFIG  _IOW('L', 0x99, struct lemnos_gpio_config)
#define LEMNOS_IOC_GPIO_GET     _IOWR('L', 0x9a, struct lemnos_gpio_value)
#define LEMNOS_IOC_GPIO_SET     _IOW('L', 0x9b, struct lemnos_gpio_value)
#define LEMNOS_IOC_PWM_REQUEST  _IOW('L', 0xb8, struct lemnos_pwm_request)
#define LEMNOS_IOC_PWM_APPLY    _IOW('L', 0xb9, struct lemnos_pwm_state)
#define LEMNOS_IOC_PWM_GET      _IOWR('L', 0xba, struct lemnos_pwm_state)

#define LEMNOS_IOC_I2C_TRANSFER _IOW('L', 0xa8, struct lemnos_i2c_transfer)
#define LEMNOS_IOC_I2C_SMBUS  _IOWR('L', 0xa9, struct lemnos_i2c_smbus)
#define LEMNOS_IOC_SPI_TRANSFER _IOW('L', 0xaa, struct lemnos_spi_transfer)
#define LEMNOS_IOC_SPI_INFO   _IOWR('L', 0xab, struct lemnos_spi_info)
#define LEMNOS_IOC_CLAIM      _IOWR('L', 0xce, struct lemnos_claim)
#define LEMNOS_IOC_NEXT       _IOWR('L', 0xcf, struct lemnos_next)
#define LEMNOS_IOC_INFO       _IOWR('L', 0xd0, struct lemnos_info)
#define LEMNOS_IOC_REGION     _IOWR('L', 0xd1, struct lemnos_region)
#define LEMNOS_IOC_ALLOC      _IOWR('L', 0xd2, struct lemnos_alloc)
#define LEMNOS_IOC_IMPORT     _IOWR('L', 0xd3, struct lemnos_import)
#define LEMNOS_IOC_RELEASE    _IOW('L', 0xd4, struct lemnos_release)
#define LEMNOS_IOC_IRQ_ENABLE _IOW('L', 0xd5, struct lemnos_irq)
/* Test only, with the module's test_irq_inject: sets the interrupt pending at its controller. */
#define LEMNOS_IOC_TEST_IRQ   _IOW('L', 0xdf, struct lemnos_irq)

#endif
