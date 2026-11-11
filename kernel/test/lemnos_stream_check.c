// SPDX-License-Identifier: GPL-2.0
/*
 * Frame streams: frames round trip over the ring with the test source, and
 * every way a consumer can lie about the ring or an address is refused with
 * an errno, faulting only its own stream, with nothing left held. The same
 * for a stream whose buffers are dma-bufs of their own
 * (LEMNOS_STREAM_CREATE_BUFFERS): each frame lands in its buffer's own memory,
 * a buffer without memory is never filled, memory swaps only while the block
 * can't be writing it, and every pin ends with the stream.
 *
 * Usage: lemnos_stream_check /dev/lemnos <device name> <unbind path> <heap> [no-source]
 *   no-source: the module has no test_stream_source. Only the checks that
 *   need no frames run. Otherwise the block ends up unbound.
 * Copyright (c) 2026 the Lemnos authors.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/dma-heap.h>
#include <poll.h>
#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/eventfd.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <unistd.h>

#include "../lemnos.h"
#include "find_block.h"

#define BUFFERS 4u
#define BUFFER_BYTES 8192u
#define STRIDE 8192u
#define POOL_BYTES ((uint64_t)BUFFERS * STRIDE)
#define FRAME_SLOTS 8u
#define JOB_SLOTS 4u
#define JOB_BYTES ((uint32_t)sizeof(struct lemnos_stream_job))
#define AUX_BYTES (16384u)

static int failures;

#define CHECK(cond, ...)                                                   \
	do {                                                               \
		int ok_ = !!(cond);                                        \
		printf(ok_ ? "ok   " : "FAIL ");                           \
		failures += !ok_;                                          \
		printf(__VA_ARGS__);                                       \
		printf("\n");                                              \
	} while (0)

struct stream {
	int fd, doorbell;
	volatile uint64_t *r;
	uint64_t ring_bytes, pool_addr;
	size_t returns_at, jobs_at, job_words;
	uint64_t frames_tail, returns_head, jobs_head;
};

struct frame {
	uint32_t buffer, bytes, fence;
	uint64_t sequence, identity;
};

static uint64_t load(const struct stream *s, size_t at)
{
	return __atomic_load_n(&s->r[at], __ATOMIC_ACQUIRE);
}

static void store(const struct stream *s, size_t at, uint64_t v)
{
	__atomic_store_n(&s->r[at], v, __ATOMIC_RELEASE);
}

static struct lemnos_stream_create good_layout(uint32_t block, int pool_fd, int doorbell)
{
	return (struct lemnos_stream_create){
		.block = block,
		.buffers = BUFFERS,
		.buffer_bytes = BUFFER_BYTES,
		.frame_slots = FRAME_SLOTS,
		.job_slots = JOB_SLOTS,
		.job_bytes = JOB_BYTES,
		.pool_fd = pool_fd,
		.doorbell_fd = doorbell,
	};
}

static int stream_make(struct stream *s, int ctl, struct lemnos_stream_create c)
{
	memset(s, 0, sizeof(*s));
	s->fd = -1;
	s->doorbell = c.doorbell_fd;
	if (ioctl(ctl, LEMNOS_IOC_STREAM_CREATE, &c))
		return -errno;
	s->fd = c.fd;
	s->ring_bytes = c.ring_bytes;
	s->pool_addr = c.pool_addr;
	s->returns_at = LEMNOS_RING_FRAMES + FRAME_SLOTS * LEMNOS_STREAM_FRAME_WORDS;
	s->jobs_at = s->returns_at + FRAME_SLOTS;
	s->job_words = 1 + (JOB_BYTES + 7) / 8;
	s->r = mmap(NULL, s->ring_bytes, PROT_READ | PROT_WRITE, MAP_SHARED, c.fd, 0);
	if (s->r == MAP_FAILED) {
		s->r = NULL;
		close(c.fd);
		s->fd = -1;
		return -errno;
	}
	return 0;
}

static int stream_open(struct stream *s, int ctl, uint32_t block, int pool_fd, int doorbell)
{
	return stream_make(s, ctl, good_layout(block, pool_fd, doorbell));
}

/* A stream with no pool: each buffer gets a dma-buf of its own. */
static int own_open(struct stream *s, int ctl, uint32_t block)
{
	struct lemnos_stream_create c = good_layout(block, -1, -1);

	c.flags = LEMNOS_STREAM_CREATE_BUFFERS;
	return stream_make(s, ctl, c);
}

static int set_buffer(const struct stream *s, uint32_t buffer, int fd,
		      struct lemnos_stream_buffer *out)
{
	struct lemnos_stream_buffer a = { .buffer = buffer, .fd = fd };

	if (ioctl(s->fd, LEMNOS_STREAM_IOC_BUFFER, &a))
		return -errno;
	if (out)
		*out = a;
	return 0;
}

static void stream_close(struct stream *s)
{
	if (s->r)
		munmap((void *)s->r, s->ring_bytes);
	if (s->fd >= 0)
		close(s->fd);
	s->r = NULL;
	s->fd = -1;
}

static int kick(const struct stream *s)
{
	return ioctl(s->fd, LEMNOS_STREAM_IOC_KICK) ? -errno : 0;
}

static struct lemnos_stream_status status(const struct stream *s)
{
	struct lemnos_stream_status st;

	memset(&st, 0xff, sizeof(st));
	ioctl(s->fd, LEMNOS_STREAM_IOC_STATUS, &st);
	return st;
}

static int run_source(const struct stream *s, uint32_t period_us, uint32_t frames)
{
	struct lemnos_stream_test_source a = { .period_us = period_us, .frames = frames };

	return ioctl(s->fd, LEMNOS_STREAM_IOC_TEST_SOURCE, &a) ? -errno : 0;
}

/* A job at the next slot, length `len`, bytes from `job`. */
static void post_raw(struct stream *s, uint64_t len, const void *job, size_t bytes)
{
	size_t at = s->jobs_at + (size_t)(s->jobs_head & (JOB_SLOTS - 1)) * s->job_words;
	uint64_t words[(sizeof(struct lemnos_stream_job) + 7) / 8] = { 0 };

	memcpy(words, job, bytes);
	store(s, at, len);
	for (size_t i = 0; i < sizeof(words) / sizeof(words[0]); i++)
		store(s, at + 1 + i, words[i]);
	s->jobs_head++;
	store(s, LEMNOS_RING_JOBS_HEAD, s->jobs_head);
}

static void post(struct stream *s, const struct lemnos_stream_job *job)
{
	post_raw(s, sizeof(*job), job, sizeof(*job));
}

static int pop_frame(struct stream *s, struct frame *f)
{
	uint64_t head = load(s, LEMNOS_RING_FRAMES_HEAD);

	if (head == s->frames_tail)
		return 0;
	size_t at = LEMNOS_RING_FRAMES + (s->frames_tail & (FRAME_SLOTS - 1)) * LEMNOS_STREAM_FRAME_WORDS;
	uint64_t pair = load(s, at);

	f->buffer = (uint32_t)pair;
	f->bytes = (uint32_t)(pair >> 32);
	f->sequence = load(s, at + 1);
	f->identity = load(s, at + 2);
	f->fence = (uint32_t)load(s, at + 4);
	s->frames_tail++;
	store(s, LEMNOS_RING_FRAMES_TAIL, s->frames_tail);
	return 1;
}

static void give_back(struct stream *s, uint32_t buffer, uint32_t fence)
{
	size_t at = s->returns_at + (s->returns_head & (FRAME_SLOTS - 1));

	store(s, at, (uint64_t)buffer | (uint64_t)fence << 32);
	s->returns_head++;
	store(s, LEMNOS_RING_RETURNS_HEAD, s->returns_head);
}

/* Waits for a frame by polling the file. 1, or 0 on timeout. */
static int wait_frame(struct stream *s, struct frame *f, int timeout_ms)
{
	struct pollfd p = { .fd = s->fd, .events = POLLIN };

	for (int waited = 0; waited < timeout_ms; waited += 10) {
		if (pop_frame(s, f))
			return 1;
		poll(&p, 1, 10);
	}
	return pop_frame(s, f);
}

