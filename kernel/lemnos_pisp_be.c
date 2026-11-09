// SPDX-License-Identifier: GPL-2.0
/*
 * The PiSP back end, memory to memory, as a frame stream of lemnos.ko. Jobs are
 * posted on the stream's ring (lemnos_pisp_be.h).
 *
 * The register sequence, the interrupt's job accounting and the probe check are
 * ported from the stock driver, drivers/media/platform/raspberrypi/pisp_be/
 * pisp_be.c, by way of the lemnos_pisp_be module on the dev branch:
 *   Copyright (c) 2021-2024 Raspberry Pi Limited.
 * Everything else, the checks and the stream integration in particular:
 *   Copyright (c) 2026 the Lemnos authors.
 *
 * What runs on the block is decided by the kernel's copy of a job. The config
 * slot and the job word are read once from memory the consumer can write,
 * into memory it cannot, and that copy is what is checked and what the block
 * reads: there is no second read of anything the consumer holds. Every
 * address, length and index is proved against what the kernel holds, using
 * the bytes the block will touch, or the job is refused. Nothing is clamped.
 *
 * The addresses the config can name, and how each is proved, are listed in
 * lpb_validate. All are checked with u64 arithmetic that cannot wrap, because
 * every term is a product of u32 and u16 values no larger than the stream's
 * limits; lem_stream_job_take does its own overflow check.
 *
 * DMA and buffer lifetime. A job holds its regions from acceptance until the
 * interrupt that finishes it, or until it is dropped having never started.
 * Closing the stream or unbinding the block waits for jobs on the block. A
 * job that does not finish in time ends the stream; if the block cannot be
 * shown idle after that, it is wedged: the job's regions are never released
 * and the stream's memory is never freed, for the life of the system,
 * because the block may still be using it.
 *
 * The stream side is the producer interface of lemnos_stream.c: a job is taken
 * with lem_stream_job_take(), which resolves its addresses and holds what they
 * name in one step, and ends with lem_stream_complete() or lem_stream_cancel().
 *
 * Locking: s->ctl (held by the stream across lpb_stop), ps->collect_lock (one
 * collector, and lpb_stop waits it out: after that `l` is not touched from a
 * job collection), l->lock (the block's job state, and the registers while a
 * job is written), s->lock in the stream code. Never s->lock then l->lock. The
 * ISR takes l->lock.
 */
#include <linux/clk.h>
#include <linux/dma-mapping.h>
#include <linux/err.h>
#include <linux/interrupt.h>
#include <linux/io.h>
#include <linux/ktime.h>
#include <linux/mm.h>
#include <linux/module.h>
#include <linux/mutex.h>
#include <linux/overflow.h>
#include <linux/platform_device.h>
#include <linux/slab.h>
#include <linux/spinlock.h>
#include <linux/uaccess.h>
#include <linux/vmalloc.h>
#include <linux/wait.h>
#include <linux/workqueue.h>

#include <uapi/linux/media/raspberrypi/pisp_be_config.h>

#include "lemnos_internal.h"
#include "lemnos_pisp_be.h"

/* Registers (pisp_be.c). */
#define BE_VERSION_REG		0x0
#define BE_CONTROL_REG		0x4
#define BE_CONTROL_COPY_CONFIG	BIT(1)
#define BE_CONTROL_QUEUE_JOB	BIT(0)
#define BE_CONTROL_NUM_TILES(n)	((n) << 16)
#define BE_TILE_ADDR_LO_REG	0x8
#define BE_TILE_ADDR_HI_REG	0xc
#define BE_STATUS_REG		0x10
#define BE_STATUS_QUEUED	BIT(0)
#define BE_BATCH_STATUS_REG	0x14
#define BE_INTERRUPT_EN_REG	0x18
#define BE_INTERRUPT_STATUS_REG	0x1c
#define BE_AXI_REG		0x20
#define BE_AXI_VALUE		0x32703200u
#define BE_CONFIG_BASE_REG	0x40
#define BE_IO_ADDR_LOW(n)	(BE_CONFIG_BASE_REG + 8 * (n))
#define BE_IO_ADDR_HIGH(n)	(BE_IO_ADDR_LOW((n)) + 4)
#define BE_GLOBAL_BAYER_ENABLE	0xb0
#define BE_GLOBAL_RGB_ENABLE	0xb4
#define BE_VERSION_BLOCK	0x02252700u
#define BE_VERSION_MINOR_BITS	0xfu

/* The window every register access stays inside; checked at probe. */
#define LPB_WINDOW (BE_CONFIG_BASE_REG + sizeof(struct pisp_be_config))
#define LPB_CONFIG_BYTES (sizeof(struct pisp_be_tiles_config))
#define LPB_TILES_BYTES (sizeof_field(struct pisp_be_tiles_config, tiles))

/* mmap offset of the config slots (pages). The ring is at 0. */
#define LPB_CONFIG_PGOFF 0x1000u

/*
 * The bytes of a config before the first register the module writes, and the
 * bytes after the last. Older uAPI layouts called them pad0 and pad1, or named
 * the addresses and non-register fields that lived there: refused non-zero
 * under any name.
 */
#define LPB_HEAD_BYTES offsetof(struct pisp_be_config, global)
#define LPB_TAIL_OFFSET (offsetof(struct pisp_be_config, axi) + \
			 sizeof_field(struct pisp_be_config, axi))

/* The block's two job slots: one running, one queued. */
#define LPB_ADDRS LEMNOS_PISP_BE_ADDRS
/* A job this long is the block having stopped (the stock driver's experience: it is). */
#define LPB_TIMEOUT_MS 1000

#define LPB_KNOWN_BAYER 0x7fffffu
#define LPB_KNOWN_RGB \
	(PISP_BE_RGB_ENABLE_INPUT | PISP_BE_RGB_ENABLE_CCM | PISP_BE_RGB_ENABLE_SAT_CONTROL | \
	 PISP_BE_RGB_ENABLE_YCBCR | PISP_BE_RGB_ENABLE_FALSE_COLOUR | PISP_BE_RGB_ENABLE_SHARPEN | \
	 PISP_BE_RGB_ENABLE_YCBCR_INVERSE | PISP_BE_RGB_ENABLE_GAMMA | PISP_BE_RGB_ENABLE_CSC0 | \
	 PISP_BE_RGB_ENABLE_CSC1 | PISP_BE_RGB_ENABLE_DOWNSCALE0 | PISP_BE_RGB_ENABLE_DOWNSCALE1 | \
	 PISP_BE_RGB_ENABLE_RESAMPLE0 | PISP_BE_RGB_ENABLE_RESAMPLE1 | PISP_BE_RGB_ENABLE_OUTPUT0 | \
	 PISP_BE_RGB_ENABLE_OUTPUT1 | PISP_BE_RGB_ENABLE_HOG)

