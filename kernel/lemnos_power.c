// SPDX-License-Identifier: GPL-2.0
/*
 * A block's clocks, supplies and resets (see lemnos.h), through the kernel's
 * own frameworks and found by the names the firmware description gives them.
 *
 * A handle is taken on first use and kept, so later calls allocate nothing.
 * What the caller turns on stays on across a close: only an unbind, which
 * must leave the frameworks balanced, turns off what was turned on here.
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <linux/clk.h>
#include <linux/device.h>
#include <linux/err.h>
#include <linux/mutex.h>
#include <linux/notifier.h>
#include <linux/regulator/consumer.h>
#include <linux/reset.h>
#include <linux/slab.h>
#include <linux/string.h>

#include "lemnos_internal.h"

struct lem_handle {
	char name[LEMNOS_POWER_NAME];
	u32 kind;
	u32 index;
	bool on;			/* turned on by this module */
	struct lem_power *power;
	struct notifier_block nb;
	union {
		struct clk *clk;
		struct regulator *supply;
		struct reset_control *rst;
	};
};

struct lem_power {
	struct device *dev;
	u32 block;
	struct mutex lock;		/* n, and each handle's on */
	u32 n;
	struct lem_handle h[LEMNOS_MAX_POWER];
};

struct lem_power *lem_power_create(struct device *dev)
{
	struct lem_power *p = kzalloc(sizeof(*p), GFP_KERNEL);

	if (!p)
		return ERR_PTR(-ENOMEM);
	p->dev = dev;
	mutex_init(&p->lock);
	return p;
}

void lem_power_set_block(struct lem_power *p, u32 block)
{
	p->block = block;
}

/* Faults only: a supply's routine notifications aren't worth an event. */
#define LEM_SUPPLY_FAULTS (REGULATOR_EVENT_UNDER_VOLTAGE | REGULATOR_EVENT_OVER_CURRENT | \
			   REGULATOR_EVENT_REGULATION_OUT | REGULATOR_EVENT_FAIL | \
			   REGULATOR_EVENT_OVER_TEMP | REGULATOR_EVENT_UNDER_VOLTAGE_WARN | \
			   REGULATOR_EVENT_OVER_CURRENT_WARN | REGULATOR_EVENT_OVER_VOLTAGE_WARN | \
			   REGULATOR_EVENT_OVER_TEMP_WARN)

static int lem_supply_event(struct notifier_block *nb, unsigned long event, void *data)
{
	struct lem_handle *h = container_of(nb, struct lem_handle, nb);

	if (event & LEM_SUPPLY_FAULTS)
		lem_event_post(LEMNOS_EVENT_SUPPLY_FAULT, h->power->block, h->index,
			       event & LEM_SUPPLY_FAULTS);
	return NOTIFY_OK;
}

static int lem_handle_open(struct lem_power *p, struct lem_handle *h)
{
	const char *name = h->name[0] ? h->name : NULL;
	struct reset_control *rst;
	struct regulator *supply;
	struct clk *clk;

	switch (h->kind) {
	case LEMNOS_POWER_CLOCK:
		clk = clk_get(p->dev, name);
		if (IS_ERR(clk))
			return PTR_ERR(clk);
		h->clk = clk;
		return 0;
	case LEMNOS_POWER_SUPPLY:
		if (!name)
			return -EINVAL;
		supply = regulator_get_optional(p->dev, name);
		if (IS_ERR(supply))
			return PTR_ERR(supply);
		h->supply = supply;
		h->nb.notifier_call = lem_supply_event;
		regulator_register_notifier(supply, &h->nb);
		return 0;
	case LEMNOS_POWER_RESET:
		rst = reset_control_get_exclusive(p->dev, name);
		if (IS_ERR(rst))
			return PTR_ERR(rst);
		h->rst = rst;
		return 0;
	default:
		return -EINVAL;
	}
}

/* The handle of that kind and name, opened if it's the first use. */
static struct lem_handle *lem_handle_find(struct lem_power *p, u32 kind, const char *name)
{
	struct lem_handle *h;
	int ret;

	for (u32 i = 0; i < p->n; i++)
		if (p->h[i].kind == kind && !strcmp(p->h[i].name, name))
			return &p->h[i];
	if (p->n == LEMNOS_MAX_POWER)
		return ERR_PTR(-ENOSPC);
	h = &p->h[p->n];
	memset(h, 0, sizeof(*h));
	strscpy(h->name, name, sizeof(h->name));
	h->kind = kind;
	h->index = p->n;
	h->power = p;
	ret = lem_handle_open(p, h);
	if (ret)
		return ERR_PTR(ret);
	p->n++;
	return h;
}