/* A faulted stream: KICK's errno, STATUS's, and the ring's state word. */
static void expect_fault(struct stream *s, int want, const char *what)
{
	int k = kick(s);
	struct lemnos_stream_status st = status(s);

	CHECK(k == -want && st.state == LEMNOS_RING_STATE_FAULT && st.fault == want &&
		      load(s, LEMNOS_RING_STATE) == LEMNOS_RING_STATE_FAULT,
	      "%s: %s (kick %d, status state %u errno %d, ring state %llu)", what, strerror(want), k,
	      st.state, st.fault, (unsigned long long)load(s, LEMNOS_RING_STATE));
}

static struct lemnos_stream_job job(uint32_t buffer, uint64_t identity, uint32_t count)
{
	struct lemnos_stream_job j;

	memset(&j, 0, sizeof(j));
	j.buffer = buffer;
	j.identity = identity;
	j.count = count;
	return j;
}

/* Ranges in `j` are relative to the pool (absolute when `rel` is 0). */
static void one_bad_job(int ctl, uint32_t block, int pool, struct lemnos_stream_job j, int rel,
			int want, const char *what)
{
	struct stream s;

	if (stream_open(&s, ctl, block, pool, -1)) {
		CHECK(0, "%s: open", what);
		return;
	}
	for (uint32_t i = 0; rel && i < j.count && i < LEMNOS_STREAM_JOB_RANGES; i++)
		j.range[i].addr += s.pool_addr;
	post(&s, &j);
	CHECK(kick(&s) == -want, "%s: KICK %s", what, strerror(want));
	expect_fault(&s, want, what);
	stream_close(&s);
}

static int stream_with_aux(struct stream *s, int ctl, uint32_t block, int pool, int aux,
			   uint64_t *aux_addr)
{
	struct lemnos_stream_register r = { .fd = aux };
	int ret = stream_open(s, ctl, block, pool, -1);

	if (ret)
		return ret;
	if (ioctl(s->fd, LEMNOS_STREAM_IOC_REGISTER, &r))
		return -errno;
	if (aux_addr)
		*aux_addr = r.addr;
	return (int)r.region;
}

/* Each is a fresh stream, faulted by exactly one thing. */
static void test_bad_jobs(int ctl, uint32_t block, int pool, int aux)
{
	struct lemnos_stream_job j;
	struct stream s;
	uint64_t aux_addr = 0;
	int region;

	j = job(BUFFERS, 1, 0);
	one_bad_job(ctl, block, pool, j, 1, EINVAL, "buffer number past the pool");

	j = job(0, 2, LEMNOS_STREAM_JOB_RANGES + 1);
	one_bad_job(ctl, block, pool, j, 0, EINVAL, "too many ranges");

	j = job(0, 3, 1);
	j.range[0] = (struct lemnos_stream_range){ .addr = 0, .len = 0 };
	one_bad_job(ctl, block, pool, j, 1, EINVAL, "a zero-length range");

	j = job(0, 4, 1);
	j.range[0] = (struct lemnos_stream_range){ .addr = 0, .len = 4 };
	j.range[1] = (struct lemnos_stream_range){ .addr = 1, .len = 1 };
	one_bad_job(ctl, block, pool, j, 1, EINVAL, "a used-looking spare range");

	j = job(0, 5, 1);
	j.range[0] = (struct lemnos_stream_range){ .addr = POOL_BYTES + 8192, .len = 8 };
	one_bad_job(ctl, block, pool, j, 1, EFAULT, "an address outside everything");

	j = job(0, 6, 1);
	j.range[0] = (struct lemnos_stream_range){ .addr = 0x1000, .len = 8 };
	one_bad_job(ctl, block, pool, j, 0, EFAULT, "a stray low address");

	j = job(0, 7, 1);
	j.range[0] = (struct lemnos_stream_range){ .addr = 0, .len = (uint64_t)BUFFER_BYTES + 1 };
	one_bad_job(ctl, block, pool, j, 1, EFAULT, "one past the end of the buffer");

	j = job(0, 8, 1);
	j.range[0] = (struct lemnos_stream_range){ .addr = BUFFER_BYTES, .len = 1 };
	one_bad_job(ctl, block, pool, j, 1, EACCES, "one past the end: the next buffer");

	j = job(BUFFERS - 1, 9, 1);
	j.range[0] = (struct lemnos_stream_range){ .addr = POOL_BYTES - 1, .len = 2 };
	one_bad_job(ctl, block, pool, j, 1, EFAULT, "one past the end of the pool");

	j = job(0, 11, 1);
	j.range[0] = (struct lemnos_stream_range){ .addr = UINT64_MAX - 7, .len = 16 };
	one_bad_job(ctl, block, pool, j, 0, EOVERFLOW, "addr + len wraps");

	j = job(0, 12, 1);
	j.range[0] = (struct lemnos_stream_range){ .addr = 0, .len = UINT64_MAX };
	one_bad_job(ctl, block, pool, j, 1, EOVERFLOW, "a length that wraps from the pool");

	j = job(0, 13, 1);
	j.range[0] = (struct lemnos_stream_range){ .addr = STRIDE, .len = 8 };
	one_bad_job(ctl, block, pool, j, 1, EACCES, "an address in another buffer");

	/* A registered dma-buf is bounded exactly the same way. */
	region = stream_with_aux(&s, ctl, block, pool, aux, &aux_addr);
	CHECK(region > 0, "a stream with a registered dma-buf");
	j = job(0, 14, 1);
	j.range[0] = (struct lemnos_stream_range){ .addr = aux_addr + AUX_BYTES - 16, .len = 16 };
	post(&s, &j);
	CHECK(kick(&s) == 0, "the last bytes of a registered dma-buf: accepted");
	j = job(1, 15, 1);
	j.range[0] = (struct lemnos_stream_range){ .addr = aux_addr + AUX_BYTES - 16, .len = 17 };
	post(&s, &j);
	expect_fault(&s, EFAULT, "one past the end of a registered dma-buf");
	stream_close(&s);
}

static void test_ring_faults(int ctl, uint32_t block, int pool)
{
	struct stream s;

	/* A job length the slot can't hold, or just wrong. */
	CHECK(!stream_open(&s, ctl, block, pool, -1), "open");
	post_raw(&s, sizeof(struct lemnos_stream_job) + 8, &(struct lemnos_stream_job){ 0 },
		 sizeof(struct lemnos_stream_job));
	expect_fault(&s, EMSGSIZE, "a job length past the slot");
	stream_close(&s);

	CHECK(!stream_open(&s, ctl, block, pool, -1), "open");
	post_raw(&s, 8, &(struct lemnos_stream_job){ 0 }, sizeof(struct lemnos_stream_job));
	expect_fault(&s, EMSGSIZE, "a job length too short");
	stream_close(&s);

	/* Heads that can't be. */
	CHECK(!stream_open(&s, ctl, block, pool, -1), "open");
	store(&s, LEMNOS_RING_JOBS_HEAD, UINT64_MAX);
	expect_fault(&s, ERANGE, "jobs head impossible");
	stream_close(&s);

	CHECK(!stream_open(&s, ctl, block, pool, -1), "open");
	store(&s, LEMNOS_RING_JOBS_HEAD, JOB_SLOTS + 1);
	expect_fault(&s, ERANGE, "jobs head one past the capacity");
	stream_close(&s);

	CHECK(!stream_open(&s, ctl, block, pool, -1), "open");
	store(&s, LEMNOS_RING_RETURNS_HEAD, 1ull << 40);
	expect_fault(&s, ERANGE, "returns head impossible");
	stream_close(&s);

	CHECK(!stream_open(&s, ctl, block, pool, -1), "open");
	store(&s, LEMNOS_RING_RETURNS_HEAD, FRAME_SLOTS + 1);
	expect_fault(&s, ERANGE, "returns head one past the capacity");
	stream_close(&s);

	/* A returned buffer number out of range. */
	CHECK(!stream_open(&s, ctl, block, pool, -1), "open");
	give_back(&s, BUFFERS, 1);
	expect_fault(&s, EINVAL, "returned buffer number past the pool");
	stream_close(&s);

	/* A buffer returned that isn't out. */
	CHECK(!stream_open(&s, ctl, block, pool, -1), "open");
	give_back(&s, 0, 1);
	expect_fault(&s, EPERM, "returned buffer that isn't out");
	stream_close(&s);
}

