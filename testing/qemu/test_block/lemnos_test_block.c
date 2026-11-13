// SPDX-License-Identifier: GPL-2.0
/*
 * Two platform devices with nothing behind them, so the module can be tested
 * in a guest with no hardware to claim. One has the compatible the guest's
 * owned= lists, a page of ordinary memory for its register region, and two
 * interrupts from a simulated controller, and a reset "rst" this module
 * provides. The other has a compatible nothing
 * lists, and must be refused. A third, when csi2_phys names memory the guest
 * set aside (memmap=), is a fake camera receiver: see lemnos_test_csi2.h.
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <linux/dma-mapping.h>
#include <linux/err.h>
#include <linux/gfp.h>
#include <linux/interrupt.h>
#include <linux/io.h>
#include <linux/irq_sim.h>
#include <linux/irqdomain.h>
#include <linux/miscdevice.h>
#include <linux/mm.h>
#include <linux/module.h>
#include <linux/platform_device.h>
#include <linux/property.h>
#include <linux/reset-controller.h>
#include <linux/string.h>
#include <linux/sysfs.h>
#include <linux/uaccess.h>
#include <linux/workqueue.h>

#include <uapi/linux/media/raspberrypi/pisp_fe_config.h>

#include "lemnos_test_csi2.h"

#define TB_IRQS 6
#define TB_BLOCK_IRQS 2
#define TB_MAGIC 0x4c454d4e
/* The fake PiSP back end: a window of ordinary memory its driver is told is registers. */
#define TB_PISP_IRQ 2
#define TB_PISP_ORDER 1
#define TB_PISP_BYTES (PAGE_SIZE << TB_PISP_ORDER)
#define TB_PISP_VERSION 0x02252700
/* The fake K230 graphics DMA engine: a page of ordinary memory its driver is told is registers. */
#define TB_GSDMA_IRQ 3
#define TB_TCSI_IRQ 4
/* The fake VIP9000: a page of ordinary memory its driver is told is registers. */
#define TB_VIP_IRQ 5

static unsigned long csi2_phys;
module_param(csi2_phys, ulong, 0444);
MODULE_PARM_DESC(csi2_phys, "Physical address of 16K set aside for the fake receiver's registers");

/* Offsets of the fake receiver's registers, as the module under test writes them. */
#define R_STATUS	0x000
#define R_CTRL		0x028
#define R_ADDR0		0x02c
#define R_STRIDE	0x030
#define R_LENGTH	0x034
#define R_DEBUG		0x038
#define R_ADDR1		0x03c
#define R_FRAME_SIZE	0x040
#define R_INTS		0x034	/* in the third window */
#define R_FORCE		BIT(3)
#define R_INT_DMA	BIT(0)
#define R_INT_FE	BIT(4)

/* Offsets of the fake front end's registers. */
#define FE_R_CONTROL	0x004
#define FE_R_STATUS	0x008
#define FE_R_INT_STATUS	0x01c
#define FE_R_CONFIG	0x040
#define FE_R_INT_EOF	BIT(0)
#define FE_R_INT_SOF	BIT(1)
#define FE_R_BUSY	0x4u	/* FE_STATUS_ACTIVE */
#define FE_R_MIDFRAME	0x5u	/* FE_STATUS_QUEUED | FE_STATUS_ACTIVE, as the board showed */
#define FE_R_ABORT	BIT(1)
#define FE_R_RESET	BIT(2)

static void __iomem *tcsi_regs[4];	/* third: mipi cfg; fourth: the front end */
static int tcsi_irq;
static struct platform_device *tcsi_pdev;
static DEFINE_MUTEX(tcsi_lock);
static u64 tcsi_cur_addr;
static u32 tcsi_cur_len;
static u32 tcsi_busy;		/* ms the line counter keeps moving after FORCE */
static u8 tcsi_pattern;
static u32 tcsi_lines;		/* the next armed frame end's lines; 0: the height */

static void tcsi_tick(struct work_struct *work);
static DECLARE_DELAYED_WORK(tcsi_work, tcsi_tick);
static u64 tfe_stats, tfe_out[2];
static u32 tfe_out_bytes[2];
static u8 tfe_pattern;

static void tfe_busy_tick(struct work_struct *work);
static DECLARE_DELAYED_WORK(tfe_busy_work, tfe_busy_tick);

static void tfe_busy_tick(struct work_struct *work)
{
	writel(0, tcsi_regs[3] + FE_R_STATUS);
}

static u32 tfe_midframe;	/* TCSI_IOC_FE_MIDFRAME's mode; 0: off */
static void tfe_midframe_tick(struct work_struct *work);
static DECLARE_DELAYED_WORK(tfe_midframe_work, tfe_midframe_tick);

/* CONTROL keeps the last value written: that is what is looked at, every ms. */
static void tfe_midframe_tick(struct work_struct *work)
{
	u32 ctrl = readl(tcsi_regs[3] + FE_R_CONTROL);
	bool fed = !(readl(tcsi_regs[0] + R_CTRL) & R_FORCE);

	if ((ctrl & FE_R_RESET) ||
	    ((tfe_midframe & TFE_ABORT_WHILE_FED) && (ctrl & FE_R_ABORT) && fed)) {
		writel(0, tcsi_regs[3] + FE_R_STATUS);
		tfe_midframe = 0;
		return;
	}
	if (tfe_midframe)
		schedule_delayed_work(&tfe_midframe_work, msecs_to_jiffies(1));
}

static u64 tfe_read_addr(unsigned int offset)
{
	return ((u64)readl(tcsi_regs[3] + FE_R_CONFIG + offset + 4) << 32) |
	       readl(tcsi_regs[3] + FE_R_CONFIG + offset);
}

static u32 tfe_read_out_bytes(unsigned int i)
{
	unsigned int at = offsetof(struct pisp_fe_config, ch[i].output.format);
	struct pisp_image_format_config f;

	memcpy_fromio(&f, tcsi_regs[3] + FE_R_CONFIG + at, sizeof(f));
	if (f.stride <= 0)
		return 0;
	return (u32)f.stride * f.height;
}

