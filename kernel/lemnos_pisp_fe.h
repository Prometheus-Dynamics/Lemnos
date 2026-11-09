/* SPDX-License-Identifier: GPL-2.0 WITH Linux-syscall-note */
/*
 * The PiSP front end, driven through the receiver's node
 * (/dev/lemnos-rp1-csi2-<device>, lemnos_rp1_csi2.h). It is the same block: the
 * receiver's channel hands its frames to the front end instead of memory. A
 * frame's image (output 0) goes to the stream's pool, its statistics and a
 * second raw output to registered dma-bufs. The stream is made with
 * LEMNOS_RP1_CSI2_IOC_STREAM_CREATE and started here, not with
 * LEMNOS_RP1_CSI2_IOC_START.
 *
 * A job on the stream's ring is one frame. Its pool buffer is output 0, and
 * its ranges are, in this order: the configuration (exactly
 * sizeof(struct pisp_fe_config) in a registered dma-buf, the module's copy is
 * what the hardware runs), then the statistics buffer (exactly
 * sizeof(struct pisp_statistics), named when and only when a statistics block
 * is enabled), then output 1 when it is enabled (its stride times its height,
 * at least). Nothing else may be named, and neither buffer may be the pool.
 *
 * The configuration's address fields, ilines and the dirty flags are not taken
 * from the caller: the module fills them in. Every address field a caller
 * leaves non-zero is refused, as is any format, size or enable bit that isn't
 * what START fixed and the configuration's own blocks describe. A compressed
 * output must have its compress block on, in the format's mode. The decompress
 * block is refused (EOPNOTSUPP): the input here is always 16 bits. The errno ends
 * the stream (LEMNOS_STREAM_IOC_STATUS, state 2): the first job's refusal also
 * comes back from START, any later one only through the stream's status.
 *
 * Everything START fixes (the link, output 0's size, stride and format) stays
 * fixed for the stream: a job can't change it. The sensor is the caller's.
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#ifndef _UAPI_LEMNOS_PISP_FE_H
#define _UAPI_LEMNOS_PISP_FE_H

#include <linux/ioctl.h>
#include <linux/types.h>

#include "lemnos.h"

#define LEMNOS_PISP_FE_ABI 1

/* A previous stop couldn't be confirmed. Bind the block again to clear it. */
#define LEMNOS_PISP_FE_INFO_WEDGED	(1u << 0)

struct lemnos_pisp_fe_info {
	__u32 abi;		/* LEMNOS_PISP_FE_ABI */
	__u32 hw_version;	/* the front end's VERSION register, read at probe */
	__u32 flags;		/* LEMNOS_PISP_FE_INFO_* */
	__u32 streaming;	/* nonzero between START and STOP */
	__u64 frames;		/* frame starts since START */
	__u64 written;		/* frames completed */
	__u64 dropped;		/* frame starts with no job queued */
	__u64 errors;		/* frames that were bad (no frame end before the next start) */
	__u64 stalled;		/* jobs the front end was not ready to queue */
};

struct lemnos_pisp_fe_start {
	__s32 stream_fd;	/* a stream made on this node */
	/* The receiver's link and frame, as LEMNOS_RP1_CSI2_IOC_START takes them. */
	__u32 lanes;		/* 1 to the block's max_lanes */
	__u32 mbps;		/* per lane, 80 to 1500 */
	__u32 vc;		/* virtual channel, 0 to 3 */
	__u32 dt;		/* CSI-2 data type, 1 to 63 */
	__u32 width;		/* the frame: a job's input must be exactly this */
	__u32 height;		/* the frame: a job's input must be exactly this */
	/* Output 0, fixed for the stream: every job's output 0 must match. */
	__u32 output_width;
	__u32 output_height;
	__u32 output_stride;	/* bytes per line, a multiple of 16 */
	__u32 output_size;	/* bytes per frame, within a pool buffer */
	__u32 output_format;	/* BPS_16, or a COMPRESSION_MODE_n with BPS_8; nothing else */
	__u32 flags;		/* zero */
	__u32 reserved[2];	/* zero */
};

/* Test only (test_pisp_fe): what the module wrote, in order. */
struct lemnos_pisp_fe_trace_entry {
	__u32 offset;		/* in the front end's window, or LEMNOS_PISP_FE_MARK_* */
	__u32 value;
};

#define LEMNOS_PISP_FE_MARK_CHANNEL_START	0xf000u
#define LEMNOS_PISP_FE_MARK_OPEN_RX		0xf001u
#define LEMNOS_PISP_FE_MARK_CHANNEL_STOP	0xf002u

struct lemnos_pisp_fe_test_trace {
	__u64 entries;		/* in: user pointer to the entries */
	__u32 max;		/* in: entries the array holds */
	__u32 count;		/* out: entries copied; the log is then empty */
};

#define LEMNOS_PISP_FE_IOC_INFO		_IOR('L', 0xc8, struct lemnos_pisp_fe_info)
#define LEMNOS_PISP_FE_IOC_START	_IOW('L', 0xc9, struct lemnos_pisp_fe_start)
#define LEMNOS_PISP_FE_IOC_STOP		_IO('L', 0xca)
#define LEMNOS_PISP_FE_IOC_TEST_TRACE	_IOWR('L', 0xcb, struct lemnos_pisp_fe_test_trace)
/* Test only: the next configuration read is overwritten right after it is copied. */
#define LEMNOS_PISP_FE_IOC_TEST_SCRIBBLE _IO('L', 0xcc)

#endif