/*
 * The honest job-driven consumer: returns a buffer, then posts the next job on
 * that same buffer, with the source running flat out. Every frame must carry
 * its job's identity and the stream must never fault.
 */
#define JOB_ROUNDS 1500

/*
 * `outstanding` jobs at a time. `hold`: one job at a time and a window held
 * open between the return and the post, longer than the source's idle tick, so
 * a jobless frame would take the buffer. `hammer`: another process KICKs flat
 * out, to land a drain between the consumer's return and its post.
 */
static void job_round_trip(int ctl, uint32_t block, int pool, uint32_t outstanding, int hold,
			   int hammer, const char *what)
{
	struct stream s;
	struct frame f;
	uint64_t next_post = 1, next_seen = 1;
	uint32_t wrong = 0, bad_kicks = 0;
	pid_t child = -1;
	int n;

	CHECK(!stream_open(&s, ctl, block, pool, -1), "%s: open", what);
	for (uint32_t b = 0; b < outstanding; b++) {
		struct lemnos_stream_job j = job(b, next_post++, 0);

		post(&s, &j);
	}
	CHECK(kick(&s) == 0 && status(&s).jobs == outstanding, "%s: first jobs accepted before frames start",
	      what);
	CHECK(!run_source(&s, 0, ~0u), "%s: the source runs", what);
	if (hammer) {
		child = fork();
		if (!child)
			for (;;)
				(void)!ioctl(s.fd, LEMNOS_STREAM_IOC_KICK);
	}
	for (n = 0; n < JOB_ROUNDS; n++) {
		if (!wait_frame(&s, &f, 5000))
			break;
		wrong += f.identity != next_seen++;
		give_back(&s, f.buffer, f.fence);
		if (hold) {
			if (kick(&s))
				bad_kicks++;
			if (!(n % 4))
				usleep(3000);
		}
		struct lemnos_stream_job j = job(f.buffer, next_post++, 0);

		post(&s, &j);
	}
	if (child > 0) {
		kill(child, SIGKILL);
		waitpid(child, NULL, 0);
	}
	struct lemnos_stream_status st = status(&s);

	CHECK(n == JOB_ROUNDS, "%s: %d frames, returning a buffer and posting a job on it", what, n);
	CHECK(!wrong, "%s: every frame carried its job's identity (%u did not)", what, wrong);
	CHECK(!bad_kicks, "%s: no kick refused (%u were)", what, bad_kicks);
	CHECK(st.state == LEMNOS_RING_STATE_OPEN && !st.fault && st.frames >= JOB_ROUNDS,
	      "%s: never faulted: state %u, errno %d, %llu frames, %llu jobs", what, st.state, st.fault,
	      (unsigned long long)st.frames, (unsigned long long)st.jobs);
	CHECK(!run_source(&s, 0, 0), "%s: source stopped", what);
	stream_close(&s);
}

static void test_job_driven(int ctl, uint32_t block, int pool)
{
	struct stream s;
	struct frame f;

	job_round_trip(ctl, block, pool, 1, 1, 0, "one job, window held open");
	job_round_trip(ctl, block, pool, 1, 0, 1, "one job, KICKs racing");
	job_round_trip(ctl, block, pool, BUFFERS, 0, 1, "every buffer posted, KICKs racing");

	/* No job queued: nothing, however long the source runs. Then one job: one frame. */
	CHECK(!stream_open(&s, ctl, block, pool, -1), "open another");
	struct lemnos_stream_job first = job(1, 0x1111, 0);

	post(&s, &first);
	CHECK(kick(&s) == 0, "one job accepted");
	CHECK(!run_source(&s, 0, ~0u), "the source runs");
	CHECK(wait_frame(&s, &f, 5000) && f.identity == 0x1111 && f.buffer == 1, "that job's frame");
	give_back(&s, f.buffer, f.fence);
	CHECK(kick(&s) == 0, "buffer returned");
	uint64_t before = status(&s).frames;

	poll(NULL, 0, 200);
	CHECK(status(&s).frames == before && !pop_frame(&s, &f) && !status(&s).dropped,
	      "no job queued: no frame for 200 ms, with every buffer free");
	struct lemnos_stream_job second = job(2, 0x2222, 0);

	post(&s, &second);
	CHECK(wait_frame(&s, &f, 5000) && f.identity == 0x2222 && f.buffer == 2,
	      "a job posted: its frame appears (buffer %u, identity 0x%llx)", f.buffer,
	      (unsigned long long)f.identity);
	stream_close(&s);
}

