// SPDX-License-Identifier: GPL-2.0
/*
 * lemnos_pisp_be: the Raspberry Pi PiSP back end as a character device.
 *
 * The register sequence, the interrupt's job accounting and the probe check
 * are ported from the stock driver, drivers/media/platform/raspberrypi/
 * pisp_be/pisp_be.c:
 *   Copyright (c) 2021-2024 Raspberry Pi Limited.
 * The rest: Copyright (c) 2026 the Lemnos authors.
 *
 * A fast path, never a requirement: the module has no of_match_table, so it
 * binds only where driver_override names it, after the stock driver is
 * unbound. See kernel-modules/lemnos/pisp-be.md.
 */
#include <linux/clk.h>
#include <linux/delay.h>
#include <linux/dma-buf.h>
#include <linux/dma-mapping.h>
#include <linux/fs.h>
#include <linux/interrupt.h>
#include <linux/io.h>
#include <linux/miscdevice.h>
#include <linux/module.h>
#include <linux/platform_device.h>
#include <linux/poll.h>
#include <linux/scatterlist.h>
#include <linux/slab.h>
#include <linux/uaccess.h>
#include <linux/wait.h>
#include <linux/workqueue.h>

#include <uapi/linux/media/raspberrypi/pisp_be_config.h>

#include "lemnos_pisp_be.h"
#include "lemnos_personality.h"

#define LPB_NAME "lemnos-pisp-be"

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
#define BE_VERSION_2712		0x02252700
#define BE_VERSION_MINOR_BITS	0xf

/* Jobs submitted and not yet read back: the two the block holds, and two more. */
#define LPB_SLOTS 4
/* A job the block has not finished after this is a stopped block. */
#define LPB_TIMEOUT_MS 1000
/*
 * Buffers kept mapped on the block between jobs, as vb2 keeps an imported
 * buffer: a job on buffers already seen maps nothing. The least recently used
 * idle one goes when a new buffer needs its place, and all go at close.
 */
#define LPB_CACHE 16

static int test_unmap_plane = -1;
module_param(test_unmap_plane, int, 0644);
MODULE_PARM_DESC(test_unmap_plane,
		 "Test only: unmap this plane right after queueing each job (-1: never)");

static bool test_raw_enables;
module_param(test_raw_enables, bool, 0644);
MODULE_PARM_DESC(test_raw_enables,
		 "Test only: queue the caller's enables unsanitised (a Bayer pipe on an RGB input stops the block)");

/* A dma-buf mapped on the block. */
struct lpb_map {
	struct dma_buf *buf;		/* NULL: a free entry */
	struct dma_buf_attachment *att;
	struct sg_table *sgt;
	dma_addr_t addr;
	u64 size;
	unsigned int users;		/* planes of jobs not yet read back */
	u64 used;			/* the use clock at its last job */
};

enum lpb_state { SLOT_FREE, SLOT_PENDING, SLOT_HW, SLOT_DONE };

struct lpb_job {
	struct list_head node;
	enum lpb_state state;
	u64 tag;
	struct pisp_be_tiles_config *cfg;	/* coherent, kernel-only */
	dma_addr_t cfg_dma;
	dma_addr_t addr[LEMNOS_PISP_BE_PLANES];
	u64 len[LEMNOS_PISP_BE_PLANES];		/* bytes from the plane's offset to the end */
	struct lpb_map *map[LEMNOS_PISP_BE_PLANES];	/* under map_lock */
	u32 bayer_en, rgb_en;
	struct lemnos_pisp_be_done done;
};

struct lpb_dev {
	struct device *dev;
	void __iomem *regs;
	struct clk *clk;
	int irq;
	u32 hw_version;
	struct miscdevice misc;
	struct mutex open_lock;		/* one open file at a time */
	bool open;

	struct mutex map_lock;		/* the cache, and each job's map[] */
	struct lpb_map cache[LPB_CACHE];
	u64 use_clock;

	spinlock_t lock;		/* everything below */
	struct lpb_job slot[LPB_SLOTS];
	struct list_head pending;	/* submitted, not yet on the block */
	struct list_head finished;	/* finished, not yet read */
	struct lpb_job *queued, *running;
	bool hw_busy;
	bool dead;
	u8 done, started;
	wait_queue_head_t wait;
	struct workqueue_struct *wq;	/* the watchdog's own */
	struct delayed_work watchdog;
};

