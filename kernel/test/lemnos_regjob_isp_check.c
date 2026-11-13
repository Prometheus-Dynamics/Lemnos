// SPDX-License-Identifier: GPL-2.0
/*
 * Register jobs (lemnos_regjob.h) on the fake ISP (test module
 * lemnos_test_regjob, module parameter test_rj_fake_regs), run with the K230
 * ISP's profile: a capture path that keeps writing its buffers between jobs.
 *
 * The fake block has no shadow registers of its own, so the test sets them
 * the way the block would after the module's forced update (or leaves them,
 * for a block that didn't take the new addresses). Followed through: the
 * module writes each plane's base, its size from the checked length and a
 * zero offset; on an idle block it switches the block to them at once, on a
 * running one only in the handler at the end of a frame; the buffers of the
 * job before stay live until that switch, and stay live after it when the
 * block didn't take the new addresses; the raw output the same way. Then the
 * refusals; the configuration role beside the frames role (its own table, the
 * shared isp_ctrl merged, the ISP core's interrupt as events, a second one
 * refused); a frame that never comes (the collection times the job out and
 * resets the block, and the configuration role hears of it); a session closed
 * on a running path (the block is reset before anything is released); and the
 * configuration role alone.
 *
 * Usage: lemnos_regjob_isp_check /dev/lemnos <device> <heap>
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <errno.h>
#include <fcntl.h>
#include <linux/dma-heap.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

#include "../lemnos.h"
#include "../lemnos_regjob.h"
#include "find_block.h"

/* The ISP's registers (kernel/lemnos_regjob_k230isp.c). */
#define ISP_CTRL 0x0400
#define ISP_ACQ_PROP 0x0404
#define ISP_ACQ_H_SIZE 0x0410
#define ISP_AWB_PROP 0x0510
#define ISP_IMSC 0x05bc
#define ISP_MIS 0x05c4
#define ISP_ICR 0x05c8
#define MI_CTRL 0x1300
#define MI_MP_CTRL 0x1310
#define MI_MP_FMT 0x1314
#define MI_MP_Y_BASE 0x1324
#define MI_MP_Y_SIZE 0x1328
#define MI_MP_Y_OFFS 0x132c
#define MI_MP_Y_LLENGTH 0x1330
#define MI_MP_CB_BASE 0x1340
#define MI_MP_CR_BASE 0x134c
#define MI_MP_Y_BASE2 0x1358
#define MI_MP_Y_BASE_SHD 0x1370
#define MI_MP_RAW_BASE 0x1394
#define MI_MP_RAW_SIZE 0x1398
#define MI_MP_RAW_OFFS 0x139c
#define MI_MP_RAW_BASE_SHD 0x13b4
#define MI_MP_JDP_BASE 0x13c0
#define MI_IMSC 0x16c0
#define MI_MIS 0x16d0
#define MI_RIS 0x16e0
#define AE_MEAN_0 0x2614
#define LSC_CTRL 0x2200
#define LSC_R_TABLE_ADDR 0x2204
#define LSC_R_TABLE_DATA 0x2214
#define LSC_XSIZE_0 0x2244
#define LSC_TABLE_SEL 0x2264

#define WINDOW 0x9000
#define PLANES 3	/* Y, Cb, Cr */
#define SLOTS 4		/* and raw */
#define BUF_BYTES (64u * 1024)
#define Y_BYTES 32768u
#define C_BYTES 16384u
#define FRAME_END 1u
#define RAW_FRAME_END 2u
#define WRAP_Y 0x100u
#define CTRL_ON 0x17u		/* enable, Bayer RGB mode, input formatter */
#define CTRL_GAMMA 0x800u	/* the configuration role's gamma-out enable */
#define FRAME_OUT 2u		/* the ISP core's frame-out interrupt */

/* The raw output's slot is pointed at the Y plane's range: the YCbCr output doesn't use it. */
static const uint32_t plane_reg[SLOTS] = { MI_MP_Y_BASE, MI_MP_CB_BASE, MI_MP_CR_BASE,
					   MI_MP_RAW_BASE };
static const uint32_t plane_off[SLOTS] = { 0, Y_BYTES, Y_BYTES, 0 };
static const uint32_t plane_len[SLOTS] = { Y_BYTES, C_BYTES, C_BYTES, Y_BYTES };

static int failures, ctl, heap, sess, cfg = -1;
static char sysdir[256];

static void check(int ok, const char *what)
{
	printf("%s %s\n", ok ? "ok  " : "FAIL", what);
	failures += !ok;
}

/* --- the fake block --- */

static int sys_put(const char *attr, const char *text)
{
	char path[320];
	int fd, ok;

	snprintf(path, sizeof(path), "%s/%s", sysdir, attr);
	fd = open(path, O_WRONLY | O_CLOEXEC);
	ok = fd >= 0 && write(fd, text, strlen(text)) == (ssize_t)strlen(text);
	if (fd >= 0)
		close(fd);
	return ok ? 0 : -1;
}

