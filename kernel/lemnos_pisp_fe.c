// SPDX-License-Identifier: GPL-2.0-only
/*
 * The PiSP front end of the camera front-end block, feeding a frame stream.
 *
 * Bound through the receiver (lemnos_rp1_csi2.c): both are one platform device
 * and one interrupt, so this file is reached through the receiver's node and
 * uses the receiver's channel to hand frames to the block's own stream input.
 *
 * Ported from the stock driver,
 * drivers/media/platform/raspberrypi/rp1-cfe/pisp-fe.c:
 *   Copyright (c) 2021-2024 Raspberry Pi Ltd.
 * The register offsets, the job submission order (input and global first, then
 * the other blocks, then QUEUE), start, stop and the interrupt bits. The
 * start sequence relative to the receiver is cfe.c's: channel, front end,
 * first job, then the link. The rest, the validation in particular, is new:
 *   Copyright (c) 2026 the Lemnos authors.
 *
 * What runs on the block is decided by the module, not the caller:
 *  - The configuration is copied once, into kernel memory, from a dma-buf the
 *    consumer also has mapped. Only that copy is checked and only that copy
 *    is written to the block. There is no later read of the consumer's memory.
 *  - The front end's DMA addresses (statistics, output 0, output 1, the unused
 *    memory input) all live in the configuration. None is taken from it: every
 *    one must be zero in the copy and is filled in here, from the frame's
 *    stream buffer and its job's ranges, which are the stream's checks and are
 *    resolved to pinned dma-bufs.
 *  - Sizes are the ones the block will really use: the input is fixed by
 *    START, an output's size is derived from its crop and downscale and must
 *    agree with its format, and an output's stride times height must fit
 *    the buffer its address came from. Refused, not clamped, on any
 *    disagreement.
 *  - Output 0's address is the frame's buffer, from lem_stream_next(): its
 *    slice of the pool, or its own dma-buf on a stream without a pool.
 *  - Every register of the configuration is written from the checked copy on
 *    every job; the caller's dirty flags are ignored. The hardware cannot end
 *    up running a stale value the checks never saw.
 *  - Only the front end's own window is touched here, at constant offsets.
 *
 * Stopping is one path: the channel first, then ABORT, then waiting for the
 * block to report idle (bounded). If it never does, nothing it could reach is
 * released: the stream is wedged, its pins kept for good, and START is
 * refused until the block is bound again.
 *
 * Locking: l->ctl orders START, STOP, close and unbind (as for the receiver;
 * see lemnos_rp1_csi2.c). fe->lock is the frame path's, nesting the stream's
 * s->lock. s->ctl is taken only before l->ctl (START enables the stream's CPU
 * mapping first) or by the stream's own stop callback, in the same order.
 */
#include <linux/delay.h>
#include <linux/file.h>
#include <linux/fs.h>
#include <linux/interrupt.h>
#include <linux/io.h>
#include <linux/ktime.h>
#include <linux/module.h>
#include <linux/moduleparam.h>
#include <linux/mutex.h>
#include <linux/overflow.h>
#include <linux/slab.h>
#include <linux/spinlock.h>
#include <linux/string.h>
#include <linux/uaccess.h>
#include <linux/workqueue.h>

#include <uapi/linux/media/raspberrypi/pisp_fe_config.h>

#include "lemnos_internal.h"
#include "lemnos_pisp_fe.h"
#include "lemnos_rp1_csi2_priv.h"

static bool test_pisp_fe;
module_param(test_pisp_fe, bool, 0444);
MODULE_PARM_DESC(test_pisp_fe,
		 "Test only: keeps a log of the front end's register writes, read by "
		 "LEMNOS_PISP_FE_IOC_TEST_TRACE, and enables LEMNOS_PISP_FE_IOC_TEST_SCRIBBLE");

/* Registers (pisp-fe.c). */
#define FE_VERSION		0x000
#define FE_CONTROL		0x004
#define FE_STATUS		0x008
#define FE_FRAME_STATUS		0x00c
#define FE_ERROR_STATUS		0x010
#define FE_OUTPUT_STATUS	0x014
#define FE_INT_EN		0x018
#define FE_INT_STATUS		0x01c
#define FE_CONTROL_QUEUE	BIT(0)
#define FE_CONTROL_ABORT	BIT(1)
#define FE_CONTROL_RESET	BIT(2)
#define FE_CONTROL_LATCH_REGS	BIT(3)
#define FE_INT_EOF		BIT(0)
#define FE_INT_SOF		BIT(1)
#define FE_INT_LINES0		BIT(8)
#define FE_INT_LINES1		BIT(9)
#define FE_STATUS_QUEUED	BIT(0)
#define FE_STATUS_WAITING	BIT(1)
#define FE_STATUS_ACTIVE	BIT(2)
#define FE_CONFIG_BASE		0x0040

/* The configuration registers: everything up to the non-register dirty flags. */
#define FE_CONFIG_BYTES		offsetof(struct pisp_fe_config, ch[PISP_FE_NUM_OUTPUTS])

#define LPF_ENABLE_STATS_CLUSTER \
	(PISP_FE_ENABLE_STATS_CROP | PISP_FE_ENABLE_DECIMATE | PISP_FE_ENABLE_BLC | \
	 PISP_FE_ENABLE_CDAF_STATS | PISP_FE_ENABLE_AWB_STATS | PISP_FE_ENABLE_RGBY | \
	 PISP_FE_ENABLE_LSC | PISP_FE_ENABLE_AGC_STATS)
