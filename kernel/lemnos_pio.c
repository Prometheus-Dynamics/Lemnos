// SPDX-License-Identifier: GPL-2.0
/*
 * RP1 PIO state machines and their DMA, through the kernel's own PIO driver
 * (see lemnos.h). A claim on "pio-0" holds one client of that driver.
 *
 * Arguments are checked here before the driver sees them: an index past the
 * end of a state machine or GPIO table, or a program longer than the
 * instruction memory, never leaves this file. The driver's register calls
 * aren't in the table. Data goes through a buffer this module allocated, and
 * the driver is always given address 0, which means its own DMA buffers.
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <linux/completion.h>
#include <linux/err.h>
#include <linux/file.h>
#include <linux/jiffies.h>
#include <linux/mm.h>
#include <linux/mutex.h>
#include <linux/pio_rp1.h>
#include <linux/slab.h>
#include <linux/uaccess.h>

#include "lemnos_internal.h"

#define LEM_PIO_RESOURCE "pio-0"
#define LEM_PIO_SMS 4
#define LEM_PIO_DIRS 2
#define LEM_PIO_GPIOS 28
#define LEM_PIO_INSTRUCTIONS 32
#define LEM_PIO_BUF_MAX SZ_256K
#define LEM_PIO_BUF_COUNT_MAX 64
#define LEM_PIO_TIMEOUT_MS 1000

enum lem_pio_check {
	LEM_PIO_CHECK_NONE,
	LEM_PIO_CHECK_SM,	/* the first u16 is a state machine */
	LEM_PIO_CHECK_MASK,	/* the first u16 is a mask of them */
	LEM_PIO_CHECK_GPIO,	/* the first u16 is a GPIO */
	LEM_PIO_CHECK_PROGRAM,	/* struct rp1_pio_add_program_args */
};

struct lem_pio_op {
	int (*call)(struct rp1_pio_client *client, void *param);
	u32 size;
	enum lem_pio_check check;
};