static u32 lpb_rd(struct lpb_dev *l, unsigned int off)
{
	return readl(l->regs + off);
}

static void lpb_wr(struct lpb_dev *l, unsigned int off, u32 val)
{
	writel(val, l->regs + off);
}

/* Writes a job to the block (pispbe_queue_job). Called with the lock held. */
static void lpb_queue_job(struct lpb_dev *l, struct lpb_job *job)
{
	unsigned int begin, end, u;

	if (lpb_rd(l, BE_STATUS_REG) & BE_STATUS_QUEUED)
		dev_err(l->dev, "not safe to queue a job\n");

	for (u = 0; u < LEMNOS_PISP_BE_PLANES; u++) {
		lpb_wr(l, BE_IO_ADDR_LOW(u), lower_32_bits(job->addr[u]));
		lpb_wr(l, BE_IO_ADDR_HIGH(u), upper_32_bits(job->addr[u]));
	}
	lpb_wr(l, BE_GLOBAL_BAYER_ENABLE, job->bayer_en);
	lpb_wr(l, BE_GLOBAL_RGB_ENABLE, job->rgb_en);

	begin = offsetof(struct pisp_be_config, global.bayer_order) / sizeof(u32);
	end = sizeof(struct pisp_be_config) / sizeof(u32);
	for (u = begin; u < end; u++)
		lpb_wr(l, BE_CONFIG_BASE_REG + sizeof(u32) * u,
		       ((u32 *)&job->cfg->config)[u]);

	lpb_wr(l, BE_TILE_ADDR_LO_REG,
	       lower_32_bits(job->cfg_dma + offsetof(struct pisp_be_tiles_config, tiles)));
	lpb_wr(l, BE_TILE_ADDR_HI_REG,
	       upper_32_bits(job->cfg_dma + offsetof(struct pisp_be_tiles_config, tiles)));
	lpb_wr(l, BE_CONTROL_REG,
	       BE_CONTROL_COPY_CONFIG | BE_CONTROL_QUEUE_JOB |
	       BE_CONTROL_NUM_TILES(job->cfg->num_tiles));
}

/* Starts the next pending job if the block can take one. Lock held. */
static void lpb_schedule(struct lpb_dev *l)
{
	struct lpb_job *job;

	if (l->hw_busy || l->dead)
		return;
	job = list_first_entry_or_null(&l->pending, struct lpb_job, node);
	if (!job)
		return;
	list_del(&job->node);
	job->state = SLOT_HW;
	l->queued = job;
	l->hw_busy = true;
	lpb_queue_job(l, job);
	mod_delayed_work(l->wq, &l->watchdog, msecs_to_jiffies(LPB_TIMEOUT_MS));
}

/* Lock held. */
static void lpb_finish(struct lpb_dev *l, struct lpb_job *job, s32 status,
		       u64 ns, u32 batch, u32 irq, u32 block)
{
	job->done = (struct lemnos_pisp_be_done){
		.tag = job->tag,
		.irq_ns = ns,
		.status = status,
		.batch_status = batch,
		.int_status = irq,
		.block_status = block,
	};
	job->state = SLOT_DONE;
	list_add_tail(&job->node, &l->finished);
}

/* The job accounting of pispbe_isr, with a completion record per job. */
static irqreturn_t lpb_isr(int irq, void *data)
{
	struct lpb_dev *l = data;
	u64 ns = ktime_get_ns();
	bool can_queue = false;
	u32 status, batch, block;
	u8 started, done;

	status = lpb_rd(l, BE_INTERRUPT_STATUS_REG);
	if (!status)
		return IRQ_NONE;
	lpb_wr(l, BE_INTERRUPT_STATUS_REG, status);
	batch = lpb_rd(l, BE_BATCH_STATUS_REG);
	block = lpb_rd(l, BE_STATUS_REG);
	done = (u8)batch;
	started = (u8)(batch >> 8);

	spin_lock(&l->lock);
	if (l->running && l->done != done) {
		lpb_finish(l, l->running, 0, ns, batch, status, block);
		l->running = NULL;
		l->done++;
	}
	if (l->started != started) {
		l->started++;
		can_queue = true;
		if (l->done != done && l->queued) {
			lpb_finish(l, l->queued, 0, ns, batch, status, block);
			l->done++;
		} else {
			l->running = l->queued;
		}
		l->queued = NULL;
	}
	if (l->done != done || l->started != started) {
		dev_dbg(l->dev, "counters: done %u (hw %u), started %u (hw %u)\n",
			l->done, done, l->started, started);
		l->started = started;
		l->done = done;
	}
	if (can_queue)
		l->hw_busy = false;
	lpb_schedule(l);
	if (!l->queued && !l->running)
		cancel_delayed_work(&l->watchdog);
	else
		mod_delayed_work(l->wq, &l->watchdog, msecs_to_jiffies(LPB_TIMEOUT_MS));
	spin_unlock(&l->lock);

	wake_up_interruptible(&l->wait);
	return IRQ_HANDLED;
}

