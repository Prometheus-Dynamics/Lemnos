// SPDX-License-Identifier: GPL-2.0-only
/*
 * rp1-csi2: one DMA channel of the camera front-end block's CSI-2 receiver,
 * feeding a frame stream. The front end proper is not used.
 *
 * Bound only through driver_override and only for compatibles in the owned=
 * parameter (see lemnos_core.c); it never replaces or patches a stock driver.
 *
 * The D-PHY bring-up, the CSI-2 DMA channel's registers and the frame start and
 * end accounting are ported from the stock driver,
 * drivers/media/platform/raspberrypi/rp1_cfe/{dphy,csi2,cfe}.c:
 *   Copyright (C) 2021-2024 Raspberry Pi Ltd.
 * by way of the receiver on the dev branch
 * (kernel-modules/lemnos/lemnos_rp1_csi2.c). Its buffer queue is gone: this
 * file has no address of its own. The rest:
 *   Copyright (c) 2026 the Lemnos authors.
 *
 * Everything the block can name or be told comes from somewhere the kernel
 * holds:
 *  - Only device addresses from lem_stream_next() reach the DMA registers:
 *    pool memory this module pinned and mapped, the end checked against the
 *    buffer. No ioctl carries an address. There is no register passthrough.
 *  - The register offsets here are constants, inside windows checked at probe.
 *  - Interrupt status is only tested for known bits. No count or length from
 *    the block reaches a frame or a publish: a frame's size is the one fixed
 *    at START, already checked against the buffer.
 *  - A buffer is the block's from lem_stream_next() until the channel is known
 *    not to be using it. Stopping runs FORCE, then waits until the channel's
 *    counter is still, then gives the buffers back; if it won't go still the
 *    buffers stay pinned for good. Closing the stream, STOP, closing this
 *    node and unbinding all come through here.
 *
 * Locking: l->ctl orders START, STOP, close and unbind. l->lock is the frame
 * path's, taken from the handler, and nests the stream's s->lock. The stream's
 * s->ctl nests l->ctl through the stop callback, so l->ctl is never held across
 * a call that takes s->ctl. l->going keeps stream creation out once unbinding
 * has started, without taking l->ctl, because unbinding waits for the stream
 * code while holding its list lock.
 */
#include <linux/atomic.h>
#include <linux/clk.h>
#include <linux/delay.h>
#include <linux/dma-mapping.h>
#include <linux/err.h>
#include <linux/file.h>
#include <linux/fs.h>
#include <linux/interrupt.h>
#include <linux/io.h>
#include <linux/kref.h>
#include <linux/miscdevice.h>
#include <linux/module.h>
#include <linux/mutex.h>
#include <linux/of.h>
#include <linux/of_graph.h>
#include <linux/platform_device.h>
#include <linux/property.h>
#include <linux/rwsem.h>
#include <linux/slab.h>
#include <linux/spinlock.h>
#include <linux/string.h>
#include <linux/uaccess.h>

#include "lemnos_internal.h"
#include "lemnos_rp1_csi2.h"
#include "lemnos_rp1_csi2_priv.h"

#define LRC_NAME	"lemnos-rp1-csi2"
#define LRC_CH		0

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
#define AUTO_ARM		BIT(4)
#define CSI2_MODE_FE_STREAMING	3
#define IRQ_EN_FS		BIT(13)
#define IRQ_EN_FE_ACK		BIT(15)
#define PACK_LINE		BIT(29)
#define CH_MODE_SHIFT		1
#define VC_SHIFT		5
#define DT_SHIFT		7

/* MIPI config (resource 2), from cfe.c. */
#define MIPICFG_CFG		0x004
#define MIPICFG_INTE		0x02c
#define MIPICFG_INTS		0x034
#define MIPICFG_CFG_SEL_CSI	BIT(0)
#define MIPICFG_INT_CSI_DMA	BIT(0)
#define MIPICFG_INT_PISP_FE	BIT(4)

/* The windows must cover what's used here, so a short region can't be mistaken for one. */
#define LRC_DMA_WINDOW		(CSI2_CH_FRAME_SIZE(LRC_CH) + 4)
#define LRC_HOST_WINDOW		(HOST_PHY_TST_CTRL1 + 4)
#define LRC_CFG_WINDOW		(MIPICFG_INTS + 4)