/* The source must be running for the buffer to be held and for tails to be read. */
static void test_with_frames(int ctl, uint32_t block, int pool, uint8_t *pool_cpu, int aux,
			     uint8_t *aux_cpu)
{
	struct stream s;
	struct frame f;
	int bell = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
	uint64_t count = 0;

	CHECK(bell >= 0 && !stream_open(&s, ctl, block, pool, bell), "stream open with a doorbell");
	CHECK(load(&s, LEMNOS_RING_MAGIC) == LEMNOS_RING_MAGIC_VALUE &&
		      load(&s, LEMNOS_RING_VERSION) == LEMNOS_RING_LAYOUT_VERSION &&
		      load(&s, LEMNOS_RING_BUFFERS) == BUFFERS &&
		      load(&s, LEMNOS_RING_BUFFER_BYTES) == BUFFER_BYTES &&
		      load(&s, LEMNOS_RING_FRAME_SLOTS) == FRAME_SLOTS &&
		      load(&s, LEMNOS_RING_JOB_SLOTS) == JOB_SLOTS &&
		      load(&s, LEMNOS_RING_JOB_BYTES) == JOB_BYTES &&
		      load(&s, LEMNOS_RING_STATE) == LEMNOS_RING_STATE_OPEN,
	      "ring header is the wire layout");
	struct pollfd p = { .fd = s.fd, .events = POLLIN };

	CHECK(poll(&p, 1, 0) == 0, "poll: nothing waiting yet");
	CHECK(!run_source(&s, 0, 8), "test source: 8 frames");
	for (uint32_t n = 0; n < 8; n++) {
		CHECK(wait_frame(&s, &f, 5000), "frame %u arrived", n);
		const uint64_t *w = (const uint64_t *)(pool_cpu + (uint64_t)f.buffer * STRIDE);

		CHECK(f.sequence == n && f.bytes == BUFFER_BYTES && f.buffer < BUFFERS &&
			      w[0] == f.sequence && w[BUFFER_BYTES / 8 - 1] == f.sequence && f.fence,
		      "frame %u: sequence %llu, buffer %u, %u bytes, fence %u, content %llu", n,
		      (unsigned long long)f.sequence, f.buffer, f.bytes, f.fence,
		      (unsigned long long)w[0]);
		give_back(&s, f.buffer, f.fence);
	}
	{
		int waited = 0;

		while (waited < 5000 && status(&s).frames < 8) {
			poll(NULL, 0, 10);
			waited += 10;
		}
	}
	CHECK(status(&s).frames == 8, "8 frames counted");
	CHECK(read(bell, &count, sizeof(count)) == sizeof(count) && count >= 8,
	      "doorbell rang %llu times", (unsigned long long)count);
	/*
	 * Ends with every buffer given back: a job refused here (or below)
	 * must be for its own reason, not because a buffer is still out.
	 */
	CHECK(kick(&s) == 0, "the good consumer is not refused");

	/* A job: names a buffer and two ranges, comes back as this frame. */
	struct lemnos_stream_register reg = { .fd = aux };

	CHECK(!ioctl(s.fd, LEMNOS_STREAM_IOC_REGISTER, &reg) && reg.region && reg.size >= AUX_BYTES,
	      "registered a dma-buf: region %u at 0x%llx", reg.region, (unsigned long long)reg.addr);
	struct lemnos_stream_job j = job(2, 0xfeedfacecafebeefull, 2);

	j.range[0] = (struct lemnos_stream_range){ .addr = reg.addr + 4096, .len = 100 };
	j.range[1] = (struct lemnos_stream_range){ .addr = s.pool_addr + 2 * STRIDE + 512, .len = 16 };
	memset(aux_cpu, 0, AUX_BYTES);
	post(&s, &j);
	CHECK(kick(&s) == 0 && status(&s).jobs == 1, "the good job accepted");
	CHECK(load(&s, LEMNOS_RING_JOBS_TAIL) == 1, "the ring saw it taken");
	CHECK(!run_source(&s, 0, 1), "the source fills it");
	CHECK(wait_frame(&s, &f, 5000), "the job's frame arrived");
	CHECK(f.buffer == 2 && f.identity == j.identity, "the frame is that job's: buffer %u, identity 0x%llx",
	      f.buffer, (unsigned long long)f.identity);
	size_t bad = 0;

	for (size_t i = 0; i < AUX_BYTES; i++)
		bad += aux_cpu[i] != ((i >= 4096 && i < 4196) ? 0xa5 : 0);
	CHECK(!bad, "the source wrote exactly the range it was given (%zu bytes differ)", bad);
	const uint8_t *in_pool = pool_cpu + 2 * STRIDE;

	CHECK(in_pool[512] == (0xa5 ^ 1) && in_pool[527] == (0xa5 ^ 1) &&
		      ((const uint64_t *)in_pool)[0] == f.sequence,
	      "the source wrote the second range into its own buffer");

	/* Unregister while a job uses it: refused. */
	j = job(1, 1, 1);
	j.range[0] = (struct lemnos_stream_range){ .addr = reg.addr, .len = 8 };
	post(&s, &j);
	CHECK(kick(&s) == 0, "a job holds the region");
	struct lemnos_stream_unregister un = { .region = reg.region };

	CHECK(ioctl(s.fd, LEMNOS_STREAM_IOC_UNREGISTER, &un) < 0 && errno == EBUSY,
	      "unregister while in use: EBUSY");

	/* The job finishes, then it can go. */
	CHECK(!run_source(&s, 0, 1), "the source runs it");
	give_back(&s, f.buffer, f.fence); /* buffer 2 */
	CHECK(wait_frame(&s, &f, 5000), "its frame arrived");
	give_back(&s, f.buffer, f.fence);
	CHECK(kick(&s) == 0, "both returned");
	CHECK(!ioctl(s.fd, LEMNOS_STREAM_IOC_UNREGISTER, &un), "unregister after it finished");
	CHECK(ioctl(s.fd, LEMNOS_STREAM_IOC_UNREGISTER, &un) < 0 && errno == ENOENT,
	      "unregister again: ENOENT");
	struct lemnos_stream_unregister pool_un = { .region = 0 };

	CHECK(ioctl(s.fd, LEMNOS_STREAM_IOC_UNREGISTER, &pool_un) < 0 && errno == EINVAL,
	      "the pool can't be unregistered");
	close(bell);
	stream_close(&s);

	/* Double posting: a buffer named twice before either runs. */
	CHECK(!stream_open(&s, ctl, block, pool, -1), "open");
	struct lemnos_stream_job a = job(1, 1, 0);

	post(&s, &a);
	CHECK(kick(&s) == 0, "first posting of buffer 1 accepted");
	post(&s, &a);
	expect_fault(&s, EBUSY, "double posting");
	stream_close(&s);

	/* A buffer the consumer still holds. */
	CHECK(!stream_open(&s, ctl, block, pool, -1), "open");
	CHECK(!run_source(&s, 0, 1), "one frame for the consumer to hold");
	CHECK(wait_frame(&s, &f, 5000), "the consumer has it");
	struct lemnos_stream_job h = job(f.buffer, 1, 0);

	post(&s, &h);
	expect_fault(&s, EACCES, "a job on a buffer the consumer holds");
	stream_close(&s);

	/* Wrong fence, and returning a buffer twice. */
	CHECK(!stream_open(&s, ctl, block, pool, -1), "open");
	CHECK(!run_source(&s, 0, 1), "one frame");
	CHECK(wait_frame(&s, &f, 5000), "the consumer has it");
	give_back(&s, f.buffer, f.fence + 1);
	expect_fault(&s, EPERM, "the wrong fence");
	stream_close(&s);

	CHECK(!stream_open(&s, ctl, block, pool, -1), "open");
	CHECK(!run_source(&s, 0, 1), "one frame");
	CHECK(wait_frame(&s, &f, 5000), "the consumer has it");
	give_back(&s, f.buffer, f.fence);
	CHECK(kick(&s) == 0, "returned once");
	give_back(&s, f.buffer, f.fence);
	expect_fault(&s, EPERM, "returned twice");
	stream_close(&s);

	/* A tail that can't be, read while publishing. */
	CHECK(!stream_open(&s, ctl, block, pool, -1), "open");
	store(&s, LEMNOS_RING_FRAMES_TAIL, 3);
	CHECK(!run_source(&s, 0, 1), "a frame to publish against the bad tail");
	{
		int waited = 0;

		while (waited < 5000 && !status(&s).fault) {
			poll(NULL, 0, 10);
			waited += 10;
		}
	}
	expect_fault(&s, ERANGE, "a frames tail past the head, seen while publishing");
	stream_close(&s);
}

/* Random garbage over the whole ring while the source and KICK run. */
static void test_racing_garbage(int ctl, uint32_t block, int pool)
{
	struct stream s;

	CHECK(!stream_open(&s, ctl, block, pool, -1), "open");
	CHECK(!run_source(&s, 100, ~0u), "the source runs");
	uint64_t words = s.ring_bytes / 8;
	pid_t child = fork();

	if (!child) {
		uint64_t x = 0x1234567890abcdefull ^ getpid();

		for (;;) {
			for (int i = 0; i < 64; i++) {
				x ^= x << 13;
				x ^= x >> 7;
				x ^= x << 17;
				s.r[x % words] = x;
			}
		}
	}
	/* Whatever happens, kicks keep getting an answer. */
	int errors[4096 + 1] = { 0 };

	for (int n = 0; n < 20000; n++) {
		int k = kick(&s);

		if (k >= -4095 && k <= 0)
			errors[-k]++;
		if (!(n % 1024))
			poll(NULL, 0, 1);
	}
	kill(child, SIGKILL);
	waitpid(child, NULL, 0);
	struct lemnos_stream_status st = status(&s);

	CHECK(st.state == LEMNOS_RING_STATE_OPEN || st.state == LEMNOS_RING_STATE_FAULT,
	      "the module survived a scribbling consumer: state %u, errno %d, kicks ok %d", st.state,
	      st.fault, errors[0]);
	/* Close with the scribbler's garbage still in place and the source possibly running. */
	stream_close(&s);
}

/*
 * The slots only: the heads stay honest, so a job's words are read while they
 * change. Each round is a fresh stream; whichever way the race goes, it must
 * end in an accepted job or a fault, never anything else.
 */