/*
 * A job outstanding for LPB_TIMEOUT_MS: the block stopped. It cannot be reset
 * from Linux, so its jobs end with -ETIMEDOUT, nothing more is queued, and
 * their mappings stay until a recovery revives the block (the block may still
 * hold the addresses).
 */
static void lpb_watchdog(struct work_struct *work)
{
	struct lpb_dev *l = container_of(to_delayed_work(work), struct lpb_dev, watchdog);
	u32 batch = lpb_rd(l, BE_BATCH_STATUS_REG);
	u32 block = lpb_rd(l, BE_STATUS_REG);
	u64 ns = ktime_get_ns();
	unsigned long flags;

	spin_lock_irqsave(&l->lock, flags);
	if (l->running || l->queued) {
		dev_err(l->dev, "job timed out: status 0x%08x, batch 0x%08x; block stopped\n",
			block, batch);
		l->dead = true;
		if (l->running)
			lpb_finish(l, l->running, -ETIMEDOUT, ns, batch, 0, block);
		if (l->queued)
			lpb_finish(l, l->queued, -ETIMEDOUT, ns, batch, 0, block);
		l->running = NULL;
		l->queued = NULL;
	}
	spin_unlock_irqrestore(&l->lock, flags);
	wake_up_interruptible(&l->wait);
}

/* map_lock held. */
static void lpb_unmap(struct lpb_map *m)
{
	if (!m->buf)
		return;
	if (m->sgt)
		dma_buf_unmap_attachment_unlocked(m->att, m->sgt, DMA_BIDIRECTIONAL);
	if (m->att)
		dma_buf_detach(m->buf, m->att);
	dma_buf_put(m->buf);
	memset(m, 0, sizeof(*m));
}

/* The cache entry of `fd`'s dma-buf, mapped, with one more user. map_lock held. */
static struct lpb_map *lpb_get(struct lpb_dev *l, int fd)
{
	struct lpb_map *m = NULL, *victim = NULL;
	struct dma_buf *buf = dma_buf_get(fd);
	int ret;

	if (IS_ERR(buf))
		return ERR_CAST(buf);
	for (unsigned int i = 0; i < LPB_CACHE; i++) {
		struct lpb_map *e = &l->cache[i];

		if (e->buf == buf) {
			dma_buf_put(buf);	/* the entry holds its own reference */
			m = e;
			goto hit;
		}
		if (!e->buf) {
			if (!victim || victim->buf)
				victim = e;
		} else if (!e->users && (!victim || (victim->buf && e->used < victim->used))) {
			victim = e;
		}
	}
	if (!victim) {
		dma_buf_put(buf);
		return ERR_PTR(-EBUSY);	/* every entry is in a job */
	}
	lpb_unmap(victim);
	m = victim;
	m->buf = buf;
	m->att = dma_buf_attach(buf, l->dev);
	if (IS_ERR(m->att)) {
		ret = PTR_ERR(m->att);
		m->att = NULL;
		goto fail;
	}
	m->sgt = dma_buf_map_attachment_unlocked(m->att, DMA_BIDIRECTIONAL);
	if (IS_ERR(m->sgt)) {
		ret = PTR_ERR(m->sgt);
		m->sgt = NULL;
		goto fail;
	}
	/* The IOMMU gives one contiguous IOVA range; the block needs one. */
	if (m->sgt->nents != 1 || sg_dma_len(m->sgt->sgl) < buf->size) {
		ret = -EINVAL;
		goto fail;
	}
	m->addr = sg_dma_address(m->sgt->sgl);
	m->size = buf->size;
hit:
	m->users++;
	m->used = ++l->use_clock;
	return m;
fail:
	lpb_unmap(m);
	return ERR_PTR(ret);
}