#define LEM_PIO_OP(op, fn, args, how)					\
	[LEMNOS_PIO_##op] = { rp1_pio_##fn, sizeof(struct rp1_pio_##args##_args), LEM_PIO_CHECK_##how }
#define LEM_PIO_GPIO_OP(op, fn, args)					\
	[LEMNOS_PIO_##op] = { rp1_pio_##fn, sizeof(struct rp1_##args##_args), LEM_PIO_CHECK_GPIO }

static const struct lem_pio_op lem_pio_ops[LEMNOS_PIO_OP_COUNT] = {
	LEM_PIO_OP(ADD_PROGRAM, add_program, add_program, PROGRAM),
	LEM_PIO_OP(REMOVE_PROGRAM, remove_program, remove_program, NONE),
	LEM_PIO_OP(SM_CLAIM, sm_claim, sm_claim, MASK),
	LEM_PIO_OP(SM_UNCLAIM, sm_unclaim, sm_claim, MASK),
	LEM_PIO_OP(SM_INIT, sm_init, sm_init, SM),
	LEM_PIO_OP(SM_SET_CONFIG, sm_set_config, sm_set_config, SM),
	LEM_PIO_OP(SM_EXEC, sm_exec, sm_exec, SM),
	LEM_PIO_OP(SM_CLEAR_FIFOS, sm_clear_fifos, sm_clear_fifos, SM),
	LEM_PIO_OP(SM_SET_CLKDIV, sm_set_clkdiv, sm_set_clkdiv, SM),
	LEM_PIO_OP(SM_SET_PINS, sm_set_pins, sm_set_pins, SM),
	LEM_PIO_OP(SM_SET_PINDIRS, sm_set_pindirs, sm_set_pindirs, SM),
	LEM_PIO_OP(SM_SET_ENABLED, sm_set_enabled, sm_set_enabled, MASK),
	LEM_PIO_OP(SM_RESTART, sm_restart, sm_restart, MASK),
	LEM_PIO_OP(SM_PUT, sm_put, sm_put, SM),
	LEM_PIO_OP(SM_GET, sm_get, sm_get, SM),
	LEM_PIO_OP(SM_FIFO_STATE, sm_fifo_state, sm_fifo_state, SM),
	LEM_PIO_OP(SM_DRAIN_TX, sm_drain_tx, sm_clear_fifos, SM),
	LEM_PIO_OP(SM_GET_FLAGS, sm_get_flags, sm_get_flags, SM),
	LEM_PIO_GPIO_OP(GPIO_INIT, gpio_init, gpio_init),
	LEM_PIO_GPIO_OP(GPIO_SET_FUNCTION, gpio_set_function, gpio_set_function),
	LEM_PIO_GPIO_OP(GPIO_SET_PULLS, gpio_set_pulls, gpio_set_pulls),
	LEM_PIO_GPIO_OP(GPIO_SET_OUTOVER, gpio_set_outover, gpio_set),
	LEM_PIO_GPIO_OP(GPIO_SET_INPUT_ENABLED, gpio_set_input_enabled, gpio_set),
};

/* One state machine's DMA in one direction. */
struct lem_pio_dma {
	struct mutex lock;
	struct completion done;
	void *scratch;		/* buf_size bytes; memory for copies only */
	u32 buf_size;
	bool stuck;		/* a transfer timed out: nothing more until reconfigured */
};

struct lem_pio {
	struct rp1_pio_client *client;
	struct lem_pio_dma dma[LEM_PIO_SMS][LEM_PIO_DIRS];
};

static void lem_pio_release(void *data)
{
	struct lem_pio *p = data;

	/* First: it ends the client's DMA, so no callback can reach the buffers below. */
	rp1_pio_close(p->client);
	for (u32 sm = 0; sm < LEM_PIO_SMS; sm++)
		for (u32 dir = 0; dir < LEM_PIO_DIRS; dir++)
			kvfree(p->dma[sm][dir].scratch);
	kfree(p);
}

static const struct lem_claim_ops lem_pio_claim_ops = {
	.release = lem_pio_release,
};

long lem_pio_open(const struct lemnos_pio_open *o)
{
	struct lem_pio *p;
	int ret;

	if (!lem_claim_held(o->claim, LEM_PIO_RESOURCE))
		return -EPERM;
	p = kzalloc(sizeof(*p), GFP_KERNEL);
	if (!p)
		return -ENOMEM;
	p->client = rp1_pio_open();
	if (IS_ERR(p->client)) {
		ret = PTR_ERR(p->client);
		kfree(p);
		return ret;
	}
	for (u32 sm = 0; sm < LEM_PIO_SMS; sm++) {
		for (u32 dir = 0; dir < LEM_PIO_DIRS; dir++) {
			mutex_init(&p->dma[sm][dir].lock);
			init_completion(&p->dma[sm][dir].done);
		}
	}
	ret = lem_claim_attach(o->claim, LEM_PIO_RESOURCE, &lem_pio_claim_ops, p);
	if (ret)
		lem_pio_release(p);
	return ret;
}

static bool lem_pio_args_ok(const struct lem_pio_op *op, const void *args)
{
	const struct rp1_pio_add_program_args *program = args;
	u16 first = *(const u16 *)args;

	switch (op->check) {
	case LEM_PIO_CHECK_SM:
		return first < LEM_PIO_SMS;
	case LEM_PIO_CHECK_MASK:
		return first < BIT(LEM_PIO_SMS);
	case LEM_PIO_CHECK_GPIO:
		return first < LEM_PIO_GPIOS;
	case LEM_PIO_CHECK_PROGRAM:
		return program->num_instrs && program->num_instrs <= LEM_PIO_INSTRUCTIONS;
	default:
		return true;
	}
}

long lem_pio_call(struct lemnos_pio_call *c)
{
	CLASS(fd, f)(c->claim);
	struct lem_pio *p = lem_claim_attached(fd_file(f), &lem_pio_claim_ops);
	const struct lem_pio_op *op;

	if (!p)
		return -EPERM;
	if (c->op >= LEMNOS_PIO_OP_COUNT)
		return -EINVAL;
	BUILD_BUG_ON(sizeof(struct rp1_pio_add_program_args) > LEMNOS_PIO_ARGS);
	op = &lem_pio_ops[c->op];
	if (!lem_pio_args_ok(op, c->args))
		return -EINVAL;
	c->result = op->call(p->client, c->args);
	return 0;
}

static struct lem_pio_dma *lem_pio_dma_of(struct lem_pio *p, u32 sm, u32 dir)
{
	return sm < LEM_PIO_SMS && dir < LEM_PIO_DIRS ? &p->dma[sm][dir] : NULL;
}

long lem_pio_config_xfer(const struct lemnos_pio_config_xfer *c)
{
	CLASS(fd, f)(c->claim);
	struct lem_pio *p = lem_claim_attached(fd_file(f), &lem_pio_claim_ops);
	struct lem_pio_dma *d;
	void *scratch;
	int ret;

	if (!p)
		return -EPERM;
	d = lem_pio_dma_of(p, c->sm, c->dir);
	if (!d || c->reserved || !c->buf_size || c->buf_size > LEM_PIO_BUF_MAX || !c->buf_count ||
	    c->buf_count > LEM_PIO_BUF_COUNT_MAX)
		return -EINVAL;
	scratch = kvmalloc(c->buf_size, GFP_KERNEL);
	if (!scratch)
		return -ENOMEM;
	mutex_lock(&d->lock);
	/* The driver stops anything still running on this channel. */
	ret = rp1_pio_sm_config_xfer(p->client, c->sm, c->dir, c->buf_size, c->buf_count);
	if (ret) {
		mutex_unlock(&d->lock);
		kvfree(scratch);
		return ret;
	}
	swap(d->scratch, scratch);
	d->buf_size = c->buf_size;
	d->stuck = false;
	reinit_completion(&d->done);
	mutex_unlock(&d->lock);
	kvfree(scratch);
	return 0;
}

static void lem_pio_done(void *param)
{
	complete(param);
}

static long lem_pio_move(struct lem_pio *p, const struct lemnos_pio_xfer *x, struct lem_pio_dma *d)
{
	unsigned long timeout = msecs_to_jiffies(x->timeout_ms ?: LEM_PIO_TIMEOUT_MS);
	void __user *user = u64_to_user_ptr(x->data);
	bool to_sm = x->dir == LEMNOS_PIO_TO_SM;
	u32 done = 0;
	long left;
	int ret;

	if (d->stuck)
		return -EIO;
	if (!d->buf_size)
		return -EINVAL;
	while (done < x->len) {
		u32 n = min(x->len - done, d->buf_size);

		if (to_sm && copy_from_user(d->scratch, user + done, n))
			return -EFAULT;
		ret = rp1_pio_sm_xfer_data(p->client, x->sm, x->dir, n, d->scratch, 0, lem_pio_done,
					   &d->done);
		if (ret)
			return ret;
		/*
		 * Killable, so a holder stuck on a channel nothing drains can still be ended: the
		 * caller's timeout can be long. Either way the transfer may still finish, into this
		 * scratch: keep it until reconfigured, or until the claim's release ends the client.
		 */
		left = wait_for_completion_killable_timeout(&d->done, timeout);
		if (left <= 0) {
			d->stuck = true;
			return left ? -EINTR : -ETIMEDOUT;
		}
		if (!to_sm && copy_to_user(user + done, d->scratch, n))
			return -EFAULT;
		done += n;
	}
	return 0;
}

long lem_pio_xfer(const struct lemnos_pio_xfer *x)
{
	CLASS(fd, f)(x->claim);
	struct lem_pio *p = lem_claim_attached(fd_file(f), &lem_pio_claim_ops);
	struct lem_pio_dma *d;
	long ret;

	if (!p)
		return -EPERM;
	d = lem_pio_dma_of(p, x->sm, x->dir);
	if (!d || x->reserved)
		return -EINVAL;
	mutex_lock(&d->lock);
	ret = lem_pio_move(p, x, d);
	mutex_unlock(&d->lock);
	return ret;
}