#define LPF_ENABLE_OUTPUT_CLUSTER(i) \
	((PISP_FE_ENABLE_CROP0 | PISP_FE_ENABLE_DOWNSCALE0 | PISP_FE_ENABLE_COMPRESS0 | \
	  PISP_FE_ENABLE_OUTPUT0) << (4 * (i)))
#define LPF_ENABLE_KNOWN \
	(PISP_FE_ENABLE_INPUT | PISP_FE_ENABLE_DECOMPRESS | PISP_FE_ENABLE_DECOMPAND | \
	 PISP_FE_ENABLE_BLA | PISP_FE_ENABLE_DPC | LPF_ENABLE_STATS_CLUSTER | \
	 LPF_ENABLE_OUTPUT_CLUSTER(0) | LPF_ENABLE_OUTPUT_CLUSTER(1))

/* The output AXI settings the block is run with; anything else is refused. */
#define LPF_AXI_MAXLEN_FLAGS	0xaf
#define LPF_AXI_CACHE_PROT	0x32
#define LPF_AXI_QOS		0x8410
#define LPF_AXI_THRESH		0x0140
#define LPF_AXI_THROTTLE	0x4100

/*
 * Waiting for the block to go idle after ABORT: STATUS must read zero this many
 * times in a row, a read every 200-400 us, at most LPF_STOP_POLLS reads.
 */
#define LPF_STOP_STILL_READS	8
#define LPF_STOP_POLLS		250

/* How often a front end with no job queued looks for a free buffer again. */
#define LPF_RETRY_MS		1

/* Test only: entries the write log holds. */
#define LPF_TRACE_ENTRIES	2048

struct lpf_frame {
	struct lem_stream_slot slot;
	bool valid;
};

struct lem_pisp_fe {
	struct lrc_dev *l;		/* our receiver; holds us for its life */
	void __iomem *base;		/* our window, checked at probe */
	u32 hw_version;

	spinlock_t lock;		/* the frame path, everything below */
	struct lem_stream *stream;	/* non-NULL from START until stopped */
	bool streaming;
	bool test_scribble;		/* test only: the next copy gets overwritten */
	struct lpf_frame cur, next;	/* being written; queued for the next frame */
	bool fs_int, fe_int;
	/*
	 * The receiver started a frame the front end has not: a front end with
	 * no job queued starts nothing and raises nothing, so the receiver's
	 * frame start is the only sign of a frame it let go by.
	 */
	bool csi_pending;
	u64 fs_ns;
	/*
	 * The copy of the job being prepared, kernel memory no one else
	 * sees, and the fields a job must match, both under the lock.
	 */
	struct pisp_fe_config cfg;
	struct lemnos_pisp_fe_start start;
	u64 frames, written, dropped, errors, stalled;

	/* Queues again while nothing is queued and no buffer was free; see lpf_arm(). */
	struct delayed_work retry;

	/* Test only, NULL unless test_pisp_fe. */
	struct lemnos_pisp_fe_trace_entry *trace;
	u32 trace_count;
};

/* --- registers: the only way this file touches the block --- */

static void lpf_trace(struct lem_pisp_fe *fe, u32 offset, u32 value)
{
	if (fe->trace && fe->trace_count < LPF_TRACE_ENTRIES)
		fe->trace[fe->trace_count++] = (struct lemnos_pisp_fe_trace_entry){ offset, value };
}

static u32 lpf_rd(struct lem_pisp_fe *fe, u32 offset)
{
	return readl(fe->base + offset);
}

static void lpf_wr(struct lem_pisp_fe *fe, u32 offset, u32 value)
{
	lpf_trace(fe, offset, value);
	writel(value, fe->base + offset);
}

static void lpf_wr_relaxed(struct lem_pisp_fe *fe, u32 offset, u32 value)
{
	lpf_trace(fe, offset, value);
	writel_relaxed(value, fe->base + offset);
}

/* --- the checks, on the kernel's copy only --- */

static bool lpf_is_zero(const void *p, size_t n)
{
	return !memchr_inv(p, 0, n);
}

/*
 * The only two kinds of line an output writes, as in the stock driver's front
 * end formats: 16 bits a sample, or a compressed mode at 8 bits a sample.
 * Uncompressed 8, 10 and 12 bit aren't offered by the stock driver either.
 */
static int lpf_check_format(u32 format)
{
	u32 mode = format & PISP_IMAGE_FORMAT_COMPRESSION_MASK;
	u32 bps = format & PISP_IMAGE_FORMAT_BPS_MASK;

	if (format & ~(PISP_IMAGE_FORMAT_BPS_MASK | PISP_IMAGE_FORMAT_COMPRESSION_MASK))
		return -EINVAL;
	if (mode ? bps != PISP_IMAGE_FORMAT_BPS_8 : bps != PISP_IMAGE_FORMAT_BPS_16)
		return -EINVAL;
	return 0;
}

/*
 * An output branch's format against what the block will really make of its
 * input, and the bytes it will write. `cfg` is the kernel's copy.
 */
