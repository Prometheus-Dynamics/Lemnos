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
