// SPDX-License-Identifier: GPL-2.0
/*
 * Register jobs (lemnos_regjob.h) on the fake video engine (test module
 * lemnos_test_regjob, module parameter test_rj_fake_regs), run with the A733
 * encoder's profile.
 *
 * A good job is followed through the fake block: every address register has to
 * hold the imported buffer's device address plus the job's offset, the end and
 * size the module derives have to be the checked length's, and bytes "written
 * by the block" at those addresses have to land in the caller's buffers. Then
 * every way a job can name memory or a register is tried wrong, on a job that
 * is otherwise good, with the exact errno required and the registers required
 * untouched. A job that never finishes has to end with ETIMEDOUT, a pulse of
 * the reset line and nothing left live, and the block has to run the next job.
 * A job started without waiting is collected by WAIT, which never sleeps and
 * times the job out itself. Unbinding with a session open ends it.
 *
 * Usage: lemnos_regjob_check /dev/lemnos <device> <unbind> <heap>
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <errno.h>
#include <fcntl.h>
#include <linux/dma-heap.h>
#include <pthread.h>
#include <stddef.h>
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

/* The same numbers as drivers/a733-video-sys's layout test. */
_Static_assert(sizeof(struct lemnos_regjob_open) == 24, "open");
_Static_assert(sizeof(struct lemnos_regjob_import) == 24, "import");
_Static_assert(sizeof(struct lemnos_regjob_write) == 8, "write");
_Static_assert(sizeof(struct lemnos_regjob_addr) == 24, "addr");
_Static_assert(sizeof(struct lemnos_regjob_run) == 64, "run");
_Static_assert(offsetof(struct lemnos_regjob_run, start) == 44, "run start");
_Static_assert(offsetof(struct lemnos_regjob_run, status) == 52, "run status");
_Static_assert(offsetof(struct lemnos_regjob_run, flags) == 56, "run flags");
_Static_assert(sizeof(struct lemnos_regjob_wait) == 24, "wait");
_Static_assert(offsetof(struct lemnos_regjob_wait, timestamp_ns) == 16, "wait timestamp");
_Static_assert(sizeof(struct lemnos_regjob_regs) == 32, "regs");
_Static_assert(sizeof(struct lemnos_regjob_event) == 16, "event");
_Static_assert(sizeof(struct lemnos_regjob_info) == 48, "info");
_Static_assert(LEMNOS_IOC_REGJOB_OPEN == 0xc0184cf0u, "open request");
_Static_assert(LEMNOS_REGJOB_IOC_IMPORT == 0xc0184cf1u, "import request");
_Static_assert(LEMNOS_REGJOB_IOC_RELEASE == 0x40044cf2u, "release request");
_Static_assert(LEMNOS_REGJOB_IOC_RUN == 0xc0404cf3u, "run request");
_Static_assert(LEMNOS_REGJOB_IOC_INFO == 0x80304cf4u, "info request");
_Static_assert(LEMNOS_REGJOB_IOC_WAIT == 0xc0184cf5u, "wait request");
_Static_assert(LEMNOS_REGJOB_IOC_WRITE == 0x40204cf6u, "write request");
_Static_assert(LEMNOS_REGJOB_IOC_EVENT == 0x80104cf7u, "event request");

/* The encoder's registers (kernel/lemnos_regjob_a733.c). */
#define MODE 0x800
#define TOP_RESET 0x804
#define IN_SIZE 0xa00
#define IN_STRIDE 0xa04
#define IN_CTRL 0xa08
#define IN_LUMA 0xa78
#define IN_CHROMA 0xa7c
#define PARAM 0xb04
#define QP 0xb08
#define AVC_CTRL 0xb14
#define TRIGGER 0xb18
#define STATUS 0xb1c
#define UNK_BUF 0xb60
#define VLE_ADDR 0xb80
#define VLE_END 0xb84
#define VLE_OFFSET 0xb88
#define VLE_MAX 0xb8c
#define REF_LUMA 0xba0
#define REF_CHROMA 0xba4
#define REC_LUMA 0xbb0
#define REC_CHROMA 0xbb4
#define REF_SLUMA 0xbb8
#define REC_SLUMA 0xbbc
#define MB_INFO 0xbc0

