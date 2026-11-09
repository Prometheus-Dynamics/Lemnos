/* SPDX-License-Identifier: GPL-2.0 WITH Linux-syscall-note */
/*
 * The PiSP back end, memory to memory, on a frame stream of lemnos.ko. A block
 * named in the `pisp_be` parameter is driven here instead of being forwarded:
 * its registers and interrupt never reach user space.
 *
 * LEMNOS_IOC_PISP_BE_CREATE (on /dev/lemnos) makes a frame stream on such a
 * block. The ring, pool, registered dma-bufs and doorbell are the stream's
 * (lemnos.h); its job slots hold struct lemnos_pisp_be_job instead of
 * struct lemnos_stream_job.
 *
 * Writing a job: copy a struct pisp_be_tiles_config (uAPI pisp_be_config.h)
 * into a config slot (mmap the file at `config_mmap_offset`, `config_bytes`
 * long, shared), then post the job on the ring and call LEMNOS_STREAM_IOC_KICK
 * to learn at once if it was refused.
 *
 * The module copies the config slot into kernel memory before looking at it,
 * checks the copy and runs the hardware from the copy, so user space changing
 * the slot, the ring or a mapping afterwards has no effect on the job. Every
 * address a job names is a device address, one per address the hardware has,
 * and must lie within the job's pool buffer or a registered dma-buf for at
 * least the bytes the job's image formats and tiles will touch. Anything else
 * ends the stream with an errno (see EINVAL/ENOTSUPP notes below and lemnos.h
 * for the stream's own refusals). Nothing is clamped.
 *
 * Refused outright because the module cannot prove what they touch: wallpaper
 * layouts, HOG and integral-image output, negative strides, unknown format
 * bits, and non-zero bytes in the config's unused head and tail (they sit where
 * older layouts kept addresses).
 *
 * A job that does not finish in time ends the stream with ETIMEDOUT. A block
 * that cannot be shown idle afterwards is wedged: its in-flight jobs' buffers
 * are never released, even after close and unbind, because it may still write
 * to them, and only a reboot clears it. LEMNOS_PISP_BE_IOC_STATUS reports this.
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#ifndef _UAPI_LEMNOS_PISP_BE_H
#define _UAPI_LEMNOS_PISP_BE_H

#include <linux/ioctl.h>
#include <linux/types.h>

#include "lemnos.h"

#define LEMNOS_PISP_BE_ABI 2

/* The back end's addresses, in its register order. */
#define LEMNOS_PISP_BE_ADDRS 13
#define LEMNOS_PISP_BE_ADDR_INPUT0 0
#define LEMNOS_PISP_BE_ADDR_TDN_INPUT 3
#define LEMNOS_PISP_BE_ADDR_STITCH_INPUT 4
#define LEMNOS_PISP_BE_ADDR_TDN_OUTPUT 5
#define LEMNOS_PISP_BE_ADDR_STITCH_OUTPUT 6
#define LEMNOS_PISP_BE_ADDR_OUTPUT(n, plane) (7 + 3 * (n) + (plane))

/* A job, in a job slot of struct lemnos_stream_create.job_bytes bytes. */
struct lemnos_pisp_be_job {
	__u64 identity;			/* carried to the frame */
	__u32 buffer;			/* the pool buffer the frame fills */
	__u32 config;			/* the config slot, below job_slots */
	__u64 addr[LEMNOS_PISP_BE_ADDRS]; /* device addresses; 0 for an address it doesn't use */
};

struct lemnos_pisp_be_create {
	struct lemnos_stream_create stream; /* job_bytes: sizeof(struct lemnos_pisp_be_job) */
	__u32 config_slot_bytes;	/* out: sizeof(struct pisp_be_tiles_config) */
	__u32 hw_version;		/* out: the block's version register */
	__u64 config_mmap_offset;	/* out: what to give mmap() for the config slots */
	__u64 config_bytes;		/* out: their total bytes */
};

/* The block; state counted since it was bound. */
struct lemnos_pisp_be_status {
	__u32 hw_version;
	__u32 wedged;			/* nonzero: buffers are being held for good */
	__u64 finished;			/* jobs the block finished */
	__u64 refused;			/* jobs refused by the checks */
};

#define LEMNOS_IOC_PISP_BE_CREATE _IOWR('L', 0xb0, struct lemnos_pisp_be_create)
/* On the stream's file. */
#define LEMNOS_PISP_BE_IOC_STATUS _IOR('L', 0xb1, struct lemnos_pisp_be_status)

#endif
