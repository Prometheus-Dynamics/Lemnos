// SPDX-License-Identifier: GPL-2.0
/*
 * The K230 graphics DMA engine (GDMA), memory to memory, as a frame stream of
 * lemnos.ko. Jobs are posted on the stream's ring (lemnos_gsdma.h).
 *
 * The engine has no MMU in front of it and takes 32-bit bus addresses: every
 * address it is given is memory it will read or write. So nothing it is given
 * comes from user space. A job is read once into kernel memory; the bytes the
 * engine will touch on each plane are worked out from the format, size and
 * rotation, and every span is proved inside a dma-buf registered on the stream
 * and below the end of the memory the engine reaches, with arithmetic that
 * cannot wrap; then the descriptors are built in the module's own coherent
 * memory from the device addresses of the memory the stream pinned. Nothing is
 * clamped: a job that fails a check ends the stream.
 *
 * Interface facts (register offsets, the descriptor's fields, the start,
 * interrupt and abort sequences, the configuration values written at probe,
 * the formats, the rotation codes, the 2 GiB bound) are from the Kendryte K230
 * Linux SDK, github.com/kendryte/k230_linux_sdk at
 * fc6cc2151d54cb307be47f81815023f7ffb0eae6: buildroot-overlay/linux/
 * 0023-add-gdma-vo-rotation.patch (drivers/soc/canaan/k230-gdma.c) and
 * 0029-canaan-add-harden-DMA-address-validation.patch. No code is copied;
 * drivers/k230-gsdma/src/uapi.rs records each fact.
 *
 * Stopping. The vendor driver stops a chain that ran too long by turning the
 * channel off and on, and assumes that worked. Here the engine's progress
 * registers (current descriptor, size and addresses) must hold still for
 * several reads in a row after the channel is off before the job's memory is
 * let go. If they never do, the engine is wedged: the job's memory and the
 * descriptors stay pinned for good, and the block refuses new streams.
 *
 * Locking: ps->collect_lock (one collector; `stopped`), g->lock (the job on
 * the engine and the registers while it starts), s->lock in the stream code.
 * Never s->lock then g->lock. The ISR takes g->lock.
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <linux/clk.h>
#include <linux/delay.h>
#include <linux/dma-mapping.h>
#include <linux/err.h>
#include <linux/interrupt.h>
#include <linux/io.h>
#include <linux/ktime.h>
#include <linux/module.h>
#include <linux/mutex.h>
#include <linux/overflow.h>
#include <linux/platform_device.h>
#include <linux/slab.h>
#include <linux/spinlock.h>
#include <linux/uaccess.h>
#include <linux/wait.h>
#include <linux/workqueue.h>

#include "lemnos_internal.h"
#include "lemnos_gsdma.h"

/* Registers (struct k230_gdma_regs). */
#define GD_CH_EN		0x00
#define GD_CH_EN_BIT		BIT(4)
#define GD_INT_MASK		0x04
#define GD_INT_STAT		0x08
#define GD_CFG			0x0c
#define GD_CTRL			0x10
#define GD_LLI_BASE		0x14
#define GD_CURRENT_LLT		0x38
#define GD_CURRENT_SIZE		0x3c
#define GD_CURRENT_SADDR	0x40
#define GD_CURRENT_DADDR	0x44
#define GD_WEIGHT		0x48
#define GD_WINDOW		0x50
#define GD_DONE_INT		BIT(18)
#define GD_INT_BITS		GENMASK(18, 16)
#define GD_CFG_VALUE		((7u << 8) | 0xffu)
#define GD_WEIGHT_VALUE		0x00444210u

/* The descriptor (struct k230_gdma_llt): six little-endian words. */
#define GD_DESC_WORDS		6
#define GD_DESC_BYTES		(GD_DESC_WORDS * 4)
#define GD_W0_MIRROR_X		BIT(2)
#define GD_W0_MIRROR_Y		BIT(3)
#define GD_W0_PIXEL(p)		((u32)(p) << 8)
#define GD_W0_NODE_INTR		BIT(30)
#define GD_PIXEL_8		0
#define GD_PIXEL_16		1
#define GD_PIXEL_24		2
#define GD_PIXEL_32		3

