// SPDX-License-Identifier: GPL-2.0
/*
 * The register-job profile of the A733's second video engine, its H.264
 * encoder and the input stage in front of it (lemnos_regjob.h).
 *
 * Facts, not code, from public sources:
 *
 * - The Radxa allwinner-bsp tree at a842bb8dc74608ba13975c97c31b9518f1976c01,
 *   drivers/ve/cedar-ve/platforms/ve_plat_sun60iw2.c and cedar_ve.c: the
 *   register blocks inside the 4 KiB window (encoder top at 0x800, input stage
 *   at 0xa00, encoder at 0xb00), the module select at 0x800 and its encoder
 *   bit (7), the encoder's interrupt enables (0xb14, bits 0..2) and status
 *   (0xb1c, bits 0..3) as its interrupt handler uses them, the top reset
 *   register at 0x804 (bits 0 and 4 set, then cleared), and the block's one
 *   reset line pulsed by IOCTL_RESET_VE.
 * - Linux, drivers/staging/media/sunxi/cedrus/cedrus_regs.h at
 *   65429ba825723c8670d5acc672f68b325eb80dff: the encoder's register names
 *   and offsets in that layout (VE_ISP_* at 0xa00, VE_AVC_* at 0xb00), which
 *   of them take a bus address, and VE_AVC_VLE_END and VE_AVC_VLE_MAX beside
 *   the bitstream address. The interrupt registers the vendor kernel names sit
 *   at VE_AVC_CTRL and VE_AVC_STATUS of this layout.
 * - jemk/cedrus (h264enc/h264enc.c, 84c5e373d77980fbac22008a6d515318d148d297):
 *   what each address register is given in a working encoder and how large
 *   each buffer behind it is in macroblocks, the meaning of the size, stride
 *   and format fields, the put-bits and encode triggers, and the status bits.
 *
 * None of this has been seen on this engine: the sizes below are the ones the
 * public encoder allocates, not a register manual's. Untested on board (see
 * testing/board-runs/H8a.md, which checks each with the IOMMU's fault report).
 *
 * The registers a caller may set are only those the public encoder sets, with
 * only the bits it sets. Everything else in the window, the decoders' blocks
 * and the top-level registers included, is refused.
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <linux/bits.h>
#include <linux/errno.h>
#include <linux/kernel.h>
#include <linux/math.h>

#include "lemnos_regjob_priv.h"

#define VE_WINDOW		0x1000

#define VE_ENC_TOP_MODE		0x800	/* module select: bit 7 the encoder, bit 6 the input stage */
#define VE_ENC_TOP_RESET	0x804

#define VE_ISP_INPUT_SIZE	0xa00	/* macroblocks: width << 16 | height */
#define VE_ISP_INPUT_STRIDE	0xa04	/* macroblocks << 16 */
#define VE_ISP_CTRL		0xa08	/* bits 31..29: the input format; bit 0 its interrupt */
#define VE_ISP_INPUT_LUMA	0xa78
#define VE_ISP_INPUT_CHROMA	0xa7c

#define VE_AVC_PARAM		0xb04
#define VE_AVC_QP		0xb08
#define VE_AVC_MOTION_EST	0xb10
#define VE_AVC_CTRL		0xb14	/* bits 2..0: interrupt enables */
#define VE_AVC_TRIGGER		0xb18
#define VE_AVC_STATUS		0xb1c
#define VE_AVC_BASIC_BITS	0xb20
#define VE_AVC_UNK_BUF		0xb60
#define VE_AVC_VLE_ADDR		0xb80
#define VE_AVC_VLE_END		0xb84
#define VE_AVC_VLE_OFFSET	0xb88	/* bits */
#define VE_AVC_VLE_MAX		0xb8c	/* bits */
#define VE_AVC_REF_LUMA		0xba0
#define VE_AVC_REF_CHROMA	0xba4
#define VE_AVC_REC_LUMA		0xbb0
#define VE_AVC_REC_CHROMA	0xbb4
#define VE_AVC_REF_SLUMA	0xbb8
#define VE_AVC_REC_SLUMA	0xbbc
#define VE_AVC_MB_INFO		0xbc0

