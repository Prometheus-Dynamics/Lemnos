// SPDX-License-Identifier: GPL-2.0
/*
 * The PIO interface against the test harness's stand-in for the driver:
 * the allowed operations, every refused index, DMA moved in pieces through
 * the module's buffers, and a transfer that never finishes.
 *
 * Usage: lemnos_pio_check /dev/lemnos
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

#include "../lemnos.h"

#define PARAM "/sys/module/rp1_pio_mock/parameters/"

static int failures, fd;

#define CHECK(cond, ...)                                                   \
	do {                                                               \
		int ok_ = !!(cond);                                        \
		printf(ok_ ? "ok   " : "FAIL ");                           \
		failures += !ok_;                                          \
		printf(__VA_ARGS__);                                       \
		printf("\n");                                              \
	} while (0)

static int param(const char *name)
{
	char path[128], text[16] = { 0 };
	int f, v = -999;

	snprintf(path, sizeof(path), PARAM "%s", name);
	f = open(path, O_RDONLY);
	if (f >= 0 && read(f, text, sizeof(text) - 1) > 0)
		sscanf(text, "%d", &v);
	if (f >= 0)
		close(f);
	return v;
}

static int set_sticky(const char *value)
{
	int f = open(PARAM "sticky_sm", O_WRONLY);
	int r = f >= 0 && write(f, value, strlen(value)) > 0 ? 0 : -1;

	if (f >= 0)
		close(f);
	return r;
}

static int call(int c, uint32_t op, const void *args, size_t size, int64_t *result, void *out)
{
	struct lemnos_pio_call p = { .claim = c, .op = op };
	int r;

	memcpy(p.args, args, size);
	r = ioctl(fd, LEMNOS_IOC_PIO_CALL, &p) ? -errno : 0;
	if (result)
		*result = p.result;
	if (out)
		memcpy(out, p.args, size);
	return r;
}

static int claim(const char *name)
{
	struct lemnos_claim c = { 0 };

	strncpy(c.resource, name, sizeof(c.resource) - 1);
	return ioctl(fd, LEMNOS_IOC_CLAIM, &c) ? -errno : c.fd;
}

static int config_xfer(int c, uint32_t sm, uint32_t dir, uint32_t size, uint32_t count)
{
	struct lemnos_pio_config_xfer x = {
		.claim = c, .sm = sm, .dir = dir, .buf_size = size, .buf_count = count,
	};

	return ioctl(fd, LEMNOS_IOC_PIO_CONFIG_XFER, &x) ? -errno : 0;
}

static int xfer(int c, uint32_t sm, uint32_t dir, void *data, uint32_t len, uint32_t timeout_ms)
{
	struct lemnos_pio_xfer x = {
		.claim = c, .sm = sm, .dir = dir, .len = len, .data = (uintptr_t)data,
		.timeout_ms = timeout_ms,
	};

	return ioctl(fd, LEMNOS_IOC_PIO_XFER, &x) ? -errno : 0;
}

int main(int argc, char **argv)
{
	const char *node = argc > 1 ? argv[1] : "/dev/lemnos";
	static uint8_t out[10000], in[10000];
	int64_t result = -1;

	setvbuf(stdout, NULL, _IOLBF, 0);
	fd = open(node, O_RDWR | O_CLOEXEC);
	if (fd < 0) {
		perror(node);
		return 2;
	}

	struct lemnos_pio_open o = { .claim = -1 };

	CHECK(ioctl(fd, LEMNOS_IOC_PIO_OPEN, &o) < 0 && errno == EPERM, "no claim: EPERM");
	int wrong = claim("not-pio");

	o.claim = wrong;
	CHECK(ioctl(fd, LEMNOS_IOC_PIO_OPEN, &o) < 0 && errno == EPERM, "the wrong claim: EPERM");
	close(wrong);

	int c = claim("pio-0");
	uint16_t sm0[8] = { 0 };

	CHECK(call(c, LEMNOS_PIO_SM_CLAIM, sm0, sizeof(sm0), NULL, NULL) == -EPERM,
	      "an operation before opening: EPERM");
	o.claim = c;
	CHECK(!ioctl(fd, LEMNOS_IOC_PIO_OPEN, &o), "client opened");
	CHECK(ioctl(fd, LEMNOS_IOC_PIO_OPEN, &o) < 0 && errno == EBUSY, "opened twice: EBUSY");
	CHECK(param("clients") == 1, "the driver has one client");

	uint16_t mask[8] = { 1 };

	CHECK(!call(c, LEMNOS_PIO_SM_CLAIM, mask, sizeof(mask), &result, NULL), "machine claimed");
	struct { uint16_t n, origin, instrs[32]; } program = { .n = 2, .origin = 0xffff };

	CHECK(!call(c, LEMNOS_PIO_ADD_PROGRAM, &program, sizeof(program), &result, NULL) && result == 5,
	      "program added at offset %lld", (long long)result);
	struct { uint16_t sm; uint8_t blocking, rsvd; uint32_t data; } word = { .data = 41 }, back;

	CHECK(!call(c, LEMNOS_PIO_SM_PUT, &word, sizeof(word), NULL, NULL), "word put");
	memset(&back, 0, sizeof(back));
	CHECK(!call(c, LEMNOS_PIO_SM_GET, &word, sizeof(word), NULL, &back) && back.data == 42,
	      "word got back through the args: %u", back.data);

	/* Every index the driver would have to trust. */
	uint16_t bad_sm[8] = { 4 }, bad_mask[8] = { 16 }, bad_gpio[8] = { 28 };

	CHECK(call(c, LEMNOS_PIO_SM_INIT, bad_sm, sizeof(bad_sm), NULL, NULL) == -EINVAL,
	      "machine 4: EINVAL");
	CHECK(call(c, LEMNOS_PIO_SM_CLAIM, bad_mask, sizeof(bad_mask), NULL, NULL) == -EINVAL,
	      "a mask past 4 machines: EINVAL");
	CHECK(call(c, LEMNOS_PIO_GPIO_INIT, bad_gpio, sizeof(bad_gpio), NULL, NULL) == -EINVAL,
	      "GPIO 28: EINVAL");
	program.n = 33;
	CHECK(call(c, LEMNOS_PIO_ADD_PROGRAM, &program, sizeof(program), NULL, NULL) == -EINVAL,
	      "a program of 33 instructions: EINVAL");
	program.n = 0;
	CHECK(call(c, LEMNOS_PIO_ADD_PROGRAM, &program, sizeof(program), NULL, NULL) == -EINVAL,
	      "an empty program: EINVAL");
	CHECK(call(c, LEMNOS_PIO_OP_COUNT, mask, sizeof(mask), NULL, NULL) == -EINVAL,
	      "an operation past the table: EINVAL");

	/* DMA. */
	CHECK(config_xfer(c, 4, 0, 4096, 4) == -EINVAL, "DMA on machine 4: EINVAL");
	CHECK(config_xfer(c, 0, 2, 4096, 4) == -EINVAL, "a direction of 2: EINVAL");
	CHECK(config_xfer(c, 0, 0, 0, 4) == -EINVAL, "a buffer of 0 bytes: EINVAL");
	CHECK(config_xfer(c, 0, 0, 1 << 19, 4) == -EINVAL, "a buffer past 256 KiB: EINVAL");
	CHECK(config_xfer(c, 0, 0, 4096, 65) == -EINVAL, "more than 64 buffers: EINVAL");
	CHECK(xfer(c, 0, 0, out, 4, 0) == -EINVAL, "a transfer before configuring: EINVAL");
	CHECK(!config_xfer(c, 0, 0, 4096, 4) && !config_xfer(c, 0, 1, 4096, 4), "DMA configured");
	for (size_t i = 0; i < sizeof(out); i++)
		out[i] = (uint8_t)(i * 3);
	/* The stand-in keeps only the last piece: send and read one piece's worth. */
	CHECK(!xfer(c, 0, 0, out, 4096, 0), "sent one buffer");
	CHECK(!xfer(c, 0, 1, in, 4096, 0) && !memcmp(in, out, 4096), "the same bytes came back");
	CHECK(!xfer(c, 0, 0, out, sizeof(out), 0), "sent more than a buffer, in pieces");
	CHECK(xfer(c, 0, 0, (void *)16, 4, 0) == -EFAULT, "a bad pointer: EFAULT");
	CHECK(param("last_addr_nonzero") == 0, "the driver was never given an address");

	/* A transfer that never finishes: refused from then on, until reconfigured. */
	CHECK(!set_sticky("1") && !config_xfer(c, 1, 0, 4096, 4), "machine 1 will hang");
	CHECK(xfer(c, 1, 0, out, 16, 20) == -ETIMEDOUT, "the wait ends: ETIMEDOUT");
	CHECK(xfer(c, 1, 0, out, 16, 20) == -EIO, "a stuck channel refuses: EIO");
	CHECK(!set_sticky("-1") && !config_xfer(c, 1, 0, 4096, 4) && !xfer(c, 1, 0, out, 16, 0),
	      "reconfiguring clears it");

	close(c);
	CHECK(param("clients") == 0, "closing the claim closed the client");
	close(fd);
	printf("%s: %d failed\n", node, failures);
	return failures ? 1 : 0;
}
