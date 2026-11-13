// SPDX-License-Identifier: GPL-2.0
/*
 * The VeriSilicon VIP9000 neural accelerator, as a frame stream of lemnos.ko.
 * Jobs are posted on the stream's ring (lemnos_vip.h).
 *
 * What runs on the block is a program of command words that Lemnos neither
 * writes nor understands beyond its framing. Such a program can name any
 * address, so the only guarantee is the block's own MMU, and this file keeps
 * the MMU to itself:
 *
 *  - One root table (the MTLB) per block, in the module's memory, which the
 *    block is pointed at once per reset. Its entries change with every job:
 *    while a job runs they lead to the module's command buffer (read-only) and
 *    to that job's slots, each a dma-buf registered on the stream, mapped page
 *    by page from the device addresses the stream computed for the ranges the
 *    job holds. When the job ends, every entry for a slot is cleared before
 *    anything else can run. Nothing is mapped that the job didn't name.
 *  - The block is never given the caller's bytes to run. A program is copied
 *    into kernel memory at load and checked there (lvip_scan): only state
 *    loads outside the block's control registers, and no command that would
 *    move the command fetcher (LINK, CALL, RETURN, WAIT, END) or raise the
 *    module's event. At each job it is copied into the module's command
 *    buffer, between a preamble that flushes the MMU's TLB and a tail that
 *    raises the completion event and ends.
 *  - The registers, the interrupt and the reset stay here; nothing of them
 *    reaches user space.
 *
 * A fault (the MMU's exception or a bus error) or a job that runs too long is
 * answered by a reset of the block and a fresh set-up; the job's frame is
 * published with 0 bytes and the stream goes on. Only a block that can't be
 * shown reset is wedged: its job's memory, the tables and the command buffer
 * are kept for good, and new streams are refused.
 *
 * Interface facts (register offsets and values, the reset, initialisation,
 * MMU set-up and trigger sequences, the command encodings, the table entry
 * formats, the interrupt status bits, the chips and which MMU set-up each
 * takes) are from the vendor's vipcore driver in the Radxa allwinner-bsp tree,
 * github.com/radxa/allwinner-bsp at a842bb8dc74608ba13975c97c31b9518f1976c01,
 * drivers/npu/aw_nna_vip/vip2/ (vip_drv_hardware.c, memory/vip_drv_mmu.c and
 * .h, task/vip_drv_task_schedule.c, inc/vip_drv_share.h,
 * inc/vip_feature_database.h, vip_drv_context.c,
 * os/linux/vip_drv_device_driver.c; VeriSilicon, dual MIT and GPL). No code
 * is copied; drivers/vip9000/src/kernel.rs records each fact and its file.
 *
 * Locking: ps->collect_lock (one collector; `stopped`), then l->prog_lock (the
 * programs); s->ctl comes before both. l->lock (a spinlock: the job on the
 * block, and the trigger) is taken inside them and by the ISR. Never s->lock
 * then l->lock. The ordered workqueue runs the end of a job, the watchdog and
 * the reset one at a time.
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
#include <linux/pm_runtime.h>
#include <linux/reset.h>
#include <linux/slab.h>
#include <linux/spinlock.h>
#include <linux/uaccess.h>
#include <linux/wait.h>
#include <linux/workqueue.h>

#include "lemnos_internal.h"
#include "lemnos_vip.h"

/* Registers (vip_drv_hardware.c). */
#define VR_CLOCK		0x00000
#define VR_CLOCK_INIT		0x00070900u
#define VR_CLOCK_FSCALE_LOAD	BIT(9)
#define VR_CLOCK_GPU_IDLE	BIT(11)
#define VR_CLOCK_SOFT_RESET	BIT(12)
#define VR_CLOCK_IDLE_BITS	0x00030000u
#define VR_CLOCK_ISOLATE	BIT(19)
#define VR_IDLE			0x00004
#define VR_IDLE_FE		BIT(0)
#define VR_IDLE_ALL		0x7fffffffu
#define VR_INT_ACK		0x00010	/* read clears */
#define VR_INT_ENABLE		0x00014
#define VR_INT_AXI		BIT(31)
#define VR_INT_MMU		BIT(30)
#define VR_CID			0x00030
#define VR_MEM_COUNTERS		0x0003c
#define VR_REORDER		0x00090
#define VR_GATING		0x00100
#define VR_GATING_INIT		0x00140021u
#define VR_GATING_MODULES	0x00104
#define VR_PULSE		0x0010c
#define VR_PULSE_INIT		0x01590880u
#define VR_MMU_CONTROL		0x00388
#define VR_MMU_ENABLE		BIT(0)
#define VR_MMU_PD		BIT(5)
#define VR_PTA_LOW		0x0038c
#define VR_PTA_HIGH		0x00390
#define VR_PTA_LOAD		0x00394
#define VR_FE_CONTROL		0x003a4
#define VR_FE_ENABLE		BIT(16)
#define VR_AHB_CONTROL		0x003a8
#define VR_AHB_RESET		BIT(0)
#define VR_AHB_INIT		BIT(1)
#define VR_PD_ENTRY		0x003b4
#define VR_OUTSTANDING		0x00414
#define VR_POWER		0x0055c
#define VR_FE_ADDRESS		0x00654
#define VR_WINDOW		0x00700

/* Commands (vip_drv_hardware.c, task/vip_drv_task_schedule.c). */
#define VC_OP(w)		((w) >> 27)
#define VC_LOAD_STATE		1u
#define VC_END			2u
#define VC_NOP			3u
#define VC_STALL		9u
#define VC_STATE(addr, n)	((VC_LOAD_STATE << 27) | ((u32)(n) << 16) | (addr))
#define VS_MMU_CONFIG		0x0061u	/* data 0xffffff7f: flush the TLB */
#define VS_PTA_CONFIG		0x006bu
#define VS_EVENT		0x0e01u
#define VS_SEMAPHORE		0x0e02u
#define VS_FLUSH		0x0e03u
#define VS_MMU_SETUP		0x0e12u
#define VS_CONTROL_END		0x0400u	/* every state below is a control register */
#define VC_TOKEN		0x00000701u	/* front end to the pixel engine */
#define VC_TOKEN_ALL		0x30000701u
#define VC_EVENT_ID		4u
#define VC_EVENT_FROM_PE	BIT(6)