/* Limits the hardware has; dphy.c and csi2.c are the source. */
#define LRC_MIN_MBPS		80
#define LRC_MAX_MBPS		1500
#define LRC_MAX_VC		3
#define LRC_MAX_DT		63
#define LRC_MAX_DIMENSION	0xffff

/*
 * Waiting for the channel to stop: its line counter must hold still for this
 * many reads in a row, a read every 200-400 us, at most LRC_STOP_POLLS reads.
 * The slowest line at the slowest link is far under 200 us.
 */
#define LRC_STOP_STILL_READS	8
#define LRC_STOP_POLLS		250

static void lrc_free(struct kref *ref)
{
	struct lrc_dev *l = container_of(ref, struct lrc_dev, ref);

	if (l->fe)
		lem_pisp_fe_destroy(l->fe);
	put_device(l->dev);
	mutex_destroy(&l->ctl);
	kfree(l);
}

void lrc_ref_get(struct lrc_dev *l)
{
	kref_get(&l->ref);
}

void lrc_ref_put(struct lrc_dev *l)
{
	kref_put(&l->ref, lrc_free);
}

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

/*
 * csi2_set_buffer: ADDR0 last, it latches the others for the next frame. Only
 * ever a device address and size from lem_stream_next(). Lock held.
 */
static void lrc_set_buffer(struct lrc_dev *l, const struct lem_stream_slot *slot)
{
	u64 addr = slot->addr >> 4;

	wr(l->dma, CSI2_CH_LENGTH(LRC_CH), slot->bytes >> 4);
	wr(l->dma, CSI2_CH_STRIDE(LRC_CH), l->start.stride >> 4);
	wr(l->dma, CSI2_CH_ADDR1(LRC_CH), addr >> 32);
	wr(l->dma, CSI2_CH_ADDR0(LRC_CH), addr & 0xffffffff);
}

/* Gives the channel the stream's next buffer when it has none waiting. Lock held. */
static void lrc_arm(struct lrc_dev *l)
{
	if (!l->streaming || l->next.valid)
		return;
	if (lem_stream_next(l->stream, &l->next.slot))
		return;			/* none free, or the stream's own error */
	l->next.valid = true;
	lrc_set_buffer(l, &l->next.slot);
}

/* cfe_sof_isr_handler. Lock held. */
static void lrc_sof(struct lrc_dev *l, u64 now)
{
	if (l->cur.valid) {		/* a start before the last frame's end */
		lem_stream_complete(l->stream, &l->cur.slot, 0, l->frames, l->fs_ns);
		l->errors++;
		l->cur.valid = false;
	}
	l->cur = l->next;
	l->next.valid = false;
	if (l->cur.valid)
		l->fs_ns = now;
	else
		l->dropped++;
	l->frames++;
	l->fs_int = true;
	l->fe_int = false;
	lrc_arm(l);
}

/* cfe_eof_isr_handler. Lock held. */
static void lrc_eof(struct lrc_dev *l)
{
	if (l->cur.valid) {
		l->written++;
		/* frames is the FS count: this frame's number is one below it. */
		lem_stream_complete(l->stream, &l->cur.slot, l->cur.slot.bytes, l->frames - 1,
				    l->fs_ns);
		l->cur.valid = false;
	}
	l->fe_int = true;
	l->fs_int = false;
	lrc_arm(l);
}

static irqreturn_t lrc_isr(int irq, void *data)
{
	struct lrc_dev *l = data;
	u64 now = ktime_get_ns();
	bool sof, eof, handled = false;
	u32 sts, status;

	sts = rd(l->cfg, MIPICFG_INTS);
	if (l->fe && (sts & MIPICFG_INT_PISP_FE)) {
		lem_pisp_fe_isr(l->fe, now);
		handled = true;
	}
	if (!(sts & MIPICFG_INT_CSI_DMA))
		return handled ? IRQ_HANDLED : IRQ_NONE;
	status = rd(l->dma, CSI2_STATUS);
	if (!status)
		return handled ? IRQ_HANDLED : IRQ_NONE;
	wr(l->dma, CSI2_STATUS, status);	/* write back to clear */
	sof = status & IRQ_FS(LRC_CH);
	eof = status & IRQ_FE_ACK(LRC_CH);

	spin_lock(&l->lock);
	if (l->streaming) {
		/*
		 * The orders cfe_isr handles: FE then FS of the next frame,
		 * FS and FE of one frame, and an FS with no FE before it. The
		 * last, unlike cfe_isr, is a frame this can't call complete,
		 * so lrc_sof flags it rather than publishing it.
		 */
		if (eof) {
			if (sof && !l->fs_int) {
				lrc_sof(l, now);
				sof = false;
			}
			lrc_eof(l);
		}
		if (sof)
			lrc_sof(l, now);
	}
	spin_unlock(&l->lock);
	return IRQ_HANDLED;
}

