// SPDX-License-Identifier: GPL-2.0
/*
 * Exercises a block on the control node: info, regions, a DMA buffer (written
 * through its dma-buf mapping and imported back), an import of a dma-buf from
 * a heap, the interrupt mask and read, and the register-mapping policy.
 * Leaves the block's registers untouched.
 *
 * Usage: lemnos_check /dev/lemnos <device name> [heap]
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <errno.h>
#include <fcntl.h>
#include <linux/dma-heap.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

#include "../lemnos.h"
#include "find_block.h"

static int failures;

#define CHECK(cond, ...)                                                   \
	do {                                                               \
		if (cond) {                                                \
			printf("ok   ");                                   \
		} else {                                                   \
			printf("FAIL ");                                   \
			failures++;                                        \
		}                                                          \
		printf(__VA_ARGS__);                                       \
		printf("\n");                                              \
	} while (0)

/* Whether the module was loaded with test_irq_inject. */
static int injecting(void)
{
	char c = 'N';
	int f = open("/sys/module/lemnos/parameters/test_irq_inject", O_RDONLY);

	if (f >= 0) {
		if (read(f, &c, 1) != 1)
			c = 'N';
		close(f);
	}
	return c == 'Y';
}

int main(int argc, char **argv)
{
	const char *node = argc > 1 ? argv[1] : "/dev/lemnos";
	const char *name = argc > 2 ? argv[2] : "";
	const char *heap = argc > 3 ? argv[3] : "/dev/dma_heap/linux,cma";
	int fd = open(node, O_RDWR | O_CLOEXEC);

	setvbuf(stdout, NULL, _IOLBF, 0);
	if (fd < 0) {
		perror(node);
		return 2;
	}
	__u32 block = find_block(fd, name);
	struct lemnos_info info = { .block = block };

	CHECK(block, "block %s is number %u", name, block);
	CHECK(open(node, O_RDWR) < 0 && errno == EBUSY, "a second open is refused (EBUSY)");
	struct lemnos_info gone = { .block = block + 1000 };
	CHECK(ioctl(fd, LEMNOS_IOC_INFO, &gone) < 0 && errno == ENODEV, "a block not bound: ENODEV");
	CHECK(!ioctl(fd, LEMNOS_IOC_INFO, &info) && info.abi == LEMNOS_ABI,
	      "info: abi %u, %u regions, %u irqs, %u-bit DMA, registers %smappable", info.abi,
	      info.regions, info.irqs, info.dma_bits,
	      info.flags & LEMNOS_INFO_MMIO_MAPPABLE ? "" : "not ");

	struct lemnos_region first = { .block = block };

	for (uint32_t i = 0; i < info.regions; i++) {
		struct lemnos_region r = { .block = block, .index = i };

		CHECK(!ioctl(fd, LEMNOS_IOC_REGION, &r) && r.size, "region %u: 0x%llx, %llu bytes", i,
		      (unsigned long long)r.start, (unsigned long long)r.size);
		if (!i)
			first = r;
	}
	struct lemnos_region past = { .block = block, .index = info.regions };
	CHECK(ioctl(fd, LEMNOS_IOC_REGION, &past) < 0 && errno == EINVAL, "region past the end: EINVAL");

	long page = sysconf(_SC_PAGESIZE);
	void *regs = mmap(NULL, page, PROT_READ, MAP_SHARED, fd, first.offset);

	if (info.flags & LEMNOS_INFO_MMIO_MAPPABLE)
		CHECK(regs != MAP_FAILED, "registers mapped");
	else
		CHECK(regs == MAP_FAILED && errno == EPERM, "registers not mapped (EPERM)");
	CHECK(!(info.flags & LEMNOS_INFO_MMIO_UNCONFINED) || (info.flags & LEMNOS_INFO_MMIO_MAPPABLE),
	      "unconfined only with mappable: flags 0x%x", info.flags);

	/* A buffer: written by the CPU through the dma-buf, imported back to the same address. */
	struct lemnos_alloc a = { .block = block, .size = 1 << 20 };

	CHECK(!ioctl(fd, LEMNOS_IOC_ALLOC, &a) && a.fd >= 0,
	      "alloc 1 MiB: dma-buf fd %d at device address 0x%llx", a.fd, (unsigned long long)a.addr);
	CHECK(a.addr + a.size <= (info.dma_bits >= 64 ? UINT64_MAX : 1ull << info.dma_bits),
	      "address below 2^%u", info.dma_bits);
	uint8_t *cpu = mmap(NULL, a.size, PROT_READ | PROT_WRITE, MAP_SHARED, a.fd, 0);

	CHECK(cpu != MAP_FAILED, "dma-buf mapped for the CPU");
	if (cpu != MAP_FAILED) {
		for (size_t i = 0; i < a.size; i++)
			cpu[i] = (uint8_t)(i * 7);
		size_t bad = 0;

		for (size_t i = 0; i < a.size; i++)
			bad += cpu[i] != (uint8_t)(i * 7);
		CHECK(!bad, "pattern read back: %zu bytes differ", bad);
		munmap(cpu, a.size);
	}
	struct lemnos_import im = { .block = block, .fd = a.fd };

	/* Behind an IOMMU the import is a second mapping: another address, same size. */
	CHECK(!ioctl(fd, LEMNOS_IOC_IMPORT, &im) && im.handle && im.size == a.size &&
		      im.addr + im.size <= (info.dma_bits >= 64 ? UINT64_MAX : 1ull << info.dma_bits),
	      "import of its own buffer: handle %u at 0x%llx (%s), %llu bytes", im.handle,
	      (unsigned long long)im.addr, im.addr == a.addr ? "same address" : "a second mapping",
	      (unsigned long long)im.size);
	struct lemnos_release rel = { .block = block, .handle = im.handle };

	CHECK(!ioctl(fd, LEMNOS_IOC_RELEASE, &rel), "release");
	CHECK(ioctl(fd, LEMNOS_IOC_RELEASE, &rel) < 0 && errno == ENOENT, "release again: ENOENT");

	/* A dma-buf from a heap, left to the close to release. */
	int h = open(heap, O_RDONLY | O_CLOEXEC);

	if (h >= 0) {
		struct dma_heap_allocation_data d = {
			.len = 4 << 20, .fd_flags = O_RDWR | O_CLOEXEC,
		};
		CHECK(!ioctl(h, DMA_HEAP_IOCTL_ALLOC, &d), "heap %s: 4 MiB", heap);
		struct lemnos_import hi = { .block = block, .fd = (int)d.fd };

		CHECK(!ioctl(fd, LEMNOS_IOC_IMPORT, &hi) && hi.size >= d.len,
		      "import of the heap buffer: 0x%llx, %llu bytes (held until close)",
		      (unsigned long long)hi.addr, (unsigned long long)hi.size);
		close((int)d.fd);
		close(h);
	} else {
		printf("skip no %s\n", heap);
	}

	/* Interrupts start masked: nothing to read, and enabling past the end is refused. */
	struct lemnos_event ev;
	struct pollfd p = { .fd = fd, .events = POLLIN };

	CHECK(poll(&p, 1, 100) == 0, "no interrupt pending (poll 100 ms)");
	struct lemnos_irq zero = { .block = block };

	if (info.irqs) {
		CHECK(!ioctl(fd, LEMNOS_IOC_IRQ_ENABLE, &zero), "enable irq 0 (an idle block raises none)");
		CHECK(poll(&p, 1, 100) == 0, "still none after enabling (poll 100 ms)");
	}
	struct lemnos_irq past_irq = { .block = block, .index = info.irqs };

	CHECK(ioctl(fd, LEMNOS_IOC_IRQ_ENABLE, &past_irq) < 0 && errno == EINVAL,
	      "enable irq %u: EINVAL", past_irq.index);
	/* lemnos_irq_check covers injection; a board run may load the module with it on. */
	if (!injecting())
		CHECK(ioctl(fd, LEMNOS_IOC_TEST_IRQ, &zero) < 0 && errno == EPERM,
		      "test interrupt without test_irq_inject: EPERM");
	fcntl(fd, F_SETFL, O_NONBLOCK);
	CHECK(read(fd, &ev, sizeof(ev)) < 0 && errno == EAGAIN, "read with none pending: EAGAIN");
	CHECK(read(fd, &ev, sizeof(ev) - 1) < 0 && errno == EINVAL, "read too short: EINVAL");

	/* A mapping holds the file open as a descriptor does: let go of both before reopening. */
	if (regs != MAP_FAILED)
		munmap(regs, page);
	if (cpu != MAP_FAILED)
		munmap(cpu, a.size);
	close(a.fd);
	close(fd);
	fd = open(node, O_RDWR | O_CLOEXEC);
	CHECK(fd >= 0, "reopened after close (imports released)");
	close(fd);
	printf("%s: %d failed\n", node, failures);
	return failures ? 1 : 0;
}
