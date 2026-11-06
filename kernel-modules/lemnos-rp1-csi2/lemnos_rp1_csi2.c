// SPDX-License-Identifier: GPL-2.0
/*
 * lemnos_rp1_csi2: the Raspberry Pi RP1 CSI-2 receiver as a character device.
 *
 * The D-PHY bring-up, the CSI-2 DMA channel's registers and the frame start
 * and end accounting are ported from the stock driver,
 * drivers/media/platform/raspberrypi/rp1_cfe/{dphy,csi2,cfe}.c:
 *   Copyright (C) 2021-2024 Raspberry Pi Ltd.
 * The rest: Copyright (c) 2026 the Lemnos authors.
 *
 * One CSI-2 DMA channel (0) writes raw frames into dma-bufs; the PiSP front
 * end is not used. A fast path, never a requirement: the module has no
 * of_match_table, so it binds only where driver_override names it, after
 * rp1-cfe is unbound. See kernel-modules/lemnos-rp1-csi2/README.md.
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
#include <linux/of.h>
#include <linux/of_graph.h>
#include <linux/platform_device.h>
#include <linux/poll.h>
#include <linux/scatterlist.h>
#include <linux/slab.h>
#include <linux/uaccess.h>
#include <linux/wait.h>

#include "lemnos_rp1_csi2.h"

#define LRC_NAME "lemnos-rp1-csi2"
#define LRC_CACHE 16
#define LRC_CH 0

/* DW CSI-2 host (resource 1), from dphy.c. */
#define HOST_VERSION		0x000
#define HOST_N_LANES		0x004
#define HOST_RESETN		0x008
#define HOST_PHY_SHUTDOWNZ	0x040
#define HOST_PHY_RSTZ		0x044
#define HOST_PHY_TST_CTRL0	0x050
#define HOST_PHY_TST_CTRL1	0x054
#define DPHY_HS_RX_CTRL_LANE0	0x44
#define DPHY_CLK_PN_SWAP	0x35
#define DPHY_D0_PN_SWAP		0x45
#define DPHY_D1_PN_SWAP		0x55
#define DPHY_D2_PN_SWAP		0x85
#define DPHY_D3_PN_SWAP		0x95

/* CSI-2 DMA (resource 0), from csi2.c. */
#define CSI2_STATUS		0x000
#define CSI2_IRQ_MASK		0x020
#define CSI2_CTRL		0x024
#define CSI2_CH_CTRL(x)		((x) * 0x40 + 0x28)
#define CSI2_CH_ADDR0(x)	((x) * 0x40 + 0x2c)
#define CSI2_CH_ADDR1(x)	((x) * 0x40 + 0x3c)
#define CSI2_CH_STRIDE(x)	((x) * 0x40 + 0x30)
#define CSI2_CH_LENGTH(x)	((x) * 0x40 + 0x34)
#define CSI2_CH_DEBUG(x)	((x) * 0x40 + 0x38)
#define CSI2_CH_FRAME_SIZE(x)	((x) * 0x40 + 0x40)
#define IRQ_FS(x)		(BIT(0) << (x))
#define IRQ_FE(x)		(BIT(4) << (x))
#define IRQ_FE_ACK(x)		(BIT(8) << (x))
#define IRQ_LE(x)		(BIT(12) << (x))
#define IRQ_LE_ACK(x)		(BIT(16) << (x))
#define IRQ_CH_MASK(x)		(IRQ_FS(x) | IRQ_FE(x) | IRQ_FE_ACK(x) | IRQ_LE(x) | IRQ_LE_ACK(x))
#define EOP_IS_EOL		BIT(0)
#define DMA_EN			BIT(0)
#define FORCE			BIT(3)
#define IRQ_EN_FS		BIT(13)
#define IRQ_EN_FE_ACK		BIT(15)
#define PACK_LINE		BIT(29)
#define PACK_BYTES		BIT(30)
#define VC_SHIFT		5
#define DT_SHIFT		7

/* MIPI config (resource 2), from cfe.c. */
#define MIPICFG_CFG		0x004
#define MIPICFG_INTE		0x02c
#define MIPICFG_INTS		0x034
#define MIPICFG_CFG_SEL_CSI	BIT(0)
#define MIPICFG_INT_CSI_DMA	BIT(0)

