// SPDX-License-Identifier: GPL-2.0
/*
 * Exercises the generic lemnos device on a bound block: info, regions, a DMA
 * buffer (written through its dma-buf mapping and imported back), an import
 * of a dma-buf from /dev/dma_heap, the interrupt mask and read, and the
 * register-mapping policy. Leaves the block's registers untouched.
 *
 * Usage: lemnos_check /dev/lemnos-<device> [heap]
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

static int failures;

#define CHECK(cond, ...)                                                   \
	do {                                                               \
		if (cond) {                                                \
			printf("ok   ");                                       \
		} else {                                                   \
			printf("FAIL ");                                       \
			failures++;                                        \
		}                                                          \
		printf(__VA_ARGS__);                                       \
		printf("\n");                                              \
	} while (0)

int main(int argc, char **argv)
{
	const char *node = argc > 1 ? argv[1] : "/dev/lemnos";
	const char *heap = argc > 2 ? argv[2] : "/dev/dma_heap/linux,cma";
	struct lemnos_info info = { 0 };
	int fd = open(node, O_RDWR | O_CLOEXEC);

	if (fd < 0) {
		perror(node);
		return 2;
	}
	CHECK(open(node, O_RDWR) < 0 && errno == EBUSY, "a second open is refused (EBUSY)");
	CHECK(!ioctl(fd, LEMNOS_IOC_INFO, &info) && info.abi == LEMNOS_ABI,
	      "info: abi %u, %u regions, %u irqs, %u-bit DMA, registers %smappable", info.abi,
	      info.regions, info.irqs, info.dma_bits,
	      info.flags & LEMNOS_INFO_MMIO_MAPPABLE ? "" : "not ");

	for (uint32_t i = 0; i < info.regions; i++) {
		struct lemnos_region r = { .index = i };

		CHECK(!ioctl(fd, LEMNOS_IOC_REGION, &r) && r.size, "region %u: 0x%llx, %llu bytes", i,
		      (unsigned long long)r.start, (unsigned long long)r.size);
	}
	struct lemnos_region past = { .index = info.regions };
	CHECK(ioctl(fd, LEMNOS_IOC_REGION, &past) < 0 && errno == EINVAL, "region past the end: EINVAL");

	long page = sysconf(_SC_PAGESIZE);
	void *regs = mmap(NULL, page, PROT_READ, MAP_SHARED, fd, 0);
	if (info.flags & LEMNOS_INFO_MMIO_MAPPABLE)
		CHECK(regs != MAP_FAILED, "registers mapped");
	else
		CHECK(regs == MAP_FAILED && errno == EPERM, "registers not mapped (EPERM)");

	/* A buffer: written by the CPU through the dma-buf, imported back to the same address. */
	struct lemnos_alloc a = { .size = 1 << 20 };
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
	struct lemnos_import im = { .fd = a.fd };
	/* Behind an IOMMU the import is a second mapping: another address, same size. */
	CHECK(!ioctl(fd, LEMNOS_IOC_IMPORT, &im) && im.handle && im.size == a.size &&
		      im.addr + im.size <= (info.dma_bits >= 64 ? UINT64_MAX : 1ull << info.dma_bits),
	      "import of its own buffer: handle %u at 0x%llx (%s), %llu bytes", im.handle,
	      (unsigned long long)im.addr, im.addr == a.addr ? "same address" : "a second mapping",
	      (unsigned long long)im.size);
	CHECK(!ioctl(fd, LEMNOS_IOC_RELEASE, &im.handle), "release");
	CHECK(ioctl(fd, LEMNOS_IOC_RELEASE, &im.handle) < 0 && errno == ENOENT,
	      "release again: ENOENT");

	/* A dma-buf from a heap, left to the close to release. */
	int h = open(heap, O_RDONLY | O_CLOEXEC);
	if (h >= 0) {
		struct dma_heap_allocation_data d = {
			.len = 4 << 20, .fd_flags = O_RDWR | O_CLOEXEC,
		};
		CHECK(!ioctl(h, DMA_HEAP_IOCTL_ALLOC, &d), "heap %s: 4 MiB", heap);
		struct lemnos_import hi = { .fd = (int)d.fd };
		CHECK(!ioctl(fd, LEMNOS_IOC_IMPORT, &hi) && hi.size >= d.len,
		      "import of the heap buffer: 0x%llx, %llu bytes (held until close)",
		      (unsigned long long)hi.addr, (unsigned long long)hi.size);
		close((int)d.fd);
		close(h);
	} else {
		printf("skip no %s\n", heap);
	}

	/* Interrupts start masked: nothing to read, and enabling past the end is refused. */
	struct lemnos_irq_event ev;
	struct pollfd p = { .fd = fd, .events = POLLIN };
	CHECK(poll(&p, 1, 100) == 0, "no interrupt pending (poll 100 ms)");
	if (info.irqs) {
		uint32_t zero = 0;

		CHECK(!ioctl(fd, LEMNOS_IOC_IRQ_ENABLE, &zero), "enable irq 0 (an idle block raises none)");
		CHECK(poll(&p, 1, 100) == 0, "still none after enabling (poll 100 ms)");
	}
	uint32_t past_irq = info.irqs;
	CHECK(ioctl(fd, LEMNOS_IOC_IRQ_ENABLE, &past_irq) < 0 && errno == EINVAL,
	      "enable irq %u: EINVAL", past_irq);
	fcntl(fd, F_SETFL, O_NONBLOCK);
	CHECK(read(fd, &ev, sizeof(ev)) < 0 && errno == EAGAIN, "read with none pending: EAGAIN");

	close(a.fd);
	close(fd);
	fd = open(node, O_RDWR | O_CLOEXEC);
	CHECK(fd >= 0, "reopened after close (imports released)");
	close(fd);
	printf("%s: %d failed\n", node, failures);
	return failures ? 1 : 0;
}