static void poke(uint32_t off, uint32_t value)
{
	char text[48];

	snprintf(text, sizeof(text), "%x %x", off, value);
	if (sys_put("poke", text))
		printf("FAIL poke %x\n", off), failures++;
}

static int stamp(uint32_t reg, uint8_t byte)
{
	char text[48];

	snprintf(text, sizeof(text), "%x %x", reg, byte);
	return sys_put("stamp", text);
}

static void regs(uint32_t *out)
{
	char path[320];
	int fd;

	snprintf(path, sizeof(path), "%s/regs", sysdir);
	size_t done = 0;
	ssize_t n = 0;

	/* sysfs hands a binary attribute out a page at a time. */
	fd = open(path, O_RDONLY | O_CLOEXEC);
	while (fd >= 0 && done < WINDOW) {
		n = pread(fd, (char *)out + done, WINDOW - done, (off_t)done);
		if (n <= 0)
			break;
		done += (size_t)n;
	}
	if (done != WINDOW)
		printf("FAIL regs\n"), failures++;
	if (fd >= 0)
		close(fd);
}

static uint32_t reg(uint32_t off)
{
	static uint32_t r[WINDOW / 4];

	regs(r);
	return r[off / 4];
}

static int resets(void)
{
	char path[320], text[32];
	int fd, n;

	snprintf(path, sizeof(path), "%s/resets", sysdir);
	fd = open(path, O_RDONLY | O_CLOEXEC);
	n = fd < 0 ? -1 : (int)read(fd, text, sizeof(text) - 1);
	if (fd >= 0)
		close(fd);
	if (n <= 0)
		return -1;
	text[n] = 0;
	return atoi(text);
}

static void sleep_ms(long ms)
{
	struct timespec t = { ms / 1000, (ms % 1000) * 1000000L };

	nanosleep(&t, NULL);
}

/* The block reaches the end of a frame of `output`: its status set, its interrupt raised. */
static void output_end(uint32_t output, uint32_t wraps)
{
	poke(MI_MIS, output);
	poke(MI_RIS, output | wraps);
	if (sys_put("irq", "1"))
		printf("FAIL irq\n"), failures++;
	sleep_ms(20);
}

static void frame_end(uint32_t wraps)
{
	output_end(FRAME_END, wraps);
}

/* A frame leaves the ISP core: its status set, interrupt 0 raised. */
static void frame_out(int settle_ms)
{
	poke(ISP_MIS, FRAME_OUT);
	if (sys_put("irq_line", "0"))
		printf("FAIL irq_line\n"), failures++;
	sleep_ms(settle_ms);
}

/* --- the session --- */

struct buf {
	int fd;
	uint32_t index;
	uint64_t addr, bytes;
	uint8_t *map;
};

static int import(struct buf *b)
{
	struct dma_heap_allocation_data d = { .len = BUF_BYTES, .fd_flags = O_RDWR | O_CLOEXEC };
	struct lemnos_regjob_import imp = { 0 };

	if (ioctl(heap, DMA_HEAP_IOCTL_ALLOC, &d))
		return -1;
	b->fd = (int)d.fd;
	imp.fd = b->fd;
	if (ioctl(sess, LEMNOS_REGJOB_IOC_IMPORT, &imp))
		return -1;
	b->index = imp.index;
	b->addr = imp.addr;
	b->bytes = imp.bytes;
	b->map = mmap(NULL, BUF_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, b->fd, 0);
	return b->map == MAP_FAILED ? -1 : 0;
}

static int release(const struct buf *b)
{
	return ioctl(sess, LEMNOS_REGJOB_IOC_RELEASE, &b->index) ? errno : 0;
}

/* The block takes a buffer's planes, as it would after the module's forced update. */
static void shadows(const struct buf *b)
{
	for (int k = 0; k < PLANES; k++) {
		poke(MI_MP_Y_BASE_SHD + 12 * k, (uint32_t)(b->addr + plane_off[k]));
		poke(MI_MP_Y_BASE_SHD + 12 * k + 4, plane_len[k]);
	}
	poke(MI_MP_RAW_BASE_SHD, (uint32_t)b->addr);
}

static struct lemnos_regjob_info info(void)
{
	struct lemnos_regjob_info in;

	memset(&in, 0xff, sizeof(in));
	if (ioctl(sess, LEMNOS_REGJOB_IOC_INFO, &in))
		printf("FAIL info: %s\n", strerror(errno)), failures++;
	return in;
}

struct job {
	struct lemnos_regjob_write w[16];
	struct lemnos_regjob_addr a[SLOTS];
	uint32_t reads[1], values[1];
	struct lemnos_regjob_run run;
};