static int lpf_check_output(const struct pisp_fe_config *cfg, unsigned int i, u64 *bytes)
{
	const struct pisp_fe_output_branch_config *ch = &cfg->ch[i];
	const struct pisp_image_format_config *o = &ch->output.format;
	u32 enables = cfg->global.enables;
	u32 iw = cfg->input.format.width, ih = cfg->input.format.height;
	u32 w = iw, h = ih;
	u32 wbytes, format = o->format, bps = format & PISP_IMAGE_FORMAT_BPS_MASK;
	u32 mode = format & PISP_IMAGE_FORMAT_COMPRESSION_MASK;
	bool compressing = enables & PISP_FE_ENABLE_COMPRESS(i);

	/* A zero-sized crop can lock the block (pisp_fe_validate_output). */
	if (enables & PISP_FE_ENABLE_CROP(i)) {
		const struct pisp_fe_crop_config *c = &ch->crop;

		if (c->width < 2 || c->height < 2 || c->offset_x >= (iw & ~1u) ||
		    c->offset_y >= ih)
			return -EINVAL;
		if ((u32)c->offset_x + c->width > iw || (u32)c->offset_y + c->height > ih)
			return -EINVAL;
		w = c->width;
		h = c->height;
	}
	if (enables & PISP_FE_ENABLE_DOWNSCALE(i)) {
		const struct pisp_fe_downscale_config *d = &ch->downscale;
		u32 dw, dh;

		if (!d->xin || !d->yin || !d->xout || !d->yout || d->xout > d->xin ||
		    d->yout > d->yin || (d->flags & ~(u8)3))
			return -EINVAL;
		dw = ((w >> 1) * d->xout / d->xin) * 2;
		dh = ((h >> 1) * d->yout / d->yin) * 2;
		if (dw < 2 || dh < 2 || d->output_width != dw || d->output_height != dh)
			return -EINVAL;
		w = dw;
		h = dh;
	}

	/*
	 * Every other format bit (planes, sampling, order, shift, 32 bpp, HOG,
	 * integral image, wallpaper, three channels) changes what a line is.
	 */
	if (lpf_check_format(format))
		return -EINVAL;
	/* The enable changes the unit, so it must agree with the format that sizes it. */
	if (compressing != !!mode)
		return -EINVAL;
	if (compressing && ch->compress.mode != mode / PISP_IMAGE_FORMAT_COMPRESSION_MODE_1)
		return -EINVAL;

	/* The stride and size the block will use, and it must agree with the format. */
	if (o->width != w || o->height != h || w < 2 || h < 2)
		return -EINVAL;
	if (o->stride <= 0 || (o->stride & 15) || o->stride2)
		return -EINVAL;
	/* Compressed lines are written in blocks of 8 pixels, a byte each. */
	wbytes = bps == PISP_IMAGE_FORMAT_BPS_16 ? 2 * w : ALIGN(w, 8);
	if (wbytes > (u32)o->stride)
		return -EINVAL;
	return check_mul_overflow((u64)(u32)o->stride, (u64)h, bytes) ? -EOVERFLOW : 0;
}

/*
 * A zero-sized crop can lock the block (pisp_fe_validate_stats); decimating
 * halves what the statistics path sees, so a crop that decimates to nothing is
 * the same crop. The grids the library always sizes at 2 or more are held to it
 * for the same reason. Unused floating regions are legitimately empty. The
 * statistics buffer's size doesn't depend on any of this.
 */
static int lpf_check_stats(const struct pisp_fe_config *cfg)
{
	u32 enables = cfg->global.enables;
	u32 min = enables & PISP_FE_ENABLE_DECIMATE ? 4 : 2;
	u32 iw = cfg->input.format.width, ih = cfg->input.format.height;

	if (enables & PISP_FE_ENABLE_STATS_CROP) {
		const struct pisp_fe_crop_config *c = &cfg->stats_crop;

		if (c->width < min || c->height < min || c->offset_x >= (iw & ~1u) ||
		    c->offset_y >= ih || (u32)c->offset_x + c->width > iw ||
		    (u32)c->offset_y + c->height > ih)
			return -EINVAL;
	} else if (iw < min || ih < min) {
		return -EINVAL;
	}
	if ((enables & PISP_FE_ENABLE_AGC_STATS) &&
	    (cfg->agc_stats.size_x < 2 || cfg->agc_stats.size_y < 2 ||
	     cfg->agc_stats.row_size_x < 2 || cfg->agc_stats.row_size_y < 2))
		return -EINVAL;
	if ((enables & PISP_FE_ENABLE_AWB_STATS) &&
	    (cfg->awb_stats.size_x < 2 || cfg->awb_stats.size_y < 2))
		return -EINVAL;
	if ((enables & PISP_FE_ENABLE_CDAF_STATS) &&
	    (cfg->cdaf_stats.size_x < 2 || cfg->cdaf_stats.size_y < 2))
		return -EINVAL;
	return 0;
}

/* The padding the block's registers have holes for: must be zero. Kernel copy. */
static int lpf_check_padding(const struct pisp_fe_config *cfg)
{
	if (!lpf_is_zero(cfg->global.pad, sizeof(cfg->global.pad)) ||
	    !lpf_is_zero(cfg->input.pad, sizeof(cfg->input.pad)) ||
	    !lpf_is_zero(cfg->input.pad2, sizeof(cfg->input.pad2)) ||
	    cfg->decompand.pad || cfg->decompress.pad || !lpf_is_zero(cfg->bla.pad, sizeof(cfg->bla.pad)) ||
	    !lpf_is_zero(cfg->blc.pad, sizeof(cfg->blc.pad)) ||
	    cfg->lsc.pad0 || cfg->rgby.pad || !lpf_is_zero(cfg->agc_stats.pad1,
							   sizeof(cfg->agc_stats.pad1)) ||
	    !lpf_is_zero(cfg->awb_stats.pad, sizeof(cfg->awb_stats.pad)) || cfg->spare1)
		return -EINVAL;
	for (unsigned int i = 0; i < PISP_FE_NUM_OUTPUTS; i++)
		if (!lpf_is_zero(cfg->ch[i].downscale.pad, sizeof(cfg->ch[i].downscale.pad)) ||
		    !lpf_is_zero(cfg->ch[i].output.pad, sizeof(cfg->ch[i].output.pad)) ||
		    cfg->ch[i].pad || cfg->ch[i].compress.pad)
			return -EINVAL;
	return 0;
}

