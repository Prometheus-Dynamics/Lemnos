/* SPDX-License-Identifier: GPL-2.0 WITH Linux-syscall-note */
/*
 * The generic Lemnos block device, /dev/lemnos-<device name>: a platform
 * block claimed through driver_override, for a driver in user space.
 *
 * - LEMNOS_IOC_INFO: the ABI, the register regions and interrupts, the DMA
 *   address width, and whether the registers may be mapped.
 * - LEMNOS_IOC_REGION: one register region's physical range; mmap() at
 *   offset index * page size maps it, when LEMNOS_INFO_MMIO_MAPPABLE is set
 *   (the block's DMA cannot reach memory the module did not map) and the
 *   caller has CAP_SYS_RAWIO.
 * - LEMNOS_IOC_ALLOC: a DMA buffer for the block, exported as a dma-buf (mmap
 *   it for the CPU), with its device address.
 * - LEMNOS_IOC_IMPORT / LEMNOS_IOC_RELEASE: a dma-buf mapped on the block,
 *   contiguous in its address space, held until released or the file closes.
 * - Interrupts: read() returns struct lemnos_irq_event, one per interrupt
 *   that fired since its last enable, and poll() waits for one. An interrupt
 *   is masked from when it fires until LEMNOS_IOC_IRQ_ENABLE: the user-space
 *   driver clears the cause, then enables it. All start masked.
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

/* The registers may be mapped (see above). */
#define LEMNOS_INFO_MMIO_MAPPABLE	(1u << 0)

struct lemnos_info {
	__u32 abi;		/* LEMNOS_ABI */
	__u32 flags;		/* LEMNOS_INFO_* */
	__u32 regions;		/* register regions, up to LEMNOS_MAX_REGIONS */
	__u32 irqs;		/* interrupts forwarded, up to LEMNOS_MAX_IRQS */
	__u32 dma_bits;		/* device addresses are below 2^dma_bits */
	__u32 reserved[3];
};

struct lemnos_region {
	__u32 index;		/* in */
	__u32 reserved;
	__u64 start;		/* physical address */
	__u64 size;		/* bytes */
};

struct lemnos_alloc {
	__u64 size;		/* in: bytes, rounded up to pages */
	__u32 flags;		/* in: zero */
	__s32 fd;		/* out: the dma-buf, close-on-exec */
	__u64 addr;		/* out: the device address */
};

struct lemnos_import {
	__s32 fd;		/* in: a dma-buf */
	__u32 handle;		/* out: for LEMNOS_IOC_RELEASE, nonzero */
	__u64 addr;		/* out: the device address */
	__u64 size;		/* out: bytes mapped */
};

struct lemnos_irq_event {
	__u32 irq;		/* index, below lemnos_info.irqs */
	__u32 count;		/* times it fired since the module bound */
	__u64 ns;		/* ktime_get_ns() when it last fired */
};

#define LEMNOS_IOC_INFO       _IOR('L', 0xd0, struct lemnos_info)
#define LEMNOS_IOC_REGION     _IOWR('L', 0xd1, struct lemnos_region)
#define LEMNOS_IOC_ALLOC      _IOWR('L', 0xd2, struct lemnos_alloc)
#define LEMNOS_IOC_IMPORT     _IOWR('L', 0xd3, struct lemnos_import)
#define LEMNOS_IOC_RELEASE    _IOW('L', 0xd4, __u32)
#define LEMNOS_IOC_IRQ_ENABLE _IOW('L', 0xd5, __u32)
/* Test only, with the module's test_irq_inject: sets interrupt n pending at its controller. */
#define LEMNOS_IOC_TEST_IRQ   _IOW('L', 0xdf, __u32)

#endif
