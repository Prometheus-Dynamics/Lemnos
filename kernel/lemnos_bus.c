// SPDX-License-Identifier: GPL-2.0
/*
 * I2C and SPI transfer lists through the kernel's own cores (see lemnos.h).
 *
 * Buffers are made once, per adapter on first use and per SPI device at bind,
 * so a transfer allocates nothing. The adapter is looked up on each transfer
 * and not held between them, so one can go away while the module is loaded.
 *
 * The check for addresses a kernel driver holds is i2c-dev's.
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <linux/device.h>
#include <linux/i2c.h>
#include <linux/module.h>
#include <linux/mutex.h>
#include <linux/rwsem.h>
#include <linux/slab.h>
#include <linux/spi/spi.h>
#include <linux/uaccess.h>
#include <linux/xarray.h>

#include "lemnos_internal.h"

#define LEM_RESOURCE_MAX 32
#define LEM_I2C_SEGMENT_MAX 65535
#define LEM_BUS_BUFFER_MAX SZ_1M

static unsigned int bus_buffer = 4096;
module_param(bus_buffer, uint, 0444);
MODULE_PARM_DESC(bus_buffer, "Largest transfer, in bytes each way, per bus (default 4096)");

static void __user *lem_user(u64 p)
{
	return u64_to_user_ptr(p);
}

/* --- I2C --- */

struct lem_i2c {
	struct mutex lock;		/* buf, msgs, seg */
	u8 *buf;
	struct i2c_msg msgs[LEMNOS_BUS_MAX_SEGMENTS];
	struct lemnos_i2c_segment seg[LEMNOS_BUS_MAX_SEGMENTS];
};

static DEFINE_XARRAY(lem_i2cs);		/* adapter number -> struct lem_i2c */

/* The bus's state; adapters exist, so their number bounds this. */
static struct lem_i2c *lem_i2c_state(u32 bus)
{
	struct lem_i2c *s = xa_load(&lem_i2cs, bus);
	struct lem_i2c *old;

	if (s)
		return s;
	s = kzalloc(sizeof(*s), GFP_KERNEL);
	if (!s)
		return ERR_PTR(-ENOMEM);
	s->buf = kmalloc(bus_buffer, GFP_KERNEL);
	if (!s->buf) {
		kfree(s);
		return ERR_PTR(-ENOMEM);
	}
	mutex_init(&s->lock);
	old = xa_cmpxchg(&lem_i2cs, bus, NULL, s, GFP_KERNEL);
	if (!old)
		return s;
	kfree(s->buf);
	kfree(s);
	return xa_is_err(old) ? ERR_PTR(xa_err(old)) : old;
}

static int lem_i2c_check_addr(struct device *dev, void *addrp)
{
	struct i2c_client *client = i2c_verify_client(dev);

	if (!client || client->addr != *(unsigned int *)addrp)
		return 0;
	return dev->driver ? -EBUSY : 0;
}

static int lem_i2c_check_mux_children(struct device *dev, void *addrp)
{
	if (dev->type == &i2c_adapter_type)
		return device_for_each_child(dev, addrp, lem_i2c_check_mux_children);
	return lem_i2c_check_addr(dev, addrp);
}

/* An address a kernel driver holds on this adapter, its muxes, or the buses above it. */
static int lem_i2c_check_held(struct i2c_adapter *adap, unsigned int addr)
{
	struct i2c_adapter *parent;
	int ret = device_for_each_child(&adap->dev, &addr, lem_i2c_check_mux_children);

	for (parent = i2c_parent_is_i2c_adapter(adap); parent && !ret;
	     parent = i2c_parent_is_i2c_adapter(parent))
		ret = device_for_each_child(&parent->dev, &addr, lem_i2c_check_addr);
	return ret;
}

/* The claim is right, the address fits, and the adapter exists: returns it held. */
static struct i2c_adapter *lem_i2c_target(s32 claim, u32 bus, u16 addr, u16 flags)
{
	char resource[LEM_RESOURCE_MAX];
	struct i2c_adapter *adap;
	int ret;

	if (flags & ~LEMNOS_I2C_TEN_BIT || addr > (flags & LEMNOS_I2C_TEN_BIT ? 0x3ff : 0x7f))
		return ERR_PTR(-EINVAL);
	snprintf(resource, sizeof(resource), "i2c-%u:0x%x", bus, addr);
	if (!lem_claim_held(claim, resource))
		return ERR_PTR(-EPERM);
	if (bus > INT_MAX)
		return ERR_PTR(-ENODEV);
	adap = i2c_get_adapter(bus);
	if (!adap)
		return ERR_PTR(-ENODEV);
	ret = lem_i2c_check_held(adap, addr);
	if (ret) {
		i2c_put_adapter(adap);
		return ERR_PTR(ret);
	}
	return adap;
}

