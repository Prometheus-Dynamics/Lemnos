// SPDX-License-Identifier: GPL-2.0
/*
 * Fake blocks for lemnos.ko's register jobs (kernel/lemnos_regjob.c), so the
 * engine can be tested in a guest with no hardware: a register window of
 * ordinary memory (the module is loaded with test_rj_fake_regs), an interrupt
 * from a simulated controller and a reset line this module provides and
 * counts.
 *
 * Two blocks: the A733 video engine's (a 4 KiB window, its interrupt first) and
 * the K230 ISP's (a 36 KiB window, three interrupts, jobs completing on the
 * second). Each device has, in sysfs:
 *   poke     "offset value" (hex): sets a register, as the hardware would
 *   irq      raises the block's interrupt
 *   irq_line "n": raises the block's interrupt n (the K230 ISP's core is 0)
 *   regs     the register window, read-only
 *   stamp    "offset byte" (hex): writes the byte at the bus address the
 *            register at offset holds, as the block's DMA would. A bus address
 *            is a physical one here: nothing translates it.
 *   resets   how many times the block's reset line was pulsed
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
#include <linux/mm.h>
#include <linux/module.h>
#include <linux/platform_device.h>
#include <linux/property.h>
#include <linux/reset-controller.h>
#include <linux/string.h>
#include <linux/sysfs.h>

#define TR_BLOCKS 2
#define TR_IRQS 3	/* per block; `irq` raises the one jobs complete on */

struct tr_block {
	const char *name;
	const char *compatible;
	u32 window;
	u32 job_irq;
	struct page *page;
	struct platform_device *pdev;
	int irq[TR_IRQS];
	atomic_t resets;
};

/* Each block's window and interrupts as its profile in lemnos.ko expects them. */
static struct tr_block tr_blocks[TR_BLOCKS] = {
	{ .name = "lemnos-test-ve", .compatible = "lemnos,test-ve", .window = SZ_4K },
	{ .name = "lemnos-test-isp", .compatible = "lemnos,test-isp", .window = 0x9000,
	  .job_irq = 1 },
};

static struct fwnode_handle *tr_fwnode;
static struct irq_domain *tr_domain;
static struct platform_device *tr_rst_pdev;
static bool tr_rst_driver_up;

/* --- the reset line --- */

static const struct property_entry tr_rst_props[] = {
	PROPERTY_ENTRY_U32("#reset-cells", 1),
	{ }
};

static const struct software_node tr_rst_node = {
	.name = "lemnos-test-rj-reset",
	.properties = tr_rst_props,
};

static int tr_rst_reset(struct reset_controller_dev *rc, unsigned long id)
{
	if (id >= TR_BLOCKS)
		return -EINVAL;
	atomic_inc(&tr_blocks[id].resets);
	return 0;
}

static int tr_rst_nop(struct reset_controller_dev *rc, unsigned long id)
{
	return 0;
}

static const struct reset_control_ops tr_rst_ops = {
	.reset = tr_rst_reset,
	.assert = tr_rst_nop,
	.deassert = tr_rst_nop,
};

static struct reset_controller_dev tr_rst = {
	.ops = &tr_rst_ops,
	.owner = THIS_MODULE,
	.nr_resets = TR_BLOCKS,
	.fwnode_reset_n_cells = 1,
};

static int tr_rst_probe(struct platform_device *pdev)
{
	tr_rst.dev = &pdev->dev;
	tr_rst.fwnode = dev_fwnode(&pdev->dev);
	return devm_reset_controller_register(&pdev->dev, &tr_rst);
}

static struct platform_driver tr_rst_driver = {
	.probe = tr_rst_probe,
	.driver = {
		.name = "lemnos-test-rj-reset",
	},
};

/* --- what the test reaches through sysfs --- */

static struct tr_block *tr_of(struct device *dev)
{
	for (int i = 0; i < TR_BLOCKS; i++)
		if (tr_blocks[i].pdev && &tr_blocks[i].pdev->dev == dev)
			return &tr_blocks[i];
	return NULL;
}