/* Every format bit that has a meaning (X_VALUE, 0x00200000, has no name in the kernel headers). */
#define LPB_KNOWN_FORMAT \
	(PISP_IMAGE_FORMAT_BPS_MASK | PISP_IMAGE_FORMAT_PLANARITY_MASK | \
	 PISP_IMAGE_FORMAT_SAMPLING_MASK | PISP_IMAGE_FORMAT_ORDER_SWAPPED | \
	 PISP_IMAGE_FORMAT_SHIFT_MASK | PISP_IMAGE_FORMAT_BPP_32 | 0x00200000u | \
	 PISP_IMAGE_FORMAT_COMPRESSION_MASK | PISP_IMAGE_FORMAT_HOG_SIGNED | \
	 PISP_IMAGE_FORMAT_HOG_UNSIGNED | PISP_IMAGE_FORMAT_INTEGRAL_IMAGE | \
	 PISP_IMAGE_FORMAT_WALLPAPER_ROLL | PISP_IMAGE_FORMAT_THREE_CHANNEL)

/* Fakes the register window with ordinary memory (the test harness has no real block). */
static bool test_pb_fake_regs;
module_param(test_pb_fake_regs, bool, 0444);
MODULE_PARM_DESC(test_pb_fake_regs,
		 "Test only: the block's register window is ordinary memory (the test harness)");

struct lpb_job {
	struct list_head node;
	struct pisp_be_tiles_config *cfg;	/* the checked copy; kernel only */
	struct pisp_tile *tiles;		/* coherent: what the block reads */
	dma_addr_t tiles_dma;
	u64 addr[LPB_ADDRS];
	u32 bayer_en, rgb_en, num_tiles;
	/* What the job holds, from lem_stream_job_take() to its end. */
	struct lem_stream_slot slot;
};

struct lem_pb;

struct lpb_stream {
	struct lem_pb *l;
	struct lem_stream *s;
	struct device *dev;		/* for dma_free_coherent; held by the stream */
	void *configs;			/* vmalloc_user: the consumer writes these */
	size_t configs_bytes;
	u32 config_slots;
	u32 njobs;
	struct lpb_job *jobs;

	struct mutex collect_lock;	/* one collector; `stopped` */
	bool stopped;			/* no collection touches `l` once this is set */

	/* All under l->lock. */
	struct list_head free, ready;
	struct lpb_job *queued, *running;
	bool stopping;
	bool wedged;			/* lpb_release keeps everything */
	u64 sequence;

	wait_queue_head_t idle;
	struct delayed_work collect;
};

struct lem_pb {
	struct device *dev;
	void __iomem *regs;
	struct clk *clk;
	struct workqueue_struct *wq;
	struct mutex create_lock;	/* one stream at a time */
	u32 hw_version;
	int irq;

	spinlock_t lock;		/* everything below */
	struct lpb_stream *attached;
	u8 done, started;
	bool busy;			/* a job is queued and not yet seen to start */
	bool wedged;			/* see the file header */
	bool module_held;
	u64 finished, refused;
	struct delayed_work watchdog;
};

static u32 lpb_rd(struct lem_pb *l, unsigned int off)
{
	return readl(l->regs + off);
}

static void lpb_wr(struct lem_pb *l, unsigned int off, u32 val)
{
	writel(val, l->regs + off);
}

/* The block can't be given up safely: from here on nothing it touched goes away. l->lock held. */
static void lpb_wedge(struct lem_pb *l, struct lpb_stream *ps, int err, u32 status, u32 batch)
{
	struct lpb_job *job, *tmp;

	if (!l->wedged)
		dev_crit(l->dev,
			 "block stopped (status 0x%08x, batch 0x%08x): its buffers stay held until a reboot\n",
			 status, batch);
	l->wedged = true;
	if (!l->module_held) {
		l->module_held = true;
		__module_get(THIS_MODULE);
	}
	/* Jobs not yet on the block can go; the ones on it can't. */
	list_for_each_entry_safe(job, tmp, &ps->ready, node) {
		list_move(&job->node, &ps->free);
		lem_stream_cancel(ps->s, &job->slot);
	}
	ps->wedged = true;
	lem_stream_fault(ps->s, err);
	wake_up_all(&ps->idle);
}

/* --- writing a job to the block --- */

/* Writes a job (pispbe_queue_job). 0, or -errno when the block isn't holding what was written. */
static int lpb_queue_job(struct lem_pb *l, struct lpb_job *job)
{
	unsigned int begin, end, u;

	for (u = 0; u < LPB_ADDRS; u++) {
		lpb_wr(l, BE_IO_ADDR_LOW(u), lower_32_bits(job->addr[u]));
		lpb_wr(l, BE_IO_ADDR_HIGH(u), upper_32_bits(job->addr[u]));
	}
	lpb_wr(l, BE_GLOBAL_BAYER_ENABLE, job->bayer_en);
	lpb_wr(l, BE_GLOBAL_RGB_ENABLE, job->rgb_en);

	/*
	 * Everything else, from the copy: reserved and non-register bytes were
	 * refused non-zero in lpb_validate, so what is written here is what was
	 * proved.
	 */
	begin = offsetof(struct pisp_be_config, global.bayer_order) / sizeof(u32);
	end = sizeof(struct pisp_be_config) / sizeof(u32);
	for (u = begin; u < end; u++)
		lpb_wr(l, BE_CONFIG_BASE_REG + sizeof(u32) * u,
		       ((u32 *)&job->cfg->config)[u]);

	/* Read back the addresses before letting the block run them (stock does too). */
	for (u = 0; u < LPB_ADDRS; u++) {
		u64 got = lpb_rd(l, BE_IO_ADDR_LOW(u));

		got |= (u64)lpb_rd(l, BE_IO_ADDR_HIGH(u)) << 32;
		if (got != job->addr[u])
			return -EIO;
	}

	lpb_wr(l, BE_TILE_ADDR_LO_REG, lower_32_bits(job->tiles_dma));
	lpb_wr(l, BE_TILE_ADDR_HI_REG, upper_32_bits(job->tiles_dma));
	lpb_wr(l, BE_CONTROL_REG,
	       BE_CONTROL_COPY_CONFIG | BE_CONTROL_QUEUE_JOB | BE_CONTROL_NUM_TILES(job->num_tiles));
	return 0;
}

/* Starts the next ready job if the block has room. l->lock held. */
static void lpb_schedule(struct lem_pb *l, struct lpb_stream *ps)
{
	struct lpb_job *job;

	if (l->busy || l->wedged || ps->stopping)
		return;
	job = list_first_entry_or_null(&ps->ready, struct lpb_job, node);
	if (!job)
		return;
	/*
	 * The block saying it still holds a queued job is one we didn't
	 * count: writing now would replace addresses under it. Wait; the
	 * watchdog ends the stream if this doesn't clear.
	 */
	if (lpb_rd(l, BE_STATUS_REG) & BE_STATUS_QUEUED) {
		mod_delayed_work(l->wq, &l->watchdog, msecs_to_jiffies(LPB_TIMEOUT_MS));
		return;
	}
	list_del(&job->node);
	if (lpb_queue_job(l, job)) {
		/* Never reached the block: nothing started, so this can be dropped. */
		list_add(&job->node, &ps->free);
		lem_stream_cancel(ps->s, &job->slot);
		lem_stream_fault(ps->s, -EIO);
		return;
	}
	ps->queued = job;
	l->busy = true;
	mod_delayed_work(l->wq, &l->watchdog, msecs_to_jiffies(LPB_TIMEOUT_MS));
}

