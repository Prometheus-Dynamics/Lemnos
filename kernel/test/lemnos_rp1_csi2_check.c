// SPDX-License-Identifier: GPL-2.0
/*
 * The rp1-csi2 receiver on the fake block in the test module: frames reach the
 * stream with every programmed address taken from the pool, a bad START or a
 * hostile interrupt status is refused, and stopping (STOP, closing either file,
 * unbinding) waits for the channel's DMA before anything is released.
 *
 * Usage: lemnos_rp1_csi2_check /dev/lemnos <block> <csi2 device> <unbind> <heap>
 *    or: lemnos_rp1_csi2_check wedge <csi2 device> <unbind> <heap>
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
#include <time.h>
#include <unistd.h>

#include "../lemnos.h"
#include "../lemnos_rp1_csi2.h"
#include "find_block.h"
#include "lemnos_test_csi2.h"

#define BUFFERS 4u
#define BUFFER_BYTES 8192u
#define STRIDE_POOL 8192u
#define FRAME_SLOTS 8u
#define JOB_SLOTS 4u
#define JOB_BYTES ((uint32_t)sizeof(struct lemnos_stream_job))

/* A frame that fits a buffer with room to spare: stride * height is its size. */
#define F_STRIDE 512u
#define F_HEIGHT 4u
#define F_SIZE (F_STRIDE * F_HEIGHT)

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
	int fd;
	volatile uint64_t *r;
	uint64_t ring_bytes, pool_addr;
	size_t returns_at, jobs_at, job_words;
	uint64_t frames_tail, returns_head, jobs_head;
};

struct frame {
	uint32_t buffer, bytes, fence;
	uint64_t sequence, identity;
};

static int ctl, hw;
static uint8_t *pool_cpu;

static uint64_t load(const struct stream *s, size_t at)
{
	return __atomic_load_n(&s->r[at], __ATOMIC_ACQUIRE);
}

static void store(const struct stream *s, size_t at, uint64_t v)
{
	__atomic_store_n(&s->r[at], v, __ATOMIC_RELEASE);
}

static void sleep_ms(unsigned int ms)
{
	struct timespec t = { ms / 1000, (long)(ms % 1000) * 1000000 };

	while (nanosleep(&t, &t) && errno == EINTR)
		;
}

static uint64_t now_ms(void)
{
	struct timespec t;

	clock_gettime(CLOCK_MONOTONIC, &t);
	return (uint64_t)t.tv_sec * 1000 + (uint64_t)t.tv_nsec / 1000000;
}

static struct lemnos_rp1_csi2_info info(void)
{
	struct lemnos_rp1_csi2_info i;

	memset(&i, 0xff, sizeof(i));
	CHECK(!ioctl(ctl, LEMNOS_RP1_CSI2_IOC_INFO, &i), "INFO");
	return i;
}

static struct tcsi_regs peek(void)
{
	struct tcsi_regs r;

	memset(&r, 0xff, sizeof(r));
	ioctl(hw, TCSI_IOC_PEEK, &r);
	return r;
}

static void busy(uint32_t ms)
{
	ioctl(hw, TCSI_IOC_BUSY, &ms);
}

/* Fires a status word and waits until the handler has seen it. */
static void fire(uint32_t status)
{
	uint64_t frames = info().frames;

	CHECK(!ioctl(hw, TCSI_IOC_FIRE, &status), "fire %x", status);
	for (int i = 0; i < 400; i++) {
		sleep_ms(1);
		if (info().frames != frames || !(status & TCSI_FS))
			break;
	}
	sleep_ms(3);
}

static int stream_open(struct stream *s, int pool_fd)
{
	struct lemnos_stream_create c = {
		.buffers = BUFFERS,
		.buffer_bytes = BUFFER_BYTES,
		.frame_slots = FRAME_SLOTS,
		.job_slots = JOB_SLOTS,
		.job_bytes = JOB_BYTES,
		.pool_fd = pool_fd,
		.doorbell_fd = -1,
	};

	memset(s, 0, sizeof(*s));
	if (ioctl(ctl, LEMNOS_RP1_CSI2_IOC_STREAM_CREATE, &c))
		return -errno;
	s->fd = c.fd;
	s->ring_bytes = c.ring_bytes;
	s->pool_addr = c.pool_addr;
	s->returns_at = LEMNOS_RING_FRAMES + FRAME_SLOTS * LEMNOS_STREAM_FRAME_WORDS;
	s->jobs_at = s->returns_at + FRAME_SLOTS;
	s->job_words = 1 + (JOB_BYTES + 7) / 8;
	s->r = mmap(NULL, s->ring_bytes, PROT_READ | PROT_WRITE, MAP_SHARED, c.fd, 0);
	if (s->r == MAP_FAILED)
		return -errno;
	return 0;
}

