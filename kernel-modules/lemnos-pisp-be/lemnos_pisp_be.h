/* SPDX-License-Identifier: GPL-2.0-only WITH Linux-syscall-note */
/*
 * lemnos_pisp_be: the Raspberry Pi PiSP back end as a plain character device,
 * /dev/lemnos-pisp-be. A fast path beside the stock V4L2 driver, never in
 * place of it: it binds only through driver_override.
 *
 * A job is a struct pisp_be_tiles_config (uAPI pisp_be_config.h) and one
 * dma-buf plane per back-end address. The module copies the configuration,
 * maps each dma-buf on the back end's own device and writes every address
 * itself; no address comes from user space. A finished job is a
 * struct lemnos_pisp_be_done, read() from the descriptor (poll for POLLIN).
 */
#ifndef _UAPI_LEMNOS_PISP_BE_H
#define _UAPI_LEMNOS_PISP_BE_H

#include <linux/ioctl.h>
#include <linux/types.h>

#define LEMNOS_PISP_BE_ABI 1

/*
 * The back end's addresses, in its register order: the main input's three
 * planes, the TDN input, the stitch input, the TDN output, the stitch output,
 * then three planes for each of the two image outputs.
 */
#define LEMNOS_PISP_BE_PLANES 13
#define LEMNOS_PISP_BE_PLANE_INPUT 0
#define LEMNOS_PISP_BE_PLANE_TDN_INPUT 3
#define LEMNOS_PISP_BE_PLANE_STITCH_INPUT 4
#define LEMNOS_PISP_BE_PLANE_TDN_OUTPUT 5
#define LEMNOS_PISP_BE_PLANE_STITCH_OUTPUT 6
#define LEMNOS_PISP_BE_PLANE_OUTPUT(n) (7 + 3 * (n))

/* One plane: a dma-buf and the plane's byte offset in it; fd -1 for none. */
struct lemnos_pisp_be_plane {
	__s32 fd;
	__u32 offset;
};

struct lemnos_pisp_be_info {
	__u32 abi;		/* LEMNOS_PISP_BE_ABI */
	__u32 hw_version;	/* the block's version register */
	__u32 queue_depth;	/* jobs that may be submitted and not yet read */
	__u32 dead;		/* nonzero after a job timed out */
};

struct lemnos_pisp_be_submit {
	__u64 config;		/* user pointer to a struct pisp_be_tiles_config */
	__u64 tag;		/* returned in the job's lemnos_pisp_be_done */
	struct lemnos_pisp_be_plane planes[LEMNOS_PISP_BE_PLANES];
};

/* What read() returns for each finished job, in completion order. */
struct lemnos_pisp_be_done {
	__u64 tag;
	__u64 irq_ns;		/* ktime_get_ns() in the interrupt that ended it */
	__s32 status;		/* 0, or -ETIMEDOUT when the block stopped */
	__u32 batch_status;	/* the batch status register at that interrupt */
	__u32 int_status;	/* the interrupt status the handler cleared */
	__u32 block_status;	/* the status register at that interrupt */
};

/* A soft-recovery attempt on a block that stopped. Neither revived one on a board. */
#define LEMNOS_PISP_BE_RECOVER_AXI	1	/* rewrite AXI, clear interrupts */
#define LEMNOS_PISP_BE_RECOVER_CLOCK	2	/* change the clock's rate and back */

struct lemnos_pisp_be_recover {
	__u32 mode;		/* in */
	__u32 status;		/* out: status register afterwards */
	__u32 batch_status;	/* out: batch status afterwards */
	__u32 revived;		/* out: nonzero when idle and done == started */
};

#define LEMNOS_PISP_BE_IOC_INFO    _IOR('L', 0xb0, struct lemnos_pisp_be_info)
#define LEMNOS_PISP_BE_IOC_SUBMIT  _IOW('L', 0xb1, struct lemnos_pisp_be_submit)
#define LEMNOS_PISP_BE_IOC_RECOVER _IOWR('L', 0xb2, struct lemnos_pisp_be_recover)

#endif