/* A frame into `b`: NV12, 128 lines of 256 bytes, the ISP on, collected later. */
static void frame_job(struct job *j, const struct buf *b)
{
	static const struct lemnos_regjob_write w[] = {
		{ ISP_CTRL, 0x1 },	   { ISP_ACQ_PROP, 0 }, { ISP_ACQ_H_SIZE, 256 },
		{ MI_MP_FMT, 0x0 },	   { MI_MP_Y_LLENGTH, 256 },
	};

	memset(j, 0, sizeof(*j));
	memcpy(j->w, w, sizeof(w));
	for (int k = 0; k < SLOTS; k++)
		j->a[k] = (struct lemnos_regjob_addr){ plane_reg[k], b->index, plane_off[k],
						      plane_len[k] };
	j->reads[0] = AE_MEAN_0;
	j->run.writes = (uintptr_t)j->w;
	j->run.addrs = (uintptr_t)j->a;
	j->run.reads = (uintptr_t)j->reads;
	j->run.values = (uintptr_t)j->values;
	j->run.nwrites = sizeof(w) / sizeof(w[0]);
	j->run.naddrs = SLOTS;
	j->run.nreads = 1;
	j->run.start = 0x1;
	j->run.timeout_ms = 3000;
	j->run.flags = LEMNOS_REGJOB_RUN_NOWAIT;
}

static int run(struct job *j)
{
	return ioctl(sess, LEMNOS_REGJOB_IOC_RUN, &j->run) ? errno : 0;
}

static uint64_t collected_ns;

static int collect(struct job *j, uint32_t *status)
{
	struct lemnos_regjob_wait w = { .values = (uintptr_t)j->values, .nreads = j->run.nreads };

	if (ioctl(sess, LEMNOS_REGJOB_IOC_WAIT, &w))
		return errno;
	*status = w.status;
	collected_ns = w.timestamp_ns;
	return 0;
}

static uint64_t now_ns(void)
{
	struct timespec t;

	clock_gettime(CLOCK_MONOTONIC, &t);
	return (uint64_t)t.tv_sec * 1000000000u + (uint64_t)t.tv_nsec;
}

/* --- the checks --- */

static struct buf a, b, c, d;