/* K230_DDR_PHYS_END: DDR from 0, and what a 32-bit address reaches with it. */
#define GD_DDR_END		0x80000000ull
#define GD_MAX_FIELD		0xffffu
#define GD_PLANES		LEMNOS_GSDMA_MAX_PLANES

/* The vendor driver's timeout, plus 1 ms per 256 KiB the job writes. */
#define GD_TIMEOUT_MS		100u
#define GD_TIMEOUT_SHIFT	18
/* Stillness: the progress registers unchanged for HOLD, read every GAP, given up after STILL. */
#define GD_STILL_HOLD_MS	10
#define GD_STILL_GAP_US		500
#define GD_STILL_MS		100

static bool test_gsdma_fake_regs;
module_param(test_gsdma_fake_regs, bool, 0444);
MODULE_PARM_DESC(test_gsdma_fake_regs,
		 "Test only: the GDMA's register window is ordinary memory (the test harness)");

static unsigned long long test_gsdma_reach;
module_param(test_gsdma_reach, ullong, 0444);
MODULE_PARM_DESC(test_gsdma_reach,
		 "Test only: lowers the end of the memory a job's images may lie in (bytes; 0 leaves it)");

struct lem_gd;

struct lgd_stream {
	struct lem_gd *g;
	struct lem_stream *s;

	struct mutex collect_lock;	/* one collector; `stopped` */
	bool stopped;			/* no collection touches `g` once this is set */

	/* Under g->lock. */
	bool stopping;
	bool wedged;
	u64 sequence;

	wait_queue_head_t idle;
	struct delayed_work collect;
};

struct lem_gd {
	struct device *dev;
	void __iomem *regs;
	struct workqueue_struct *wq;
	struct mutex create_lock;	/* one stream at a time */
	__le32 *chain;			/* coherent: what the engine walks */
	dma_addr_t chain_dma;
	u64 reach;			/* one past the last address an image may use */
	int irq;

	spinlock_t lock;		/* everything below */
	struct lgd_stream *attached;
	bool running;			/* the engine has `slot` */
	bool aborting;			/* the watchdog is stopping it */
	bool wedged;
	bool module_held;
	struct lem_stream_slot slot;
	u64 finished, refused, timeouts;
	struct delayed_work watchdog;
};

static u32 lgd_rd(struct lem_gd *g, unsigned int off)
{
	return readl(g->regs + off);
}

static void lgd_wr(struct lem_gd *g, unsigned int off, u32 val)
{
	writel(val, g->regs + off);
}

static void lgd_channel(struct lem_gd *g, bool on)
{
	u32 v = lgd_rd(g, GD_CH_EN);

	lgd_wr(g, GD_CH_EN, on ? v | GD_CH_EN_BIT : v & ~GD_CH_EN_BIT);
}

/* --- the checks --- */

struct lgd_format {
	u32 fourcc;
	u32 planes;
	/* Per plane: bytes of an element, its descriptor code, subsampling across and down. */
	u8 bytes[2], code[2], across[2], down[2];
};

#define FOURCC(a, b, c, d) ((u32)(a) | (u32)(b) << 8 | (u32)(c) << 16 | (u32)(d) << 24)

