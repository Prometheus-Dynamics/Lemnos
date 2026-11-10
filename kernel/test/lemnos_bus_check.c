// SPDX-License-Identifier: GPL-2.0
/*
 * Transfers through the kernel's I2C and SPI cores. Expects i2c-stub loaded
 * with a chip at 0x30, and the test buses at 40 with SPI device 40.0 bound.
 *
 * Usage: lemnos_bus_check /dev/lemnos
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <errno.h>
#include <fcntl.h>
#include <linux/i2c.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

#include "../lemnos.h"

#define TEST_BUS 40
#define STUB_ADDR 0x30
#define MEM_ADDR 0x50
#define HELD_ADDR 0x51
#define BUFFER 4096

static int failures;

#define CHECK(cond, ...)                                                   \
	do {                                                               \
		int ok_ = !!(cond);                                        \
		printf(ok_ ? "ok   " : "FAIL ");                           \
		failures += !ok_;                                          \
		printf(__VA_ARGS__);                                       \
		printf("\n");                                              \
	} while (0)

static int fd;

static int claim(const char *name)
{
	struct lemnos_claim c = { 0 };

	strncpy(c.resource, name, sizeof(c.resource) - 1);
	return ioctl(fd, LEMNOS_IOC_CLAIM, &c) ? -errno : c.fd;
}

static int claim_i2c(int bus, int addr)
{
	char name[LEMNOS_NAME_MAX];

	snprintf(name, sizeof(name), "i2c-%d:0x%x", bus, addr);
	return claim(name);
}

/* 0 or -errno. */
static int i2c(int c, uint32_t bus, uint16_t addr, struct lemnos_i2c_segment *s, uint32_t n)
{
	struct lemnos_i2c_transfer t = {
		.claim = c, .bus = bus, .addr = addr, .count = n,
		.segments = (uintptr_t)s,
	};

	return ioctl(fd, LEMNOS_IOC_I2C_TRANSFER, &t) ? -errno : 0;
}

static int smbus(struct lemnos_i2c_smbus *s)
{
	return ioctl(fd, LEMNOS_IOC_I2C_SMBUS, s) ? -errno : 0;
}

static int spi(int c, uint32_t cs, uint32_t mode, struct lemnos_spi_segment *s, uint32_t n)
{
	struct lemnos_spi_transfer t = {
		.claim = c, .bus = TEST_BUS, .cs = cs, .mode = mode, .count = n,
		.segments = (uintptr_t)s,
	};

	return ioctl(fd, LEMNOS_IOC_SPI_TRANSFER, &t) ? -errno : 0;
}

/* i2c-stub's adapter number, found by name; -1 if it isn't there. */
static int stub_bus(void)
{
	for (int n = 0; n < 64; n++) {
		char path[64], name[64] = { 0 };
		int f;

		snprintf(path, sizeof(path), "/sys/bus/i2c/devices/i2c-%d/name", n);
		f = open(path, O_RDONLY);
		if (f < 0)
			continue;
		(void)!read(f, name, sizeof(name) - 1);
		close(f);
		if (!strncmp(name, "SMBus stub", 10))
			return n;
	}
	return -1;
}

static void check_smbus(void)
{
	int bus = stub_bus();

	CHECK(bus >= 0, "i2c-stub is adapter %d", bus);
	int c = claim_i2c(bus, STUB_ADDR);
	struct lemnos_i2c_smbus s = {
		.claim = c, .bus = bus, .addr = STUB_ADDR, .read = 0, .command = 0x10,
		.protocol = I2C_SMBUS_BYTE_DATA, .data = { 0xa5 },
	};

	CHECK(c >= 0, "claimed the stub's chip");
	CHECK(!smbus(&s), "SMBus write byte");
	memset(s.data, 0, sizeof(s.data));
	s.read = 1;
	CHECK(!smbus(&s) && s.data[0] == 0xa5, "SMBus read byte: 0x%02x", s.data[0]);

	s = (struct lemnos_i2c_smbus){ .claim = c, .bus = bus, .addr = STUB_ADDR, .command = 0x20,
				       .protocol = I2C_SMBUS_WORD_DATA, .data = { 0x34, 0x12 } };
	CHECK(!smbus(&s), "SMBus write word");
	memset(s.data, 0, sizeof(s.data));
	s.read = 1;
	CHECK(!smbus(&s) && s.data[0] == 0x34 && s.data[1] == 0x12, "SMBus read word");

	s = (struct lemnos_i2c_smbus){ .claim = c, .bus = bus, .addr = STUB_ADDR, .command = 0x30,
				       .protocol = I2C_SMBUS_I2C_BLOCK_DATA,
				       .data = { 4, 1, 2, 3, 4 } };
	CHECK(!smbus(&s), "SMBus write block");
	memset(s.data, 0, sizeof(s.data));
	s.read = 1;
	s.data[0] = 4;
	CHECK(!smbus(&s) && s.data[0] == 4 && s.data[1] == 1 && s.data[4] == 4, "SMBus read block");

	s.data[0] = 33;
	CHECK(smbus(&s) == -EINVAL, "a block past 32 bytes: EINVAL");
	s = (struct lemnos_i2c_smbus){ .claim = c, .bus = bus, .addr = STUB_ADDR, .protocol = 99 };
	CHECK(smbus(&s) == -EINVAL, "an unknown protocol: EINVAL");

	uint8_t byte = 0;
	struct lemnos_i2c_segment g = { .buf = (uintptr_t)&byte, .len = 1 };

	CHECK(i2c(c, bus, STUB_ADDR, &g, 1) == -EOPNOTSUPP,
	      "plain transfer on an adapter without them: EOPNOTSUPP");
	close(c);
	s = (struct lemnos_i2c_smbus){ .claim = c, .bus = bus, .addr = STUB_ADDR, .read = 1,
				       .protocol = I2C_SMBUS_BYTE_DATA };
	CHECK(smbus(&s) == -EPERM, "after the claim closes: EPERM");
}