static void test_stream(void)
{
	static struct job j;
	uint32_t status = 0;
	int ok = 1;

	/* The first frame, on an idle block: switched at once. */
	shadows(&a);
	frame_job(&j, &a);
	check(run(&j) == 0, "a first frame starts on the idle block");
	for (int k = 0; k < PLANES; k++) {
		uint32_t base = plane_reg[k];

		ok &= reg(base) == (uint32_t)(a.addr + plane_off[k]);
		ok &= reg(base + 4) == plane_len[k] && reg(base + 8) == 0;
		ok &= reg(MI_MP_Y_BASE2 + 4 * k) == (uint32_t)(a.addr + plane_off[k]);
	}
	ok &= reg(MI_MP_RAW_BASE) == (uint32_t)a.addr && reg(MI_MP_RAW_SIZE) == Y_BYTES &&
	      reg(MI_MP_RAW_OFFS) == 0;
	ok &= reg(MI_MP_JDP_BASE) == (uint32_t)a.addr && reg(MI_MP_JDP_BASE + 4) == Y_BYTES &&
	      reg(MI_MP_JDP_BASE + 8) == 0;
	check(ok, "every address register of the main path (Y, Cb, Cr, their second set, raw, "
		  "JPEG) is the module's, from the checked range");
	check(reg(MI_MP_CTRL) == 0x38, "the block was switched to them before the start");
	check(reg(MI_CTRL) == 1 && reg(MI_IMSC) == (FRAME_END | RAW_FRAME_END),
	      "the main path is on, with both outputs' frame-end interrupts");
	check(reg(ISP_CTRL) == 1 && reg(ISP_ACQ_H_SIZE) == 256, "the job's own registers are written");
	check(release(&a) == EBUSY, "the frame's buffer is held");
	memset(a.map, 0, BUF_BYTES);
	check(!stamp(MI_MP_Y_BASE, 0x5a) && !stamp(MI_MP_CB_BASE, 0x5b),
	      "the block writes the planes");
	poke(AE_MEAN_0, 0x1234);
	uint64_t before_ns = now_ns();

	frame_end(WRAP_Y);
	check(collect(&j, &status) == 0 && status == (FRAME_END | WRAP_Y) && j.values[0] == 0x1234,
	      "the frame's end completes it, with the wraps seen and the reads taken then");
	check(collected_ns > before_ns && collected_ns < now_ns(),
	      "with the time the handler took it");
	check(a.map[0] == 0x5a && a.map[Y_BYTES] == 0x5b, "what the block wrote is in the buffer");
	check(info().live == 1 && release(&a) == EBUSY,
	      "the path keeps writing the buffer: it stays held");

	/* The next frame, on the running block: switched only at the frame's end. */
	poke(MI_MP_CTRL, 0);
	frame_job(&j, &b);
	check(run(&j) == 0, "the next frame starts on the running block");
	check(reg(MI_MP_Y_BASE) == (uint32_t)b.addr && reg(MI_MP_CTRL) == 0,
	      "its addresses are written, the switch waits for the frame's end");
	check(info().live == 2 && release(&a) == EBUSY, "both frames' buffers are held meanwhile");
	shadows(&b);
	frame_end(0);
	check(reg(MI_MP_CTRL) == 0x38, "the handler switched the block at the frame's end");
	check(collect(&j, &status) == 0 && status == FRAME_END, "and the job completed there");
	check(info().live == 1 && release(&a) == 0, "the frame before is free once the block left it");
	check(release(&b) == EBUSY, "the frame being written is not");

	/* A block that doesn't take the new addresses. */
	frame_job(&j, &c);
	check(run(&j) == 0, "a frame whose switch the block won't take");
	frame_end(0);
	check(collect(&j, &status) == 0 && status == (FRAME_END | LEMNOS_REGJOB_STATUS_UNLATCHED),
	      "completes with LEMNOS_REGJOB_STATUS_UNLATCHED");
	check(info().live == 2 && release(&b) == EBUSY && release(&c) == EBUSY,
	      "and the block may use either buffer: both stay held");
	frame_job(&j, &d);
	check(run(&j) == 0, "a later frame");
	shadows(&d);
	frame_end(0);
	check(collect(&j, &status) == 0 && status == FRAME_END, "is taken");
	check(release(&b) == 0 && release(&c) == 0 && release(&d) == EBUSY,
	      "and lets every buffer before it go");
	check(info().jobs == 4, "four frames counted");

	/* The raw output: the same switch, its own enable and frame end, its shadow read back. */
	check(!import(&b), "a buffer for raw frames");
	frame_job(&j, &b);
	j.run.start = RAW_FRAME_END;
	check(run(&j) == 0, "a raw frame starts on the running block");
	check(reg(MI_MP_RAW_BASE) == (uint32_t)b.addr && reg(MI_MP_RAW_SIZE) == Y_BYTES,
	      "the raw output's base and size are the module's");
	shadows(&b);
	output_end(RAW_FRAME_END, 0);
	check(collect(&j, &status) == 0 && status == RAW_FRAME_END && reg(MI_CTRL) == RAW_FRAME_END,
	      "the raw output's frame end completes it, with the raw output on");
	frame_job(&j, &d);
	j.run.start = RAW_FRAME_END;
	check(run(&j) == 0, "a raw frame whose switch the block won't take");
	poke(MI_MP_Y_BASE_SHD, (uint32_t)d.addr);
	poke(MI_MP_Y_BASE_SHD + 4, Y_BYTES);
	poke(MI_MP_Y_BASE_SHD + 12, (uint32_t)(d.addr + Y_BYTES));
	poke(MI_MP_Y_BASE_SHD + 16, C_BYTES);
	poke(MI_MP_Y_BASE_SHD + 24, (uint32_t)(d.addr + Y_BYTES));
	poke(MI_MP_Y_BASE_SHD + 28, C_BYTES);
	output_end(RAW_FRAME_END, 0);
	check(collect(&j, &status) == 0 && status == (RAW_FRAME_END | LEMNOS_REGJOB_STATUS_UNLATCHED),
	      "the raw shadow still naming the old buffer: LEMNOS_REGJOB_STATUS_UNLATCHED");
	frame_job(&j, &d);
	check(run(&j) == 0, "back to YCbCr");
	shadows(&d);
	frame_end(0);
	check(collect(&j, &status) == 0 && status == FRAME_END && release(&b) == 0,
	      "taken, and the raw frames' buffer is free");
}

/* A job refused with `err`, and no register touched. */
static void refused(struct job *j, int err, const char *what)
{
	static uint32_t before[WINDOW / 4], after[WINDOW / 4];
	char text[200];
	int got;

	regs(before);
	got = run(j);
	regs(after);
	snprintf(text, sizeof(text), "%s: %s", what, strerror(err));
	if (got != err)
		printf("     got %s\n", strerror(got));
	check(got == err && !memcmp(before, after, sizeof(before)), text);
}

static void add_write(struct job *j, uint32_t offset, uint32_t value)
{
	j->w[j->run.nwrites++] = (struct lemnos_regjob_write){ offset, value };
}