/* The embedded channel: its registers sit one channel stride (0x40) above the image's. */
#define R_EMB(r)	((r) + 0x40)
static u64 tcsi_emb_addr;
static u32 tcsi_emb_len;
static u32 tcsi_emb_lines = 1;	/* the next embedded frame end's lines; 0: no line */

static void tcsi_tick(struct work_struct *work)
{
	void __iomem *dma = tcsi_regs[0];
	bool again = true;

	mutex_lock(&tcsi_lock);
	if (!tcsi_busy) {
		/* Told to stop, including after "forever": it must not wrap. */
		again = false;
	} else if (readl(dma + R_CTRL) & R_FORCE) {
		writel(readl(dma + R_DEBUG) + 1, dma + R_DEBUG);
		if (tcsi_busy != U32_MAX && !--tcsi_busy)
			again = false;
	}
	mutex_unlock(&tcsi_lock);
	if (again)
		schedule_delayed_work(&tcsi_work, msecs_to_jiffies(1));
}

/*
 * A frame start latches what's armed (ADDR0 last, so it's whatever the module
 * under test finished writing) and a frame end writes that buffer, bounded by
 * LENGTH, as the channel would.
 */
static long tcsi_ioctl(struct file *file, unsigned int cmd, unsigned long arg)
{
	void __iomem *dma = tcsi_regs[0], *cfg = tcsi_regs[2];
	void __user *uarg = (void __user *)arg;

	if (cmd == TCSI_IOC_PEEK_EMB) {
		struct tcsi_regs r = {
			.addr0 = readl(dma + R_EMB(R_ADDR0)),
			.addr1 = readl(dma + R_EMB(R_ADDR1)),
			.length = readl(dma + R_EMB(R_LENGTH)),
			.stride = readl(dma + R_EMB(R_STRIDE)),
			.ctrl = readl(dma + R_EMB(R_CTRL)),
			.frame_size = readl(dma + R_EMB(R_FRAME_SIZE)),
			.debug = readl(dma + R_EMB(R_DEBUG)),
		};

		return copy_to_user(uarg, &r, sizeof(r)) ? -EFAULT : 0;
	}
	if (cmd == TCSI_IOC_PEEK) {
		struct tcsi_regs r = {
			.addr0 = readl(dma + R_ADDR0),
			.addr1 = readl(dma + R_ADDR1),
			.length = readl(dma + R_LENGTH),
			.stride = readl(dma + R_STRIDE),
			.ctrl = readl(dma + R_CTRL),
			.frame_size = readl(dma + R_FRAME_SIZE),
			.debug = readl(dma + R_DEBUG),
		};

		return copy_to_user(uarg, &r, sizeof(r)) ? -EFAULT : 0;
	}
	if (cmd == TCSI_IOC_FE_PEEK) {
		struct tcsi_fe_peek p;

		if (!tcsi_regs[3])
			return -ENODEV;
		if (copy_from_user(&p, uarg, sizeof(p)))
			return -EFAULT;
		if (p.offset > SZ_4K - 4)
			return -EINVAL;
		p.value = readl(tcsi_regs[3] + p.offset);
		return copy_to_user(uarg, &p, sizeof(p)) ? -EFAULT : 0;
	}
	if (cmd == TCSI_IOC_FE_STATUS) {
		u32 v;

		if (copy_from_user(&v, uarg, sizeof(v)))
			return -EFAULT;
		writel(v, tcsi_regs[3] + FE_R_STATUS);
		return 0;
	}
	if (cmd == TCSI_IOC_FE_BUSY) {
		u32 ms;

		if (copy_from_user(&ms, uarg, sizeof(ms)))
			return -EFAULT;
		writel(FE_R_BUSY, tcsi_regs[3] + FE_R_STATUS);
		cancel_delayed_work(&tfe_busy_work);
		if (ms != U32_MAX)
			schedule_delayed_work(&tfe_busy_work, msecs_to_jiffies(ms));
		return 0;
	}
	if (cmd == TCSI_IOC_FE_MIDFRAME) {
		u32 mode;

		if (copy_from_user(&mode, uarg, sizeof(mode)))
			return -EFAULT;
		cancel_delayed_work_sync(&tfe_midframe_work);
		tfe_midframe = mode;
		/* Whatever CONTROL held before this frame is history. */
		writel(0, tcsi_regs[3] + FE_R_CONTROL);
		writel(mode ? FE_R_MIDFRAME : 0, tcsi_regs[3] + FE_R_STATUS);
		if (mode)
			schedule_delayed_work(&tfe_midframe_work, msecs_to_jiffies(1));
		return 0;
	}
	if (cmd == TCSI_IOC_FE_FIRE) {
		u32 status;

		if (copy_from_user(&status, uarg, sizeof(status)))
			return -EFAULT;
		if (status & FE_R_INT_SOF) {
			tfe_stats = tfe_read_addr(offsetof(struct pisp_fe_config, stats_buffer));
			tfe_out[0] = tfe_read_addr(offsetof(struct pisp_fe_config, output_buffer[0]));
			tfe_out[1] = tfe_read_addr(offsetof(struct pisp_fe_config, output_buffer[1]));
			tfe_out_bytes[0] = tfe_read_out_bytes(0);
			tfe_out_bytes[1] = tfe_read_out_bytes(1);
		}
		if (status & FE_R_INT_EOF) {
			++tfe_pattern;
			for (int i = 0; i < 2; i++)
				if (tfe_out[i] && tfe_out_bytes[i])
					memset(phys_to_virt(tfe_out[i]), tfe_pattern, tfe_out_bytes[i]);
			if (tfe_stats)
				memset(phys_to_virt(tfe_stats), tfe_pattern,
				       sizeof(struct pisp_statistics));
			tfe_stats = 0;
			tfe_out[0] = tfe_out[1] = 0;
		}
		writel(status, tcsi_regs[3] + FE_R_INT_STATUS);
		writel(R_INT_FE, cfg + R_INTS);
		return irq_set_irqchip_state(tcsi_irq, IRQCHIP_STATE_PENDING, true);
	}
	if (cmd == TCSI_IOC_LINES)
		return get_user(tcsi_lines, (u32 __user *)uarg);
	if (cmd == TCSI_IOC_EMB_LINES)
		return get_user(tcsi_emb_lines, (u32 __user *)uarg);
	if (cmd == TCSI_IOC_BUSY) {
		u32 ms;

		if (copy_from_user(&ms, uarg, sizeof(ms)))
			return -EFAULT;
		mutex_lock(&tcsi_lock);
		tcsi_busy = ms;
		mutex_unlock(&tcsi_lock);
		if (ms)
			schedule_delayed_work(&tcsi_work, msecs_to_jiffies(1));
		return 0;
	}
	if (cmd == TCSI_IOC_FIRE) {
		u32 status;

		if (copy_from_user(&status, uarg, sizeof(status)))
			return -EFAULT;
		if (status & TCSI_FS) {
			tcsi_cur_addr = (u64)readl(dma + R_ADDR1) << 36 |
					(u64)readl(dma + R_ADDR0) << 4;
			tcsi_cur_len = readl(dma + R_LENGTH) << 4;
		}
		if (status & TCSI_EMB_FS) {
			tcsi_emb_addr = (u64)readl(dma + R_EMB(R_ADDR1)) << 36 |
					(u64)readl(dma + R_EMB(R_ADDR0)) << 4;
			tcsi_emb_len = readl(dma + R_EMB(R_LENGTH)) << 4;
		}
		if ((status & TCSI_EMB_FE_ACK) && tcsi_emb_addr) {
			u32 dbg = readl(dma + R_EMB(R_DEBUG));
			u32 lines = tcsi_emb_lines;

			if (lines)
				memset(phys_to_virt(tcsi_emb_addr), ++tcsi_pattern, tcsi_emb_len);
			tcsi_emb_addr = 0;
			tcsi_emb_lines = 1;
			writel(((dbg >> 16) + 1) << 16 | ((dbg + lines) & 0xffff), dma + R_EMB(R_DEBUG));
		}
		if ((status & TCSI_FE_ACK) && tcsi_cur_addr) {
			u32 dbg = readl(dma + R_DEBUG);
			u32 lines = tcsi_lines ? tcsi_lines : readl(dma + R_FRAME_SIZE) >> 16;

			memset(phys_to_virt(tcsi_cur_addr), ++tcsi_pattern, tcsi_cur_len);
			tcsi_cur_addr = 0;
			tcsi_lines = 0;
			/* Frames in the high half, lines in the low, each wrapping. */
			writel(((dbg >> 16) + 1) << 16 | ((dbg + lines) & 0xffff), dma + R_DEBUG);
		}
		writel(status, dma + R_STATUS);
		writel(R_INT_DMA, cfg + R_INTS);
		return irq_set_irqchip_state(tcsi_irq, IRQCHIP_STATE_PENDING, true);
	}
	return -ENOTTY;
}