#define WINDOW 0x1000
#define SLOTS 11
#define BUF_BYTES (64u * 1024)
#define SLOT_SPACING 4096u
#define VLE_BYTES 4096u
#define MB_W 4u
#define MB_H 2u

static const uint32_t slot_regs[SLOTS] = { IN_LUMA,   IN_CHROMA,  VLE_ADDR,  REF_LUMA,
					   REF_CHROMA, REF_SLUMA, REC_LUMA,  REC_CHROMA,
					   REC_SLUMA, MB_INFO,    UNK_BUF };
/* What the profile needs for a 64x32 NV12 frame. */
static const uint64_t slot_need[SLOTS] = { 2048, 1024, 1, 2048, 2048, 512, 2048, 2048, 512,
					   128, 64 };

static int failures, ctl, heap, sess;
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

static void raise_irq(void)
{
	if (sys_put("irq", "1"))
		printf("FAIL irq\n"), failures++;
}

static void regs(uint32_t *out)
{
	char path[320];
	int fd;

	snprintf(path, sizeof(path), "%s/regs", sysdir);
	fd = open(path, O_RDONLY | O_CLOEXEC);
	if (fd < 0 || pread(fd, out, WINDOW, 0) != WINDOW)
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

/* --- the session --- */

static int heap_alloc(uint64_t len)
{
	struct dma_heap_allocation_data d = { .len = len, .fd_flags = O_RDWR | O_CLOEXEC };

	return ioctl(heap, DMA_HEAP_IOCTL_ALLOC, &d) ? -1 : (int)d.fd;
}

struct buf {
	int fd;
	uint32_t index;
	uint64_t addr, bytes;
	uint8_t *map;
};

static int import(struct buf *b, uint64_t len)
{
	struct lemnos_regjob_import imp = { 0 };

	b->fd = heap_alloc(len);
	if (b->fd < 0)
		return -1;
	imp.fd = b->fd;
	if (ioctl(sess, LEMNOS_REGJOB_IOC_IMPORT, &imp))
		return -1;
	b->index = imp.index;
	b->addr = imp.addr;
	b->bytes = imp.bytes;
	b->map = mmap(NULL, len, PROT_READ | PROT_WRITE, MAP_SHARED, b->fd, 0);
	return b->map == MAP_FAILED ? -1 : 0;
}

struct job {
	struct lemnos_regjob_write w[LEMNOS_REGJOB_MAX_WRITES + 1];
	struct lemnos_regjob_addr a[SLOTS + 1];
	uint32_t reads[8], values[8];
	struct lemnos_regjob_run run;
};

static struct buf main_buf;

/* A good 64x32 NV12 encode: each address 4 KiB apart in one buffer. */
static void good_job(struct job *j)
{
	static const struct lemnos_regjob_write w[] = {
		{ MODE, 0x80 },	       { IN_SIZE, MB_W << 16 | MB_H }, { IN_STRIDE, MB_W << 16 },
		{ IN_CTRL, 0 },	       { PARAM, 0x110 },		{ QP, 0x041e1e },
		{ VLE_OFFSET, 0 },
	};

	memset(j, 0, sizeof(*j));
	memcpy(j->w, w, sizeof(w));
	for (int k = 0; k < SLOTS; k++) {
		j->a[k].reg = slot_regs[k];
		j->a[k].buffer = main_buf.index;
		j->a[k].offset = (uint64_t)k * SLOT_SPACING;
		j->a[k].bytes = k == 2 ? VLE_BYTES : slot_need[k];
	}
	j->reads[0] = VLE_MAX;
	j->reads[1] = STATUS;
	j->run.writes = (uintptr_t)j->w;
	j->run.addrs = (uintptr_t)j->a;
	j->run.reads = (uintptr_t)j->reads;
	j->run.values = (uintptr_t)j->values;
	j->run.nwrites = sizeof(w) / sizeof(w[0]);
	j->run.naddrs = SLOTS;
	j->run.nreads = 2;
	j->run.start = 0x8;
	j->run.timeout_ms = 3000;
}

static int run(struct job *j)
{
	return ioctl(sess, LEMNOS_REGJOB_IOC_RUN, &j->run) ? errno : 0;
}

struct runner {
	pthread_t thread;
	struct job *job;
	int result;
};

static void *runner_main(void *arg)
{
	struct runner *r = arg;

	r->result = run(r->job);
	return NULL;
}

/* Waits until the start register holds the start value: everything is written by then. */
static int wait_started(void)
{
	for (int i = 0; i < 2000; i++) {
		if (reg(TRIGGER) == 0x8)
			return 0;
		sleep_ms(1);
	}
	return -1;
}

static struct lemnos_regjob_info info(void)
{
	struct lemnos_regjob_info in;

	memset(&in, 0xff, sizeof(in));
	if (ioctl(sess, LEMNOS_REGJOB_IOC_INFO, &in))
		printf("FAIL info: %s\n", strerror(errno)), failures++;
	return in;
}

/* --- the checks --- */

static void test_good_job(void)
{
	static struct job j;
	struct runner r = { .job = &j };
	int ok = 1;

	good_job(&j);
	poke(TRIGGER, 0);
	poke(STATUS, 0);
	memset(main_buf.map, 0, BUF_BYTES);
	pthread_create(&r.thread, NULL, runner_main, &r);
	check(!wait_started(), "a good job is written and started");
	for (int k = 0; k < SLOTS; k++) {
		uint64_t want = main_buf.addr + (uint64_t)k * SLOT_SPACING;

		if (reg(slot_regs[k]) != (uint32_t)want) {
			printf("     slot %x holds %x, not %llx\n", slot_regs[k], reg(slot_regs[k]),
			       (unsigned long long)want);
			ok = 0;
		}
		if (stamp(slot_regs[k], 0x40 + k))
			ok = 0;
	}
	check(ok, "every address register holds the buffer's address plus the job's offset");
	check(reg(VLE_END) == (uint32_t)(main_buf.addr + 2 * SLOT_SPACING + VLE_BYTES - 1),
	      "the bitstream's end is the module's, from the checked length");
	check(reg(VLE_MAX) == VLE_BYTES * 8, "the bitstream's size in bits is the module's");
	check(!stamp(VLE_END, 0x7e), "the block writes the bitstream's last byte");
	check((reg(AVC_CTRL) & 7) == 7, "the completion interrupt is enabled");
	check(reg(IN_SIZE) == (MB_W << 16 | MB_H) && reg(PARAM) == 0x110,
	      "the job's own registers are written");
	poke(STATUS, 1);
	raise_irq();
	pthread_join(r.thread, NULL);
	check(r.result == 0, "the job ends at its interrupt");
	check(j.run.status == 1, "with the status the interrupt gave");
	check(j.values[0] == VLE_BYTES * 8, "registers are read back after the job");
	check((reg(AVC_CTRL) & 7) == 0, "the handler turned the interrupt off");
	ok = 1;
	for (int k = 0; k < SLOTS; k++)
		ok &= main_buf.map[k * SLOT_SPACING] == 0x40 + k;
	check(ok, "what the block wrote at each address is in the caller's buffer, where the job said");
	check(main_buf.map[2 * SLOT_SPACING + VLE_BYTES - 1] == 0x7e,
	      "the bitstream's last byte is the job's last byte");
	check(info().jobs == 1 && info().live == 0, "the job is counted, and nothing is live after it");
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

static void set_write(struct job *j, uint32_t offset, uint32_t value)
{
	for (uint32_t i = 0; i < j->run.nwrites; i++) {
		if (j->w[i].offset == offset) {
			j->w[i].value = value;
			return;
		}
	}
	j->w[j->run.nwrites++] = (struct lemnos_regjob_write){ offset, value };
}

static void drop_write(struct job *j, uint32_t offset)
{
	for (uint32_t i = 0; i < j->run.nwrites; i++) {
		if (j->w[i].offset == offset) {
			j->w[i] = j->w[--j->run.nwrites];
			return;
		}
	}
}

static void test_refusals(void)
{
	static struct job j;
	struct buf other;
	uint64_t refused_before = info().refused;

	poke(TRIGGER, 0);
	/* Registers. */
	good_job(&j), set_write(&j, VLE_ADDR, (uint32_t)main_buf.addr);
	refused(&j, EPERM, "a write to an address register");
	good_job(&j), set_write(&j, IN_LUMA, 0);
	refused(&j, EPERM, "a write of 0 to an address register");
	good_job(&j), set_write(&j, VLE_END, 0xffffffff);
	refused(&j, EPERM, "a write to the bitstream's end, which the module derives");
	good_job(&j), set_write(&j, VLE_MAX, 0xffffffff);
	refused(&j, EPERM, "a write to the bitstream's size, which the module derives");
	good_job(&j), set_write(&j, AVC_CTRL, 7);
	refused(&j, EPERM, "a write to the interrupt enables, which are the module's");
	good_job(&j), set_write(&j, TOP_RESET, 1);
	refused(&j, EPERM, "a write to the top reset register");
	good_job(&j), set_write(&j, 0x100, 1);
	refused(&j, EPERM, "a write to a register outside the profile");
	good_job(&j), set_write(&j, PARAM, 0x1);
	refused(&j, EPERM, "a bit the profile doesn't let a caller set");
	good_job(&j), set_write(&j, 0x1000, 0);
	refused(&j, EINVAL, "a register past the window");
	good_job(&j), set_write(&j, 0xa02, 0);
	refused(&j, EINVAL, "a register off its boundary");
	good_job(&j), j.run.nwrites = LEMNOS_REGJOB_MAX_WRITES + 1;
	refused(&j, EINVAL, "more writes than a job may have");

	/* Addresses. */
	good_job(&j), j.run.naddrs = SLOTS - 1;
	refused(&j, EINVAL, "an address register left out");
	good_job(&j), j.a[1].reg = IN_LUMA;
	refused(&j, EINVAL, "an address register named twice");
	good_job(&j), j.a[0].reg = PARAM;
	refused(&j, EINVAL, "a register that takes no address named as one");
	good_job(&j), j.a[0].buffer = 40;
	refused(&j, ENOENT, "a buffer the session never imported");
	good_job(&j), j.a[0].buffer = LEMNOS_REGJOB_MAX_BUFFERS;
	refused(&j, ENOENT, "a buffer index past the table");
	check(!import(&other, 4096), "a second buffer imported");
	check(!ioctl(sess, LEMNOS_REGJOB_IOC_RELEASE, &other.index), "and released");
	good_job(&j), j.a[0].buffer = other.index;
	refused(&j, ENOENT, "a buffer the session released");
	good_job(&j), j.a[0].offset = BUF_BYTES - 1024, j.a[0].bytes = 2048;
	refused(&j, EFAULT, "a range running past the buffer's end");
	good_job(&j), j.a[0].offset = BUF_BYTES;
	refused(&j, EFAULT, "a range starting at the buffer's end");
	good_job(&j), j.a[0].offset = UINT64_MAX - 100, j.a[0].bytes = 4096;
	refused(&j, EFAULT, "an offset and length that wrap");
	good_job(&j), j.a[0].offset = 0, j.a[0].bytes = UINT64_MAX;
	refused(&j, EFAULT, "a length that wraps");
	good_job(&j), j.a[0].bytes = 0;
	refused(&j, EINVAL, "a length of 0");
	good_job(&j), j.a[0].offset = 16;
	refused(&j, EINVAL, "an address the register can't take (unaligned)");
	good_job(&j), j.a[0].bytes = 2047;
	refused(&j, EFAULT, "an input plane shorter than the frame the block reads");
	good_job(&j), j.a[3].bytes = 2047;
	refused(&j, EFAULT, "a reference picture shorter than the block reads");
	good_job(&j), j.a[10].bytes = 63;
	refused(&j, EFAULT, "a macroblock buffer shorter than the block writes");
	good_job(&j), set_write(&j, IN_STRIDE, 8u << 16);
	refused(&j, EFAULT, "a stride that makes the input reach past its range");
	good_job(&j), set_write(&j, IN_CTRL, 1u << 29);
	refused(&j, EFAULT, "a format whose chroma reaches past its range");
	good_job(&j), set_write(&j, VLE_OFFSET, VLE_BYTES * 8);
	refused(&j, EFAULT, "a bitstream offset at its end");

	/* Geometry. */
	good_job(&j), drop_write(&j, IN_SIZE);
	refused(&j, EINVAL, "a job without its frame size");
	good_job(&j), j.w[j.run.nwrites++] = (struct lemnos_regjob_write){ IN_SIZE, 1u << 16 | 1 };
	refused(&j, EINVAL, "a frame size written twice");
	good_job(&j), set_write(&j, IN_STRIDE, 2u << 16);
	refused(&j, EINVAL, "a stride narrower than the frame");
	good_job(&j), set_write(&j, IN_SIZE, 0);
	refused(&j, EINVAL, "an empty frame");
	good_job(&j), set_write(&j, IN_CTRL, 2u << 29);
	refused(&j, EINVAL, "an input format the profile doesn't size");
	good_job(&j), set_write(&j, MODE, 0);
	refused(&j, EINVAL, "the encoder not selected");

	/* The rest of the job. */
	good_job(&j), j.run.start = 0x1;
	refused(&j, EPERM, "a start value the profile doesn't allow");
	good_job(&j), j.run.timeout_ms = 0;
	refused(&j, EINVAL, "no timeout");
	good_job(&j), j.run.timeout_ms = LEMNOS_REGJOB_MAX_TIMEOUT_MS + 1;
	refused(&j, EINVAL, "a timeout past the limit");
	good_job(&j), j.reads[0] = 0x1000;
	refused(&j, EINVAL, "a read past the window");
	good_job(&j), j.reads[0] = 0xb05;
	refused(&j, EINVAL, "a read off its boundary");
	good_job(&j), j.run.writes = 16;
	refused(&j, EFAULT, "writes at an address the caller can't read");
	good_job(&j), j.run.flags = 2;
	refused(&j, EINVAL, "an unknown flag");
	good_job(&j), j.run.reserved = 1;
	refused(&j, EINVAL, "a reserved field set");

	check(info().refused - refused_before == 43, "every refusal is counted");
	check(info().jobs == 1, "and none ran");
	check(reg(TRIGGER) == 0, "the block was never started");
}

static void test_timeout(void)
{
	static struct job j;
	struct lemnos_regjob_info in;
	struct runner r = { .job = &j };
	int before = resets();

	good_job(&j);
	j.run.timeout_ms = 200;
	poke(TRIGGER, 0);
	poke(STATUS, 0);
	check(run(&j) == ETIMEDOUT, "a job the block never finishes ends with ETIMEDOUT");
	check(resets() == before + 1, "and the block's reset line was pulsed");
	in = info();
	check(in.timeouts == 1 && in.resets == 1 && !in.wedged && in.live == 0,
	      "the block was recovered: nothing live, not wedged");
	check((reg(AVC_CTRL) & 7) == 0, "its interrupt is off after the reset");

	/* The interrupt the block would have raised arrives late: it belongs to no job. */
	poke(STATUS, 1);
	poke(AVC_CTRL, 7);
	raise_irq();
	sleep_ms(20);

	good_job(&j);
	poke(TRIGGER, 0);
	poke(STATUS, 0);
	pthread_create(&r.thread, NULL, runner_main, &r);
	check(!wait_started(), "the next job starts on the recovered block");
	sleep_ms(50);
	check(reg(AVC_CTRL) & 7, "a late interrupt didn't finish it");
	poke(STATUS, 1);
	raise_irq();
	pthread_join(r.thread, NULL);
	check(r.result == 0 && j.run.status == 1, "and it finishes at its own interrupt");
}

static int wait_job(struct job *j, struct lemnos_regjob_wait *w)
{
	memset(w, 0, sizeof(*w));
	w->values = (uintptr_t)j->values;
	w->nreads = j->run.nreads;
	return ioctl(sess, LEMNOS_REGJOB_IOC_WAIT, w) ? errno : 0;
}

static void test_nowait(void)
{
	static struct job j, k;
	struct lemnos_regjob_wait w;
	uint64_t jobs = info().jobs;
	int before = resets();

	check(wait_job(&j, &w) == EINVAL, "nothing to collect (EINVAL)");
	good_job(&j);
	j.run.flags = LEMNOS_REGJOB_RUN_NOWAIT;
	poke(TRIGGER, 0);
	poke(STATUS, 0);
	check(run(&j) == 0 && reg(TRIGGER) == 0x8, "a job started without waiting");
	check(wait_job(&j, &w) == EAGAIN, "is not over until its interrupt (EAGAIN)");
	good_job(&k);
	check(run(&k) == EBUSY, "no other job starts before it is collected (EBUSY)");
	j.run.nreads = 1;
	check(wait_job(&j, &w) == EINVAL, "it is collected with its own reads (EINVAL)");
	j.run.nreads = 2;
	check(info().live == 1, "its buffer is live");
	poke(STATUS, 1);
	raise_irq();
	sleep_ms(20);
	j.values[0] = 0;
	check(wait_job(&j, &w) == 0 && w.status == 1 && j.values[0] == VLE_BYTES * 8,
	      "after its interrupt it is collected, with its status and reads");
	check(info().jobs == jobs + 1 && info().live == 0, "counted, and nothing is live");
	check(wait_job(&j, &w) == EINVAL, "and only once");

	good_job(&j);
	j.run.flags = LEMNOS_REGJOB_RUN_NOWAIT;
	j.run.timeout_ms = 100;
	poke(TRIGGER, 0);
	poke(STATUS, 0);
	check(run(&j) == 0, "a job started without waiting that never ends");
	check(wait_job(&j, &w) == EAGAIN && resets() == before, "runs until its time is up");
	sleep_ms(150);
	check(wait_job(&j, &w) == ETIMEDOUT && resets() == before + 1 && info().live == 0,
	      "then the collection resets the block (ETIMEDOUT) and nothing is live");
	check(wait_job(&j, &w) == EINVAL, "and there is nothing more to collect");
}

static void test_one_session(void)
{
	struct lemnos_regjob_open o = { 0 };
	__u32 block = find_block(ctl, "lemnos-test-ve");

	o.block = block;
	check(ioctl(ctl, LEMNOS_IOC_REGJOB_OPEN, &o) && errno == EBUSY,
	      "a second session on the block is refused (EBUSY)");
	o.flags = LEMNOS_REGJOB_OPEN_CONFIG;
	check(ioctl(ctl, LEMNOS_IOC_REGJOB_OPEN, &o) && errno == EINVAL,
	      "a configuration session on a profile with no such role (EINVAL)");
	o.flags = 2;
	check(ioctl(ctl, LEMNOS_IOC_REGJOB_OPEN, &o) && errno == EINVAL, "unknown flags (EINVAL)");
	check(ioctl(sess, LEMNOS_REGJOB_IOC_WRITE,
		    &(struct lemnos_regjob_regs){ 0 }) && errno == EINVAL,
	      "writes outside a job, on a profile whose registers move with its jobs (EINVAL)");
	check(ioctl(sess, LEMNOS_REGJOB_IOC_EVENT,
		    &(struct lemnos_regjob_event){ 0 }) && errno == EINVAL,
	      "events on a frames session (EINVAL)");
	check(ioctl(sess, LEMNOS_REGJOB_IOC_RELEASE, &(uint32_t){ 63 }) && errno == ENOENT,
	      "releasing an index never imported (ENOENT)");
}

static int open_session(__u32 block)
{
	struct lemnos_regjob_open o = { .block = block };

	if (ioctl(ctl, LEMNOS_IOC_REGJOB_OPEN, &o))
		return -1;
	check(o.profile == LEMNOS_REGJOB_PROFILE_A733_VE && o.window_bytes == WINDOW &&
		      o.abi == LEMNOS_REGJOB_ABI,
	      "the session runs the encoder's profile on a 4 KiB window");
	return o.fd;
}

static void test_unbind(const char *unbind, const char *device)
{
	static struct job j;
	int fd;

	fd = open(unbind, O_WRONLY | O_CLOEXEC);
	check(fd >= 0 && write(fd, device, strlen(device)) == (ssize_t)strlen(device),
	      "the block unbinds with a session open");
	if (fd >= 0)
		close(fd);
	good_job(&j);
	check(run(&j) == ENODEV, "the session's jobs fail with ENODEV after the unbind");
	check(ioctl(sess, LEMNOS_REGJOB_IOC_IMPORT, &(struct lemnos_regjob_import){ .fd = heap }) &&
		      errno == ENODEV,
	      "and its imports");
	check(!close(sess), "the session closes after the unbind");
}

int main(int argc, char **argv)
{
	if (argc != 5) {
		printf("usage: lemnos_regjob_check /dev/lemnos <device> <unbind> <heap>\n");
		return 2;
	}
	const char *device = argv[2];
	__u32 block;

	setvbuf(stdout, NULL, _IONBF, 0);
	snprintf(sysdir, sizeof(sysdir), "/sys/bus/platform/devices/%s", device);
	ctl = open(argv[1], O_RDWR | O_CLOEXEC);
	heap = open(argv[4], O_RDONLY | O_CLOEXEC);
	if (ctl < 0 || heap < 0) {
		printf("FAIL open: %s\n", strerror(errno));
		return 1;
	}
	block = find_block(ctl, device);
	if (!block) {
		printf("FAIL no block %s\n", device);
		return 1;
	}
	{
		struct lemnos_power p = { .block = block };

		check(ioctl(ctl, LEMNOS_IOC_POWER, &p) && errno == ENOTTY,
		      "the block's clocks and resets are the module's (ENOTTY)");
	}
	sess = open_session(block);
	check(sess >= 0, "a session opens");
	if (sess < 0)
		return 1;
	check(!import(&main_buf, BUF_BYTES) && main_buf.bytes == BUF_BYTES,
	      "a buffer is imported with its size");
	check(main_buf.addr && main_buf.addr + BUF_BYTES <= (1ull << 32),
	      "its address fits the block's 32-bit registers");
	test_one_session();
	test_good_job();
	test_refusals();
	test_timeout();
	test_nowait();
	check(!ioctl(sess, LEMNOS_REGJOB_IOC_RELEASE, &main_buf.index),
	      "a buffer is released once the block is done with it");
	check(!close(sess), "the session closes");
	sess = open_session(block);
	check(sess >= 0, "a new session opens after the close");
	check(!import(&main_buf, BUF_BYTES), "and imports");
	test_unbind(argv[3], device);
	printf("%s\n", failures ? "lemnos_regjob_check: FAILED" : "lemnos_regjob_check: passed");
	return failures ? 1 : 0;
}