struct lrc_map {
	struct dma_buf *buf;		/* NULL: a free entry */
	struct dma_buf_attachment *att;
	struct sg_table *sgt;
	dma_addr_t addr;
	u64 size;
	unsigned int users;		/* buffers not yet read back */
	u64 used;
};

struct lrc_buf {
	struct list_head node;
	bool busy;			/* queued and not yet read */
	struct lrc_map *map;		/* under map_lock */
	dma_addr_t addr;
	struct lemnos_rp1_csi2_done done;
};

struct lrc_dev {
	struct device *dev;
	void __iomem *dma, *host, *cfg;
	struct clk *clk;
	int irq;
	u32 host_version, max_lanes;
	u8 polarity[5];			/* clock, then data lanes 0-3 */
	struct miscdevice misc;
	struct mutex open_lock;
	bool open;
	struct mutex ctl_lock;		/* START, QUEUE, STOP, close */

	struct mutex map_lock;
	struct lrc_map cache[LRC_CACHE];
	u64 use_clock;

	spinlock_t lock;		/* everything below */
	struct lrc_buf buf[LEMNOS_RP1_CSI2_DEPTH];
	struct list_head pending;	/* queued, not yet given to the channel */
	struct list_head finished;	/* done, not yet read */
	struct lrc_buf *next;		/* in the channel's shadow registers */
	struct lrc_buf *cur;		/* being written */
	bool streaming, fs_int, fe_int;
	struct lemnos_rp1_csi2_start start;
	u32 frames, dropped;
	wait_queue_head_t wait;
};

static u32 rd(void __iomem *base, unsigned int off)
{
	return readl(base + off);
}

static void wr(void __iomem *base, unsigned int off, u32 val)
{
	writel(val, base + off);
}

/* The D-PHY test interface (dphy.c). */
static void tst_ctrl0(struct lrc_dev *l, u32 mask, u32 val)
{
	wr(l->host, HOST_PHY_TST_CTRL0, (rd(l->host, HOST_PHY_TST_CTRL0) & ~mask) | val);
}

static void tst_ctrl1(struct lrc_dev *l, u32 mask, u32 val)
{
	wr(l->host, HOST_PHY_TST_CTRL1, (rd(l->host, HOST_PHY_TST_CTRL1) & ~mask) | val);
}

static void dphy_transaction(struct lrc_dev *l, u8 code, u8 data)
{
	tst_ctrl0(l, 2, 2);			/* tstclk 1 */
	tst_ctrl1(l, BIT(16), 0);		/* testen 0 */
	tst_ctrl1(l, 0xff, code);
	tst_ctrl1(l, BIT(16), BIT(16));
	tst_ctrl0(l, 2, 0);
	tst_ctrl1(l, BIT(16), 0);
	tst_ctrl1(l, 0xff, data);
	tst_ctrl0(l, 2, 2);
}

static u8 hsfreqrange(u32 mbps)
{
	/* The D-PHY databook's table 5-1, as dphy.c has it. */
	static const u16 table[][2] = {
		{ 89, 0x00 }, { 99, 0x10 }, { 109, 0x20 }, { 129, 0x01 },
		{ 139, 0x11 }, { 149, 0x21 }, { 169, 0x02 }, { 179, 0x12 },
		{ 199, 0x22 }, { 219, 0x03 }, { 239, 0x13 }, { 249, 0x23 },
		{ 269, 0x04 }, { 299, 0x14 }, { 329, 0x05 }, { 359, 0x15 },
		{ 399, 0x25 }, { 449, 0x06 }, { 499, 0x16 }, { 549, 0x07 },
		{ 599, 0x17 }, { 649, 0x08 }, { 699, 0x18 }, { 749, 0x09 },
		{ 799, 0x19 }, { 849, 0x29 }, { 899, 0x39 }, { 949, 0x0a },
		{ 999, 0x1a }, { 1049, 0x2a }, { 1099, 0x3a }, { 1149, 0x0b },
		{ 1199, 0x1b }, { 1249, 0x2b }, { 1299, 0x3b }, { 1349, 0x0c },
		{ 1399, 0x1c }, { 1449, 0x2c }, { 1500, 0x3c },
	};
	unsigned int i;

	for (i = 0; i < ARRAY_SIZE(table) - 1; i++)
		if (mbps <= table[i][0])
			break;
	return table[i][1];
}