static const struct file_operations tcsi_fops = {
	.owner = THIS_MODULE,
	.unlocked_ioctl = tcsi_ioctl,
	.compat_ioctl = compat_ptr_ioctl,
	.llseek = noop_llseek,
};

static struct miscdevice tcsi_misc = {
	.minor = MISC_DYNAMIC_MINOR,
	.name = "lemnos-test-csi2",
	.fops = &tcsi_fops,
};

static const struct property_entry tcsi_props[] = {
	PROPERTY_ENTRY_STRING("compatible", "lemnos,test-csi2"),
	{ }
};

static struct fwnode_handle *tb_fwnode;
static struct irq_domain *tb_domain;
static int tb_irq[TB_IRQS];
static struct page *tb_page;
static struct platform_device *tb_owned, *tb_other, *tb_pisp;
static struct page *tb_pisp_pages;

static const struct property_entry tb_rst_props[] = {
	PROPERTY_ENTRY_U32("#reset-cells", 1),
	{ }
};

static const struct software_node tb_rst_node = {
	.name = "lemnos-test-reset",
	.properties = tb_rst_props,
};

static bool tb_asserted;

static int tb_rst_assert(struct reset_controller_dev *rc, unsigned long id)
{
	tb_asserted = true;
	return 0;
}

static int tb_rst_deassert(struct reset_controller_dev *rc, unsigned long id)
{
	tb_asserted = false;
	return 0;
}

static int tb_rst_status(struct reset_controller_dev *rc, unsigned long id)
{
	return tb_asserted;
}

static int tb_rst_reset(struct reset_controller_dev *rc, unsigned long id)
{
	return 0;
}

static const struct reset_control_ops tb_rst_ops = {
	.assert = tb_rst_assert,
	.deassert = tb_rst_deassert,
	.status = tb_rst_status,
	.reset = tb_rst_reset,
};

static struct reset_controller_dev tb_rst = {
	.ops = &tb_rst_ops,
	.owner = THIS_MODULE,
	.nr_resets = 1,
	.fwnode_reset_n_cells = 1,
};

/*
 * The reset line's provider is a device with a driver, as a real one is: a
 * consumer that refers to a node nothing has probed is deferred.
 */
static int tb_rst_probe(struct platform_device *pdev)
{
	tb_rst.dev = &pdev->dev;
	tb_rst.fwnode = dev_fwnode(&pdev->dev);
	return devm_reset_controller_register(&pdev->dev, &tb_rst);
}

static struct platform_driver tb_rst_driver = {
	.probe = tb_rst_probe,
	.driver = {
		.name = "lemnos-test-reset",
	},
};

static struct platform_device *tb_rst_pdev;
static bool tb_rst_driver_up;

static const struct property_entry tb_owned_props[] = {
	PROPERTY_ENTRY_STRING("compatible", "lemnos,test-block"),
	PROPERTY_ENTRY_REF("resets", &tb_rst_node, 0),
	PROPERTY_ENTRY_STRING("reset-names", "rst"),
	{ }
};

static const struct property_entry tb_other_props[] = {
	PROPERTY_ENTRY_STRING("compatible", "lemnos,not-owned"),
	{ }
};

static const struct property_entry tb_pisp_props[] = {
	PROPERTY_ENTRY_STRING("compatible", "lemnos,test-pisp-be"),
	{ }
};

static u32 __iomem *tb_pisp_regs(void)
{
	return page_address(tb_pisp_pages);
}