static void stream_close(struct stream *s)
{
	munmap((void *)s->r, s->ring_bytes);
	close(s->fd);
	s->fd = -1;
}

static struct lemnos_rp1_csi2_start good_start(int stream_fd)
{
	return (struct lemnos_rp1_csi2_start){
		.stream_fd = stream_fd,
		.lanes = 2,
		.mbps = 800,
		.dt = 0x2b,
		.width = 16,
		.height = F_HEIGHT,
		.stride = F_STRIDE,
		.size = F_SIZE,
	};
}

static int start(const struct lemnos_rp1_csi2_start *a)
{
	return ioctl(ctl, LEMNOS_RP1_CSI2_IOC_START, a) ? -errno : 0;
}

static int stop(void)
{
	return ioctl(ctl, LEMNOS_RP1_CSI2_IOC_STOP) ? -errno : 0;
}

static int pop_frame(struct stream *s, struct frame *f)
{
	uint64_t head = load(s, LEMNOS_RING_FRAMES_HEAD);
	size_t at;
	uint64_t pair;

	if (head == s->frames_tail)
		return 0;
	at = LEMNOS_RING_FRAMES + (s->frames_tail & (FRAME_SLOTS - 1)) * LEMNOS_STREAM_FRAME_WORDS;
	pair = load(s, at);
	f->buffer = (uint32_t)pair;
	f->bytes = pair >> 32;
	f->sequence = load(s, at + 1);
	f->identity = load(s, at + 2);
	f->fence = (uint32_t)load(s, at + 4);
	s->frames_tail++;
	store(s, LEMNOS_RING_FRAMES_TAIL, s->frames_tail);
	return 1;
}

static void give_back(struct stream *s, uint32_t buffer, uint32_t fence)
{
	store(s, s->returns_at + (s->returns_head & (FRAME_SLOTS - 1)),
	      (uint64_t)buffer | (uint64_t)fence << 32);
	s->returns_head++;
	store(s, LEMNOS_RING_RETURNS_HEAD, s->returns_head);
}

static void post(struct stream *s, const struct lemnos_stream_job *job)
{
	size_t at = s->jobs_at + (s->jobs_head & (JOB_SLOTS - 1)) * s->job_words;
	uint64_t words[sizeof(*job) / 8];

	memcpy(words, job, sizeof(*job));
	store(s, at, sizeof(*job));
	for (size_t i = 0; i < sizeof(*job) / 8; i++)
		store(s, at + 1 + i, words[i]);
	s->jobs_head++;
	store(s, LEMNOS_RING_JOBS_HEAD, s->jobs_head);
}

static uint64_t buffer_addr(const struct stream *s, uint32_t b)
{
	return s->pool_addr + (uint64_t)b * STRIDE_POOL;
}

static void expect_armed(const struct stream *s, uint32_t b, const char *what)
{
	struct tcsi_regs r = peek();
	uint64_t a = (uint64_t)r.addr1 << 36 | (uint64_t)r.addr0 << 4;

	CHECK(a == buffer_addr(s, b) && r.length == F_SIZE >> 4 && r.stride == F_STRIDE >> 4,
	      "%s: buffer %u armed (addr %llx length %u stride %u)", what, b, (unsigned long long)a,
	      r.length, r.stride);
}