/* dphy_start: the host in reset, the lanes, the PHY's rate and polarities. */
static void dphy_start(struct lrc_dev *l, u32 lanes, u32 mbps)
{
	wr(l->host, HOST_RESETN, 0);
	wr(l->host, HOST_N_LANES, lanes - 1);
	wr(l->host, HOST_PHY_RSTZ, 0);
	wr(l->host, HOST_PHY_SHUTDOWNZ, 0);
	tst_ctrl0(l, 2, 2);
	tst_ctrl1(l, BIT(16), 0);
	tst_ctrl0(l, 1, 1);			/* tstclr */
	usleep_range(15, 20);
	tst_ctrl0(l, 1, 0);
	usleep_range(15, 20);
	dphy_transaction(l, DPHY_HS_RX_CTRL_LANE0, hsfreqrange(mbps) << 1);
	dphy_transaction(l, DPHY_CLK_PN_SWAP, l->polarity[0]);
	dphy_transaction(l, DPHY_D0_PN_SWAP, l->polarity[1]);
	dphy_transaction(l, DPHY_D1_PN_SWAP, l->polarity[2]);
	dphy_transaction(l, DPHY_D2_PN_SWAP, l->polarity[3]);
	dphy_transaction(l, DPHY_D3_PN_SWAP, l->polarity[4]);
	usleep_range(5, 10);
	wr(l->host, HOST_PHY_SHUTDOWNZ, 1);
	usleep_range(5, 10);
	wr(l->host, HOST_PHY_RSTZ, 1);
	wr(l->host, HOST_RESETN, 0xffffffff);
	usleep_range(10, 50);
}

/* csi2_set_buffer: ADDR0 last, it latches the others for the next frame. Lock held. */
static void lrc_set_buffer(struct lrc_dev *l, dma_addr_t dma)
{
	u64 addr = (u64)dma >> 4;

	wr(l->dma, CSI2_CH_LENGTH(LRC_CH), l->start.size >> 4);
	wr(l->dma, CSI2_CH_STRIDE(LRC_CH), l->start.stride >> 4);
	wr(l->dma, CSI2_CH_ADDR1(LRC_CH), addr >> 32);
	wr(l->dma, CSI2_CH_ADDR0(LRC_CH), addr & 0xffffffff);
}

/* Gives the channel the next queued buffer when it has none waiting. Lock held. */
static void lrc_arm(struct lrc_dev *l)
{
	struct lrc_buf *b;

	if (!l->streaming || l->next)
		return;
	b = list_first_entry_or_null(&l->pending, struct lrc_buf, node);
	if (!b)
		return;
	list_del(&b->node);
	l->next = b;
	lrc_set_buffer(l, b->addr);
}

/* Lock held. */
static void lrc_finish(struct lrc_dev *l, struct lrc_buf *b, s32 status)
{
	b->done.status = status;
	list_add_tail(&b->node, &l->finished);
	wake_up_interruptible(&l->wait);
}

/* cfe_sof_isr_handler, one channel. Lock held. */
static void lrc_sof(struct lrc_dev *l, u64 now)
{
	if (l->cur)			/* a start before the last frame's end */
		lrc_finish(l, l->cur, -EIO);
	l->cur = l->next;
	l->next = NULL;
	if (l->cur) {
		l->cur->done.fs_ns = now;
		l->cur->done.sequence = l->frames;
	} else {
		l->dropped++;
	}
	l->frames++;
	l->fs_int = true;
	l->fe_int = false;
	lrc_arm(l);
}

/* cfe_eof_isr_handler. Lock held. */
static void lrc_eof(struct lrc_dev *l, u64 now)
{
	if (l->cur) {
		l->cur->done.fe_ns = now;
		lrc_finish(l, l->cur, 0);
	}
	l->cur = NULL;
	l->fe_int = true;
	l->fs_int = false;
}

static irqreturn_t lrc_isr(int irq, void *data)
{
	struct lrc_dev *l = data;
	u64 now = ktime_get_ns();
	bool sof, eof;
	u32 sts, status;

	sts = rd(l->cfg, MIPICFG_INTS);
	if (!(sts & MIPICFG_INT_CSI_DMA))
		return IRQ_NONE;
	status = rd(l->dma, CSI2_STATUS);
	wr(l->dma, CSI2_STATUS, status);	/* write back to clear */
	sof = status & IRQ_FS(LRC_CH);
	eof = status & IRQ_FE_ACK(LRC_CH);

	spin_lock(&l->lock);
	if (l->streaming) {
		/* The three orders cfe_isr handles: FE then FS of the next frame,
		 * FS and FE of one frame, and a missed FE before an FS. */
		if (eof) {
			if (sof && !l->fs_int) {
				lrc_sof(l, now);
				sof = false;
			}
			lrc_eof(l, now);
		}
		if (sof) {
			if (l->fs_int && !l->fe_int)
				lrc_eof(l, now);
			lrc_sof(l, now);
		}
	}
	spin_unlock(&l->lock);
	return IRQ_HANDLED;
}