/* k230_gdma_build_format_desc's ten formats. */
static const struct lgd_format lgd_formats[] = {
	{ FOURCC('A', 'R', '1', '5'), 1, { 2 }, { GD_PIXEL_16 }, { 1 }, { 1 } },
	{ FOURCC('A', 'R', '1', '2'), 1, { 2 }, { GD_PIXEL_16 }, { 1 }, { 1 } },
	{ FOURCC('R', 'G', '1', '6'), 1, { 2 }, { GD_PIXEL_16 }, { 1 }, { 1 } },
	{ FOURCC('R', 'G', '2', '4'), 1, { 3 }, { GD_PIXEL_24 }, { 1 }, { 1 } },
	{ FOURCC('B', 'G', '2', '4'), 1, { 3 }, { GD_PIXEL_24 }, { 1 }, { 1 } },
	{ FOURCC('A', 'R', '2', '4'), 1, { 4 }, { GD_PIXEL_32 }, { 1 }, { 1 } },
	{ FOURCC('N', 'V', '1', '2'), 2, { 1, 2 }, { GD_PIXEL_8, GD_PIXEL_16 }, { 1, 2 }, { 1, 2 } },
	{ FOURCC('N', 'V', '2', '1'), 2, { 1, 2 }, { GD_PIXEL_8, GD_PIXEL_16 }, { 1, 2 }, { 1, 2 } },
	{ FOURCC('N', 'V', '1', '6'), 2, { 1, 2 }, { GD_PIXEL_8, GD_PIXEL_16 }, { 1, 2 }, { 1, 1 } },
	{ FOURCC('N', 'V', '6', '1'), 2, { 1, 2 }, { GD_PIXEL_8, GD_PIXEL_16 }, { 1, 2 }, { 1, 1 } },
};

/* A job, checked: what is built into descriptors. */
struct lgd_plan {
	u32 planes;
	u32 word0[GD_PLANES];		/* rotation, mirrors, element */
	u16 width[GD_PLANES], height[GD_PLANES];	/* the source plane's, in elements */
	u16 src_pitch[GD_PLANES], dst_pitch[GD_PLANES];
	/* Ranges for lem_stream_job_take: source then destination, per plane. */
	struct lemnos_stream_range range[2 * GD_PLANES];
	u64 bytes;			/* written, for the timeout */
};

/*
 * One side of one plane: `rows` rows of `row` bytes, `pitch` apart, from
 * `addr`. Proves the pitch and that the span neither wraps nor passes `reach`.
 * rows and pitch are at most 65535 and row at most 4 * 65535, so the extent
 * fits in u64 with room to spare; the sum with addr is checked.
 */
static int lgd_span(u64 addr, u32 pitch, u64 rows, u64 row, u64 reach,
		    struct lemnos_stream_range *out)
{
	u64 extent, end;

	if (!pitch || pitch > GD_MAX_FIELD || pitch < row)
		return -EINVAL;
	extent = (u64)pitch * (rows - 1) + row;
	if (check_add_overflow(addr, extent, &end))
		return -EOVERFLOW;
	if (end > reach)
		return -ENXIO;
	out->addr = addr;
	out->len = extent;
	return 0;
}

static bool lgd_overlap(const struct lemnos_stream_range *a, const struct lemnos_stream_range *b)
{
	/* Both were proved not to wrap. */
	return a->addr < b->addr + b->len && b->addr < a->addr + a->len;
}

/* Checks the kernel's copy of a job. Touches nothing. */
static int lgd_validate(const struct lem_gd *g, const struct lemnos_gsdma_job *job,
			struct lgd_plan *plan)
{
	const struct lgd_format *f = NULL;
	u32 rotate = job->rotation & 0xf, turn;
	bool swaps;

	memset(plan, 0, sizeof(*plan));
	for (u32 i = 0; i < ARRAY_SIZE(lgd_formats); i++)
		if (lgd_formats[i].fourcc == job->fourcc)
			f = &lgd_formats[i];
	if (!f || job->planes != f->planes)
		return -EINVAL;
	if (!job->width || job->width > GD_MAX_FIELD || !job->height || job->height > GD_MAX_FIELD)
		return -EINVAL;
	/* Exactly one turn, mirrors, nothing else (k230_gdma_rotation_valid). */
	if (job->rotation & ~0x3fu || hweight32(rotate) != 1)
		return -EINVAL;
	turn = __ffs(rotate);		/* the descriptor's code: clockwise quarter turns */
	swaps = turn & 1;
	/* Planes the format doesn't have are all zero. */
	for (u32 i = f->planes; i < GD_PLANES; i++) {
		const struct lemnos_gsdma_plane *p = &job->plane[i];

		if (p->src_addr || p->dst_addr || p->src_pitch || p->dst_pitch)
			return -EINVAL;
	}