/* The command buffer: the module's preamble, the program, the module's tail. */
#define LVIP_PRE_WORDS		6
#define LVIP_TAIL_WORDS		10
#define LVIP_CMD_BYTES		PAGE_ALIGN(65535u * 8)
#define LVIP_CMD_PAGES		(LVIP_CMD_BYTES >> 12)

/* The MMU, 32-bit addresses, "1K mode" (memory/vip_drv_mmu.h). */
#define LVIP_MTLB_SHIFT		24
#define LVIP_MTLB_ENTRIES	256
#define LVIP_STLB_ENTRIES	4096
#define LVIP_STLB_BYTES		(LVIP_STLB_ENTRIES * 4)
#define LVIP_PAGE		4096u
#define LVIP_PER_SLOT		(LEMNOS_VIP_SLOT_SPAN >> LVIP_MTLB_SHIFT)	/* 16 */
#define LVIP_MTLB_PRESENT	BIT(0)	/* with 4K pages: type 0 */
#define LVIP_PTE_PRESENT	BIT(0)
#define LVIP_PTE_EXCEPTION	BIT(1)
#define LVIP_PTE_WRITABLE	BIT(2)
#define LVIP_PHYS_LIMIT		BIT_ULL(40)
#define LVIP_PD_MODE_1K		1u

/* Bounds. */
#define LVIP_TIMEOUT_MS		5000u
#define LVIP_RESET_MS		20u
#define LVIP_IDLE_MS		50u

static bool test_vip_fake_regs;
module_param(test_vip_fake_regs, bool, 0444);
MODULE_PARM_DESC(test_vip_fake_regs,
		 "Test only: the VIP9000's register window is ordinary memory (the test harness)");

static unsigned int test_vip_timeout_ms;
module_param(test_vip_timeout_ms, uint, 0444);
MODULE_PARM_DESC(test_vip_timeout_ms, "Test only: a job's time on the VIP9000 (ms; 0 leaves it)");

/* The chips the vendor's feature table lists with an MMU, and how each is pointed at it. */
static const struct {
	u32 cid;
	u32 mmu;
} lvip_chips[] = {
	{ 0x10000016, LEMNOS_VIP_MMU_COMMAND },
	{ 0x10000021, LEMNOS_VIP_MMU_PD },
	{ 0x1000003b, LEMNOS_VIP_MMU_PD },
};

struct lvip_program {
	u32 *words;			/* the checked copy; kernel only */
	u32 bytes;
};

struct lem_vip;

struct lvip_stream {
	struct lem_vip *l;
	struct lem_stream *s;

	struct mutex collect_lock;	/* one collector; `stopped` */
	bool stopped;

	/* Under l->prog_lock. */
	struct lvip_program prog[LEMNOS_VIP_PROGRAMS];

	/* Under l->lock. */
	bool stopping;
	bool wedged;
	u64 sequence;

	wait_queue_head_t idle;
	struct delayed_work collect;
};

struct lem_vip {
	struct device *dev;
	void __iomem *regs;
	struct workqueue_struct *wq;
	struct mutex create_lock;	/* one stream at a time */
	struct mutex prog_lock;
	u32 cid, mmu, timeout_ms;
	int irq;

	/* The tables and the command buffer: coherent, below 4 GiB. */
	__le32 *mtlb;
	dma_addr_t mtlb_dma;
	__le32 *stlb0;			/* the command buffer's window */
	dma_addr_t stlb0_dma;
	__le32 *cmd;
	dma_addr_t cmd_dma;
	__le32 *pta;			/* the page table array's one entry (command set-up) */
	dma_addr_t pta_dma;
	/* Per slot, allocated as first needed and kept until unbind. */
	__le32 *stlb[LEMNOS_VIP_SLOTS][LVIP_PER_SLOT];
	dma_addr_t stlb_dma[LEMNOS_VIP_SLOTS][LVIP_PER_SLOT];
	u32 mapped[LEMNOS_VIP_SLOTS];	/* pages mapped for the job on the block */

	spinlock_t lock;		/* everything below */
	struct lvip_stream *attached;
	bool running;			/* the block has `slot` */
	bool ending;			/* its end is with the workqueue */
	bool fault, timed_out;
	bool wedged;
	bool module_held;
	u32 int_status;
	struct lem_stream_slot slot;
	u64 finished, refused, faults, timeouts;
	u32 last_fault, resets;
	struct work_struct end;
	struct delayed_work watchdog;
};

static u32 lvip_rd(struct lem_vip *l, unsigned int off)
{
	return readl(l->regs + off);
}

static void lvip_wr(struct lem_vip *l, unsigned int off, u32 val)
{
	writel(val, l->regs + off);
}

/* --- programs --- */

/*
 * Whether a program may run: whole commands to its end, each one of those
 * listed in lemnos_vip.h. `at` is the offset of a refused command. Touches
 * nothing but `words`.
 */
static int lvip_scan(const u32 *words, u32 n, u32 *at)
{
	u32 i = 0;

	while (i < n) {
		u32 w = words[i], op = VC_OP(w), len;

		*at = i * 4;
		switch (op) {
		case VC_LOAD_STATE: {
			u32 addr = w & 0xffff, count = (w >> 16) & 0x3ff, end;

			if (!count)
				count = 1024;
			end = addr + count;	/* at most 0x10000 + 1023: no wrap in u32 */
			if (addr < VS_CONTROL_END || end > 0x10000 ||
			    (addr <= VS_EVENT && VS_EVENT < end) ||
			    (addr <= VS_MMU_SETUP && VS_MMU_SETUP < end))
				return -EPERM;
			/* The header and the values, padded to a whole command word. */
			len = ALIGN(1 + count, 2);
			break;
		}
		case VC_NOP:
		case VC_STALL:
			len = 2;
			break;
		default:
			return -EPERM;
		}
		if (len > n - i)
			return -EINVAL;
		i += len;
	}
	return 0;
}

