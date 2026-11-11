// SPDX-License-Identifier: GPL-2.0
/*
 * The PIO interface against the test harness's stand-in for the driver:
 * the allowed operations, every refused index, DMA moved in pieces through
 * the module's buffers, and a transfer that never finishes.
 *
 * Usage: lemnos_pio_check /dev/lemnos
 *        lemnos_pio_check /dev/lemnos board
 *
 * The first form is for the test harness only: it drives machines 0 and 1
 * whoever owns them. "board" is the form for real hardware that others may be
 * using: it takes a free machine from the driver, touches no pin, loads a
 * three-instruction loopback (pull, mov isr osr, push), and checks words and
 * DMA come back through it; then a child is killed with a transfer stuck on
 * its machine, and its machine and program space must be free again.
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <pthread.h>
#include <signal.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <time.h>
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


/* pull block; mov isr, osr; push block. No pin is read or driven. */
static const uint16_t loopback[3] = { 0x80a0, 0xa0c7, 0x8020 };

struct sm_init { uint16_t sm, initial_pc; uint32_t clkdiv, execctrl, shiftctrl, pinctrl; };
struct sm_word { uint16_t sm; uint8_t blocking, rsvd; uint32_t data; };
struct sm_enable { uint16_t mask; uint8_t enable, rsvd; };

static uint64_t now_ns(void)
{
	struct timespec t;

	clock_gettime(CLOCK_MONOTONIC, &t);
	return (uint64_t)t.tv_sec * 1000000000ull + (uint64_t)t.tv_nsec;
}

static int cmp_u64(const void *a, const void *b)
{
	uint64_t x = *(const uint64_t *)a, y = *(const uint64_t *)b;

	return x < y ? -1 : x > y;
}

/* Opens the node, claims pio-0, opens a client, takes a free machine and loads the loopback on
 * it, running. Returns the claim; *sm and *origin say where. */
static int board_setup(const char *node, int *sm, int *origin)
{
	struct lemnos_pio_open o;
	uint16_t any[8] = { 0 };
	struct { uint16_t n, origin, instrs[32]; } program = { .n = 3, .origin = 0xffff };
	int64_t result = -1;
	int c;

	fd = open(node, O_RDWR | O_CLOEXEC);
	if (fd < 0)
		return -1;
	c = claim("pio-0");
	o.claim = c;
	if (c < 0 || ioctl(fd, LEMNOS_IOC_PIO_OPEN, &o))
		return -1;
	if (call(c, LEMNOS_PIO_SM_CLAIM, any, sizeof(any), &result, NULL) || result < 0 || result > 3)
		return -1;
	*sm = (int)result;
	memcpy(program.instrs, loopback, sizeof(loopback));
	if (call(c, LEMNOS_PIO_ADD_PROGRAM, &program, sizeof(program), &result, NULL) || result < 0)
		return -1;
	*origin = (int)result;
	/* clkdiv 1.0; wrap from origin+2 back to origin; shift right, no autopush or autopull;
	 * pinctrl 0: no pin in any group. */
	struct sm_init init = {
		.sm = (uint16_t)*sm, .initial_pc = (uint16_t)*origin, .clkdiv = 1u << 16,
		.execctrl = ((uint32_t)(*origin + 2) << 12) | ((uint32_t)*origin << 7),
		.shiftctrl = (1u << 18) | (1u << 19), .pinctrl = 0,
	};
	if (call(c, LEMNOS_PIO_SM_INIT, &init, sizeof(init), &result, NULL) || result < 0)
		return -1;
	struct sm_enable en = { .mask = (uint16_t)(1u << *sm), .enable = 1 };
	if (call(c, LEMNOS_PIO_SM_SET_ENABLED, &en, sizeof(en), &result, NULL) || result < 0)
		return -1;
	return c;
}

struct mover { int claim, sm; uint32_t dir, len, timeout; void *data; int ret; };

static void *move_thread(void *arg)
{
	struct mover *m = arg;

	m->ret = xfer(m->claim, (uint32_t)m->sm, m->dir, m->data, m->len, m->timeout);
	return NULL;
}

