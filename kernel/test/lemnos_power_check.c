// SPDX-License-Identifier: GPL-2.0
/*
 * A block's clocks, supplies and resets, and the event stream: bind and
 * unbind, a supply fault, a temperature limit crossed both ways.
 *
 * Usage: lemnos_power_check /dev/lemnos <device name> <bind path> <unbind path>
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

#include "../lemnos.h"
#include "find_block.h"

/* From the kernel's regulator consumer header, which user space does not get. */
#define REGULATOR_EVENT_OVER_CURRENT 0x02

#define PARAM "/sys/module/lemnos_test_bus/parameters/"

static int failures, fd;
static __u32 block;

#define CHECK(cond, ...)                                                   \
	do {                                                               \
		int ok_ = !!(cond);                                        \
		printf(ok_ ? "ok   " : "FAIL ");                           \
		failures += !ok_;                                          \
		printf(__VA_ARGS__);                                       \
		printf("\n");                                              \
	} while (0)

static int put(const char *path, const char *text)
{
	int f = open(path, O_WRONLY);
	int r = f >= 0 && write(f, text, strlen(text)) > 0 ? 0 : -1;

	if (f >= 0)
		close(f);
	return r;
}

/* 0 or -errno; *value (if given) in and out. */
static int power(uint32_t kind, uint32_t op, const char *name, uint64_t *value, uint64_t value2)
{
	struct lemnos_power p = { .block = block, .kind = kind, .op = op, .value2 = value2 };
	int r;

	strncpy(p.name, name, sizeof(p.name) - 1);
	if (value)
		p.value = *value;
	r = ioctl(fd, LEMNOS_IOC_POWER, &p) ? -errno : 0;
	if (value)
		*value = p.value;
	return r;
}

/* The next event of `kind`, skipping others, within a second. */
static int next_event(uint32_t kind, struct lemnos_event *e)
{
	struct pollfd p = { .fd = fd, .events = POLLIN };

	for (int i = 0; i < 16; i++) {
		if (poll(&p, 1, 1000) != 1 || read(fd, e, sizeof(*e)) != sizeof(*e))
			return -1;
		if (e->kind == kind)
			return 0;
	}
	return -1;
}