/*
 * Whether the channel is seen to have stopped. The FORCE and the two ADDR0
 * writes have gone out and the read flushed them; what remains is what was
 * already in flight on the bus, and its line counter moves until that's done.
 * Bounded. Sleeps: never on the frame path.
 */
static bool lrc_dma_stopped(struct lrc_dev *l)
{
	u32 last = rd(l->dma, CSI2_CH_DEBUG(LRC_CH));
	unsigned int still = 0;

	for (unsigned int i = 0; i < LRC_STOP_POLLS; i++) {
		u32 now;

		usleep_range(200, 400);
		now = rd(l->dma, CSI2_CH_DEBUG(LRC_CH));
		if (now == last) {
			if (++still >= LRC_STOP_STILL_READS)
				return true;
		} else {
			still = 0;
			last = now;
		}
	}
	return false;
}

/*
 * Stops the channel, waits for it, and gives back what it held. Only when the
 * channel is seen to have stopped does anything go back: otherwise it was
 * still writing, so every buffer it could reach stays pinned for good. Takes
 * no stream lock of its own (s->ctl may be held by a caller above us).
 * ctl held.
 */
static void lrc_stop_locked(struct lrc_dev *l, struct lem_stream *s)
{
	unsigned long flags;

	spin_lock_irqsave(&l->lock, flags);
	l->streaming = false;
	/* csi2_stop_channel: FORCE stops mid-frame; ADDR0 latches it, twice. */
	wr(l->dma, CSI2_CH_CTRL(LRC_CH), FORCE);
	wr(l->dma, CSI2_CH_ADDR0(LRC_CH), 0);
	wr(l->dma, CSI2_CH_ADDR0(LRC_CH), 0);
	wr(l->dma, CSI2_IRQ_MASK, 0);
	wr(l->cfg, MIPICFG_INTE, 0);
	spin_unlock_irqrestore(&l->lock, flags);

	/* Everything written above is posted across a bus: a read makes it land first. */
	(void)rd(l->dma, CSI2_CH_CTRL(LRC_CH));
	/* No handler still holds cur/next, and none will start. */
	synchronize_irq(l->irq);

	if (lrc_dma_stopped(l)) {
		if (l->cur.valid)
			lem_stream_cancel(s, &l->cur.slot);
		if (l->next.valid)
			lem_stream_cancel(s, &l->next.slot);
		l->cur.valid = l->next.valid = false;
	} else {
		/* Not known to be finished: pinned for good, and refused until rebound. */
		dev_err(l->dev, "DMA did not stop; buffers stay pinned until reboot\n");
		l->wedged = true;
		lem_stream_wedge(s);
	}

	spin_lock_irqsave(&l->lock, flags);
	l->stream = NULL;
	spin_unlock_irqrestore(&l->lock, flags);
	if (lem_stream_detach(s))
		kref_put(&l->ref, lrc_free);
}

/* The stream is going: its stop() contract. Takes l->ctl under the stream's s->ctl. */
static void lrc_stream_stop(void *priv, struct lem_stream *s)
{
	struct lrc_dev *l = priv;

	mutex_lock(&l->ctl);
	if (l->stream == s)	/* unbinding stops it first; a late call finds nothing to do */
		lrc_stop_locked(l, s);
	mutex_unlock(&l->ctl);
	/* Our reference, taken at attach: the stream gave it up by calling this. */
	kref_put(&l->ref, lrc_free);
}