/* Bad starts: each refused, with the receiver left alone and able to run. */
static void test_bad_starts(int pool_fd)
{
	struct stream s;
	struct lemnos_rp1_csi2_start a;
	const struct {
		const char *name;
		size_t off;
		uint32_t value;
	} bad[] = {
		{ "lanes 0", offsetof(struct lemnos_rp1_csi2_start, lanes), 0 },
		{ "lanes too many", offsetof(struct lemnos_rp1_csi2_start, lanes), 3 },
		{ "mbps low", offsetof(struct lemnos_rp1_csi2_start, mbps), 79 },
		{ "mbps high", offsetof(struct lemnos_rp1_csi2_start, mbps), 1501 },
		{ "vc", offsetof(struct lemnos_rp1_csi2_start, vc), 4 },
		{ "dt 0", offsetof(struct lemnos_rp1_csi2_start, dt), 0 },
		{ "dt 64", offsetof(struct lemnos_rp1_csi2_start, dt), 64 },
		{ "width 0", offsetof(struct lemnos_rp1_csi2_start, width), 0 },
		{ "width huge", offsetof(struct lemnos_rp1_csi2_start, width), 0x10000 },
		{ "height 0", offsetof(struct lemnos_rp1_csi2_start, height), 0 },
		{ "height huge", offsetof(struct lemnos_rp1_csi2_start, height), 0x10000 },
		{ "stride 0", offsetof(struct lemnos_rp1_csi2_start, stride), 0 },
		{ "stride odd", offsetof(struct lemnos_rp1_csi2_start, stride), 17 },
		{ "stride*height past size", offsetof(struct lemnos_rp1_csi2_start, stride), 1024 },
		{ "size 0", offsetof(struct lemnos_rp1_csi2_start, size), 0 },
		{ "size odd", offsetof(struct lemnos_rp1_csi2_start, size), 2049 },
		{ "size past buffer", offsetof(struct lemnos_rp1_csi2_start, size), 8192 + 16 },
		{ "size past buffer, huge", offsetof(struct lemnos_rp1_csi2_start, size), 0xfffffff0 },
		{ "flags", offsetof(struct lemnos_rp1_csi2_start, flags), 2 },
		{ "reserved", offsetof(struct lemnos_rp1_csi2_start, reserved), 1 },
	};

	CHECK(!stream_open(&s, pool_fd), "stream made");
	a = good_start(-1);
	CHECK(start(&a) == -EBADF, "a bad descriptor is refused (EBADF)");
	a = good_start(ctl);
	CHECK(start(&a) == -EINVAL, "a file that isn't a stream is refused (EINVAL)");

	for (size_t i = 0; i < sizeof(bad) / sizeof(bad[0]); i++) {
		a = good_start(s.fd);
		memcpy((char *)&a + bad[i].off, &bad[i].value, sizeof(bad[i].value));
		/* The rest stays valid, so the field under test is what's refused. */
		CHECK(start(&a) == -EINVAL, "refused (EINVAL): %s", bad[i].name);
		CHECK(!info().streaming, "not streaming after: %s", bad[i].name);
	}
	a = good_start(s.fd);
	a.flags = LEMNOS_RP1_CSI2_UNPACK16;
	a.width = F_STRIDE;	/* needs a stride of width * 2 */
	CHECK(start(&a) == -EINVAL, "refused: UNPACK16 stride too small");

	stream_close(&s);
}

/* A stream on another block can't feed this receiver. */
static void test_foreign_stream(int pool_fd, const char *block_name)
{
	int lem = open("/dev/lemnos", O_RDWR | O_CLOEXEC);
	uint32_t block;
	struct lemnos_stream_create c = {
		.buffers = BUFFERS,
		.buffer_bytes = BUFFER_BYTES,
		.frame_slots = FRAME_SLOTS,
		.job_slots = JOB_SLOTS,
		.job_bytes = JOB_BYTES,
		.pool_fd = pool_fd,
		.doorbell_fd = -1,
	};
	struct lemnos_rp1_csi2_start a;

	CHECK(lem >= 0, "control node opened");
	block = find_block(lem, block_name);
	CHECK(block != 0, "found the plain block");
	c.block = block;
	CHECK(!ioctl(lem, LEMNOS_IOC_STREAM_CREATE, &c), "stream on another block made");
	a = good_start(c.fd);
	CHECK(start(&a) == -EXDEV, "a stream made on another block is refused (EXDEV)");
	close(c.fd);
	close(lem);
}