static void test_racing_slots(int ctl, uint32_t block, int pool)
{
	unsigned int accepted = 0, faulted = 0, other = 0;

	for (int round = 0; round < 200; round++) {
		struct stream s;

		if (stream_open(&s, ctl, block, pool, -1)) {
			CHECK(0, "open round %d", round);
			return;
		}
		run_source(&s, 1, ~0u);
		pid_t child = fork();

		if (!child) {
			uint64_t x = 0xdeadbeefcafef00dull ^ (uint64_t)getpid() ^ (uint64_t)round;
			size_t base = s.returns_at, end = s.ring_bytes / 8;

			for (int i = 0;; i++) {
				x ^= x << 13;
				x ^= x >> 7;
				x ^= x << 17;
				s.r[base + x % (end - base)] = x;
				if (!(i % 4096))
					usleep(1);
			}
		}
		for (int i = 0; i < 40; i++) {
			struct lemnos_stream_job j = job(i & (BUFFERS - 1), (uint64_t)i, 1);
			int k;

			j.range[0] = (struct lemnos_stream_range){
				.addr = s.pool_addr + (i & (BUFFERS - 1)) * (uint64_t)STRIDE, .len = 8
			};
			post(&s, &j);
			k = kick(&s);
			if (!k)
				accepted++;
			else if (k == -EFAULT || k == -EINVAL || k == -EBUSY || k == -EACCES || k == -EOVERFLOW ||
				 k == -EPERM || k == -ERANGE || k == -EMSGSIZE)
				faulted++;
			else
				other++;
		}
		kill(child, SIGKILL);
		waitpid(child, NULL, 0);
		stream_close(&s);
	}
	CHECK(!other, "racing the slots: %u jobs accepted, %u refused, %u anything else", accepted,
	      faulted, other);
}

/* Closing with a frame being made and jobs queued. */
static void test_close_in_flight(int ctl, uint32_t block, int pool)
{
	for (int round = 0; round < 32; round++) {
		struct stream s;

		if (stream_open(&s, ctl, block, pool, -1)) {
			CHECK(0, "open round %d", round);
			return;
		}
		struct lemnos_stream_job j = job(0, 1, 0);

		post(&s, &j);
		run_source(&s, 0, ~0u);
		stream_close(&s);
	}
	CHECK(1, "32 closes with frames in flight");
}

/* No job at all, so the source keeps taking free buffers until the close lands. */
static void test_close_jobless_in_flight(int ctl, uint32_t block, int pool)
{
	for (int round = 0; round < 32; round++) {
		struct stream s;

		if (stream_open(&s, ctl, block, pool, -1)) {
			CHECK(0, "open jobless round %d", round);
			return;
		}
		run_source(&s, 0, ~0u);
		stream_close(&s);
	}
	CHECK(1, "32 closes with jobless frames in flight");
}

/* A consumer that dies with the source running. The ring and its pin end with it. */
static void test_crash(int ctl, uint32_t block, int pool)
{
	int p[2];
	char ready = 0;

	CHECK(!pipe(p), "pipe");
	pid_t child = fork();

	if (!child) {
		struct stream s;
		char ok = !stream_open(&s, ctl, block, pool, -1);

		if (ok)
			ok = !run_source(&s, 100, ~0u);
		(void)!write(p[1], &ok, 1);
		for (;;)
			pause();
	}
	close(p[1]);
	CHECK(read(p[0], &ready, 1) == 1 && ready, "a process holds a running stream");
	close(p[0]);
	poll(NULL, 0, 50);
	kill(child, SIGKILL);
	int status_ = 0;

	CHECK(waitpid(child, &status_, 0) == child && WIFSIGNALED(status_), "that process killed");
}

static void test_refusals_at_create(int ctl, uint32_t block, int pool)
{
	struct lemnos_stream_create c;

#define BAD_LAYOUT(field, val, name)                                              \
	do {                                                                      \
		c = good_layout(block, pool, -1);                                 \
		c.field = (val);                                                  \
		CHECK(ioctl(ctl, LEMNOS_IOC_STREAM_CREATE, &c) < 0 && errno == EINVAL, \
		      "%s: EINVAL", name);                                        \
	} while (0)

	BAD_LAYOUT(flags, 2, "unknown flags");
	BAD_LAYOUT(flags, 3, "a known flag with an unknown one");
	BAD_LAYOUT(flags, LEMNOS_STREAM_CREATE_BUFFERS, "own buffers and a pool descriptor");
	BAD_LAYOUT(buffers, 0, "no buffers");
	BAD_LAYOUT(buffers, LEMNOS_STREAM_MAX_BUFFERS + 1, "too many buffers");
	BAD_LAYOUT(buffer_bytes, 0, "zero buffer bytes");
	BAD_LAYOUT(buffer_bytes, LEMNOS_STREAM_MAX_BUFFER_BYTES + 1, "too-large buffers");
	BAD_LAYOUT(frame_slots, 3, "frame slots not a power of two");
	BAD_LAYOUT(frame_slots, 2, "fewer frame slots than buffers");
	BAD_LAYOUT(job_slots, 0, "no job slots");
	BAD_LAYOUT(job_slots, 128, "too many job slots");
	BAD_LAYOUT(job_bytes, sizeof(struct lemnos_stream_job) - 8, "job smaller than the job struct");
	BAD_LAYOUT(job_bytes, LEMNOS_STREAM_MAX_JOB_BYTES + 1, "too-large jobs");
	c = good_layout(block, pool, -1);
	c.buffers = 4096;
	c.buffer_bytes = 1u << 28;
	c.frame_slots = 8192;
	CHECK(ioctl(ctl, LEMNOS_IOC_STREAM_CREATE, &c) < 0 && errno == EINVAL,
	      "the whole pool past its limit: EINVAL");

	c = good_layout(block, -1, -1);
	CHECK(ioctl(ctl, LEMNOS_IOC_STREAM_CREATE, &c) < 0 && errno == EBADF, "no pool: EBADF");
	c = good_layout(block, pool, 9999);
	CHECK(ioctl(ctl, LEMNOS_IOC_STREAM_CREATE, &c) < 0 && errno == EBADF,
	      "a doorbell that isn't an eventfd: EBADF");
	c = good_layout(block, 0, -1);
	CHECK(ioctl(ctl, LEMNOS_IOC_STREAM_CREATE, &c) < 0 && errno == EINVAL,
	      "a pool that isn't a dma-buf: refused (%s)", strerror(errno));

	struct lemnos_alloc small = { .block = block, .size = STRIDE };

	if (ioctl(ctl, LEMNOS_IOC_ALLOC, &small))
		CHECK(0, "small alloc");
	c = good_layout(block, small.fd, -1);
	CHECK(ioctl(ctl, LEMNOS_IOC_STREAM_CREATE, &c) < 0 && errno == EINVAL,
	      "a pool smaller than the layout: EINVAL");
	close(small.fd);

	struct stream s;

	CHECK(!stream_open(&s, ctl, block, pool, -1), "a good stream");
	CHECK(mmap(NULL, s.ring_bytes + 4096, PROT_READ, MAP_SHARED, s.fd, 0) == MAP_FAILED,
	      "a mapping longer than the ring: refused (%s)", strerror(errno));
	CHECK(mmap(NULL, s.ring_bytes, PROT_READ, MAP_PRIVATE, s.fd, 0) == MAP_FAILED,
	      "a private mapping: refused (%s)", strerror(errno));
	CHECK(mmap(NULL, s.ring_bytes, PROT_READ | PROT_EXEC, MAP_SHARED, s.fd, 0) == MAP_FAILED,
	      "an executable mapping: refused (%s)", strerror(errno));
	stream_close(&s);
}

static void test_register(int ctl, uint32_t block, int pool, int aux)
{
	struct stream s;
	uint64_t addr = 0;
	int region;

	CHECK(!stream_open(&s, ctl, block, pool, -1), "open");
	struct lemnos_stream_register r = { .fd = pool };

	CHECK(ioctl(s.fd, LEMNOS_STREAM_IOC_REGISTER, &r) < 0 && errno == EBUSY,
	      "registering the pool again: EBUSY");
	stream_close(&s);

	region = stream_with_aux(&s, ctl, block, pool, aux, &addr);
	CHECK(region > 0, "registered a heap dma-buf");
	if (region > 0) {
		struct lemnos_stream_register again = { .fd = aux };
		struct lemnos_stream_register bad = { .fd = -1 };

		CHECK(ioctl(s.fd, LEMNOS_STREAM_IOC_REGISTER, &again) < 0 && errno == EBUSY,
		      "the same dma-buf twice: EBUSY");
		CHECK(ioctl(s.fd, LEMNOS_STREAM_IOC_REGISTER, &bad) < 0 && errno == EBADF,
		      "a bad fd: EBADF");
		struct lemnos_stream_unregister u = { .region = 999 };

		CHECK(ioctl(s.fd, LEMNOS_STREAM_IOC_UNREGISTER, &u) < 0 && errno == EINVAL,
		      "a region past the table: EINVAL");
	}
	stream_close(&s);
}