static int lrc_check_start(const struct lrc_dev *l, const struct lemnos_rp1_csi2_start *s)
{
	if (s->lanes < 1 || s->lanes > l->max_lanes)
		return -EINVAL;
	if (s->mbps < LRC_MIN_MBPS || s->mbps > LRC_MAX_MBPS)
		return -EINVAL;
	if (s->vc > LRC_MAX_VC || s->dt < 1 || s->dt > LRC_MAX_DT)
		return -EINVAL;
	if (!s->width || !s->height || s->width > LRC_MAX_DIMENSION ||
	    s->height > LRC_MAX_DIMENSION)
		return -EINVAL;
	if (!s->stride || (s->stride & 15) || !s->size || (s->size & 15))
		return -EINVAL;
	if (s->flags & ~LEMNOS_RP1_CSI2_UNPACK16)
		return -EINVAL;
	if (s->reserved[0] || s->reserved[1])
		return -EINVAL;
	if ((u64)s->stride * s->height > s->size)
		return -EINVAL;
	if ((s->flags & LEMNOS_RP1_CSI2_UNPACK16) && s->stride < 2 * s->width)
		return -EINVAL;
	return 0;
}

static long lrc_start(struct lrc_dev *l, const struct lemnos_rp1_csi2_start *s)
{
	const struct lem_stream_producer producer = { .stop = lrc_stream_stop, .priv = l };
	struct lem_stream *stream;
	struct file *file;
	unsigned long flags;
	long ret;
	u32 ctrl;

	ret = lrc_check_start(l, s);
	if (ret)
		return ret;
	if (l->wedged)
		return -EIO;
	if (l->stream || (l->fe && lem_pisp_fe_streaming(l->fe)))
		return -EBUSY;

	file = fget(s->stream_fd);
	if (!file)
		return -EBADF;
	stream = lem_stream_from_file(file, l->dev);
	if (IS_ERR(stream)) {
		ret = PTR_ERR(stream);
		goto put_file;
	}
	/* Released by whoever takes the producer off the stream, stop or unbind. */
	kref_get(&l->ref);
	ret = lem_stream_attach(stream, &producer, s->stride, s->height, s->size);
	if (ret) {
		kref_put(&l->ref, lrc_free);
		goto put_file;
	}

	spin_lock_irqsave(&l->lock, flags);
	l->start = *s;
	l->stream = stream;
	l->cur.valid = l->next.valid = false;
	l->frames = l->written = l->dropped = l->errors = 0;
	l->fs_int = l->fe_int = false;
	spin_unlock_irqrestore(&l->lock, flags);

	/* csi2_start_channel, the caller's vc and dt. */
	wr(l->dma, CSI2_CH_CTRL(LRC_CH), 0);
	wr(l->dma, CSI2_CH_DEBUG(LRC_CH), 0);
	wr(l->dma, CSI2_STATUS, IRQ_CH_MASK(LRC_CH));
	ctrl = DMA_EN | IRQ_EN_FS | IRQ_EN_FE_ACK | PACK_LINE;
	/* CH_MODE (bits 2:1): CSI2_MODE_REMAP, as cfe_start_channel picks for a 16-bit format. */
	if (s->flags & LEMNOS_RP1_CSI2_UNPACK16)
		ctrl |= 1 << CH_MODE_SHIFT;
	wr(l->dma, CSI2_CH_FRAME_SIZE(LRC_CH), (s->height << 16) | s->width);
	ctrl |= (s->vc << VC_SHIFT) | (s->dt << DT_SHIFT);
	wr(l->dma, CSI2_CH_CTRL(LRC_CH), ctrl);

	spin_lock_irqsave(&l->lock, flags);
	l->streaming = true;
	lrc_arm(l);
	spin_unlock_irqrestore(&l->lock, flags);

	/* cfe_start_streaming's order: MIPI config, then csi2_open_rx. */
	wr(l->cfg, MIPICFG_CFG, MIPICFG_CFG_SEL_CSI);
	wr(l->cfg, MIPICFG_INTE, MIPICFG_INT_CSI_DMA);
	wr(l->dma, CSI2_IRQ_MASK, 0);
	dphy_start(l, s->lanes, s->mbps);
	wr(l->dma, CSI2_CTRL, EOP_IS_EOL);
	ret = 0;
put_file:
	fput(file);
	return ret;
}

