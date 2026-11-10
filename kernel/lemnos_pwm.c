// SPDX-License-Identifier: GPL-2.0
/*
 * PWM channels through the PWM core (see lemnos.h).
 *
 * The core offers modules no way to ask for a channel by number except its
 * translation helper, which takes a chip and a channel and requests it the
 * normal way; of_pwm_xlate_with_flags() is that helper, and it works from
 * its arguments alone, device tree or not. The chip is found as the "pwm"
 * class child of its parent, a platform device.
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <linux/device.h>
#include <linux/file.h>
#include <linux/of.h>
#include <linux/platform_device.h>
#include <linux/pwm.h>
#include <linux/slab.h>
#include <linux/string.h>

#include "lemnos_internal.h"

#define LEM_PWM_RESOURCE_MAX LEMNOS_NAME_MAX

struct lem_channel {
	struct pwm_device *pwm;
};

/* Leaves the output as it is: nothing here turns anything off. */
static void lem_channel_release(void *data)
{
	struct lem_channel *c = data;

	pwm_put(c->pwm);
	kfree(c);
}

static const struct lem_claim_ops lem_channel_ops = {
	.release = lem_channel_release,
};

static int lem_is_pwm_chip(struct device *dev, const void *data)
{
	return dev->class && !strcmp(dev->class->name, "pwm");
}

static struct pwm_device *lem_pwm_find(const char *provider, u32 channel)
{
	struct of_phandle_args args = { .args_count = 1, .args = { channel } };
	struct device *parent, *dev;
	struct pwm_device *pwm;

	parent = bus_find_device_by_name(&platform_bus_type, NULL, provider);
	if (!parent)
		return ERR_PTR(-ENODEV);
	dev = device_find_child(parent, NULL, lem_is_pwm_chip);
	put_device(parent);
	if (!dev)
		return ERR_PTR(-ENODEV);
	pwm = of_pwm_xlate_with_flags(container_of(dev, struct pwm_chip, dev), &args);
	put_device(dev);
	return pwm;
}

long lem_pwm_request(struct lemnos_pwm_request *r)
{
	size_t len = strnlen(r->provider, sizeof(r->provider));
	char resource[LEM_PWM_RESOURCE_MAX];
	struct lem_channel *c;
	struct pwm_device *pwm;
	int ret;

	if (!len || len == sizeof(r->provider))
		return -EINVAL;
	snprintf(resource, sizeof(resource), "pwm-%s:%u", r->provider, r->channel);
	if (!lem_claim_held(r->claim, resource))
		return -EPERM;
	c = kzalloc(sizeof(*c), GFP_KERNEL);
	if (!c)
		return -ENOMEM;
	pwm = lem_pwm_find(r->provider, r->channel);
	if (IS_ERR(pwm)) {
		kfree(c);
		return PTR_ERR(pwm);
	}
	c->pwm = pwm;
	ret = lem_claim_attach(r->claim, resource, &lem_channel_ops, c);
	if (ret)
		lem_channel_release(c);
	return ret;
}

long lem_pwm_apply(const struct lemnos_pwm_state *s)
{
	CLASS(fd, f)(s->claim);
	struct lem_channel *c = lem_claim_attached(fd_file(f), &lem_channel_ops);
	struct pwm_state state = {
		.period = s->period_ns,
		.duty_cycle = s->duty_ns,
		.polarity = s->inverted ? PWM_POLARITY_INVERSED : PWM_POLARITY_NORMAL,
		.enabled = s->enabled,
	};

	if (!c)
		return -EPERM;
	if (s->enabled > 1 || s->inverted > 1 || s->reserved)
		return -EINVAL;
	return pwm_apply_might_sleep(c->pwm, &state);
}

long lem_pwm_get(struct lemnos_pwm_state *s)
{
	CLASS(fd, f)(s->claim);
	struct lem_channel *c = lem_claim_attached(fd_file(f), &lem_channel_ops);
	struct pwm_state state;

	if (!c)
		return -EPERM;
	pwm_get_state(c->pwm, &state);
	s->enabled = state.enabled;
	s->inverted = state.polarity == PWM_POLARITY_INVERSED;
	s->period_ns = state.period;
	s->duty_ns = state.duty_cycle;
	return 0;
}