	plan->planes = f->planes;
	for (u32 i = 0; i < f->planes; i++) {
		const struct lemnos_gsdma_plane *p = &job->plane[i];
		u64 width = DIV_ROUND_UP(job->width, f->across[i]);
		u64 height = DIV_ROUND_UP(job->height, f->down[i]);
		u64 bytes = f->bytes[i];
		struct lemnos_stream_range *src = &plan->range[2 * i], *dst = src + 1;
		int ret;

		ret = lgd_span(p->src_addr, p->src_pitch, height, width * bytes, g->reach, src);
		if (ret)
			return ret;
		if (swaps)
			ret = lgd_span(p->dst_addr, p->dst_pitch, width, height * bytes, g->reach, dst);
		else
			ret = lgd_span(p->dst_addr, p->dst_pitch, height, width * bytes, g->reach, dst);
		if (ret)
			return ret;
		plan->word0[i] = turn | GD_W0_PIXEL(f->code[i]) |
				 (job->rotation & LEMNOS_GSDMA_REFLECT_X ? GD_W0_MIRROR_X : 0) |
				 (job->rotation & LEMNOS_GSDMA_REFLECT_Y ? GD_W0_MIRROR_Y : 0);
		plan->width[i] = width;
		plan->height[i] = height;
		plan->src_pitch[i] = p->src_pitch;
		plan->dst_pitch[i] = p->dst_pitch;
		plan->bytes += dst->len;
	}
	/* The engine would read what it already wrote. */
	for (u32 i = 0; i < f->planes; i++) {
		const struct lemnos_stream_range *dst = &plan->range[2 * i + 1];

		for (u32 j = 0; j < f->planes; j++)
			if (lgd_overlap(dst, &plan->range[2 * j]) ||
			    (j < i && lgd_overlap(dst, &plan->range[2 * j + 1])))
				return -ENOTUNIQ;
	}
	return 0;
}

/*
 * The descriptors, from the device addresses the stream computed for the ranges
 * it holds. Each is checked again against the reach, so nothing outside the
 * memory the engine may use can reach a descriptor whatever happened between.
 */
static int lgd_build(struct lem_gd *g, struct lem_stream *s, const struct lgd_plan *plan,
		     const struct lem_stream_slot *slot)
{
	u32 desc[GD_PLANES * GD_DESC_WORDS] = {};

	for (u32 i = 0; i < plan->planes; i++) {
		u64 src, dst, end;
		u32 *d = &desc[i * GD_DESC_WORDS];
		bool last = i + 1 == plan->planes;
		int ret;

		ret = lem_stream_range_addr(s, &slot->job.range[2 * i], &src);
		if (!ret)
			ret = lem_stream_range_addr(s, &slot->job.range[2 * i + 1], &dst);
		if (ret)
			return ret;
		if (check_add_overflow(src, slot->job.range[2 * i].len, &end) || end > g->reach ||
		    check_add_overflow(dst, slot->job.range[2 * i + 1].len, &end) || end > g->reach)
			return -ENXIO;
		d[0] = plan->word0[i] | (last ? GD_W0_NODE_INTR : 0);
		d[1] = lower_32_bits(src);
		d[2] = plan->width[i] | (u32)plan->height[i] << 16;
		d[3] = plan->src_pitch[i] | (u32)plan->dst_pitch[i] << 16;
		d[4] = lower_32_bits(dst);
		/* The chain was proved inside the engine's reach at probe. */
		d[5] = last ? 0 : lower_32_bits(g->chain_dma + (i + 1) * GD_DESC_BYTES);
	}
	for (u32 i = 0; i < ARRAY_SIZE(desc); i++)
		WRITE_ONCE(g->chain[i], cpu_to_le32(desc[i]));
	return 0;
}

/* --- the engine --- */

/* The block can't be given up safely. g->lock held. */
static void lgd_wedge(struct lem_gd *g, struct lgd_stream *ps, int err)
{
	if (!g->wedged)
		dev_crit(g->dev, "engine did not stop: its job's buffers stay held until a reboot\n");
	g->wedged = true;
	if (!g->module_held) {
		g->module_held = true;
		__module_get(THIS_MODULE);
	}
	if (ps) {
		ps->wedged = true;
		lem_stream_fault(ps->s, err);
		wake_up_all(&ps->idle);
	}
}