/* Frames: START arms, FS latches, FE publishes, and the data is where the kernel said. */
static void test_frames(int pool_fd)
{
	struct stream s;
	struct frame f;
	struct lemnos_rp1_csi2_start a;
	struct lemnos_rp1_csi2_info i;
	struct tcsi_regs r;
	uint8_t seen;

	CHECK(!stream_open(&s, pool_fd), "stream made");
	a = good_start(s.fd);
	CHECK(!start(&a), "START");
	expect_armed(&s, 0, "after START");
	r = peek();
	CHECK(r.ctrl & 1 && (r.ctrl >> 7 & 63) == 0x2b, "channel enabled with the data type (ctrl %x)",
	      r.ctrl);
	CHECK(r.frame_size == (F_HEIGHT << 16 | 16), "frame size register %x", r.frame_size);
	CHECK(info().streaming, "streaming");

	memset(pool_cpu, 0, STRIDE_POOL * BUFFERS);
	fire(TCSI_FS);
	expect_armed(&s, 1, "after frame start");
	fire(TCSI_FE_ACK);
	i = info();
	CHECK(i.frames == 1 && i.written == 1, "frame written (frames %llu written %llu)",
	      (unsigned long long)i.frames, (unsigned long long)i.written);
	CHECK(pop_frame(&s, &f), "frame published");
	CHECK(f.buffer == 0 && f.bytes == F_SIZE && f.sequence == 0,
	      "frame: buffer %u bytes %u sequence %llu", f.buffer, f.bytes,
	      (unsigned long long)f.sequence);
	seen = pool_cpu[0];
	CHECK(seen != 0 && pool_cpu[F_SIZE - 1] == seen,
	      "the channel wrote the frame into its buffer (byte %x)", seen);
	CHECK(pool_cpu[F_SIZE] == 0, "nothing past the frame's bytes");
	CHECK(pool_cpu[STRIDE_POOL] == 0, "the next buffer untouched");
	give_back(&s, f.buffer, f.fence);

	/*
	 * Now hold every buffer: 1 and 2 go through, 3 is armed. The one after
	 * that has nothing to arm: the frame start is dropped and no register
	 * is written.
	 */
	fire(TCSI_FS);
	fire(TCSI_FE_ACK);
	fire(TCSI_FS);
	fire(TCSI_FE_ACK);
	CHECK(pop_frame(&s, &f) && f.buffer == 1, "frame on buffer 1");
	CHECK(pop_frame(&s, &f) && f.buffer == 2, "frame on buffer 2");
	expect_armed(&s, 3, "after two frames");
	fire(TCSI_FS);
	fire(TCSI_FE_ACK);
	CHECK(pop_frame(&s, &f) && f.buffer == 3, "frame on buffer 3");
	CHECK(info().dropped == 0, "no drops so far");
	expect_armed(&s, 0, "the returned buffer is armed");
	fire(TCSI_FS);
	fire(TCSI_FE_ACK);
	CHECK(pop_frame(&s, &f) && f.buffer == 0, "frame on the returned buffer");
	r = peek();
	fire(TCSI_FS);
	i = info();
	CHECK(i.dropped == 1, "a frame start with no buffer armed is dropped (%llu)",
	      (unsigned long long)i.dropped);
	CHECK(peek().addr0 == r.addr0, "and the armed address is left alone");
	CHECK(!pop_frame(&s, &f), "and no frame comes of it");

	stream_close(&s);
	CHECK(!info().streaming, "closing the stream stopped the receiver");
}

/* Hostile interrupt status: whatever the block says, only bounded, kernel-held work is done. */
static void test_hostile_hw(int pool_fd)
{
	struct stream s;
	struct frame f;
	struct lemnos_rp1_csi2_start a;
	struct lemnos_rp1_csi2_info i;
	uint64_t frames;

	CHECK(!stream_open(&s, pool_fd), "stream made");
	a = good_start(s.fd);
	CHECK(!start(&a), "START");
	/* A frame end with no frame start: nothing to complete, nothing published. */
	fire(TCSI_FE_ACK);
	sleep_ms(5);
	CHECK(!pop_frame(&s, &f), "a frame end with no frame start publishes nothing");
	i = info();
	CHECK(i.frames == 0 && i.written == 0, "and counts nothing");

	/* A start before the last frame's end: the old frame is flagged bad, not published. */
	fire(TCSI_FS);
	fire(TCSI_FS);
	i = info();
	CHECK(i.frames == 2 && i.errors == 1,
	      "a frame start with one open is an error (frames %llu errors %llu)",
	      (unsigned long long)i.frames, (unsigned long long)i.errors);
	CHECK(!pop_frame(&s, &f), "the bad frame isn't published");

	/* All-ones status, repeatedly: only the bits it means, counts move by one each. */
	frames = info().frames;
	for (int n = 0; n < 3; n++) {
		uint32_t all = 0xffffffffu;

		CHECK(!ioctl(hw, TCSI_IOC_FIRE, &all), "fire all ones");
		sleep_ms(5);
	}
	i = info();
	CHECK(i.frames <= frames + 6, "frames bounded by interrupts seen (%llu)",
	      (unsigned long long)i.frames);
	while (pop_frame(&s, &f))
		CHECK(f.buffer < BUFFERS && f.bytes <= F_SIZE, "published frames are bounded");

	stream_close(&s);
}

