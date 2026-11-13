// SPDX-License-Identifier: GPL-2.0
/*
 * The register-job profile of the K230's ISP (an ISP8000 with a version 2
 * memory interface), in two roles (lemnos_regjob.h): the frames role sets up
 * the input, the windows and the main path and points the path at the
 * caller's buffers (a job is one frame's buffers; the path keeps running
 * between jobs), and the configuration role writes the tuning registers and
 * takes the ISP core's interrupt, for statistics. The frame path and the
 * tuning are two drivers' work on one block; isp_ctrl is in both roles'
 * tables, each with its own bits, and is written by read-modify-write.
 *
 * Facts, not code, from public sources:
 *
 * - The vendor SDK (k230_linux_sdk at fc6cc21),
 *   buildroot-overlay/package/vvcam/isp/vvcam_isp_driver.c and
 *   vvcam_isp_hal.h: the compatible, the three interrupts in order (the ISP
 *   core, the memory interface, the front end), the one unnamed reset line,
 *   the memory interface's interrupt status and clear registers (0x16d0,
 *   0x16d8) and the main path's frame-end bits, and the ISP core's interrupt
 *   registers (0x5bc, 0x5c4, 0x5c8) and its frame-out bit.
 * - VeriSilicon's register description of the same ISP, MIT-licensed, as
 *   published in nxp-imx/isp-vvcam (lf-6.18.20_2.0.0,
 *   e6453233e67f5d2f180d6ec277158dd54fa67a25): vvcam/isp/mrv_all_regs.h for
 *   the register offsets and fields (the memory interface's control at
 *   0x1300, the main path's control, format, bus and picture registers from
 *   0x1310, its Y, Cb and Cr base, size and offset registers from 0x1324,
 *   their second set from 0x1358 and their shadows from 0x1370, the raw
 *   output's base, size and offset from 0x1394 and its shadow at 0x13b4, the
 *   JPEG output's from 0x13c0, the interrupt mask, raw status and clear
 *   registers at 0x16c0, 0x16e0 and 0x16d8 with the two frame-end bits (YCbCr,
 *   raw) and the wrap bits; the ISP core's control, acquisition, output
 *   window, demosaic, image stabilisation and resizer registers, and the
 *   measurement blocks', with their fields); vvcam/isp/mrv_all_bits.h for what the base
 *   and offset enables do (a forced update copies base and size into their
 *   shadows together); and vvcam/isp/isp_miv2.c for how a working driver
 *   gives the main path a buffer (base, size, offset 0 per plane).
 *
 * The memory interface writes each plane within base .. base + size and
 * wraps at the size (it has a wrap interrupt per plane). So the module bounds
 * the block by writing the size itself, from the checked length, and the
 * caller's frame geometry needs no check: a frame larger than its buffer
 * wraps inside it. The base and size reach the block only through their
 * shadows, which the module makes it take in one forced update: on an idle
 * block before the start, and on a running one in the interrupt handler at
 * the end of a frame, so a frame never starts half on one buffer's addresses.
 * Automatic updates, the second address set (ping-pong) and every other path
 * of the memory interface (the self paths, JPEG, the multi-camera and read
 * DMA) stay off: their enables are not a caller's to set. A job starts the
 * main path's YCbCr output or its raw output, one of them; every address
 * register of the main path (Y, Cb, Cr, their second set, raw, JPEG) is
 * written from the job's checked buffers either way, so none is left naming
 * memory from before.
 *
 * The two tables are the ones the drivers in user space check against too
 * (drivers/k230-capture-sys/src/access.rs for the frames role,
 * drivers/k230isp-sys/src/access.rs for the configuration role), less the
 * acquisition's latency FIFO bit, which selects the read port from memory.
 *
 * None of this has been seen on this chip. Untested on board (see
 * testing/board-runs/H8b.md).
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <linux/bits.h>
#include <linux/errno.h>
#include <linux/kernel.h>
#include <linux/minmax.h>

#include "lemnos_regjob_priv.h"

#define ISP_WINDOW		0x9000

#define VI_DPCL			0x0018
#define ISP_CTRL		0x0400
#define ISP_ACQ_PROP		0x0404
#define ISP_ACQ_H_OFFS		0x0408	/* then v_offs, h_size, v_size */
#define ISP_ACQ_NR_FRAMES	0x0418
#define ISP_OUT_H_OFFS		0x0594	/* then v_offs, h_size, v_size */
#define ISP_DEMOSAIC		0x05a4
#define ISP_IMSC		0x05bc
#define ISP_MIS			0x05c4
#define ISP_ICR			0x05c8
#define MRSZ_CTRL		0x0c00
#define MRSZ_FORMAT_CONV_CTRL	0x0c6c
#define MI_CTRL			0x1300	/* path enables */
#define MI_MP_CTRL		0x1310
#define MI_MP_FMT		0x1314
#define MI_MP_BUS_CFG		0x1318
#define MI_MP_BUS_ID		0x131c
#define MI_MP_Y_BASE		0x1324	/* then size and offset counter */
#define MI_MP_Y_LLENGTH		0x1330	/* then pic width, height and size */
#define MI_MP_CB_BASE		0x1340
#define MI_MP_CR_BASE		0x134c
#define MI_MP_Y_BASE2		0x1358	/* then Cb and Cr: the ping-pong set */
#define MI_MP_Y_BASE_SHD	0x1370	/* then size and offset, then Cb's, then Cr's */
#define MI_MP_RAW_BASE		0x1394	/* then size and offset counter */
#define MI_MP_RAW_LLENGTH	0x13a0	/* then pic width, height and size */
#define MI_MP_RAW_BASE_SHD	0x13b4
#define MI_MP_JDP_BASE		0x13c0	/* then size and offset counter */
#define MI_IMSC			0x16c0
#define MI_MIS			0x16d0
#define MI_ICR			0x16d8
#define MI_RIS			0x16e0
#define ISP_IS_CTRL		0x2300
#define ISP_IS_H_OFFS		0x2308	/* then v_offs, h_size, v_size */