/* "offset value": sets a register of the fake block, as hardware would (hex). */
static ssize_t poke_store(struct device *dev, struct device_attribute *attr, const char *buf,
			  size_t count)
{
	unsigned long offset, value;
	char tmp[32];
	char *sep;

	strscpy(tmp, buf, sizeof(tmp));
	sep = strchr(tmp, ' ');
	if (!sep)
		return -EINVAL;
	*sep++ = 0;
	if (kstrtoul(strim(tmp), 16, &offset) || kstrtoul(strim(sep), 16, &value))
		return -EINVAL;
	if (offset >= TB_PISP_BYTES || offset % 4)
		return -EINVAL;
	writel((u32)value, (char __iomem *)tb_pisp_regs() + offset);
	return count;
}
static DEVICE_ATTR_WO(poke);

/* Raises the fake block's interrupt. */
static ssize_t irq_store(struct device *dev, struct device_attribute *attr, const char *buf,
			 size_t count)
{
	int ret = irq_set_irqchip_state(tb_irq[TB_PISP_IRQ], IRQCHIP_STATE_PENDING, true);

	return ret ?: count;
}
static DEVICE_ATTR_WO(irq);

/* Whether a page the driver gave the block is still allocated: write its address, read its count. */
static u64 tb_pisp_page_addr;

static ssize_t page_refs_store(struct device *dev, struct device_attribute *attr, const char *buf,
			       size_t count)
{
	if (kstrtou64(buf, 16, &tb_pisp_page_addr))
		return -EINVAL;
	return count;
}

static ssize_t page_refs_show(struct device *dev, struct device_attribute *attr, char *buf)
{
	unsigned long pfn = PHYS_PFN(tb_pisp_page_addr);

	return sysfs_emit(buf, "%d\n", pfn_valid(pfn) ? page_ref_count(pfn_to_page(pfn)) : -1);
}
static DEVICE_ATTR_RW(page_refs);

/* The fake block's register window, for the test to compare with what it asked for. */
static ssize_t regs_read(struct file *f, struct kobject *kobj, const struct bin_attribute *a,
			 char *buf, loff_t off, size_t count)
{
	if (off >= TB_PISP_BYTES)
		return 0;
	count = min_t(size_t, count, TB_PISP_BYTES - off);
	memcpy_fromio(buf, (char __iomem *)tb_pisp_regs() + off, count);
	return count;
}
static const BIN_ATTR_RO(regs, TB_PISP_BYTES);

static struct attribute *tb_pisp_attrs[] = { &dev_attr_poke.attr, &dev_attr_irq.attr,
					   &dev_attr_page_refs.attr, NULL };
static const struct bin_attribute *const tb_pisp_bin_attrs[] = { &bin_attr_regs, NULL };
static const struct attribute_group tb_pisp_group = {
	.attrs = tb_pisp_attrs,
	.bin_attrs = tb_pisp_bin_attrs,
};


/*
 * The fake K230 graphics DMA engine. Its registers are a page of ordinary
 * memory, looked at every millisecond: a 1 in CTRL starts the chain at
 * LLI_BASE, which is walked as the engine would, copying each plane through
 * the kernel's mapping of the bus addresses it names (no IOMMU in this guest:
 * a bus address is physical), then DONE is set and the interrupt raised.
 * With `hang` set, a started chain never finishes and its progress register
 * keeps moving, for `hang` ms after the channel is turned off (~0: forever).
 */
#define TGD_CH_EN	0x00
#define TGD_CH_EN_BIT	BIT(4)
#define TGD_INT_STAT	0x08
#define TGD_CTRL	0x10
#define TGD_LLI_BASE	0x14
#define TGD_CUR_DADDR	0x44
#define TGD_DONE	BIT(18)

static struct platform_device *tb_gsdma;
static struct page *tb_gsdma_page;
static DEFINE_MUTEX(tgd_lock);
static u32 tgd_hang, tgd_left, tgd_jobs, tgd_word0[3], tgd_errors;
static bool tgd_hanging;

static void tgd_tick(struct work_struct *work);
static DECLARE_DELAYED_WORK(tgd_work, tgd_tick);

static u32 __iomem *tgd_regs(void)
{
	return page_address(tb_gsdma_page);
}

static u32 tgd_rd(unsigned int off)
{
	return readl((u8 __iomem *)tgd_regs() + off);
}

static void tgd_wr(unsigned int off, u32 v)
{
	writel(v, (u8 __iomem *)tgd_regs() + off);
}

static void *tgd_at(u64 bus)
{
	return pfn_valid(PHYS_PFN(bus)) ? phys_to_virt(bus) : NULL;
}

/* One descriptor: its plane, mirrored, then turned clockwise. */
static void tgd_plane(const u32 *d)
{
	static const u32 bpe_of[4] = { 1, 2, 3, 4 };
	u32 turn = d[0] & 3, bpe = bpe_of[(d[0] >> 8) & 3];
	bool mx = d[0] & BIT(2), my = d[0] & BIT(3);
	u32 w = d[2] & 0xffff, h = d[2] >> 16, sp = d[3] & 0xffff, dp = d[3] >> 16;
	u8 *src = tgd_at(d[1]), *dst = tgd_at(d[4]);

	if (!src || !dst) {
		tgd_errors++;
		return;
	}
	for (u32 y = 0; y < h; y++) {
		for (u32 x = 0; x < w; x++) {
			u32 ax = mx ? w - 1 - x : x, ay = my ? h - 1 - y : y, dx, dy;

			switch (turn) {
			case 0: dx = ax; dy = ay; break;
			case 1: dx = h - 1 - ay; dy = ax; break;
			case 2: dx = w - 1 - ax; dy = h - 1 - ay; break;
			default: dx = ay; dy = w - 1 - ax; break;
			}
			memcpy(dst + (u64)dy * dp + (u64)dx * bpe, src + (u64)y * sp + (u64)x * bpe,
			       bpe);
		}
	}
}

static void tgd_run(void)
{
	u64 at = tgd_rd(TGD_LLI_BASE);

	for (u32 i = 0; i < 3 && at; i++) {
		u32 *d = tgd_at(at);

		if (!d) {
			tgd_errors++;
			break;
		}
		tgd_word0[i] = d[0];
		tgd_plane(d);
		if (d[0] & BIT(30))
			break;
		at = d[5];
	}
	tgd_jobs++;
}