/*
 * Checks a job's configuration, on the kernel's copy, against what START fixed
 * and the buffers the job's ranges name, and fills in the address fields the
 * caller may not set. Nothing here touches the block.
 */
static int lpf_validate(struct lem_pisp_fe *fe, struct pisp_fe_config *cfg,
			const struct lem_stream_slot *slot)
{
	const struct lemnos_pisp_fe_start *start = &fe->start;
	struct lem_stream *s = fe->stream;
	const struct lem_job *job = &slot->job;
	u32 enables = cfg->global.enables;
	bool stats = enables & LPF_ENABLE_STATS_CLUSTER;
	bool out1 = enables & PISP_FE_ENABLE_OUTPUT1;
	u64 stats_addr = 0, out1_addr = 0, bytes;
	u32 want = 1 + stats + out1, r = 1;
	int ret;

	/* Enables first: nothing else means anything until they are known good. */
	if (enables & ~LPF_ENABLE_KNOWN)
		return -EINVAL;
	if (!(enables & PISP_FE_ENABLE_INPUT) || !(enables & PISP_FE_ENABLE_OUTPUT0))
		return -EINVAL;
	/* Changes the input's unit, and this module's input is always 16 bits: nothing sizes it. */
	if (enables & PISP_FE_ENABLE_DECOMPRESS)
		return -EOPNOTSUPP;
	/* Disabled outputs: no crop, downscale or compression may be left on. */
	if (!(enables & PISP_FE_ENABLE_OUTPUT(0)) && (enables & LPF_ENABLE_OUTPUT_CLUSTER(0)))
		return -EINVAL;
	if (!(enables & PISP_FE_ENABLE_OUTPUT(1)) && (enables & LPF_ENABLE_OUTPUT_CLUSTER(1)))
		return -EINVAL;

	/* Every address field: only ever filled in below. */
	if (cfg->stats_buffer.addr_lo || cfg->stats_buffer.addr_hi ||
	    cfg->input_buffer.addr_lo || cfg->input_buffer.addr_hi ||
	    cfg->input_buffer.frame_id || cfg->input_buffer.pad)
		return -EINVAL;
	for (unsigned int i = 0; i < PISP_FE_NUM_OUTPUTS; i++)
		if (cfg->output_buffer[i].addr_lo || cfg->output_buffer[i].addr_hi ||
		    cfg->ch[i].output.ilines)
			return -EINVAL;

	/* The job must name exactly what the enables need, in the order lemnos_pisp_fe.h gives. */
	if (job->count != want)
		return -EINVAL;

	/* Input: the frame on the link, unchanged, from the stream interface only. */
	if (cfg->input.streaming != 1 || cfg->input.format.format != PISP_IMAGE_FORMAT_BPS_16 ||
	    cfg->input.format.width != start->width || cfg->input.format.height != start->height ||
	    cfg->input.format.stride || cfg->input.format.stride2 ||
	    start->width < 2 || start->height < 2)
		return -EINVAL;
	/* The memory-input path's settings: this module only ever uses the stream. */
	if (cfg->input.axi.maxlen_flags || cfg->input.axi.cache_prot || cfg->input.axi.qos ||
	    cfg->input.holdoff)
		return -EINVAL;
	if (cfg->output_axi.maxlen_flags != LPF_AXI_MAXLEN_FLAGS ||
	    cfg->output_axi.cache_prot != LPF_AXI_CACHE_PROT ||
	    cfg->output_axi.qos != LPF_AXI_QOS || cfg->output_axi.thresh != LPF_AXI_THRESH ||
	    cfg->output_axi.throttle != LPF_AXI_THROTTLE)
		return -EINVAL;

	/* A disabled branch is inert: nothing of it can reach the registers. */
	if (!out1)
		memset(&cfg->ch[1], 0, sizeof(cfg->ch[1]));
	/* Floating regions have no enable of their own, and with no statistics no buffer either. */
	if (!stats)
		memset(&cfg->floating_stats, 0, sizeof(cfg->floating_stats));
	ret = lpf_check_padding(cfg);
	if (ret)
		return ret;
	if (cfg->global.bayer_order > 3 && cfg->global.bayer_order != 128)
		return -EINVAL;
	/* The same for what's disabled inside a branch: inert, so nothing of it is written. */
	for (unsigned int i = 0; i < PISP_FE_NUM_OUTPUTS; i++) {
		if (!(enables & PISP_FE_ENABLE_CROP(i)))
			memset(&cfg->ch[i].crop, 0, sizeof(cfg->ch[i].crop));
		if (!(enables & PISP_FE_ENABLE_DOWNSCALE(i)))
			memset(&cfg->ch[i].downscale, 0, sizeof(cfg->ch[i].downscale));
		if (!(enables & PISP_FE_ENABLE_COMPRESS(i)))
			memset(&cfg->ch[i].compress, 0, sizeof(cfg->ch[i].compress));
	}

	/* Output 0: fixed at START, into the frame's buffer of the stream. */
	ret = lpf_check_output(cfg, 0, &bytes);
	if (ret)
		return ret;
	if (cfg->ch[0].output.format.width != start->output_width ||
	    cfg->ch[0].output.format.height != start->output_height ||
	    (u32)cfg->ch[0].output.format.stride != start->output_stride ||
	    cfg->ch[0].output.format.format != start->output_format ||
	    bytes > start->output_size || bytes > slot->bytes)
		return -EINVAL;

	if (stats) {
		const struct lem_job_range *rng = &job->range[r++];

		ret = lpf_check_stats(cfg);
		if (ret)
			return ret;
		if (rng->len != sizeof(struct pisp_statistics))
			return -EINVAL;
		ret = lem_stream_range_addr(s, rng, &stats_addr);
		if (ret)
			return ret;
		if (stats_addr & (LEM_STREAM_DMA_ALIGN - 1))
			return -EINVAL;
	}
	if (out1) {
		const struct lem_job_range *rng = &job->range[r++];

		ret = lpf_check_output(cfg, 1, &bytes);
		if (ret)
			return ret;
		if (bytes > rng->len)
			return -EINVAL;
		ret = lem_stream_range_addr(s, rng, &out1_addr);
		if (ret)
			return ret;
		if (out1_addr & (LEM_STREAM_DMA_ALIGN - 1))
			return -EINVAL;
	}

	/* Only now does any address reach the copy: these are the module's own. */
	cfg->stats_buffer.addr_lo = lower_32_bits(stats_addr);
	cfg->stats_buffer.addr_hi = upper_32_bits(stats_addr);
	cfg->output_buffer[0].addr_lo = lower_32_bits(slot->addr);
	cfg->output_buffer[0].addr_hi = upper_32_bits(slot->addr);
	cfg->output_buffer[1].addr_lo = lower_32_bits(out1_addr);
	cfg->output_buffer[1].addr_hi = upper_32_bits(out1_addr);
	/* ILINES interrupts 3/4 of the way down each output (pisp_fe_submit_job). */
	cfg->ch[0].output.ilines = max(0x80u, (3u * cfg->ch[0].output.format.height) >> 2);
	cfg->ch[1].output.ilines = max(0x80u, (3u * cfg->ch[1].output.format.height) >> 2);
	return 0;
}