/* map_lock held. */
static void lrc_unmap(struct lrc_map *m)
{
	if (!m->buf)
		return;
	if (m->sgt)
		dma_buf_unmap_attachment_unlocked(m->att, m->sgt, DMA_FROM_DEVICE);
	if (m->att)
		dma_buf_detach(m->buf, m->att);
	dma_buf_put(m->buf);
	memset(m, 0, sizeof(*m));
}

/* The cache entry of `fd`'s dma-buf, mapped, with one more user. map_lock held. */
static struct lrc_map *lrc_get(struct lrc_dev *l, int fd)
{
	struct lrc_map *m, *victim = NULL;
	struct dma_buf *buf = dma_buf_get(fd);
	int ret;

	if (IS_ERR(buf))
		return ERR_CAST(buf);
	for (unsigned int i = 0; i < LRC_CACHE; i++) {
		struct lrc_map *e = &l->cache[i];

		if (e->buf == buf) {
			dma_buf_put(buf);
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
		return ERR_PTR(-EBUSY);
	}
	lrc_unmap(victim);
	m = victim;
	m->buf = buf;
	m->att = dma_buf_attach(buf, l->dev);
	if (IS_ERR(m->att)) {
		ret = PTR_ERR(m->att);
		m->att = NULL;
		goto fail;
	}
	m->sgt = dma_buf_map_attachment_unlocked(m->att, DMA_FROM_DEVICE);
	if (IS_ERR(m->sgt)) {
		ret = PTR_ERR(m->sgt);
		m->sgt = NULL;
		goto fail;
	}
	/* No IOMMU in front of RP1: the channel writes one contiguous range. */
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
	lrc_unmap(m);
	return ERR_PTR(ret);
}

static void lrc_put(struct lrc_dev *l, struct lrc_buf *b)
{
	mutex_lock(&l->map_lock);
	if (b->map)
		b->map->users--;
	b->map = NULL;
	mutex_unlock(&l->map_lock);
}

/* csi2_stop_channel and csi2_close_rx, then every buffer back. ctl_lock held. */
static void lrc_stop(struct lrc_dev *l)
{
	struct lrc_buf *b, *tmp;
	unsigned long flags;

	spin_lock_irqsave(&l->lock, flags);
	if (!l->streaming) {
		spin_unlock_irqrestore(&l->lock, flags);
		return;
	}
	l->streaming = false;
	/* FORCE stops mid-frame; ADDR0 latches it, twice as the hardware needs. */
	wr(l->dma, CSI2_CH_CTRL(LRC_CH), FORCE);
	wr(l->dma, CSI2_CH_ADDR0(LRC_CH), 0);
	wr(l->dma, CSI2_CH_ADDR0(LRC_CH), 0);
	wr(l->dma, CSI2_IRQ_MASK, 0);
	wr(l->cfg, MIPICFG_INTE, 0);
	if (l->cur)
		lrc_finish(l, l->cur, -ECANCELED);
	if (l->next)
		lrc_finish(l, l->next, -ECANCELED);
	l->cur = l->next = NULL;
	list_for_each_entry_safe(b, tmp, &l->pending, node) {
		list_del(&b->node);
		lrc_finish(l, b, -ECANCELED);
	}
	spin_unlock_irqrestore(&l->lock, flags);
	/* The D-PHY is left out of reset, as rp1-cfe leaves it: resetting the host
	 * mid-packet can lose the next stream's first packet. */
}

static long lrc_start(struct lrc_dev *l, void __user *arg)
{
	struct lemnos_rp1_csi2_start s;
	unsigned long flags;
	u32 ctrl;

	if (copy_from_user(&s, arg, sizeof(s)))
		return -EFAULT;
	if (s.lanes < 1 || s.lanes > l->max_lanes || s.mbps < 80 || s.mbps > 1500 ||
	    s.vc > 3 || s.dt < 1 || s.dt > 63 || (s.stride & 15) || (s.size & 15) ||
	    !s.size || (s.flags & ~LEMNOS_RP1_CSI2_PACK_BYTES) ||
	    s.width > 0xffff || s.height > 0xffff || !s.width != !s.height ||
	    s.reserved[0] || s.reserved[1] || s.reserved[2])
		return -EINVAL;
	if (s.width && !(s.flags & LEMNOS_RP1_CSI2_PACK_BYTES) &&
	    (u64)s.stride * s.height > s.size)
		return -EINVAL;
	spin_lock_irqsave(&l->lock, flags);
	if (l->streaming) {
		spin_unlock_irqrestore(&l->lock, flags);
		return -EBUSY;
	}
	l->start = s;
	l->frames = l->dropped = 0;
	l->fs_int = l->fe_int = false;
	spin_unlock_irqrestore(&l->lock, flags);

	/* csi2_start_channel, without the V4L2 frame descriptor: the caller's vc and dt. */
	wr(l->dma, CSI2_CH_CTRL(LRC_CH), 0);
	wr(l->dma, CSI2_CH_DEBUG(LRC_CH), 0);
	wr(l->dma, CSI2_STATUS, IRQ_CH_MASK(LRC_CH));
	ctrl = DMA_EN | IRQ_EN_FS | IRQ_EN_FE_ACK | PACK_LINE;
	if (s.flags & LEMNOS_RP1_CSI2_PACK_BYTES)
		ctrl |= PACK_BYTES;
	wr(l->dma, CSI2_CH_FRAME_SIZE(LRC_CH), (s.height << 16) | s.width);
	ctrl |= (s.vc << VC_SHIFT) | (s.dt << DT_SHIFT);
	wr(l->dma, CSI2_CH_CTRL(LRC_CH), ctrl);

	spin_lock_irqsave(&l->lock, flags);
	l->streaming = true;
	lrc_arm(l);
	spin_unlock_irqrestore(&l->lock, flags);

	/* cfe_start_streaming's order: MIPI config, then csi2_open_rx. */
	wr(l->cfg, MIPICFG_CFG, MIPICFG_CFG_SEL_CSI);
	wr(l->cfg, MIPICFG_INTE, MIPICFG_INT_CSI_DMA);
	wr(l->dma, CSI2_IRQ_MASK, 0);
	dphy_start(l, s.lanes, s.mbps);
	wr(l->dma, CSI2_CTRL, EOP_IS_EOL);
	return 0;
}

static long lrc_queue(struct lrc_dev *l, void __user *arg)
{
	struct lemnos_rp1_csi2_queue q;
	struct lrc_buf *b = NULL;
	struct lrc_map *m;
	unsigned long flags;
	u32 size;

	if (copy_from_user(&q, arg, sizeof(q)))
		return -EFAULT;
	if (q.offset & 15)
		return -EINVAL;
	spin_lock_irqsave(&l->lock, flags);
	size = l->start.size;
	for (unsigned int i = 0; i < LEMNOS_RP1_CSI2_DEPTH; i++) {
		if (!l->buf[i].busy) {
			b = &l->buf[i];
			b->busy = true;
			break;
		}
	}
	spin_unlock_irqrestore(&l->lock, flags);
	if (!size) {
		if (b)
			b->busy = false;
		return -ENODATA;	/* START first: it sets the frame's size */
	}
	if (!b)
		return -EBUSY;

	mutex_lock(&l->map_lock);
	m = lrc_get(l, q.fd);
	mutex_unlock(&l->map_lock);
	if (IS_ERR(m)) {
		b->busy = false;
		return PTR_ERR(m);
	}
	if ((u64)q.offset + size > m->size) {
		b->map = m;
		lrc_put(l, b);
		b->busy = false;
		return -EINVAL;
	}
	b->map = m;
	b->addr = m->addr + q.offset;
	memset(&b->done, 0, sizeof(b->done));
	b->done.tag = q.tag;

	spin_lock_irqsave(&l->lock, flags);
	if (!l->streaming) {
		spin_unlock_irqrestore(&l->lock, flags);
		lrc_put(l, b);
		b->busy = false;
		return -ENODATA;
	}
	list_add_tail(&b->node, &l->pending);
	lrc_arm(l);
	spin_unlock_irqrestore(&l->lock, flags);
	return 0;
}

static long lrc_info(struct lrc_dev *l, void __user *arg)
{
	struct lemnos_rp1_csi2_info i = {
		.abi = LEMNOS_RP1_CSI2_ABI,
		.host_version = l->host_version,
		.max_lanes = l->max_lanes,
		.depth = LEMNOS_RP1_CSI2_DEPTH,
	};
	unsigned long flags;

	spin_lock_irqsave(&l->lock, flags);
	i.streaming = l->streaming;
	i.frames = l->frames;
	i.dropped = l->dropped;
	spin_unlock_irqrestore(&l->lock, flags);
	return copy_to_user(arg, &i, sizeof(i)) ? -EFAULT : 0;
}

static long lrc_ioctl(struct file *f, unsigned int cmd, unsigned long arg)
{
	struct lrc_dev *l = f->private_data;
	void __user *p = (void __user *)arg;
	long ret;

	if (cmd == LEMNOS_RP1_CSI2_IOC_INFO)
		return lrc_info(l, p);
	mutex_lock(&l->ctl_lock);
	switch (cmd) {
	case LEMNOS_RP1_CSI2_IOC_START:
		ret = lrc_start(l, p);
		break;
	case LEMNOS_RP1_CSI2_IOC_QUEUE:
		ret = lrc_queue(l, p);
		break;
	case LEMNOS_RP1_CSI2_IOC_STOP:
		lrc_stop(l);
		ret = 0;
		break;
	default:
		ret = -ENOTTY;
	}
	mutex_unlock(&l->ctl_lock);
	return ret;
}

static bool lrc_has_finished(struct lrc_dev *l)
{
	unsigned long flags;
	bool any;

	spin_lock_irqsave(&l->lock, flags);
	any = !list_empty(&l->finished);
	spin_unlock_irqrestore(&l->lock, flags);
	return any;
}

/* One or more lemnos_rp1_csi2_done records; blocks unless O_NONBLOCK. */
static ssize_t lrc_read(struct file *f, char __user *buf, size_t len, loff_t *pos)
{
	struct lrc_dev *l = f->private_data;
	size_t n = 0;

	if (len < sizeof(struct lemnos_rp1_csi2_done))
		return -EINVAL;
	if (!(f->f_flags & O_NONBLOCK)) {
		int ret = wait_event_interruptible(l->wait, lrc_has_finished(l));

		if (ret)
			return ret;
	}
	while (n + sizeof(struct lemnos_rp1_csi2_done) <= len) {
		struct lemnos_rp1_csi2_done rec;
		struct lrc_buf *b;
		unsigned long flags;

		spin_lock_irqsave(&l->lock, flags);
		b = list_first_entry_or_null(&l->finished, struct lrc_buf, node);
		if (b)
			list_del(&b->node);
		spin_unlock_irqrestore(&l->lock, flags);
		if (!b)
			break;
		rec = b->done;
		lrc_put(l, b);
		b->busy = false;
		if (copy_to_user(buf + n, &rec, sizeof(rec)))
			return n ? n : -EFAULT;
		n += sizeof(rec);
	}
	return n ? n : -EAGAIN;
}

static __poll_t lrc_poll(struct file *f, poll_table *wait)
{
	struct lrc_dev *l = f->private_data;

	poll_wait(f, &l->wait, wait);
	return lrc_has_finished(l) ? EPOLLIN | EPOLLRDNORM : 0;
}

static int lrc_open(struct inode *inode, struct file *f)
{
	struct lrc_dev *l = container_of(f->private_data, struct lrc_dev, misc);
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

static int lrc_release(struct inode *inode, struct file *f)
{
	struct lrc_dev *l = f->private_data;
	unsigned long flags;

	mutex_lock(&l->ctl_lock);
	lrc_stop(l);
	spin_lock_irqsave(&l->lock, flags);
	INIT_LIST_HEAD(&l->finished);
	l->start.size = 0;
	spin_unlock_irqrestore(&l->lock, flags);
	for (unsigned int i = 0; i < LEMNOS_RP1_CSI2_DEPTH; i++) {
		lrc_put(l, &l->buf[i]);
		l->buf[i].busy = false;
	}
	mutex_lock(&l->map_lock);
	for (unsigned int i = 0; i < LRC_CACHE; i++)
		lrc_unmap(&l->cache[i]);
	mutex_unlock(&l->map_lock);
	mutex_unlock(&l->ctl_lock);
	mutex_lock(&l->open_lock);
	l->open = false;
	mutex_unlock(&l->open_lock);
	return 0;
}

static const struct file_operations lrc_fops = {
	.owner = THIS_MODULE,
	.open = lrc_open,
	.release = lrc_release,
	.read = lrc_read,
	.poll = lrc_poll,
	.unlocked_ioctl = lrc_ioctl,
	.compat_ioctl = compat_ptr_ioctl,
	.llseek = noop_llseek,
};

/* The lanes wired and their polarities, from the device tree's endpoint. */
static void lrc_read_dt(struct lrc_dev *l)
{
	struct device_node *ep = of_graph_get_endpoint_by_regs(l->dev->of_node, 0, -1);
	u32 pol[5] = { 0 };
	int n;

	l->max_lanes = 2;
	if (!ep)
		return;
	n = of_property_count_u32_elems(ep, "data-lanes");
	if (n >= 1 && n <= 4)
		l->max_lanes = n;
	n = of_property_count_u32_elems(ep, "lane-polarities");
	if (n >= 1 && n <= 5 && !of_property_read_u32_array(ep, "lane-polarities", pol, n))
		for (int i = 0; i < n; i++)
			l->polarity[i] = !!pol[i];
	of_node_put(ep);
}

static int lrc_probe(struct platform_device *pdev)
{
	struct device *dev = &pdev->dev;
	struct lrc_dev *l;
	int ret;

	l = devm_kzalloc(dev, sizeof(*l), GFP_KERNEL);
	if (!l)
		return -ENOMEM;
	l->dev = dev;
	spin_lock_init(&l->lock);
	mutex_init(&l->open_lock);
	mutex_init(&l->ctl_lock);
	mutex_init(&l->map_lock);
	init_waitqueue_head(&l->wait);
	INIT_LIST_HEAD(&l->pending);
	INIT_LIST_HEAD(&l->finished);
	platform_set_drvdata(pdev, l);

	l->dma = devm_platform_ioremap_resource(pdev, 0);
	if (IS_ERR(l->dma))
		return PTR_ERR(l->dma);
	l->host = devm_platform_ioremap_resource(pdev, 1);
	if (IS_ERR(l->host))
		return PTR_ERR(l->host);
	l->cfg = devm_platform_ioremap_resource(pdev, 2);
	if (IS_ERR(l->cfg))
		return PTR_ERR(l->cfg);
	l->irq = platform_get_irq(pdev, 0);
	if (l->irq <= 0)
		return -EINVAL;
	ret = dma_set_mask_and_coherent(dev, DMA_BIT_MASK(64));
	if (ret)
		return ret;
	l->clk = devm_clk_get_enabled(dev, NULL);
	if (IS_ERR(l->clk))
		return dev_err_probe(dev, PTR_ERR(l->clk), "no clock\n");
	lrc_read_dt(l);
	l->host_version = rd(l->host, HOST_VERSION);
	/* Quiet until START. */
	wr(l->cfg, MIPICFG_INTE, 0);
	wr(l->dma, CSI2_IRQ_MASK, 0);
	ret = devm_request_irq(dev, l->irq, lrc_isr, IRQF_SHARED, LRC_NAME, l);
	if (ret)
		return ret;

	l->misc.minor = MISC_DYNAMIC_MINOR;
	l->misc.name = LRC_NAME;
	l->misc.fops = &lrc_fops;
	l->misc.parent = dev;
	ret = misc_register(&l->misc);
	if (ret)
		return ret;
	dev_info(dev, "CSI-2 host 0x%08x, %u lanes, on /dev/%s\n", l->host_version,
		 l->max_lanes, LRC_NAME);
	return 0;
}

static void lrc_remove(struct platform_device *pdev)
{
	struct lrc_dev *l = platform_get_drvdata(pdev);

	misc_deregister(&l->misc);
	wr(l->cfg, MIPICFG_INTE, 0);
	wr(l->dma, CSI2_IRQ_MASK, 0);
}

/* No of_match_table: bound only through driver_override. */
static struct platform_driver lrc_driver = {
	.probe = lrc_probe,
	.remove = lrc_remove,
	.driver = {
		.name = LRC_NAME,
	},
};
module_platform_driver(lrc_driver);

MODULE_DESCRIPTION("RP1 CSI-2 receiver as a character device (Lemnos fast path)");
MODULE_LICENSE("GPL");
MODULE_IMPORT_NS("DMA_BUF");
