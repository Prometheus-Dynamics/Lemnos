/* SPDX-License-Identifier: GPL-2.0 WITH Linux-syscall-note */
/*
 * The VeriSilicon VIP9000 neural accelerator, on a frame stream of lemnos.ko.
 * A block named in the `vip9000` parameter is driven here: its registers,
 * interrupt and MMU never reach user space.
 *
 * LEMNOS_IOC_VIP_CREATE (on /dev/lemnos) makes a frame stream on such a block.
 * The ring, pool, registered dma-bufs and doorbell are the stream's (lemnos.h);
 * its job slots hold struct lemnos_vip_job instead of struct lemnos_stream_job.
 * A pool buffer holds no data: it is how the module counts a job and publishes
 * its end, so one page each is enough.
 *
 * Programs. LEMNOS_VIP_IOC_LOAD copies a program, the block's command words,
 * into kernel memory and checks it once (below). A job names a loaded program
 * and its slots; the module copies the program into a command buffer of its
 * own, which the block can read and not write, and runs it from there. The
 * caller's bytes are never run from memory the caller can still write.
 *
 * Addresses. The block's MMU translates every address the block uses. While a
 * job runs, its tables map exactly:
 *   - the module's command buffer, read-only, at LEMNOS_VIP_PROGRAM_ADDR;
 *   - slot n of the job at LEMNOS_VIP_SLOT_ADDR(n), its bytes and no more,
 *     writable, from a dma-buf registered on the stream.
 * Nothing else: no heap of the module's, no on-chip memory, nothing of another
 * job. Every entry for a slot is cleared when its job ends. An address outside
 * them is an MMU fault: the job's frame is published with 0 bytes, the block is
 * reset and set up again, and the stream goes on.
 *
 * What a program may hold, checked at LEMNOS_VIP_IOC_LOAD (EPERM otherwise):
 *   - LOAD_STATE, except to a state below 0x400 (the block's control
 *     registers: clocks, interrupts, the MMU, the command fetcher), the event
 *     state 0x0E01 (the module's completion) and 0x0E12 (part of the MMU's
 *     set-up);
 *   - NOP and STALL.
 * Every other command, END, WAIT, LINK, CALL and RETURN among them, is refused:
 * the block would leave the module's copy, end before the module's event, or
 * change its own translation. A command running past the end of the program is
 * EINVAL.
 *
 * Refusals of a job, which end the stream with the errno:
 *   EINVAL   an unknown program, more slots than LEMNOS_VIP_SLOTS, a slot not
 *            4096-aligned or not a whole number of pages, longer than
 *            LEMNOS_VIP_SLOT_SPAN, a non-zero unused slot or reserved field
 *   EFAULT   a slot outside every registered dma-buf (the stream's refusal)
 *   EACCES   a slot in the pool: data lives in registered dma-bufs
 *   EOVERFLOW a slot whose end wraps
 *
 * A job that doesn't finish in time, or faults, is stopped by a reset of the
 * block. Its frame is published with 0 bytes and the stream goes on. A block
 * that cannot be shown reset is wedged: the job's memory is never released,
 * even after close and unbind, new streams are refused with EIO, and only a
 * reboot clears it.
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#ifndef _UAPI_LEMNOS_VIP_H
#define _UAPI_LEMNOS_VIP_H

#include <linux/ioctl.h>
#include <linux/types.h>

#include "lemnos.h"

#define LEMNOS_VIP_ABI 1

/* Slots a job may name. */
#define LEMNOS_VIP_SLOTS 13
/* Programs a stream holds at once. */
#define LEMNOS_VIP_PROGRAMS 8
/* The largest program, in bytes: whole 8-byte command words. */
#define LEMNOS_VIP_MAX_PROGRAM_BYTES (65535u * 8 - 64)

/* Where the block sees its memory while a job runs. */
#define LEMNOS_VIP_PROGRAM_ADDR 0x00100000u
#define LEMNOS_VIP_SLOT_SPAN (1u << 28)
#define LEMNOS_VIP_SLOT_ADDR(n) (((__u32)(n) + 1) << 28)

/* A job, in a job slot of struct lemnos_stream_create.job_bytes bytes. */
struct lemnos_vip_job {
	__u64 identity;			/* carried to the frame */
	__u32 buffer;			/* the pool buffer the frame is counted by */
	__u32 program;			/* from LEMNOS_VIP_IOC_LOAD */
	__u32 slots;			/* slots used, from 0; the rest are zero */
	__u32 reserved;
	struct lemnos_stream_range slot[LEMNOS_VIP_SLOTS]; /* device addresses, registered */
};

/* lemnos_vip_create.mmu: how the module points the block at its tables. */
#define LEMNOS_VIP_MMU_COMMAND 1	/* a page table array, selected by a command */
#define LEMNOS_VIP_MMU_PD 2		/* the page directory register */

struct lemnos_vip_create {
	struct lemnos_stream_create stream; /* job_bytes: sizeof(struct lemnos_vip_job) */
	__u32 chip;			/* out: the block's customer id */
	__u32 mmu;			/* out: LEMNOS_VIP_MMU_* */
};

struct lemnos_vip_load {
	__u64 data;			/* in: the program's bytes, a user pointer */
	__u32 bytes;			/* in: a multiple of 8, up to LEMNOS_VIP_MAX_PROGRAM_BYTES */
	__u32 program;			/* out: 1 to LEMNOS_VIP_PROGRAMS */
	__u32 refused_at;		/* out, on EPERM or EINVAL: the byte offset of the command */
	__u32 reserved;
};

struct lemnos_vip_unload {
	__u32 program;
	__u32 reserved;
};

/* The block; counted since it was bound. */
struct lemnos_vip_status {
	__u32 wedged;			/* nonzero: buffers are being held for good */
	__u32 chip;
	__u64 finished;			/* jobs the block finished */
	__u64 refused;			/* jobs refused by the checks */
	__u64 faults;			/* jobs stopped by an MMU or bus fault */
	__u64 timeouts;			/* jobs stopped for taking too long */
	__u32 last_fault;		/* the interrupt status of the last fault */
	__u32 resets;			/* resets since bound */
};

#define LEMNOS_IOC_VIP_CREATE _IOWR('L', 0xe0, struct lemnos_vip_create)
/* On the stream's file. */
#define LEMNOS_VIP_IOC_LOAD _IOWR('L', 0xe1, struct lemnos_vip_load)
#define LEMNOS_VIP_IOC_UNLOAD _IOW('L', 0xe2, struct lemnos_vip_unload)
#define LEMNOS_VIP_IOC_STATUS _IOR('L', 0xe3, struct lemnos_vip_status)

#endif