static long lvip_load(struct lvip_stream *ps, struct lemnos_vip_load __user *uarg)
{
	struct lem_vip *l = ps->l;
	struct lemnos_vip_load a;
	u32 *words, at = 0;
	long ret;
	int slot = -1;

	if (copy_from_user(&a, uarg, sizeof(a)))
		return -EFAULT;
	if (a.reserved || !a.bytes || a.bytes % 8 || a.bytes > LEMNOS_VIP_MAX_PROGRAM_BYTES)
		return -EINVAL;
	words = kvmalloc(a.bytes, GFP_KERNEL);
	if (!words)
		return -ENOMEM;
	/* The copy is what is checked, and what runs. */
	if (copy_from_user(words, u64_to_user_ptr(a.data), a.bytes)) {
		kvfree(words);
		return -EFAULT;
	}
	ret = lvip_scan(words, a.bytes / 4, &at);
	if (ret) {
		kvfree(words);
		a.refused_at = at;
		if (copy_to_user(&uarg->refused_at, &a.refused_at, sizeof(a.refused_at)))
			return -EFAULT;
		return ret;
	}
	mutex_lock(&l->prog_lock);
	for (u32 i = 0; i < LEMNOS_VIP_PROGRAMS; i++)
		if (!ps->prog[i].words) {
			slot = i;
			break;
		}
	if (slot >= 0) {
		ps->prog[slot].words = words;
		ps->prog[slot].bytes = a.bytes;
	}
	mutex_unlock(&l->prog_lock);
	if (slot < 0) {
		kvfree(words);
		return -EBUSY;
	}
	a.program = slot + 1;
	a.refused_at = 0;
	return copy_to_user(uarg, &a, sizeof(a)) ? -EFAULT : 0;
}

/* A job holds no reference to its program: the program is copied when the job starts. */
static long lvip_unload(struct lvip_stream *ps, struct lemnos_vip_unload __user *uarg)
{
	struct lem_vip *l = ps->l;
	struct lemnos_vip_unload a;
	u32 *words = NULL;

	if (copy_from_user(&a, uarg, sizeof(a)))
		return -EFAULT;
	if (a.reserved || !a.program || a.program > LEMNOS_VIP_PROGRAMS)
		return -EINVAL;
	mutex_lock(&l->prog_lock);
	swap(words, ps->prog[a.program - 1].words);
	ps->prog[a.program - 1].bytes = 0;
	mutex_unlock(&l->prog_lock);
	if (!words)
		return -ENOENT;
	kvfree(words);
	return 0;
}

/* --- the tables --- */

static u32 lvip_pte(u64 addr, bool writable)
{
	return (lower_32_bits(addr) & ~(LVIP_PAGE - 1)) | (((u32)(addr >> 32) & 0xff) << 4) |
	       (writable ? LVIP_PTE_WRITABLE : 0) | LVIP_PTE_EXCEPTION | LVIP_PTE_PRESENT;
}

/* The root as it is between jobs: the command buffer's window and nothing else. */
static void lvip_tables_idle(struct lem_vip *l)
{
	for (u32 s = 0; s < LEMNOS_VIP_SLOTS; s++) {
		for (u32 k = 0; k < LVIP_PER_SLOT && l->mapped[s]; k++) {
			u32 pages = min_t(u32, l->mapped[s], LVIP_STLB_ENTRIES);

			memset(l->stlb[s][k], 0, pages * 4);
			l->mapped[s] -= pages;
		}
	}
	for (u32 i = 1; i < LVIP_MTLB_ENTRIES; i++)
		WRITE_ONCE(l->mtlb[i], 0);
	WRITE_ONCE(l->mtlb[0], cpu_to_le32(lower_32_bits(l->stlb0_dma) | LVIP_MTLB_PRESENT));
	/* What the block reads next is what was written here. */
	wmb();
}

/* The tables for slot `s` of the job, from its memory at `addr`. Nothing runs. */
static int lvip_map_slot(struct lem_vip *l, u32 s, u64 addr, u64 len)
{
	u32 pages = (u32)len / LVIP_PAGE, first = LEMNOS_VIP_SLOT_ADDR(s) >> LVIP_MTLB_SHIFT;
	u64 end;

	/* The entries hold 40 bits of address. */
	if (check_add_overflow(addr, len, &end) || end > LVIP_PHYS_LIMIT)
		return -ENXIO;
	for (u32 k = 0; k * LVIP_STLB_ENTRIES < pages; k++) {
		if (!l->stlb[s][k]) {
			l->stlb[s][k] = dma_alloc_coherent(l->dev, LVIP_STLB_BYTES,
							   &l->stlb_dma[s][k], GFP_KERNEL);
			if (!l->stlb[s][k])
				return -ENOMEM;
		}
		WRITE_ONCE(l->mtlb[first + k],
			   cpu_to_le32(lower_32_bits(l->stlb_dma[s][k]) | LVIP_MTLB_PRESENT));
	}
	for (u32 p = 0; p < pages; p++)
		WRITE_ONCE(l->stlb[s][p / LVIP_STLB_ENTRIES][p % LVIP_STLB_ENTRIES],
			   cpu_to_le32(lvip_pte(addr + (u64)p * LVIP_PAGE, true)));
	l->mapped[s] = pages;
	return 0;
}