static void tgd_tick(struct work_struct *work)
{
	bool raise = false, on;

	mutex_lock(&tgd_lock);
	if (tgd_rd(TGD_CTRL) == 1 && !tgd_hanging) {
		tgd_wr(TGD_CTRL, 0);
		tgd_wr(TGD_INT_STAT, 0);
		if (tgd_hang) {
			tgd_hanging = true;
			tgd_left = tgd_hang;
		} else {
			tgd_run();
			tgd_wr(TGD_INT_STAT, TGD_DONE);
			raise = true;
		}
	}
	if (tgd_hanging) {
		/* It moves while on; once off, for `left` ms more. */
		if (!(tgd_rd(TGD_CH_EN) & TGD_CH_EN_BIT) && tgd_left != U32_MAX && !tgd_left--)
			tgd_hanging = false;
		else
			tgd_wr(TGD_CUR_DADDR, tgd_rd(TGD_CUR_DADDR) + 1);
	}
	/* Looked at every ms only while a driver has the channel on, so it costs nothing otherwise. */
	on = tgd_hanging || (tgd_rd(TGD_CH_EN) & TGD_CH_EN_BIT);
	mutex_unlock(&tgd_lock);
	if (raise)
		irq_set_irqchip_state(tb_irq[TB_GSDMA_IRQ], IRQCHIP_STATE_PENDING, true);
	schedule_delayed_work(&tgd_work, msecs_to_jiffies(on ? 1 : 50));
}

/* "N": jobs from now on hang, moving for N ms after the channel is off (4294967295: forever). */
static ssize_t hang_store(struct device *dev, struct device_attribute *attr, const char *buf,
			  size_t count)
{
	u32 v;

	if (kstrtou32(buf, 0, &v))
		return -EINVAL;
	mutex_lock(&tgd_lock);
	tgd_hang = v;
	mutex_unlock(&tgd_lock);
	return count;
}
static DEVICE_ATTR_WO(hang);

/* "jobs errors word0 word0 word0": chains run, addresses it couldn't reach, the last descriptors' first words. */
static ssize_t ran_show(struct device *dev, struct device_attribute *attr, char *buf)
{
	ssize_t n;

	mutex_lock(&tgd_lock);
	n = sysfs_emit(buf, "%u %u %x %x %x\n", tgd_jobs, tgd_errors, tgd_word0[0], tgd_word0[1],
		       tgd_word0[2]);
	mutex_unlock(&tgd_lock);
	return n;
}
static DEVICE_ATTR_RO(ran);

static struct attribute *tb_gsdma_attrs[] = { &dev_attr_hang.attr, &dev_attr_ran.attr, NULL };
static const struct attribute_group tb_gsdma_group = { .attrs = tb_gsdma_attrs };

static const struct property_entry tb_gsdma_props[] = {
	PROPERTY_ENTRY_STRING("compatible", "lemnos,test-gsdma"),
	{ }
};

/*
 * The fake VIP9000. Its registers are a page of ordinary memory, looked at
 * every millisecond while a driver has it. A 1 in the command fetcher's enable
 * bit starts the commands at its address, which are run as the block would,
 * through its MMU when that is on: the root table from the page directory
 * register, or from the page table array a set-up command selected; each
 * second-level entry present, and writable for a write. A miss is the MMU's
 * exception: interrupt status bit 30, the fetcher stays busy until a reset.
 * Commands: state loads, NOP, STALL and END; END makes it idle and raises the
 * interrupt with the events the commands asked for. Three states of its own
 * copy memory (0x5000 source, 0x5001 destination, 0x5002 bytes: copies) and
 * hang (0x5003). A reset is seen as the clock register's idle bits coming back
 * after the fake cleared them when the MMU went on; it turns the MMU off and
 * makes the block idle. With `stuck`, resets are ignored.
 */
#define TVIP_CLOCK	0x000
#define TVIP_IDLE	0x004
#define TVIP_INT	0x010
#define TVIP_CID	0x030
#define TVIP_MMU	0x388
#define TVIP_PTA_LOW	0x38c
#define TVIP_PTA_HIGH	0x390
#define TVIP_FE_CTL	0x3a4
#define TVIP_PD	0x3b4
#define TVIP_FE_ADDR	0x654
#define TVIP_IDLE_ALL	0x7fffffffu
#define TVIP_CLOCK_BITS	0x00030000u

static struct platform_device *tb_vip;
static struct page *tb_vip_page;
static DEFINE_MUTEX(tvip_lock);
static u32 tvip_cid = 0x1000003b;
static bool tvip_stuck, tvip_marked, tvip_pta, tvip_busy;
static u32 tvip_jobs, tvip_faults, tvip_resets, tvip_flushes, tvip_copied, tvip_errors;
static u32 tvip_src, tvip_dst;

static void tvip_tick(struct work_struct *work);
static DECLARE_DELAYED_WORK(tvip_work, tvip_tick);

static u32 tvip_rd(unsigned int off)
{
	return readl((u8 __iomem *)page_address(tb_vip_page) + off);
}

static void tvip_wr(unsigned int off, u32 v)
{
	writel(v, (u8 __iomem *)page_address(tb_vip_page) + off);
}

static void *tvip_phys(u64 pa)
{
	return pfn_valid(PHYS_PFN(pa)) ? phys_to_virt(pa) : NULL;
}