/* Drops a job's planes; `keep`: a stopped block may still hold them. */
static void lpb_release_job(struct lpb_dev *l, struct lpb_job *job, bool keep)
{
	mutex_lock(&l->map_lock);
	for (unsigned int p = 0; p < LEMNOS_PISP_BE_PLANES; p++) {
		if (job->map[p] && !keep)
			job->map[p]->users--;
		job->map[p] = NULL;
	}
	mutex_unlock(&l->map_lock);
	memset(job->addr, 0, sizeof(job->addr));
}

/* Maps plane `p` and checks it holds `need` bytes from its offset. map_lock held. */
static int lpb_map_plane(struct lpb_dev *l, struct lpb_job *job, unsigned int p,
			 const struct lemnos_pisp_be_plane *plane, u64 need)
{
	struct lpb_map *m;

	if (!need || (plane->offset & 15))
		return -EINVAL;
	m = lpb_get(l, plane->fd);
	if (IS_ERR(m))
		return PTR_ERR(m);
	job->map[p] = m;
	if ((u64)plane->offset + need > m->size)
		return -EINVAL;
	job->addr[p] = m->addr + plane->offset;
	job->len[p] = m->size - plane->offset;
	return 0;
}

/* Bytes of plane `p` (0 to 2) of an image in `f`, or 0 when it has none. */
static u64 lpb_plane_bytes(const struct pisp_image_format_config *f, unsigned int p)
{
	u32 planarity = f->format & PISP_IMAGE_FORMAT_PLANARITY_MASK;
	u64 rows = f->height;

	if (p == 0)
		return (u64)f->stride * rows;
	if (planarity == PISP_IMAGE_FORMAT_PLANARITY_INTERLEAVED)
		return 0;
	if (planarity == PISP_IMAGE_FORMAT_PLANARITY_SEMI_PLANAR && p == 2)
		return 0;
	if (f->format & PISP_IMAGE_FORMAT_SAMPLING_420)
		rows = (rows + 1) / 2;
	return (u64)f->stride2 * rows;
}

static int lpb_check_format(const struct pisp_image_format_config *f)
{
	if (f->stride < 0 || f->stride2 < 0)
		return -EINVAL;	/* negative strides (flips) are not taken yet */
	if (f->format & PISP_IMAGE_FORMAT_WALLPAPER_ROLL)
		return -EOPNOTSUPP;
	return 0;
}

/*
 * The enables as pispbe_xlate_addrs sanitises them, from which planes the
 * caller gave, then every plane an enabled stage uses mapped and sized.
 */
