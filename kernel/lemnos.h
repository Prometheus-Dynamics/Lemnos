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
 *   address width, and whether the registers may be mapped.
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
 * - Interrupts: read() returns struct lemnos_irq_event, oldest first, one per
 *   interrupt that fired since its last enable, and poll() waits for one. An
 *   interrupt is masked from when it fires until LEMNOS_IOC_IRQ_ENABLE: the
 *   driver clears the cause, then enables it. All start masked, and closing
 *   the file masks them again.
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
 * that dma-buf to read them. The module publishes frames, takes returned
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
 *
 * A job's refusals (the errno of KICK and of STATUS.fault):
 *   EINVAL     buffer number too large, zero length, unknown count, non-zero spare
 *   EMSGSIZE   a job length other than sizeof(struct lemnos_stream_job)
 *   ERANGE     a ring head or tail that can't be (more than the capacity away)
 *   EOVERFLOW  addr + len wraps
 *   EFAULT     a range outside every registered region
 *   EACCES     a range in another buffer of the pool, or a buffer the consumer holds
 *   EBUSY      the buffer is already posted (double posting)
 *   EPERM      a returned buffer that isn't out, or with the wrong fence
 * A malformed consumer faults only its own stream.
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

/* The registers may be mapped (see above). */
#define LEMNOS_INFO_MMIO_MAPPABLE	(1u << 0)

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

struct lemnos_irq_event {
	__u32 block;
	__u32 irq;		/* index, below lemnos_info.irqs */
	__u32 count;		/* times it fired since the block was bound */
	__u32 reserved;
	__u64 ns;		/* ktime_get_ns() when it last fired */
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

struct lemnos_stream_create {
	__u32 block;		/* in */
	__u32 flags;		/* in: zero */
	__u32 buffers;		/* in: up to LEMNOS_STREAM_MAX_BUFFERS */
	__u32 buffer_bytes;	/* in: up to LEMNOS_STREAM_MAX_BUFFER_BYTES */
	__u32 frame_slots;	/* in: a power of two, at least buffers */
	__u32 job_slots;	/* in: a power of two, up to LEMNOS_STREAM_MAX_JOB_SLOTS */
	__u32 job_bytes;	/* in: at least sizeof(struct lemnos_stream_job) */
	__s32 pool_fd;		/* in: a dma-buf holding buffers * stride bytes */
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