static void test_refusals(void)
{
	static struct job j;
	uint64_t refused_before = info().refused;

	frame_job(&j, &d), add_write(&j, MI_MP_Y_BASE, (uint32_t)a.addr);
	refused(&j, EPERM, "a write to a plane's base");
	frame_job(&j, &d), add_write(&j, MI_MP_Y_SIZE, 0x1ffffff0);
	refused(&j, EPERM, "a write to a plane's size, which the module derives");
	frame_job(&j, &d), add_write(&j, MI_MP_Y_OFFS, 0x100);
	refused(&j, EPERM, "a write to a plane's offset");
	frame_job(&j, &d), add_write(&j, MI_MP_Y_BASE2, (uint32_t)a.addr);
	refused(&j, EPERM, "a write to the ping-pong set");
	frame_job(&j, &d), add_write(&j, MI_MP_RAW_BASE, (uint32_t)a.addr);
	refused(&j, EPERM, "a write to the raw output's base");
	frame_job(&j, &d), add_write(&j, ISP_CTRL, CTRL_GAMMA);
	refused(&j, EPERM, "a bit of isp_ctrl that is the configuration role's");
	frame_job(&j, &d), add_write(&j, ISP_AWB_PROP, 1);
	refused(&j, EPERM, "a register that is the configuration role's");
	frame_job(&j, &d), add_write(&j, MI_MP_CTRL, 0x2);
	refused(&j, EPERM, "automatic updates, which would switch the block on their own");
	frame_job(&j, &d), add_write(&j, MI_CTRL, 0x3);
	refused(&j, EPERM, "another path's enable");
	frame_job(&j, &d), add_write(&j, MI_IMSC, 0xffffffff);
	refused(&j, EPERM, "the interrupt mask, which is the module's");
	frame_job(&j, &d), add_write(&j, ISP_IMSC, 0x2);
	refused(&j, EPERM, "the ISP core's interrupts");
	frame_job(&j, &d), add_write(&j, ISP_ACQ_PROP, 1u << 16);
	refused(&j, EPERM, "the ISP's input from memory");
	frame_job(&j, &d), add_write(&j, 0x3d68, 0);
	refused(&j, EPERM, "the front end's command address");
	frame_job(&j, &d), j.run.start = 0x3;
	refused(&j, EPERM, "a start that turns on both outputs at once");
	frame_job(&j, &d), j.a[0].bytes = 15;
	refused(&j, EFAULT, "a plane shorter than the size register's grain");
	frame_job(&j, &d), j.a[1].offset = Y_BYTES + 8;
	refused(&j, EINVAL, "an unaligned plane");
	frame_job(&j, &d), j.a[1].offset = BUF_BYTES - 8192;
	refused(&j, EFAULT, "a plane past its buffer's end");
	frame_job(&j, &d), j.a[2].buffer = a.index;
	refused(&j, ENOENT, "a buffer the session released");
	frame_job(&j, &d), j.run.naddrs = PLANES;
	refused(&j, EINVAL, "the raw output's address left out");
	check(info().refused - refused_before == 19, "every refusal is counted");
	check(info().live == 1 && reg(MI_MP_Y_BASE) == (uint32_t)d.addr,
	      "and the running path is as it was");
}

/* --- the configuration role --- */

static int open_config(__u32 block)
{
	struct lemnos_regjob_open o = { .block = block, .flags = LEMNOS_REGJOB_OPEN_CONFIG };

	if (ioctl(ctl, LEMNOS_IOC_REGJOB_OPEN, &o))
		return -errno;
	check(o.profile == LEMNOS_REGJOB_PROFILE_K230_ISP && o.window_bytes == WINDOW,
	      "the configuration session runs the ISP's profile");
	return o.fd;
}

/* Writes and reads at once on `fd`: 0 or the errno. */
static int regs_now(int fd, const struct lemnos_regjob_write *w, uint32_t nw,
		    const uint32_t *reads, uint32_t *values, uint32_t nr)
{
	struct lemnos_regjob_regs r = {
		.writes = (uintptr_t)w,
		.reads = (uintptr_t)reads,
		.values = (uintptr_t)values,
		.nwrites = nw,
		.nreads = nr,
	};

	return ioctl(fd, LEMNOS_REGJOB_IOC_WRITE, &r) ? errno : 0;
}

static int write1(int fd, uint32_t offset, uint32_t value)
{
	struct lemnos_regjob_write w = { offset, value };

	return regs_now(fd, &w, 1, NULL, NULL, 0);
}

/* A write refused with `err` on `fd`, and no register touched. */
static void write_refused(int fd, uint32_t offset, uint32_t value, int err, const char *what)
{
	static uint32_t before[WINDOW / 4], after[WINDOW / 4];
	char text[200];
	int got;

	regs(before);
	got = write1(fd, offset, value);
	regs(after);
	snprintf(text, sizeof(text), "%s: %s", what, strerror(err));
	if (got != err)
		printf("     got %s\n", strerror(got));
	check(got == err && !memcmp(before, after, sizeof(before)), text);
}

static int event(int fd, struct lemnos_regjob_event *e)
{
	return ioctl(fd, LEMNOS_REGJOB_IOC_EVENT, e) ? errno : 0;
}

/* Takes every queued event; how many there were. */
static int drain(int fd)
{
	struct lemnos_regjob_event e;
	int n = 0;

	while (!event(fd, &e))
		n++;
	return n;
}