static int lpb_prepare(struct lpb_dev *l, struct lpb_job *job,
		       const struct lemnos_pisp_be_submit *req)
{
	const struct pisp_be_config *c = &job->cfg->config;
	const struct lemnos_pisp_be_plane *pl = req->planes;
	u32 bayer = c->global.bayer_enables, rgb = c->global.rgb_enables;
	const struct pisp_image_format_config *fmt[LEMNOS_PISP_BE_PLANES] = {};
	unsigned int sub[LEMNOS_PISP_BE_PLANES] = {};
	bool has[LEMNOS_PISP_BE_PLANES];
	int ret;

	if (!(bayer & PISP_BE_BAYER_ENABLE_INPUT) == !(rgb & PISP_BE_RGB_ENABLE_INPUT))
		return -EINVAL;	/* exactly one input */
	if (job->cfg->num_tiles == 0 || job->cfg->num_tiles > PISP_BACK_END_NUM_TILES)
		return -EINVAL;
	for (unsigned int p = 0; p < LEMNOS_PISP_BE_PLANES; p++)
		has[p] = pl[p].fd >= 0;
	if (!has[0])
		return -EINVAL;

	if (bayer & PISP_BE_BAYER_ENABLE_INPUT) {
		if (!has[3] || !(bayer & PISP_BE_BAYER_ENABLE_TDN_INPUT) ||
		    !(bayer & PISP_BE_BAYER_ENABLE_TDN) || (c->tdn.reset & 1)) {
			bayer &= ~(PISP_BE_BAYER_ENABLE_TDN_INPUT |
				   PISP_BE_BAYER_ENABLE_TDN_DECOMPRESS);
			if (!(c->tdn.reset & 1))
				bayer &= ~PISP_BE_BAYER_ENABLE_TDN;
		}
		if (!has[4] || !(bayer & PISP_BE_BAYER_ENABLE_STITCH_INPUT) ||
		    !(bayer & PISP_BE_BAYER_ENABLE_STITCH))
			bayer &= ~(PISP_BE_BAYER_ENABLE_STITCH_INPUT |
				   PISP_BE_BAYER_ENABLE_STITCH_DECOMPRESS |
				   PISP_BE_BAYER_ENABLE_STITCH);
		if (!has[5])
			bayer &= ~(PISP_BE_BAYER_ENABLE_TDN_COMPRESS |
				   PISP_BE_BAYER_ENABLE_TDN_OUTPUT);
		if (!has[6])
			bayer &= ~(PISP_BE_BAYER_ENABLE_STITCH_COMPRESS |
				   PISP_BE_BAYER_ENABLE_STITCH_OUTPUT);
	} else {
		bayer = 0;	/* no Bayer input: the whole Bayer pipe off, else lockup */
	}
	for (unsigned int o = 0; o < PISP_BACK_END_NUM_OUTPUTS; o++)
		if (!has[LEMNOS_PISP_BE_PLANE_OUTPUT(o)])
			rgb &= ~(PISP_BE_RGB_ENABLE_OUTPUT0 << o);

	/* Which format sizes each plane, for the planes the job uses. */
	for (unsigned int p = 0; p < 3; p++) {
		fmt[p] = &c->input_format;
		sub[p] = p;
	}
	if (bayer & PISP_BE_BAYER_ENABLE_TDN_INPUT)
		fmt[3] = &c->tdn_input_format;
	if (bayer & PISP_BE_BAYER_ENABLE_STITCH_INPUT)
		fmt[4] = &c->stitch_input_format;
	if (bayer & PISP_BE_BAYER_ENABLE_TDN_OUTPUT)
		fmt[5] = &c->tdn_output_format;
	if (bayer & PISP_BE_BAYER_ENABLE_STITCH_OUTPUT)
		fmt[6] = &c->stitch_output_format;
	for (unsigned int o = 0; o < PISP_BACK_END_NUM_OUTPUTS; o++) {
		if (!(rgb & (PISP_BE_RGB_ENABLE_OUTPUT0 << o)))
			continue;
		for (unsigned int p = 0; p < 3; p++) {
			fmt[LEMNOS_PISP_BE_PLANE_OUTPUT(o) + p] = &c->output_format[o].image;
			sub[LEMNOS_PISP_BE_PLANE_OUTPUT(o) + p] = p;
		}
	}

	for (unsigned int p = 0; p < LEMNOS_PISP_BE_PLANES; p++) {
		u64 need;

		if (!fmt[p])
			continue;
		ret = lpb_check_format(fmt[p]);
		if (ret)
			return ret;
		need = lpb_plane_bytes(fmt[p], sub[p]);
		if (!need)
			continue;	/* a plane the format does not have */
		if (!has[p])
			return -EINVAL;	/* a plane the format has, not given */
		mutex_lock(&l->map_lock);
		ret = lpb_map_plane(l, job, p, &pl[p], need);
		mutex_unlock(&l->map_lock);
		if (ret)
			return ret;
	}

	/* Every tile's offsets inside their planes. */
	for (unsigned int t = 0; t < job->cfg->num_tiles; t++) {
		const struct pisp_tile *tile = &job->cfg->tiles[t];
		struct { unsigned int plane; u32 off; } offs[] = {
			{ 0, tile->input_addr_offset },
			{ 1, tile->input_addr_offset2 },
			{ 3, tile->tdn_input_addr_offset },
			{ 4, tile->stitch_input_addr_offset },
			{ 5, tile->tdn_output_addr_offset },
			{ 6, tile->stitch_output_addr_offset },
			{ 7, tile->output_addr_offset[0] },
			{ 8, tile->output_addr_offset2[0] },
			{ 10, tile->output_addr_offset[1] },
			{ 11, tile->output_addr_offset2[1] },
		};

		for (unsigned int i = 0; i < ARRAY_SIZE(offs); i++) {
			unsigned int p = offs[i].plane;

			if (job->map[p] && offs[i].off >= job->len[p])
				return -EINVAL;
		}
	}

	if (test_raw_enables) {
		dev_warn(l->dev, "test: queueing unsanitised enables\n");
		bayer = c->global.bayer_enables;
		rgb = c->global.rgb_enables;
	}
	job->bayer_en = bayer;
	job->rgb_en = rgb;
	return 0;
}