/* The configuration role's registers. */
#define ISP_AWB_PROP		0x0510
#define ISP_AWB_GAIN_G		0x0538	/* then gain_rb */
#define ISP_CC_COEFF_0		0x0570
#define ISP_GAMMA_OUT_MODE	0x05f4
#define ISP_GAMMA_OUT_Y_0	0x05f8
#define ISP_CT_OFFSET_R		0x0648	/* then g and b */
#define LSC_CTRL		0x2200
#define HIST_PROP		0x2400
#define HIST_WEIGHTS_0		0x2454
#define AE_CTRL			0x2600
#define BLS_CTRL		0x2700
#define BLS_A_FIXED		0x2728
#define AWB_MEAS_MODE		0x2c00

#define CTRL_ENABLE		BIT(0)
#define CTRL_MODE		(BIT(3) | BIT(2) | BIT(1))
#define CTRL_INFORM_ENABLE	BIT(4)
#define CTRL_CFG_UPD		BIT(9)
#define CTRL_GEN_CFG_UPD	BIT(10)
#define CTRL_GAMMA_OUT_ENABLE	BIT(11)
#define ACQ_BAYER_PAT		(BIT(4) | BIT(3))
#define ACQ_INPUT_SELECTION	(BIT(14) | BIT(13) | BIT(12))
#define WINDOW_MASK		0x00003fffu
#define ACQ_NR_FRAMES_MASK	0x000003ffu
#define DEMOSAIC_BYPASS		BIT(10)
#define MRSZ_SCALE_MASK		0x000000ffu
#define CONV_MASK		0x0000001fu	/* output and input formats */
#define MP_FMT_MASK		0x00007fffu
#define BUS_CFG_SWAP_RAW	0x00f00000u
#define BUS_ID_ENABLES		(BIT(25) | BIT(8))	/* the ID's switch and write-ID enables */
#define VI_DPCL_MP_MASK		0x0000000fu	/* the main path's selector, the channel mode */
#define MIS_FRAME_OUT		BIT(1)

#define MI_MP_YCBCR_ENABLE	BIT(0)
#define MI_MP_RAW_ENABLE	BIT(1)
#define MP_CTRL_UPDATE		(BIT(5) | BIT(4) | BIT(3))	/* offset and base enables, forced update */
#define MI_MP_FRAME_END		(BIT(1) | BIT(0))	/* raw, YCbCr */
#define MI_MP_WRAP		(BIT(10) | BIT(9) | BIT(8))	/* Cr, Cb, Y */
#define MI_SIZE_MASK		0x1ffffff0u
#define MI_ALIGN		16u

enum { P_Y, P_CB, P_CR, P_RAW, P_COUNT };

/* Each slot's base, size and offset counter; the second address set only for Y, Cb and Cr. */
static const u32 plane_base[P_COUNT] = { MI_MP_Y_BASE, MI_MP_CB_BASE, MI_MP_CR_BASE,
					 MI_MP_RAW_BASE };