static void check_i2c(void)
{
	static uint8_t big[BUFFER + 2];
	int c = claim_i2c(TEST_BUS, MEM_ADDR);
	uint8_t reg_and_data[] = { 0x10, 0xde, 0xad, 0xbe, 0xef };
	uint8_t reg = 0x10, got[4] = { 0 };
	struct lemnos_i2c_segment write = { .buf = (uintptr_t)reg_and_data, .len = 5 };
	struct lemnos_i2c_segment write_read[2] = {
		{ .buf = (uintptr_t)&reg, .len = 1 },
		{ .buf = (uintptr_t)got, .len = 4, .flags = LEMNOS_I2C_READ },
	};
	CHECK(c >= 0, "claimed the target");
	CHECK(!i2c(c, TEST_BUS, MEM_ADDR, &write, 1), "write");
	CHECK(!i2c(c, TEST_BUS, MEM_ADDR, write_read, 2) && got[0] == 0xde && got[3] == 0xef,
	      "write then read in one transfer: %02x %02x %02x %02x", got[0], got[1], got[2], got[3]);

	int wrong = claim_i2c(TEST_BUS, 0x52);
	int other = claim("not-a-bus");
	int dupe = dup(c);
	int devnull = open("/dev/null", O_RDONLY);

	CHECK(i2c(-1, TEST_BUS, MEM_ADDR, &write, 1) == -EPERM, "no claim: EPERM");
	CHECK(i2c(devnull, TEST_BUS, MEM_ADDR, &write, 1) == -EPERM, "a file that isn't a claim: EPERM");
	CHECK(i2c(wrong, TEST_BUS, MEM_ADDR, &write, 1) == -EPERM, "a claim on another target: EPERM");
	CHECK(i2c(other, TEST_BUS, MEM_ADDR, &write, 1) == -EPERM, "a claim on something else: EPERM");
	CHECK(!i2c(dupe, TEST_BUS, MEM_ADDR, &write, 1), "a copy of the claim works");
	close(dupe);
	close(devnull);
	close(other);
	close(wrong);

	CHECK(i2c(c, TEST_BUS, MEM_ADDR, &write, 0) == -EINVAL, "no segments: EINVAL");
	CHECK(i2c(c, TEST_BUS, MEM_ADDR, &write, LEMNOS_BUS_MAX_SEGMENTS + 1) == -EINVAL,
	      "too many segments: EINVAL");
	struct lemnos_i2c_segment huge = { .buf = (uintptr_t)big, .len = BUFFER + 1 };

	CHECK(i2c(c, TEST_BUS, MEM_ADDR, &huge, 1) == -EMSGSIZE, "more than the buffer: EMSGSIZE");
	struct lemnos_i2c_segment half[3] = {
		{ .buf = (uintptr_t)big, .len = BUFFER / 2 },
		{ .buf = (uintptr_t)big, .len = BUFFER / 2 },
		{ .buf = (uintptr_t)big, .len = 1 },
	};
	CHECK(i2c(c, TEST_BUS, MEM_ADDR, half, 3) == -EMSGSIZE, "segments that sum past it: EMSGSIZE");
	CHECK(!i2c(c, TEST_BUS, MEM_ADDR, half, 2), "segments that fill it exactly");
	struct lemnos_i2c_segment bad = { .buf = 16, .len = 4 };

	CHECK(i2c(c, TEST_BUS, MEM_ADDR, &bad, 1) == -EFAULT, "a bad pointer: EFAULT");
	bad.flags = 0x8000;
	CHECK(i2c(c, TEST_BUS, MEM_ADDR, &bad, 1) == -EINVAL, "unknown segment flags: EINVAL");
	close(c);

	c = claim_i2c(TEST_BUS, HELD_ADDR);
	CHECK(i2c(c, TEST_BUS, HELD_ADDR, &write, 1) == -EBUSY, "an address a kernel driver holds: EBUSY");
	close(c);
	c = claim_i2c(TEST_BUS, 0x60);
	CHECK(i2c(c, TEST_BUS, 0x60, &write, 1) == -ENXIO, "nothing answers: ENXIO");
	close(c);
	c = claim_i2c(39, MEM_ADDR);
	CHECK(i2c(c, 39, MEM_ADDR, &write, 1) == -ENODEV, "no such adapter: ENODEV");
	close(c);
	c = claim("i2c-40:0x80");
	CHECK(i2c(c, TEST_BUS, 0x80, &write, 1) == -EINVAL, "an address past 7 bits: EINVAL");
	close(c);
}