/* The job on the engine is over: publish its frame. g->lock held. */
static void lgd_finish(struct lem_gd *g, struct lgd_stream *ps, u32 bytes)
{
	g->running = false;
	g->aborting = false;
	lem_stream_complete(ps->s, &g->slot, bytes, ps->sequence++, ktime_get_ns());
	wake_up_all(&ps->idle);
	if (!ps->stopping)
		queue_delayed_work(g->wq, &ps->collect, 0);
}

static irqreturn_t lgd_isr(int irq, void *data)
{
	struct lem_gd *g = data;
	struct lgd_stream *ps;
	u32 stat = lgd_rd(g, GD_INT_STAT);

	if (!stat)
		return IRQ_NONE;
	lgd_wr(g, GD_INT_STAT, stat);
	spin_lock(&g->lock);
	ps = g->attached;
	/* While the watchdog stops it, the job's end is the watchdog's to call. */
	if (ps && g->running && !g->aborting && !g->wedged && (stat & GD_DONE_INT)) {
		g->finished++;
		cancel_delayed_work(&g->watchdog);
		lgd_finish(g, ps, g->slot.bytes);
	}
	spin_unlock(&g->lock);
	return IRQ_HANDLED;
}

/* Whether the engine's progress holds still, read over a bounded time. Sleeps. */
static bool lgd_still(struct lem_gd *g)
{
	ktime_t start = ktime_get(), until = ktime_add_ms(start, GD_STILL_MS), since = start;
	u32 last[4] = {};
	bool first = true;

	for (;;) {
		ktime_t t = ktime_get();
		u32 now[4] = {
			lgd_rd(g, GD_CURRENT_LLT), lgd_rd(g, GD_CURRENT_SIZE),
			lgd_rd(g, GD_CURRENT_SADDR), lgd_rd(g, GD_CURRENT_DADDR),
		};

		if (first || memcmp(now, last, sizeof(now)))
			since = t;
		else if (ktime_ms_delta(t, since) >= GD_STILL_HOLD_MS)
			return true;
		if (!ktime_before(t, until))
			return false;
		first = false;
		memcpy(last, now, sizeof(now));
		usleep_range(GD_STILL_GAP_US, 2 * GD_STILL_GAP_US);
	}
}

/* A job outstanding too long: stop the channel, and only let go once it is seen stopped. */
static void lgd_watchdog(struct work_struct *work)
{
	struct lem_gd *g = container_of(to_delayed_work(work), struct lem_gd, watchdog);
	struct lgd_stream *ps;
	unsigned long flags;
	bool still;

	spin_lock_irqsave(&g->lock, flags);
	if (!g->running || g->aborting || g->wedged) {
		spin_unlock_irqrestore(&g->lock, flags);
		return;
	}
	g->aborting = true;
	lgd_channel(g, false);
	lgd_wr(g, GD_INT_STAT, 0xffffffffu);
	spin_unlock_irqrestore(&g->lock, flags);

	still = lgd_still(g);

	spin_lock_irqsave(&g->lock, flags);
	ps = g->attached;
	if (still) {
		lgd_channel(g, true);
		g->timeouts++;
		dev_warn_ratelimited(g->dev, "job timed out; engine stopped and ready again\n");
		if (ps)
			lgd_finish(g, ps, 0);
		else
			g->running = g->aborting = false;
	} else {
		lgd_wedge(g, ps, -EIO);
	}
	spin_unlock_irqrestore(&g->lock, flags);
}

/* --- jobs from the ring --- */