/*
 * Copies one job's configuration from the consumer's dma-buf and checks it.
 * The caller, on 0, may write the copy to the block; on an errno it must
 * cancel the job. The consumer's memory is read once, here.
 */
static int lpf_prepare(struct lem_pisp_fe *fe, const struct lem_stream_slot *slot)
{
	const struct lem_job_range *config = &slot->job.range[0];
	int ret;

	if (slot->job.count < 1)
		return -EINVAL;
	ret = lem_stream_range_read(fe->stream, config, &fe->cfg, sizeof(fe->cfg));
	if (ret)
		return ret;
	if (fe->test_scribble) {
		/* Test only: the consumer's copy goes bad right after the read. */
		fe->test_scribble = false;
		lem_stream_range_poke(fe->stream, config,
				      offsetof(struct pisp_fe_config, ch[0].output.format.stride),
				      0x7ffffff0);
	}
	return lpf_validate(fe, &fe->cfg, slot);
}

/*
 * pisp_fe_config_write and pisp_fe_submit_job: every register from the kernel's
 * copy, then QUEUE, ordered by the non-relaxed write. `fe->cfg` was validated.
 */
static void lpf_program(struct lem_pisp_fe *fe)
{
	const u32 *cfg = (const u32 *)&fe->cfg;

	for (unsigned int off = 0; off < FE_CONFIG_BYTES; off += 4, cfg++)
		lpf_wr_relaxed(fe, FE_CONFIG_BASE + off, *cfg);
	lpf_wr(fe, FE_CONTROL, FE_CONTROL_QUEUE);
}

/*
 * Gives the block the stream's next job when it has none queued. Lock held.
 * As the receiver's: with no buffer free, this tries again every LPF_RETRY_MS
 * until one is or the block stops, since a block with nothing queued may make
 * no interrupt that would.
 */
static void lpf_arm(struct lem_pisp_fe *fe)
{
	int ret;

	if (!fe->streaming || fe->next.valid)
		return;
	/* The previous job must have been consumed; this read also orders what follows. */
	if (lpf_rd(fe, FE_STATUS) & FE_STATUS_QUEUED) {
		fe->stalled++;
		return;
	}
	ret = lem_stream_next(fe->stream, &fe->next.slot);
	if (ret) {
		if (ret == -ENOBUFS)
			queue_delayed_work(system_highpri_wq, &fe->retry,
					   msecs_to_jiffies(LPF_RETRY_MS));
		return;		/* none free, or the stream's own error */
	}
	ret = lpf_prepare(fe, &fe->next.slot);
	if (ret) {
		lem_stream_cancel(fe->stream, &fe->next.slot);
		lem_stream_fault(fe->stream, ret);
		return;
	}
	lpf_program(fe);
	fe->next.valid = true;
}

static void lpf_retry(struct work_struct *work)
{
	struct lem_pisp_fe *fe = container_of(to_delayed_work(work), struct lem_pisp_fe, retry);
	unsigned long flags;

	spin_lock_irqsave(&fe->lock, flags);
	lpf_arm(fe);
	spin_unlock_irqrestore(&fe->lock, flags);
}