static void check_spi(void)
{
	static uint8_t big[BUFFER + 1];
	int c = claim("spi-40.0");
	uint8_t tx[] = { 1, 2, 3, 4, 5, 6, 7, 8 }, rx[8] = { 0 }, rx2[2] = { 0 };
	struct lemnos_spi_segment one = {
		.tx = (uintptr_t)tx, .rx = (uintptr_t)rx, .len = sizeof(tx),
	};
	struct lemnos_spi_segment two[2] = {
		{ .tx = (uintptr_t)tx, .len = 2 },
		{ .tx = (uintptr_t)(tx + 6), .rx = (uintptr_t)rx2, .len = 2, .delay_us = 5 },
	};

	CHECK(c >= 0, "claimed the device");
	CHECK(!spi(c, 0, 0, &one, 1) && !memcmp(tx, rx, sizeof(tx)), "full duplex: 8 bytes came back");
	CHECK(!spi(c, 0, 0, two, 2) && rx2[0] == 7 && rx2[1] == 8, "two segments: %u %u", rx2[0], rx2[1]);
	CHECK(!spi(c, 0, 3, &one, 1), "mode 3 applied");
	struct lemnos_spi_info info = { .bus = TEST_BUS, .cs = 0 };

	CHECK(!ioctl(fd, LEMNOS_IOC_SPI_INFO, &info) && info.mode == 3,
	      "info: bound, and in the mode last applied (%u)", info.mode);
	info.cs = 1;
	CHECK(ioctl(fd, LEMNOS_IOC_SPI_INFO, &info) && errno == ENODEV,
	      "info on a device not bound here: ENODEV");
	CHECK(spi(c, 0, 0x1000, &one, 1) == -EINVAL, "an unknown mode bit: EINVAL");
	CHECK(spi(-1, 0, 0, &one, 1) == -EPERM, "no claim: EPERM");
	struct lemnos_spi_segment neither = { .len = 4 };

	CHECK(spi(c, 0, 0, &neither, 1) == -EINVAL, "a length with no buffer: EINVAL");
	CHECK(spi(c, 0, 0, &one, 0) == -EINVAL, "no segments: EINVAL");
	struct lemnos_spi_segment huge = { .tx = (uintptr_t)big, .len = BUFFER + 1 };

	CHECK(spi(c, 0, 0, &huge, 1) == -EMSGSIZE, "more than the buffer: EMSGSIZE");
	huge.len = BUFFER;
	CHECK(!spi(c, 0, 0, &huge, 1), "exactly the buffer");
	struct lemnos_spi_segment bad = { .tx = 16, .len = 4 };

	CHECK(spi(c, 0, 0, &bad, 1) == -EFAULT, "a bad pointer: EFAULT");
	close(c);

	c = claim("spi-40.1");
	CHECK(spi(c, 1, 0, &one, 1) == -ENODEV, "a device not bound here: ENODEV");
	close(c);
}

int main(int argc, char **argv)
{
	const char *node = argc > 1 ? argv[1] : "/dev/lemnos";

	setvbuf(stdout, NULL, _IOLBF, 0);
	fd = open(node, O_RDWR | O_CLOEXEC);
	if (fd < 0) {
		perror(node);
		return 2;
	}
	check_smbus();
	check_i2c();
	check_spi();
	close(fd);
	printf("%s: %d failed\n", node, failures);
	return failures ? 1 : 0;
}
