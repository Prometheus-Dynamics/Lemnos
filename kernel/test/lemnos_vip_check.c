// SPDX-License-Identifier: GPL-2.0
/*
 * The VIP9000's checks, with a fake block in place of the hardware (test block
 * "lemnos-test-vip", module parameter test_vip_fake_regs). The fake runs the
 * command buffer the module built, through the tables the module built, as
 * the block's MMU would: so where it could write is where the module mapped.
 *
 * Programs: every command a program may not hold is refused at load with its
 * offset. Jobs: every way a job can name memory it doesn't hold is refused,
 * ending the stream with the exact errno, before the block runs anything.
 * Then the MMU: a program that reaches past its slot, into a slot it didn't
 * name, into a slot of the job before, into its own commands, or nowhere,
 * faults; the frame is published with 0 bytes, nothing outside the job's
 * memory changed, the block is reset and the next job runs. A job that never
 * ends is stopped the same way. A block that doesn't reset is wedged.
 *
 * Usage: lemnos_vip_check /dev/lemnos <device> <heap> <run|command|board|wedge>
 *
 * command is run with the fake reporting a chip that is pointed at its tables
 * by a command. board is for a real block: the load and job refusals and a job
 * of NOPs, nothing that needs the fake's own states.
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <errno.h>
#include <fcntl.h>
#include <linux/dma-heap.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

#include "../lemnos.h"
#include "../lemnos_vip.h"
#include "find_block.h"

/* The same numbers as drivers/vip9000-sys's layout test. */
_Static_assert(sizeof(struct lemnos_vip_job) == 232, "job");
_Static_assert(offsetof(struct lemnos_vip_job, buffer) == 8, "job buffer");
_Static_assert(offsetof(struct lemnos_vip_job, program) == 12, "job program");
_Static_assert(offsetof(struct lemnos_vip_job, slots) == 16, "job slots");
_Static_assert(offsetof(struct lemnos_vip_job, slot) == 24, "job slot");
_Static_assert(sizeof(struct lemnos_vip_create) == 64, "create");
_Static_assert(offsetof(struct lemnos_vip_create, chip) == 56, "create chip");
_Static_assert(offsetof(struct lemnos_vip_create, mmu) == 60, "create mmu");
_Static_assert(sizeof(struct lemnos_vip_load) == 24, "load");
_Static_assert(offsetof(struct lemnos_vip_load, bytes) == 8, "load bytes");
_Static_assert(offsetof(struct lemnos_vip_load, program) == 12, "load program");
_Static_assert(offsetof(struct lemnos_vip_load, refused_at) == 16, "load refused_at");
_Static_assert(sizeof(struct lemnos_vip_unload) == 8, "unload");
_Static_assert(sizeof(struct lemnos_vip_status) == 48, "status");
_Static_assert(offsetof(struct lemnos_vip_status, finished) == 8, "status finished");
_Static_assert(offsetof(struct lemnos_vip_status, refused) == 16, "status refused");
_Static_assert(offsetof(struct lemnos_vip_status, faults) == 24, "status faults");
_Static_assert(offsetof(struct lemnos_vip_status, timeouts) == 32, "status timeouts");
_Static_assert(offsetof(struct lemnos_vip_status, last_fault) == 40, "status last_fault");
_Static_assert(offsetof(struct lemnos_vip_status, resets) == 44, "status resets");
_Static_assert(LEMNOS_IOC_VIP_CREATE == 0xc0404ce0u, "create request");
_Static_assert(LEMNOS_VIP_IOC_LOAD == 0xc0184ce1u, "load request");
_Static_assert(LEMNOS_VIP_IOC_UNLOAD == 0x40084ce2u, "unload request");
_Static_assert(LEMNOS_VIP_IOC_STATUS == 0x80304ce3u, "status request");
_Static_assert(LEMNOS_VIP_SLOT_ADDR(12) == 0xd0000000u, "last slot");

#define BUFFERS 2u
#define BUFFER_BYTES 4096u
#define FRAME_SLOTS 2u
#define JOB_SLOTS 2u
#define JOB_BYTES ((uint32_t)sizeof(struct lemnos_vip_job))
#define REGION_BYTES (1u << 20)
#define PAGE 4096u
/* The job's slots in the region: 0 at its start, 1 at 64K, 2 at 128K; two pages each. */
#define SLOT_AT(n) ((uint64_t)(n) * 0x10000u)
#define SLOT_BYTES (2 * PAGE)