/* A job the block has finished: its frame, and what it held. l->lock held. */
static void lpb_finish(struct lem_pb *l, struct lpb_stream *ps, struct lpb_job *job)
{
	lem_stream_complete(ps->s, &job->slot, job->slot.bytes, ps->sequence++, ktime_get_ns());
	list_add(&job->node, &ps->free);
	l->finished++;
}

/*
 * The job accounting of pispbe_isr. The hardware's counters are only trusted as
 * far as the jobs we put on it: a counter that moved more than our jobs can
 * explain is a block we no longer understand, and it is wedged rather than
 * guessed at, with its jobs still held. A job the counters can't account for
 * stays in place. l->lock held.
 */
static void lpb_process(struct lem_pb *l, struct lpb_stream *ps, u32 batch)
{
	u8 done_delta = (u8)(batch - l->done);
	u8 started_delta = (u8)((u8)(batch >> 8) - l->started);
	unsigned int running = ps->running ? 1 : 0, queued = ps->queued ? 1 : 0;

	if (started_delta > queued || done_delta > running + started_delta ||
	    (started_delta && ps->running && !done_delta)) {
		lpb_wedge(l, ps, -EIO, lpb_rd(l, BE_STATUS_REG), batch);
		return;
	}
	if (done_delta && ps->running) {
		lpb_finish(l, ps, ps->running);
		ps->running = NULL;
		l->done++;
		done_delta--;
	}
	if (started_delta) {
		struct lpb_job *job = ps->queued;

		l->started++;
		ps->queued = NULL;
		l->busy = false;
		if (done_delta) {
			lpb_finish(l, ps, job);
			l->done++;
			done_delta--;
		} else {
			ps->running = job;
		}
	}
	if (ps->stopping && !ps->queued && !ps->running)
		wake_up_all(&ps->idle);
	if (!ps->stopping && !l->wedged) {
		lpb_schedule(l, ps);
		if (!list_empty(&ps->free))
			queue_delayed_work(l->wq, &ps->collect, 0);
	}
	if (ps->queued || ps->running || !list_empty(&ps->ready))
		mod_delayed_work(l->wq, &l->watchdog, msecs_to_jiffies(LPB_TIMEOUT_MS));
	else
		cancel_delayed_work(&l->watchdog);
}

static irqreturn_t lpb_isr(int irq, void *data)
{
	struct lem_pb *l = data;
	struct lpb_stream *ps;
	u32 status, batch;

	status = lpb_rd(l, BE_INTERRUPT_STATUS_REG);
	if (!status)
		return IRQ_NONE;
	lpb_wr(l, BE_INTERRUPT_STATUS_REG, status);
	batch = lpb_rd(l, BE_BATCH_STATUS_REG);

	spin_lock(&l->lock);
	ps = l->attached;
	/* A wedged block's jobs are never finished, whatever it says later. */
	if (ps && !l->wedged)
		lpb_process(l, ps, batch);
	spin_unlock(&l->lock);
	return IRQ_HANDLED;
}

/* A job outstanding too long: the block stopped. l->lock held. */
static void lpb_watchdog_locked(struct lem_pb *l, struct lpb_stream *ps)
{
	u32 batch = lpb_rd(l, BE_BATCH_STATUS_REG);
	u32 status = lpb_rd(l, BE_STATUS_REG);

	if (!status && (u8)batch == (u8)(batch >> 8)) {
		u64 finished = l->finished;

		/* Nothing on the block: a job was waiting for it to say it had room. */
		if (!ps->queued && !ps->running) {
			lpb_schedule(l, ps);
			return;
		}
		/*
		 * Counters agree the block is idle and nothing is queued: the
		 * jobs are done and the interrupt was lost. Account them. If
		 * none finished, the counters and the jobs disagree and the
		 * block is wedged; whatever came after them has its own
		 * timeout, set by lpb_process.
		 */
		lpb_process(l, ps, batch);
		if (l->finished != finished || l->wedged)
			return;
	}
	lpb_wedge(l, ps, -ETIMEDOUT, status, batch);
}

static void lpb_watchdog(struct work_struct *work)
{
	struct lem_pb *l = container_of(to_delayed_work(work), struct lem_pb, watchdog);
	struct lpb_stream *ps;
	unsigned long flags;

	spin_lock_irqsave(&l->lock, flags);
	ps = l->attached;
	if (ps && (ps->queued || ps->running || !list_empty(&ps->ready)) && !l->wedged)
		lpb_watchdog_locked(l, ps);
	spin_unlock_irqrestore(&l->lock, flags);
}

/* --- the checks --- */

/* Bytes a row of `w` pixels of an interleaved plane (or plane 0) takes, rounded up. */
static u64 lpb_row_bytes(u32 format, u32 w)
{
	u64 b;

	if (format & PISP_IMAGE_FORMAT_BPP_32)
		return (u64)w * 4;
	switch (format & PISP_IMAGE_FORMAT_BPS_MASK) {
	case PISP_IMAGE_FORMAT_BPS_16:
		b = (u64)w * 2;
		break;
	case PISP_IMAGE_FORMAT_BPS_12:
		b = DIV_ROUND_UP_ULL((u64)w * 3, 2);
		break;
	case PISP_IMAGE_FORMAT_BPS_10:
		b = DIV_ROUND_UP_ULL((u64)w, 3) * 4;
		break;
	default:
		b = w;
		break;
	}
	if ((format & PISP_IMAGE_FORMAT_THREE_CHANNEL) &&
	    (format & PISP_IMAGE_FORMAT_PLANARITY_MASK) == PISP_IMAGE_FORMAT_PLANARITY_INTERLEAVED)
		b *= (format & PISP_IMAGE_FORMAT_SAMPLING_MASK) == PISP_IMAGE_FORMAT_SAMPLING_422 ? 2 : 3;
	return b;
}

/* Bytes of a row of `w` pixels in `plane` of `format`, as much as the block touches. */
static u64 lpb_plane_row(u32 format, unsigned int plane, u32 w)
{
	u32 planarity = format & PISP_IMAGE_FORMAT_PLANARITY_MASK;
	u32 sampling = format & PISP_IMAGE_FORMAT_SAMPLING_MASK;
	u64 pixels = w;

	if (plane && planarity == PISP_IMAGE_FORMAT_PLANARITY_PLANAR &&
	    sampling != PISP_IMAGE_FORMAT_SAMPLING_444)
		pixels = DIV_ROUND_UP_ULL((u64)w, 2);
	/* Semi-planar chroma: a pair per two pixels, so a row is as long as luma. */
	if (format & PISP_IMAGE_FORMAT_COMPRESSION_MASK)
		pixels = ALIGN(pixels, 8);	/* fixed-rate blocks of 8 pixels, 1 byte each */
	return lpb_row_bytes(format, pixels);
}

