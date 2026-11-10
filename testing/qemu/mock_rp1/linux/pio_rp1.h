/* SPDX-License-Identifier: GPL-2.0 */
/*
 * A stand-in for the kernel's RP1 PIO driver interface, so the module's PIO
 * file can run in a guest with no RP1. The argument layouts are interface
 * facts transcribed from rp1_pio_if.h (Linux-syscall-note).
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#ifndef LEMNOS_MOCK_PIO_RP1_H
#define LEMNOS_MOCK_PIO_RP1_H

#include <linux/types.h>

struct rp1_pio_client;
typedef struct rp1_pio_client *PIO;

struct rp1_pio_add_program_args { u16 num_instrs, origin, instrs[32]; };
struct rp1_pio_remove_program_args { u16 num_instrs, origin; };
struct rp1_pio_sm_claim_args { u16 mask; };
struct rp1_pio_sm_config_words { u32 clkdiv, execctrl, shiftctrl, pinctrl; };
struct rp1_pio_sm_init_args { u16 sm, initial_pc; struct rp1_pio_sm_config_words config; };
struct rp1_pio_sm_set_config_args { u16 sm, rsvd; struct rp1_pio_sm_config_words config; };
struct rp1_pio_sm_exec_args { u16 sm, instr; u8 blocking, rsvd; };
struct rp1_pio_sm_clear_fifos_args { u16 sm; };
struct rp1_pio_sm_set_clkdiv_args { u16 sm, div_int; u8 div_frac, rsvd; };
struct rp1_pio_sm_set_pins_args { u16 sm, rsvd; u32 values, mask; };
struct rp1_pio_sm_set_pindirs_args { u16 sm, rsvd; u32 dirs, mask; };
struct rp1_pio_sm_set_enabled_args { u16 mask; u8 enable, rsvd; };
struct rp1_pio_sm_restart_args { u16 mask; };
struct rp1_pio_sm_put_args { u16 sm; u8 blocking, rsvd; u32 data; };
struct rp1_pio_sm_get_args { u16 sm; u8 blocking, rsvd; u32 data; };
struct rp1_pio_sm_fifo_state_args { u16 sm; u8 tx, rsvd; u16 level; u8 empty, full; };
struct rp1_pio_sm_get_flags_args { u16 sm; u8 clear, rsvd; u32 flags, timeout; };
struct rp1_gpio_init_args { u16 gpio; };
struct rp1_gpio_set_function_args { u16 gpio, fn; };
struct rp1_gpio_set_pulls_args { u16 gpio; u8 up, down; };
struct rp1_gpio_set_args { u16 gpio, value; };

PIO rp1_pio_open(void);
void rp1_pio_close(PIO client);
int rp1_pio_sm_config_xfer(PIO client, uint sm, uint dir, uint buf_size, uint buf_count);
int rp1_pio_sm_xfer_data(PIO client, uint sm, uint dir, uint data_bytes, void *data,
			 dma_addr_t dma_addr, void (*callback)(void *param), void *param);

#define LEMNOS_MOCK_DECLARE(fn) int rp1_pio_##fn(PIO client, void *param)
LEMNOS_MOCK_DECLARE(add_program);
LEMNOS_MOCK_DECLARE(remove_program);
LEMNOS_MOCK_DECLARE(sm_claim);
LEMNOS_MOCK_DECLARE(sm_unclaim);
LEMNOS_MOCK_DECLARE(sm_init);
LEMNOS_MOCK_DECLARE(sm_set_config);
LEMNOS_MOCK_DECLARE(sm_exec);
LEMNOS_MOCK_DECLARE(sm_clear_fifos);
LEMNOS_MOCK_DECLARE(sm_set_clkdiv);
LEMNOS_MOCK_DECLARE(sm_set_pins);
LEMNOS_MOCK_DECLARE(sm_set_pindirs);
LEMNOS_MOCK_DECLARE(sm_set_enabled);
LEMNOS_MOCK_DECLARE(sm_restart);
LEMNOS_MOCK_DECLARE(sm_put);
LEMNOS_MOCK_DECLARE(sm_get);
LEMNOS_MOCK_DECLARE(sm_fifo_state);
LEMNOS_MOCK_DECLARE(sm_drain_tx);
LEMNOS_MOCK_DECLARE(sm_get_flags);
LEMNOS_MOCK_DECLARE(gpio_init);
LEMNOS_MOCK_DECLARE(gpio_set_function);
LEMNOS_MOCK_DECLARE(gpio_set_pulls);
LEMNOS_MOCK_DECLARE(gpio_set_outover);
LEMNOS_MOCK_DECLARE(gpio_set_input_enabled);

#endif