/* Commands (the encodings lemnos_vip.c cites). */
#define STATE(addr, n) (1u << 27 | (uint32_t)(n) << 16 | (addr))
#define NOP (3u << 27)
#define END (2u << 27)
#define LINK (8u << 27)
#define STALL (9u << 27)
/* The fake's own states: source, destination, bytes (copies); hang. */
#define F_COPY 0x5000u
#define F_HANG 0x5003u

static int failures, ctl_fd, heap_fd, board;
static uint32_t block_id;
static char sysdir[256];

static void check(int ok, const char *what, int line)
{
	if (!ok) {
		printf("FAIL %s (line %d)\n", what, line);
		failures++;
	}
}

#define CHECK(c) check((c), #c, __LINE__)

static int heap_alloc(uint64_t len)
{
	struct dma_heap_allocation_data d = { .len = len, .fd_flags = O_RDWR | O_CLOEXEC };

	return ioctl(heap_fd, DMA_HEAP_IOCTL_ALLOC, &d) ? -1 : (int)d.fd;
}

static int put_sys(const char *name, const char *text)
{
	char path[320];
	int fd, ok;

	snprintf(path, sizeof(path), "%s/%s", sysdir, name);
	fd = open(path, O_WRONLY | O_CLOEXEC);
	ok = fd >= 0 && write(fd, text, strlen(text)) == (ssize_t)strlen(text);
	if (fd >= 0)
		close(fd);
	return ok ? 0 : -1;
}

/* What the fake has run: jobs (MMU on), faults, resets, TLB flushes, bytes copied, bad commands. */
struct ran {
	unsigned int jobs, faults, resets, flushes, copied, errors;
};

static struct ran ran(void)
{
	struct ran r;
	char path[320], text[128];
	int fd;
	ssize_t n;

	memset(&r, 0xff, sizeof(r));
	if (board)
		return r;
	snprintf(path, sizeof(path), "%s/ran", sysdir);
	fd = open(path, O_RDONLY | O_CLOEXEC);
	n = fd < 0 ? -1 : read(fd, text, sizeof(text) - 1);
	if (fd >= 0)
		close(fd);
	if (n > 0) {
		text[n] = 0;
		sscanf(text, "%u %u %u %u %u %u", &r.jobs, &r.faults, &r.resets, &r.flushes,
		       &r.copied, &r.errors);
	}
	return r;
}

struct stream {
	int fd, pool_fd, region_fd;
	uint64_t *ring;
	size_t ring_bytes;
	uint64_t pool_addr, region_addr;
	uint32_t chip, mmu;
	uint8_t *image;			/* the region, mapped */
	size_t returns_at, jobs_at, job_words;
	uint64_t jobs_head, frames_tail, returns_head;
};

static int stream_open(struct stream *st)
{
	struct lemnos_vip_create c;
	struct lemnos_stream_register reg;

	memset(st, 0, sizeof(*st));
	st->fd = st->pool_fd = st->region_fd = -1;
	st->pool_fd = heap_alloc((uint64_t)BUFFERS * BUFFER_BYTES);
	st->region_fd = heap_alloc(REGION_BYTES);
	if (st->pool_fd < 0 || st->region_fd < 0)
		return -1;
	memset(&c, 0, sizeof(c));
	c.stream.block = block_id;
	c.stream.buffers = BUFFERS;
	c.stream.buffer_bytes = BUFFER_BYTES;
	c.stream.frame_slots = FRAME_SLOTS;
	c.stream.job_slots = JOB_SLOTS;
	c.stream.job_bytes = JOB_BYTES;
	c.stream.pool_fd = st->pool_fd;
	c.stream.doorbell_fd = -1;
	if (ioctl(ctl_fd, LEMNOS_IOC_VIP_CREATE, &c))
		return -1;
	st->fd = c.stream.fd;
	st->ring_bytes = c.stream.ring_bytes;
	st->pool_addr = c.stream.pool_addr;
	st->chip = c.chip;
	st->mmu = c.mmu;
	st->ring = mmap(NULL, st->ring_bytes, PROT_READ | PROT_WRITE, MAP_SHARED, st->fd, 0);
	st->image = mmap(NULL, REGION_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, st->region_fd, 0);
	if (st->ring == MAP_FAILED || st->image == MAP_FAILED)
		return -1;
	st->returns_at = LEMNOS_RING_FRAMES + FRAME_SLOTS * LEMNOS_STREAM_FRAME_WORDS;
	st->jobs_at = st->returns_at + FRAME_SLOTS;
	st->job_words = 1 + (JOB_BYTES + 7) / 8;

	memset(&reg, 0, sizeof(reg));
	reg.fd = st->region_fd;
	if (ioctl(st->fd, LEMNOS_STREAM_IOC_REGISTER, &reg))
		return -1;
	st->region_addr = reg.addr;
	return 0;
}