static long lem_i2c_run(struct lem_i2c *s, struct i2c_adapter *adap,
			const struct lemnos_i2c_transfer *t)
{
	u16 flags = t->flags & LEMNOS_I2C_TEN_BIT ? I2C_M_TEN : 0;
	size_t used = 0;
	int n;

	if (copy_from_user(s->seg, lem_user(t->segments), t->count * sizeof(s->seg[0])))
		return -EFAULT;
	for (u32 i = 0; i < t->count; i++) {
		const struct lemnos_i2c_segment *g = &s->seg[i];
		bool read = g->flags & LEMNOS_I2C_READ;

		if (g->flags & ~LEMNOS_I2C_READ)
			return -EINVAL;
		if (g->len > LEM_I2C_SEGMENT_MAX || g->len > bus_buffer - used)
			return -EMSGSIZE;
		s->msgs[i] = (struct i2c_msg){
			.addr = t->addr,
			.flags = flags | (read ? I2C_M_RD : 0),
			.len = g->len,
			.buf = s->buf + used,
		};
		if (!read && copy_from_user(s->msgs[i].buf, lem_user(g->buf), g->len))
			return -EFAULT;
		used += g->len;
	}
	n = i2c_transfer(adap, s->msgs, t->count);
	if (n < 0)
		return n;
	if (n != t->count)
		return -EIO;
	for (u32 i = 0; i < t->count; i++)
		if ((s->msgs[i].flags & I2C_M_RD) &&
		    copy_to_user(lem_user(s->seg[i].buf), s->msgs[i].buf, s->msgs[i].len))
			return -EFAULT;
	return 0;
}

long lem_i2c_transfer(const struct lemnos_i2c_transfer *t)
{
	struct i2c_adapter *adap;
	struct lem_i2c *s;
	long ret;

	if (!t->count || t->count > LEMNOS_BUS_MAX_SEGMENTS)
		return -EINVAL;
	adap = lem_i2c_target(t->claim, t->bus, t->addr, t->flags);
	if (IS_ERR(adap))
		return PTR_ERR(adap);
	if (!i2c_check_functionality(adap, I2C_FUNC_I2C)) {
		ret = -EOPNOTSUPP;
		goto put;
	}
	s = lem_i2c_state(t->bus);
	if (IS_ERR(s)) {
		ret = PTR_ERR(s);
		goto put;
	}
	mutex_lock(&s->lock);
	ret = lem_i2c_run(s, adap, t);
	mutex_unlock(&s->lock);
put:
	i2c_put_adapter(adap);
	return ret;
}

static bool lem_smbus_valid(const struct lemnos_i2c_smbus *s)
{
	u8 len = s->data[0];

	if (s->read > 1)
		return false;
	switch (s->protocol) {
	case I2C_SMBUS_QUICK:
	case I2C_SMBUS_BYTE:
	case I2C_SMBUS_BYTE_DATA:
	case I2C_SMBUS_WORD_DATA:
	case I2C_SMBUS_PROC_CALL:
		return true;
	case I2C_SMBUS_BLOCK_DATA:
		return s->read || (len >= 1 && len <= I2C_SMBUS_BLOCK_MAX);
	case I2C_SMBUS_I2C_BLOCK_DATA:
	case I2C_SMBUS_BLOCK_PROC_CALL:
		return len >= 1 && len <= I2C_SMBUS_BLOCK_MAX;
	default:
		return false;
	}
}

long lem_i2c_smbus(struct lemnos_i2c_smbus *s)
{
	union i2c_smbus_data data;
	struct i2c_adapter *adap;
	long ret;

	BUILD_BUG_ON(sizeof(data) != sizeof(s->data));
	if (!lem_smbus_valid(s))
		return -EINVAL;
	adap = lem_i2c_target(s->claim, s->bus, s->addr, s->flags);
	if (IS_ERR(adap))
		return PTR_ERR(adap);
	memcpy(&data, s->data, sizeof(data));
	ret = i2c_smbus_xfer(adap, s->addr, s->flags & LEMNOS_I2C_TEN_BIT ? I2C_CLIENT_TEN : 0,
			     s->read ? I2C_SMBUS_READ : I2C_SMBUS_WRITE, s->command, s->protocol,
			     &data);
	i2c_put_adapter(adap);
	if (ret < 0)
		return ret;
	memcpy(s->data, &data, sizeof(data));
	return 0;
}

/* --- SPI --- */

#define LEM_SPI_MODES (SPI_CPHA | SPI_CPOL | SPI_CS_HIGH | SPI_LSB_FIRST)

struct lem_spi {
	struct spi_device *spi;
	struct mutex lock;		/* everything below */
	u8 *tx;
	u8 *rx;
	u32 mode;			/* as last applied */
	struct spi_message msg;
	struct spi_transfer x[LEMNOS_BUS_MAX_SEGMENTS];
	struct lemnos_spi_segment seg[LEMNOS_BUS_MAX_SEGMENTS];
};

static DECLARE_RWSEM(lem_spi_sem);	/* held for reading across a transfer */
static DEFINE_XARRAY(lem_spis);		/* lem_spi_key() -> struct lem_spi */

static unsigned long lem_spi_key(u32 bus, u32 cs)
{
	return (unsigned long)bus << 8 | cs;
}