/* Job-driven: the next buffer comes from the job; a broken frame comes back with 0 bytes. */
static void test_jobs(int pool_fd)
{
	struct stream s;
	struct frame f;
	struct lemnos_rp1_csi2_start a;
	struct lemnos_stream_job job = { .identity = 0xabc, .buffer = 2, .count = 0 };
	struct lemnos_rp1_csi2_info i;

	CHECK(!stream_open(&s, pool_fd), "stream made");
	post(&s, &job);
	CHECK(!ioctl(s.fd, LEMNOS_STREAM_IOC_KICK), "job accepted");
	a = good_start(s.fd);
	CHECK(!start(&a), "START");
	expect_armed(&s, 2, "job names buffer 2");
	fire(TCSI_FS);
	fire(TCSI_FE_ACK);
	CHECK(pop_frame(&s, &f), "frame published");
	CHECK(f.buffer == 2 && f.identity == 0xabc && f.bytes == F_SIZE,
	      "the job's identity came back unchanged (buffer %u identity %llx)", f.buffer,
	      (unsigned long long)f.identity);

	/* The stream is job-driven now: no job, no buffer armed for this start. */
	fire(TCSI_FS);
	i = info();
	CHECK(i.dropped == 1, "a frame start with no job is dropped (%llu)",
	      (unsigned long long)i.dropped);

	/* A job whose frame goes wrong comes back with 0 bytes, identity intact. */
	give_back(&s, f.buffer, f.fence);
	job = (struct lemnos_stream_job){ .identity = 0xdef, .buffer = 3 };
	post(&s, &job);
	fire(TCSI_FE_ACK);	/* that frame end (no buffer) lets the job be armed */
	fire(TCSI_FS);
	fire(TCSI_FS);		/* a second start: the frame on buffer 3 went wrong */
	CHECK(pop_frame(&s, &f) && f.buffer == 3 && f.bytes == 0 && f.identity == 0xdef,
	      "a bad frame on a job comes back with 0 bytes (buffer %u bytes %u identity %llx)",
	      f.buffer, f.bytes, (unsigned long long)f.identity);

	stream_close(&s);
}

/* A frame is left in flight; `ms` of fake counter movement must hold the stop back. */
static uint64_t timed_stop(void (*action)(void *), void *arg, uint32_t ms)
{
	uint64_t t;

	busy(ms);
	t = now_ms();
	action(arg);
	return now_ms() - t;
}

static void act_stop(void *a)
{
	(void)a;
	CHECK(!stop(), "STOP");
}

static void act_close_stream(void *a)
{
	stream_close(a);
}

static void test_stop_waits(int pool_fd)
{
	struct stream s;
	struct lemnos_rp1_csi2_start a;
	uint64_t ms;

	CHECK(!stream_open(&s, pool_fd), "stream made");
	a = good_start(s.fd);
	CHECK(!start(&a), "START");
	fire(TCSI_FS);
	ms = timed_stop(act_stop, NULL, 40);
	CHECK(ms >= 30, "STOP waits for DMA, frame in flight (took %llu ms)", (unsigned long long)ms);
	CHECK(!info().streaming, "stopped");
	busy(0);
	/*
	 * Every buffer came back, including the two the stopped frame held:
	 * START again and all four must go through with nothing dropped.
	 */
	CHECK(!start(&a), "START again on the same stream");
	{
		struct frame f;
		unsigned int seen = 0, got = 0;

		for (int n = 0; n < 4; n++) {
			fire(TCSI_FS);
			fire(TCSI_FE_ACK);
			if (pop_frame(&s, &f)) {
				seen |= 1u << f.buffer;
				got++;
				give_back(&s, f.buffer, f.fence);
			}
		}
		CHECK(got == 4 && seen == 0xf && info().dropped == 0,
		      "stopping gave every buffer back (%u frames, buffers %x, %llu dropped)", got,
		      seen, (unsigned long long)info().dropped);
	}

	/* Stop again, then close: the DMA wait also holds for a stream's close. */
	ms = timed_stop(act_stop, NULL, 20);
	CHECK(ms >= 12, "STOP again waits (took %llu ms)", (unsigned long long)ms);
	busy(0);
	CHECK(!start(&a), "START");
	fire(TCSI_FS);
	ms = timed_stop(act_close_stream, &s, 30);
	CHECK(ms >= 20, "closing the stream with a frame in flight waits for DMA (took %llu ms)",
	      (unsigned long long)ms);
	CHECK(!info().streaming, "closing the stream stopped the receiver");
	busy(0);
}

