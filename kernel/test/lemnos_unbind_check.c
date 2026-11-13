// SPDX-License-Identifier: GPL-2.0
/*
 * A block unbound while the control node is open: the unbind waits for the
 * DMA buffers still held, register mappings fault afterwards, and requests
 * for the block fail. The module must be loaded with the block in unconfined=.
 *
 * Usage: lemnos_unbind_check /dev/lemnos <device name> <unbind path>
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

#include "../lemnos.h"
#include "find_block.h"

static int failures;
static sigjmp_buf fault;

#define CHECK(cond, ...)                                                   \
	do {                                                               \
		int ok_ = !!(cond);                                        \
		printf(ok_ ? "ok   " : "FAIL ");                           \
		failures += !ok_;                                          \
		printf(__VA_ARGS__);                                       \
		printf("\n");                                              \
	} while (0)

static void on_fault(int sig)
{
	siglongjmp(fault, sig);
}

int main(int argc, char **argv)
{
	if (argc < 4) {
		fprintf(stderr, "usage: lemnos_unbind_check /dev/lemnos <device name> <unbind path>\n");
		return 2;
	}
	const char *name = argv[2];
	int fd = open(argv[1], O_RDWR | O_CLOEXEC | O_NONBLOCK);

	setvbuf(stdout, NULL, _IONBF, 0);
	if (fd < 0) {
		perror(argv[1]);
		return 2;
	}
	long page = sysconf(_SC_PAGESIZE);
	__u32 block = find_block(fd, name);
	struct lemnos_region r = { .block = block };

	CHECK(block && !ioctl(fd, LEMNOS_IOC_REGION, &r), "block %u, region 0", block);
	volatile uint32_t *regs = mmap(NULL, page, PROT_READ, MAP_SHARED, fd, r.offset);

	CHECK(regs != MAP_FAILED && regs[0], "registers mapped and readable");
	CHECK(mmap(NULL, page, PROT_READ, MAP_PRIVATE, fd, r.offset) == MAP_FAILED && errno == EINVAL,
	      "a private mapping is refused (EINVAL)");

	struct lemnos_alloc a = { .block = block, .size = 1 << 16 };

	CHECK(!ioctl(fd, LEMNOS_IOC_ALLOC, &a), "alloc a buffer");
	struct lemnos_import im = { .block = block, .fd = a.fd };

	CHECK(!ioctl(fd, LEMNOS_IOC_IMPORT, &im), "import it");
	struct lemnos_irq zero = { .block = block };

	CHECK(!ioctl(fd, LEMNOS_IOC_IRQ_ENABLE, &zero), "enable irq 0");

	pid_t child = fork();

	if (!child) {
		/* The copy this process inherited would hold the buffer too. */
		close(a.fd);
		int u = open(argv[3], O_WRONLY);

		_exit(u >= 0 && write(u, name, strlen(name)) == (ssize_t)strlen(name) ? 0 : 1);
	}
	int status = 0;

	poll(NULL, 0, 500);
	CHECK(waitpid(child, &status, WNOHANG) == 0, "the unbind waits while the buffer is held");

	struct lemnos_info info = { .block = block };

	CHECK(ioctl(fd, LEMNOS_IOC_INFO, &info) < 0 && errno == ENODEV, "requests for it: ENODEV");
	struct sigaction sa = { .sa_handler = on_fault };

	sigaction(SIGBUS, &sa, NULL);
	sigaction(SIGSEGV, &sa, NULL);
	int sig = sigsetjmp(fault, 1);

	if (!sig) {
		uint32_t v = regs[0];

		CHECK(0, "the register mapping still reads 0x%08x", v);
	} else {
		CHECK(sig == SIGBUS, "the register mapping faults (signal %d)", sig);
	}

	close(a.fd);
	CHECK(waitpid(child, &status, 0) == child && WIFEXITED(status) && !WEXITSTATUS(status),
	      "closing the buffer ends the unbind");
	struct lemnos_next next = { 0 };
	int more = 0;

	while (!ioctl(fd, LEMNOS_IOC_NEXT, &next)) {
		more += !strcmp(next.name, name);
		next.after = next.block;
	}
	CHECK(!more, "the block is gone from the list");
	close(fd);
	printf("%s: %d failed\n", argv[1], failures);
	return failures ? 1 : 0;
}