static void test_roles(__u32 block)
{
	struct lemnos_regjob_open o = { .block = block };
	struct lemnos_regjob_event e;
	static struct job j;
	uint32_t reads[2] = { ISP_AWB_PROP, AE_MEAN_0 }, values[2] = { 0 };
	struct lemnos_regjob_write w = { ISP_AWB_PROP, 0xabcd };
	uint64_t before_ns;
	int n, ok;

	check(ioctl(ctl, LEMNOS_IOC_REGJOB_OPEN, &o) && errno == EBUSY,
	      "a second frames session is refused (EBUSY)");
	cfg = open_config(block);
	check(cfg >= 0, "a configuration session opens beside the running frames session");
	o.flags = LEMNOS_REGJOB_OPEN_CONFIG;
	check(ioctl(ctl, LEMNOS_IOC_REGJOB_OPEN, &o) && errno == EBUSY,
	      "a second configuration session is refused (EBUSY)");
	check(reg(ISP_IMSC) == FRAME_OUT, "the ISP core's frame-out interrupt is on for it");

	poke(AE_MEAN_0, 0x77);
	check(regs_now(cfg, &w, 1, reads, values, 2) == 0 && reg(ISP_AWB_PROP) == 0xabcd &&
		      values[0] == 0xabcd && values[1] == 0x77,
	      "its writes are made at once, and its reads read after them");

	/* Its table, not the frames role's. */
	write_refused(cfg, MI_MP_Y_BASE, (uint32_t)a.addr, EPERM, "configuration: a plane's base");
	write_refused(cfg, MI_CTRL, 1, EPERM, "configuration: the path enable");
	write_refused(cfg, MI_MP_CTRL, 0x38, EPERM, "configuration: the forced update");
	write_refused(cfg, ISP_IMSC, 0, EPERM, "configuration: the ISP core's interrupt mask");
	write_refused(cfg, ISP_ACQ_H_SIZE, 64, EPERM,
		      "configuration: the frames role's acquisition window");
	write_refused(cfg, ISP_CTRL, 1, EPERM, "configuration: isp_ctrl's enable");
	write_refused(cfg, ISP_AWB_PROP + 2, 0, EINVAL, "configuration: an unaligned register");
	write_refused(cfg, WINDOW, 0, EINVAL, "configuration: past the window");
	reads[0] = 2;
	check(regs_now(cfg, NULL, 0, reads, values, 1) == EINVAL, "an unaligned read (EINVAL)");
	check(ioctl(cfg, LEMNOS_REGJOB_IOC_IMPORT, &(struct lemnos_regjob_import){ .fd = a.fd }) &&
		      errno == EINVAL,
	      "it imports no buffer (EINVAL)");
	frame_job(&j, &d);
	check(ioctl(cfg, LEMNOS_REGJOB_IOC_RUN, &j.run) && errno == EINVAL, "it runs no job (EINVAL)");
	check(ioctl(cfg, LEMNOS_REGJOB_IOC_RELEASE, &d.index) && errno == EINVAL &&
		      ioctl(cfg, LEMNOS_REGJOB_IOC_WAIT, &(struct lemnos_regjob_wait){ 0 }) &&
		      errno == EINVAL,
	      "and releases or collects nothing (EINVAL)");
	check(event(sess, &e) == EINVAL, "the frames session has no events (EINVAL)");

	/* isp_ctrl: both roles' bits, each written by read-modify-write. */
	poke(ISP_CTRL, 0);
	check(write1(sess, ISP_CTRL, CTRL_ON) == 0 && reg(ISP_CTRL) == CTRL_ON,
	      "the frames role turns the ISP on outside a job");
	check(write1(cfg, ISP_CTRL, CTRL_GAMMA) == 0 && reg(ISP_CTRL) == (CTRL_ON | CTRL_GAMMA),
	      "the configuration role's gamma bit leaves the frames role's bits as they were");
	check(write1(sess, ISP_CTRL, 0x1) == 0 && reg(ISP_CTRL) == (0x1 | CTRL_GAMMA),
	      "the frames role's write leaves the gamma bit as it was");
	frame_job(&j, &d);
	check(run(&j) == 0, "a frame job writing isp_ctrl");
	shadows(&d);
	frame_end(0);
	check(collect(&j, &(uint32_t){ 0 }) == 0 && reg(ISP_CTRL) == (0x1 | CTRL_GAMMA),
	      "keeps the gamma bit too");
	write_refused(sess, ISP_AWB_PROP, 1, EPERM, "frames, outside a job: a configuration register");
	write_refused(sess, MI_MP_Y_BASE, 0, EPERM, "frames, outside a job: a plane's base");

	/*
	 * Lens shading, the configuration role's: the enable, a table address, two 12-bit samples
	 * to a word, a sector size, and the table select, each with its field bits (the masks of
	 * lemnos_regjob_k230isp.c). A value past a field is refused, as the masks say.
	 */
	check(write1(cfg, LSC_CTRL, 1) == 0 && reg(LSC_CTRL) == 1,
	      "lens shading: the enable is written");
	check(write1(cfg, LSC_R_TABLE_ADDR, 153) == 0 && reg(LSC_R_TABLE_ADDR) == 153,
	      "lens shading: bank 1's address (153)");
	check(write1(cfg, LSC_R_TABLE_DATA, 0x00ffffff) == 0 &&
		      reg(LSC_R_TABLE_DATA) == 0x00ffffff,
	      "lens shading: two 12-bit samples to a word");
	check(write1(cfg, LSC_XSIZE_0, 0x03ff03ff) == 0 && reg(LSC_XSIZE_0) == 0x03ff03ff,
	      "lens shading: two 10-bit sector sizes");
	write_refused(cfg, LSC_R_TABLE_DATA, 0x01000000, EPERM,
		      "lens shading: a bit past the sample pair");
	write_refused(cfg, LSC_XSIZE_0, 0x00000400, EPERM, "lens shading: a sector size past 10 bits");
	write_refused(cfg, LSC_CTRL, 2, EPERM, "lens shading: a bit past the enable");
	write_refused(cfg, LSC_TABLE_SEL, 2, EPERM, "lens shading: a table past 1");

	/* Events: the ISP core's interrupt, read and cleared by the module. */
	check(event(cfg, &e) == EAGAIN, "no event before the interrupt (EAGAIN)");
	before_ns = now_ns();
	frame_out(20);
	check(event(cfg, &e) == 0 && e.status == FRAME_OUT && e.lost == 0 &&
		      e.timestamp_ns > before_ns && e.timestamp_ns < now_ns(),
	      "a frame leaving the ISP is an event, with its status and time");
	check(reg(ISP_ICR) == FRAME_OUT, "the module cleared it");
	check(event(cfg, &e) == EAGAIN, "and it is taken once");
	poke(ISP_MIS, 0);
	if (sys_put("irq_line", "0"))
		printf("FAIL irq_line\n"), failures++;
	sleep_ms(20);
	check(event(cfg, &e) == EAGAIN, "an interrupt with nothing pending is no event");
	for (n = 0; n < LEMNOS_REGJOB_EVENTS + 2; n++)
		frame_out(3);
	ok = 1;
	for (n = 0; n < LEMNOS_REGJOB_EVENTS; n++)
		ok &= event(cfg, &e) == 0 && e.lost == 0;
	check(ok, "the queue holds LEMNOS_REGJOB_EVENTS events");
	check(event(cfg, &e) == EAGAIN, "and drops the ones past it");
	frame_out(20);
	check(event(cfg, &e) == 0 && e.lost == 2, "the next one says how many were dropped");
	frame_end(0);
	check(event(cfg, &e) == EAGAIN, "the memory interface's interrupt is the frames role's");
	poke(ISP_MIS, 0);
}