static long lpb_submit(struct lpb_dev *l, void __user *arg)
{
	struct lemnos_pisp_be_submit req;
	struct lpb_job *job = NULL;
	unsigned long flags;
	int ret;

	if (copy_from_user(&req, arg, sizeof(req)))
		return -EFAULT;

	spin_lock_irqsave(&l->lock, flags);
	if (l->dead) {
		spin_unlock_irqrestore(&l->lock, flags);
		return -EIO;
	}
	for (unsigned int s = 0; s < LPB_SLOTS; s++) {
		if (l->slot[s].state == SLOT_FREE) {
			job = &l->slot[s];
			job->state = SLOT_PENDING;	/* reserved */
			break;
		}
	}
	spin_unlock_irqrestore(&l->lock, flags);
	if (!job)
		return -EBUSY;	/* read the finished jobs first */

	/* The block reads the tiles from here; user space never sees it. */
	if (copy_from_user(job->cfg, u64_to_user_ptr(req.config), sizeof(*job->cfg))) {
		ret = -EFAULT;
		goto fail;
	}
	job->tag = req.tag;
	ret = lpb_prepare(l, job, &req);
	if (ret)
		goto fail;

	spin_lock_irqsave(&l->lock, flags);
	if (l->dead) {
		spin_unlock_irqrestore(&l->lock, flags);
		ret = -EIO;
		goto fail;
	}
	list_add_tail(&job->node, &l->pending);
	lpb_schedule(l);
	spin_unlock_irqrestore(&l->lock, flags);

	if (test_unmap_plane >= 0 && test_unmap_plane < LEMNOS_PISP_BE_PLANES) {
		dev_warn(l->dev, "test: unmapping plane %d of a queued job\n", test_unmap_plane);
		mutex_lock(&l->map_lock);
		if (job->map[test_unmap_plane]) {
			lpb_unmap(job->map[test_unmap_plane]);
			job->map[test_unmap_plane] = NULL;
		}
		mutex_unlock(&l->map_lock);
	}
	return 0;

fail:
	lpb_release_job(l, job, false);
	spin_lock_irqsave(&l->lock, flags);
	job->state = SLOT_FREE;
	spin_unlock_irqrestore(&l->lock, flags);
	return ret;
}

static long lpb_recover(struct lpb_dev *l, void __user *arg)
{
	struct lemnos_pisp_be_recover r;
	unsigned long flags, rate;
	u32 batch;

	if (copy_from_user(&r, arg, sizeof(r)))
		return -EFAULT;
	switch (r.mode) {
	case LEMNOS_PISP_BE_RECOVER_AXI:
		lpb_wr(l, BE_AXI_REG, BE_AXI_VALUE);
		lpb_wr(l, BE_INTERRUPT_STATUS_REG, 0xffffffffu);
		break;
	case LEMNOS_PISP_BE_RECOVER_CLOCK:
		rate = clk_get_rate(l->clk);
		if (!rate || clk_set_rate(l->clk, rate / 2))
			return -EOPNOTSUPP;
		msleep(10);
		clk_set_rate(l->clk, rate);
		break;
	default:
		return -EINVAL;
	}
	msleep(10);
	r.status = lpb_rd(l, BE_STATUS_REG);
	batch = lpb_rd(l, BE_BATCH_STATUS_REG);
	r.batch_status = batch;
	r.revived = r.status == 0 && (u8)batch == (u8)(batch >> 8);
	if (r.revived) {
		spin_lock_irqsave(&l->lock, flags);
		l->done = (u8)batch;
		l->started = (u8)(batch >> 8);
		l->hw_busy = false;
		l->dead = false;
		spin_unlock_irqrestore(&l->lock, flags);
	}
	if (copy_to_user(arg, &r, sizeof(r)))
		return -EFAULT;
	return 0;
}