static long lem_clock(struct lem_handle *h, struct lemnos_power *r)
{
	long rate;
	int ret;

	switch (r->op) {
	case LEMNOS_POWER_ENABLE:
		if (h->on)
			return 0;
		ret = clk_prepare_enable(h->clk);
		h->on = !ret;
		return ret;
	case LEMNOS_POWER_DISABLE:
		if (h->on)
			clk_disable_unprepare(h->clk);
		h->on = false;
		return 0;
	case LEMNOS_POWER_STATUS:
		r->value = h->on;
		return 0;
	case LEMNOS_POWER_GET:
		r->value = clk_get_rate(h->clk);
		return 0;
	case LEMNOS_POWER_SET:
		rate = clk_round_rate(h->clk, r->value);
		if (rate < 0)
			return rate;
		ret = clk_set_rate(h->clk, rate);
		if (ret)
			return ret;
		r->value = clk_get_rate(h->clk);
		return 0;
	default:
		return -EINVAL;
	}
}

static long lem_supply(struct lem_handle *h, struct lemnos_power *r)
{
	int ret;

	switch (r->op) {
	case LEMNOS_POWER_ENABLE:
		if (h->on)
			return 0;
		ret = regulator_enable(h->supply);
		h->on = !ret;
		return ret;
	case LEMNOS_POWER_DISABLE:
		if (!h->on)
			return 0;
		ret = regulator_disable(h->supply);
		h->on = ret != 0;
		return ret;
	case LEMNOS_POWER_STATUS:
		ret = regulator_is_enabled(h->supply);
		if (ret < 0)
			return ret;
		r->value = ret;
		return 0;
	case LEMNOS_POWER_GET:
		ret = regulator_get_voltage(h->supply);
		if (ret < 0)
			return ret;
		r->value = ret;
		return 0;
	case LEMNOS_POWER_SET:
		if (r->value > INT_MAX || r->value2 > INT_MAX || r->value > r->value2)
			return -EINVAL;
		ret = regulator_set_voltage(h->supply, r->value, r->value2);
		if (ret)
			return ret;
		ret = regulator_get_voltage(h->supply);
		if (ret < 0)
			return ret;
		r->value = ret;
		return 0;
	default:
		return -EINVAL;
	}
}

static long lem_reset(struct lem_handle *h, struct lemnos_power *r)
{
	int ret;

	switch (r->op) {
	case LEMNOS_POWER_ENABLE:
		return reset_control_deassert(h->rst);
	case LEMNOS_POWER_DISABLE:
		return reset_control_assert(h->rst);
	case LEMNOS_POWER_PULSE:
		return reset_control_reset(h->rst);
	case LEMNOS_POWER_STATUS:
		ret = reset_control_status(h->rst);
		if (ret < 0)
			return ret;
		r->value = ret;
		return 0;
	default:
		return -EINVAL;
	}
}

long lem_power_request(struct lem_power *p, struct lemnos_power *r)
{
	struct lem_handle *h;
	long ret;

	if (strnlen(r->name, sizeof(r->name)) == sizeof(r->name))
		return -EINVAL;
	mutex_lock(&p->lock);
	h = lem_handle_find(p, r->kind, r->name);
	if (IS_ERR(h)) {
		ret = PTR_ERR(h);
		goto out;
	}
	switch (h->kind) {
	case LEMNOS_POWER_CLOCK:
		ret = lem_clock(h, r);
		break;
	case LEMNOS_POWER_SUPPLY:
		ret = lem_supply(h, r);
		break;
	default:
		ret = lem_reset(h, r);
		break;
	}
	r->handle = h->index;
	/* ENOTSUPP is the kernel's own; user space knows EOPNOTSUPP. */
	if (ret == -ENOTSUPP)
		ret = -EOPNOTSUPP;
out:
	mutex_unlock(&p->lock);
	return ret;
}

void lem_power_destroy(struct lem_power *p)
{
	for (u32 i = 0; i < p->n; i++) {
		struct lem_handle *h = &p->h[i];

		switch (h->kind) {
		case LEMNOS_POWER_CLOCK:
			if (h->on)
				clk_disable_unprepare(h->clk);
			clk_put(h->clk);
			break;
		case LEMNOS_POWER_SUPPLY:
			regulator_unregister_notifier(h->supply, &h->nb);
			if (h->on)
				regulator_disable(h->supply);
			regulator_put(h->supply);
			break;
		default:
			reset_control_put(h->rst);
			break;
		}
	}
	mutex_destroy(&p->lock);
	kfree(p);
}