/*
 * csi2_start_channel(CSI2_MODE_FE_STREAMING, auto arm) and the latch write, in
 * that order: cfe_start_channel for the channel feeding the front end. The
 * channel's addresses are zero and never come from anywhere else: it hands the
 * stream to the front end, and the front end has every DMA address.
 */
void lrc_fe_channel_start(struct lrc_dev *l, u32 width, u32 height, u32 vc, u32 dt)
{
	u32 ctrl;

	wr(l->dma, CSI2_CH_CTRL(LRC_CH), 0);
	wr(l->dma, CSI2_CH_DEBUG(LRC_CH), 0);
	wr(l->dma, CSI2_STATUS, IRQ_CH_MASK(LRC_CH));
	ctrl = DMA_EN | IRQ_EN_FS | IRQ_EN_FE_ACK | PACK_LINE | AUTO_ARM;
	ctrl |= CSI2_MODE_FE_STREAMING << CH_MODE_SHIFT;
	wr(l->dma, CSI2_CH_FRAME_SIZE(LRC_CH), (height << 16) | width);
	ctrl |= (vc << VC_SHIFT) | (dt << DT_SHIFT);
	wr(l->dma, CSI2_CH_CTRL(LRC_CH), ctrl);
	wr(l->dma, CSI2_CH_LENGTH(LRC_CH), 0xffffffffu >> 4);
	wr(l->dma, CSI2_CH_STRIDE(LRC_CH), 0);
	wr(l->dma, CSI2_CH_ADDR1(LRC_CH), 0);
	wr(l->dma, CSI2_CH_ADDR0(LRC_CH), 0);
}

/* cfe_start_streaming's order after the channel and the front end are armed. */
void lrc_fe_open_rx(struct lrc_dev *l, u32 lanes, u32 mbps)
{
	wr(l->cfg, MIPICFG_CFG, MIPICFG_CFG_SEL_CSI);
	wr(l->cfg, MIPICFG_INTE, MIPICFG_INT_CSI_DMA | MIPICFG_INT_PISP_FE);
	wr(l->dma, CSI2_IRQ_MASK, 0);
	dphy_start(l, lanes, mbps);
	wr(l->dma, CSI2_CTRL, EOP_IS_EOL);
}

void lrc_fe_channel_stop(struct lrc_dev *l)
{
	wr(l->dma, CSI2_CH_CTRL(LRC_CH), FORCE);
	wr(l->dma, CSI2_CH_ADDR0(LRC_CH), 0);
	wr(l->dma, CSI2_CH_ADDR0(LRC_CH), 0);
	wr(l->dma, CSI2_IRQ_MASK, 0);
	wr(l->cfg, MIPICFG_INTE, 0);
}

static long lrc_info(struct lrc_dev *l, void __user *arg)
{
	struct lemnos_rp1_csi2_info info = {
		.abi = LEMNOS_RP1_CSI2_ABI,
		.host_version = l->host_version,
		.max_lanes = l->max_lanes,
	};
	unsigned long flags;

	mutex_lock(&l->ctl);
	info.flags = l->wedged ? LEMNOS_RP1_CSI2_INFO_WEDGED : 0;
	spin_lock_irqsave(&l->lock, flags);
	info.streaming = l->streaming;
	info.frames = l->frames;
	info.written = l->written;
	info.dropped = l->dropped;
	info.errors = l->errors;
	spin_unlock_irqrestore(&l->lock, flags);
	mutex_unlock(&l->ctl);
	return copy_to_user(arg, &info, sizeof(info)) ? -EFAULT : 0;
}

/* Runs lem_stream_create without l->ctl: see the locking note above. */
static long lrc_stream_create(struct lrc_dev *l, void __user *arg)
{
	struct lemnos_stream_create c;
	long ret;

	if (copy_from_user(&c, arg, sizeof(c)))
		return -EFAULT;
	down_read(&l->going);
	ret = l->gone ? -ENODEV : lem_stream_create(l->dev, &c);
	up_read(&l->going);
	/* A failed copy leaves the stream to the file's close: the caller has lost it. */
	if (!ret && copy_to_user(arg, &c, sizeof(c)))
		return -EFAULT;
	return ret;
}