/* The command buffer for `prog`: flush the TLB, the program, then the module's end. */
static u32 lvip_fill_command(struct lem_vip *l, const struct lvip_program *prog)
{
	static const u32 pre[LVIP_PRE_WORDS] = {
		VC_STATE(VS_MMU_CONFIG, 1), 0xffffff7fu,
		VC_STATE(VS_SEMAPHORE, 1), VC_TOKEN_ALL,
		VC_STALL << 27, VC_TOKEN_ALL,
	};
	static const u32 tail[LVIP_TAIL_WORDS] = {
		VC_STATE(VS_SEMAPHORE, 1), VC_TOKEN_ALL,
		VC_STALL << 27, VC_TOKEN_ALL,
		VC_STATE(VS_FLUSH, 1), 0x00000c23u,
		VC_STATE(VS_EVENT, 1), VC_EVENT_ID | VC_EVENT_FROM_PE,
		VC_END << 27, 0,
	};
	u32 n = prog->bytes / 4, i;

	for (i = 0; i < LVIP_PRE_WORDS; i++)
		l->cmd[i] = cpu_to_le32(pre[i]);
	for (u32 j = 0; j < n; j++)
		l->cmd[i + j] = cpu_to_le32(prog->words[j]);
	i += n;
	for (u32 j = 0; j < LVIP_TAIL_WORDS; j++)
		l->cmd[i + j] = cpu_to_le32(tail[j]);
	return i + LVIP_TAIL_WORDS;
}

/* --- the block --- */

/* Whether `ok` holds within `ms`, read every 100 us. Sleeps. */
static bool lvip_poll(struct lem_vip *l, bool (*ok)(struct lem_vip *l), unsigned int ms)
{
	ktime_t until = ktime_add_ms(ktime_get(), ms);

	for (;;) {
		if (ok(l))
			return true;
		if (!ktime_before(ktime_get(), until))
			return false;
		usleep_range(100, 200);
	}
}

static bool lvip_reset_done(struct lem_vip *l)
{
	return lvip_rd(l, VR_IDLE) == VR_IDLE_ALL &&
	       (lvip_rd(l, VR_CLOCK) & VR_CLOCK_IDLE_BITS) == VR_CLOCK_IDLE_BITS &&
	       !(lvip_rd(l, VR_MMU_CONTROL) & VR_MMU_ENABLE);
}

static bool lvip_fe_idle(struct lem_vip *l)
{
	return lvip_rd(l, VR_IDLE) & VR_IDLE_FE;
}

/* Test only: the harness's fake has taken the start (memory can't turn busy as it is written). */
static bool lvip_fake_started(struct lem_vip *l)
{
	return !(lvip_rd(l, VR_FE_CONTROL) & VR_FE_ENABLE);
}

static bool lvip_all_idle(struct lem_vip *l)
{
	return lvip_rd(l, VR_IDLE) == VR_IDLE_ALL;
}

/* The vendor's software reset, seen to work twice in a row. Sleeps. */
static int lvip_reset(struct lem_vip *l)
{
	u32 good = 0;

	lvip_wr(l, VR_INT_ENABLE, 0);
	lvip_wr(l, VR_CLOCK, VR_CLOCK_INIT);
	for (u32 tries = 0; good < 2 && tries < 10; tries++) {
		u32 pulse = (VR_PULSE_INIT & ~BIT(16)) | BIT(17) | BIT(0);
		u32 ctl = VR_CLOCK_INIT | VR_CLOCK_ISOLATE;

		lvip_wr(l, VR_GATING_MODULES, 0);
		lvip_wr(l, VR_PULSE, pulse);
		lvip_wr(l, VR_PULSE, pulse & ~BIT(0));
		lvip_wr(l, VR_CLOCK, VR_CLOCK_INIT | VR_CLOCK_FSCALE_LOAD);
		udelay(5);
		lvip_wr(l, VR_CLOCK, VR_CLOCK_INIT);
		udelay(20);
		lvip_wr(l, VR_CLOCK, ctl);
		lvip_wr(l, VR_AHB_CONTROL, VR_AHB_RESET);
		udelay(20);
		lvip_wr(l, VR_AHB_CONTROL, 0);
		udelay(40);
		lvip_wr(l, VR_CLOCK, ctl & ~VR_CLOCK_SOFT_RESET);
		lvip_wr(l, VR_CLOCK, ctl & ~VR_CLOCK_SOFT_RESET & ~VR_CLOCK_ISOLATE);
		udelay(50);
		if (lvip_poll(l, lvip_reset_done, LVIP_RESET_MS))
			good++;
		else
			good = 0;
	}
	if (good < 2)
		return -EIO;
	lvip_wr(l, VR_POWER, 0x00ffffff);
	lvip_wr(l, VR_REORDER, lvip_rd(l, VR_REORDER) & ~BIT(6));
	l->resets++;
	return lvip_all_idle(l) ? 0 : -EIO;
}

/* Points the block at the root, as the chip takes it. The MMU is off. Sleeps. */
static int lvip_mmu_on(struct lem_vip *l)
{
	if (l->mmu == LEMNOS_VIP_MMU_PD) {
		lvip_wr(l, VR_PD_ENTRY, (u32)(l->mtlb_dma >> 12) << 4 | LVIP_PD_MODE_1K);
		lvip_wr(l, VR_MMU_CONTROL, VR_MMU_PD | VR_MMU_ENABLE);
		return 0;
	}
	/* The array's entry, then a command (run with the MMU off: addresses physical) selecting it. */
	l->pta[0] = cpu_to_le32(lower_32_bits(l->mtlb_dma) | LVIP_PD_MODE_1K);
	l->pta[1] = cpu_to_le32(upper_32_bits(l->mtlb_dma));
	lvip_wr(l, VR_PTA_LOW, lower_32_bits(l->pta_dma));
	lvip_wr(l, VR_PTA_HIGH, upper_32_bits(l->pta_dma));
	lvip_wr(l, VR_PTA_LOAD, 1);
	{
		static const u32 setup[] = {
			VC_STATE(VS_PTA_CONFIG, 1), 0xfffe0000u,
			VC_STATE(VS_MMU_SETUP, 1), 0x00490000u,
			VC_STATE(VS_SEMAPHORE, 1), VC_TOKEN,
			VC_STALL << 27, VC_TOKEN,
			VC_END << 27, 0,
		};

		for (u32 i = 0; i < ARRAY_SIZE(setup); i++)
			l->cmd[i] = cpu_to_le32(setup[i]);
		wmb();
		lvip_wr(l, VR_FE_ADDRESS, lower_32_bits(l->cmd_dma));
		lvip_wr(l, VR_FE_CONTROL, VR_FE_ENABLE | ARRAY_SIZE(setup) / 2);
	}
	if (test_vip_fake_regs && !lvip_poll(l, lvip_fake_started, LVIP_RESET_MS))
		return -EIO;
	if (!lvip_poll(l, lvip_fe_idle, LVIP_RESET_MS))
		return -EIO;
	udelay(10);
	lvip_wr(l, VR_MMU_CONTROL, VR_MMU_ENABLE);
	return 0;
}