/* As the receiver's: a start before the last frame's end is a frame not called complete. */
static void lpf_sof(struct lem_pisp_fe *fe, u64 now)
{
	if (fe->cur.valid) {
		lem_stream_complete(fe->stream, &fe->cur.slot, 0, fe->frames, fe->fs_ns);
		fe->errors++;
		fe->cur.valid = false;
	}
	fe->csi_pending = false;
	fe->cur = fe->next;
	fe->next.valid = false;
	if (fe->cur.valid)
		fe->fs_ns = now;
	else
		fe->dropped++;
	fe->frames++;
	fe->fs_int = true;
	fe->fe_int = false;
	lpf_arm(fe);
}

static void lpf_eof(struct lem_pisp_fe *fe)
{
	if (fe->cur.valid) {
		fe->written++;
		lem_stream_complete(fe->stream, &fe->cur.slot, fe->cur.slot.bytes, fe->frames - 1,
				    fe->fs_ns);
		fe->cur.valid = false;
	}
	fe->fe_int = true;
	fe->fs_int = false;
	lpf_arm(fe);
}

/*
 * pisp_fe_isr, and the frame-event ordering cfe_isr handles. The status is
 * only tested for known bits. Called from the receiver's handler.
 */
void lem_pisp_fe_isr(struct lem_pisp_fe *fe, u64 now)
{
	u32 ints;
	bool sof, eof;

	lpf_wr(fe, FE_CONTROL, FE_CONTROL_LATCH_REGS);
	ints = lpf_rd(fe, FE_INT_STATUS);
	lpf_wr(fe, FE_INT_STATUS, ints);
	sof = ints & FE_INT_SOF;
	eof = ints & FE_INT_EOF;

	spin_lock(&fe->lock);
	if (fe->streaming && (sof || eof)) {
		/* FE then FS of the next frame, FS and FE of one, FS with no FE before it. */
		if (eof) {
			if (sof && !fe->fs_int) {
				lpf_sof(fe, now);
				sof = false;
			}
			lpf_eof(fe);
		}
		if (sof)
			lpf_sof(fe, now);
	}
	spin_unlock(&fe->lock);
}

/*
 * The receiver's frame start on the channel feeding the front end, handled
 * before the front end's own interrupt when both are pending, since the
 * receiver starts a frame before the front end does. A frame the receiver
 * started and the front end didn't by the next one had no job queued.
 */
void lem_pisp_fe_csi_fs(struct lem_pisp_fe *fe)
{
	spin_lock(&fe->lock);
	if (fe->streaming) {
		if (fe->csi_pending)
			fe->dropped++;
		fe->csi_pending = true;
	}
	spin_unlock(&fe->lock);
}

/* --- start and stop --- */

bool lem_pisp_fe_streaming(struct lem_pisp_fe *fe)
{
	return READ_ONCE(fe->stream);
}

/* The block reports idle after ABORT and holds it. Bounded; sleeps; never on the frame path. */
static bool lpf_idle(struct lem_pisp_fe *fe)
{
	unsigned int still = 0;

	for (unsigned int i = 0; i < LPF_STOP_POLLS; i++) {
		usleep_range(200, 400);
		if (!lpf_rd(fe, FE_STATUS)) {
			if (++still >= LPF_STOP_STILL_READS)
				return true;
		} else {
			still = 0;
		}
	}
	return false;
}

/*
 * pisp_fe_stop, then cfe_stop_channel's channel stop: the other way round from
 * cfe_stop_channel. The abort lands while the channel still feeds the block. A
 * block the channel's stop cuts off mid-frame waits for the rest of that frame
 * for good, ABORT or not: STATUS stayed 0x5 (queued and active) on the board
 * after the stock order. A block still busy once the channel is stopped is
 * reset, which pisp_fe_start does to every block before it runs. Only when the
 * block reports idle does anything go back; otherwise every pin stays, for
 * good. l->ctl held.
 */
void lem_pisp_fe_stop(struct lem_pisp_fe *fe)
{
	struct lrc_dev *l = fe->l;
	struct lem_stream *s = fe->stream;
	unsigned long flags;
	bool idle;

	if (!s)
		return;
	spin_lock_irqsave(&fe->lock, flags);
	fe->streaming = false;
	spin_unlock_irqrestore(&fe->lock, flags);
	/* Not streaming, so a retry finds nothing to do and doesn't come again. */
	cancel_delayed_work_sync(&fe->retry);

	lpf_wr(fe, FE_INT_EN, 0);
	lpf_wr(fe, FE_CONTROL, FE_CONTROL_ABORT);
	/* Those writes are posted across a bus: a read lands them first. */
	(void)lpf_rd(fe, FE_STATUS);
	/* pisp_fe_stop waits this long before it expects idle; so does this. */
	usleep_range(1000, 2000);
	idle = lpf_idle(fe);

	lpf_trace(fe, LEMNOS_PISP_FE_MARK_CHANNEL_STOP, 0);
	lrc_fe_channel_stop(l);
	synchronize_irq(l->irq);
	if (!idle) {
		u32 status = lpf_rd(fe, FE_STATUS);

		lpf_wr(fe, FE_CONTROL, FE_CONTROL_RESET);
		(void)lpf_rd(fe, FE_STATUS);
		idle = lpf_idle(fe);
		dev_warn_ratelimited(l->dev, "PiSP front end busy after abort (STATUS %#x): reset, %s\n",
				     status, idle ? "idle" : "still busy");
	}
	if (idle) {
		lpf_wr(fe, FE_INT_STATUS, ~0u);
		if (fe->cur.valid)
			lem_stream_cancel(s, &fe->cur.slot);
		if (fe->next.valid)
			lem_stream_cancel(s, &fe->next.slot);
		fe->cur.valid = fe->next.valid = false;
	} else {
		dev_err(l->dev, "PiSP front end did not stop; buffers stay pinned until reboot\n");
		l->wedged = true;
		lem_stream_wedge(s);
	}

	spin_lock_irqsave(&fe->lock, flags);
	fe->stream = NULL;
	spin_unlock_irqrestore(&fe->lock, flags);
	if (lem_stream_detach(s))
		lrc_ref_put(l);
}