static u32 lpb_plane_lines(u32 format, unsigned int plane, u32 h)
{
	if (plane && (format & PISP_IMAGE_FORMAT_SAMPLING_MASK) == PISP_IMAGE_FORMAT_SAMPLING_420)
		return DIV_ROUND_UP(h, 2);
	return h;
}

static unsigned int lpb_nplanes(u32 format)
{
	if (!(format & PISP_IMAGE_FORMAT_THREE_CHANNEL))
		return 1;
	switch (format & PISP_IMAGE_FORMAT_PLANARITY_MASK) {
	case PISP_IMAGE_FORMAT_PLANARITY_SEMI_PLANAR:
		return 2;
	case PISP_IMAGE_FORMAT_PLANARITY_PLANAR:
		return 3;
	default:
		return 1;
	}
}

/*
 * Refuses a format the module can't prove what it touches. `align` is the
 * stride alignment the block needs for this kind of plane.
 */
static int lpb_check_format(const struct pisp_image_format_config *f, u32 align)
{
	u32 fmt = f->format;
	u32 planarity = fmt & PISP_IMAGE_FORMAT_PLANARITY_MASK;
	u32 sampling = fmt & PISP_IMAGE_FORMAT_SAMPLING_MASK;

	if (!f->width || !f->height)
		return -EINVAL;
	if (fmt & ~LPB_KNOWN_FORMAT)
		return -EINVAL;
	/* Nothing the module can size: refused, whatever else the config says. */
	if (fmt & (PISP_IMAGE_FORMAT_WALLPAPER_ROLL | PISP_IMAGE_FORMAT_HOG_SIGNED |
		   PISP_IMAGE_FORMAT_HOG_UNSIGNED | PISP_IMAGE_FORMAT_INTEGRAL_IMAGE))
		return -EOPNOTSUPP;
	if (planarity == PISP_IMAGE_FORMAT_PLANARITY_MASK || sampling == PISP_IMAGE_FORMAT_SAMPLING_MASK)
		return -EINVAL;
	if (planarity != PISP_IMAGE_FORMAT_PLANARITY_INTERLEAVED &&
	    !(fmt & PISP_IMAGE_FORMAT_THREE_CHANNEL))
		return -EINVAL;
	if (planarity == PISP_IMAGE_FORMAT_PLANARITY_SEMI_PLANAR &&
	    sampling == PISP_IMAGE_FORMAT_SAMPLING_444)
		return -EOPNOTSUPP;
	if (planarity == PISP_IMAGE_FORMAT_PLANARITY_INTERLEAVED &&
	    sampling == PISP_IMAGE_FORMAT_SAMPLING_420)
		return -EOPNOTSUPP;
	if (fmt & PISP_IMAGE_FORMAT_COMPRESSION_MASK) {
		if ((fmt & PISP_IMAGE_FORMAT_THREE_CHANNEL) ||
		    (fmt & PISP_IMAGE_FORMAT_BPS_MASK) != PISP_IMAGE_FORMAT_BPS_8)
			return -EOPNOTSUPP;
		if (align < PISP_BACK_END_COMPRESSED_ALIGN)
			align = PISP_BACK_END_COMPRESSED_ALIGN;
	}

	/* Strides: positive (a negative stride reads backwards from the image's end). */
	if (f->stride < 0)
		return -EOPNOTSUPP;
	if (!f->stride || (u32)f->stride % align)
		return -EINVAL;
	if ((u32)f->stride < lpb_plane_row(fmt, 0, f->width))
		return -EINVAL;
	if (lpb_nplanes(fmt) > 1) {
		if (f->stride2 < 0)
			return -EOPNOTSUPP;
		if (!f->stride2 || (u32)f->stride2 % align)
			return -EINVAL;
		if ((u32)f->stride2 < lpb_plane_row(fmt, 1, f->width))
			return -EINVAL;
	} else if (f->stride2) {
		return -EINVAL;		/* a stride for a plane the format doesn't have */
	}
	return 0;
}

/* What one of the hardware's address slots covers for this job. */
struct lpb_plane {
	const struct pisp_image_format_config *fmt;	/* NULL: the slot is unused */
	unsigned int plane;				/* within that image */
	u32 stride;
	u64 bytes;
};

static void lpb_set_plane(struct lpb_plane *p, const struct pisp_image_format_config *f,
			  unsigned int plane)
{
	u32 stride = plane ? f->stride2 : f->stride;

	p->fmt = f;
	p->plane = plane;
	p->stride = stride;
	p->bytes = (u64)stride * lpb_plane_lines(f->format, plane, f->height);
}

/*
 * The block reads or writes `lines` rows of `w` pixels starting `off` bytes
 * into the plane, at the plane's stride. Those bytes, all of them, must lie
 * inside the plane's extent, which is inside a held region. Overflow can't
 * happen: every term is below 2^48.
 */
static int lpb_tile_fits(const struct lpb_plane *p, u32 off, u32 w, u32 h)
{
	u32 lines = lpb_plane_lines(p->fmt->format, p->plane, h);
	u64 row = lpb_plane_row(p->fmt->format, p->plane, w);
	u64 end;

	if (!lines || row > p->stride)
		return -EFAULT;
	end = (u64)off + (u64)(lines - 1) * p->stride + row;
	return end > p->bytes ? -EFAULT : 0;
}

/*
 * The enables and the jobs that must follow them, refused rather than sanitised
 * (the stock driver clears bits here to avoid a lockup; a config that
 * contradicts itself is the caller's bug and we say so instead). Uses and
 * returns the enables of the kernel copy.
 */