/* Copies, checks, takes and starts what is posted. collect_lock held; process context. */
static long lgd_collect_locked(struct lgd_stream *ps)
{
	struct lem_gd *g = ps->g;
	struct lemnos_gsdma_job job;
	struct lem_stream_slot slot;
	struct lgd_plan plan;
	unsigned long flags;
	u64 timeout;
	int ret;

	if (ps->stopped)
		return -ENODEV;
	spin_lock_irqsave(&g->lock, flags);
	if (g->wedged || ps->stopping) {
		ret = g->wedged ? -EIO : -ENODEV;
		spin_unlock_irqrestore(&g->lock, flags);
		return ret;
	}
	ret = g->running;
	spin_unlock_irqrestore(&g->lock, flags);
	if (ret)
		return 0;	/* one job at a time; the next is collected when it ends */

	ret = lem_stream_job_peek(ps->s, &job, sizeof(job));
	if (ret <= 0)
		return ret;
	ret = lgd_validate(g, &job, &plan);
	if (ret)
		goto refuse;
	ret = lem_stream_job_take(ps->s, job.buffer, job.identity, plan.range, 2 * plan.planes,
				  &slot);
	if (ret)
		goto refuse;
	/* Nothing runs on the engine: the chain is the collector's to write. */
	ret = lgd_build(g, ps->s, &plan, &slot);
	if (ret) {
		/* A span in the pool resolves, but is no image: never started, dropped. */
		lem_stream_cancel(ps->s, &slot);
		goto refuse;
	}
	timeout = GD_TIMEOUT_MS + (plan.bytes >> GD_TIMEOUT_SHIFT);

	spin_lock_irqsave(&g->lock, flags);
	if (g->wedged || ps->stopping) {
		ret = g->wedged ? -EIO : -ENODEV;
		spin_unlock_irqrestore(&g->lock, flags);
		lem_stream_cancel(ps->s, &slot);
		return ret;
	}
	g->slot = slot;
	g->running = true;
	lgd_wr(g, GD_INT_STAT, 0xffffffffu);
	lgd_wr(g, GD_LLI_BASE, lower_32_bits(g->chain_dma));
	/* The chain and the base before the start. */
	wmb();
	lgd_wr(g, GD_CTRL, 1);
	mod_delayed_work(g->wq, &g->watchdog, msecs_to_jiffies(timeout));
	spin_unlock_irqrestore(&g->lock, flags);
	return 0;
refuse:
	spin_lock_irqsave(&g->lock, flags);
	g->refused++;
	spin_unlock_irqrestore(&g->lock, flags);
	return ret;
}

/* LEMNOS_STREAM_IOC_KICK. An errno ends the stream. */
static long lgd_kick(void *priv)
{
	struct lgd_stream *ps = priv;
	long ret;

	mutex_lock(&ps->collect_lock);
	ret = lgd_collect_locked(ps);
	mutex_unlock(&ps->collect_lock);
	return ret;
}

/* After a job ends: pick up what the consumer posted since, without a kick. */
static void lgd_collect_work(struct work_struct *work)
{
	struct lgd_stream *ps = container_of(to_delayed_work(work), struct lgd_stream, collect);
	long ret = lgd_kick(ps);

	if (ret && ret != -ENODEV)
		lem_stream_fault(ps->s, ret);
}

/* --- the stream's lifetime --- */

/* The longest a job can be given, with its stopping, and a margin. */
#define GD_STOP_WAIT_MS (GD_TIMEOUT_MS + (((u64)GD_DDR_END * 2) >> GD_TIMEOUT_SHIFT) + \
			 2 * GD_STILL_MS + 1000)

/*
 * Stops DMA. The job on the engine finishes or is stopped by the watchdog, or
 * the engine is wedged and the stream keeps its pins. s->ctl held, s->dead set.
 */
