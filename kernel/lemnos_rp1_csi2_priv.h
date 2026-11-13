/* SPDX-License-Identifier: GPL-2.0 */
/*
 * What the receiver (lemnos_rp1_csi2.c) and the front end on the same block
 * (lemnos_pisp_fe.c) share. Copyright (c) 2026 the Lemnos authors.
 */
#ifndef LEMNOS_RP1_CSI2_PRIV_H
#define LEMNOS_RP1_CSI2_PRIV_H

#include <linux/io.h>
#include <linux/kref.h>
#include <linux/miscdevice.h>
#include <linux/mutex.h>
#include <linux/rwsem.h>
#include <linux/spinlock.h>
#include <linux/workqueue.h>
#include <linux/types.h>

#include "lemnos_internal.h"
#include "lemnos_rp1_csi2.h"

struct lem_pisp_fe;

struct lrc_frame {
	struct lem_stream_slot slot;
	bool valid;
};

/*
 * The embedded data channel (CSI-2 data type 0x12 on the image's virtual
 * channel): its own stream, its own buffers, its own frame accounting. See
 * lemnos_rp1_csi2.c, "Embedded data".
 */
struct lrc_emb {
	int req_fd;			/* asked for before START; -1: none */
	u32 req_bytes;
	struct lem_stream *stream;	/* non-NULL while streaming */
	u32 bytes;			/* the line's length, from START */
	struct lrc_frame cur, next;	/* cur: a line being written; next: armed */
	u64 seq, fs_ns;			/* cur's frame, and its start */
	u16 fe_line;			/* the channel's line count at the last frame end */
};

/*
 * One open file of the node: the parts it has claimed (LEMNOS_RP1_CSI2_PART_*). `parts`
 * is written only by this file's own CLAIM, under l->ctl; the file's own calls read it.
 */
struct lrc_file {
	struct lrc_dev *l;
	u32 parts;
};

struct lrc_dev {
	struct kref ref;		/* the block, each open file, each attached stream */
	struct device *dev;		/* held until freed */
	void __iomem *dma, *host, *cfg;
	int irq;
	u32 host_version, max_lanes;
	u8 polarity[5];			/* clock, then data lanes 0-3 */
	struct miscdevice misc;
	char name[64];
	/* The file that claims each part (index: the part's bit, LEMNOS_RP1_CSI2_PART_*); under ctl. */
	struct lrc_file *owner[2];

	struct rw_semaphore going;	/* stream creation against unbinding */
	struct mutex ctl;		/* START, STOP, close, unbind */
	bool gone;			/* unbound: every register access is off */
	bool wedged;			/* a stop couldn't be confirmed */

	struct lem_pisp_fe *fe;		/* the front end, when the block has one; NULL otherwise */
	spinlock_t lock;		/* the frame path, everything below */
	struct lem_stream *stream;	/* non-NULL while streaming */
	bool streaming, fs_int, fe_int;
	struct lrc_frame cur, next;	/* being written; in the channel's shadow registers */
	u64 fs_ns;			/* the latest frame start, armed or not (0 before one) */
	u16 fe_line;			/* the channel's line count at the last frame end */
	struct lemnos_rp1_csi2_start start;
	struct lrc_emb emb;
	u64 frames, written, dropped, errors;
	/* Arms again while nothing is armed and no buffer was free; see lrc_arm(). */
	struct delayed_work retry;
};


void lrc_ref_get(struct lrc_dev *l);
void lrc_ref_put(struct lrc_dev *l);

/*
 * The link with the channel feeding the front end instead of memory: the
 * channel in its streaming mode, then the D-PHY and host once the front end is
 * ready for frames. Called holding l->ctl with nothing streaming. See cfe.c.
 */
void lrc_fe_channel_start(struct lrc_dev *l, u32 width, u32 height, u32 vc, u32 dt);
void lrc_fe_open_rx(struct lrc_dev *l, u32 lanes, u32 mbps);
void lrc_fe_channel_stop(struct lrc_dev *l);

/* lemnos_pisp_fe.c */
/* The front end's register window, from its configuration block's end. */
#define LEM_PISP_FE_WINDOW	(0x40 + 0x270)
struct lem_pisp_fe *lem_pisp_fe_create(struct lrc_dev *l, void __iomem *window);
void lem_pisp_fe_destroy(struct lem_pisp_fe *fe);
long lem_pisp_fe_ioctl(struct lem_pisp_fe *fe, unsigned int cmd, void __user *arg);
/* l->ctl held; nothing streaming afterwards, or the block is wedged. */
void lem_pisp_fe_stop(struct lem_pisp_fe *fe);
void lem_pisp_fe_isr(struct lem_pisp_fe *fe, u64 now);
void lem_pisp_fe_csi_fs(struct lem_pisp_fe *fe);
bool lem_pisp_fe_streaming(struct lem_pisp_fe *fe);

#endif