static int lpb_check_enables(const struct pisp_be_config *c, u32 *bayer_out, u32 *rgb_out)
{
	u32 bayer = c->global.bayer_enables, rgb = c->global.rgb_enables;
	bool tdn_reset = c->tdn.reset & 1;

	if ((bayer & ~LPB_KNOWN_BAYER) || (rgb & ~LPB_KNOWN_RGB))
		return -EINVAL;
	if (!(bayer & PISP_BE_BAYER_ENABLE_INPUT) == !(rgb & PISP_BE_RGB_ENABLE_INPUT))
		return -EINVAL;		/* exactly one input */
	if (rgb & PISP_BE_RGB_ENABLE_HOG)
		return -EOPNOTSUPP;	/* HOG's address and size aren't provable */

	if (bayer & PISP_BE_BAYER_ENABLE_INPUT) {
		/* Bits that move data must come with what they read or write. */
		if ((bayer & PISP_BE_BAYER_ENABLE_TDN_INPUT) &&
		    !(bayer & PISP_BE_BAYER_ENABLE_TDN))
			return -EINVAL;
		if ((bayer & PISP_BE_BAYER_ENABLE_TDN) && !(bayer & PISP_BE_BAYER_ENABLE_TDN_INPUT) &&
		    !tdn_reset)
			return -EINVAL;	/* TDN without its average and not resetting: lockup */
		if ((bayer & PISP_BE_BAYER_ENABLE_TDN_INPUT) && tdn_reset)
			return -EINVAL;	/* reset means there is no average to read */
		if ((bayer & PISP_BE_BAYER_ENABLE_TDN_DECOMPRESS) &&
		    !(bayer & PISP_BE_BAYER_ENABLE_TDN_INPUT))
			return -EINVAL;
		if ((bayer & PISP_BE_BAYER_ENABLE_TDN_COMPRESS) &&
		    !(bayer & PISP_BE_BAYER_ENABLE_TDN_OUTPUT))
			return -EINVAL;
		if (!(bayer & PISP_BE_BAYER_ENABLE_STITCH_INPUT) !=
		    !(bayer & PISP_BE_BAYER_ENABLE_STITCH))
			return -EINVAL;	/* each needs the other */
		if ((bayer & PISP_BE_BAYER_ENABLE_STITCH_DECOMPRESS) &&
		    !(bayer & PISP_BE_BAYER_ENABLE_STITCH_INPUT))
			return -EINVAL;
		if ((bayer & PISP_BE_BAYER_ENABLE_STITCH_COMPRESS) &&
		    !(bayer & PISP_BE_BAYER_ENABLE_STITCH_OUTPUT))
			return -EINVAL;
	} else if (bayer) {
		return -EINVAL;		/* no Bayer input: the whole Bayer pipe off */
	}
	*bayer_out = bayer;
	*rgb_out = rgb;
	return 0;
}

/* A block that compresses works in units of 8 pixels, so its format has to say so. */
static bool lpb_compression_sized(u32 bayer, u32 bit, const struct pisp_image_format_config *f)
{
	return !(bayer & bit) || (f->format & PISP_IMAGE_FORMAT_COMPRESSION_MASK);
}

/*
 * The addresses. A config (in its kernel copy) and a job's thirteen addresses
 * name, as device addresses or as offsets from them, everything the block will
 * access. Each is in one of these and is checked here, in the kernel's copy,
 * against held memory with sizes computed from the config itself:
 *
 *   addr[0..2]        input planes              input_format, enabled by the input
 *   addr[3]           TDN input                 tdn_input_format, TDN_INPUT
 *   addr[4]           stitch input              stitch_input_format, STITCH_INPUT
 *   addr[5]           TDN output                tdn_output_format, TDN_OUTPUT
 *   addr[6]           stitch output             stitch_output_format, STITCH_OUTPUT
 *   addr[7..9]        output 0 planes           output_format[0], OUTPUT0
 *   addr[10..12]      output 1 planes           output_format[1], OUTPUT1
 *   tile input_addr_offset, input_addr_offset2   input planes (0, then 1 and 2)
 *   tile tdn_input_addr_offset                   addr[3]
 *   tile tdn_output_addr_offset                  addr[5]
 *   tile stitch_input_addr_offset                addr[4]
 *   tile stitch_output_addr_offset               addr[6]
 *   tile output_addr_offset[j], ..._offset2[j]   addr[7+3j], then 8+3j and 9+3j
 *   tile output_hog_addr_offset                  refused: HOG is refused entirely
 *   hog (config block)                           refused: rgb_enables HOG, see above
 *   output transform                             VFLIP refused: the block walks up
 *                                                from the tile's bottom line. HFLIP
 *                                                stays: the offset names the left end
 *   pad0[0..103], the config's first addresses   must be zero, never written
 *   pad0[104..111], the HOG address              must be zero, never written
 *
 * A slot's address is checked for its whole extent (stride * lines of the
 * format, for every plane the format has and the enables use) against
 * lem_stream_job_take, and each tile's touched rows against that extent.
 * Slots the enables don't use must carry address 0, and a format of a disabled
 * stage isn't looked at, because the hardware doesn't either.
 */
static int lpb_validate(struct lpb_stream *ps, const u64 *addr_in, struct lpb_job *job,
			struct lemnos_stream_range *range, u32 *nrange)
{
	struct pisp_be_tiles_config *t = job->cfg;
	const struct pisp_be_config *c = &t->config;
	struct lpb_plane p[LPB_ADDRS] = {};
	u32 bayer, rgb, num_tiles = t->num_tiles;
	int ret;

	/* Bytes where older layouts kept addresses and fields the hardware may use. */
	if (memchr_inv(c, 0, LPB_HEAD_BYTES) ||
	    memchr_inv((const u8 *)c + LPB_TAIL_OFFSET, 0, sizeof(*c) - LPB_TAIL_OFFSET) ||
	    memchr_inv(&c->axi, 0, sizeof(c->axi)))
		return -EINVAL;
	if (!num_tiles || num_tiles > PISP_BACK_END_NUM_TILES)
		return -EINVAL;
	ret = lpb_check_enables(c, &bayer, &rgb);
	if (ret)
		return ret;

	/* Which formats describe which address slots. Input first. */
	ret = lpb_check_format(&c->input_format, PISP_BACK_END_INPUT_ALIGN);
	if (ret)
		return ret;
	for (unsigned int i = 0; i < lpb_nplanes(c->input_format.format); i++)
		lpb_set_plane(&p[i], &c->input_format, i);
	if (bayer & PISP_BE_BAYER_ENABLE_INPUT) {
		bool compressed = c->input_format.format & PISP_IMAGE_FORMAT_COMPRESSION_MASK;

		if (compressed != !!(bayer & PISP_BE_BAYER_ENABLE_DECOMPRESS))
			return -EINVAL;
		/* Seen on a board: the job never finishes and only a reboot frees the block. */
		if (!compressed && (c->input_format.format & PISP_IMAGE_FORMAT_BPS_MASK) ==
				   PISP_IMAGE_FORMAT_BPS_8)
			return -EOPNOTSUPP;
	}