/* Reset, set up, tables empty, interrupts on. Nothing may be running on it. Sleeps. */
static int lvip_up(struct lem_vip *l)
{
	int ret = lvip_reset(l);

	if (ret)
		return ret;
	lvip_wr(l, VR_CLOCK, VR_CLOCK_INIT);
	lvip_wr(l, VR_CLOCK, VR_CLOCK_INIT & ~VR_CLOCK_GPU_IDLE);
	lvip_wr(l, VR_AHB_CONTROL, VR_AHB_INIT);
	lvip_wr(l, VR_MEM_COUNTERS, ~0u);
	lvip_wr(l, VR_MEM_COUNTERS, 0);
	lvip_wr(l, VR_GATING, VR_GATING_INIT);
	lvip_tables_idle(l);
	ret = lvip_mmu_on(l);
	if (ret)
		return ret;
	lvip_wr(l, VR_GATING_MODULES, 0);
	lvip_wr(l, VR_PULSE, (lvip_rd(l, VR_PULSE) & ~BIT(16)) | BIT(17));
	lvip_wr(l, VR_OUTSTANDING, lvip_rd(l, VR_OUTSTANDING) & ~BIT(0));
	lvip_rd(l, VR_INT_ACK);
	if (test_vip_fake_regs)
		lvip_wr(l, VR_INT_ACK, 0);
	lvip_wr(l, VR_INT_ENABLE, ~0u);
	return 0;
}

/* The block can't be given up safely. l->lock held. */
static void lvip_wedge(struct lem_vip *l, struct lvip_stream *ps, int err)
{
	if (!l->wedged)
		dev_crit(l->dev, "block did not reset: its job's buffers stay held until a reboot\n");
	l->wedged = true;
	if (!l->module_held) {
		l->module_held = true;
		__module_get(THIS_MODULE);
	}
	if (ps) {
		ps->wedged = true;
		lem_stream_fault(ps->s, err);
		wake_up_all(&ps->idle);
	}
}

static irqreturn_t lvip_isr(int irq, void *data)
{
	struct lem_vip *l = data;
	u32 st = lvip_rd(l, VR_INT_ACK);

	if (test_vip_fake_regs)
		lvip_wr(l, VR_INT_ACK, 0);
	if (!st)
		return IRQ_NONE;
	spin_lock(&l->lock);
	if (l->running && !l->ending && !l->wedged) {
		l->int_status |= st;
		if (st & (VR_INT_AXI | VR_INT_MMU)) {
			l->fault = true;
			l->ending = true;
		} else if (st & BIT(VC_EVENT_ID)) {
			l->ending = true;
		}
		if (l->ending)
			queue_work(l->wq, &l->end);
	}
	spin_unlock(&l->lock);
	return IRQ_HANDLED;
}

/* A job outstanding too long, or a stream ending under one: stop it. */
static void lvip_watchdog(struct work_struct *work)
{
	struct lem_vip *l = container_of(to_delayed_work(work), struct lem_vip, watchdog);
	unsigned long flags;

	spin_lock_irqsave(&l->lock, flags);
	if (l->running && !l->ending && !l->wedged) {
		l->timed_out = true;
		l->ending = true;
		queue_work(l->wq, &l->end);
	}
	spin_unlock_irqrestore(&l->lock, flags);
}

/*
 * The end of the job on the block, on the ordered workqueue. An event with the
 * block idle is a finished job. Anything else (a fault, the watchdog, an event
 * without the block going idle) resets the block, and only a block seen reset
 * lets go of the job's memory.
 */
static void lvip_end(struct work_struct *work)
{
	struct lem_vip *l = container_of(work, struct lem_vip, end);
	struct lvip_stream *ps;
	unsigned long flags;
	bool clean, fault, timed_out;
	u32 status;
	int ret = 0;

	spin_lock_irqsave(&l->lock, flags);
	if (!l->running || !l->ending || l->wedged) {
		spin_unlock_irqrestore(&l->lock, flags);
		return;
	}
	cancel_delayed_work(&l->watchdog);
	fault = l->fault;
	timed_out = l->timed_out;
	status = l->int_status;
	spin_unlock_irqrestore(&l->lock, flags);

	clean = !fault && !timed_out && lvip_poll(l, lvip_all_idle, LVIP_IDLE_MS);
	if (clean)
		lvip_tables_idle(l);
	else
		ret = lvip_up(l);

	spin_lock_irqsave(&l->lock, flags);
	ps = l->attached;
	if (ret) {
		lvip_wedge(l, ps, -EIO);
		spin_unlock_irqrestore(&l->lock, flags);
		return;
	}
	if (clean) {
		l->finished++;
	} else if (fault) {
		l->faults++;
		l->last_fault = status;
		dev_warn_ratelimited(l->dev, "job faulted (0x%08x); block reset\n", status);
	} else {
		l->timeouts++;
		dev_warn_ratelimited(l->dev, "job stopped after %u ms; block reset\n", l->timeout_ms);
	}
	l->running = l->ending = l->fault = l->timed_out = false;
	l->int_status = 0;
	if (ps) {
		lem_stream_complete(ps->s, &l->slot, clean ? l->slot.bytes : 0, ps->sequence++,
				    ktime_get_ns());
		wake_up_all(&ps->idle);
		if (!ps->stopping)
			queue_delayed_work(l->wq, &ps->collect, 0);
	}
	spin_unlock_irqrestore(&l->lock, flags);
}

