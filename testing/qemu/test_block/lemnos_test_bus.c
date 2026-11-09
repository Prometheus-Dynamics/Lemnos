// SPDX-License-Identifier: GPL-2.0
/*
 * A bus of each kind with nothing on it but memory, so the module's transfers
 * can be tested in a guest:
 * - I2C adapter 40, plain transfers only: address 0x50 behaves as a 256-byte
 *   memory with a one-byte address, and 0x51 is held by a kernel driver.
 * - SPI controller 40, with its data output wired to its input. Chip select 0
 *   is for the module; 1 is for the kernel's spi-loopback-test.
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <linux/delay.h>
#include <linux/i2c.h>
#include <linux/math64.h>
#include <linux/module.h>
#include <linux/platform_device.h>
#include <linux/property.h>
#include <linux/spi/spi.h>
#include <linux/string.h>

#define TB_BUS 40
#define TB_MEM_ADDR 0x50
#define TB_HELD_ADDR 0x51

static u8 tb_mem[256];
static u8 tb_pos;

static int tb_i2c_xfer(struct i2c_adapter *adap, struct i2c_msg *msgs, int num)
{
	for (int i = 0; i < num; i++) {
		struct i2c_msg *m = &msgs[i];
		u16 n = 0;

		if (m->addr != TB_MEM_ADDR && m->addr != TB_HELD_ADDR)
			return -ENXIO;
		if (m->flags & I2C_M_RD) {
			while (n < m->len)
				m->buf[n++] = tb_mem[tb_pos++];
			continue;
		}
		if (m->len)
			tb_pos = m->buf[n++];
		while (n < m->len)
			tb_mem[tb_pos++] = m->buf[n++];
	}
	return num;
}

static u32 tb_i2c_func(struct i2c_adapter *adap)
{
	return I2C_FUNC_I2C | I2C_FUNC_SMBUS_EMUL;
}

static const struct i2c_algorithm tb_i2c_algo = {
	.xfer = tb_i2c_xfer,
	.functionality = tb_i2c_func,
};

static struct i2c_adapter tb_adapter = {
	.owner = THIS_MODULE,
	.class = 0,
	.algo = &tb_i2c_algo,
	.name = "lemnos-test-i2c",
	.nr = TB_BUS,
};

static int tb_held_probe(struct i2c_client *client)
{
	return 0;
}

static const struct i2c_device_id tb_held_id[] = { { "lemnos-test-held" }, { } };

static struct i2c_driver tb_held_driver = {
	.driver = { .name = "lemnos-test-held" },
	.probe = tb_held_probe,
	.id_table = tb_held_id,
};

static const struct i2c_board_info tb_held_info = {
	I2C_BOARD_INFO("lemnos-test-held", TB_HELD_ADDR),
};

static struct i2c_client *tb_held;

/* SPI: whatever goes out comes back. */
static int tb_spi_transfer_one(struct spi_controller *ctlr, struct spi_device *spi,
			       struct spi_transfer *t)
{
	/* The kernel's loopback test refuses a transfer faster than its clock allows. */
	fsleep(div_u64(8ULL * t->len * USEC_PER_SEC, t->effective_speed_hz ?: spi->max_speed_hz) + 1);
	if (t->rx_buf) {
		if (t->tx_buf)
			memcpy(t->rx_buf, t->tx_buf, t->len);
		else
			memset(t->rx_buf, 0, t->len);
	}
	return 0;
}

static const struct property_entry tb_spi_props[] = {
	PROPERTY_ENTRY_STRING("compatible", "lemnos,test-spi"),
	{ }
};

/* The SPI core names a device with a software node after the node: "spi-lemnos-test". */
static const struct software_node tb_spi_node = {
	.name = "lemnos-test",
	.properties = tb_spi_props,
};

static struct platform_device *tb_parent;
static struct spi_controller *tb_ctlr;
static struct spi_device *tb_spi[2];

static void tb_spi_teardown(void)
{
	for (int i = 0; i < 2; i++)
		if (tb_spi[i])
			spi_unregister_device(tb_spi[i]);
	if (tb_ctlr)
		spi_unregister_controller(tb_ctlr);
	if (!IS_ERR_OR_NULL(tb_parent))
		platform_device_unregister(tb_parent);
	software_node_unregister(&tb_spi_node);
}

static int tb_spi_setup(void)
{
	struct spi_board_info info = {
		.modalias = "lemnos-test-spi",
		.max_speed_hz = 10000000,
		.bus_num = TB_BUS,
		.chip_select = 0,
		.swnode = &tb_spi_node,
	};
	int ret = software_node_register(&tb_spi_node);

	if (ret)
		return ret;
	tb_parent = platform_device_register_simple("lemnos-test-spi-parent", -1, NULL, 0);
	if (IS_ERR(tb_parent))
		return PTR_ERR(tb_parent);
	tb_ctlr = spi_alloc_host(&tb_parent->dev, 0);
	if (!tb_ctlr)
		return -ENOMEM;
	tb_ctlr->bus_num = TB_BUS;
	tb_ctlr->num_chipselect = 2;
	tb_ctlr->mode_bits = SPI_CPHA | SPI_CPOL | SPI_CS_HIGH | SPI_LSB_FIRST | SPI_LOOP;
	tb_ctlr->bits_per_word_mask = SPI_BPW_RANGE_MASK(1, 32);
	tb_ctlr->max_speed_hz = 10000000;
	tb_ctlr->transfer_one = tb_spi_transfer_one;
	ret = spi_register_controller(tb_ctlr);
	if (ret) {
		spi_controller_put(tb_ctlr);
		tb_ctlr = NULL;
		return ret;
	}
	tb_spi[0] = spi_new_device(tb_ctlr, &info);
	if (!tb_spi[0])
		return -ENODEV;
	strscpy(info.modalias, "spi-loopback-test", sizeof(info.modalias));
	info.chip_select = 1;
	info.swnode = NULL;
	tb_spi[1] = spi_new_device(tb_ctlr, &info);
	return tb_spi[1] ? 0 : -ENODEV;
}

static int __init tb_init(void)
{
	int ret = i2c_add_numbered_adapter(&tb_adapter);

	if (ret)
		return ret;
	ret = i2c_add_driver(&tb_held_driver);
	if (ret)
		goto adapter;
	tb_held = i2c_new_client_device(&tb_adapter, &tb_held_info);
	if (IS_ERR(tb_held)) {
		ret = PTR_ERR(tb_held);
		goto driver;
	}
	ret = tb_spi_setup();
	if (ret) {
		tb_spi_teardown();
		goto client;
	}
	return 0;
client:
	i2c_unregister_device(tb_held);
driver:
	i2c_del_driver(&tb_held_driver);
adapter:
	i2c_del_adapter(&tb_adapter);
	return ret;
}

static void __exit tb_exit(void)
{
	tb_spi_teardown();
	i2c_unregister_device(tb_held);
	i2c_del_driver(&tb_held_driver);
	i2c_del_adapter(&tb_adapter);
}

module_init(tb_init);
module_exit(tb_exit);

MODULE_DESCRIPTION("I2C and SPI buses with nothing on them, for testing the Lemnos module");
MODULE_LICENSE("GPL");