static int lem_spi_apply_mode(struct lem_spi *d, u32 mode)
{
	struct spi_device *spi = d->spi;
	u32 saved = spi->mode;
	int ret;

	if (mode == d->mode)
		return 0;
	spi->mode = (spi->mode & ~LEM_SPI_MODES) | mode;
	ret = spi_setup(spi);
	if (ret) {
		spi->mode = saved;
		return ret;
	}
	d->mode = mode;
	return 0;
}

static long lem_spi_run(struct lem_spi *d, const struct lemnos_spi_transfer *t)
{
	size_t used = 0;
	long ret;

	ret = lem_spi_apply_mode(d, t->mode);
	if (ret)
		return ret;
	if (copy_from_user(d->seg, lem_user(t->segments), t->count * sizeof(d->seg[0])))
		return -EFAULT;
	memset(d->x, 0, t->count * sizeof(d->x[0]));
	for (u32 i = 0; i < t->count; i++) {
		const struct lemnos_spi_segment *g = &d->seg[i];
		struct spi_transfer *x = &d->x[i];

		if (g->reserved || (g->len && !g->tx && !g->rx))
			return -EINVAL;
		if (g->len > bus_buffer - used)
			return -EMSGSIZE;
		if (g->tx) {
			if (copy_from_user(d->tx + used, lem_user(g->tx), g->len))
				return -EFAULT;
			x->tx_buf = d->tx + used;
		}
		if (g->rx)
			x->rx_buf = d->rx + used;
		x->len = g->len;
		x->speed_hz = g->speed_hz;
		x->bits_per_word = g->bits;
		x->cs_change = g->cs_change;
		x->delay.value = g->delay_us;
		x->delay.unit = SPI_DELAY_UNIT_USECS;
		used += g->len;
	}
	spi_message_init_with_transfers(&d->msg, d->x, t->count);
	ret = spi_sync(d->spi, &d->msg);
	if (ret)
		return ret;
	for (u32 i = 0; i < t->count; i++)
		if (d->seg[i].rx &&
		    copy_to_user(lem_user(d->seg[i].rx), d->x[i].rx_buf, d->seg[i].len))
			return -EFAULT;
	return 0;
}

long lem_spi_transfer(const struct lemnos_spi_transfer *t)
{
	char resource[LEM_RESOURCE_MAX];
	struct lem_spi *d;
	long ret = -ENODEV;

	if (!t->count || t->count > LEMNOS_BUS_MAX_SEGMENTS || t->reserved ||
	    t->mode & ~LEM_SPI_MODES || t->cs > U8_MAX)
		return -EINVAL;
	snprintf(resource, sizeof(resource), "spi-%u.%u", t->bus, t->cs);
	if (!lem_claim_held(t->claim, resource))
		return -EPERM;
	down_read(&lem_spi_sem);
	d = xa_load(&lem_spis, lem_spi_key(t->bus, t->cs));
	if (d) {
		mutex_lock(&d->lock);
		ret = lem_spi_run(d, t);
		mutex_unlock(&d->lock);
	}
	up_read(&lem_spi_sem);
	return ret;
}

static int lem_spi_probe(struct spi_device *spi)
{
	struct device *dev = &spi->dev;
	struct lem_spi *d;
	int ret;

	if (!lem_block_owned(dev))
		return dev_err_probe(dev, -ENODEV, "not a device Lemnos owns (see owned=)\n");
	d = devm_kzalloc(dev, sizeof(*d), GFP_KERNEL);
	if (!d)
		return -ENOMEM;
	d->tx = devm_kmalloc(dev, bus_buffer, GFP_KERNEL);
	d->rx = devm_kmalloc(dev, bus_buffer, GFP_KERNEL);
	if (!d->tx || !d->rx)
		return -ENOMEM;
	d->spi = spi;
	d->mode = spi->mode & LEM_SPI_MODES;
	mutex_init(&d->lock);
	spi_set_drvdata(spi, d);
	ret = xa_insert(&lem_spis, lem_spi_key(spi->controller->bus_num, spi_get_chipselect(spi, 0)),
			d, GFP_KERNEL);
	return ret;
}

static void lem_spi_remove(struct spi_device *spi)
{
	down_write(&lem_spi_sem);
	xa_erase(&lem_spis, lem_spi_key(spi->controller->bus_num, spi_get_chipselect(spi, 0)));
	up_write(&lem_spi_sem);
}

/* No id table: bound only through driver_override. */
static struct spi_driver lem_spi_driver = {
	.driver = {
		.name = "lemnos",
	},
	.probe = lem_spi_probe,
	.remove = lem_spi_remove,
};

int lem_bus_init(void)
{
	if (!bus_buffer || bus_buffer > LEM_BUS_BUFFER_MAX)
		return -EINVAL;
	return spi_register_driver(&lem_spi_driver);
}

void lem_bus_exit(void)
{
	struct lem_i2c *s;
	unsigned long bus;

	spi_unregister_driver(&lem_spi_driver);
	xa_for_each(&lem_i2cs, bus, s) {
		mutex_destroy(&s->lock);
		kfree(s->buf);
		kfree(s);
	}
	xa_destroy(&lem_i2cs);
}