static long lrc_ioctl(struct file *file, unsigned int cmd, unsigned long arg)
{
	struct lrc_dev *l = file->private_data;
	void __user *uarg = (void __user *)arg;
	long ret;

	switch (cmd) {
	case LEMNOS_RP1_CSI2_IOC_INFO:
		return lrc_info(l, uarg);
	case LEMNOS_RP1_CSI2_IOC_STREAM_CREATE:
		return lrc_stream_create(l, uarg);
	case LEMNOS_RP1_CSI2_IOC_START: {
		struct lemnos_rp1_csi2_start s;

		if (copy_from_user(&s, uarg, sizeof(s)))
			return -EFAULT;
		mutex_lock(&l->ctl);
		ret = l->gone ? -ENODEV : lrc_start(l, &s);
		mutex_unlock(&l->ctl);
		return ret;
	}
	case LEMNOS_RP1_CSI2_IOC_STOP:
		ret = 0;
		mutex_lock(&l->ctl);
		if (l->gone)
			ret = -ENODEV;
		else if (l->stream)
			lrc_stop_locked(l, l->stream);
		mutex_unlock(&l->ctl);
		return ret;
	default:
		return l->fe ? lem_pisp_fe_ioctl(l->fe, cmd, uarg) : -ENOTTY;
	}
}

static int lrc_open(struct inode *inode, struct file *file)
{
	struct lrc_dev *l = container_of(file->private_data, struct lrc_dev, misc);

	if (atomic_xchg(&l->opened, 1))
		return -EBUSY;
	kref_get(&l->ref);
	file->private_data = l;
	return 0;
}

static int lrc_release(struct inode *inode, struct file *file)
{
	struct lrc_dev *l = file->private_data;

	mutex_lock(&l->ctl);
	if (l->stream)
		lrc_stop_locked(l, l->stream);
	if (l->fe)
		lem_pisp_fe_stop(l->fe);
	mutex_unlock(&l->ctl);
	atomic_set(&l->opened, 0);
	kref_put(&l->ref, lrc_free);
	return 0;
}

static const struct file_operations lrc_fops = {
	.owner = THIS_MODULE,
	.open = lrc_open,
	.release = lrc_release,
	.unlocked_ioctl = lrc_ioctl,
	.compat_ioctl = compat_ptr_ioctl,
	.llseek = noop_llseek,
};

/* The lanes wired and their polarities, from the device tree's endpoint. */
static void lrc_read_dt(struct lrc_dev *l)
{
	struct device_node *ep = NULL;
	u32 pol[5] = { 0 };
	int n;

	l->max_lanes = 2;
	if (l->dev->of_node)
		ep = of_graph_get_endpoint_by_regs(l->dev->of_node, 0, -1);
	if (!ep)
		return;
	n = of_property_count_u32_elems(ep, "data-lanes");
	if (n >= 1 && n <= 4)
		l->max_lanes = n;
	n = of_property_count_u32_elems(ep, "lane-polarities");
	if (n >= 1 && n <= (int)ARRAY_SIZE(pol) &&
	    !of_property_read_u32_array(ep, "lane-polarities", pol, n))
		for (int i = 0; i < n; i++)
			l->polarity[i] = !!pol[i];
	of_node_put(ep);
}

/* The block's register window, refused when it doesn't cover what's used. */
static void __iomem *lrc_window(struct platform_device *pdev, unsigned int index, size_t need)
{
	struct resource *res = platform_get_resource(pdev, IORESOURCE_MEM, index);

	if (!res || resource_size(res) < need)
		return IOMEM_ERR_PTR(-ENODEV);
	return devm_platform_ioremap_resource(pdev, index);
}