	if (bayer & PISP_BE_BAYER_ENABLE_TDN_INPUT) {
		ret = lpb_check_format(&c->tdn_input_format, PISP_BACK_END_INPUT_ALIGN);
		if (ret)
			return ret;
		lpb_set_plane(&p[3], &c->tdn_input_format, 0);
	}
	if (bayer & PISP_BE_BAYER_ENABLE_STITCH_INPUT) {
		ret = lpb_check_format(&c->stitch_input_format, PISP_BACK_END_INPUT_ALIGN);
		if (ret)
			return ret;
		lpb_set_plane(&p[4], &c->stitch_input_format, 0);
	}
	if (bayer & PISP_BE_BAYER_ENABLE_TDN_OUTPUT) {
		ret = lpb_check_format(&c->tdn_output_format, PISP_BACK_END_OUTPUT_MIN_ALIGN);
		if (ret)
			return ret;
		lpb_set_plane(&p[5], &c->tdn_output_format, 0);
	}
	if (bayer & PISP_BE_BAYER_ENABLE_STITCH_OUTPUT) {
		ret = lpb_check_format(&c->stitch_output_format, PISP_BACK_END_OUTPUT_MIN_ALIGN);
		if (ret)
			return ret;
		lpb_set_plane(&p[6], &c->stitch_output_format, 0);
	}
	if (!lpb_compression_sized(bayer, PISP_BE_BAYER_ENABLE_TDN_DECOMPRESS, &c->tdn_input_format) ||
	    !lpb_compression_sized(bayer, PISP_BE_BAYER_ENABLE_TDN_COMPRESS, &c->tdn_output_format) ||
	    !lpb_compression_sized(bayer, PISP_BE_BAYER_ENABLE_STITCH_DECOMPRESS,
				   &c->stitch_input_format) ||
	    !lpb_compression_sized(bayer, PISP_BE_BAYER_ENABLE_STITCH_COMPRESS,
				   &c->stitch_output_format))
		return -EINVAL;
	for (unsigned int o = 0; o < PISP_BACK_END_NUM_OUTPUTS; o++) {
		const struct pisp_image_format_config *f = &c->output_format[o].image;

		if (!(rgb & PISP_BE_RGB_ENABLE_OUTPUT(o)))
			continue;
		if (c->output_format[o].transform & ~PISP_BE_TRANSFORM_HFLIP)
			return -EOPNOTSUPP;
		ret = lpb_check_format(f, PISP_BACK_END_OUTPUT_MIN_ALIGN);
		if (ret)
			return ret;
		for (unsigned int i = 0; i < lpb_nplanes(f->format); i++)
			lpb_set_plane(&p[LEMNOS_PISP_BE_ADDR_OUTPUT(o, i)], f, i);
	}

	/* Every slot: an address only where the job uses one, inside held memory. */
	*nrange = 0;
	for (unsigned int i = 0; i < LPB_ADDRS; i++) {
		u64 addr = addr_in[i];
		u32 align = i <= LEMNOS_PISP_BE_ADDR_STITCH_INPUT ? PISP_BACK_END_INPUT_ALIGN :
			    PISP_BACK_END_OUTPUT_MIN_ALIGN;

		if (!p[i].fmt) {
			/* Disabled stage or a plane the format doesn't have: can't become live. */
			if (addr)
				return -EINVAL;
			job->addr[i] = 0;
			continue;
		}
		if (!addr || (addr & (align - 1)))
			return -EINVAL;
		/* Proved, and held, by lem_stream_job_take(). */
		range[*nrange].addr = addr;
		range[*nrange].len = p[i].bytes;
		(*nrange)++;
		job->addr[i] = addr;
	}

	/* Every tile, for every address it can use. */
	for (u32 n = 0; n < num_tiles; n++) {
		const struct pisp_tile *tile = &t->tiles[n];
		u32 w = tile->input_width, h = tile->input_height;
		u32 img_w = c->input_format.width;

		if (!w || !h)
			return -EINVAL;
		if (c->input_format.format & PISP_IMAGE_FORMAT_COMPRESSION_MASK)
			img_w = ALIGN(img_w, 8);	/* tiles are padded to the block grid */
		if ((u32)tile->input_offset_x + w > img_w ||
		    (u32)tile->input_offset_y + h > c->input_format.height)
			return -EINVAL;
		ret = lpb_tile_fits(&p[0], tile->input_addr_offset, w, h);
		if (ret)
			return ret;
		if (p[1].fmt) {
			ret = lpb_tile_fits(&p[1], tile->input_addr_offset2, w, h);
			if (ret)
				return ret;
		}
		if (p[2].fmt) {
			ret = lpb_tile_fits(&p[2], tile->input_addr_offset2, w, h);
			if (ret)
				return ret;
		}
		if (p[3].fmt) {
			ret = lpb_tile_fits(&p[3], tile->tdn_input_addr_offset, w, h);
			if (ret)
				return ret;
		}
		if (p[4].fmt) {
			ret = lpb_tile_fits(&p[4], tile->stitch_input_addr_offset, w, h);
			if (ret)
				return ret;
		}
		if (p[5].fmt) {
			ret = lpb_tile_fits(&p[5], tile->tdn_output_addr_offset, w, h);
			if (ret)
				return ret;
		}
		if (p[6].fmt) {
			ret = lpb_tile_fits(&p[6], tile->stitch_output_addr_offset, w, h);
			if (ret)
				return ret;
		}
		for (unsigned int o = 0; o < PISP_BACK_END_NUM_OUTPUTS; o++) {
			const struct pisp_image_format_config *f = &c->output_format[o].image;
			u32 ow = tile->output_width[o], oh = tile->output_height[o];
			const struct lpb_plane *op = &p[LEMNOS_PISP_BE_ADDR_OUTPUT(o, 0)];

			if (!(rgb & PISP_BE_RGB_ENABLE_OUTPUT(o)))
				continue;
			if (!ow != !oh)
				return -EINVAL;
			if (!ow)
				continue;	/* this tile writes nothing to this output */
			if ((u32)tile->output_offset_x[o] + ow > f->width ||
			    (u32)tile->output_offset_y[o] + oh > f->height)
				return -EINVAL;
			ret = lpb_tile_fits(&op[0], tile->output_addr_offset[o], ow, oh);
			if (ret)
				return ret;
			for (unsigned int i = 1; i < 3; i++) {
				if (!op[i].fmt)
					continue;
				ret = lpb_tile_fits(&op[i], tile->output_addr_offset2[o], ow, oh);
				if (ret)
					return ret;
			}
		}
	}

	job->bayer_en = bayer;
	job->rgb_en = rgb;
	job->num_tiles = num_tiles;
	return 0;
}

/* --- jobs from the ring --- */

/* Copies, checks, takes and starts what is posted. collect_lock held; process context. */
static long lpb_collect_locked(struct lpb_stream *ps)
{
	struct lem_pb *l = ps->l;
	struct lemnos_pisp_be_job wire;
	struct lemnos_stream_range range[LPB_ADDRS];
	unsigned long flags;

	if (ps->stopped)
		return -ENODEV;
	for (u32 n = 0; n <= ps->njobs; n++) {
		struct lpb_job *job;
		u32 nrange;
		int ret;

		spin_lock_irqsave(&l->lock, flags);
		if (l->wedged || ps->stopping) {
			int err = l->wedged ? -EIO : -ENODEV;

			spin_unlock_irqrestore(&l->lock, flags);
			return err;
		}
		job = list_first_entry_or_null(&ps->free, struct lpb_job, node);
		if (job)
			list_del(&job->node);
		spin_unlock_irqrestore(&l->lock, flags);
		if (!job)
			return 0;

		ret = lem_stream_job_peek(ps->s, &wire, sizeof(wire));
		if (ret <= 0)
			goto put_back;
		if (wire.config >= ps->config_slots) {
			ret = -EINVAL;
			goto refuse;
		}
		/* The copy: what is checked and what runs. */
		memcpy(job->cfg, (u8 *)ps->configs + (size_t)wire.config * LPB_CONFIG_BYTES,
		       LPB_CONFIG_BYTES);
		ret = lpb_validate(ps, wire.addr, job, range, &nrange);
		if (ret)
			goto refuse;
		ret = lem_stream_job_take(ps->s, wire.buffer, wire.identity, range, nrange, &job->slot);
		if (ret)
			goto refuse;
		memcpy(job->tiles, job->cfg->tiles, LPB_TILES_BYTES);

		spin_lock_irqsave(&l->lock, flags);
		list_add_tail(&job->node, &ps->ready);
		lpb_schedule(l, ps);
		spin_unlock_irqrestore(&l->lock, flags);
		continue;
refuse:
		spin_lock_irqsave(&l->lock, flags);
		l->refused++;
		spin_unlock_irqrestore(&l->lock, flags);
put_back:
		spin_lock_irqsave(&l->lock, flags);
		list_add(&job->node, &ps->free);
		spin_unlock_irqrestore(&l->lock, flags);
		return ret;
	}
	return 0;
}