/* The kernel's pointer for the block's address `va`, and the bytes to its page's end; NULL: a fault. */
static u8 *tvip_at(u32 va, bool write, u32 *left)
{
	u32 mmu = tvip_rd(TVIP_MMU);
	u64 pa = va;

	if (mmu & BIT(0)) {
		u64 root;
		u32 *mtlb, *stlb, m, pte;

		if (mmu & BIT(5)) {
			root = (u64)(tvip_rd(TVIP_PD) >> 4) << 12;
		} else {
			u32 *pta;

			if (!tvip_pta)
				return NULL;
			pta = tvip_phys(tvip_rd(TVIP_PTA_LOW) | (u64)tvip_rd(TVIP_PTA_HIGH) << 32);
			if (!pta)
				return NULL;
			root = (pta[0] & ~0xfffu) | (u64)pta[1] << 32;
		}
		mtlb = tvip_phys(root);
		if (!mtlb)
			return NULL;
		m = le32_to_cpu(READ_ONCE(mtlb[va >> 24]));
		if (!(m & BIT(0)))
			return NULL;
		stlb = tvip_phys(m & ~0x3fu);
		if (!stlb)
			return NULL;
		pte = le32_to_cpu(READ_ONCE(stlb[(va >> 12) & 0xfff]));
		if (!(pte & BIT(0)) || (write && !(pte & BIT(2))))
			return NULL;
		pa = (pte & ~0xfffu) | (u64)((pte >> 4) & 0xff) << 32 | (va & 0xfff);
	}
	*left = 4096 - (va & 0xfff);
	return tvip_phys(pa);
}

static bool tvip_copy(u32 len)
{
	u32 src = tvip_src, dst = tvip_dst;

	while (len) {
		u32 ls, ld, n;
		u8 *s = tvip_at(src, false, &ls), *d = tvip_at(dst, true, &ld);

		if (!s || !d)
			return false;
		n = min3(len, ls, ld);
		memcpy(d, s, n);
		src += n;
		dst += n;
		len -= n;
		tvip_copied += n;
	}
	return true;
}

enum { TVIP_END, TVIP_FAULT, TVIP_HANG };

/* Runs what the fetcher was started on. */
static int tvip_run(u32 *events)
{
	u32 va = tvip_rd(TVIP_FE_ADDR), left = (tvip_rd(TVIP_FE_CTL) & 0xffff) * 2;

	while (left) {
		u32 l, w, op, n;
		u32 *p = (u32 *)tvip_at(va, false, &l);

		if (!p)
			return TVIP_FAULT;
		w = le32_to_cpu(p[0]);
		op = w >> 27;
		if (op == 2)
			return TVIP_END;
		if (op == 3 || op == 9) {
			n = 2;
		} else if (op == 1) {
			u32 addr = w & 0xffff, count = (w >> 16) & 0x3ff;

			if (!count)
				count = 1024;
			for (u32 i = 0; i < count; i++) {
				u32 vl, v, *q = (u32 *)tvip_at(va + 4 + i * 4, false, &vl);

				if (!q)
					return TVIP_FAULT;
				v = le32_to_cpu(*q);
				switch (addr + i) {
				case 0x0061:
					tvip_flushes++;
					break;
				case 0x0e01:
					*events |= BIT(v & 0x1f);
					break;
				case 0x0e12:
					if (!(tvip_rd(TVIP_MMU) & BIT(0)))
						tvip_pta = true;
					break;
				case 0x5000:
					tvip_src = v;
					break;
				case 0x5001:
					tvip_dst = v;
					break;
				case 0x5002:
					if (!tvip_copy(v))
						return TVIP_FAULT;
					break;
				case 0x5003:
					return TVIP_HANG;
				}
			}
			n = ALIGN(1 + count, 2);
		} else {
			tvip_errors++;
			return TVIP_HANG;
		}
		if (n > left)
			return TVIP_HANG;
		left -= n;
		va += n * 4;
	}
	return TVIP_HANG;
}

static void tvip_tick(struct work_struct *work)
{
	bool raise = false, on;

	mutex_lock(&tvip_lock);
	if (!tvip_stuck) {
		u32 clock = tvip_rd(TVIP_CLOCK);

		if (tvip_marked && (clock & TVIP_CLOCK_BITS) == TVIP_CLOCK_BITS) {
			tvip_wr(TVIP_MMU, 0);
			tvip_wr(TVIP_FE_CTL, 0);
			tvip_wr(TVIP_IDLE, TVIP_IDLE_ALL);
			tvip_pta = tvip_busy = tvip_marked = false;
			tvip_resets++;
		} else if (!tvip_marked && (tvip_rd(TVIP_MMU) & BIT(0))) {
			tvip_wr(TVIP_CLOCK, clock & ~TVIP_CLOCK_BITS);
			tvip_marked = true;
		}
	}
	if (!tvip_busy && (tvip_rd(TVIP_FE_CTL) & BIT(16))) {
		u32 events = 0;
		int r;

		tvip_wr(TVIP_IDLE, TVIP_IDLE_ALL & ~BIT(0));
		r = tvip_run(&events);
		tvip_wr(TVIP_FE_CTL, 0);
		if (tvip_rd(TVIP_MMU) & BIT(0))
			tvip_jobs++;
		if (r == TVIP_END) {
			tvip_wr(TVIP_IDLE, TVIP_IDLE_ALL);
			if (events) {
				tvip_wr(TVIP_INT, tvip_rd(TVIP_INT) | events);
				raise = true;
			}
		} else {
			tvip_busy = true;
			if (r == TVIP_FAULT) {
				tvip_faults++;
				tvip_wr(TVIP_INT, tvip_rd(TVIP_INT) | BIT(30));
				raise = true;
			}
		}
	}
	on = tvip_rd(TVIP_CLOCK) || tvip_marked;
	mutex_unlock(&tvip_lock);
	if (raise)
		irq_set_irqchip_state(tb_irq[TB_VIP_IRQ], IRQCHIP_STATE_PENDING, true);
	schedule_delayed_work(&tvip_work, msecs_to_jiffies(on ? 1 : 50));
}

/* The customer id the block reports; set before the driver binds. */
static ssize_t cid_store(struct device *dev, struct device_attribute *attr, const char *buf,
			 size_t count)
{
	u32 v;

	if (kstrtou32(buf, 0, &v))
		return -EINVAL;
	mutex_lock(&tvip_lock);
	tvip_cid = v;
	tvip_wr(TVIP_CID, v);
	mutex_unlock(&tvip_lock);
	return count;
}

static ssize_t cid_show(struct device *dev, struct device_attribute *attr, char *buf)
{
	return sysfs_emit(buf, "0x%08x\n", tvip_cid);
}
static DEVICE_ATTR_RW(cid);