static int lrc_probe(struct platform_device *pdev)
{
	struct device *dev = &pdev->dev;
	struct clk *clk;
	struct lrc_dev *l;
	int ret;

	if (!lem_block_owned(dev))
		return dev_err_probe(dev, -ENODEV, "not a block Lemnos owns (see owned=)\n");
	l = kzalloc(sizeof(*l), GFP_KERNEL);
	if (!l)
		return -ENOMEM;
	kref_init(&l->ref);
	l->dev = get_device(dev);
	mutex_init(&l->ctl);
	init_rwsem(&l->going);
	spin_lock_init(&l->lock);
	platform_set_drvdata(pdev, l);

	l->dma = lrc_window(pdev, 0, LRC_DMA_WINDOW);
	if (IS_ERR(l->dma)) {
		ret = PTR_ERR(l->dma);
		goto fail;
	}
	l->host = lrc_window(pdev, 1, LRC_HOST_WINDOW);
	if (IS_ERR(l->host)) {
		ret = PTR_ERR(l->host);
		goto fail;
	}
	l->cfg = lrc_window(pdev, 2, LRC_CFG_WINDOW);
	if (IS_ERR(l->cfg)) {
		ret = PTR_ERR(l->cfg);
		goto fail;
	}
	/* The front end's own registers, when the block has them (cfe.c, resource 3). */
	if (platform_get_resource(pdev, IORESOURCE_MEM, 3)) {
		void __iomem *fe = lrc_window(pdev, 3, LEM_PISP_FE_WINDOW);

		if (IS_ERR(fe)) {
			ret = PTR_ERR(fe);
			goto fail;
		}
		l->fe = lem_pisp_fe_create(l, fe);
		if (IS_ERR(l->fe)) {
			ret = PTR_ERR(l->fe);
			l->fe = NULL;
			goto fail;
		}
	}
	l->irq = platform_get_irq(pdev, 0);
	if (l->irq <= 0) {
		ret = l->irq ?: -EINVAL;
		goto fail;
	}
	ret = dma_set_mask_and_coherent(dev, DMA_BIT_MASK(64));
	if (ret)
		goto fail;
	clk = devm_clk_get_optional_enabled(dev, NULL);
	if (IS_ERR(clk)) {
		ret = dev_err_probe(dev, PTR_ERR(clk), "no clock\n");
		goto fail;
	}
	lrc_read_dt(l);
	l->host_version = rd(l->host, HOST_VERSION);
	/* Quiet until START. */
	wr(l->cfg, MIPICFG_INTE, 0);
	wr(l->dma, CSI2_IRQ_MASK, 0);
	ret = devm_request_irq(dev, l->irq, lrc_isr, 0, LRC_NAME, l);
	if (ret)
		goto fail;

	snprintf(l->name, sizeof(l->name), "%s-%s", LRC_NAME, dev_name(dev));
	l->misc.minor = MISC_DYNAMIC_MINOR;
	l->misc.name = l->name;
	l->misc.fops = &lrc_fops;
	l->misc.parent = dev;
	ret = misc_register(&l->misc);
	if (ret)
		goto fail;
	dev_info(dev, "CSI-2 host 0x%08x, %u lanes, /dev/%s\n", l->host_version, l->max_lanes,
		 l->name);
	return 0;
fail:
	kref_put(&l->ref, lrc_free);
	return ret;
}

static void lrc_remove(struct platform_device *pdev)
{
	struct lrc_dev *l = platform_get_drvdata(pdev);
	struct device *dev = &pdev->dev;

	/* No new opens; a file already open now fails with ENODEV. */
	misc_deregister(&l->misc);
	down_write(&l->going);
	l->gone = true;
	up_write(&l->going);

	mutex_lock(&l->ctl);
	if (l->stream)
		lrc_stop_locked(l, l->stream);
	if (l->fe)
		lem_pisp_fe_stop(l->fe);
	wr(l->cfg, MIPICFG_INTE, 0);
	wr(l->dma, CSI2_IRQ_MASK, 0);
	mutex_unlock(&l->ctl);

	/* The handler touches l, which a last file close may free below. */
	devm_free_irq(dev, l->irq, l);
	/* A stream holds dma-bufs mapped to this device; they go before it does. */
	lem_streams_block_gone(dev);
	kref_put(&l->ref, lrc_free);
}

/* No of_match_table: bound only through driver_override. */
static struct platform_driver lrc_driver = {
	.probe = lrc_probe,
	.remove = lrc_remove,
	.driver = {
		.name = LRC_NAME,
	},
};

int lem_rp1_csi2_register(void)
{
	return platform_driver_register(&lrc_driver);
}

void lem_rp1_csi2_unregister(void)
{
	platform_driver_unregister(&lrc_driver);
}