static void stream_close(struct stream *st)
{
	if (st->ring && st->ring != MAP_FAILED)
		munmap(st->ring, st->ring_bytes);
	if (st->image && st->image != MAP_FAILED)
		munmap(st->image, REGION_BYTES);
	if (st->fd >= 0)
		close(st->fd);
	if (st->region_fd >= 0)
		close(st->region_fd);
	if (st->pool_fd >= 0)
		close(st->pool_fd);
	memset(st, 0, sizeof(*st));
}

/* 0 or the errno; the program's number, or the offset refused. */
static int load(struct stream *st, const uint32_t *words, uint32_t bytes, uint32_t *out)
{
	struct lemnos_vip_load a = { .data = (uintptr_t)words, .bytes = bytes };
	int r = ioctl(st->fd, LEMNOS_VIP_IOC_LOAD, &a) ? errno : 0;

	if (out)
		*out = r ? a.refused_at : a.program;
	return r;
}

static int unload(struct stream *st, uint32_t program)
{
	struct lemnos_vip_unload a = { .program = program };

	return ioctl(st->fd, LEMNOS_VIP_IOC_UNLOAD, &a) ? errno : 0;
}

static void post(struct stream *st, const struct lemnos_vip_job *job)
{
	size_t at = st->jobs_at + (st->jobs_head & (JOB_SLOTS - 1)) * st->job_words;
	uint64_t words[(sizeof(*job) + 7) / 8] = { 0 };

	memcpy(words, job, sizeof(*job));
	st->ring[at] = sizeof(*job);
	for (size_t i = 0; i < sizeof(words) / 8; i++)
		st->ring[at + 1 + i] = words[i];
	st->jobs_head++;
	__atomic_thread_fence(__ATOMIC_RELEASE);
	__atomic_store_n(&st->ring[LEMNOS_RING_JOBS_HEAD], st->jobs_head, __ATOMIC_RELEASE);
}

static int kick(struct stream *st)
{
	return ioctl(st->fd, LEMNOS_STREAM_IOC_KICK) ? errno : 0;
}

static struct lemnos_stream_status status_of(struct stream *st)
{
	struct lemnos_stream_status s;

	memset(&s, 0xff, sizeof(s));
	ioctl(st->fd, LEMNOS_STREAM_IOC_STATUS, &s);
	return s;
}

static struct lemnos_vip_status block_status(struct stream *st)
{
	struct lemnos_vip_status s;

	memset(&s, 0xff, sizeof(s));
	ioctl(st->fd, LEMNOS_VIP_IOC_STATUS, &s);
	return s;
}

/* The next frame's bytes, its identity, and its buffer given back; -1 if none comes. */
static long wait_frame(struct stream *st, uint64_t *identity, int ms)
{
	for (int t = 0; t < ms; t += 2) {
		uint64_t head = __atomic_load_n(&st->ring[LEMNOS_RING_FRAMES_HEAD], __ATOMIC_ACQUIRE);

		if (head != st->frames_tail) {
			size_t at = LEMNOS_RING_FRAMES +
				    (st->frames_tail & (FRAME_SLOTS - 1)) * LEMNOS_STREAM_FRAME_WORDS;
			uint64_t word = st->ring[at], fence = st->ring[at + 4];
			size_t ret_at = st->returns_at + (st->returns_head & (FRAME_SLOTS - 1));

			if (identity)
				*identity = st->ring[at + 2];
			st->frames_tail++;
			__atomic_store_n(&st->ring[LEMNOS_RING_FRAMES_TAIL], st->frames_tail,
					 __ATOMIC_RELEASE);
			st->ring[ret_at] = (uint32_t)word | fence << 32;
			st->returns_head++;
			__atomic_store_n(&st->ring[LEMNOS_RING_RETURNS_HEAD], st->returns_head,
					 __ATOMIC_RELEASE);
			return (long)(word >> 32);
		}
		usleep(2000);
	}
	return -1;
}

