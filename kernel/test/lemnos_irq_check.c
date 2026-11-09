// SPDX-License-Identifier: GPL-2.0
/*
 * The control node's two test-only paths, on a bound block with the module
 * loaded test_map_unconfined=1 test_irq_inject=1:
 * - the registers through mmap (read only: region 0's first word);
 * - interrupts: one set pending at the controller reaches read() when
 *   enabled, waits while masked and arrives on enable, and the time from the
 *   injection to the handler and to user space, over many.
 *
 * Usage: lemnos_irq_check /dev/lemnos <device name> [rounds]
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

#include "../lemnos.h"
#include "find_block.h"

static int failures;

/* Evaluates `cond` once: it is often the call under test. */
#define CHECK(cond, ...)                                                   \
	do {                                                               \
		int ok_ = !!(cond);                                        \
		printf(ok_ ? "ok   " : "FAIL ");                           \
		failures += !ok_;                                          \
		printf(__VA_ARGS__);                                       \
		printf("\n");                                              \
	} while (0)

static uint64_t now_ns(void)
{
	struct timespec t;

	clock_gettime(CLOCK_MONOTONIC, &t);
	return (uint64_t)t.tv_sec * 1000000000u + (uint64_t)t.tv_nsec;
}

static int cmp(const void *a, const void *b)
{
	uint64_t x = *(const uint64_t *)a, y = *(const uint64_t *)b;

	return x < y ? -1 : x > y;
}

static void report(const char *what, uint64_t *v, int n)
{
	qsort(v, n, sizeof(*v), cmp);
	printf("     %s: p50 %.1f p99 %.1f max %.1f us (%d)\n", what, v[n / 2] / 1000.0,
	       v[n * 99 / 100] / 1000.0, v[n - 1] / 1000.0, n);
}

int main(int argc, char **argv)
{
	const char *node = argc > 1 ? argv[1] : "/dev/lemnos";
	const char *name = argc > 2 ? argv[2] : "";
	int rounds = argc > 3 ? atoi(argv[3]) : 2000;
	struct lemnos_irq_event ev;
	struct pollfd p;
	int fd = open(node, O_RDWR | O_CLOEXEC | O_NONBLOCK);

	setvbuf(stdout, NULL, _IOLBF, 0);
	if (fd < 0) {
		perror(node);
		return 2;
	}
	p = (struct pollfd){ .fd = fd, .events = POLLIN };
	__u32 block = find_block(fd, name);
	struct lemnos_info info = { .block = block };
	struct lemnos_irq zero = { .block = block };

	CHECK(block, "block %s is number %u", name, block);
	CHECK(!ioctl(fd, LEMNOS_IOC_INFO, &info) && info.irqs >= 1,
	      "info: %u irqs, registers %smappable", info.irqs,
	      info.flags & LEMNOS_INFO_MMIO_MAPPABLE ? "" : "not ");

	long page = sysconf(_SC_PAGESIZE);
	struct lemnos_region r = { .block = block };

	CHECK(!ioctl(fd, LEMNOS_IOC_REGION, &r), "region 0 offset 0x%llx", (unsigned long long)r.offset);
	volatile uint32_t *regs = mmap(NULL, page, PROT_READ, MAP_SHARED, fd, r.offset);

	CHECK(regs != MAP_FAILED, "region 0 mapped read-only");
	if (regs != MAP_FAILED) {
		uint32_t v = regs[0];

		CHECK(v != 0 && v != 0xffffffffu, "region 0, word 0: 0x%08x", v);
		munmap((void *)regs, page);
	}

	/* Enabled, then injected: one event. */
	CHECK(!ioctl(fd, LEMNOS_IOC_IRQ_ENABLE, &zero), "enable irq 0");
	int e = ioctl(fd, LEMNOS_IOC_TEST_IRQ, &zero);

	CHECK(!e, "inject irq 0 (%s)", e ? strerror(errno) : "ok");
	CHECK(poll(&p, 1, 1000) == 1, "event within 1 s");
	CHECK(read(fd, &ev, sizeof(ev)) == sizeof(ev) && ev.block == block && ev.irq == 0,
	      "read: block %u, irq %u, count %u", ev.block, ev.irq, ev.count);
	uint32_t first = ev.count;

	/*
	 * Masked since it fired: an injection is not delivered. A level interrupt
	 * is not resent on enable (a real device still holds its line, so it
	 * fires again); an injected one is gone once the controller took it.
	 * Either way the next one after enable arrives.
	 */
	CHECK(!ioctl(fd, LEMNOS_IOC_TEST_IRQ, &zero), "inject while masked");
	CHECK(poll(&p, 1, 200) == 0, "no event while masked (200 ms)");
	CHECK(!ioctl(fd, LEMNOS_IOC_IRQ_ENABLE, &zero), "enable");
	if (poll(&p, 1, 200) == 1 && read(fd, &ev, sizeof(ev)) == sizeof(ev))
		printf("     the masked one was resent on enable (an edge interrupt), count %u\n", ev.count);
	else
		printf("     the masked one was not resent (a level interrupt, its line not held)\n");
	CHECK(!ioctl(fd, LEMNOS_IOC_IRQ_ENABLE, &zero) && !ioctl(fd, LEMNOS_IOC_TEST_IRQ, &zero) &&
		      poll(&p, 1, 1000) == 1 && read(fd, &ev, sizeof(ev)) == sizeof(ev) &&
		      ev.count > first,
	      "the next one after enable arrives (count %u)", ev.count);
	struct lemnos_irq past = { .block = block, .index = info.irqs };

	CHECK(ioctl(fd, LEMNOS_IOC_TEST_IRQ, &past) < 0 && errno == EINVAL, "inject past the end: EINVAL");

	/* Latency, as the back end's own was measured: SCHED_FIFO 10, poll then read. */
	struct sched_param sp = { .sched_priority = 10 };
	int fifo = !sched_setscheduler(0, SCHED_FIFO, &sp);
	uint64_t *to_irq = calloc(rounds, sizeof(uint64_t));
	uint64_t *to_wake = calloc(rounds, sizeof(uint64_t));
	int n = 0, lost = 0;

	for (int i = 0; i < rounds; i++) {
		if (ioctl(fd, LEMNOS_IOC_IRQ_ENABLE, &zero))
			break;
		if (poll(&p, 1, 0) == 1 && read(fd, &ev, sizeof(ev)) > 0) { /* a stray: redo */
			i--;
			continue;
		}
		uint64_t t0 = now_ns();

		if (ioctl(fd, LEMNOS_IOC_TEST_IRQ, &zero))
			break;
		if (poll(&p, 1, 1000) != 1 || read(fd, &ev, sizeof(ev)) != sizeof(ev)) {
			if (++lost >= 3)
				break;
			continue;
		}
		uint64_t t1 = now_ns();

		to_irq[n] = ev.ns - t0;
		to_wake[n] = t1 - t0;
		n++;
	}
	CHECK(n == rounds && !lost, "%d of %d injected interrupts read (%s), %d lost", n, rounds,
	      fifo ? "SCHED_FIFO 10" : "SCHED_OTHER", lost);
	if (n) {
		report("injection to handler", to_irq, n);
		report("injection to user space", to_wake, n);
	}
	close(fd);
	printf("%s: %d failed\n", node, failures);
	return failures ? 1 : 0;
}
