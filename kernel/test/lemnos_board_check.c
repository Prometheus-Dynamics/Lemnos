// SPDX-License-Identifier: GPL-2.0
/*
 * What lemnos_power_check covers, for a real block on a board, changing as
 * little as it can: the bind and unbind events, the block's clock (its rate
 * and state; turned on and off again only with "toggle"), a thermal zone read
 * beside sysfs, and a watch set around the temperature the zone has now, which
 * must fire at once on the side it is already past.
 *
 * Usage: lemnos_board_check /dev/lemnos <device> <bind path> <unbind path> <clock name or -> <zone type> <zone sysfs temp> [toggle]
 *   The device must be unbound, with the module loaded owning its compatible.
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "find_block.h"

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

static long long get(const char *path)
{
	char text[32] = { 0 };
	int f = open(path, O_RDONLY);
	long long v = f >= 0 && read(f, text, sizeof(text) - 1) > 0 ? atoll(text) : -1;

	if (f >= 0)
		close(f);
	return v;
}

/* 0 or -errno; *value (if given) in and out. */
static int power(uint32_t kind, uint32_t op, const char *name, uint64_t *value)
{
	struct lemnos_power p = { .block = block, .kind = kind, .op = op };
	int r;

	strncpy(p.name, name, sizeof(p.name) - 1);
	if (value)
		p.value = *value;
	r = ioctl(fd, LEMNOS_IOC_POWER, &p) ? -errno : 0;
	if (value)
		*value = p.value;
	return r;
}

/* The next event of `kind`, skipping others, within `ms` each. */
static int next_event(uint32_t kind, struct lemnos_event *e, int ms)
{
	struct pollfd p = { .fd = fd, .events = POLLIN };

	for (int i = 0; i < 16; i++) {
		if (poll(&p, 1, ms) != 1 || read(fd, e, sizeof(*e)) != sizeof(*e))
			return -1;
		if (e->kind == kind)
			return 0;
	}
	return -1;
}

int main(int argc, char **argv)
{
	if (argc < 8) {
		fprintf(stderr,
			"usage: lemnos_board_check /dev/lemnos <device> <bind> <unbind> <clock or -> <zone> <zone temp file> [toggle]\n");
		return 2;
	}
	const char *name = argv[2], *clock = strcmp(argv[5], "-") ? argv[5] : "";
	int toggle = argc > 8 && !strcmp(argv[8], "toggle");
	struct lemnos_event e;
	uint64_t v = 0;
	char path[160];

	setvbuf(stdout, NULL, _IOLBF, 0);
	fd = open(argv[1], O_RDWR | O_CLOEXEC | O_NONBLOCK);
	if (fd < 0) {
		perror(argv[1]);
		return 2;
	}
	snprintf(path, sizeof(path), "/sys/bus/platform/devices/%s/driver_override", name);
	CHECK(!put(path, "lemnos") && !put(argv[3], name), "block bound");
	CHECK(!next_event(LEMNOS_EVENT_BLOCK_BOUND, &e, 1000), "event: block bound");
	block = find_block(fd, name);
	CHECK(block && e.block == block, "for block %u", block);

	int r = power(LEMNOS_POWER_CLOCK, LEMNOS_POWER_GET, clock, &v);

	CHECK(!r, "clock \"%s\" rate: %llu Hz (%s)", clock, (unsigned long long)v, r ? strerror(-r) : "ok");
	r = power(LEMNOS_POWER_CLOCK, LEMNOS_POWER_STATUS, clock, &v);
	CHECK(!r, "clock status: %llu", (unsigned long long)v);
	if (toggle) {
		CHECK(!power(LEMNOS_POWER_CLOCK, LEMNOS_POWER_ENABLE, clock, NULL), "clock on");
		CHECK(!power(LEMNOS_POWER_CLOCK, LEMNOS_POWER_STATUS, clock, &v) && v == 1, "status on");
		printf("     (read the clock's enable count now)\n");
		sleep(2);
		CHECK(!power(LEMNOS_POWER_CLOCK, LEMNOS_POWER_DISABLE, clock, NULL), "clock off");
		CHECK(!power(LEMNOS_POWER_CLOCK, LEMNOS_POWER_STATUS, clock, &v) && v == 0, "status off");
	}
	CHECK(power(LEMNOS_POWER_CLOCK, LEMNOS_POWER_GET, "nope", &v) < 0, "an unknown clock is refused");
	CHECK(power(LEMNOS_POWER_SUPPLY, LEMNOS_POWER_GET, "nope", &v) < 0, "an unknown supply is refused");
	CHECK(power(LEMNOS_POWER_RESET, LEMNOS_POWER_STATUS, "nope", &v) < 0, "an unknown reset is refused");

	struct lemnos_thermal t = { 0 };

	strncpy(t.zone, argv[6], sizeof(t.zone) - 1);
	long long before = get(argv[7]);
	r = ioctl(fd, LEMNOS_IOC_THERMAL_READ, &t) ? -errno : 0;
	long long after = get(argv[7]);

	CHECK(!r, "zone %s reads %lld mC; sysfs before %lld, after %lld", argv[6], (long long)t.temperature,
	      before, after);
	long long lo = before < after ? before : after, hi = before < after ? after : before;

	CHECK(t.temperature >= lo - 3000 && t.temperature <= hi + 3000, "within 3 degrees of sysfs");

	/* Already above the high limit: the first poll reports it. */
	struct lemnos_thermal_watch w = { .low_mc = (int)t.temperature - 20000,
					  .high_mc = (int)t.temperature - 10000,
					  .period_ms = 50 };

	strncpy(w.zone, argv[6], sizeof(w.zone) - 1);
	CHECK(!ioctl(fd, LEMNOS_IOC_THERMAL_WATCH, &w), "watching %d to %d mC", w.low_mc, w.high_mc);
	r = next_event(LEMNOS_EVENT_TEMPERATURE, &e, 1000);
	CHECK(!r && e.index == w.watch && e.value >= w.high_mc, "event: high, %lld mC",
	      r ? -1LL : (long long)e.value);
	strcpy(t.zone, "no-such-zone");
	CHECK(ioctl(fd, LEMNOS_IOC_THERMAL_READ, &t) < 0, "an unknown zone is refused");

	CHECK(!put(argv[4], name), "unbound");
	CHECK(!next_event(LEMNOS_EVENT_BLOCK_UNBOUND, &e, 1000) && e.block == block, "event: block unbound");
	CHECK(!put(path, "\n"), "override cleared");
	close(fd);
	printf("%s: %d failed\n", argv[1], failures);
	return failures ? 1 : 0;
}