/* The table and the number of streams are fixed at the size the module was built with. */
static void test_limits(int ctl, uint32_t block, int pool, int heap_fd)
{
	struct stream streams[64];
	int fds[16], n = 0, got = 0;

	for (; n < 64; n++)
		if (stream_open(&streams[n], ctl, block, pool, -1))
			break;
	CHECK(n == 64, "%d streams held", n);
	{
		struct stream extra;

		CHECK(stream_open(&extra, ctl, block, pool, -1) == -ENOSPC, "one more stream: ENOSPC");
	}
	while (n)
		stream_close(&streams[--n]);

	struct stream s;

	CHECK(!stream_open(&s, ctl, block, pool, -1), "open");
	for (int i = 0; i < 16; i++) {
		struct dma_heap_allocation_data a = { .len = 4096, .fd_flags = O_RDWR | O_CLOEXEC };

		fds[i] = ioctl(heap_fd, DMA_HEAP_IOCTL_ALLOC, &a) ? -1 : (int)a.fd;
		struct lemnos_stream_register r = { .fd = fds[i] };

		if (fds[i] >= 0 && !ioctl(s.fd, LEMNOS_STREAM_IOC_REGISTER, &r))
			got++;
		else if (errno == ENOSPC)
			break;
	}
	CHECK(got == 15, "%d dma-bufs registered, then ENOSPC", got);
	for (int i = 0; i < 16; i++)
		if (fds[i] >= 0)
			close(fds[i]);
	stream_close(&s);
}

static int alloc_pool(int ctl, uint32_t block, uint64_t size)
{
	struct lemnos_alloc a = { .block = block, .size = size };

	return ioctl(ctl, LEMNOS_IOC_ALLOC, &a) ? -1 : a.fd;
}

/* --- buffers of their own (LEMNOS_STREAM_CREATE_BUFFERS) --- */

/* One buffer's memory: from the block, so its device address is known here too. */
struct own {
	int fd;
	uint64_t addr, size;
	uint8_t *cpu;
};

static struct own own_alloc(int ctl, uint32_t block, uint64_t size)
{
	struct lemnos_alloc a = { .block = block, .size = size };
	struct own o = { .fd = -1, .size = size };

	if (ioctl(ctl, LEMNOS_IOC_ALLOC, &a))
		return o;
	o.fd = a.fd;
	o.addr = a.addr;
	o.cpu = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, a.fd, 0);
	if (o.cpu == MAP_FAILED) {
		close(o.fd);
		o.fd = -1;
	}
	return o;
}

static void own_free(struct own *o)
{
	if (o->fd < 0)
		return;
	munmap(o->cpu, o->size);
	close(o->fd);
	o->fd = -1;
}

/* How many dma-bufs are attached to the device `name`, from the kernel's own list. */
static int attached(const char *name)
{
	static char buf[1 << 18];
	char line[160];
	int fd = open("/sys/kernel/debug/dma_buf/bufinfo", O_RDONLY | O_CLOEXEC);
	ssize_t n, total = 0;
	int count = 0;

	if (fd < 0)
		return -1;
	while (total < (ssize_t)sizeof(buf) - 1 &&
	       (n = read(fd, buf + total, sizeof(buf) - 1 - total)) > 0)
		total += n;
	close(fd);
	buf[total] = 0;
	snprintf(line, sizeof(line), "\t%s\n", name);
	for (const char *at = buf; (at = strstr(at, line)); at += strlen(line))
		count++;
	return count;
}

/* Runs this process on one CPU. The test source's work runs on the CPU that started it. */
static void pin(int cpu)
{
	cpu_set_t set;

	CPU_ZERO(&set);
	CPU_SET(cpu, &set);
	sched_setaffinity(0, sizeof(set), &set);
}

/* Everything LEMNOS_STREAM_IOC_BUFFER and the create flag refuse. No frames needed. */
static void test_own_refusals(int ctl, uint32_t block, const char *name, int pool, int aux,
			      int heap_fd)
{
	struct lemnos_stream_create c = good_layout(block, -1, -1);
	struct lemnos_stream_buffer out;
	struct lemnos_stream_register r;
	struct lemnos_stream_job j;
	struct stream s, s2;
	struct own o[BUFFERS], spare;
	struct dma_heap_allocation_data small = { .len = 4096, .fd_flags = O_RDWR | O_CLOEXEC };
	int before = attached(name), ret;

	c.flags = LEMNOS_STREAM_CREATE_BUFFERS;
	c.buffers = LEMNOS_STREAM_MAX_OWN_BUFFERS + 1;
	c.frame_slots = 128;
	CHECK(ioctl(ctl, LEMNOS_IOC_STREAM_CREATE, &c) < 0 && errno == EINVAL,
	      "own buffers: more than the table holds: EINVAL");

	CHECK(!stream_open(&s, ctl, block, pool, -1), "a stream with a pool");
	CHECK(set_buffer(&s, 0, aux, NULL) == -EOPNOTSUPP,
	      "giving a buffer memory on a stream with a pool: EOPNOTSUPP");
	stream_close(&s);

	CHECK(!own_open(&s, ctl, block) && s.pool_addr == 0, "a stream with no pool, pool_addr 0");
	CHECK(attached(name) == before, "it pins nothing yet");
	for (uint32_t i = 0; i < BUFFERS; i++)
		o[i] = own_alloc(ctl, block, i == 3 ? 2 * BUFFER_BYTES : BUFFER_BYTES);
	spare = own_alloc(ctl, block, BUFFER_BYTES);
	CHECK(o[0].fd >= 0 && o[1].fd >= 0 && o[2].fd >= 0 && o[3].fd >= 0 && spare.fd >= 0,
	      "five dma-bufs of the block's");

	CHECK(set_buffer(&s, BUFFERS, o[0].fd, NULL) == -EINVAL, "a buffer number past the last: EINVAL");
	CHECK(set_buffer(&s, 0, -1, NULL) == -EBADF, "a bad fd: EBADF");
	CHECK(set_buffer(&s, 0, ctl, NULL) == -EINVAL, "a file that isn't a dma-buf: EINVAL");
	CHECK(heap_fd >= 0 && !ioctl(heap_fd, DMA_HEAP_IOCTL_ALLOC, &small) &&
		      set_buffer(&s, 0, (int)small.fd, NULL) == -EINVAL,
	      "a dma-buf smaller than a buffer: EINVAL");
	close((int)small.fd);
	CHECK(attached(name) == before, "a refused dma-buf isn't left pinned");

	/* A job on a buffer with no memory, on a stream of its own because it ends it. */
	CHECK(!own_open(&s2, ctl, block), "another stream with no pool");
	j = job(0, 1, 0);
	post(&s2, &j);
	expect_fault(&s2, EFAULT, "a job on a buffer with no memory");
	CHECK(set_buffer(&s2, 0, o[0].fd, NULL) == -EFAULT, "a faulted stream takes no memory");
	stream_close(&s2);

	for (uint32_t i = 0; i < BUFFERS; i++) {
		ret = set_buffer(&s, i, o[i].fd, &out);
		CHECK(!ret && out.addr == o[i].addr && out.size == BUFFER_BYTES,
		      "buffer %u has its own dma-buf: %d, at %llx (the block's %llx), %llu bytes", i,
		      ret, (unsigned long long)out.addr, (unsigned long long)o[i].addr,
		      (unsigned long long)out.size);
	}
	CHECK(attached(name) == before + (int)BUFFERS, "one pin for each");
	memset(&out, 0, sizeof(out));
	CHECK(!set_buffer(&s, 0, o[0].fd, &out) && out.addr == o[0].addr,
	      "the dma-buf a buffer already has: nothing changes, same address");
	CHECK(attached(name) == before + (int)BUFFERS, "and no second pin");
	CHECK(set_buffer(&s, 1, o[0].fd, NULL) == -EBUSY, "another buffer's dma-buf: EBUSY");
	r = (struct lemnos_stream_register){ .fd = o[2].fd };
	CHECK(ioctl(s.fd, LEMNOS_STREAM_IOC_REGISTER, &r) < 0 && errno == EBUSY,
	      "registering a buffer's dma-buf: EBUSY");
	r = (struct lemnos_stream_register){ .fd = aux };
	CHECK(!ioctl(s.fd, LEMNOS_STREAM_IOC_REGISTER, &r), "a dma-buf registered beside them");
	CHECK(set_buffer(&s, 3, aux, NULL) == -EBUSY, "a registered dma-buf as a buffer: EBUSY");

	/* Swapping a free buffer's memory: the old pin goes, the new address is the buffer's. */
	CHECK(!set_buffer(&s, 3, spare.fd, &out) && out.addr == spare.addr,
	      "a free buffer's memory is swapped");
	CHECK(attached(name) == before + (int)BUFFERS + 1, "the old dma-buf's pin went with it");
	CHECK(!set_buffer(&s, 3, o[3].fd, &out) && out.addr == o[3].addr && out.size == BUFFER_BYTES,
	      "and swapped back: a longer dma-buf reaches only a buffer's bytes");

	/* A job holds its buffer: its memory can't change under it. */
	j = job(1, 7, 1);
	j.range[0] = (struct lemnos_stream_range){ .addr = o[1].addr + 512, .len = 16 };
	post(&s, &j);
	CHECK(kick(&s) == 0 && status(&s).jobs == 1, "a job naming a range in its own buffer: accepted");
	CHECK(set_buffer(&s, 1, spare.fd, NULL) == -EBUSY, "swapping a buffer a job names: EBUSY");
	CHECK(!set_buffer(&s, 1, o[1].fd, NULL), "naming what it has is still answered");
	j = job(2, 8, 1);
	j.range[0] = (struct lemnos_stream_range){ .addr = o[3].addr, .len = 16 };
	post(&s, &j);
	expect_fault(&s, EACCES, "a range in another buffer's dma-buf");
	stream_close(&s);

	/* Past a buffer's bytes but inside its (longer) dma-buf: not the buffer's, so nowhere. */
	CHECK(!own_open(&s, ctl, block) && !set_buffer(&s, 3, o[3].fd, NULL), "again, one buffer");
	j = job(3, 1, 1);
	j.range[0] = (struct lemnos_stream_range){ .addr = o[3].addr + BUFFER_BYTES - 8, .len = 16 };
	post(&s, &j);
	expect_fault(&s, EFAULT, "a range running past the buffer's bytes");
	stream_close(&s);

	CHECK(attached(name) == before, "closing the streams released every pin (%d left)",
	      attached(name) - before);
	for (uint32_t i = 0; i < BUFFERS; i++)
		own_free(&o[i]);
	own_free(&spare);
}

