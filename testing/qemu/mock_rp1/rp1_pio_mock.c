// SPDX-License-Identifier: GPL-2.0
/*
 * The stand-in for the RP1 PIO driver. Calls are counted, sm_get returns what
 * sm_put was given plus one, data sent to a machine is what comes back from
 * it, and a machine past the configured limit has nothing to deliver, so the
 * caller's wait runs out. Nothing here is hardware.
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <linux/err.h>
#include <linux/module.h>
#include <linux/pio_rp1.h>
#include <linux/slab.h>
#include <linux/string.h>

static int calls, clients, last_put, sticky_sm = -1, last_addr_nonzero;
module_param(calls, int, 0444);
module_param(clients, int, 0444);
module_param(last_addr_nonzero, int, 0444);
/* The one machine whose transfers never finish, or -1. */
module_param(sticky_sm, int, 0644);

static u8 store[SZ_64K];

struct rp1_pio_client {
	int unused;
};

PIO rp1_pio_open(void)
{
	PIO c = kzalloc(sizeof(*c), GFP_KERNEL);

	if (!c)
		return ERR_PTR(-ENOMEM);
	clients++;
	return c;
}
EXPORT_SYMBOL_GPL(rp1_pio_open);

void rp1_pio_close(PIO client)
{
	clients--;
	kfree(client);
}
EXPORT_SYMBOL_GPL(rp1_pio_close);

int rp1_pio_sm_config_xfer(PIO client, uint sm, uint dir, uint buf_size, uint buf_count)
{
	return buf_size <= sizeof(store) ? 0 : -EINVAL;
}
EXPORT_SYMBOL_GPL(rp1_pio_sm_config_xfer);

int rp1_pio_sm_xfer_data(PIO client, uint sm, uint dir, uint data_bytes, void *data,
			 dma_addr_t dma_addr, void (*callback)(void *param), void *param)
{
	if (dma_addr)
		last_addr_nonzero = 1;
	if (data_bytes > sizeof(store))
		return -EINVAL;
	if ((int)sm == sticky_sm)
		return 0;
	if (dir == 0)
		memcpy(store, data, data_bytes);
	else
		memcpy(data, store, data_bytes);
	callback(param);
	return 0;
}
EXPORT_SYMBOL_GPL(rp1_pio_sm_xfer_data);

#define LEMNOS_MOCK_DEFINE(fn)						\
	int rp1_pio_##fn(PIO client, void *param)			\
	{								\
		calls++;						\
		return 0;						\
	}								\
	EXPORT_SYMBOL_GPL(rp1_pio_##fn)

LEMNOS_MOCK_DEFINE(remove_program);
LEMNOS_MOCK_DEFINE(sm_claim);
LEMNOS_MOCK_DEFINE(sm_unclaim);
LEMNOS_MOCK_DEFINE(sm_init);
LEMNOS_MOCK_DEFINE(sm_set_config);
LEMNOS_MOCK_DEFINE(sm_exec);
LEMNOS_MOCK_DEFINE(sm_clear_fifos);
LEMNOS_MOCK_DEFINE(sm_set_clkdiv);
LEMNOS_MOCK_DEFINE(sm_set_pins);
LEMNOS_MOCK_DEFINE(sm_set_pindirs);
LEMNOS_MOCK_DEFINE(sm_set_enabled);
LEMNOS_MOCK_DEFINE(sm_restart);
LEMNOS_MOCK_DEFINE(sm_fifo_state);
LEMNOS_MOCK_DEFINE(sm_drain_tx);
LEMNOS_MOCK_DEFINE(sm_get_flags);
LEMNOS_MOCK_DEFINE(gpio_init);
LEMNOS_MOCK_DEFINE(gpio_set_function);
LEMNOS_MOCK_DEFINE(gpio_set_pulls);
LEMNOS_MOCK_DEFINE(gpio_set_outover);
LEMNOS_MOCK_DEFINE(gpio_set_input_enabled);

/* A program goes in at offset 5, as the driver reports where it landed. */
int rp1_pio_add_program(PIO client, void *param)
{
	calls++;
	return 5;
}
EXPORT_SYMBOL_GPL(rp1_pio_add_program);

int rp1_pio_sm_put(PIO client, void *param)
{
	last_put = ((struct rp1_pio_sm_put_args *)param)->data;
	calls++;
	return 0;
}
EXPORT_SYMBOL_GPL(rp1_pio_sm_put);

int rp1_pio_sm_get(PIO client, void *param)
{
	((struct rp1_pio_sm_get_args *)param)->data = last_put + 1;
	calls++;
	return 0;
}
EXPORT_SYMBOL_GPL(rp1_pio_sm_get);

MODULE_DESCRIPTION("A stand-in for the RP1 PIO driver, for testing the Lemnos module");
MODULE_LICENSE("GPL");