/* --- jobs from the ring --- */

/* The kernel's copy of a job: what it names, before anything is held. */
static int lvip_validate(const struct lemnos_vip_job *job)
{
	if (job->reserved || job->slots > LEMNOS_VIP_SLOTS || !job->program ||
	    job->program > LEMNOS_VIP_PROGRAMS)
		return -EINVAL;
	for (u32 i = 0; i < LEMNOS_VIP_SLOTS; i++) {
		const struct lemnos_stream_range *r = &job->slot[i];

		if (i >= job->slots) {
			if (r->addr || r->len)
				return -EINVAL;
			continue;
		}
		if (!r->len || r->len > LEMNOS_VIP_SLOT_SPAN || (r->len & (LVIP_PAGE - 1)) ||
		    (r->addr & (LVIP_PAGE - 1)))
			return -EINVAL;
	}
	return 0;
}

/* Copies, checks, takes, maps and starts what is posted. collect_lock held; process context. */
static long lvip_collect_locked(struct lvip_stream *ps)
{
	struct lem_vip *l = ps->l;
	struct lemnos_vip_job job;
	struct lem_stream_slot slot;
	unsigned long flags;
	u32 words;
	int ret;

	if (ps->stopped)
		return -ENODEV;
	spin_lock_irqsave(&l->lock, flags);
	if (l->wedged || ps->stopping) {
		ret = l->wedged ? -EIO : -ENODEV;
		spin_unlock_irqrestore(&l->lock, flags);
		return ret;
	}
	ret = l->running;
	spin_unlock_irqrestore(&l->lock, flags);
	if (ret)
		return 0;	/* one job at a time; the next is collected when it ends */

	ret = lem_stream_job_peek(ps->s, &job, sizeof(job));
	if (ret <= 0)
		return ret;
	ret = lvip_validate(&job);
	if (ret)
		goto refuse;
	mutex_lock(&l->prog_lock);
	if (!ps->prog[job.program - 1].words) {
		mutex_unlock(&l->prog_lock);
		ret = -EINVAL;
		goto refuse;
	}
	ret = lem_stream_job_take(ps->s, job.buffer, job.identity, job.slot, job.slots, &slot);
	if (ret) {
		mutex_unlock(&l->prog_lock);
		goto refuse;
	}
	/* Nothing runs on the block, and the tables are as between jobs: they are the collector's. */
	for (u32 i = 0; i < job.slots && !ret; i++) {
		u64 addr;

		ret = lem_stream_range_addr(ps->s, &slot.job.range[i], &addr);
		if (!ret)
			ret = lvip_map_slot(l, i, addr, slot.job.range[i].len);
	}
	if (ret) {
		mutex_unlock(&l->prog_lock);
		/* A slot in the pool resolves, but is no data: never started, dropped. */
		lvip_tables_idle(l);
		lem_stream_cancel(ps->s, &slot);
		goto refuse;
	}
	words = lvip_fill_command(l, &ps->prog[job.program - 1]);
	mutex_unlock(&l->prog_lock);
	/* The tables and the commands before the start. */
	wmb();

	spin_lock_irqsave(&l->lock, flags);
	if (l->wedged || ps->stopping) {
		ret = l->wedged ? -EIO : -ENODEV;
		spin_unlock_irqrestore(&l->lock, flags);
		lvip_tables_idle(l);
		lem_stream_cancel(ps->s, &slot);
		return ret;
	}
	l->slot = slot;
	l->running = true;
	l->int_status = 0;
	lvip_wr(l, VR_FE_ADDRESS, LEMNOS_VIP_PROGRAM_ADDR);
	lvip_wr(l, VR_FE_CONTROL, VR_FE_ENABLE | words / 2);
	mod_delayed_work(l->wq, &l->watchdog, msecs_to_jiffies(l->timeout_ms));
	spin_unlock_irqrestore(&l->lock, flags);
	return 0;
refuse:
	spin_lock_irqsave(&l->lock, flags);
	l->refused++;
	spin_unlock_irqrestore(&l->lock, flags);
	return ret;
}

/* LEMNOS_STREAM_IOC_KICK. An errno ends the stream. */
static long lvip_kick(void *priv)
{
	struct lvip_stream *ps = priv;
	long ret;

	mutex_lock(&ps->collect_lock);
	ret = lvip_collect_locked(ps);
	mutex_unlock(&ps->collect_lock);
	return ret;
}

/* After a job ends: pick up what the consumer posted since, without a kick. */
static void lvip_collect_work(struct work_struct *work)
{
	struct lvip_stream *ps = container_of(to_delayed_work(work), struct lvip_stream, collect);
	long ret = lvip_kick(ps);

	if (ret && ret != -ENODEV)
		lem_stream_fault(ps->s, ret);
}

/* --- the stream's lifetime --- */

/*
 * Stops DMA: a job on the block is cut short by a reset, or the block is
 * wedged and the stream keeps its pins. s->ctl held, s->dead set.
 */