/* LEMNOS_STREAM_IOC_KICK. An errno ends the stream. */
static long lpb_kick(void *priv)
{
	struct lpb_stream *ps = priv;
	long ret;

	mutex_lock(&ps->collect_lock);
	ret = lpb_collect_locked(ps);
	mutex_unlock(&ps->collect_lock);
	return ret;
}

/* After a job finishes: pick up what the consumer posted since, without a kick. */
static void lpb_collect_work(struct work_struct *work)
{
	struct lpb_stream *ps = container_of(to_delayed_work(work), struct lpb_stream, collect);
	long ret = lpb_kick(ps);

	if (ret && ret != -ENODEV)
		lem_stream_fault(ps->s, ret);
}

/* --- the stream's lifetime --- */

/*
 * Stops DMA. Everything on the block finishes, or it's wedged and the stream
 * keeps its pins. s->ctl held, s->dead set: nothing new can be taken.
 */
static void lpb_stop(void *priv, struct lem_stream *s)
{
	struct lpb_stream *ps = priv;
	struct lem_pb *l = ps->l;
	struct lpb_job *job, *tmp;
	unsigned long flags;
	bool busy, wedged;

	spin_lock_irqsave(&l->lock, flags);
	ps->stopping = true;
	spin_unlock_irqrestore(&l->lock, flags);
	cancel_delayed_work_sync(&ps->collect);

	/* A collection already running finishes first; none starts after. */
	mutex_lock(&ps->collect_lock);
	ps->stopped = true;
	mutex_unlock(&ps->collect_lock);

	spin_lock_irqsave(&l->lock, flags);
	list_for_each_entry_safe(job, tmp, &ps->ready, node) {
		list_move(&job->node, &ps->free);
		lem_stream_cancel(s, &job->slot);
	}
	spin_unlock_irqrestore(&l->lock, flags);

	/* Let the block finish what it holds before anything goes. */
	wait_event_timeout(ps->idle, ({
		bool done;

		spin_lock_irqsave(&l->lock, flags);
		done = (!ps->queued && !ps->running) || l->wedged;
		spin_unlock_irqrestore(&l->lock, flags);
		done;
	}), msecs_to_jiffies(LPB_TIMEOUT_MS * 2));

	spin_lock_irqsave(&l->lock, flags);
	busy = ps->queued || ps->running;
	if (busy && !l->wedged)
		lpb_wedge(l, ps, -ETIMEDOUT, lpb_rd(l, BE_STATUS_REG), lpb_rd(l, BE_BATCH_STATUS_REG));
	wedged = l->wedged;
	/* Wedged too: the stream's memory goes with its file, and nothing here may touch it. */
	if (l->attached == ps)
		l->attached = NULL;
	spin_unlock_irqrestore(&l->lock, flags);

	if (wedged || busy) {
		dev_crit(l->dev, "stream wedged: keeping its buffers until a reboot\n");
		lem_stream_wedge(s);
	}
}

static void lpb_release(void *priv)
{
	struct lpb_stream *ps = priv;

	/* The block may still read the tiles: leave them, and everything else, for good. */
	if (ps->wedged)
		return;
	for (u32 i = 0; i < ps->njobs; i++) {
		struct lpb_job *job = &ps->jobs[i];

		if (job->tiles)
			dma_free_coherent(ps->dev, LPB_TILES_BYTES, job->tiles, job->tiles_dma);
		kvfree(job->cfg);
	}
	kvfree(ps->jobs);
	vfree(ps->configs);
	kfree(ps);
}

static int lpb_mmap(void *priv, struct vm_area_struct *vma)
{
	struct lpb_stream *ps = priv;
	unsigned long len = vma->vm_end - vma->vm_start;

	if (!(vma->vm_flags & VM_SHARED) || vma->vm_pgoff != LPB_CONFIG_PGOFF ||
	    len != PAGE_ALIGN(ps->configs_bytes))
		return -EINVAL;
	if (vma->vm_flags & VM_EXEC)
		return -EPERM;
	vm_flags_clear(vma, VM_MAYEXEC);
	return remap_vmalloc_range(vma, ps->configs, 0);
}

static long lpb_ioctl(void *priv, unsigned int cmd, void __user *arg)
{
	struct lpb_stream *ps = priv;
	struct lem_pb *l = ps->l;
	struct lemnos_pisp_be_status st;
	unsigned long flags;

	if (cmd != LEMNOS_PISP_BE_IOC_STATUS)
		return -ENOTTY;
	memset(&st, 0, sizeof(st));
	spin_lock_irqsave(&l->lock, flags);
	st.hw_version = l->hw_version;
	st.wedged = l->wedged;
	st.finished = l->finished;
	st.refused = l->refused;
	spin_unlock_irqrestore(&l->lock, flags);
	return copy_to_user(arg, &st, sizeof(st)) ? -EFAULT : 0;
}