/* Frames land in each buffer's own dma-buf, and in the new one after a swap. */
static void test_own_frames(int ctl, uint32_t block, const char *name)
{
	struct stream s;
	struct frame f, held[BUFFERS];
	struct own o[BUFFERS], fresh, other;
	struct lemnos_stream_buffer out;
	int before = attached(name), swapped = 0, busy = 0, odd = 0;
	unsigned int seen = 0;

	for (uint32_t i = 0; i < BUFFERS; i++)
		o[i] = own_alloc(ctl, block, BUFFER_BYTES);
	fresh = own_alloc(ctl, block, BUFFER_BYTES);
	other = own_alloc(ctl, block, BUFFER_BYTES);
	CHECK(!own_open(&s, ctl, block) && fresh.fd >= 0 && other.fd >= 0 && o[3].fd >= 0,
	      "own buffers: stream and memory");

	CHECK(!run_source(&s, 0, 2), "the source runs with no buffer having memory");
	poll(NULL, 0, 50);
	CHECK(!pop_frame(&s, &f) && status(&s).frames == 0, "and no frame is made");
	memset(o[2].cpu, 0x11, BUFFER_BYTES);
	CHECK(!set_buffer(&s, 2, o[2].fd, NULL), "buffer 2 gets memory");
	for (int n = 0; n < 2; n++) {
		const uint64_t *w = (const uint64_t *)o[2].cpu;

		CHECK(wait_frame(&s, &f, 5000) && f.buffer == 2 && w[0] == f.sequence &&
			      w[BUFFER_BYTES / 8 - 1] == f.sequence,
		      "frame %d is on the only buffer with memory, in that memory (buffer %u, content %llu)",
		      n, f.buffer, (unsigned long long)w[0]);
		give_back(&s, f.buffer, f.fence);
	}

	for (uint32_t i = 0; i < BUFFERS; i++) {
		memset(o[i].cpu, 0x11, BUFFER_BYTES);
		if (i != 2)
			CHECK(!set_buffer(&s, i, o[i].fd, NULL), "buffer %u gets memory", i);
	}
	CHECK(!run_source(&s, 0, BUFFERS), "one frame for each buffer");
	for (uint32_t n = 0; n < BUFFERS; n++) {
		CHECK(wait_frame(&s, &f, 5000) && f.buffer < BUFFERS, "frame %u arrived", n);
		if (f.buffer >= BUFFERS)
			continue;
		const uint64_t *w = (const uint64_t *)o[f.buffer].cpu;

		CHECK(w[0] == f.sequence && w[BUFFER_BYTES / 8 - 1] == f.sequence,
		      "buffer %u's frame is in buffer %u's own dma-buf (content %llu, sequence %llu)",
		      f.buffer, f.buffer, (unsigned long long)w[0], (unsigned long long)f.sequence);
		seen |= 1u << f.buffer;
		held[f.buffer] = f;
	}
	CHECK(seen == (1u << BUFFERS) - 1, "every buffer was filled once (%x)", seen);

	/* The consumer holds them all. Buffer 1 gets new memory before it goes back. */
	memset(o[1].cpu, 0x22, BUFFER_BYTES);
	memset(fresh.cpu, 0x33, BUFFER_BYTES);
	CHECK(!set_buffer(&s, 1, fresh.fd, &out) && out.addr == fresh.addr,
	      "a buffer the consumer holds is given new memory");
	for (uint32_t i = 0; i < BUFFERS; i++)
		give_back(&s, i, held[i].fence);
	CHECK(!run_source(&s, 0, BUFFERS), "four more frames");
	for (uint32_t n = 0; n < BUFFERS; n++) {
		if (!wait_frame(&s, &f, 5000)) {
			CHECK(0, "frame %u after the swap", n);
			break;
		}
		if (f.buffer == 1) {
			const uint64_t *w = (const uint64_t *)fresh.cpu;

			CHECK(w[0] == f.sequence && w[BUFFER_BYTES / 8 - 1] == f.sequence,
			      "buffer 1's next frame is in its new memory (content %llu, sequence %llu)",
			      (unsigned long long)w[0], (unsigned long long)f.sequence);
			swapped = 1;
		}
		give_back(&s, f.buffer, f.fence);
	}
	CHECK(swapped && o[1].cpu[0] == 0x22 && o[1].cpu[BUFFER_BYTES - 1] == 0x22,
	      "and the memory it had is not written again (%x)", o[1].cpu[0]);
	CHECK(attached(name) == before + (int)BUFFERS, "one pin a buffer, the old one's gone");

	/*
	 * Swapping under a running source: each call either swaps (the buffer was
	 * free or with us) or is refused because the source had it. Nothing else.
	 */
	CHECK(kick(&s) == 0, "the stream is sound before the race");
	/* The source on one CPU and the swaps on another, so they really overlap. */
	pin(1);
	CHECK(!run_source(&s, 0, ~0u), "the source runs flat out");
	pin(0);
	for (int round = 0; round < 20000; round++) {
		int ret = set_buffer(&s, 1, round & 1 ? fresh.fd : other.fd, NULL);

		swapped += ret == 0;
		busy += ret == -EBUSY;
		odd += ret && ret != -EBUSY;
		/* The source sleeps when it has no buffer: keep it fed. */
		while (pop_frame(&s, &f))
			give_back(&s, f.buffer, f.fence);
	}
	CHECK(!odd && swapped > 1,
	      "20000 swaps under the source: %d swapped, %d busy, %d anything else", swapped - 1,
	      busy, odd);
	{
		cpu_set_t all;

		memset(&all, 0xff, sizeof(all));
		sched_setaffinity(0, sizeof(all), &all);
	}
	CHECK(!run_source(&s, 0, 0), "the source stops");
	while (pop_frame(&s, &f))
		give_back(&s, f.buffer, f.fence);
	CHECK(kick(&s) == 0 && status(&s).state == LEMNOS_RING_STATE_OPEN,
	      "and the stream is still sound");

	/* Closed with frames in flight: every buffer's pin goes with the stream. */
	CHECK(!run_source(&s, 0, ~0u), "running again");
	stream_close(&s);
	CHECK(attached(name) == before, "closing released every buffer's pin (%d left)",
	      attached(name) - before);
	for (uint32_t i = 0; i < BUFFERS; i++)
		own_free(&o[i]);
	own_free(&fresh);
	own_free(&other);
}