/* "1": resets are ignored from now on (until "0"). */
static ssize_t stuck_store(struct device *dev, struct device_attribute *attr, const char *buf,
			   size_t count)
{
	bool v;

	if (kstrtobool(buf, &v))
		return -EINVAL;
	mutex_lock(&tvip_lock);
	tvip_stuck = v;
	mutex_unlock(&tvip_lock);
	return count;
}
static DEVICE_ATTR_WO(stuck);

/* "jobs faults resets flushes copied errors": since the module loaded. */
static ssize_t vip_ran_show(struct device *dev, struct device_attribute *attr, char *buf)
{
	ssize_t n;

	mutex_lock(&tvip_lock);
	n = sysfs_emit(buf, "%u %u %u %u %u %u\n", tvip_jobs, tvip_faults, tvip_resets,
		       tvip_flushes, tvip_copied, tvip_errors);
	mutex_unlock(&tvip_lock);
	return n;
}
static struct device_attribute dev_attr_vip_ran = __ATTR(ran, 0444, vip_ran_show, NULL);

static struct attribute *tb_vip_attrs[] = { &dev_attr_cid.attr, &dev_attr_stuck.attr,
					    &dev_attr_vip_ran.attr, NULL };
static const struct attribute_group tb_vip_group = { .attrs = tb_vip_attrs };

static const struct property_entry tb_vip_props[] = {
	PROPERTY_ENTRY_STRING("compatible", "lemnos,test-vip"),
	{ }
};

static void tb_teardown(void)
{
	if (!IS_ERR_OR_NULL(tb_vip)) {
		sysfs_remove_group(&tb_vip->dev.kobj, &tb_vip_group);
		platform_device_unregister(tb_vip);
	}
	cancel_delayed_work_sync(&tvip_work);
	if (tb_vip_page)
		__free_page(tb_vip_page);
	if (!IS_ERR_OR_NULL(tb_gsdma)) {
		sysfs_remove_group(&tb_gsdma->dev.kobj, &tb_gsdma_group);
		platform_device_unregister(tb_gsdma);
	}
	cancel_delayed_work_sync(&tgd_work);
	if (tb_gsdma_page)
		__free_page(tb_gsdma_page);
	if (!IS_ERR_OR_NULL(tb_pisp)) {
		sysfs_remove_group(&tb_pisp->dev.kobj, &tb_pisp_group);
		platform_device_unregister(tb_pisp);
	}
	if (tb_pisp_pages)
		__free_pages(tb_pisp_pages, TB_PISP_ORDER);
	if (tcsi_misc.minor != MISC_DYNAMIC_MINOR)
		misc_deregister(&tcsi_misc);
	cancel_delayed_work_sync(&tcsi_work);
	cancel_delayed_work_sync(&tfe_busy_work);
	cancel_delayed_work_sync(&tfe_midframe_work);
	if (!IS_ERR_OR_NULL(tcsi_pdev))
		platform_device_unregister(tcsi_pdev);
	for (int i = 0; i < ARRAY_SIZE(tcsi_regs); i++)
		if (tcsi_regs[i])
			iounmap(tcsi_regs[i]);
	if (!IS_ERR_OR_NULL(tb_other))
		platform_device_unregister(tb_other);
	if (!IS_ERR_OR_NULL(tb_owned))
		platform_device_unregister(tb_owned);
	if (!IS_ERR_OR_NULL(tb_rst_pdev))
		platform_device_unregister(tb_rst_pdev);
	tb_rst_pdev = NULL;
	if (tb_rst_driver_up)
		platform_driver_unregister(&tb_rst_driver);
	tb_rst_driver_up = false;
	for (int i = 0; i < TB_IRQS; i++)
		if (tb_irq[i])
			irq_dispose_mapping(tb_irq[i]);
	if (!IS_ERR_OR_NULL(tb_domain))
		irq_domain_remove_sim(tb_domain);
	if (tb_fwnode)
		irq_domain_free_fwnode(tb_fwnode);
	if (tb_page)
		__free_page(tb_page);
}

/* Filled by tb_init; platform_device_register_full copies them, and the stack frame stays small. */
static struct resource pisp_res[2] __initdata;
static struct resource gsdma_res[2] __initdata;
static struct resource vip_res[2] __initdata;