static void lgd_stop(void *priv, struct lem_stream *s)
{
	struct lgd_stream *ps = priv;
	struct lem_gd *g = ps->g;
	unsigned long flags;
	bool busy, wedged;

	spin_lock_irqsave(&g->lock, flags);
	ps->stopping = true;
	spin_unlock_irqrestore(&g->lock, flags);
	cancel_delayed_work_sync(&ps->collect);

	/* A collection already running finishes first; none starts after. */
	mutex_lock(&ps->collect_lock);
	ps->stopped = true;
	mutex_unlock(&ps->collect_lock);

	wait_event_timeout(ps->idle, ({
		bool done;

		spin_lock_irqsave(&g->lock, flags);
		done = !g->running || g->wedged;
		spin_unlock_irqrestore(&g->lock, flags);
		done;
	}), msecs_to_jiffies(GD_STOP_WAIT_MS));

	spin_lock_irqsave(&g->lock, flags);
	busy = g->running && !g->wedged;
	if (busy)
		lgd_wedge(g, ps, -ETIMEDOUT);
	wedged = g->wedged && ps->wedged;
	if (g->attached == ps)
		g->attached = NULL;
	spin_unlock_irqrestore(&g->lock, flags);

	if (wedged) {
		dev_crit(g->dev, "stream wedged: keeping its buffers until a reboot\n");
		lem_stream_wedge(s);
	}
}

static void lgd_release(void *priv)
{
	kfree(priv);
}

static long lgd_ioctl(void *priv, unsigned int cmd, void __user *arg)
{
	struct lgd_stream *ps = priv;
	struct lem_gd *g = ps->g;
	struct lemnos_gsdma_status st;
	unsigned long flags;

	if (cmd != LEMNOS_GSDMA_IOC_STATUS)
		return -ENOTTY;
	memset(&st, 0, sizeof(st));
	spin_lock_irqsave(&g->lock, flags);
	st.wedged = g->wedged;
	st.finished = g->finished;
	st.refused = g->refused;
	st.timeouts = g->timeouts;
	spin_unlock_irqrestore(&g->lock, flags);
	return copy_to_user(arg, &st, sizeof(st)) ? -EFAULT : 0;
}

long lem_gd_create(struct lem_gd *g, struct device *dev, void *args)
{
	struct lemnos_gsdma_create *a = args;
	struct lem_stream_producer producer = {
		.stop = lgd_stop,
		.job_bytes = sizeof(struct lemnos_gsdma_job),
		.kick = lgd_kick,
		.ioctl = lgd_ioctl,
		.release = lgd_release,
	};
	struct lgd_stream *ps;
	unsigned long flags;
	long ret;

	BUILD_BUG_ON(sizeof(struct lemnos_gsdma_job) != 104);
	BUILD_BUG_ON(sizeof(struct lemnos_gsdma_create) != 64);

	if (a->stream.job_bytes != sizeof(struct lemnos_gsdma_job))
		return -EINVAL;
	ps = kzalloc(sizeof(*ps), GFP_KERNEL);
	if (!ps)
		return -ENOMEM;
	ps->g = g;
	init_waitqueue_head(&ps->idle);
	mutex_init(&ps->collect_lock);
	INIT_DELAYED_WORK(&ps->collect, lgd_collect_work);

	mutex_lock(&g->create_lock);
	spin_lock_irqsave(&g->lock, flags);
	ret = g->wedged ? -EIO : g->attached ? -EBUSY : 0;
	if (!ret)
		g->attached = ps;	/* nothing it can do until the stream has a job */
	spin_unlock_irqrestore(&g->lock, flags);
	if (ret)
		goto unlock;

	producer.priv = ps;
	ret = lem_stream_create_producer(dev, &a->stream, &producer, &ps->s);
	if (ret) {
		spin_lock_irqsave(&g->lock, flags);
		g->attached = NULL;
		spin_unlock_irqrestore(&g->lock, flags);
		goto unlock;
	}
	mutex_unlock(&g->create_lock);
	a->reach = g->reach;
	return 0;
unlock:
	mutex_unlock(&g->create_lock);
	/* Only a stream that exists owns ps, and its creation failed. */
	kfree(ps);
	return ret;
}

/* The streams are gone (or wedged). */
void lem_gd_remove(struct lem_gd *g)
{
	unsigned long flags;
	bool wedged;

	spin_lock_irqsave(&g->lock, flags);
	lgd_wr(g, GD_INT_MASK, lgd_rd(g, GD_INT_MASK) | GD_INT_BITS);
	g->attached = NULL;
	wedged = g->wedged;
	spin_unlock_irqrestore(&g->lock, flags);
	cancel_delayed_work_sync(&g->watchdog);
	/* A wedged engine may still read its chain. */
	if (!wedged)
		dma_free_coherent(g->dev, GD_PLANES * GD_DESC_BYTES, g->chain, g->chain_dma);
}

