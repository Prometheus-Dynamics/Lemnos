// SPDX-License-Identifier: GPL-2.0
/*
 * The event stream's queue, and temperature watches (see lemnos.h).
 * Interrupts bypass the queue, on the core's ready list, so they can't be
 * lost to it.
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <linux/ktime.h>
#include <linux/mutex.h>
#include <linux/spinlock.h>
#include <linux/string.h>
#include <linux/thermal.h>
#include <linux/wait.h>
#include <linux/workqueue.h>

#include "lemnos_internal.h"

#define LEM_EVENTS 64
#define LEM_WATCH_MIN_MS 10

static DEFINE_SPINLOCK(lem_events_lock);
static struct lemnos_event lem_events[LEM_EVENTS];
static u32 lem_events_head, lem_events_count, lem_events_seq;

/* Full: the newest is lost, and the seq gap says so. */
void lem_event_post(u32 kind, u32 block, u32 index, s64 value)
{
	u64 ns = ktime_get_ns();
	unsigned long flags;

	spin_lock_irqsave(&lem_events_lock, flags);
	lem_events_seq++;
	if (lem_events_count < LEM_EVENTS) {
		lem_events[(lem_events_head + lem_events_count) % LEM_EVENTS] = (struct lemnos_event){
			.block = block,
			.index = index,
			.count = lem_events_seq,
			.kind = kind,
			.ns = ns,
			.value = value,
		};
		lem_events_count++;
	}
	spin_unlock_irqrestore(&lem_events_lock, flags);
	wake_up_interruptible(&lem_wait);
}

bool lem_event_pending(void)
{
	unsigned long flags;
	bool any;

	spin_lock_irqsave(&lem_events_lock, flags);
	any = lem_events_count;
	spin_unlock_irqrestore(&lem_events_lock, flags);
	return any;
}

bool lem_event_take(struct lemnos_event *e)
{
	unsigned long flags;
	bool got = false;

	spin_lock_irqsave(&lem_events_lock, flags);
	if (lem_events_count) {
		*e = lem_events[lem_events_head];
		lem_events_head = (lem_events_head + 1) % LEM_EVENTS;
		lem_events_count--;
		got = true;
	}
	spin_unlock_irqrestore(&lem_events_lock, flags);
	return got;
}

/* --- temperature --- */

struct lem_watch {
	struct delayed_work work;
	struct thermal_zone_device *zone;
	u32 index;
	int low, high;
	unsigned long period;
	bool above;
	bool used;
};

static DEFINE_MUTEX(lem_watches_lock);
static struct lem_watch lem_watches[LEMNOS_MAX_THERMAL_WATCHES];

static struct thermal_zone_device *lem_zone(const char *name, size_t size)
{
	if (strnlen(name, size) == size || !*name)
		return ERR_PTR(-EINVAL);
	return thermal_zone_get_zone_by_name(name);
}

long lem_thermal_read(struct lemnos_thermal *t)
{
	struct thermal_zone_device *zone = lem_zone(t->zone, sizeof(t->zone));
	int temp, ret;

	if (IS_ERR(zone))
		return PTR_ERR(zone);
	ret = thermal_zone_get_temp(zone, &temp);
	if (ret)
		return ret;
	t->temperature = temp;
	return 0;
}

static void lem_watch_tick(struct work_struct *work)
{
	struct lem_watch *w = container_of(to_delayed_work(work), struct lem_watch, work);
	int temp;

	/* A zone that fails a read is tried again next period. */
	if (!thermal_zone_get_temp(w->zone, &temp)) {
		bool cross = w->above ? temp <= w->low : temp >= w->high;

		if (cross) {
			w->above = !w->above;
			lem_event_post(LEMNOS_EVENT_TEMPERATURE, 0, w->index, temp);
		}
	}
	schedule_delayed_work(&w->work, w->period);
}

long lem_thermal_watch(struct lemnos_thermal_watch *r)
{
	struct thermal_zone_device *zone = lem_zone(r->zone, sizeof(r->zone));
	long ret = -ENOSPC;

	if (IS_ERR(zone))
		return PTR_ERR(zone);
	if (r->low_mc >= r->high_mc || r->period_ms < LEM_WATCH_MIN_MS)
		return -EINVAL;
	mutex_lock(&lem_watches_lock);
	for (u32 i = 0; i < ARRAY_SIZE(lem_watches); i++) {
		struct lem_watch *w = &lem_watches[i];

		if (w->used)
			continue;
		*w = (struct lem_watch){
			.zone = zone,
			.index = i,
			.low = r->low_mc,
			.high = r->high_mc,
			.period = msecs_to_jiffies(r->period_ms),
			.used = true,
		};
		INIT_DELAYED_WORK(&w->work, lem_watch_tick);
		schedule_delayed_work(&w->work, 0);
		r->watch = i;
		ret = 0;
		break;
	}
	mutex_unlock(&lem_watches_lock);
	return ret;
}

void lem_events_reset(void)
{
	unsigned long flags;

	mutex_lock(&lem_watches_lock);
	for (u32 i = 0; i < ARRAY_SIZE(lem_watches); i++) {
		if (!lem_watches[i].used)
			continue;
		cancel_delayed_work_sync(&lem_watches[i].work);
		lem_watches[i].used = false;
	}
	mutex_unlock(&lem_watches_lock);
	spin_lock_irqsave(&lem_events_lock, flags);
	lem_events_head = 0;
	lem_events_count = 0;
	lem_events_seq = 0;
	spin_unlock_irqrestore(&lem_events_lock, flags);
}
