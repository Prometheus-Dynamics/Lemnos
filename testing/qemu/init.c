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
#include <sys/stat.h>
#include <sys/reboot.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#define BLOCK "lemnos-test-block"
#define OTHER "lemnos-test-other"
#define SPI "spi-lemnos-test"
#define PISP "lemnos-test-pisp"
#define BIND "/sys/bus/platform/drivers/lemnos/bind"
#define UNBIND "/sys/bus/platform/drivers/lemnos/unbind"
#define OWNED "owned=lemnos,test-block,lemnos,test-spi"
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

	if (fd < 0) {
		printf("open %s: %s\n", path, strerror(errno));
		return errno;
	}
	if (write(fd, text, strlen(text)) < 0)
		e = errno;
	close(fd);
	if (e && e != ENODEV)
		printf("write %s: %s\n", path, strerror(e));
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

#define SIM "/sys/kernel/config/gpio-sim/lemnos"

static char gpio_chip[16], gpio_dir[128];

/* The contents of a one-line file, without the newline. */
static int get_text(const char *path, char *out, size_t size)
{
	int fd = open(path, O_RDONLY | O_CLOEXEC);
	ssize_t n = fd < 0 ? -1 : read(fd, out, size - 1);

	if (fd >= 0)
		close(fd);
	if (n <= 0)
		return -1;
	out[n] = 0;
	out[strcspn(out, "\n")] = 0;
	return 0;
}

/* An eight-line simulated chip, through configfs. */
static int gpio_sim_up(void)
{
	char dev[64], chip[32];

	if (mkdir("/sys/kernel/config", 0755) && errno != EEXIST && errno != EROFS)
		return -1;
	if (mount("configfs", "/sys/kernel/config", "configfs", 0, NULL) && errno != EBUSY)
		return -1;
	if (!access("/gpio-sim.ko", R_OK) && load("/gpio-sim.ko", ""))
		return -1;
	if (mkdir(SIM, 0755) || mkdir(SIM "/bank0", 0755) || put(SIM "/bank0/num_lines", "8") ||
	    put(SIM "/live", "1"))
		return -1;
	if (get_text(SIM "/dev_name", dev, sizeof(dev)) ||
	    get_text(SIM "/bank0/chip_name", chip, sizeof(chip)))
		return -1;
	snprintf(gpio_chip, sizeof(gpio_chip), "%s", chip + strlen("gpiochip"));
	snprintf(gpio_dir, sizeof(gpio_dir), "/sys/devices/platform/%s/%s", dev, chip);
	return 0;
}

static void gpio_sim_down(void)
{
	put(SIM "/live", "0");
	rmdir(SIM "/bank0");
	rmdir(SIM);
	if (!access("/gpio-sim.ko", R_OK))
		unload("gpio_sim");
}

static int spi_bind(const char *driver)
{
	char path[96];

	snprintf(path, sizeof(path), "/sys/bus/spi/drivers/%s/bind", driver);
	return put("/sys/bus/spi/devices/" SPI "/driver_override", driver) || put(path, SPI);
}

static int spi_unbind(const char *driver)
{
	char path[96];

	snprintf(path, sizeof(path), "/sys/bus/spi/drivers/%s/unbind", driver);
	return put(path, SPI) || put("/sys/bus/spi/devices/" SPI "/driver_override", "\n");
}