static const struct lrj_slot isp_slots[P_COUNT] = {
	[P_Y] = { MI_MP_Y_BASE, MI_ALIGN },
	[P_CB] = { MI_MP_CB_BASE, MI_ALIGN },
	[P_CR] = { MI_MP_CR_BASE, MI_ALIGN },
	[P_RAW] = { MI_MP_RAW_BASE, MI_ALIGN },
};

#define ALL 0xffffffffu

/*
 * What the frames role may write: the input, the windows, the main path's
 * format and geometry. None of these holds an address, a size of memory, a
 * DMA start or an interrupt enable.
 */
static const struct lrj_plain isp_frames[] = {
	{ VI_DPCL, 1, VI_DPCL_MP_MASK },
	/* isp_ctrl: the enable, the mode, the input formatter and the two update strobes. */
	{ ISP_CTRL, 1, CTRL_ENABLE | CTRL_MODE | CTRL_INFORM_ENABLE | CTRL_CFG_UPD |
			       CTRL_GEN_CFG_UPD },
	/* isp_acq_prop: the Bayer pattern and the input width; not the read port from memory. */
	{ ISP_ACQ_PROP, 1, ACQ_BAYER_PAT | ACQ_INPUT_SELECTION },
	{ ISP_ACQ_H_OFFS, 4, WINDOW_MASK },
	{ ISP_ACQ_NR_FRAMES, 1, ACQ_NR_FRAMES_MASK },
	{ ISP_OUT_H_OFFS, 4, WINDOW_MASK },
	{ ISP_DEMOSAIC, 1, DEMOSAIC_BYPASS },
	{ ISP_IS_CTRL, 1, BIT(0) },
	{ ISP_IS_H_OFFS, 4, WINDOW_MASK },
	{ MRSZ_CTRL, 1, MRSZ_SCALE_MASK },
	{ MRSZ_FORMAT_CONV_CTRL, 1, CONV_MASK },
	{ MI_MP_FMT, 1, MP_FMT_MASK },
	{ MI_MP_BUS_CFG, 1, BUS_CFG_SWAP_RAW },
	{ MI_MP_BUS_ID, 1, BUS_ID_ENABLES },
	/* Line length and picture width, height and size: the YCbCr output's, then the raw's. */
	{ MI_MP_Y_LLENGTH, 4, ALL },
	{ MI_MP_RAW_LLENGTH, 4, ALL },
};

/*
 * What the configuration role may write: the tuning and the measurement
 * windows. None of these holds an address, a length or a DMA control.
 */
static const struct lrj_plain isp_config[] = {
	/* isp_ctrl: the gamma-out enable and the two update strobes. */
	{ ISP_CTRL, 1, CTRL_GAMMA_OUT_ENABLE | CTRL_CFG_UPD | CTRL_GEN_CFG_UPD },
	{ ISP_AWB_PROP, 1, ALL },
	{ ISP_AWB_GAIN_G, 2, ALL },
	{ ISP_CC_COEFF_0, 9, ALL },
	{ ISP_GAMMA_OUT_MODE, 1, ALL },
	{ ISP_GAMMA_OUT_Y_0, 17, ALL },
	{ ISP_CT_OFFSET_R, 3, ALL },
	/* Lens shading: its control, the table addresses (into its own memory) and data. */
	{ LSC_CTRL, 26, ALL },
	{ HIST_PROP, 5, ALL },
	{ HIST_WEIGHTS_0, 7, ALL },
	{ AE_CTRL, 5, ALL },
	{ BLS_CTRL, 1, ALL },
	{ BLS_A_FIXED, 4, ALL },
	{ AWB_MEAS_MODE, 5, ALL },
};

/* One output of the main path at a time: YCbCr or raw. */
static bool isp_start_ok(u32 value)
{
	return value == MI_MP_YCBCR_ENABLE || value == MI_MP_RAW_ENABLE;
}

/* The size the module writes for a plane: the length, to the register's grain and range. */
static u32 isp_size(u64 bytes)
{
	return min_t(u64, bytes, MI_SIZE_MASK) & MI_SIZE_MASK;
}

static int isp_check(const struct lrj_ctx *c, u64 *need)
{
	/* The block wraps inside the size the module writes: one grain is all it needs. */
	for (u32 k = 0; k < P_COUNT; k++)
		need[k] = MI_ALIGN;
	return 0;
}