long lem_pb_create(struct lem_pb *l, struct device *dev, void *args)
{
	struct lemnos_pisp_be_create *a = args;
	struct lem_stream_producer producer = {
		.stop = lpb_stop,
		.job_bytes = sizeof(struct lemnos_pisp_be_job),
		.kick = lpb_kick,
		.ioctl = lpb_ioctl,
		.mmap = lpb_mmap,
		.release = lpb_release,
	};
	struct lpb_stream *ps;
	unsigned long flags;
	long ret = -ENOMEM;

	BUILD_BUG_ON(LPB_HEAD_BYTES != 112);
	BUILD_BUG_ON(LPB_CONFIG_BYTES != 16720);

	if (a->stream.job_bytes != sizeof(struct lemnos_pisp_be_job) ||
	    !a->stream.job_slots || a->stream.job_slots > LEMNOS_STREAM_MAX_JOB_SLOTS ||
	    !is_power_of_2(a->stream.job_slots))
		return -EINVAL;

	ps = kzalloc(sizeof(*ps), GFP_KERNEL);
	if (!ps)
		return -ENOMEM;
	ps->l = l;
	ps->dev = dev;
	ps->njobs = a->stream.job_slots;
	ps->config_slots = a->stream.job_slots;
	ps->configs_bytes = (size_t)ps->config_slots * LPB_CONFIG_BYTES;
	INIT_LIST_HEAD(&ps->free);
	INIT_LIST_HEAD(&ps->ready);
	init_waitqueue_head(&ps->idle);
	mutex_init(&ps->collect_lock);
	INIT_DELAYED_WORK(&ps->collect, lpb_collect_work);

	ps->jobs = kvcalloc(ps->njobs, sizeof(*ps->jobs), GFP_KERNEL);
	ps->configs = vmalloc_user(PAGE_ALIGN(ps->configs_bytes));
	if (!ps->jobs || !ps->configs)
		goto fail;
	for (u32 i = 0; i < ps->njobs; i++) {
		struct lpb_job *job = &ps->jobs[i];

		job->cfg = kvzalloc(LPB_CONFIG_BYTES, GFP_KERNEL);
		job->tiles = dma_alloc_coherent(dev, LPB_TILES_BYTES, &job->tiles_dma, GFP_KERNEL);
		if (!job->cfg || !job->tiles)
			goto fail;
		list_add_tail(&job->node, &ps->free);
	}

	mutex_lock(&l->create_lock);
	if (READ_ONCE(l->wedged)) {
		ret = -EIO;
		goto unlock;
	}
	spin_lock_irqsave(&l->lock, flags);
	if (l->attached) {
		ret = -EBUSY;
		spin_unlock_irqrestore(&l->lock, flags);
		goto unlock;
	}
	{
		/* An idle block's counters agree; new jobs are counted from here. */
		u32 batch = lpb_rd(l, BE_BATCH_STATUS_REG);

		if ((u8)batch != (u8)(batch >> 8) || lpb_rd(l, BE_STATUS_REG)) {
			ret = -EBUSY;
			spin_unlock_irqrestore(&l->lock, flags);
			goto unlock;
		}
		l->done = (u8)batch;
		l->started = (u8)(batch >> 8);
		l->busy = false;
	}
	l->attached = ps;	/* nothing it can do until the stream has its first job */
	spin_unlock_irqrestore(&l->lock, flags);

	/* The stream's frame is a whole buffer; where a job writes is proved job by job. */
	producer.priv = ps;
	ret = lem_stream_create_producer(dev, &a->stream, &producer, &ps->s);
	if (ret) {
		spin_lock_irqsave(&l->lock, flags);
		l->attached = NULL;
		spin_unlock_irqrestore(&l->lock, flags);
		goto unlock;
	}
	mutex_unlock(&l->create_lock);

	a->config_slot_bytes = LPB_CONFIG_BYTES;
	a->hw_version = l->hw_version;
	a->config_mmap_offset = (u64)LPB_CONFIG_PGOFF << PAGE_SHIFT;
	a->config_bytes = PAGE_ALIGN(ps->configs_bytes);
	return 0;

unlock:
	mutex_unlock(&l->create_lock);
fail:
	/* Only a stream that exists owns ps, and its creation failed. */
	lpb_release(ps);
	return ret;
}

/* The ISR's work is done and the streams are gone (or wedged). */
void lem_pb_remove(struct lem_pb *l)
{
	unsigned long flags;

	spin_lock_irqsave(&l->lock, flags);
	lpb_wr(l, BE_INTERRUPT_EN_REG, 0);
	l->attached = NULL;
	spin_unlock_irqrestore(&l->lock, flags);
	cancel_delayed_work_sync(&l->watchdog);
}

static void lpb_destroy_wq(void *wq)
{
	destroy_workqueue(wq);
}

/* pispbe_hw_init: a known version, and a block that is idle. */
static int lpb_hw_init(struct lem_pb *l)
{
	u32 u = lpb_rd(l, BE_VERSION_REG);

	l->hw_version = u;
	if ((u & ~BE_VERSION_MINOR_BITS) != BE_VERSION_BLOCK)
		return -ENODEV;
	lpb_wr(l, BE_INTERRUPT_STATUS_REG, 0xffffffffu);
	u = lpb_rd(l, BE_BATCH_STATUS_REG);
	l->done = (u8)u;
	l->started = (u8)(u >> 8);
	u = lpb_rd(l, BE_STATUS_REG);
	if (u != 0 || l->done != l->started) {
		dev_err(l->dev, "block is stuck or busy (status 0x%08x)\n", u);
		return -EBUSY;
	}
	lpb_wr(l, BE_AXI_REG, BE_AXI_VALUE);
	lpb_wr(l, BE_INTERRUPT_EN_REG, 0x3);
	return 0;
}

struct lem_pb *lem_pb_probe(struct platform_device *pdev)
{
	struct device *dev = &pdev->dev;
	struct resource *res;
	struct lem_pb *l;
	int ret;

	l = devm_kzalloc(dev, sizeof(*l), GFP_KERNEL);
	if (!l)
		return ERR_PTR(-ENOMEM);
	l->dev = dev;
	spin_lock_init(&l->lock);
	mutex_init(&l->create_lock);
	INIT_DELAYED_WORK(&l->watchdog, lpb_watchdog);

	res = platform_get_resource(pdev, IORESOURCE_MEM, 0);
	if (!res || resource_size(res) < LPB_WINDOW)
		return ERR_PTR(-EINVAL);
	if (test_pb_fake_regs) {
		void *mem = devm_memremap(dev, res->start, resource_size(res), MEMREMAP_WB);

		if (IS_ERR(mem))
			return ERR_CAST(mem);
		l->regs = (void __iomem *)mem;
		dev_warn(dev, "test_pb_fake_regs: registers are ordinary memory\n");
	} else {
		l->regs = devm_ioremap_resource(dev, res);
		if (IS_ERR(l->regs))
			return ERR_CAST(l->regs);
	}
	l->irq = platform_get_irq(pdev, 0);
	if (l->irq <= 0)
		return ERR_PTR(-EINVAL);
	ret = dma_set_mask_and_coherent(dev, DMA_BIT_MASK(36));
	if (ret)
		return ERR_PTR(ret);
	l->clk = devm_clk_get_optional_enabled(dev, NULL);
	if (IS_ERR(l->clk))
		return ERR_CAST(l->clk);
	ret = lpb_hw_init(l);
	if (ret)
		return ERR_PTR(ret);

	l->wq = alloc_ordered_workqueue("lemnos-pisp-be", 0);
	if (!l->wq)
		return ERR_PTR(-ENOMEM);
	ret = devm_add_action_or_reset(dev, lpb_destroy_wq, l->wq);
	if (ret)
		return ERR_PTR(ret);
	ret = devm_request_irq(dev, l->irq, lpb_isr, 0, dev_name(dev), l);
	if (ret)
		return ERR_PTR(ret);
	return l;
}