static long lpb_ioctl(struct file *f, unsigned int cmd, unsigned long arg)
{
	struct lpb_dev *l = f->private_data;
	void __user *uarg = (void __user *)arg;

	switch (cmd) {
	case LEMNOS_PISP_BE_IOC_INFO: {
		struct lemnos_pisp_be_info info = {
			.abi = LEMNOS_PISP_BE_ABI,
			.hw_version = l->hw_version,
			.queue_depth = LPB_SLOTS,
			.dead = READ_ONCE(l->dead),
		};

		return copy_to_user(uarg, &info, sizeof(info)) ? -EFAULT : 0;
	}
	case LEMNOS_PISP_BE_IOC_SUBMIT:
		return lpb_submit(l, uarg);
	case LEMNOS_PISP_BE_IOC_RECOVER:
		return lpb_recover(l, uarg);
	default:
		return -ENOTTY;
	}
}

static bool lpb_has_finished(struct lpb_dev *l)
{
	unsigned long flags;
	bool any;

	spin_lock_irqsave(&l->lock, flags);
	any = !list_empty(&l->finished);
	spin_unlock_irqrestore(&l->lock, flags);
	return any;
}

/* One or more lemnos_pisp_be_done records; blocks unless O_NONBLOCK. */
static ssize_t lpb_read(struct file *f, char __user *buf, size_t len, loff_t *pos)
{
	struct lpb_dev *l = f->private_data;
	size_t n = 0;

	if (len < sizeof(struct lemnos_pisp_be_done))
		return -EINVAL;
	if (!(f->f_flags & O_NONBLOCK)) {
		int ret = wait_event_interruptible(l->wait, lpb_has_finished(l));

		if (ret)
			return ret;
	}
	while (n + sizeof(struct lemnos_pisp_be_done) <= len) {
		struct lemnos_pisp_be_done rec;
		struct lpb_job *job;
		unsigned long flags;
		bool stopped;

		spin_lock_irqsave(&l->lock, flags);
		job = list_first_entry_or_null(&l->finished, struct lpb_job, node);
		if (job)
			list_del(&job->node);
		stopped = l->dead;
		spin_unlock_irqrestore(&l->lock, flags);
		if (!job)
			break;
		rec = job->done;
		/* A stopped block may still hold the addresses: keep its mappings. */
		lpb_release_job(l, job, stopped && rec.status);
		spin_lock_irqsave(&l->lock, flags);
		job->state = SLOT_FREE;
		spin_unlock_irqrestore(&l->lock, flags);
		if (copy_to_user(buf + n, &rec, sizeof(rec)))
			return n ? n : -EFAULT;
		n += sizeof(rec);
	}
	return n ? n : -EAGAIN;
}

static __poll_t lpb_poll(struct file *f, poll_table *wait)
{
	struct lpb_dev *l = f->private_data;

	poll_wait(f, &l->wait, wait);
	return lpb_has_finished(l) ? EPOLLIN | EPOLLRDNORM : 0;
}

static int lpb_open(struct inode *inode, struct file *f)
{
	struct lpb_dev *l = container_of(f->private_data, struct lpb_dev, misc);
	int ret = 0;

	mutex_lock(&l->open_lock);
	if (l->open)
		ret = -EBUSY;
	else
		l->open = true;
	mutex_unlock(&l->open_lock);
	f->private_data = l;
	return ret;
}

static int lpb_release(struct inode *inode, struct file *f)
{
	struct lpb_dev *l = f->private_data;
	unsigned long flags;
	bool busy;

	/* Let the block finish what it holds before its buffers go. */
	wait_event_timeout(l->wait, ({
		spin_lock_irqsave(&l->lock, flags);
		busy = !list_empty(&l->pending) || l->queued || l->running;
		spin_unlock_irqrestore(&l->lock, flags);
		!busy;
	}), msecs_to_jiffies(2 * LPB_TIMEOUT_MS));

	spin_lock_irqsave(&l->lock, flags);
	INIT_LIST_HEAD(&l->pending);
	INIT_LIST_HEAD(&l->finished);
	busy = l->queued || l->running || l->dead;
	spin_unlock_irqrestore(&l->lock, flags);
	for (unsigned int s = 0; s < LPB_SLOTS; s++) {
		/* A stopped block keeps its jobs' mappings, deliberately leaked. */
		lpb_release_job(l, &l->slot[s], busy && l->slot[s].state == SLOT_HW);
		l->slot[s].state = SLOT_FREE;
	}
	mutex_lock(&l->map_lock);
	for (unsigned int i = 0; i < LPB_CACHE; i++)
		if (!l->cache[i].users)
			lpb_unmap(&l->cache[i]);
	mutex_unlock(&l->map_lock);
	mutex_lock(&l->open_lock);
	l->open = false;
	mutex_unlock(&l->open_lock);
	return 0;
}

