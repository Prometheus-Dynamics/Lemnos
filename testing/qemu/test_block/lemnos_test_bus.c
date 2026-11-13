// SPDX-License-Identifier: GPL-2.0
/*
 * A bus of each kind with nothing on it but memory, so the module's transfers
 * can be tested in a guest:
 * - I2C adapter 40, plain transfers only: address 0x50 behaves as a 256-byte
 *   memory with a one-byte address, and 0x51 is held by a kernel driver.
 * - SPI controller 40, with its data output wired to its input. Chip select 0
 *   is for the module; 1 is left unbound (the harness does not run the kernel's
 *   spi-loopback-test: it tests the kernel's SPI core, not this code, and its
 *   timing check fails under host load).
 * - A two-channel PWM chip that stores what it is told, under the platform
 *   device lemnos-test-parent.
 * - For the block lemnos-test-block: a supply "vdd" that can be made to report
 *   a fault and a clock "core", found by device name.
 * - A thermal zone "lemnos-test" whose temperature is the parameter temp_mc.
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <linux/clk-provider.h>
#include <linux/clkdev.h>
#include <linux/delay.h>
#include <linux/i2c.h>
#include <linux/math64.h>
#include <linux/module.h>
#include <linux/platform_device.h>
#include <linux/property.h>
#include <linux/pwm.h>
#include <linux/regulator/driver.h>
#include <linux/regulator/machine.h>
#include <linux/thermal.h>
#include <linux/spi/spi.h>
#include <linux/string.h>

#define TB_BUS 40
#define TB_MEM_ADDR 0x50
#define TB_HELD_ADDR 0x51

static struct platform_device *tb_parent;

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

static int tb_pwm_apply(struct pwm_chip *chip, struct pwm_device *pwm,
			const struct pwm_state *state)
{
	return 0;
}

static const struct pwm_ops tb_pwm_ops = {
	.apply = tb_pwm_apply,
};

static struct pwm_chip *tb_pwm;

static const struct property_entry tb_spi_props[] = {
	PROPERTY_ENTRY_STRING("compatible", "lemnos,test-spi"),
	{ }
};

/* The SPI core names a device with a software node after the node: "spi-lemnos-test". */
static const struct software_node tb_spi_node = {
	.name = "lemnos-test",
	.properties = tb_spi_props,
};

static int temp_mc = 40000;
module_param(temp_mc, int, 0644);

static int tb_get_temp(struct thermal_zone_device *tz, int *temp)
{
	*temp = READ_ONCE(temp_mc);
	return 0;
}

static const struct thermal_zone_device_ops tb_tz_ops = {
	.get_temp = tb_get_temp,
};

static struct thermal_zone_device *tb_tz;

/* A supply from 1.0 V to 3.3 V in 100 mV steps. */
static unsigned int tb_sel;
static bool tb_on;
static struct regulator_dev *tb_rdev;

static int tb_reg_enable(struct regulator_dev *rdev)
{
	tb_on = true;
	return 0;
}

static int tb_reg_disable(struct regulator_dev *rdev)
{
	tb_on = false;
	return 0;
}

static int tb_reg_is_enabled(struct regulator_dev *rdev)
{
	return tb_on;
}

static int tb_reg_get_sel(struct regulator_dev *rdev)
{
	return tb_sel;
}

static int tb_reg_set_sel(struct regulator_dev *rdev, unsigned int sel)
{
	tb_sel = sel;
	return 0;
}

static const struct regulator_ops tb_reg_ops = {
	.enable = tb_reg_enable,
	.disable = tb_reg_disable,
	.is_enabled = tb_reg_is_enabled,
	.list_voltage = regulator_list_voltage_linear,
	.map_voltage = regulator_map_voltage_linear,
	.get_voltage_sel = tb_reg_get_sel,
	.set_voltage_sel = tb_reg_set_sel,
};