/* A program copying `len` bytes from block address `src` to `dst`, through the fake's states. */
static uint32_t copy_prog(uint32_t *w, uint32_t src, uint32_t dst, uint32_t len)
{
	w[0] = STATE(F_COPY, 3);
	w[1] = src;
	w[2] = dst;
	w[3] = len;
	w[4] = NOP;
	w[5] = 0;
	return 6 * 4;
}

/* A job of program `program`, with `slots` slots of SLOT_BYTES at SLOT_AT(n) in the region. */
static struct lemnos_vip_job job_of(const struct stream *st, uint32_t program, uint32_t slots,
				    uint64_t identity)
{
	struct lemnos_vip_job j = { .identity = identity, .program = program, .slots = slots };

	for (uint32_t i = 0; i < slots; i++) {
		j.slot[i].addr = st->region_addr + SLOT_AT(i);
		j.slot[i].len = SLOT_BYTES;
	}
	return j;
}

static void fill(struct stream *st, uint8_t seed)
{
	for (uint32_t i = 0; i < REGION_BYTES; i++)
		st->image[i] = (uint8_t)(seed + i * 7 + (i >> 9));
}

/* --- load --- */

static void run_load(void)
{
	struct stream st;
	uint32_t w[64], out, at;
	int got;

	if (stream_open(&st)) {
		printf("FAIL stream: %s\n", strerror(errno));
		failures++;
		return;
	}
	printf("chip 0x%08x, MMU set up by %s\n", st.chip,
	       st.mmu == LEMNOS_VIP_MMU_PD ? "register" : "command");
	CHECK(st.chip != 0);
	CHECK(st.mmu == LEMNOS_VIP_MMU_PD || st.mmu == LEMNOS_VIP_MMU_COMMAND);

	/* Each refused at its offset: after one good NOP. */
	static const struct {
		uint32_t a, b;
		int err;
		const char *what;
	} bad[] = {
		{ LINK, 0x00100000, EPERM, "LINK" },
		{ END, 0, EPERM, "END" },
		{ 7u << 27, 0, EPERM, "WAIT" },
		{ 10u << 27, 0, EPERM, "CALL" },
		{ 11u << 27, 0, EPERM, "RETURN" },
		{ 13u << 27, 0, EPERM, "CHIP_ENABLE" },
		{ STATE(0x0061, 1), 0xffffffff, EPERM, "the MMU's control state" },
		{ STATE(0x006b, 1), 0, EPERM, "the page table array's state" },
		{ STATE(0x03ff, 2), 0, EPERM, "a load crossing into the control states" },
		{ STATE(0x0e01, 1), 4, EPERM, "the module's event" },
		{ STATE(0x0e00, 2), 0, EPERM, "a load covering the module's event" },
		{ STATE(0x0e12, 1), 0, EPERM, "the MMU's set-up state" },
		{ STATE(0x0e10, 3), 0, EPERM, "a load covering the MMU's set-up state" },
		{ STATE(0xffff, 2), 0, EPERM, "a load past the last state" },
		{ STATE(0x0e00, 0), 0, EPERM, "1024 states: through the event" },
		{ STATE(0x5000, 3), 0, EINVAL, "a load running past the program" },
	};
	for (size_t i = 0; i < sizeof(bad) / sizeof(bad[0]); i++) {
		w[0] = NOP;
		w[1] = 0;
		w[2] = bad[i].a;
		w[3] = bad[i].b;
		got = load(&st, w, 16, &at);
		if (got != bad[i].err || at != 8) {
			printf("FAIL load %s: %s at %u, wanted %s at 8\n", bad[i].what,
			       strerror(got), at, strerror(bad[i].err));
			failures++;
		}
	}
	CHECK(load(&st, w, 0, NULL) == EINVAL);
	CHECK(load(&st, w, 12, NULL) == EINVAL);
	CHECK(load(&st, w, LEMNOS_VIP_MAX_PROGRAM_BYTES + 8, NULL) == EINVAL);
	CHECK(load(&st, NULL, 8, NULL) == EFAULT);
	{
		struct lemnos_vip_load a = { .data = (uintptr_t)w, .bytes = 8, .reserved = 1 };

		CHECK(ioctl(st.fd, LEMNOS_VIP_IOC_LOAD, &a) && errno == EINVAL);
	}

	/* What may run: state loads past the control states, NOP, STALL. */
	w[0] = STATE(0x0400, 1);
	w[1] = 0;
	w[2] = STATE(0x0e02, 1);	/* the semaphore, next to the event */
	w[3] = 0x701;
	w[4] = STALL;
	w[5] = 0x701;
	w[6] = STATE(0x0e13, 3);	/* the states after the MMU's set-up, padded */
	w[7] = 1;
	w[8] = 2;
	w[9] = 3;
	w[10] = NOP;
	w[11] = 0;
	CHECK(load(&st, w, 48, &out) == 0);
	CHECK(out >= 1 && out <= LEMNOS_VIP_PROGRAMS);

	/* At most LEMNOS_VIP_PROGRAMS at once. */
	for (uint32_t i = 1; i < LEMNOS_VIP_PROGRAMS; i++)
		CHECK(load(&st, w, 8, NULL) == 0);
	CHECK(load(&st, w, 8, NULL) == EBUSY);
	CHECK(unload(&st, out) == 0);
	CHECK(unload(&st, out) == ENOENT);
	CHECK(unload(&st, 0) == EINVAL);
	CHECK(unload(&st, LEMNOS_VIP_PROGRAMS + 1) == EINVAL);
	CHECK(load(&st, w, 8, &at) == 0);
	CHECK(at == out);
	CHECK(status_of(&st).state == LEMNOS_RING_STATE_OPEN);
	stream_close(&st);
}