/* The stream is going: its stop() contract. Takes l->ctl under the stream's s->ctl. */
static void lpf_stream_stop(void *priv, struct lem_stream *s)
{
	struct lem_pisp_fe *fe = priv;
	struct lrc_dev *l = fe->l;

	mutex_lock(&l->ctl);
	if (fe->stream == s)
		lem_pisp_fe_stop(fe);
	mutex_unlock(&l->ctl);
	lrc_ref_put(l);
}

static int lpf_check_start(const struct lrc_dev *l, const struct lemnos_pisp_fe_start *a)
{
	if (a->lanes < 1 || a->lanes > l->max_lanes || a->mbps < 80 || a->mbps > 1500)
		return -EINVAL;
	if (a->vc > 3 || a->dt < 1 || a->dt > 63)
		return -EINVAL;
	if (a->width < 2 || a->height < 2 || a->width > 0xffff || a->height > 0xffff)
		return -EINVAL;
	if (a->output_width < 2 || a->output_height < 2 || a->output_width > 0xffff ||
	    a->output_height > 0xffff)
		return -EINVAL;
	if (!a->output_stride || (a->output_stride & 15) || !a->output_size ||
	    (u64)a->output_stride * a->output_height > a->output_size)
		return -EINVAL;
	if (a->flags || a->reserved[0] || a->reserved[1])
		return -EINVAL;
	return lpf_check_format(a->output_format);
}

/*
 * The first job is taken and checked before anything is written: a bad first
 * configuration, or a stream with none posted, gets its errno here. Then the
 * order is cfe.c's: channel, front end, first job, and the link last.
 */
static long lpf_start_locked(struct lem_pisp_fe *fe, const struct lemnos_pisp_fe_start *a,
			     struct lem_stream *stream)
{
	const struct lem_stream_producer producer = { .stop = lpf_stream_stop, .priv = fe };
	struct lrc_dev *l = fe->l;
	unsigned long flags;
	int ret;

	if (l->wedged)
		return -EIO;
	if (l->stream || fe->stream)
		return -EBUSY;

	lrc_ref_get(l);
	ret = lem_stream_attach(stream, &producer, a->output_stride, a->output_height,
				a->output_size);
	if (ret) {
		lrc_ref_put(l);
		return ret;
	}

	spin_lock_irqsave(&fe->lock, flags);
	fe->start = *a;
	fe->stream = stream;
	fe->cur.valid = fe->next.valid = false;
	fe->frames = fe->written = fe->dropped = fe->errors = fe->stalled = 0;
	fe->fs_int = fe->fe_int = false;
	fe->csi_pending = false;
	ret = lem_stream_next(stream, &fe->next.slot);
	if (!ret) {
		ret = lpf_prepare(fe, &fe->next.slot);
		if (ret)
			lem_stream_cancel(stream, &fe->next.slot);
	}
	if (ret) {
		fe->stream = NULL;
		spin_unlock_irqrestore(&fe->lock, flags);
		lem_stream_fault(stream, ret == -ENOBUFS ? -EINVAL : ret);
		if (lem_stream_detach(stream))
			lrc_ref_put(l);
		return ret == -ENOBUFS ? -EINVAL : ret;
	}
	fe->next.valid = true;
	spin_unlock_irqrestore(&fe->lock, flags);

	/* Nothing has touched the block before this line. */
	lpf_trace(fe, LEMNOS_PISP_FE_MARK_CHANNEL_START, 0);
	lrc_fe_channel_start(l, a->width, a->height, a->vc, a->dt);

	/* pisp_fe_start. */
	lpf_wr(fe, FE_CONTROL, FE_CONTROL_RESET);
	lpf_wr(fe, FE_INT_STATUS, ~0u);
	lpf_wr(fe, FE_INT_EN, FE_INT_EOF | FE_INT_SOF | FE_INT_LINES0 | FE_INT_LINES1);

	/* The first job, already checked; no interrupt can run yet. */
	spin_lock_irqsave(&fe->lock, flags);
	fe->streaming = true;
	lpf_program(fe);
	spin_unlock_irqrestore(&fe->lock, flags);

	lpf_trace(fe, LEMNOS_PISP_FE_MARK_OPEN_RX, 0);
	lrc_fe_open_rx(l, a->lanes, a->mbps);
	return 0;
}

