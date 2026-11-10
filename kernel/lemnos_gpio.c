// SPDX-License-Identifier: GPL-2.0
/*
 * GPIO lines through gpiolib (see lemnos.h).
 *
 * gpiod_request() isn't exported, so a line is taken the way any module
 * outside a device tree takes one: gpio_request() on the chip's base plus
 * the offset. gpiolib refuses a line someone holds.
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <linux/delay.h>
#include <linux/device.h>
#include <linux/file.h>
#include <linux/gpio.h>
#include <linux/gpio/consumer.h>
#include <linux/gpio/driver.h>
#include <linux/interrupt.h>
#include <linux/mutex.h>
#include <linux/pinctrl/pinconf-generic.h>
#include <linux/slab.h>
#include <linux/spinlock.h>
#include <linux/string.h>
#include <linux/uaccess.h>
#include <linux/wait.h>

#include "lemnos_internal.h"

#define LEM_GPIO_RESOURCE_MAX 40
#define LEM_GPIO_LABEL_MAX 16
#define LEM_GPIO_EVENTS 16
#define LEM_GPIO_BIAS (LEMNOS_GPIO_PULL_UP | LEMNOS_GPIO_PULL_DOWN | LEMNOS_GPIO_BIAS_OFF)
#define LEM_GPIO_DRIVE (LEMNOS_GPIO_OPEN_DRAIN | LEMNOS_GPIO_OPEN_SOURCE)
#define LEM_GPIO_EDGES (LEMNOS_GPIO_RISING | LEMNOS_GPIO_FALLING)
#define LEM_GPIO_FLAGS (LEMNOS_GPIO_OUTPUT | LEMNOS_GPIO_ACTIVE_LOW | LEM_GPIO_BIAS | \
			LEM_GPIO_DRIVE | LEM_GPIO_EDGES)

struct lem_line {
	struct gpio_desc *desc;
	int gpio;
	struct mutex lock;		/* configuration */
	bool active_low;
	u32 flags;
	int irq;			/* 0 when no edges are asked for */

	spinlock_t events_lock;		/* everything below */
	wait_queue_head_t wait;
	u64 stamp;			/* the hard handler's time of the last edge */
	u32 seq;
	u32 head;
	u32 count;
	struct lemnos_gpio_event events[LEM_GPIO_EVENTS];
	char label[LEM_GPIO_LABEL_MAX];
};

static irqreturn_t lem_line_edge(int irq, void *data)
{
	struct lem_line *l = data;

	WRITE_ONCE(l->stamp, ktime_get_ns());
	return IRQ_WAKE_THREAD;
}

static irqreturn_t lem_line_thread(int irq, void *data)
{
	struct lem_line *l = data;
	int level = gpiod_get_raw_value_cansleep(l->desc);
	/* A nested controller has no hard handler to have stamped the edge. */
	u64 ns = xchg(&l->stamp, 0) ?: ktime_get_ns();
	unsigned long flags;

	if (level < 0)
		return IRQ_HANDLED;
	spin_lock_irqsave(&l->events_lock, flags);
	/* Full: the newest one is lost, and the seq gap says so. */
	l->seq++;
	if (l->count < LEM_GPIO_EVENTS) {
		l->events[(l->head + l->count) % LEM_GPIO_EVENTS] = (struct lemnos_gpio_event){
			.ns = ns,
			.seq = l->seq,
			.rising = (level ^ l->active_low) != 0,
		};
		l->count++;
	}
	spin_unlock_irqrestore(&l->events_lock, flags);
	wake_up_interruptible(&l->wait);
	return IRQ_HANDLED;
}

static void lem_line_stop_edges(struct lem_line *l)
{
	if (l->irq)
		free_irq(l->irq, l);
	l->irq = 0;
}

static void lem_line_release(void *data)
{
	struct lem_line *l = data;

	lem_line_stop_edges(l);
	gpio_free(l->gpio);
	kfree(l);
}

static bool lem_line_events_ready(struct lem_line *l)
{
	unsigned long flags;
	bool any;

	spin_lock_irqsave(&l->events_lock, flags);
	any = l->count;
	spin_unlock_irqrestore(&l->events_lock, flags);
	return any;
}