/* --- refused jobs --- */

static void e_unregistered(struct stream *st, struct lemnos_vip_job *j)
{
	j->slot[1].addr = st->region_addr + REGION_BYTES;
}

static void e_past_region(struct stream *st, struct lemnos_vip_job *j)
{
	j->slot[1].addr = st->region_addr + REGION_BYTES - PAGE;
}

static void e_wraps(struct stream *st, struct lemnos_vip_job *j)
{
	j->slot[1].addr = ~(uint64_t)0 - PAGE + 1;
}

static void e_pool(struct stream *st, struct lemnos_vip_job *j)
{
	j->slot[1].addr = st->pool_addr;
	j->slot[1].len = PAGE;
}

static void e_unaligned(struct stream *st, struct lemnos_vip_job *j)
{
	j->slot[1].addr += 64;
}

static void e_part_page(struct stream *st, struct lemnos_vip_job *j)
{
	j->slot[1].len = PAGE + 64;
}

static void e_empty(struct stream *st, struct lemnos_vip_job *j)
{
	j->slot[1].len = 0;
}

static void e_too_long(struct stream *st, struct lemnos_vip_job *j)
{
	j->slot[1].len = LEMNOS_VIP_SLOT_SPAN + (uint64_t)PAGE;
}

static void e_too_many(struct stream *st, struct lemnos_vip_job *j)
{
	j->slots = LEMNOS_VIP_SLOTS + 1;
}

static void e_unused_slot(struct stream *st, struct lemnos_vip_job *j)
{
	j->slot[5].addr = st->region_addr;
}

static void e_reserved(struct stream *st, struct lemnos_vip_job *j)
{
	j->reserved = 1;
}

static void e_no_program(struct stream *st, struct lemnos_vip_job *j)
{
	j->program = 0;
}

static void e_not_loaded(struct stream *st, struct lemnos_vip_job *j)
{
	j->program = LEMNOS_VIP_PROGRAMS;
}

static void e_bad_buffer(struct stream *st, struct lemnos_vip_job *j)
{
	j->buffer = BUFFERS;
}

/* A job that must be refused with `err`, ending its stream, on a stream of its own. */
static void refused(int err, const char *what, void (*edit)(struct stream *, struct lemnos_vip_job *))
{
	struct stream st;
	struct lemnos_vip_job job;
	uint32_t w[8], prog;
	struct ran before, after;
	struct lemnos_vip_status b0;
	int got;

	if (stream_open(&st)) {
		printf("FAIL %s: stream (%s)\n", what, strerror(errno));
		failures++;
		stream_close(&st);
		return;
	}
	w[0] = NOP;
	w[1] = 0;
	CHECK(load(&st, w, 8, &prog) == 0);
	job = job_of(&st, prog, 2, 0x77);
	edit(&st, &job);
	before = ran();
	b0 = block_status(&st);
	post(&st, &job);
	got = kick(&st);
	if (got != err) {
		printf("FAIL %s: %s, wanted %s\n", what, strerror(got), strerror(err));
		failures++;
	}
	CHECK(status_of(&st).state == LEMNOS_RING_STATE_FAULT);
	CHECK(status_of(&st).fault == err);
	usleep(5000);
	/* Nothing reached the block. */
	CHECK(block_status(&st).finished == b0.finished);
	CHECK(block_status(&st).refused == b0.refused + 1);
	after = ran();
	CHECK(after.jobs == before.jobs);
	stream_close(&st);
}