/* route is how I2C, GPIO and PWM should go; spi_route is how SPI should. */
static int backend_check(const char *route, const char *spi_route)
{
	return run((char *const[]){ "/lemnos-module-check", (char *)route, (char *)spi_route,
				    gpio_chip, gpio_dir, NULL });
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

	check(!load("/rp1_pio_mock.ko", ""), "stand-in PIO driver loaded");
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

	check(!load("/i2c-stub.ko", "chip_addr=0x30"), "i2c-stub loaded");
	check(!load("/lemnos_test_bus.ko", ""), "test buses loaded");
	check(!load("/spi-loopback-test.ko", "loopback=1"), "spi-loopback-test loaded");
	check(!access("/sys/bus/spi/devices/spi40.1/driver", F_OK),
	      "spi-loopback-test passed on the test controller");
	check(!put("/sys/bus/spi/devices/" SPI "/driver_override", "lemnos") &&
		      !put("/sys/bus/spi/drivers/lemnos/bind", SPI),
	      "SPI device bound");
	check(!run((char *const[]){ "/lemnos_bus_check", "/dev/lemnos", NULL }),
	      "lemnos_bus_check");
	check(!put("/sys/bus/spi/drivers/lemnos/unbind", SPI), "SPI device unbound");
	check(!put("/sys/bus/spi/devices/" SPI "/driver_override", "\n"), "override cleared");

	check(!gpio_sim_up(), "simulated GPIO chip up");
	check(!run((char *const[]){ "/lemnos_gpio_check", "/dev/lemnos", gpio_chip, gpio_dir,
				    "lemnos-test-parent", NULL }),
	      "lemnos_gpio_check");

	check(!run((char *const[]){ "/lemnos_power_check", "/dev/lemnos", BLOCK, BIND, UNBIND,
				    NULL }),
	      "lemnos_power_check");

	check(!run((char *const[]){ "/lemnos_pio_check", "/dev/lemnos", NULL }),
	      "lemnos_pio_check");

	/* One binary, the real backend: the module where it serves a bus, stock elsewhere. */
	check(!spi_bind("lemnos"), "SPI device bound to the module");
	check(!backend_check("module", "module"), "backend: everything through the module");
	check(!spi_unbind("lemnos"), "SPI device unbound from the module");
	if (!access("/spidev.ko", R_OK))
		check(!load("/spidev.ko", ""), "spidev loaded");
	check(!spi_bind("spidev"), "SPI device bound to spidev");
	check(!backend_check("module", "stock"), "backend: SPI alone stays stock");
	check(!forget_override(BLOCK) && !forget_override(OTHER), "overrides cleared");
	check(!unload("lemnos"), "module unloaded with nothing left held");
	check(!backend_check("stock", "stock"), "backend: no module, everything stock");
	check(!spi_unbind("spidev"), "SPI device unbound from spidev");
	if (!access("/spidev.ko", R_OK))
		check(!unload("spidev"), "spidev unloaded");
	gpio_sim_down();
	check(!unload("spi_loopback_test"), "spi-loopback-test unloaded");
	check(!unload("lemnos_test_bus"), "test buses unloaded");
	check(!unload("i2c_stub"), "i2c-stub unloaded");

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

	check(!load("/lemnos.ko", OWNED_CSI2 " test_pisp_fe=1"),
	      "module loaded with the front end test paths");
	check(!bind_block(BLOCK), "test block bound");
	check(!bind_csi2(), "receiver with front end bound");
	check(!run((char *const[]){ "/lemnos_pisp_fe_check", "/dev/lemnos", CSI2, CSI2_UNBIND,
				    (char *)cma_heap(), NULL }),
	      "lemnos_pisp_fe_check");
	check(!put(UNBIND, BLOCK), "test block unbound");
	check(!forget_override(BLOCK) && !forget_override(CSI2), "overrides cleared");
	check(!unload("lemnos"), "module unloaded after the front end checks");

	check(!load("/lemnos.ko", OWNED_CSI2 " test_pisp_fe=1"),
	      "module loaded for the front end's stuck-DMA check");
	check(!bind_csi2(), "receiver bound again");
	check(!run((char *const[]){ "/lemnos_pisp_fe_check", "wedge", CSI2, CSI2_UNBIND,
				    (char *)cma_heap(), NULL }),
	      "lemnos_pisp_fe_check wedge");
	check(!forget_override(CSI2), "override cleared");
	check(!unload("lemnos"), "module unloaded with a wedged front end stream pinned");
	check(!unload("lemnos_test_block"), "test block module unloaded");

	check(!load("/lemnos_test_block.ko", ""), "test block module loaded for the PiSP back end");
	/* The PiSP back end on its fake block. */
	check(!load("/lemnos.ko", OWNED ",lemnos,test-pisp-be pisp_be=lemnos,test-pisp-be test_pb_fake_regs=1"),
	      "module loaded with the PiSP back end");
	check(!bind_block(PISP), "PiSP block bound");
	check(!run((char *const[]){ "/lemnos_pisp_be_check", "/dev/lemnos", PISP, BIND, UNBIND,
				    (char *)cma_heap(), "refuse", NULL }),
	      "lemnos_pisp_be_check refuse");
	check(!run((char *const[]){ "/lemnos_pisp_be_check", "/dev/lemnos", PISP, BIND, UNBIND,
				    (char *)cma_heap(), "lifetime", NULL }),
	      "lemnos_pisp_be_check lifetime");
	check(!put(UNBIND, PISP), "PiSP block unbound");
	check(!forget_override(PISP), "PiSP override cleared");
	check(!unload("lemnos"), "module unloaded with the back end idle");

	check(!load("/lemnos.ko", OWNED ",lemnos,test-pisp-be pisp_be=lemnos,test-pisp-be test_pb_fake_regs=1"),
	      "module loaded for the wedge checks");
	check(!bind_block(PISP), "PiSP block bound");
	check(!run((char *const[]){ "/lemnos_pisp_be_check", "/dev/lemnos", PISP, BIND, UNBIND,
				    (char *)cma_heap(), "wedge", NULL }),
	      "lemnos_pisp_be_check wedge");
	check(unload("lemnos") && errno == EAGAIN, "a wedged block keeps the module loaded");
	/* The test block's devices go with it; a wedged block's memory stays pinned regardless. */
	check(!put(UNBIND, PISP), "wedged PiSP block unbound");
	/* The stand-in PIO driver stays: the wedged module above depends on it. */

	printf("lemnos-qemu: %s (%d failed)\n", failures ? "FAILED" : "passed", failures);
	sync();
	reboot(RB_POWER_OFF);
	return 0;
}