#define VE_MODE_ENC		BIT(7)
#define VE_INT_ENABLES		0x7u
#define VE_INT_STATUS		0xfu
#define VE_TRIGGER_ENCODE	0x8u
#define VE_TRIGGER_PUT_BITS	0x1f01u	/* bit 0, and the bit count in bits 12..8 */
#define VE_FMT_SHIFT		29
#define VE_FMT_NV12		0u
#define VE_FMT_NV16		1u
#define VE_MB_MASK		0x3ffu	/* the macroblock fields the profile accepts: up to 16368 pixels */

/* Every address a job must give, in this order. */
enum {
	S_INPUT_LUMA,
	S_INPUT_CHROMA,
	S_VLE,
	S_REF_LUMA,
	S_REF_CHROMA,
	S_REF_SLUMA,
	S_REC_LUMA,
	S_REC_CHROMA,
	S_REC_SLUMA,
	S_MB_INFO,
	S_UNK_BUF,
	S_COUNT
};

/* 256 bytes: none of the public encoder's buffers is less aligned. */
#define VE_ALIGN 256u

static const struct lrj_slot ve_slots[S_COUNT] = {
	[S_INPUT_LUMA] = { VE_ISP_INPUT_LUMA, VE_ALIGN },
	[S_INPUT_CHROMA] = { VE_ISP_INPUT_CHROMA, VE_ALIGN },
	[S_VLE] = { VE_AVC_VLE_ADDR, VE_ALIGN },
	[S_REF_LUMA] = { VE_AVC_REF_LUMA, VE_ALIGN },
	[S_REF_CHROMA] = { VE_AVC_REF_CHROMA, VE_ALIGN },
	[S_REF_SLUMA] = { VE_AVC_REF_SLUMA, VE_ALIGN },
	[S_REC_LUMA] = { VE_AVC_REC_LUMA, VE_ALIGN },
	[S_REC_CHROMA] = { VE_AVC_REC_CHROMA, VE_ALIGN },
	[S_REC_SLUMA] = { VE_AVC_REC_SLUMA, VE_ALIGN },
	[S_MB_INFO] = { VE_AVC_MB_INFO, VE_ALIGN },
	[S_UNK_BUF] = { VE_AVC_UNK_BUF, VE_ALIGN },
};

/* What a caller may write, and only these bits: what the public encoder writes. */
static const struct lrj_plain ve_plain[] = {
	{ VE_ENC_TOP_MODE, 1, VE_MODE_ENC },
	{ VE_ISP_INPUT_SIZE, 1, VE_MB_MASK << 16 | VE_MB_MASK },
	{ VE_ISP_INPUT_STRIDE, 1, VE_MB_MASK << 16 },
	{ VE_ISP_CTRL, 1, 0x7u << VE_FMT_SHIFT },
	{ VE_AVC_PARAM, 1, 0x80000110u },	/* no emulation prevention, CABAC, P slice */
	{ VE_AVC_QP, 1, 0x00ff3f3fu },
	{ VE_AVC_MOTION_EST, 1, 0x00000104u },
	{ VE_AVC_TRIGGER, 1, VE_TRIGGER_PUT_BITS },
	{ VE_AVC_BASIC_BITS, 1, 0xffffffffu },
	{ VE_AVC_VLE_OFFSET, 1, 0xffffffffu },
};

static bool ve_start_ok(u32 value)
{
	return value == VE_TRIGGER_ENCODE;
}

/*
 * The bytes the block reaches from each address, from the job's geometry.
 * Every term is at most 16 bits wide, so nothing here can wrap a u64.
 */