static void lvip_stop(void *priv, struct lem_stream *s)
{
	struct lvip_stream *ps = priv;
	struct lem_vip *l = ps->l;
	unsigned long flags;
	bool busy, wedged;

	spin_lock_irqsave(&l->lock, flags);
	ps->stopping = true;
	if (l->running)
		mod_delayed_work(l->wq, &l->watchdog, 0);
	spin_unlock_irqrestore(&l->lock, flags);
	cancel_delayed_work_sync(&ps->collect);

	/* A collection already running finishes first; none starts after. */
	mutex_lock(&ps->collect_lock);
	ps->stopped = true;
	mutex_unlock(&ps->collect_lock);
	/* One that started a job just now: the watchdog is set short. */
	spin_lock_irqsave(&l->lock, flags);
	if (l->running)
		mod_delayed_work(l->wq, &l->watchdog, 0);
	spin_unlock_irqrestore(&l->lock, flags);

	wait_event_timeout(ps->idle, ({
		bool done;

		spin_lock_irqsave(&l->lock, flags);
		done = !l->running || l->wedged;
		spin_unlock_irqrestore(&l->lock, flags);
		done;
	}), msecs_to_jiffies(LVIP_TIMEOUT_MS));

	spin_lock_irqsave(&l->lock, flags);
	busy = l->running && !l->wedged;
	if (busy)
		lvip_wedge(l, ps, -ETIMEDOUT);
	wedged = l->wedged && ps->wedged;
	if (l->attached == ps)
		l->attached = NULL;
	spin_unlock_irqrestore(&l->lock, flags);

	if (wedged) {
		dev_crit(l->dev, "stream wedged: keeping its buffers until a reboot\n");
		lem_stream_wedge(s);
	}
}

static void lvip_release(void *priv)
{
	struct lvip_stream *ps = priv;

	for (u32 i = 0; i < LEMNOS_VIP_PROGRAMS; i++)
		kvfree(ps->prog[i].words);
	kfree(ps);
}

static long lvip_ioctl(void *priv, unsigned int cmd, void __user *arg)
{
	struct lvip_stream *ps = priv;
	struct lem_vip *l = ps->l;
	struct lemnos_vip_status st;
	unsigned long flags;

	switch (cmd) {
	case LEMNOS_VIP_IOC_LOAD:
		return lvip_load(ps, arg);
	case LEMNOS_VIP_IOC_UNLOAD:
		return lvip_unload(ps, arg);
	case LEMNOS_VIP_IOC_STATUS:
		break;
	default:
		return -ENOTTY;
	}
	memset(&st, 0, sizeof(st));
	spin_lock_irqsave(&l->lock, flags);
	st.wedged = l->wedged;
	st.chip = l->cid;
	st.finished = l->finished;
	st.refused = l->refused;
	st.faults = l->faults;
	st.timeouts = l->timeouts;
	st.last_fault = l->last_fault;
	st.resets = l->resets;
	spin_unlock_irqrestore(&l->lock, flags);
	return copy_to_user(arg, &st, sizeof(st)) ? -EFAULT : 0;
}

long lem_vip_create(struct lem_vip *l, struct device *dev, void *args)
{
	struct lemnos_vip_create *a = args;
	struct lem_stream_producer producer = {
		.stop = lvip_stop,
		.job_bytes = sizeof(struct lemnos_vip_job),
		.kick = lvip_kick,
		.ioctl = lvip_ioctl,
		.release = lvip_release,
	};
	struct lvip_stream *ps;
	unsigned long flags;
	long ret;

	BUILD_BUG_ON(sizeof(struct lemnos_vip_job) != 232);
	BUILD_BUG_ON(sizeof(struct lemnos_vip_create) != 64);
	BUILD_BUG_ON(sizeof(struct lemnos_vip_load) != 24);
	BUILD_BUG_ON(sizeof(struct lemnos_vip_status) != 48);
	BUILD_BUG_ON(LEMNOS_VIP_SLOTS > LEM_JOB_HOLDS);
	BUILD_BUG_ON(LEMNOS_VIP_PROGRAM_ADDR + LVIP_CMD_BYTES > (1u << LVIP_MTLB_SHIFT));
	BUILD_BUG_ON((LVIP_PRE_WORDS + LVIP_TAIL_WORDS) * 4 + LEMNOS_VIP_MAX_PROGRAM_BYTES >
		     65535u * 8);

	if (a->stream.job_bytes != sizeof(struct lemnos_vip_job))
		return -EINVAL;
	ps = kzalloc(sizeof(*ps), GFP_KERNEL);
	if (!ps)
		return -ENOMEM;
	ps->l = l;
	init_waitqueue_head(&ps->idle);
	mutex_init(&ps->collect_lock);
	INIT_DELAYED_WORK(&ps->collect, lvip_collect_work);

	mutex_lock(&l->create_lock);
	spin_lock_irqsave(&l->lock, flags);
	ret = l->wedged ? -EIO : l->attached ? -EBUSY : 0;
	if (!ret)
		l->attached = ps;	/* nothing it can do until the stream has a job */
	spin_unlock_irqrestore(&l->lock, flags);
	if (ret)
		goto unlock;

	producer.priv = ps;
	ret = lem_stream_create_producer(dev, &a->stream, &producer, &ps->s);
	if (ret) {
		spin_lock_irqsave(&l->lock, flags);
		l->attached = NULL;
		spin_unlock_irqrestore(&l->lock, flags);
		goto unlock;
	}
	mutex_unlock(&l->create_lock);
	a->chip = l->cid;
	a->mmu = l->mmu;
	return 0;
unlock:
	mutex_unlock(&l->create_lock);
	/* Only a stream that exists owns ps, and its creation failed. */
	kfree(ps);
	return ret;
}

static void lvip_free(struct lem_vip *l)
{
	for (u32 s = 0; s < LEMNOS_VIP_SLOTS; s++)
		for (u32 k = 0; k < LVIP_PER_SLOT; k++)
			if (l->stlb[s][k])
				dma_free_coherent(l->dev, LVIP_STLB_BYTES, l->stlb[s][k],
						  l->stlb_dma[s][k]);
	if (l->pta)
		dma_free_coherent(l->dev, PAGE_SIZE, l->pta, l->pta_dma);
	if (l->cmd)
		dma_free_coherent(l->dev, LVIP_CMD_BYTES, l->cmd, l->cmd_dma);
	if (l->stlb0)
		dma_free_coherent(l->dev, LVIP_STLB_BYTES, l->stlb0, l->stlb0_dma);
	if (l->mtlb)
		dma_free_coherent(l->dev, PAGE_SIZE, l->mtlb, l->mtlb_dma);
}