static void run_refusals(void)
{
	refused(EFAULT, "a slot outside every registered buffer", e_unregistered);
	refused(EFAULT, "a slot running past its buffer", e_past_region);
	refused(EOVERFLOW, "a slot whose end wraps", e_wraps);
	refused(EACCES, "a slot in the pool", e_pool);
	refused(EINVAL, "a slot not on a page", e_unaligned);
	refused(EINVAL, "a slot not whole pages", e_part_page);
	refused(EINVAL, "an empty slot", e_empty);
	refused(EINVAL, "a slot longer than its span", e_too_long);
	refused(EINVAL, "more slots than there are", e_too_many);
	refused(EINVAL, "an unused slot not zero", e_unused_slot);
	refused(EINVAL, "a reserved field not zero", e_reserved);
	refused(EINVAL, "program 0", e_no_program);
	refused(EINVAL, "a program not loaded", e_not_loaded);
	refused(EINVAL, "a pool buffer that isn't one", e_bad_buffer);
}

/* --- jobs on the block --- */

/* Runs `job`; the frame's bytes (-1: none came). */
static long run_job(struct stream *st, const struct lemnos_vip_job *job, int ms)
{
	uint64_t id = 0;
	long n;

	post(st, job);
	if (kick(st)) {
		printf("FAIL kick: %s\n", strerror(errno));
		failures++;
		return -1;
	}
	n = wait_frame(st, &id, ms);
	CHECK(id == job->identity);
	return n;
}

/* A program copying from `src` to `dst` that must fault, with the region otherwise untouched. */
static void faults(struct stream *st, uint32_t slots, uint32_t src, uint32_t dst, uint32_t len,
		   const char *what)
{
	static uint8_t before[REGION_BYTES];
	uint32_t w[8], prog;
	struct lemnos_vip_status b0 = block_status(st), b1;
	struct ran r0 = ran(), r1;
	long n;

	fill(st, 0x5a);
	memcpy(before, st->image, REGION_BYTES);
	CHECK(load(st, w, copy_prog(w, src, dst, len), &prog) == 0);
	struct lemnos_vip_job j = job_of(st, prog, slots, 0xf00 + b0.faults);

	n = run_job(st, &j, 3000);
	b1 = block_status(st);
	r1 = ran();
	if (n != 0 || b1.faults != b0.faults + 1 || r1.faults != r0.faults + 1) {
		printf("FAIL %s: frame %ld, faults %llu -> %llu, fake faults %u -> %u\n", what, n,
		       (unsigned long long)b0.faults, (unsigned long long)b1.faults, r0.faults,
		       r1.faults);
		failures++;
	}
	CHECK(b1.last_fault & (1u << 30));
	CHECK(b1.resets > b0.resets);
	CHECK(b1.wedged == 0);
	CHECK(status_of(st).state == LEMNOS_RING_STATE_OPEN);
	/* Whatever the fake reached before the fault, nothing outside the job's slots changed. */
	for (uint32_t i = 0; i < REGION_BYTES; i++) {
		int in_slot = 0;

		for (uint32_t s = 0; s < slots; s++)
			in_slot |= i >= SLOT_AT(s) && i < SLOT_AT(s) + SLOT_BYTES;
		if (!in_slot && st->image[i] != before[i]) {
			printf("FAIL %s: byte %u of the region changed\n", what, i);
			failures++;
			break;
		}
	}
	CHECK(unload(st, prog) == 0);
}