static ssize_t lem_line_read(void *data, struct file *file, char __user *buf, size_t len)
{
	struct lem_line *l = data;
	struct lemnos_gpio_event ev[LEM_GPIO_EVENTS];
	size_t want = min_t(size_t, len / sizeof(ev[0]), ARRAY_SIZE(ev));
	unsigned long flags;
	size_t n = 0;
	int ret;

	if (!want)
		return -EINVAL;
	for (;;) {
		spin_lock_irqsave(&l->events_lock, flags);
		while (n < want && l->count) {
			ev[n++] = l->events[l->head];
			l->head = (l->head + 1) % LEM_GPIO_EVENTS;
			l->count--;
		}
		spin_unlock_irqrestore(&l->events_lock, flags);
		if (n)
			break;
		if (file->f_flags & O_NONBLOCK)
			return -EAGAIN;
		ret = wait_event_interruptible(l->wait, lem_line_events_ready(l));
		if (ret)
			return ret;
	}
	return copy_to_user(buf, ev, n * sizeof(ev[0])) ? -EFAULT : n * sizeof(ev[0]);
}

static __poll_t lem_line_poll(void *data, struct file *file, poll_table *wait)
{
	struct lem_line *l = data;

	poll_wait(file, &l->wait, wait);
	return lem_line_events_ready(l) ? EPOLLIN | EPOLLRDNORM : 0;
}

static const struct lem_claim_ops lem_line_ops = {
	.release = lem_line_release,
	.read = lem_line_read,
	.poll = lem_line_poll,
};

static int lem_chip_match(struct gpio_chip *gc, const void *data)
{
	return !strcmp(dev_name(gpio_device_to_device(gc->gpiodev)), data);
}

long lem_gpio_request(const struct lemnos_gpio_request *r)
{
	char resource[LEM_GPIO_RESOURCE_MAX], chip[LEM_GPIO_LABEL_MAX];
	struct gpio_device *gdev;
	struct gpio_desc *desc;
	struct lem_line *l;
	int ret;

	if (r->reserved)
		return -EINVAL;
	snprintf(resource, sizeof(resource), "gpio-%u:%u", r->chip, r->offset);
	if (!lem_claim_held(r->claim, resource))
		return -EPERM;
	snprintf(chip, sizeof(chip), "gpiochip%u", r->chip);
	gdev = gpio_device_find(chip, lem_chip_match);
	if (!gdev)
		return -ENODEV;
	desc = gpio_device_get_desc(gdev, r->offset);
	if (IS_ERR(desc)) {
		gpio_device_put(gdev);
		return PTR_ERR(desc);
	}
	l = kzalloc(sizeof(*l), GFP_KERNEL);
	if (!l) {
		gpio_device_put(gdev);
		return -ENOMEM;
	}
	l->desc = desc;
	l->gpio = gpio_device_get_base(gdev) + r->offset;
	gpio_device_put(gdev);
	mutex_init(&l->lock);
	spin_lock_init(&l->events_lock);
	init_waitqueue_head(&l->wait);
	strscpy(l->label, "lemnos", sizeof(l->label));
	ret = gpio_request(l->gpio, l->label);
	if (ret) {
		kfree(l);
		return ret;
	}
	ret = lem_claim_attach(r->claim, resource, &lem_line_ops, l);
	if (ret)
		lem_line_release(l);
	return ret;
}

static int lem_line_set_one(struct gpio_desc *desc, enum pin_config_param param, u32 arg)
{
	int ret = gpiod_set_config(desc, pinconf_to_config_packed(param, arg));

	/* ENOTSUPP is the kernel's own; user space knows EOPNOTSUPP. */
	return ret == -ENOTSUPP ? -EOPNOTSUPP : ret;
}

static int lem_line_set_bias(struct lem_line *l, u32 flags)
{
	if (flags & LEMNOS_GPIO_PULL_UP)
		return lem_line_set_one(l->desc, PIN_CONFIG_BIAS_PULL_UP, 1);
	if (flags & LEMNOS_GPIO_PULL_DOWN)
		return lem_line_set_one(l->desc, PIN_CONFIG_BIAS_PULL_DOWN, 1);
	if (flags & LEMNOS_GPIO_BIAS_OFF)
		return lem_line_set_one(l->desc, PIN_CONFIG_BIAS_DISABLE, 0);
	return 0;
}