static int board(const char *node)
{
	static uint8_t out[65536], in[65536];
	static uint64_t times[2000];
	int sm = -1, origin = -1, c, pipefd[2];
	int64_t result;

	c = board_setup(node, &sm, &origin);
	CHECK(c >= 0, "a free machine (%d) running the loopback at offset %d", sm, origin);
	if (c < 0) {
		printf("%s: setup failed (%s)\n", node, strerror(errno));
		return 1;
	}

	int words_ok = 1;

	for (uint32_t i = 0; i < 2000; i++) {
		struct sm_word w = { .sm = (uint16_t)sm, .blocking = 1, .data = 0x5a000000u | i }, back;
		uint64_t t0 = now_ns();

		words_ok &= !call(c, LEMNOS_PIO_SM_PUT, &w, sizeof(w), &result, NULL);
		memset(&back, 0, sizeof(back));
		back.sm = (uint16_t)sm;
		back.blocking = 1;
		words_ok &= !call(c, LEMNOS_PIO_SM_GET, &back, sizeof(back), &result, &back) &&
			    back.data == w.data;
		times[i] = now_ns() - t0;
	}
	qsort(times, 2000, sizeof(times[0]), cmp_u64);
	CHECK(words_ok, "2000 words came back through the machine");
	printf("measure put+get one word: median %llu ns  p99 %llu ns\n",
	       (unsigned long long)times[1000], (unsigned long long)times[1980]);

	CHECK(!config_xfer(c, (uint32_t)sm, LEMNOS_PIO_TO_SM, 4096, 4) &&
	      !config_xfer(c, (uint32_t)sm, LEMNOS_PIO_FROM_SM, 4096, 4), "DMA configured both ways");
	for (size_t i = 0; i < sizeof(out); i++)
		out[i] = (uint8_t)(i * 7 + 3);
	for (int round = 0; round < 3; round++) {
		struct mover rx = { c, sm, LEMNOS_PIO_FROM_SM, sizeof(in), 2000, in, -1 };
		struct mover tx = { c, sm, LEMNOS_PIO_TO_SM, sizeof(out), 2000, out, -1 };
		pthread_t a, b;

		memset(in, 0, sizeof(in));
		uint64_t t0 = now_ns();

		pthread_create(&a, NULL, move_thread, &rx);
		pthread_create(&b, NULL, move_thread, &tx);
		pthread_join(b, NULL);
		pthread_join(a, NULL);
		uint64_t t = now_ns() - t0;

		CHECK(!rx.ret && !tx.ret && !memcmp(in, out, sizeof(in)),
		      "64 KiB out and back by DMA, in 4 KiB pieces: %s, %s, %llu us",
		      strerror(-tx.ret), strerror(-rx.ret), (unsigned long long)(t / 1000));
	}
	CHECK(xfer(c, (uint32_t)sm, LEMNOS_PIO_TO_SM, (void *)16, 4, 0) == -EFAULT,
	      "a bad pointer: EFAULT");
	/* Nothing reads the machine now: once its two FIFOs are full the send stalls. */
	CHECK(xfer(c, (uint32_t)sm, LEMNOS_PIO_TO_SM, out, 4096, 50) == -ETIMEDOUT,
	      "a send nothing drains ends: ETIMEDOUT");
	CHECK(xfer(c, (uint32_t)sm, LEMNOS_PIO_TO_SM, out, 16, 50) == -EIO,
	      "and the channel refuses after: EIO");
	CHECK(!config_xfer(c, (uint32_t)sm, LEMNOS_PIO_TO_SM, 4096, 4), "reconfiguring clears it");
	close(c);
	close(fd);

	/* A holder killed with a transfer stuck on its machine. */
	if (pipe(pipefd))
		return 2;
	pid_t child = fork();

	if (!child) {
		int csm = -1, corigin = -1, cc = board_setup(node, &csm, &corigin);
		int msg[2] = { csm, corigin };

		if (cc < 0 || config_xfer(cc, (uint32_t)csm, LEMNOS_PIO_TO_SM, 4096, 4))
			_exit(3);
		if (write(pipefd[1], msg, sizeof(msg)) != sizeof(msg))
			_exit(4);
		xfer(cc, (uint32_t)csm, LEMNOS_PIO_TO_SM, out, sizeof(out), 60000);
		_exit(5);
	}
	int msg[2] = { -1, -1 }, status = 0;

	close(pipefd[1]);
	CHECK(read(pipefd[0], msg, sizeof(msg)) == sizeof(msg), "a child holds machine %d, program at %d, sending",
	      msg[0], msg[1]);
	usleep(300000);
	kill(child, SIGKILL);
	waitpid(child, &status, 0);
	CHECK(WIFSIGNALED(status), "the child was killed mid-transfer");
	int nsm = -1, norigin = -1;

	c = board_setup(node, &nsm, &norigin);
	CHECK(c >= 0, "a new holder got a machine (%d) and program space (%d)", nsm, norigin);
	if (c >= 0) {
		uint16_t mask[8] = { (uint16_t)(1u << msg[0]) };

		/* Its own machine is claimed already; the child's must be free unless it is the same. */
		CHECK(msg[0] == nsm || !call(c, LEMNOS_PIO_SM_CLAIM, mask, sizeof(mask), &result, NULL),
		      "the killed child's machine %d can be claimed: %lld", msg[0], (long long)result);
		close(c);
	}
	close(fd);
	printf("%s board: %d failed\n", node, failures);
	return failures ? 1 : 0;
}

int main(int argc, char **argv)
{
	const char *node = argc > 1 ? argv[1] : "/dev/lemnos";
	static uint8_t out[10000], in[10000];
	int64_t result = -1;

	setvbuf(stdout, NULL, _IOLBF, 0);
	if (argc > 2 && !strcmp(argv[2], "board"))
		return board(node);
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

	/* A holder killed while its transfer hangs, with a long timeout: it ends at once. */
	CHECK(!set_sticky("1"), "machine 1 will hang again");
	pid_t child = fork();

	if (!child) {
		struct lemnos_pio_open co;

		fd = open(node, O_RDWR | O_CLOEXEC);
		co.claim = claim("pio-0");
		if (fd < 0 || co.claim < 0 || ioctl(fd, LEMNOS_IOC_PIO_OPEN, &co) ||
		    config_xfer(co.claim, 1, 0, 4096, 4))
			_exit(3);
		xfer(co.claim, 1, 0, out, 16, 60000);
		_exit(4);
	}
	int status = 0;

	usleep(200000);
	uint64_t t0 = now_ns();

	kill(child, SIGKILL);
	waitpid(child, &status, 0);
	uint64_t ms = (now_ns() - t0) / 1000000;

	CHECK(WIFSIGNALED(status) && ms < 2000, "a holder killed mid-transfer ends at once: %llu ms",
	      (unsigned long long)ms);
	CHECK(!set_sticky("-1") && param("clients") == 0, "and its client is closed");
	printf("%s: %d failed\n", node, failures);
	return failures ? 1 : 0;
}