static void test_timeout(void)
{
	static struct job j;
	uint32_t status;
	int before = resets();

	check(!import(&a), "a buffer for a frame that never comes");
	frame_job(&j, &a);
	j.run.timeout_ms = 100;
	check(run(&j) == 0, "a frame that never comes");
	check(collect(&j, &status) == EAGAIN, "is waited for (EAGAIN)");
	sleep_ms(150);
	check(collect(&j, &status) == ETIMEDOUT && resets() == before + 1,
	      "until its time is up: ETIMEDOUT, the block reset");
	check(info().live == 0 && reg(MI_CTRL) == 0 && reg(MI_IMSC) == 0,
	      "the path is off and nothing is live");
	check(release(&a) == 0 && release(&d) == 0, "every buffer can go");
	{
		struct lemnos_regjob_event e;

		check(event(cfg, &e) == 0 && e.status == LEMNOS_REGJOB_EVENT_RESET,
		      "the configuration role hears of the reset (LEMNOS_REGJOB_EVENT_RESET)");
		check(reg(ISP_IMSC) == FRAME_OUT, "and keeps its interrupt");
		frame_out(20);
		check(event(cfg, &e) == 0 && e.status == FRAME_OUT, "which still comes");
	}
}

static void test_close_running(__u32 block)
{
	static struct job j;
	struct lemnos_regjob_open o = { .block = block };
	uint32_t status;
	int before = resets();

	check(!import(&a), "a buffer for a running path");
	shadows(&a);
	frame_job(&j, &a);
	check(run(&j) == 0, "the path runs again");
	frame_end(0);
	check(collect(&j, &status) == 0 && info().live == 1, "a frame completes, the path runs on");
	check(!close(sess), "the session closes on the running path");
	check(resets() == before + 1, "the block was reset before its buffer was let go");
	check(drain(cfg) == 1 && write1(cfg, ISP_AWB_PROP, 5) == 0 && reg(ISP_AWB_PROP) == 5,
	      "the configuration session hears of it, and goes on");
	check(!ioctl(ctl, LEMNOS_IOC_REGJOB_OPEN, &o), "a new session opens");
	sess = o.fd;
	check(info().live == 0 && !info().wedged, "with nothing live");
	close(sess);
}