static int __init tb_init(void)
{
	struct resource res[1 + TB_BLOCK_IRQS];
	struct platform_device_info pisp = {
		.name = "lemnos-test-pisp",
		.id = PLATFORM_DEVID_NONE,
		.res = pisp_res,
		.num_res = ARRAY_SIZE(pisp_res),
		.dma_mask = DMA_BIT_MASK(64),
		.properties = tb_pisp_props,
	};
	struct platform_device_info gsdma = {
		.name = "lemnos-test-gsdma",
		.id = PLATFORM_DEVID_NONE,
		.res = gsdma_res,
		.num_res = ARRAY_SIZE(gsdma_res),
		.dma_mask = DMA_BIT_MASK(64),
		.properties = tb_gsdma_props,
	};
	struct platform_device_info vip = {
		.name = "lemnos-test-vip",
		.id = PLATFORM_DEVID_NONE,
		.res = vip_res,
		.num_res = ARRAY_SIZE(vip_res),
		.dma_mask = DMA_BIT_MASK(64),
		.properties = tb_vip_props,
	};
	struct platform_device_info info = {
		.name = "lemnos-test-block",
		.id = PLATFORM_DEVID_NONE,
		.res = res,
		.num_res = ARRAY_SIZE(res),
		.dma_mask = DMA_BIT_MASK(64),
		.properties = tb_owned_props,
	};
	struct platform_device_info other = {
		.name = "lemnos-test-other",
		.id = PLATFORM_DEVID_NONE,
		.dma_mask = DMA_BIT_MASK(64),
		.properties = tb_other_props,
	};
	int ret = -ENOMEM;

	tb_page = alloc_page(GFP_KERNEL | __GFP_ZERO);
	if (!tb_page)
		goto fail;
	*(u32 *)page_address(tb_page) = TB_MAGIC;

	tb_fwnode = irq_domain_alloc_named_fwnode("lemnos-test-block");
	if (!tb_fwnode)
		goto fail;
	tb_domain = irq_domain_create_sim(tb_fwnode, TB_IRQS);
	if (IS_ERR(tb_domain)) {
		ret = PTR_ERR(tb_domain);
		goto fail;
	}
	memset(res, 0, sizeof(res));
	res[0] = (struct resource)DEFINE_RES_MEM(page_to_phys(tb_page), PAGE_SIZE);
	for (int i = 0; i < TB_IRQS; i++) {
		tb_irq[i] = irq_create_mapping(tb_domain, i);
		if (!tb_irq[i]) {
			ret = -EINVAL;
			goto fail;
		}
		if (i < TB_BLOCK_IRQS)
			res[1 + i] = (struct resource)DEFINE_RES_IRQ(tb_irq[i]);
	}
	ret = platform_driver_register(&tb_rst_driver);
	if (ret)
		goto fail;
	tb_rst_driver_up = true;
	tb_rst_pdev = platform_device_register_full(&(struct platform_device_info){
		.name = "lemnos-test-reset",
		.id = PLATFORM_DEVID_NONE,
		.swnode = &tb_rst_node,
	});
	if (IS_ERR(tb_rst_pdev)) {
		ret = PTR_ERR(tb_rst_pdev);
		tb_rst_pdev = NULL;
		goto fail;
	}
	if (!tb_rst_pdev->dev.driver) {
		ret = -ENODEV;
		goto fail;
	}
	tb_owned = platform_device_register_full(&info);
	if (IS_ERR(tb_owned)) {
		ret = PTR_ERR(tb_owned);
		goto fail;
	}
	tb_other = platform_device_register_full(&other);
	if (IS_ERR(tb_other)) {
		ret = PTR_ERR(tb_other);
		goto fail;
	}
	if (csi2_phys) {
		struct resource cr[5];
		struct platform_device_info csi = {
			.name = "lemnos-test-csi2",
			.id = PLATFORM_DEVID_NONE,
			.res = cr,
			.num_res = ARRAY_SIZE(cr),
			.dma_mask = DMA_BIT_MASK(64),
			.properties = tcsi_props,
		};

		memset(cr, 0, sizeof(cr));
		for (int i = 0; i < 4; i++) {
			cr[i] = (struct resource)DEFINE_RES_MEM(csi2_phys + i * SZ_4K, SZ_4K);
			tcsi_regs[i] = ioremap(csi2_phys + i * SZ_4K, SZ_4K);
			if (!tcsi_regs[i]) {
				ret = -ENOMEM;
				goto fail;
			}
			memset_io(tcsi_regs[i], 0, SZ_4K);
		}
		tcsi_irq = tb_irq[TB_TCSI_IRQ];
		cr[4] = (struct resource)DEFINE_RES_IRQ(tcsi_irq);
		tcsi_pdev = platform_device_register_full(&csi);
		if (IS_ERR(tcsi_pdev)) {
			ret = PTR_ERR(tcsi_pdev);
			goto fail;
		}
		ret = misc_register(&tcsi_misc);
		if (ret) {
			tcsi_misc.minor = MISC_DYNAMIC_MINOR;
			goto fail;
		}
	}

	tb_pisp_pages = alloc_pages(GFP_KERNEL | __GFP_ZERO, TB_PISP_ORDER);
	if (!tb_pisp_pages) {
		ret = -ENOMEM;
		goto fail;
	}
	writel(TB_PISP_VERSION, tb_pisp_regs());
	pisp_res[0] = (struct resource)DEFINE_RES_MEM(page_to_phys(tb_pisp_pages), TB_PISP_BYTES);
	pisp_res[1] = (struct resource)DEFINE_RES_IRQ(tb_irq[TB_PISP_IRQ]);
	tb_pisp = platform_device_register_full(&pisp);
	if (IS_ERR(tb_pisp)) {
		ret = PTR_ERR(tb_pisp);
		goto fail;
	}
	ret = sysfs_create_group(&tb_pisp->dev.kobj, &tb_pisp_group);
	if (ret)
		goto fail;

	tb_gsdma_page = alloc_page(GFP_KERNEL | __GFP_ZERO);
	if (!tb_gsdma_page) {
		ret = -ENOMEM;
		goto fail;
	}
	gsdma_res[0] = (struct resource)DEFINE_RES_MEM(page_to_phys(tb_gsdma_page), PAGE_SIZE);
	gsdma_res[1] = (struct resource)DEFINE_RES_IRQ(tb_irq[TB_GSDMA_IRQ]);
	tb_gsdma = platform_device_register_full(&gsdma);
	if (IS_ERR(tb_gsdma)) {
		ret = PTR_ERR(tb_gsdma);
		goto fail;
	}
	ret = sysfs_create_group(&tb_gsdma->dev.kobj, &tb_gsdma_group);
	if (ret)
		goto fail;
	schedule_delayed_work(&tgd_work, msecs_to_jiffies(1));

	tb_vip_page = alloc_page(GFP_KERNEL | __GFP_ZERO);
	if (!tb_vip_page) {
		ret = -ENOMEM;
		goto fail;
	}
	tvip_wr(TVIP_IDLE, TVIP_IDLE_ALL);
	tvip_wr(TVIP_CID, tvip_cid);
	vip_res[0] = (struct resource)DEFINE_RES_MEM(page_to_phys(tb_vip_page), PAGE_SIZE);
	vip_res[1] = (struct resource)DEFINE_RES_IRQ(tb_irq[TB_VIP_IRQ]);
	tb_vip = platform_device_register_full(&vip);
	if (IS_ERR(tb_vip)) {
		ret = PTR_ERR(tb_vip);
		goto fail;
	}
	ret = sysfs_create_group(&tb_vip->dev.kobj, &tb_vip_group);
	if (ret)
		goto fail;
	schedule_delayed_work(&tvip_work, msecs_to_jiffies(1));
	return 0;
fail:
	tb_teardown();
	return ret;
}

static void __exit tb_exit(void)
{
	tb_teardown();
}

module_init(tb_init);
module_exit(tb_exit);

MODULE_DESCRIPTION("Devices with nothing behind them, for testing the Lemnos module");
MODULE_LICENSE("GPL");