static long lpf_start(struct lem_pisp_fe *fe, const struct lemnos_pisp_fe_start *a)
{
	struct lrc_dev *l = fe->l;
	struct lem_stream *stream;
	struct file *file;
	long ret;

	ret = lpf_check_start(l, a);
	if (ret)
		return ret;
	file = fget(a->stream_fd);
	if (!file)
		return -EBADF;
	stream = lem_stream_from_file(file, l->dev);
	if (IS_ERR(stream)) {
		ret = PTR_ERR(stream);
		goto put_file;
	}
	/* s->ctl: before l->ctl, the order the stream's stop callback already keeps. */
	ret = lem_stream_cpu_read_enable(stream);
	if (ret)
		goto put_file;
	mutex_lock(&l->ctl);
	ret = l->gone ? -ENODEV : lpf_start_locked(fe, a, stream);
	mutex_unlock(&l->ctl);
put_file:
	fput(file);
	return ret;
}

static long lpf_info(struct lem_pisp_fe *fe, void __user *arg)
{
	struct lemnos_pisp_fe_info info = {
		.abi = LEMNOS_PISP_FE_ABI,
		.hw_version = fe->hw_version,
	};
	struct lrc_dev *l = fe->l;
	unsigned long flags;

	mutex_lock(&l->ctl);
	info.flags = l->wedged ? LEMNOS_PISP_FE_INFO_WEDGED : 0;
	spin_lock_irqsave(&fe->lock, flags);
	info.streaming = fe->streaming;
	info.frames = fe->frames;
	info.written = fe->written;
	info.dropped = fe->dropped;
	info.errors = fe->errors;
	info.stalled = fe->stalled;
	spin_unlock_irqrestore(&fe->lock, flags);
	mutex_unlock(&l->ctl);
	return copy_to_user(arg, &info, sizeof(info)) ? -EFAULT : 0;
}

static long lpf_test_trace(struct lem_pisp_fe *fe, void __user *arg)
{
	struct lemnos_pisp_fe_test_trace t;
	unsigned long flags;
	u32 count;
	void *copy;

	if (!fe->trace)
		return -EPERM;
	if (copy_from_user(&t, arg, sizeof(t)))
		return -EFAULT;
	copy = kmalloc_array(LPF_TRACE_ENTRIES, sizeof(*fe->trace), GFP_KERNEL);
	if (!copy)
		return -ENOMEM;
	spin_lock_irqsave(&fe->lock, flags);
	count = min(fe->trace_count, t.max);
	memcpy(copy, fe->trace, count * sizeof(*fe->trace));
	fe->trace_count = 0;
	spin_unlock_irqrestore(&fe->lock, flags);
	t.count = count;
	if (copy_to_user(u64_to_user_ptr(t.entries), copy, count * sizeof(*fe->trace)) ||
	    copy_to_user(arg, &t, sizeof(t))) {
		kfree(copy);
		return -EFAULT;
	}
	kfree(copy);
	return 0;
}

long lem_pisp_fe_ioctl(struct lem_pisp_fe *fe, unsigned int cmd, void __user *arg)
{
	struct lrc_dev *l = fe->l;
	long ret;

	switch (cmd) {
	case LEMNOS_PISP_FE_IOC_INFO:
		return lpf_info(fe, arg);
	case LEMNOS_PISP_FE_IOC_START: {
		struct lemnos_pisp_fe_start a;

		if (copy_from_user(&a, arg, sizeof(a)))
			return -EFAULT;
		return lpf_start(fe, &a);
	}
	case LEMNOS_PISP_FE_IOC_STOP:
		ret = 0;
		mutex_lock(&l->ctl);
		if (l->gone)
			ret = -ENODEV;
		else
			lem_pisp_fe_stop(fe);
		mutex_unlock(&l->ctl);
		return ret;
	case LEMNOS_PISP_FE_IOC_TEST_TRACE:
		return lpf_test_trace(fe, arg);
	case LEMNOS_PISP_FE_IOC_TEST_SCRIBBLE:
		if (!test_pisp_fe)
			return -EPERM;
		WRITE_ONCE(fe->test_scribble, true);
		return 0;
	default:
		return -ENOTTY;
	}
}

struct lem_pisp_fe *lem_pisp_fe_create(struct lrc_dev *l, void __iomem *window)
{
	struct lem_pisp_fe *fe;

	BUILD_BUG_ON(FE_CONFIG_BYTES % 4);
	BUILD_BUG_ON(FE_CONFIG_BASE + FE_CONFIG_BYTES != LEM_PISP_FE_WINDOW);
	fe = kzalloc(sizeof(*fe), GFP_KERNEL);
	if (!fe)
		return ERR_PTR(-ENOMEM);
	fe->l = l;
	fe->base = window;
	spin_lock_init(&fe->lock);
	INIT_DELAYED_WORK(&fe->retry, lpf_retry);
	if (test_pisp_fe) {
		fe->trace = kcalloc(LPF_TRACE_ENTRIES, sizeof(*fe->trace), GFP_KERNEL);
		if (!fe->trace) {
			kfree(fe);
			return ERR_PTR(-ENOMEM);
		}
		dev_warn(l->dev, "test_pisp_fe: register log and scribble enabled\n");
	}
	fe->hw_version = lpf_rd(fe, FE_VERSION);
	dev_info(l->dev, "PiSP FE HW v%u.%u\n", (fe->hw_version >> 24) & 0xff,
		 (fe->hw_version >> 20) & 0x0f);
	return fe;
}

void lem_pisp_fe_destroy(struct lem_pisp_fe *fe)
{
	cancel_delayed_work_sync(&fe->retry);
	kfree(fe->trace);
	kfree(fe);
}