/* A good copy: slot 0's first `len` bytes to slot 1, checked byte by byte. */
static void good_copy(struct stream *st, uint32_t len, uint64_t identity)
{
	uint32_t w[8], prog;
	struct lemnos_vip_status b0 = block_status(st);
	struct ran r0 = ran(), r1;
	struct lemnos_vip_job j;
	long n;

	fill(st, (uint8_t)identity);
	CHECK(load(st, w, copy_prog(w, LEMNOS_VIP_SLOT_ADDR(0), LEMNOS_VIP_SLOT_ADDR(1), len),
		   &prog) == 0);
	j = job_of(st, prog, 2, identity);
	n = run_job(st, &j, 3000);
	if (n != BUFFER_BYTES)
		printf("FAIL good copy %llu: frame %ld\n", (unsigned long long)identity, n), failures++;
	CHECK(block_status(st).finished == b0.finished + 1);
	CHECK(!memcmp(st->image + SLOT_AT(1), st->image + SLOT_AT(0), len));
	r1 = ran();
	CHECK(r1.jobs == r0.jobs + 1);
	CHECK(r1.copied == r0.copied + len);
	/* The module's preamble flushed the TLB first. */
	CHECK(r1.flushes == r0.flushes + 1);
	CHECK(r1.errors == 0);
	CHECK(unload(st, prog) == 0);
}

static void run_jobs(int command)
{
	struct stream st;
	uint32_t w[8], prog;
	struct lemnos_vip_job j;
	long n;

	if (stream_open(&st)) {
		printf("FAIL stream: %s\n", strerror(errno));
		failures++;
		return;
	}
	CHECK(board || st.mmu == (command ? LEMNOS_VIP_MMU_COMMAND : LEMNOS_VIP_MMU_PD));

	/* A job of NOPs: what a real block can run too. */
	w[0] = NOP;
	w[1] = 0;
	w[2] = STALL;
	w[3] = 0x701;
	CHECK(load(&st, w, 16, &prog) == 0);
	j = job_of(&st, prog, 1, 1);
	CHECK(run_job(&st, &j, 3000) == BUFFER_BYTES);
	CHECK(block_status(&st).finished >= 1);
	CHECK(unload(&st, prog) == 0);
	if (board)
		goto one_stream;

	good_copy(&st, PAGE, 2);
	good_copy(&st, SLOT_BYTES, 3);	/* across a page of each slot */

	faults(&st, 2, LEMNOS_VIP_SLOT_ADDR(0), LEMNOS_VIP_SLOT_ADDR(1) + SLOT_BYTES, 64,
	       "a write past the slot's end");
	good_copy(&st, PAGE, 4);	/* recovered */
	faults(&st, 2, LEMNOS_VIP_SLOT_ADDR(0), LEMNOS_VIP_SLOT_ADDR(1) + PAGE, PAGE + 64,
	       "a write running off the slot's end");
	faults(&st, 2, LEMNOS_VIP_SLOT_ADDR(0) - PAGE, LEMNOS_VIP_SLOT_ADDR(1), 64,
	       "a read before the first slot");
	faults(&st, 2, LEMNOS_VIP_SLOT_ADDR(0), LEMNOS_VIP_SLOT_ADDR(2), 64,
	       "a write to a slot the job didn't name");
	faults(&st, 2, LEMNOS_VIP_SLOT_ADDR(0), LEMNOS_VIP_PROGRAM_ADDR, 64,
	       "a write to the job's own commands");
	faults(&st, 2, LEMNOS_VIP_SLOT_ADDR(0), 0xf0000000u, 64, "a write to nothing mapped");
	faults(&st, 2, 0x00000000u, LEMNOS_VIP_SLOT_ADDR(1), 64, "a read of address 0");

	/* The job before named slot 2; this one doesn't, and can't reach it. */
	CHECK(load(&st, w, copy_prog(w, LEMNOS_VIP_SLOT_ADDR(0), LEMNOS_VIP_SLOT_ADDR(2), PAGE),
		   &prog) == 0);
	j = job_of(&st, prog, 3, 5);
	n = run_job(&st, &j, 3000);
	CHECK(n == BUFFER_BYTES);
	CHECK(!memcmp(st.image + SLOT_AT(2), st.image + SLOT_AT(0), PAGE));
	CHECK(unload(&st, prog) == 0);
	faults(&st, 2, LEMNOS_VIP_SLOT_ADDR(0), LEMNOS_VIP_SLOT_ADDR(2), 64,
	       "a write to the slot of the job before");
	faults(&st, 1, LEMNOS_VIP_SLOT_ADDR(1), LEMNOS_VIP_SLOT_ADDR(0), 64,
	       "a read of the slot of the job before");

	/* A job that never ends: stopped by a reset, published empty, and the stream goes on. */
	{
		struct lemnos_vip_status b0 = block_status(&st);

		w[0] = STATE(F_HANG, 1);
		w[1] = 0;
		CHECK(load(&st, w, 8, &prog) == 0);
		j = job_of(&st, prog, 1, 6);
		CHECK(run_job(&st, &j, 5000) == 0);
		CHECK(block_status(&st).timeouts == b0.timeouts + 1);
		CHECK(block_status(&st).resets > b0.resets);
		CHECK(block_status(&st).wedged == 0);
		CHECK(status_of(&st).state == LEMNOS_RING_STATE_OPEN);
		CHECK(unload(&st, prog) == 0);
	}
	good_copy(&st, PAGE, 7);
	/* Unloaded programs are gone. */
	{
		struct lemnos_vip_job k = job_of(&st, 1, 1, 8);

		post(&st, &k);
		CHECK(kick(&st) == EINVAL);
	}

one_stream:
	/* One stream at a time. */
	{
		struct stream second;

		CHECK(stream_open(&second) && errno == EBUSY);
		stream_close(&second);
	}
	stream_close(&st);
	/* Closed: the block takes a stream again, with its programs gone. */
	CHECK(!stream_open(&st));
	if (!board)
		good_copy(&st, PAGE, 9);
	stream_close(&st);
}