static void *tr_regs(struct tr_block *b)
{
	return page_address(b->page);
}

/* "offset value", both hex. */
static int tr_pair(const char *buf, unsigned long *a, unsigned long *b)
{
	char tmp[48];
	char *sep;

	strscpy(tmp, buf, sizeof(tmp));
	sep = strchr(tmp, ' ');
	if (!sep)
		return -EINVAL;
	*sep++ = 0;
	if (kstrtoul(strim(tmp), 16, a) || kstrtoul(strim(sep), 16, b))
		return -EINVAL;
	return 0;
}

static ssize_t poke_store(struct device *dev, struct device_attribute *attr, const char *buf,
			  size_t count)
{
	struct tr_block *b = tr_of(dev);
	unsigned long offset, value;

	if (!b || tr_pair(buf, &offset, &value) || offset >= b->window || offset % 4)
		return -EINVAL;
	WRITE_ONCE(*(u32 *)(tr_regs(b) + offset), (u32)value);
	return count;
}
static DEVICE_ATTR_WO(poke);

static ssize_t irq_store(struct device *dev, struct device_attribute *attr, const char *buf,
			 size_t count)
{
	struct tr_block *b = tr_of(dev);
	int ret;

	if (!b)
		return -ENODEV;
	ret = irq_set_irqchip_state(b->irq[b->job_irq], IRQCHIP_STATE_PENDING, true);
	return ret ?: count;
}
static DEVICE_ATTR_WO(irq);

static ssize_t irq_line_store(struct device *dev, struct device_attribute *attr, const char *buf,
			      size_t count)
{
	struct tr_block *b = tr_of(dev);
	unsigned int n;
	int ret;

	if (!b)
		return -ENODEV;
	if (kstrtouint(buf, 0, &n) || n >= TR_IRQS)
		return -EINVAL;
	ret = irq_set_irqchip_state(b->irq[n], IRQCHIP_STATE_PENDING, true);
	return ret ?: count;
}
static DEVICE_ATTR_WO(irq_line);

static ssize_t stamp_store(struct device *dev, struct device_attribute *attr, const char *buf,
			   size_t count)
{
	struct tr_block *b = tr_of(dev);
	unsigned long offset, value;
	phys_addr_t at;

	if (!b || tr_pair(buf, &offset, &value) || offset >= b->window || offset % 4)
		return -EINVAL;
	at = READ_ONCE(*(u32 *)(tr_regs(b) + offset));
	if (!pfn_valid(PHYS_PFN(at)))
		return -EFAULT;
	*(u8 *)phys_to_virt(at) = (u8)value;
	return count;
}
static DEVICE_ATTR_WO(stamp);

static ssize_t resets_show(struct device *dev, struct device_attribute *attr, char *buf)
{
	struct tr_block *b = tr_of(dev);

	return b ? sysfs_emit(buf, "%d\n", atomic_read(&b->resets)) : -ENODEV;
}
static DEVICE_ATTR_RO(resets);

static ssize_t regs_read(struct file *f, struct kobject *kobj, const struct bin_attribute *a,
			 char *buf, loff_t off, size_t count)
{
	struct tr_block *b = tr_of(kobj_to_dev(kobj));

	if (!b)
		return -ENODEV;
	if (off >= b->window)
		return 0;
	count = min_t(size_t, count, b->window - off);
	memcpy(buf, tr_regs(b) + off, count);
	return count;
}
static const BIN_ATTR_RO(regs, 0x9000);

static struct attribute *tr_attrs[] = { &dev_attr_poke.attr, &dev_attr_irq.attr,
					&dev_attr_irq_line.attr, &dev_attr_stamp.attr,
					&dev_attr_resets.attr, NULL };
static const struct bin_attribute *const tr_bin_attrs[] = { &bin_attr_regs, NULL };
static const struct attribute_group tr_group = {
	.attrs = tr_attrs,
	.bin_attrs = tr_bin_attrs,
};

/* --- setup --- */

