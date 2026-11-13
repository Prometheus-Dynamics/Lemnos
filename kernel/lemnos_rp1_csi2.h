/* SPDX-License-Identifier: GPL-2.0 WITH Linux-syscall-note */
/*
 * The rp1-csi2 receiver: the camera front-end block's D-PHY, its CSI-2 host
 * and one DMA channel, with frames delivered through a frame stream
 * (lemnos.h, "Frame streams"). The node is /dev/lemnos-rp1-csi2-<device>. Several
 * files may open it; each drives one part of the block by claiming it (see "Parts" below).
 *
 * The sensor is user space's: nothing here talks to it. START takes the link
 * the sensor was set up for and the stream to keep (virtual channel, data
 * type, layout) and the stream's file, and the receiver fills that stream's
 * buffers: which buffer each frame goes to is the stream's business (a posted
 * job, or a free buffer before the first job), and so is where a buffer is (a
 * slice of the stream's pool, or the buffer's own dma-buf on a stream made
 * with LEMNOS_STREAM_CREATE_BUFFERS, where a buffer with no memory yet is
 * never armed). There is no call that takes a memory address, and no way to
 * read or write the block's registers.
 *
 * A stream's file carries one receiver at most; closing the stream's file,
 * STOP, closing this node and unbinding the block all stop the receiver, and
 * none returns until the block's DMA has stopped and every buffer it held is
 * back. If the DMA can't be seen to stop, LEMNOS_RP1_CSI2_INFO_WEDGED is set:
 * the buffers involved stay pinned for good and the receiver refuses START
 * until it is bound again.
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#ifndef _UAPI_LEMNOS_RP1_CSI2_H
#define _UAPI_LEMNOS_RP1_CSI2_H

#include <linux/ioctl.h>
#include <linux/types.h>

#include "lemnos.h"

#define LEMNOS_RP1_CSI2_ABI 1

/* A previous stop couldn't be confirmed. Bind the block again to clear it. */
#define LEMNOS_RP1_CSI2_INFO_WEDGED	(1u << 0)

/*
 * Unpack each pixel to 16 bits, most significant bit aligned: the layout the
 * back end reads. Needs a stride of at least width * 2.
 */
#define LEMNOS_RP1_CSI2_UNPACK16	(1u << 0)

struct lemnos_rp1_csi2_info {
	__u32 abi;		/* LEMNOS_RP1_CSI2_ABI */
	__u32 host_version;	/* the CSI-2 host's VERSION register, read at probe */
	__u32 max_lanes;	/* data lanes wired */
	__u32 flags;		/* LEMNOS_RP1_CSI2_INFO_* */
	__u32 streaming;	/* nonzero between START and STOP */
	__u32 reserved;
	__u64 frames;		/* frame starts since START */
	__u64 written;		/* frames completed */
	__u64 dropped;		/* frame starts with no buffer armed */
	__u64 errors;		/* frames gone wrong: a start with one open, or the wrong line count */
};

struct lemnos_rp1_csi2_start {
	__s32 stream_fd;	/* a stream made on this receiver */
	__u32 lanes;		/* 1 to max_lanes */
	__u32 mbps;		/* per lane, 80 to 1500 */
	__u32 vc;		/* virtual channel, 0 to 3 */
	__u32 dt;		/* CSI-2 data type, 1 to 63 */
	__u32 width;		/* pixels, 1 to 65535 */
	__u32 height;		/* lines, 1 to 65535 */
	__u32 stride;		/* bytes per line, a multiple of 16 */
	__u32 size;		/* bytes per frame, a multiple of 16, within a buffer */
	__u32 flags;		/* LEMNOS_RP1_CSI2_UNPACK16 */
	__u32 reserved[2];	/* zero */
};