static const struct file_operations lpb_fops = {
	.owner = THIS_MODULE,
	.open = lpb_open,
	.release = lpb_release,
	.read = lpb_read,
	.poll = lpb_poll,
	.unlocked_ioctl = lpb_ioctl,
	.compat_ioctl = compat_ptr_ioctl,
	.llseek = noop_llseek,
};

/* pispbe_hw_init: a known version, and a block that is idle. */
static int lpb_hw_init(struct lpb_dev *l)
{
	u32 u = lpb_rd(l, BE_VERSION_REG);

	l->hw_version = u;
	if ((u & ~BE_VERSION_MINOR_BITS) != BE_VERSION_2712)
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

static void lpb_destroy_wq(void *wq)
{
	destroy_workqueue(wq);
}

static int lpb_probe(struct platform_device *pdev)
{
	struct device *dev = &pdev->dev;
	struct lpb_dev *l;
	int ret;

	l = devm_kzalloc(dev, sizeof(*l), GFP_KERNEL);
	if (!l)
		return -ENOMEM;
	l->dev = dev;
	spin_lock_init(&l->lock);
	mutex_init(&l->open_lock);
	mutex_init(&l->map_lock);
	init_waitqueue_head(&l->wait);
	INIT_LIST_HEAD(&l->pending);
	INIT_LIST_HEAD(&l->finished);
	INIT_DELAYED_WORK(&l->watchdog, lpb_watchdog);
	l->wq = alloc_ordered_workqueue(LPB_NAME, 0);
	if (!l->wq)
		return -ENOMEM;
	ret = devm_add_action_or_reset(dev, lpb_destroy_wq, l->wq);
	if (ret)
		return ret;
	platform_set_drvdata(pdev, l);

	l->regs = devm_platform_ioremap_resource(pdev, 0);
	if (IS_ERR(l->regs))
		return PTR_ERR(l->regs);
	l->irq = platform_get_irq(pdev, 0);
	if (l->irq <= 0)
		return -EINVAL;
	ret = dma_set_mask_and_coherent(dev, DMA_BIT_MASK(36));
	if (ret)
		return ret;
	/* The firmware's ISP clock, marked critical: enabling it is all it takes. */
	l->clk = devm_clk_get_enabled(dev, NULL);
	if (IS_ERR(l->clk))
		return dev_err_probe(dev, PTR_ERR(l->clk), "no clock\n");
	ret = lpb_hw_init(l);
	if (ret)
		return ret;

	for (unsigned int s = 0; s < LPB_SLOTS; s++) {
		l->slot[s].cfg = dmam_alloc_coherent(dev, sizeof(struct pisp_be_tiles_config),
						     &l->slot[s].cfg_dma, GFP_KERNEL);
		if (!l->slot[s].cfg)
			return -ENOMEM;
	}
	ret = devm_request_irq(dev, l->irq, lpb_isr, 0, LPB_NAME, l);
	if (ret)
		return ret;

	l->misc.minor = MISC_DYNAMIC_MINOR;
	l->misc.name = LPB_NAME;
	l->misc.fops = &lpb_fops;
	l->misc.parent = dev;
	ret = misc_register(&l->misc);
	if (ret)
		return ret;
	dev_info(dev, "PiSP back end 0x%08x on /dev/%s\n", l->hw_version, LPB_NAME);
	return 0;
}

static void lpb_remove(struct platform_device *pdev)
{
	struct lpb_dev *l = platform_get_drvdata(pdev);

	misc_deregister(&l->misc);
	lpb_wr(l, BE_INTERRUPT_EN_REG, 0);
	cancel_delayed_work_sync(&l->watchdog);
}

/* No of_match_table: bound only through driver_override. */
struct platform_driver lemnos_pisp_be_driver = {
	.probe = lpb_probe,
	.remove = lpb_remove,
	.driver = {
		.name = LPB_NAME,
	},
};