static int ve_check(const struct lrj_ctx *c, u64 *need)
{
	u32 mode, size, stride, ctrl, offset;
	u64 mb_w, mb_h, mb_stride, w32, luma, chroma, vle_bits;
	int ret;

	ret = lrj_value(c, VE_ENC_TOP_MODE, &mode) ?:
	      lrj_value(c, VE_ISP_INPUT_SIZE, &size) ?:
	      lrj_value(c, VE_ISP_INPUT_STRIDE, &stride) ?:
	      lrj_value(c, VE_ISP_CTRL, &ctrl) ?:
	      lrj_value(c, VE_AVC_VLE_OFFSET, &offset);
	if (ret)
		return ret;
	if (!(mode & VE_MODE_ENC))
		return -EINVAL;
	mb_w = size >> 16 & VE_MB_MASK;
	mb_h = size & VE_MB_MASK;
	mb_stride = stride >> 16 & VE_MB_MASK;
	if (!mb_w || !mb_h || mb_stride < mb_w)
		return -EINVAL;
	if (ctrl >> VE_FMT_SHIFT != VE_FMT_NV12 && ctrl >> VE_FMT_SHIFT != VE_FMT_NV16)
		return -EINVAL;

	/* The input frame: a luma plane and an interleaved chroma plane at the stride. */
	need[S_INPUT_LUMA] = mb_stride * 16 * mb_h * 16;
	need[S_INPUT_CHROMA] = ctrl >> VE_FMT_SHIFT == VE_FMT_NV12 ? mb_stride * 16 * mb_h * 8 :
								      mb_stride * 16 * mb_h * 16;
	/* Reference and reconstruction pictures, in the encoder's own layout. */
	w32 = ALIGN(mb_w * 16, 32);
	luma = w32 * ALIGN(mb_h * 16, 32);
	chroma = w32 * ALIGN(mb_h * 8, 32);
	need[S_REF_LUMA] = need[S_REC_LUMA] = luma;
	need[S_REF_CHROMA] = need[S_REC_CHROMA] = chroma;
	need[S_REF_SLUMA] = need[S_REC_SLUMA] = luma / 4;
	need[S_MB_INFO] = mb_w * 32;
	need[S_UNK_BUF] = ALIGN(mb_w, 4) * mb_h * 8;

	/* The bitstream: its end and its size in bits are the module's, from the length. */
	vle_bits = c->bytes[S_VLE] * 8;
	if (vle_bits > U32_MAX || offset >= vle_bits)
		return -EFAULT;
	need[S_VLE] = 1;
	return 0;
}

static void ve_derive(struct lem_rj *rj, const struct lrj_ctx *c)
{
	lrj_wr(rj, VE_AVC_VLE_END, lower_32_bits(c->addr[S_VLE] + c->bytes[S_VLE] - 1));
	lrj_wr(rj, VE_AVC_VLE_MAX, lower_32_bits(c->bytes[S_VLE] * 8));
}

static void ve_arm(struct lem_rj *rj)
{
	lrj_wr(rj, VE_AVC_STATUS, VE_INT_STATUS);
	lrj_wr(rj, VE_AVC_CTRL, lrj_rd(rj, VE_AVC_CTRL) | VE_INT_ENABLES);
}

static int ve_irq(struct lem_rj *rj, u32 *status)
{
	u32 ctrl = lrj_rd(rj, VE_AVC_CTRL);
	u32 st = lrj_rd(rj, VE_AVC_STATUS) & VE_INT_STATUS;

	if (!st || !(ctrl & VE_INT_ENABLES))
		return 0;
	/* As the vendor handler does: the enables off, then the status cleared. */
	lrj_wr(rj, VE_AVC_CTRL, ctrl & ~VE_INT_ENABLES);
	lrj_wr(rj, VE_AVC_STATUS, st);
	*status = st;
	return 1;
}

static void ve_quiet(struct lem_rj *rj)
{
	lrj_wr(rj, VE_AVC_CTRL, lrj_rd(rj, VE_AVC_CTRL) & ~VE_INT_ENABLES);
	lrj_wr(rj, VE_ISP_CTRL, lrj_rd(rj, VE_ISP_CTRL) & ~1u);
}

static void ve_reset_regs(struct lem_rj *rj)
{
	u32 v = lrj_rd(rj, VE_ENC_TOP_RESET);

	lrj_wr(rj, VE_ENC_TOP_RESET, v | BIT(0) | BIT(4));
	lrj_wr(rj, VE_ENC_TOP_RESET, lrj_rd(rj, VE_ENC_TOP_RESET) & ~(BIT(0) | BIT(4)));
}

const struct lrj_profile lrj_a733_ve = {
	.id = LEMNOS_REGJOB_PROFILE_A733_VE,
	.name = "the A733 video engine's encoder",
	.window = VE_WINDOW,
	.plain = ve_plain,
	.nplain = ARRAY_SIZE(ve_plain),
	.slots = ve_slots,
	.nslots = S_COUNT,
	.start_reg = VE_AVC_TRIGGER,
	.addr_limit = 1ull << 32,
	.continuous = false,
	.start_ok = ve_start_ok,
	.check = ve_check,
	.derive = ve_derive,
	.arm = ve_arm,
	.irq = ve_irq,
	.quiet = ve_quiet,
	.reset_regs = ve_reset_regs,
};