static void test_close_node(const char *node, int pool_fd)
{
	struct stream s;
	struct lemnos_rp1_csi2_start a;
	uint64_t ms;
	int second;

	second = open(node, O_RDWR | O_CLOEXEC);
	CHECK(second < 0 && errno == EBUSY, "the node has one opener at a time");
	CHECK(!stream_open(&s, pool_fd), "stream made");
	a = good_start(s.fd);
	CHECK(!start(&a), "START");
	fire(TCSI_FS);
	busy(25);
	ms = now_ms();
	close(ctl);
	ms = now_ms() - ms;
	CHECK(ms >= 15, "closing the node with a frame in flight waits for DMA (took %llu ms)",
	      (unsigned long long)ms);
	ctl = open(node, O_RDWR | O_CLOEXEC);
	CHECK(ctl >= 0, "the node opens again");
	CHECK(!info().streaming, "not streaming after the close");
	busy(0);
	CHECK(load(&s, LEMNOS_RING_STATE) == LEMNOS_RING_STATE_OPEN, "the stream stays open");
	CHECK(!start(&a), "START works again after the node closed");
	CHECK(!stop(), "STOP");
	stream_close(&s);
}

/* Unbinding with a frame in flight: waits for DMA; the stream ends; the node goes. */
static void test_unbind(const char *node, const char *unbind, const char *dev, int pool_fd)
{
	struct stream s;
	struct lemnos_rp1_csi2_start a;
	uint64_t ms;
	int fd;

	CHECK(!stream_open(&s, pool_fd), "stream made");
	a = good_start(s.fd);
	CHECK(!start(&a), "START");
	fire(TCSI_FS);
	busy(35);
	fd = open(unbind, O_WRONLY | O_CLOEXEC);
	CHECK(fd >= 0, "unbind file opened");
	ms = now_ms();
	CHECK(write(fd, dev, strlen(dev)) == (ssize_t)strlen(dev), "unbound");
	ms = now_ms() - ms;
	close(fd);
	CHECK(ms >= 20, "unbinding with a frame in flight waits for DMA (took %llu ms)",
	      (unsigned long long)ms);
	busy(0);
	CHECK(load(&s, LEMNOS_RING_STATE) == LEMNOS_RING_STATE_CLOSED, "the stream ended");
	CHECK(access(node, F_OK), "the node went away");
	CHECK(ioctl(ctl, LEMNOS_RP1_CSI2_IOC_STOP) == -1 && errno == ENODEV,
	      "an open file now gets ENODEV");
	stream_close(&s);
	close(ctl);
	ctl = -1;
}

static int bufinfo_attached(const char *dev)
{
	static char buf[65536];
	int fd = open("/sys/kernel/debug/dma_buf/bufinfo", O_RDONLY | O_CLOEXEC);
	ssize_t n;

	if (fd < 0)
		return -1;
	n = read(fd, buf, sizeof(buf) - 1);
	close(fd);
	if (n < 0)
		return -1;
	buf[n] = 0;
	return strstr(buf, dev) != NULL;
}

static int make_pool(const char *heap_path)
{
	int heap = open(heap_path, O_RDONLY | O_CLOEXEC);
	struct dma_heap_allocation_data alloc = {
		.len = (uint64_t)BUFFERS * STRIDE_POOL,
		.fd_flags = O_RDWR | O_CLOEXEC,
	};

	if (heap < 0)
		return -1;
	if (ioctl(heap, DMA_HEAP_IOCTL_ALLOC, &alloc))
		return -1;
	close(heap);
	return (int)alloc.fd;
}