int main(int argc, char **argv)
{
	if (argc < 5) {
		fprintf(stderr, "usage: lemnos_power_check /dev/lemnos <device> <bind> <unbind>\n");
		return 2;
	}
	const char *name = argv[2];
	struct lemnos_event e;
	uint64_t v = 0;

	setvbuf(stdout, NULL, _IOLBF, 0);
	fd = open(argv[1], O_RDWR | O_CLOEXEC | O_NONBLOCK);
	if (fd < 0) {
		perror(argv[1]);
		return 2;
	}

	/* The block was unbound by the previous check: binding is the first event. */
	char path[128];

	snprintf(path, sizeof(path), "/sys/bus/platform/devices/%s/driver_override", name);
	CHECK(!put(path, "lemnos") && !put(argv[3], name), "block bound");
	CHECK(!next_event(LEMNOS_EVENT_BLOCK_BOUND, &e), "event: block bound");
	block = find_block(fd, name);
	CHECK(block && e.block == block, "for block %u", block);

	/* Clock. */
	CHECK(!power(LEMNOS_POWER_CLOCK, LEMNOS_POWER_GET, "core", &v, 0) && v == 25000000,
	      "clock rate: %llu Hz", (unsigned long long)v);
	CHECK(!power(LEMNOS_POWER_CLOCK, LEMNOS_POWER_ENABLE, "core", NULL, 0), "clock on");
	CHECK(!power(LEMNOS_POWER_CLOCK, LEMNOS_POWER_ENABLE, "core", NULL, 0), "clock on again");
	CHECK(!power(LEMNOS_POWER_CLOCK, LEMNOS_POWER_STATUS, "core", &v, 0) && v == 1, "status on");
	CHECK(!power(LEMNOS_POWER_CLOCK, LEMNOS_POWER_DISABLE, "core", NULL, 0), "clock off");
	CHECK(!power(LEMNOS_POWER_CLOCK, LEMNOS_POWER_STATUS, "core", &v, 0) && v == 0, "status off");
	CHECK(power(LEMNOS_POWER_CLOCK, LEMNOS_POWER_GET, "nope", &v, 0) < 0, "an unknown clock is refused");

	/* Supply. */
	CHECK(!power(LEMNOS_POWER_SUPPLY, LEMNOS_POWER_ENABLE, "vdd", NULL, 0), "supply on");
	CHECK(!power(LEMNOS_POWER_SUPPLY, LEMNOS_POWER_STATUS, "vdd", &v, 0) && v == 1, "supply status on");
	v = 1800000;
	CHECK(!power(LEMNOS_POWER_SUPPLY, LEMNOS_POWER_SET, "vdd", &v, 1800000) && v == 1800000,
	      "set 1.8 V: %llu uV", (unsigned long long)v);
	CHECK(!power(LEMNOS_POWER_SUPPLY, LEMNOS_POWER_GET, "vdd", &v, 0) && v == 1800000, "reads 1.8 V");
	v = 2000000;
	CHECK(power(LEMNOS_POWER_SUPPLY, LEMNOS_POWER_SET, "vdd", &v, 1000000) == -EINVAL,
	      "minimum past maximum: EINVAL");
	CHECK(power(LEMNOS_POWER_SUPPLY, LEMNOS_POWER_GET, "nope", &v, 0) < 0, "an unknown supply is refused");
	CHECK(power(LEMNOS_POWER_SUPPLY, LEMNOS_POWER_GET, "", &v, 0) == -EINVAL, "a supply needs a name");

	/* A fault on the supply. */
	CHECK(!put(PARAM "fault", "1"), "the supply reports an over-current");
	CHECK(!next_event(LEMNOS_EVENT_SUPPLY_FAULT, &e) && e.block == block &&
		      (e.value & REGULATOR_EVENT_OVER_CURRENT),
	      "event: supply fault, value 0x%llx", (unsigned long long)e.value);

	/* Reset. */
	int r = power(LEMNOS_POWER_RESET, LEMNOS_POWER_DISABLE, "rst", NULL, 0);

	CHECK(!r, "reset asserted (%s)", r ? strerror(-r) : "ok");
	CHECK(!power(LEMNOS_POWER_RESET, LEMNOS_POWER_STATUS, "rst", &v, 0) && v == 1, "status asserted");
	CHECK(!power(LEMNOS_POWER_RESET, LEMNOS_POWER_ENABLE, "rst", NULL, 0), "reset released");
	CHECK(!power(LEMNOS_POWER_RESET, LEMNOS_POWER_STATUS, "rst", &v, 0) && v == 0, "status released");
	CHECK(!power(LEMNOS_POWER_RESET, LEMNOS_POWER_PULSE, "rst", NULL, 0), "reset pulsed");
	CHECK(power(LEMNOS_POWER_RESET, 99, "rst", NULL, 0) == -EINVAL, "an unknown op: EINVAL");
	CHECK(power(99, LEMNOS_POWER_GET, "x", NULL, 0) == -EINVAL, "an unknown kind: EINVAL");

	/* Temperature. */
	struct lemnos_thermal t = { 0 };

	strcpy(t.zone, "lemnos-test");
	CHECK(!ioctl(fd, LEMNOS_IOC_THERMAL_READ, &t) && t.temperature == 40000,
	      "zone reads %lld mC", (long long)t.temperature);
	struct lemnos_thermal_watch w = { .low_mc = 45000, .high_mc = 60000, .period_ms = 10 };

	strcpy(w.zone, "lemnos-test");
	CHECK(!ioctl(fd, LEMNOS_IOC_THERMAL_WATCH, &w), "watching 45 to 60 degrees");
	CHECK(!put(PARAM "temp_mc", "70000"), "the zone heats up");
	CHECK(!next_event(LEMNOS_EVENT_TEMPERATURE, &e) && e.value == 70000, "event: high, %lld mC",
	      (long long)e.value);
	CHECK(!put(PARAM "temp_mc", "30000"), "the zone cools");
	CHECK(!next_event(LEMNOS_EVENT_TEMPERATURE, &e) && e.value == 30000, "event: low, %lld mC",
	      (long long)e.value);
	struct lemnos_thermal_watch bad = { .low_mc = 2, .high_mc = 1, .period_ms = 100 };

	strcpy(bad.zone, "lemnos-test");
	CHECK(ioctl(fd, LEMNOS_IOC_THERMAL_WATCH, &bad) < 0 && errno == EINVAL, "low past high: EINVAL");
	strcpy(t.zone, "no-such-zone");
	CHECK(ioctl(fd, LEMNOS_IOC_THERMAL_READ, &t) < 0, "an unknown zone is refused");

	/* Unbinding with the supply on: turned off, and the event arrives. */
	CHECK(!put(argv[4], name), "unbound with the supply still on");
	CHECK(!next_event(LEMNOS_EVENT_BLOCK_UNBOUND, &e) && e.block == block, "event: block unbound");
	CHECK(!put(path, "\n"), "override cleared");

	close(fd);
	printf("%s: %d failed\n", argv[1], failures);
	return failures ? 1 : 0;
}