int main(int argc, char **argv)
{
	if (argc < 5) {
		fprintf(stderr,
			"usage: lemnos_stream_check /dev/lemnos <name> <unbind path> <heap> [no-source]\n");
		return 2;
	}
	const char *node = argv[1], *name = argv[2], *unbind = argv[3], *heap = argv[4];
	int no_source = argc > 5 && !strcmp(argv[5], "no-source");
	int ctl = open(node, O_RDWR | O_CLOEXEC);

	setvbuf(stdout, NULL, _IOLBF, 0);
	if (ctl < 0) {
		perror(node);
		return 2;
	}
	uint32_t block = find_block(ctl, name);

	CHECK(block, "block %s is number %u", name, block);

	int pool = alloc_pool(ctl, block, POOL_BYTES);

	CHECK(pool >= 0, "pool allocated");
	uint8_t *pool_cpu = mmap(NULL, POOL_BYTES, PROT_READ, MAP_SHARED, pool, 0);

	CHECK(pool_cpu != MAP_FAILED, "pool mapped read-only");
	int h = open(heap, O_RDONLY | O_CLOEXEC);
	struct dma_heap_allocation_data heap_alloc = { .len = AUX_BYTES, .fd_flags = O_RDWR | O_CLOEXEC };

	CHECK(h >= 0 && !ioctl(h, DMA_HEAP_IOCTL_ALLOC, &heap_alloc), "a second dma-buf from the heap");
	int aux = (int)heap_alloc.fd;
	uint8_t *aux_cpu = mmap(NULL, AUX_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, aux, 0);

	CHECK(aux_cpu != MAP_FAILED, "and mapped");

	if (no_source) {
		struct stream s;

		test_refusals_at_create(ctl, block, pool);
		test_own_refusals(ctl, block, name, pool, aux, h);
		CHECK(!stream_open(&s, ctl, block, pool, -1), "a stream without the test source");
		CHECK(run_source(&s, 0, 1) == -EPERM, "the test source is off: EPERM");
		struct lemnos_stream_job j = job(0, 1, 1);

		j.range[0] = (struct lemnos_stream_range){ .addr = 0x10, .len = 8 };
		post(&s, &j);
		CHECK(kick(&s) == -EFAULT, "a bad address refused without the test source: EFAULT");
		stream_close(&s);
		close(aux);
		munmap(aux_cpu, AUX_BYTES);
		close(h);
		munmap(pool_cpu, POOL_BYTES);
		close(pool);
		close(ctl);
		printf("%s (no source): %d failed\n", node, failures);
		return failures ? 1 : 0;
	}

	test_refusals_at_create(ctl, block, pool);
	test_register(ctl, block, pool, aux);
	test_limits(ctl, block, pool, h);
	test_own_refusals(ctl, block, name, pool, aux, h);
	test_own_frames(ctl, block, name);
	test_with_frames(ctl, block, pool, pool_cpu, aux, aux_cpu);
	test_job_driven(ctl, block, pool);
	test_bad_jobs(ctl, block, pool, aux);
	test_ring_faults(ctl, block, pool);
	test_racing_garbage(ctl, block, pool);
	test_racing_slots(ctl, block, pool);

	/* A stream unrelated to all that still works: faults stay on their own stream. */
	{
		int pool2 = alloc_pool(ctl, block, POOL_BYTES);
		struct stream s;
		struct frame f;

		CHECK(pool2 >= 0 && !stream_open(&s, ctl, block, pool2, -1), "another stream");
		CHECK(!run_source(&s, 0, 2) && wait_frame(&s, &f, 5000),
		      "frames still flow on a stream that broke no rules");
		give_back(&s, f.buffer, f.fence);
		CHECK(wait_frame(&s, &f, 5000), "and keep flowing");
		CHECK(kick(&s) == 0, "it isn't faulted");
		stream_close(&s);
		close(pool2);
	}

	test_close_in_flight(ctl, block, pool);
	test_close_jobless_in_flight(ctl, block, pool);
	test_crash(ctl, block, pool);

	/* Streams are gone: the pool is ours alone again, and its one more release frees it. */
	munmap(pool_cpu, POOL_BYTES);
	close(pool);

	/*
	 * A block unbound with a stream open: the stream ends, its pin of
	 * the block's memory goes, the file stays and says so.
	 */
	{
		int fresh = alloc_pool(ctl, block, POOL_BYTES);
		struct stream s;

		struct stream own;
		struct own mem[BUFFERS];

		CHECK(fresh >= 0 && !stream_open(&s, ctl, block, fresh, -1), "a stream before the unbind");
		CHECK(!run_source(&s, 100, ~0u), "running");
		close(fresh); /* only the stream's pin holds it now */
		/* And one with buffers of their own, each held by nothing but its pin. */
		CHECK(!own_open(&own, ctl, block), "a stream with no pool before the unbind");
		for (uint32_t i = 0; i < BUFFERS; i++) {
			mem[i] = own_alloc(ctl, block, BUFFER_BYTES);
			CHECK(!set_buffer(&own, i, mem[i].fd, NULL), "its buffer %u has memory", i);
			own_free(&mem[i]);
		}
		CHECK(!run_source(&own, 100, ~0u), "running too");
		pid_t child = fork();

		if (!child) {
			int u = open(unbind, O_WRONLY);

			_exit(u >= 0 && write(u, name, strlen(name)) == (ssize_t)strlen(name) ? 0 : 1);
		}
		int st = 0;

		CHECK(waitpid(child, &st, 0) == child && WIFEXITED(st) && !WEXITSTATUS(st),
		      "the unbind finished with the stream's pin released");
		CHECK(kick(&s) == -ENODEV, "the stream's file says the block went: ENODEV");
		CHECK(status(&s).state == LEMNOS_RING_STATE_CLOSED &&
			      load(&s, LEMNOS_RING_STATE) == LEMNOS_RING_STATE_CLOSED,
		      "the stream reads closed");
		struct pollfd p = { .fd = s.fd, .events = POLLIN };

		CHECK(poll(&p, 1, 0) == 1 && (p.revents & POLLHUP), "and poll says it ended");
		stream_close(&s);
		CHECK(status(&own).state == LEMNOS_RING_STATE_CLOSED, "the stream with no pool ended too");
		CHECK(set_buffer(&own, 0, aux, NULL) == -ENODEV,
		      "and takes no memory once the block went: ENODEV");
		CHECK(attached(name) == 0, "nothing is left attached to the block (%d)", attached(name));
		stream_close(&own);
	}

	close(aux);
	munmap(aux_cpu, AUX_BYTES);
	close(h);
	close(ctl);
	printf("%s: %d failed\n", node, failures);
	return failures ? 1 : 0;
}