/* A block that doesn't reset: wedged, and its job's memory stays held. */
static void run_wedge(void)
{
	struct stream st;
	struct lemnos_vip_job j;
	uint32_t w[2] = { STATE(F_HANG, 1), 0 }, prog;
	int region;

	if (stream_open(&st)) {
		printf("FAIL stream: %s\n", strerror(errno));
		failures++;
		return;
	}
	CHECK(load(&st, w, 8, &prog) == 0);
	CHECK(!put_sys("stuck", "1"));
	j = job_of(&st, prog, 1, 1);
	post(&st, &j);
	CHECK(kick(&st) == 0);
	for (int t = 0; t < 400 && status_of(&st).state == LEMNOS_RING_STATE_OPEN; t++)
		usleep(5000);
	{
		struct lemnos_vip_status b = block_status(&st);
		struct lemnos_stream_status s = status_of(&st);

		if (s.state != LEMNOS_RING_STATE_FAULT || s.fault != EIO || b.wedged != 1)
			printf("FAIL not wedged: state %u fault %d; wedged %u timeouts %llu\n",
			       s.state, s.fault, b.wedged, (unsigned long long)b.timeouts),
				failures++;
	}
	region = dup(st.region_fd);
	stream_close(&st);
	{
		/* The stream is gone, its memory isn't: the kernel still holds the region. */
		char path[64], text[256];
		int f;
		ssize_t n;

		snprintf(path, sizeof(path), "/proc/self/fdinfo/%d", region);
		f = open(path, O_RDONLY | O_CLOEXEC);
		n = f < 0 ? -1 : read(f, text, sizeof(text) - 1);
		if (f >= 0)
			close(f);
		text[n > 0 ? n : 0] = 0;
		char *p = strstr(text, "count:");

		CHECK(p && strtol(p + 6, NULL, 10) > 1);
	}
	close(region);
	CHECK(stream_open(&st) && errno == EIO);
	stream_close(&st);
}

int main(int argc, char **argv)
{
	if (argc != 5) {
		fprintf(stderr, "usage: %s /dev/lemnos <device> <heap> <run|command|board|wedge>\n",
			argv[0]);
		return 2;
	}
	ctl_fd = open(argv[1], O_RDWR | O_CLOEXEC);
	heap_fd = open(argv[3], O_RDWR | O_CLOEXEC);
	snprintf(sysdir, sizeof(sysdir), "/sys/bus/platform/devices/%s", argv[2]);
	if (ctl_fd < 0 || heap_fd < 0) {
		perror("open");
		return 1;
	}
	block_id = find_block(ctl_fd, argv[2]);
	if (!block_id) {
		printf("FAIL no block %s\n", argv[2]);
		return 1;
	}
	board = !strcmp(argv[4], "board");
	if (!strcmp(argv[4], "run") || board) {
		run_load();
		run_refusals();
		run_jobs(0);
	} else if (!strcmp(argv[4], "command")) {
		run_jobs(1);
	} else if (!strcmp(argv[4], "wedge")) {
		run_wedge();
	} else {
		return 2;
	}
	printf("lemnos_vip_check %s: %d failed\n", argv[4], failures);
	return failures ? 1 : 0;
}