static void lgd_destroy_wq(void *wq)
{
	destroy_workqueue(wq);
}

struct lem_gd *lem_gd_probe(struct platform_device *pdev)
{
	struct device *dev = &pdev->dev;
	struct resource *res;
	struct lem_gd *g;
	struct clk *clk;
	int ret;

	g = devm_kzalloc(dev, sizeof(*g), GFP_KERNEL);
	if (!g)
		return ERR_PTR(-ENOMEM);
	g->dev = dev;
	spin_lock_init(&g->lock);
	mutex_init(&g->create_lock);
	INIT_DELAYED_WORK(&g->watchdog, lgd_watchdog);
	g->reach = GD_DDR_END;
	if (test_gsdma_reach) {
		g->reach = min_t(u64, g->reach, test_gsdma_reach);
		dev_warn(dev, "test_gsdma_reach: images must end below 0x%llx\n", g->reach);
	}

	res = platform_get_resource(pdev, IORESOURCE_MEM, 0);
	if (!res || resource_size(res) < GD_WINDOW)
		return ERR_PTR(-EINVAL);
	if (test_gsdma_fake_regs) {
		void *mem = devm_memremap(dev, res->start, resource_size(res), MEMREMAP_WB);

		if (IS_ERR(mem))
			return ERR_CAST(mem);
		g->regs = (void __iomem *)mem;
		dev_warn(dev, "test_gsdma_fake_regs: registers are ordinary memory\n");
	} else {
		g->regs = devm_ioremap_resource(dev, res);
		if (IS_ERR(g->regs))
			return ERR_CAST(g->regs);
	}
	g->irq = platform_get_irq(pdev, 0);
	if (g->irq <= 0)
		return ERR_PTR(-EINVAL);
	/* 32-bit bus addresses: the dma-bufs mapped for it, and the chain, below 4 GiB. */
	ret = dma_set_mask_and_coherent(dev, DMA_BIT_MASK(32));
	if (ret)
		return ERR_PTR(ret);
	clk = devm_clk_get_optional_enabled(dev, NULL);
	if (IS_ERR(clk))
		return ERR_CAST(clk);

	g->chain = dma_alloc_coherent(dev, GD_PLANES * GD_DESC_BYTES, &g->chain_dma, GFP_KERNEL);
	if (!g->chain)
		return ERR_PTR(-ENOMEM);
	if (g->chain_dma + GD_PLANES * GD_DESC_BYTES > GD_DDR_END) {
		dev_err(dev, "descriptor memory past the engine's reach\n");
		ret = -ENOMEM;
		goto free;
	}
	g->wq = alloc_ordered_workqueue("lemnos-gsdma", 0);
	if (!g->wq) {
		ret = -ENOMEM;
		goto free;
	}
	ret = devm_add_action_or_reset(dev, lgd_destroy_wq, g->wq);
	if (ret)
		goto free;

	/* Probe's sequence: interrupts clear and masked, channel on, the three unmasked. */
	lgd_wr(g, GD_INT_STAT, 0xffffffffu);
	lgd_wr(g, GD_INT_MASK, 0xffffffffu);
	lgd_channel(g, true);
	lgd_wr(g, GD_INT_MASK, lgd_rd(g, GD_INT_MASK) & ~GD_INT_BITS);
	lgd_wr(g, GD_CFG, GD_CFG_VALUE);
	lgd_wr(g, GD_WEIGHT, GD_WEIGHT_VALUE);

	ret = devm_request_irq(dev, g->irq, lgd_isr, 0, dev_name(dev), g);
	if (ret)
		goto free;
	return g;
free:
	dma_free_coherent(dev, GD_PLANES * GD_DESC_BYTES, g->chain, g->chain_dma);
	return ERR_PTR(ret);
}
