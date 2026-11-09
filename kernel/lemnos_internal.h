/* SPDX-License-Identifier: GPL-2.0 */
/* What the module's files share. Copyright (c) 2026 the Lemnos authors. */
#ifndef LEMNOS_INTERNAL_H
#define LEMNOS_INTERNAL_H

#include <linux/types.h>

#include "lemnos.h"

long lem_claim(struct lemnos_claim *c);

struct device;
struct file;
struct lem_stream;

/* Whether `dev` is one of the blocks the owned= parameter lists. */
bool lem_block_owned(struct device *dev);

long lem_stream_create(struct device *dev, struct lemnos_stream_create *c);
void lem_streams_block_gone(struct device *dev);

/* A device address a block is programmed with is aligned this much. */
#define LEM_STREAM_DMA_ALIGN 16u

/* A validated range: what the source uses; nothing the consumer can change. */
struct lem_job_range {
	u32 region;
	u64 offset;
	u64 len;
};

struct lem_job {
	u64 identity;
	u32 buffer;
	u32 count;
	struct lem_job_range range[LEMNOS_STREAM_JOB_RANGES];
};

/*
 * One frame a block holds, from lem_stream_next() until lem_stream_complete()
 * or lem_stream_cancel(). The buffer and every region its job names stay pinned
 * for that long. `addr` is the start of that buffer's memory, computed here
 * with overflow checks from what the module mapped; `bytes` are the frame's,
 * checked against the buffer when the producer attached.
 */
struct lem_stream_slot {
	struct lem_job job;
	u64 addr;
	u32 bytes;
};

/*
 * A block feeding a stream. stop() is called once with the stream ending (file
 * closed, or the block unbound), from process context, never with s->lock held:
 * it must stop the block and not return until the block can no longer touch
 * any buffer. A block that can't be sure calls lem_stream_wedge(), and this
 * stream's pins are then kept for good.
 */
struct lem_stream_producer {
	void (*stop)(void *priv, struct lem_stream *s);
	void *priv;
};

/* The stream open as `file`, if it was created on `dev`; an ERR_PTR otherwise. */
struct lem_stream *lem_stream_from_file(struct file *file, struct device *dev);
/* One producer per stream. `bytes` is what it may write per buffer. */
int lem_stream_attach(struct lem_stream *s, const struct lem_stream_producer *producer, u32 stride,
		      u32 height, u32 bytes);
/* Removes the producer; true when it was there and this call took it off. */
bool lem_stream_detach(struct lem_stream *s);
/* The next frame's buffer, any context: 0, or -ENOBUFS when none is free. */
int lem_stream_next(struct lem_stream *s, struct lem_stream_slot *slot);
/* Done with a slot: publish `bytes` of it, or 0 to say the frame went wrong. */
void lem_stream_complete(struct lem_stream *s, const struct lem_stream_slot *slot, u32 bytes,
			 u64 sequence, u64 timestamp_ns);
/* Done with a slot that was never written. */
void lem_stream_cancel(struct lem_stream *s, const struct lem_stream_slot *slot);
/* The block's DMA can't be shown to have stopped: keep everything pinned for good. */
void lem_stream_wedge(struct lem_stream *s);

int lem_rp1_csi2_register(void);
void lem_rp1_csi2_unregister(void);

#endif
