// SPDX-License-Identifier: GPL-2.0
/*
 * Two platform devices with nothing behind them, so the module can be tested
 * in a guest with no hardware to claim. One has the compatible the guest's
 * owned= lists, a page of ordinary memory for its register region, and two
 * interrupts from a simulated controller. The other has a compatible nothing
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
#include <linux/module.h>
#include <linux/platform_device.h>
#include <linux/property.h>
#include <linux/uaccess.h>
#include <linux/workqueue.h>

#include "lemnos_test_csi2.h"

#define TB_IRQS 3
#define TB_BLOCK_IRQS 2
#define TB_MAGIC 0x4c454d4e

static unsigned long csi2_phys;
module_param(csi2_phys, ulong, 0444);
MODULE_PARM_DESC(csi2_phys, "Physical address of 12K set aside for the fake receiver's registers");

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

static void __iomem *tcsi_regs[3];
static int tcsi_irq;
static struct platform_device *tcsi_pdev;
static DEFINE_MUTEX(tcsi_lock);
static u64 tcsi_cur_addr;
static u32 tcsi_cur_len;
static u32 tcsi_busy;		/* ms the line counter keeps moving after FORCE */
static u8 tcsi_pattern;

static void tcsi_tick(struct work_struct *work);
static DECLARE_DELAYED_WORK(tcsi_work, tcsi_tick);

static void tcsi_tick(struct work_struct *work)
{
	void __iomem *dma = tcsi_regs[0];
	bool again = true;

	mutex_lock(&tcsi_lock);
	if (readl(dma + R_CTRL) & R_FORCE) {
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
		if ((status & TCSI_FE_ACK) && tcsi_cur_addr) {
			memset(phys_to_virt(tcsi_cur_addr), ++tcsi_pattern, tcsi_cur_len);
			tcsi_cur_addr = 0;
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
static struct platform_device *tb_owned, *tb_other;

static const struct property_entry tb_owned_props[] = {
	PROPERTY_ENTRY_STRING("compatible", "lemnos,test-block"),
	{ }
};

static const struct property_entry tb_other_props[] = {
	PROPERTY_ENTRY_STRING("compatible", "lemnos,not-owned"),
	{ }
};

static void tb_teardown(void)
{
	if (tcsi_misc.minor != MISC_DYNAMIC_MINOR)
		misc_deregister(&tcsi_misc);
	cancel_delayed_work_sync(&tcsi_work);
	if (!IS_ERR_OR_NULL(tcsi_pdev))
		platform_device_unregister(tcsi_pdev);
	for (int i = 0; i < ARRAY_SIZE(tcsi_regs); i++)
		if (tcsi_regs[i])
			iounmap(tcsi_regs[i]);
	if (!IS_ERR_OR_NULL(tb_other))
		platform_device_unregister(tb_other);
	if (!IS_ERR_OR_NULL(tb_owned))
		platform_device_unregister(tb_owned);
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

static int __init tb_init(void)
{
	struct resource res[1 + TB_BLOCK_IRQS];
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
		struct resource cr[4];
		struct platform_device_info csi = {
			.name = "lemnos-test-csi2",
			.id = PLATFORM_DEVID_NONE,
			.res = cr,
			.num_res = ARRAY_SIZE(cr),
			.dma_mask = DMA_BIT_MASK(64),
			.properties = tcsi_props,
		};

		memset(cr, 0, sizeof(cr));
		for (int i = 0; i < 3; i++) {
			cr[i] = (struct resource)DEFINE_RES_MEM(csi2_phys + i * SZ_4K, SZ_4K);
			tcsi_regs[i] = ioremap(csi2_phys + i * SZ_4K, SZ_4K);
			if (!tcsi_regs[i]) {
				ret = -ENOMEM;
				goto fail;
			}
			memset_io(tcsi_regs[i], 0, SZ_4K);
		}
		tcsi_irq = tb_irq[TB_IRQS - 1];
		cr[3] = (struct resource)DEFINE_RES_IRQ(tcsi_irq);
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