/*
 * Memory the module allocated for the ISP (LEMNOS_IOC_ALLOC on its block), the way the capture
 * driver gets its ring: imported into a frames session at the same bus address, and a frame
 * written into it.
 */
static void test_module_memory(__u32 block)
{
	static struct job j;
	struct lemnos_regjob_open o = { .block = block };
	struct lemnos_alloc al = { .block = block, .size = BUF_BYTES };
	struct lemnos_regjob_import imp = { 0 };
	struct buf m = { 0 };
	uint32_t status;

	check(!ioctl(ctl, LEMNOS_IOC_REGJOB_OPEN, &o), "a frames session for the module's memory");
	sess = o.fd;
	check(!ioctl(ctl, LEMNOS_IOC_ALLOC, &al) && al.fd >= 0 && al.size == BUF_BYTES,
	      "the module allocates memory for the ISP");
	imp.fd = (__u32)al.fd;
	check(!ioctl(sess, LEMNOS_REGJOB_IOC_IMPORT, &imp) && imp.addr == al.addr &&
		      imp.bytes == BUF_BYTES,
	      "it imports into the session at the address the module gave it");
	m.fd = al.fd;
	m.index = imp.index;
	m.addr = imp.addr;
	m.bytes = imp.bytes;
	shadows(&m);
	frame_job(&j, &m);
	check(run(&j) == 0 && reg(MI_MP_Y_BASE) == (uint32_t)al.addr,
	      "a frame into it starts, the base the module's own");
	frame_end(0);
	check(collect(&j, &status) == 0 && status == FRAME_END, "and completes");
	check(!close(sess), "the session closes");
	check(drain(cfg) == 1, "the running path was reset, and the configuration session told");
	check(!close(al.fd), "the memory goes");
}

/* The configuration role alone, then closed. */
static void test_config_alone(__u32 block)
{
	struct lemnos_regjob_open o = { .block = block };
	struct lemnos_regjob_event e;

	check(!close(cfg), "the configuration session closes");
	check(reg(ISP_IMSC) == 0, "its interrupt goes off with it");
	cfg = open_config(block);
	check(cfg >= 0 && write1(cfg, ISP_AWB_PROP, 9) == 0 && reg(ISP_AWB_PROP) == 9,
	      "a configuration session works with no frames session");
	frame_out(20);
	check(event(cfg, &e) == 0 && e.status == FRAME_OUT, "and has its events");
	check(!ioctl(ctl, LEMNOS_IOC_REGJOB_OPEN, &o), "a frames session opens beside it");
	close(o.fd);
	check(drain(cfg) == 0, "and closing that one, on an idle block, resets nothing");
	close(cfg);
}

int main(int argc, char **argv)
{
	struct lemnos_regjob_open o = { 0 };

	if (argc != 4) {
		printf("usage: lemnos_regjob_isp_check /dev/lemnos <device> <heap>\n");
		return 2;
	}
	setvbuf(stdout, NULL, _IONBF, 0);
	snprintf(sysdir, sizeof(sysdir), "/sys/bus/platform/devices/%s", argv[2]);
	ctl = open(argv[1], O_RDWR | O_CLOEXEC);
	heap = open(argv[3], O_RDONLY | O_CLOEXEC);
	if (ctl < 0 || heap < 0) {
		printf("FAIL open: %s\n", strerror(errno));
		return 1;
	}
	o.block = find_block(ctl, argv[2]);
	if (!o.block) {
		printf("FAIL no block %s\n", argv[2]);
		return 1;
	}
	check(!ioctl(ctl, LEMNOS_IOC_REGJOB_OPEN, &o), "a session opens");
	sess = o.fd;
	check(o.profile == LEMNOS_REGJOB_PROFILE_K230_ISP && o.window_bytes == WINDOW,
	      "it runs the ISP's profile on a 36 KiB window");
	check(!import(&a) && !import(&b) && !import(&c) && !import(&d), "four buffers imported");
	check(reg(MI_CTRL) == 0 && reg(MI_IMSC) == 0 && reg(MI_MP_CTRL) == 0,
	      "the bound block is quiet, with no automatic update");
	test_stream();
	test_refusals();
	test_roles(o.block);
	test_timeout();
	test_close_running(o.block);
	test_module_memory(o.block);
	test_config_alone(o.block);
	printf("%s\n", failures ? "lemnos_regjob_isp_check: FAILED" : "lemnos_regjob_isp_check: passed");
	return failures ? 1 : 0;
}
