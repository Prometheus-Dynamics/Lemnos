// SPDX-License-Identifier: GPL-2.0
/*
 * The guest's first process: loads the module, binds the test block, runs the
 * checks, prints one result line and powers off. Static, so the image needs
 * nothing else.
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/reboot.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#define BLOCK "lemnos-test-block"
#define OTHER "lemnos-test-other"
#define BIND "/sys/bus/platform/drivers/lemnos/bind"
#define UNBIND "/sys/bus/platform/drivers/lemnos/unbind"
#define OWNED "owned=lemnos,test-block"
#define CSI2 "lemnos-test-csi2"
#define CSI2_DRIVER "lemnos-rp1-csi2"
#define CSI2_BIND "/sys/bus/platform/drivers/" CSI2_DRIVER "/bind"
#define CSI2_UNBIND "/sys/bus/platform/drivers/" CSI2_DRIVER "/unbind"
#define OWNED_CSI2 "owned=lemnos,test-block,lemnos,test-csi2"
/* The fake receiver's registers: guest memory the kernel left alone (memmap=). */
#define CSI2_PHYS "csi2_phys=0x20000000"

static int failures;

static void check(int ok, const char *what)
{
	printf("%s %s\n", ok ? "ok  " : "FAIL", what);
	failures += !ok;
}

static int load(const char *path, const char *params)
{
	int fd = open(path, O_RDONLY | O_CLOEXEC);
	int r = fd < 0 ? -1 : (int)syscall(SYS_finit_module, fd, params, 0);

	if (r)
		printf("load %s: %s\n", path, strerror(errno));
	if (fd >= 0)
		close(fd);
	return r;
}

static int unload(const char *name)
{
	int r = (int)syscall(SYS_delete_module, name, O_NONBLOCK);

	if (r)
		printf("unload %s: %s\n", name, strerror(errno));
	return r;
}

/* 0, or the errno of the failed open or write. */
static int put(const char *path, const char *text)
{
	int fd = open(path, O_WRONLY | O_CLOEXEC);
	int e = 0;

	if (fd < 0)
		return errno;
	if (write(fd, text, strlen(text)) < 0)
		e = errno;
	close(fd);
	return e;
}

static int bind_csi2(void)
{
	char path[128];

	snprintf(path, sizeof(path), "/sys/bus/platform/devices/%s/driver_override", CSI2);
	if (put(path, CSI2_DRIVER))
		return -1;
	return put(CSI2_BIND, CSI2);
}

static int bind_block(const char *dev)
{
	char path[128];

	snprintf(path, sizeof(path), "/sys/bus/platform/devices/%s/driver_override", dev);
	if (put(path, "lemnos"))
		return -1;
	return put(BIND, dev);
}

/* A device keeps its override after an unbind, and would bind on the next load. */
static int forget_override(const char *dev)
{
	char path[128];

	snprintf(path, sizeof(path), "/sys/bus/platform/devices/%s/driver_override", dev);
	return put(path, "\n");
}

static int run(char *const argv[])
{
	int status = -1;
	pid_t p = fork();

	if (!p) {
		execv(argv[0], argv);
		_exit(127);
	}
	if (p < 0 || waitpid(p, &status, 0) != p)
		return -1;
	return WIFEXITED(status) ? WEXITSTATUS(status) : 128;
}

static const char *cma_heap(void)
{
	static const char *const heaps[] = { "/dev/dma_heap/default_cma_region",
					     "/dev/dma_heap/linux,cma", "/dev/dma_heap/reserved" };

	for (size_t i = 0; i < sizeof(heaps) / sizeof(heaps[0]); i++)
		if (!access(heaps[i], R_OK))
			return heaps[i];
	return heaps[1];
}