static const struct regulator_desc tb_reg_desc = {
	.name = "lemnos-test-vdd",
	.ops = &tb_reg_ops,
	.type = REGULATOR_VOLTAGE,
	.owner = THIS_MODULE,
	.n_voltages = 24,
	.min_uV = 1000000,
	.uV_step = 100000,
};

static struct regulator_consumer_supply tb_supply =
	REGULATOR_SUPPLY("vdd", "lemnos-test-block");

static struct regulator_init_data tb_reg_init = {
	.constraints = {
		.min_uV = 1000000,
		.max_uV = 3300000,
		.valid_ops_mask = REGULATOR_CHANGE_VOLTAGE | REGULATOR_CHANGE_STATUS,
	},
	.num_consumer_supplies = 1,
	.consumer_supplies = &tb_supply,
};

/* Writing anything here makes the supply report an over-current. */
static int tb_fault_set(const char *val, const struct kernel_param *kp)
{
	if (!tb_rdev)
		return -ENODEV;
	regulator_notifier_call_chain(tb_rdev, REGULATOR_EVENT_OVER_CURRENT, NULL);
	return 0;
}

static const struct kernel_param_ops tb_fault_ops = {
	.set = tb_fault_set,
};
module_param_cb(fault, &tb_fault_ops, NULL, 0200);

static struct clk_hw *tb_clk;
static struct clk_lookup *tb_clk_lookup;

static int tb_power_setup(void)
{
	struct regulator_config cfg = {
		.dev = &tb_parent->dev,
		.init_data = &tb_reg_init,
	};
	int ret;

	tb_rdev = regulator_register(&tb_parent->dev, &tb_reg_desc, &cfg);
	if (IS_ERR(tb_rdev)) {
		ret = PTR_ERR(tb_rdev);
		tb_rdev = NULL;
		return ret;
	}
	tb_clk = clk_hw_register_fixed_rate(&tb_parent->dev, "lemnos-test-clk", NULL, 0, 25000000);
	if (IS_ERR(tb_clk)) {
		ret = PTR_ERR(tb_clk);
		tb_clk = NULL;
		return ret;
	}
	tb_clk_lookup = clkdev_hw_create(tb_clk, "core", "lemnos-test-block");
	if (!tb_clk_lookup)
		return -ENOMEM;
	tb_tz = thermal_tripless_zone_device_register("lemnos-test", NULL, &tb_tz_ops, NULL);
	if (IS_ERR(tb_tz)) {
		ret = PTR_ERR(tb_tz);
		tb_tz = NULL;
		return ret;
	}
	return thermal_zone_device_enable(tb_tz);
}

static void tb_power_teardown(void)
{
	if (tb_tz)
		thermal_zone_device_unregister(tb_tz);
	if (tb_clk_lookup)
		clkdev_drop(tb_clk_lookup);
	if (tb_clk)
		clk_hw_unregister_fixed_rate(tb_clk);
	if (tb_rdev)
		regulator_unregister(tb_rdev);
}

static struct spi_controller *tb_ctlr;
static struct spi_device *tb_spi[2];

static void tb_spi_teardown(void)
{
	for (int i = 0; i < 2; i++)
		if (tb_spi[i])
			spi_unregister_device(tb_spi[i]);
	tb_power_teardown();
	if (tb_pwm) {
		pwmchip_remove(tb_pwm);
		pwmchip_put(tb_pwm);
	}
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
	tb_parent = platform_device_register_simple("lemnos-test-parent", -1, NULL, 0);
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
	tb_pwm = pwmchip_alloc(&tb_parent->dev, 2, 0);
	if (IS_ERR(tb_pwm)) {
		ret = PTR_ERR(tb_pwm);
		tb_pwm = NULL;
		return ret;
	}
	tb_pwm->ops = &tb_pwm_ops;
	ret = pwmchip_add(tb_pwm);
	if (ret) {
		pwmchip_put(tb_pwm);
		tb_pwm = NULL;
		return ret;
	}
	ret = tb_power_setup();
	if (ret)
		return ret;
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