/*
 * The frame-start count, read with LEMNOS_RP1_CSI2_IOC_FRAME_STARTS. `frames`
 * counts the frame-start interrupts the channel raised since START, each one
 * whether or not a buffer was armed for it (with none, the frame is a drop). So
 * the frame that starts as the k-th counted start has sequence k - 1: the
 * sequence a published frame carries (lemnos.h, "Frame streams"). A channel with
 * no buffer address raises no interrupt, so a start then is in no count: `frames`
 * is the sensor's frame starts only while every start shows (see the driver's
 * docs and testing/board-runs/J4b2.md).
 * `latest_ns` is CLOCK_MONOTONIC (ktime_get_ns) at the latest start, the one
 * the count names, and 0 while `frames` is 0. `valid` is nonzero while the
 * receiver is streaming, which is when `frames` and `latest_ns` are the count
 * since START; it is zero before START and after STOP, and a restart starts
 * the count again at zero.
 */
struct lemnos_rp1_csi2_frame_starts {
	__u64 frames;		/* frame starts since START */
	__u64 latest_ns;	/* CLOCK_MONOTONIC of the latest start, 0 with none */
	__u32 valid;		/* nonzero while streaming */
	__u32 reserved;		/* zero */
};

#define LEMNOS_RP1_CSI2_IOC_INFO	 _IOR('L', 0xc0, struct lemnos_rp1_csi2_info)
#define LEMNOS_RP1_CSI2_IOC_STREAM_CREATE _IOWR('L', 0xc1, struct lemnos_stream_create)
#define LEMNOS_RP1_CSI2_IOC_START	 _IOW('L', 0xc2, struct lemnos_rp1_csi2_start)
#define LEMNOS_RP1_CSI2_IOC_STOP	 _IO('L', 0xc3)
#define LEMNOS_RP1_CSI2_IOC_FRAME_STARTS _IOR('L', 0xc4, struct lemnos_rp1_csi2_frame_starts)

/*
 * The embedded data stream: CSI-2 data type 0x12 on the image's virtual
 * channel, written by its own DMA channel, one buffer per frame. It is asked
 * for before START and takes effect at START. stream_fd -1 asks for none (the
 * default); a stream_fd names a stream made on this node, and bytes is the
 * embedded line's length, a multiple of 16 from 16 to 65520. Each frame's
 * buffer carries that frame's sequence and the line's bytes, or no bytes
 * when the frame had no embedded line or the line was cut short.
 */
#define LEMNOS_RP1_CSI2_EMBEDDED_DT	0x12
#define LEMNOS_RP1_CSI2_EMBEDDED_MAX	65520u

struct lemnos_rp1_csi2_embedded {
	__s32 stream_fd;	/* a stream made on this node, or -1 for none */
	__u32 bytes;		/* the embedded line's length; 0 with -1 */
	__u32 reserved[2];	/* zero */
};

#define LEMNOS_RP1_CSI2_IOC_EMBEDDED	 _IOW('L', 0xc5, struct lemnos_rp1_csi2_embedded)

/*
 * Parts. The block has two: the receiver (LEMNOS_RP1_CSI2_PART_CSI2: START, STOP,
 * EMBEDDED, and the frame streams made with STREAM_CREATE) and the front end on
 * the same block, when it has one (LEMNOS_RP1_CSI2_PART_FE: the front end's
 * START, STOP, TEST_TRACE and TEST_SCRIBBLE). A file drives a part only after it
 * has claimed it with LEMNOS_RP1_CSI2_IOC_CLAIM. Each part has one claimant at a
 * time: a claim of a part another file holds, or one this file already holds, is
 * EBUSY. Two files can hold different parts. A file's claims end when it is
 * closed (its receiver stops when the receiver's claim ends, and the front end
 * stops with it, since the front end is fed by the receiver); a claim the file
 * never made is refused with EPERM. Observe-only calls need no claim from any
 * file: INFO, FRAME_STARTS, and the front end's INFO.
 */
#define LEMNOS_RP1_CSI2_PART_CSI2	(1u << 0)
#define LEMNOS_RP1_CSI2_PART_FE		(1u << 1)

struct lemnos_rp1_csi2_claim {
	__u32 parts;		/* LEMNOS_RP1_CSI2_PART_* bits, one or more; EINVAL for a part the block lacks */
	__u32 reserved;		/* zero */
};

#define LEMNOS_RP1_CSI2_IOC_CLAIM	 _IOW('L', 0xc6, struct lemnos_rp1_csi2_claim)

#endif