/* DMA that never stops: the buffers stay pinned for good and the receiver refuses to start. */
static int run_wedge(const char *node, const char *dev, const char *heap)
{
	struct stream s, s2;
	struct lemnos_rp1_csi2_start a;
	int pool_fd, pool2_fd, fd;
	uint32_t forever = 0xffffffffu;

	ctl = open(node, O_RDWR | O_CLOEXEC);
	hw = open("/dev/lemnos-test-csi2", O_RDWR | O_CLOEXEC);
	pool_fd = make_pool(heap);
	pool2_fd = make_pool(heap);
	CHECK(ctl >= 0 && hw >= 0 && pool_fd >= 0 && pool2_fd >= 0, "wedge: set up");

	CHECK(!stream_open(&s, pool_fd), "stream made");
	a = good_start(s.fd);
	CHECK(!start(&a), "START");
	fire(TCSI_FS);
	busy(forever);
	CHECK(!stop(), "STOP returns, DMA never seen to stop");
	busy(0);
	CHECK(info().flags & LEMNOS_RP1_CSI2_INFO_WEDGED, "wedged is reported");
	CHECK(!stream_open(&s2, pool2_fd), "a new stream can be made");
	a = good_start(s2.fd);
	CHECK(start(&a) == -EIO, "wedged refuses START (EIO)");
	stream_close(&s2);
	close(pool2_fd);

	/* Everything the DMA could reach stays pinned, even with nobody holding it. */
	stream_close(&s);
	close(pool_fd);
	sleep_ms(5);
	CHECK(bufinfo_attached(dev) == 1, "the pool stays attached to the device after every close");

	fd = open("/sys/bus/platform/drivers/lemnos-rp1-csi2/unbind", O_WRONLY | O_CLOEXEC);
	CHECK(fd >= 0 && write(fd, dev, strlen(dev)) == (ssize_t)strlen(dev), "unbound while wedged");
	close(fd);
	close(ctl);
	close(hw);
	return failures ? 1 : 0;
}

int main(int argc, char **argv)
{
	const char *dev, *heap, *unbind, *block;
	char node[128];
	int pool_fd;
	struct stream s;

	if (argc == 5 && !strcmp(argv[1], "wedge")) {
		snprintf(node, sizeof(node), "/dev/lemnos-rp1-csi2-%s", argv[2]);
		return run_wedge(node, argv[2], argv[4]);
	}
	if (argc != 6)
		return 2;
	block = argv[2];
	dev = argv[3];
	unbind = argv[4];
	heap = argv[5];
	snprintf(node, sizeof(node), "/dev/lemnos-rp1-csi2-%s", dev);

	ctl = open(node, O_RDWR | O_CLOEXEC);
	hw = open("/dev/lemnos-test-csi2", O_RDWR | O_CLOEXEC);
	CHECK(ctl >= 0 && hw >= 0, "opened");
	pool_fd = make_pool(heap);
	CHECK(pool_fd >= 0, "pool allocated");
	pool_cpu = mmap(NULL, BUFFERS * STRIDE_POOL, PROT_READ | PROT_WRITE, MAP_SHARED, pool_fd, 0);
	CHECK(pool_cpu != MAP_FAILED, "pool mapped");

	CHECK(info().abi == LEMNOS_RP1_CSI2_ABI, "ABI");

	test_bad_starts(pool_fd);
	test_foreign_stream(pool_fd, block);
	CHECK(!stream_open(&s, pool_fd), "a stream can be made and dropped untouched");
	stream_close(&s);
	test_frames(pool_fd);
	test_hostile_hw(pool_fd);
	test_jobs(pool_fd);
	test_stop_waits(pool_fd);
	test_close_node(node, pool_fd);
	test_unbind(node, unbind, dev, pool_fd);
	CHECK(bufinfo_attached(dev) == 0, "every mapping released after the unbind");

	munmap(pool_cpu, BUFFERS * STRIDE_POOL);
	close(pool_fd);
	close(hw);
	printf("%s\n", failures ? "FAILED" : "all checks passed");
	return failures ? 1 : 0;
}
