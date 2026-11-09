// SPDX-License-Identifier: GPL-2.0
/*
 * Two platform devices with nothing behind them, so the module can be tested
 * in a guest with no hardware to claim. One has the compatible the guest's
 * owned= lists, a page of ordinary memory for its register region, and two
 * interrupts from a simulated controller. The other has a compatible nothing
 * lists, and must be refused.
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <linux/dma-mapping.h>
#include <linux/err.h>
#include <linux/gfp.h>
#include <linux/io.h>
#include <linux/irq_sim.h>
#include <linux/irqdomain.h>
#include <linux/module.h>
#include <linux/platform_device.h>
#include <linux/property.h>

#define TB_IRQS 2
#define TB_MAGIC 0x4c454d4e

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
	struct resource res[1 + TB_IRQS];
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