/* The streams are gone (or wedged). */
void lem_vip_remove(struct lem_vip *l)
{
	unsigned long flags;
	bool wedged;

	spin_lock_irqsave(&l->lock, flags);
	lvip_wr(l, VR_INT_ENABLE, 0);
	l->attached = NULL;
	wedged = l->wedged;
	spin_unlock_irqrestore(&l->lock, flags);
	cancel_delayed_work_sync(&l->watchdog);
	cancel_work_sync(&l->end);
	/* A wedged block may still read its tables and commands. */
	if (!wedged)
		lvip_free(l);
}

static void lvip_destroy_wq(void *wq)
{
	destroy_workqueue(wq);
}

static void lvip_pm_put(void *dev)
{
	pm_runtime_put(dev);
}

static int lvip_alloc(struct lem_vip *l)
{
	l->mtlb = dma_alloc_coherent(l->dev, PAGE_SIZE, &l->mtlb_dma, GFP_KERNEL);
	l->stlb0 = dma_alloc_coherent(l->dev, LVIP_STLB_BYTES, &l->stlb0_dma, GFP_KERNEL);
	l->cmd = dma_alloc_coherent(l->dev, LVIP_CMD_BYTES, &l->cmd_dma, GFP_KERNEL);
	l->pta = dma_alloc_coherent(l->dev, PAGE_SIZE, &l->pta_dma, GFP_KERNEL);
	if (!l->mtlb || !l->stlb0 || !l->cmd || !l->pta)
		return -ENOMEM;
	/* The command buffer's window: its pages, read-only, and nothing else. */
	for (u32 p = 0; p < LVIP_CMD_PAGES; p++)
		l->stlb0[(LEMNOS_VIP_PROGRAM_ADDR >> 12) + p] =
			cpu_to_le32(lvip_pte(l->cmd_dma + (u64)p * LVIP_PAGE, false));
	return 0;
}

struct lem_vip *lem_vip_probe(struct platform_device *pdev)
{
	struct device *dev = &pdev->dev;
	struct reset_control *rst;
	struct clk_bulk_data *clks;
	struct resource *res;
	struct lem_vip *l;
	int ret;

	l = devm_kzalloc(dev, sizeof(*l), GFP_KERNEL);
	if (!l)
		return ERR_PTR(-ENOMEM);
	l->dev = dev;
	spin_lock_init(&l->lock);
	mutex_init(&l->create_lock);
	mutex_init(&l->prog_lock);
	INIT_WORK(&l->end, lvip_end);
	INIT_DELAYED_WORK(&l->watchdog, lvip_watchdog);
	l->timeout_ms = LVIP_TIMEOUT_MS;
	if (test_vip_timeout_ms) {
		l->timeout_ms = test_vip_timeout_ms;
		dev_warn(dev, "test_vip_timeout_ms: jobs get %u ms\n", l->timeout_ms);
	}

	res = platform_get_resource(pdev, IORESOURCE_MEM, 0);
	if (!res || resource_size(res) < VR_WINDOW)
		return ERR_PTR(-EINVAL);
	if (test_vip_fake_regs) {
		void *mem = devm_memremap(dev, res->start, resource_size(res), MEMREMAP_WB);

		if (IS_ERR(mem))
			return ERR_CAST(mem);
		l->regs = (void __iomem *)mem;
		dev_warn(dev, "test_vip_fake_regs: registers are ordinary memory\n");
	} else {
		l->regs = devm_ioremap_resource(dev, res);
		if (IS_ERR(l->regs))
			return ERR_CAST(l->regs);
	}
	l->irq = platform_get_irq(pdev, 0);
	if (l->irq <= 0)
		return ERR_PTR(-EINVAL);
	/* Data at 40 bits through the tables; the tables and commands below 4 GiB. */
	ret = dma_set_mask(dev, DMA_BIT_MASK(40));
	if (!ret)
		ret = dma_set_coherent_mask(dev, DMA_BIT_MASK(32));
	if (ret)
		return ERR_PTR(ret);
	ret = devm_clk_bulk_get_all_enabled(dev, &clks);
	if (ret < 0)
		return ERR_PTR(ret);
	rst = devm_reset_control_array_get_optional_exclusive(dev);
	if (IS_ERR(rst))
		return ERR_CAST(rst);
	ret = reset_control_deassert(rst);
	if (ret)
		return ERR_PTR(ret);
	ret = devm_pm_runtime_enable(dev);
	if (!ret)
		ret = pm_runtime_resume_and_get(dev);
	if (ret)
		return ERR_PTR(ret);
	ret = devm_add_action_or_reset(dev, lvip_pm_put, dev);
	if (ret)
		return ERR_PTR(ret);

	lvip_wr(l, VR_CLOCK, VR_CLOCK_INIT);
	l->cid = lvip_rd(l, VR_CID);
	for (u32 i = 0; i < ARRAY_SIZE(lvip_chips); i++)
		if (lvip_chips[i].cid == l->cid)
			l->mmu = lvip_chips[i].mmu;
	if (!l->mmu) {
		dev_err(dev, "chip 0x%08x: not one whose MMU is known; refused\n", l->cid);
		return ERR_PTR(-ENODEV);
	}

	ret = lvip_alloc(l);
	if (ret)
		goto free;
	ret = lvip_up(l);
	if (ret) {
		dev_err(dev, "block did not reset\n");
		goto free;
	}
	l->wq = alloc_ordered_workqueue("lemnos-vip", 0);
	if (!l->wq) {
		ret = -ENOMEM;
		goto free;
	}
	ret = devm_add_action_or_reset(dev, lvip_destroy_wq, l->wq);
	if (ret)
		goto free;
	ret = devm_request_irq(dev, l->irq, lvip_isr, 0, dev_name(dev), l);
	if (ret)
		goto free;
	dev_info(dev, "chip 0x%08x, MMU set up %s\n", l->cid,
		 l->mmu == LEMNOS_VIP_MMU_PD ? "by page directory" : "by command");
	return l;
free:
	lvip_wr(l, VR_INT_ENABLE, 0);
	lvip_free(l);
	return ERR_PTR(ret);
}