static void tr_teardown(void)
{
	for (int i = 0; i < TR_BLOCKS; i++) {
		struct tr_block *b = &tr_blocks[i];

		if (!IS_ERR_OR_NULL(b->pdev)) {
			sysfs_remove_group(&b->pdev->dev.kobj, &tr_group);
			platform_device_unregister(b->pdev);
		}
		b->pdev = NULL;
		for (int n = 0; n < TR_IRQS; n++) {
			if (b->irq[n])
				irq_dispose_mapping(b->irq[n]);
			b->irq[n] = 0;
		}
		if (b->page)
			__free_pages(b->page, get_order(b->window));
		b->page = NULL;
	}
	if (!IS_ERR_OR_NULL(tr_rst_pdev))
		platform_device_unregister(tr_rst_pdev);
	tr_rst_pdev = NULL;
	if (tr_rst_driver_up)
		platform_driver_unregister(&tr_rst_driver);
	tr_rst_driver_up = false;
	if (!IS_ERR_OR_NULL(tr_domain))
		irq_domain_remove_sim(tr_domain);
	tr_domain = NULL;
	if (tr_fwnode)
		irq_domain_free_fwnode(tr_fwnode);
	tr_fwnode = NULL;
}

static int tr_add(int i)
{
	struct tr_block *b = &tr_blocks[i];
	struct property_entry props[] = {
		PROPERTY_ENTRY_STRING("compatible", b->compatible),
		PROPERTY_ENTRY_REF("resets", &tr_rst_node, i),
		{ }
	};
	struct resource res[1 + TR_IRQS];
	struct platform_device_info info = {
		.name = b->name,
		.id = PLATFORM_DEVID_NONE,
		.res = res,
		.num_res = ARRAY_SIZE(res),
		.dma_mask = DMA_BIT_MASK(64),
		.properties = props,
	};

	b->page = alloc_pages(GFP_KERNEL | __GFP_ZERO, get_order(b->window));
	if (!b->page)
		return -ENOMEM;
	res[0] = (struct resource)DEFINE_RES_MEM(page_to_phys(b->page), b->window);
	for (int n = 0; n < TR_IRQS; n++) {
		b->irq[n] = irq_create_mapping(tr_domain, i * TR_IRQS + n);
		if (!b->irq[n])
			return -EINVAL;
		res[1 + n] = (struct resource)DEFINE_RES_IRQ(b->irq[n]);
	}
	b->pdev = platform_device_register_full(&info);
	if (IS_ERR(b->pdev))
		return PTR_ERR(b->pdev);
	return sysfs_create_group(&b->pdev->dev.kobj, &tr_group);
}

static int __init tr_init(void)
{
	int ret;

	tr_fwnode = irq_domain_alloc_named_fwnode("lemnos-test-regjob");
	if (!tr_fwnode)
		return -ENOMEM;
	tr_domain = irq_domain_create_sim(tr_fwnode, TR_BLOCKS * TR_IRQS);
	if (IS_ERR(tr_domain)) {
		ret = PTR_ERR(tr_domain);
		goto fail;
	}
	ret = platform_driver_register(&tr_rst_driver);
	if (ret)
		goto fail;
	tr_rst_driver_up = true;
	tr_rst_pdev = platform_device_register_full(&(struct platform_device_info){
		.name = "lemnos-test-rj-reset",
		.id = PLATFORM_DEVID_NONE,
		.swnode = &tr_rst_node,
	});
	if (IS_ERR(tr_rst_pdev)) {
		ret = PTR_ERR(tr_rst_pdev);
		goto fail;
	}
	if (!tr_rst_pdev->dev.driver) {
		ret = -ENODEV;
		goto fail;
	}
	for (int i = 0; i < TR_BLOCKS; i++) {
		ret = tr_add(i);
		if (ret)
			goto fail;
	}
	return 0;
fail:
	tr_teardown();
	return ret;
}

static void __exit tr_exit(void)
{
	tr_teardown();
}

module_init(tr_init);
module_exit(tr_exit);

MODULE_DESCRIPTION("Fake blocks for the Lemnos module's register jobs");
MODULE_LICENSE("GPL");
