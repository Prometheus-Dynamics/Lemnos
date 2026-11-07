/* SPDX-License-Identifier: GPL-2.0-only WITH Linux-syscall-note */
/*
 * lemnos_rp1_csi2: the Raspberry Pi RP1 CSI-2 receiver (the CFE block's
 * D-PHY, CSI-2 host and CSI-2 DMA, without the PiSP front end) as a plain
 * character device, /dev/lemnos-rp1-csi2. A fast path beside the stock
 * rp1-cfe V4L2 driver, never in place of it: it binds only through
 * driver_override.
 *
 * The sensor is user space's: nothing here talks to it. START takes the link
 * (lanes, per-lane rate) and the stream (virtual channel, data type, size)
 * the sensor was set up for; QUEUE hands a dma-buf to write a frame into;
 * read() returns one struct lemnos_rp1_csi2_done per frame, in order.
 */
#ifndef _UAPI_LEMNOS_RP1_CSI2_H
#define _UAPI_LEMNOS_RP1_CSI2_H

#include <linux/ioctl.h>
#include <linux/types.h>

#define LEMNOS_RP1_CSI2_ABI 1

/* Frames that may be queued and not yet read. */
#define LEMNOS_RP1_CSI2_DEPTH 8

struct lemnos_rp1_csi2_info {
	__u32 abi;		/* LEMNOS_RP1_CSI2_ABI */
	__u32 host_version;	/* the CSI-2 host's VERSION register */
	__u32 max_lanes;	/* data lanes wired, from the device tree */
	__u32 depth;		/* LEMNOS_RP1_CSI2_DEPTH */
	__u32 streaming;	/* nonzero between START and STOP */
	__u32 frames;		/* frame starts since START */
	__u32 dropped;		/* frame starts with no buffer to write into */
	__u32 reserved;
};

/* Write lines packed, without stride padding (embedded data). */
#define LEMNOS_RP1_CSI2_PACK_BYTES	(1u << 0)
/*
 * Unpack each pixel of a RAW10/12/14 stream to 16 bits, most significant bit
 * aligned (rp1-cfe's 16-bit remap, V4L2's SBGGR16 and friends): what the PiSP
 * back end reads. Needs width and height, a stride of at least width x 2, and
 * not PACK_BYTES.
 */
#define LEMNOS_RP1_CSI2_UNPACK16	(1u << 1)

struct lemnos_rp1_csi2_start {
	__u32 lanes;		/* 1 to max_lanes */
	__u32 mbps;		/* per-lane rate, 80 to 1500: link frequency x 2 / 1e6 */
	__u32 vc;		/* virtual channel, 0 to 3 */
	__u32 dt;		/* CSI-2 data type (0x2b: RAW10), 1 to 63 */
	__u32 width;		/* pixels; with height, the frame-size check (0: none) */
	__u32 height;		/* lines */
	__u32 stride;		/* bytes per line in the buffer, a multiple of 16 */
	__u32 size;		/* bytes written at most, a multiple of 16 */
	__u32 flags;		/* LEMNOS_RP1_CSI2_PACK_BYTES */
	__u32 reserved[3];	/* zero */
};

/* A buffer for one frame: a dma-buf, the frame's 16-byte-aligned offset in it. */
struct lemnos_rp1_csi2_queue {
	__s32 fd;
	__u32 offset;
	__u64 tag;		/* returned in the frame's lemnos_rp1_csi2_done */
};

/* What read() returns for each buffer, in order. */
struct lemnos_rp1_csi2_done {
	__u64 tag;
	__u64 fs_ns;		/* ktime_get_ns() at the frame start */
	__u64 fe_ns;		/* ktime_get_ns() at the frame end (0 if none) */
	__u32 sequence;		/* frame starts before this one since START */
	__s32 status;		/* 0; -EIO: a frame start came before its end;
				 * -ECANCELED: returned by STOP or close */
};

#define LEMNOS_RP1_CSI2_IOC_INFO  _IOR('L', 0xc0, struct lemnos_rp1_csi2_info)
#define LEMNOS_RP1_CSI2_IOC_START _IOW('L', 0xc1, struct lemnos_rp1_csi2_start)
#define LEMNOS_RP1_CSI2_IOC_QUEUE _IOW('L', 0xc2, struct lemnos_rp1_csi2_queue)
#define LEMNOS_RP1_CSI2_IOC_STOP  _IO('L', 0xc3)

#endif
