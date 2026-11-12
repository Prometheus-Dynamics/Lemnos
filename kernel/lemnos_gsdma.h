/* SPDX-License-Identifier: GPL-2.0 WITH Linux-syscall-note */
/*
 * The K230 graphics DMA engine (GDMA), memory to memory, on a frame stream of
 * lemnos.ko. A block named in the `gsdma` parameter is driven here: its
 * registers and interrupt never reach user space.
 *
 * LEMNOS_IOC_GSDMA_CREATE (on /dev/lemnos) makes a frame stream on such a
 * block. The ring, pool, registered dma-bufs and doorbell are the stream's
 * (lemnos.h); its job slots hold struct lemnos_gsdma_job instead of
 * struct lemnos_stream_job. A pool buffer holds no image: it is how the
 * module counts a job and publishes its end, so one page each is enough. The
 * images are dma-bufs registered on the stream (LEMNOS_STREAM_IOC_REGISTER),
 * named by their device addresses.
 *
 * A job copies one frame of up to three planes, turned by quarter turns and
 * mirrored. The module reads the job once into kernel memory, works out from
 * the format, size and rotation exactly which bytes the engine will read and
 * write on each plane, and proves each of those spans inside one registered
 * dma-buf and below the end of the memory the engine can address. Only then
 * does it build the engine's descriptors, in memory of its own, from the
 * device addresses of the memory it pinned. Anything else ends the stream with
 * an errno:
 *   EINVAL   a format, size, pitch, plane count or rotation the engine doesn't
 *            take, a pitch shorter than a row, a non-zero reserved field
 *   EFAULT   a span outside every registered dma-buf (the stream's own refusal)
 *   EACCES   a span in the pool: images live in registered dma-bufs
 *   EOVERFLOW a span whose end wraps
 *   ENXIO    a span reaching past the memory the engine can address
 *   ENOTUNIQ a destination plane overlapping a source plane or another
 *            destination plane
 *
 * A job that doesn't finish in time is stopped: the channel is turned off and
 * on (as the vendor driver does), and once the engine's progress registers hold
 * still the job's frame is published with 0 bytes and the stream goes on. An
 * engine whose progress doesn't stop is wedged: the job's memory is never
 * released, even after close and unbind, new streams are refused with EIO, and
 * only a reboot clears it.
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#ifndef _UAPI_LEMNOS_GSDMA_H
#define _UAPI_LEMNOS_GSDMA_H

#include <linux/ioctl.h>
#include <linux/types.h>

#include "lemnos.h"

#define LEMNOS_GSDMA_ABI 1
#define LEMNOS_GSDMA_MAX_PLANES 3

/* lemnos_gsdma_job.rotation: DRM's bits (drm_mode.h), exactly one ROTATE. */
#define LEMNOS_GSDMA_ROTATE_0	(1u << 0)
#define LEMNOS_GSDMA_ROTATE_90	(1u << 1)
#define LEMNOS_GSDMA_ROTATE_180	(1u << 2)
#define LEMNOS_GSDMA_ROTATE_270	(1u << 3)
#define LEMNOS_GSDMA_REFLECT_X	(1u << 4)
#define LEMNOS_GSDMA_REFLECT_Y	(1u << 5)

/* One plane: where its first row starts on each side, and the bytes from row to row. */
struct lemnos_gsdma_plane {
	__u64 src_addr;			/* device address, in a registered dma-buf */
	__u64 dst_addr;
	__u32 src_pitch;		/* 1 to 65535, at least a row */
	__u32 dst_pitch;
};

/* A job, in a job slot of struct lemnos_stream_create.job_bytes bytes. */
struct lemnos_gsdma_job {
	__u64 identity;			/* carried to the frame */
	__u32 buffer;			/* the pool buffer the frame is counted by */
	__u32 fourcc;			/* DRM four-character code, the same on both sides */
	__u32 width;			/* the source's, in pixels: 1 to 65535 */
	__u32 height;
	__u32 rotation;			/* LEMNOS_GSDMA_ROTATE_* | LEMNOS_GSDMA_REFLECT_* */
	__u32 planes;			/* the format's plane count */
	struct lemnos_gsdma_plane plane[LEMNOS_GSDMA_MAX_PLANES];
};

struct lemnos_gsdma_create {
	struct lemnos_stream_create stream; /* job_bytes: sizeof(struct lemnos_gsdma_job) */
	__u64 reach;			/* out: one past the last device address the engine reaches */
};

/* The block; counted since it was bound. */
struct lemnos_gsdma_status {
	__u32 wedged;			/* nonzero: buffers are being held for good */
	__u32 reserved;
	__u64 finished;			/* jobs the engine finished */
	__u64 refused;			/* jobs refused by the checks */
	__u64 timeouts;			/* jobs stopped for taking too long */
};

#define LEMNOS_IOC_GSDMA_CREATE _IOWR('L', 0xe8, struct lemnos_gsdma_create)
/* On the stream's file. */
#define LEMNOS_GSDMA_IOC_STATUS _IOR('L', 0xe9, struct lemnos_gsdma_status)

#endif