int main(void)
{
	mount("devtmpfs", "/dev", "devtmpfs", 0, NULL);
	mount("sysfs", "/sys", "sysfs", 0, NULL);
	mount("proc", "/proc", "proc", 0, NULL);
	mount("debugfs", "/sys/kernel/debug", "debugfs", 0, NULL);
	int con = open("/dev/console", O_RDWR);

	if (con >= 0) {
		dup2(con, 0);
		dup2(con, 1);
		dup2(con, 2);
	}
	setvbuf(stdout, NULL, _IONBF, 0);
	printf("lemnos-qemu: start\n");

	check(!load("/lemnos_test_block.ko", CSI2_PHYS), "test block module loaded");
	check(!load("/lemnos.ko", OWNED), "module loaded");
	check(!access("/dev/lemnos", R_OK), "control node exists");
	check(bind_block(OTHER) == ENODEV, "a block not listed is refused (ENODEV)");
	check(bind_csi2() == ENODEV, "the receiver not in owned= is refused (ENODEV)");
	check(!forget_override(CSI2), "receiver override cleared");
	check(!bind_block(BLOCK), "block bound");
	check(!run((char *const[]){ "/lemnos_check", "/dev/lemnos", BLOCK, (char *)cma_heap(),
				    NULL }),
	      "lemnos_check");
	check(!run((char *const[]){ "/lemnos_stream_check", "/dev/lemnos", BLOCK, UNBIND,
				    (char *)cma_heap(), "no-source", NULL }),
	      "lemnos_stream_check, no test source");
	check(!put(UNBIND, BLOCK), "unbound");
	check(!forget_override(BLOCK) && !forget_override(OTHER), "overrides cleared");
	check(!unload("lemnos"), "module unloaded");

	check(!load("/lemnos.ko", OWNED " test_map_unconfined=1 test_irq_inject=1"),
	      "module loaded with the test paths");
	check(!bind_block(BLOCK), "block bound");
	check(!run((char *const[]){ "/lemnos_irq_check", "/dev/lemnos", BLOCK, "2000", NULL }),
	      "lemnos_irq_check");
	check(!run((char *const[]){ "/lemnos_unbind_check", "/dev/lemnos", BLOCK, UNBIND, NULL }),
	      "lemnos_unbind_check");
	check(!run((char *const[]){ "/lemnos_claim_check", "/dev/lemnos", NULL }),
	      "lemnos_claim_check");
	check(!forget_override(BLOCK) && !forget_override(OTHER), "overrides cleared");
	check(!unload("lemnos"), "module unloaded with nothing left held");

	check(!load("/lemnos.ko", OWNED " test_stream_source=1"),
	      "module loaded with the test stream source");
	check(!bind_block(BLOCK), "block bound");
	check(!run((char *const[]){ "/lemnos_stream_check", "/dev/lemnos", BLOCK, UNBIND,
				    (char *)cma_heap(), NULL }),
	      "lemnos_stream_check");
	check(!forget_override(BLOCK), "override cleared");
	check(!unload("lemnos"), "module unloaded with every stream released");

	check(!load("/lemnos.ko", OWNED_CSI2), "module loaded owning the receiver");
	check(!bind_block(BLOCK), "test block bound");
	check(!bind_csi2(), "receiver bound");
	check(!run((char *const[]){ "/lemnos_rp1_csi2_check", "/dev/lemnos", BLOCK, CSI2, CSI2_UNBIND,
				    (char *)cma_heap(), NULL }),
	      "lemnos_rp1_csi2_check");
	check(!put(UNBIND, BLOCK), "test block unbound");
	check(!forget_override(BLOCK) && !forget_override(CSI2), "overrides cleared");
	check(!unload("lemnos"), "module unloaded after the receiver checks");

	check(!load("/lemnos.ko", OWNED_CSI2), "module loaded for the stuck-DMA check");
	check(!bind_csi2(), "receiver bound again");
	check(!run((char *const[]){ "/lemnos_rp1_csi2_check", "wedge", CSI2, CSI2_UNBIND,
				    (char *)cma_heap(), NULL }),
	      "lemnos_rp1_csi2_check wedge");
	check(!forget_override(CSI2), "override cleared");
	check(!unload("lemnos"), "module unloaded with a wedged stream pinned");
	check(!unload("lemnos_test_block"), "test block module unloaded");

	printf("lemnos-qemu: %s (%d failed)\n", failures ? "FAILED" : "passed", failures);
	sync();
	reboot(RB_POWER_OFF);
	return 0;
}