static int lem_line_set_drive(struct lem_line *l, u32 flags)
{
	if (flags & LEMNOS_GPIO_OPEN_DRAIN)
		return lem_line_set_one(l->desc, PIN_CONFIG_DRIVE_OPEN_DRAIN, 0);
	if (flags & LEMNOS_GPIO_OPEN_SOURCE)
		return lem_line_set_one(l->desc, PIN_CONFIG_DRIVE_OPEN_SOURCE, 0);
	return 0;
}

static int lem_line_start_edges(struct lem_line *l, u32 flags)
{
	unsigned long trigger = IRQF_ONESHOT;
	int irq, ret;

	if (flags & LEMNOS_GPIO_RISING)
		trigger |= l->active_low ? IRQF_TRIGGER_FALLING : IRQF_TRIGGER_RISING;
	if (flags & LEMNOS_GPIO_FALLING)
		trigger |= l->active_low ? IRQF_TRIGGER_RISING : IRQF_TRIGGER_FALLING;
	irq = gpiod_to_irq(l->desc);
	if (irq < 0)
		return irq;
	ret = request_threaded_irq(irq, lem_line_edge, lem_line_thread, trigger, l->label, l);
	if (ret)
		return ret;
	l->irq = irq;
	return 0;
}

static long lem_line_configure(struct lem_line *l, const struct lemnos_gpio_config *c)
{
	bool output = c->flags & LEMNOS_GPIO_OUTPUT;
	int ret;

	lem_line_stop_edges(l);
	l->active_low = c->flags & LEMNOS_GPIO_ACTIVE_LOW;
	ret = lem_line_set_bias(l, c->flags);
	if (ret)
		return ret;
	ret = output ? gpiod_direction_output_raw(l->desc, !!c->value ^ l->active_low)
		     : gpiod_direction_input(l->desc);
	if (ret)
		return ret;
	ret = lem_line_set_drive(l, c->flags);
	if (ret)
		return ret;
	if (c->debounce_us) {
		ret = gpiod_set_debounce(l->desc, c->debounce_us);
		if (ret)
			return ret == -ENOTSUPP ? -EOPNOTSUPP : ret;
	}
	return c->flags & LEM_GPIO_EDGES ? lem_line_start_edges(l, c->flags) : 0;
}

long lem_gpio_config(const struct lemnos_gpio_config *c)
{
	CLASS(fd, f)(c->claim);
	struct lem_line *l = lem_claim_attached(fd_file(f), &lem_line_ops);
	long ret;

	if (!l)
		return -EPERM;
	if ((c->flags & ~LEM_GPIO_FLAGS) || hweight32(c->flags & LEM_GPIO_BIAS) > 1 ||
	    hweight32(c->flags & LEM_GPIO_DRIVE) > 1 || c->value > 1 ||
	    ((c->flags & LEMNOS_GPIO_OUTPUT) && (c->flags & LEM_GPIO_EDGES)))
		return -EINVAL;
	mutex_lock(&l->lock);
	ret = lem_line_configure(l, c);
	if (!ret)
		l->flags = c->flags;
	mutex_unlock(&l->lock);
	return ret;
}

long lem_gpio_get(struct lemnos_gpio_value *v)
{
	CLASS(fd, f)(v->claim);
	struct lem_line *l = lem_claim_attached(fd_file(f), &lem_line_ops);
	int level;

	if (!l)
		return -EPERM;
	level = gpiod_get_raw_value_cansleep(l->desc);
	if (level < 0)
		return level;
	v->value = (level ^ l->active_low) != 0;
	return 0;
}

long lem_gpio_set(const struct lemnos_gpio_value *v)
{
	CLASS(fd, f)(v->claim);
	struct lem_line *l = lem_claim_attached(fd_file(f), &lem_line_ops);

	if (!l)
		return -EPERM;
	if (v->value > 1)
		return -EINVAL;
	if (!(READ_ONCE(l->flags) & LEMNOS_GPIO_OUTPUT))
		return -EPERM;
	return gpiod_set_raw_value_cansleep(l->desc, !!v->value ^ l->active_low);
}