static void isp_derive(struct lem_rj *rj, const struct lrj_ctx *c)
{
	for (u32 k = 0; k < P_COUNT; k++) {
		lrj_wr(rj, plane_base[k] + 4, isp_size(c->bytes[k]));
		lrj_wr(rj, plane_base[k] + 8, 0);
	}
	/* The ping-pong set is never enabled; it holds the same addresses all the same. */
	for (u32 k = P_Y; k <= P_CR; k++)
		lrj_wr(rj, MI_MP_Y_BASE2 + 4 * k, lower_32_bits(c->addr[k]));
	/* The JPEG output is never enabled; it is pointed at the raw output's memory. */
	lrj_wr(rj, MI_MP_JDP_BASE, lower_32_bits(c->addr[P_RAW]));
	lrj_wr(rj, MI_MP_JDP_BASE + 4, isp_size(c->bytes[P_RAW]));
	lrj_wr(rj, MI_MP_JDP_BASE + 8, 0);
}

static u32 isp_latch(struct lem_rj *rj, const struct lrj_ctx *c)
{
	lrj_wr(rj, MI_MP_CTRL, MP_CTRL_UPDATE);
	for (u32 k = P_Y; k <= P_CR; k++) {
		u32 shd = MI_MP_Y_BASE_SHD + 12 * k;

		if (lrj_rd(rj, shd) != lower_32_bits(c->addr[k]) ||
		    lrj_rd(rj, shd + 4) != isp_size(c->bytes[k]))
			return LEMNOS_REGJOB_STATUS_UNLATCHED;
	}
	if (lrj_rd(rj, MI_MP_RAW_BASE_SHD) != lower_32_bits(c->addr[P_RAW]))
		return LEMNOS_REGJOB_STATUS_UNLATCHED;
	return 0;
}

static void isp_arm(struct lem_rj *rj)
{
	lrj_wr(rj, MI_ICR, MI_MP_FRAME_END | MI_MP_WRAP);
	lrj_wr(rj, MI_IMSC, MI_MP_FRAME_END);
}

static int isp_irq(struct lem_rj *rj, u32 *status)
{
	u32 mis = lrj_rd(rj, MI_MIS);

	if (!(mis & MI_MP_FRAME_END))
		return 0;
	/* The frame's end, and whether any plane wrapped in it (a frame larger than its buffer). */
	*status = (mis & MI_MP_FRAME_END) | (lrj_rd(rj, MI_RIS) & MI_MP_WRAP);
	lrj_wr(rj, MI_ICR, MI_MP_FRAME_END | MI_MP_WRAP);
	return 1;
}

static void isp_quiet(struct lem_rj *rj)
{
	lrj_wr(rj, MI_IMSC, 0);
	lrj_wr(rj, MI_ICR, ALL);
	lrj_wr(rj, MI_CTRL, 0);
	lrj_wr(rj, ISP_IMSC, 0);
	lrj_wr(rj, ISP_ICR, ALL);
}

static void isp_reset_regs(struct lem_rj *rj)
{
	/* No automatic update: the shadows change only when the module says so. */
	lrj_wr(rj, MI_MP_CTRL, 0);
}

static u32 isp_event(struct lem_rj *rj)
{
	u32 mis = lrj_rd(rj, ISP_MIS);

	if (mis)
		lrj_wr(rj, ISP_ICR, mis);
	return mis;
}

static void isp_event_arm(struct lem_rj *rj, bool on)
{
	lrj_wr(rj, ISP_ICR, ALL);
	lrj_wr(rj, ISP_IMSC, on ? MIS_FRAME_OUT : 0);
}

const struct lrj_profile lrj_k230_isp = {
	.id = LEMNOS_REGJOB_PROFILE_K230_ISP,
	.name = "the K230 ISP's main path and configuration",
	.window = ISP_WINDOW,
	.plain = isp_frames,
	.nplain = ARRAY_SIZE(isp_frames),
	.keep_bits = true,
	.config = isp_config,
	.nconfig = ARRAY_SIZE(isp_config),
	.slots = isp_slots,
	.nslots = P_COUNT,
	.start_reg = MI_CTRL,
	.addr_limit = 1ull << 32,
	.continuous = true,
	.irq_index = 1,
	.start_ok = isp_start_ok,
	.check = isp_check,
	.derive = isp_derive,
	.arm = isp_arm,
	.irq = isp_irq,
	.latch = isp_latch,
	.quiet = isp_quiet,
	.reset_regs = isp_reset_regs,
	.event_irq_index = 0,
	.event = isp_event,
	.event_arm = isp_event_arm,
};
