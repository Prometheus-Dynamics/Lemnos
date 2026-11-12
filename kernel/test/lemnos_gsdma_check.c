// SPDX-License-Identifier: GPL-2.0
/*
 * The K230 graphics DMA engine's job checks, with a fake engine in place of
 * the hardware (test block "lemnos-test-gsdma", module parameter
 * test_gsdma_fake_regs). The fake walks the descriptors the module built and
 * copies through the bus addresses they name, so what it writes is what the
 * module told it.
 *
 * Every field of a job that becomes memory the engine touches is tried out of
 * bounds, wrapped, overlapping and disagreeing with its format, on a job that
 * is otherwise good, and the exact errno is required. Good jobs are checked
 * pixel by pixel, turned and not. A job that never finishes is stopped and
 * the stream goes on; an engine that never stops is wedged and keeps the
 * job's memory.
 *
 * Usage: lemnos_gsdma_check /dev/lemnos <device> <heap> <run|board|reach|wedge>
 *
 * board is run for a real engine: the refusals (proved by the block's own
 * finished count staying put), and the good copies checked pixel by pixel,
 * without anything that needs the fake engine.
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
#include "../lemnos_gsdma.h"
#include "find_block.h"

/* The same numbers as drivers/k230-gsdma-sys's layout test. */
_Static_assert(sizeof(struct lemnos_gsdma_plane) == 24, "plane");
_Static_assert(sizeof(struct lemnos_gsdma_job) == 104, "job");
_Static_assert(offsetof(struct lemnos_gsdma_job, fourcc) == 12, "job fourcc");
_Static_assert(offsetof(struct lemnos_gsdma_job, rotation) == 24, "job rotation");
_Static_assert(offsetof(struct lemnos_gsdma_job, plane) == 32, "job planes");
_Static_assert(sizeof(struct lemnos_gsdma_create) == 64, "create");
_Static_assert(offsetof(struct lemnos_gsdma_create, reach) == 56, "create reach");
_Static_assert(sizeof(struct lemnos_gsdma_status) == 32, "status");
_Static_assert(offsetof(struct lemnos_gsdma_plane, dst_addr) == 8, "plane dst");
_Static_assert(offsetof(struct lemnos_gsdma_plane, src_pitch) == 16, "plane src pitch");
_Static_assert(offsetof(struct lemnos_gsdma_plane, dst_pitch) == 20, "plane dst pitch");
_Static_assert(offsetof(struct lemnos_gsdma_job, buffer) == 8, "job buffer");
_Static_assert(offsetof(struct lemnos_gsdma_job, width) == 16, "job width");
_Static_assert(offsetof(struct lemnos_gsdma_job, height) == 20, "job height");
_Static_assert(offsetof(struct lemnos_gsdma_job, planes) == 28, "job plane count");
_Static_assert(offsetof(struct lemnos_gsdma_status, finished) == 8, "status finished");
_Static_assert(offsetof(struct lemnos_gsdma_status, refused) == 16, "status refused");
_Static_assert(offsetof(struct lemnos_gsdma_status, timeouts) == 24, "status timeouts");
_Static_assert(LEMNOS_IOC_GSDMA_CREATE == 0xc0404ce8u, "create request");
_Static_assert(LEMNOS_GSDMA_IOC_STATUS == 0x80204ce9u, "status request");

#define BUFFERS 2u
#define BUFFER_BYTES 4096u
#define FRAME_SLOTS 2u
#define JOB_SLOTS 2u
#define JOB_BYTES ((uint32_t)sizeof(struct lemnos_gsdma_job))
#define REGION_BYTES (1u << 20)
#define DST_OFFSET (REGION_BYTES / 2)
#define FOURCC(a, b, c, d) ((uint32_t)(a) | (uint32_t)(b) << 8 | (uint32_t)(c) << 16 | (uint32_t)(d) << 24)
#define AR24 FOURCC('A', 'R', '2', '4')
#define NV12 FOURCC('N', 'V', '1', '2')

/* board: a real engine, so nothing from the fake engine's sysfs files. */
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

/* What the fake engine has run: chains, unreachable addresses, the last first words. */
static int ran(unsigned int *jobs, unsigned int *errors, unsigned int word0[3])
{
	char path[320], text[128];
	int fd;
	ssize_t n;

	snprintf(path, sizeof(path), "%s/ran", sysdir);
	fd = open(path, O_RDONLY | O_CLOEXEC);
	n = fd < 0 ? -1 : read(fd, text, sizeof(text) - 1);
	if (fd >= 0)
		close(fd);
	if (n <= 0)
		return -1;
	text[n] = 0;
	return sscanf(text, "%u %u %x %x %x", jobs, errors, &word0[0], &word0[1], &word0[2]) == 5 ?
		       0 : -1;
}

struct stream {
	int fd, pool_fd, region_fd;
	uint64_t *ring;
	size_t ring_bytes;
	uint64_t pool_addr, region_addr, reach;
	uint8_t *image;			/* the region, mapped */
	size_t returns_at, jobs_at, job_words;
	uint64_t jobs_head, frames_tail, returns_head;
};

static int stream_open(struct stream *st)
{
	struct lemnos_gsdma_create c;
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
	if (ioctl(ctl_fd, LEMNOS_IOC_GSDMA_CREATE, &c))
		return -1;
	st->fd = c.stream.fd;
	st->ring_bytes = c.stream.ring_bytes;
	st->pool_addr = c.stream.pool_addr;
	st->reach = c.reach;
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

static void post(struct stream *st, const struct lemnos_gsdma_job *job)
{
	size_t at = st->jobs_at + (st->jobs_head & (JOB_SLOTS - 1)) * st->job_words;
	uint64_t words[sizeof(*job) / 8];

	memcpy(words, job, sizeof(*job));
	st->ring[at] = sizeof(*job);
	for (size_t i = 0; i < sizeof(*job) / 8; i++)
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

static struct lemnos_gsdma_status block_status(struct stream *st)
{
	struct lemnos_gsdma_status s;

	memset(&s, 0xff, sizeof(s));
	ioctl(st->fd, LEMNOS_GSDMA_IOC_STATUS, &s);
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

/* An ARGB8888 copy of w x h, turned by `rotation`, from the region's start to its second half. */
static struct lemnos_gsdma_job argb_job(const struct stream *st, uint32_t w, uint32_t h,
					uint32_t rotation)
{
	int swaps = rotation & (LEMNOS_GSDMA_ROTATE_90 | LEMNOS_GSDMA_ROTATE_270);
	struct lemnos_gsdma_job j = {
		.identity = 0x1234,
		.buffer = 0,
		.fourcc = AR24,
		.width = w,
		.height = h,
		.rotation = rotation,
		.planes = 1,
	};

	j.plane[0].src_addr = st->region_addr;
	j.plane[0].src_pitch = w * 4;
	j.plane[0].dst_addr = st->region_addr + DST_OFFSET;
	j.plane[0].dst_pitch = (swaps ? h : w) * 4;
	return j;
}

/* A job that must be refused with `err`, ending its stream, on a stream of its own. */
static void refused(const struct lemnos_gsdma_job *job_in, int err, const char *what,
		    void (*edit)(struct stream *, struct lemnos_gsdma_job *))
{
	struct stream st;
	struct lemnos_gsdma_job job;
	unsigned int before[5] = {0}, after[5] = {0};
	uint64_t finished;
	int got;

	if (stream_open(&st)) {
		printf("FAIL %s: stream (%s)\n", what, strerror(errno));
		failures++;
		stream_close(&st);
		return;
	}
	job = job_in ? *job_in : argb_job(&st, 8, 4, LEMNOS_GSDMA_ROTATE_0);
	if (edit)
		edit(&st, &job);
	if (!board)
		ran(&before[0], &before[1], &before[2]);
	finished = block_status(&st).finished;
	post(&st, &job);
	got = kick(&st);
	if (got != err) {
		printf("FAIL %s: %s, wanted %s\n", what, strerror(got), strerror(err));
		failures++;
	}
	CHECK(status_of(&st).state == LEMNOS_RING_STATE_FAULT);
	CHECK(status_of(&st).fault == err);
	usleep(5000);
	/* Nothing reached the engine. */
	CHECK(block_status(&st).finished == finished);
	if (!board) {
		ran(&after[0], &after[1], &after[2]);
		CHECK(after[0] == before[0]);
	}
	stream_close(&st);
}

static void e_outside(struct stream *st, struct lemnos_gsdma_job *j)
{
	j->plane[0].dst_addr = st->region_addr + REGION_BYTES;
}

static void e_past_end(struct stream *st, struct lemnos_gsdma_job *j)
{
	/* The last row ends one byte past the buffer: 3 rows of 32 bytes, then 33. */
	j->plane[0].dst_addr = st->region_addr + REGION_BYTES - (3 * 32 + 31);
}

static void e_tall(struct stream *st, struct lemnos_gsdma_job *j)
{
	/* A pitch long enough to step past the buffer within the rows. */
	j->plane[0].dst_pitch = 0xffff;
	j->height = 32;
}

static void e_pitch_short(struct stream *st, struct lemnos_gsdma_job *j)
{
	j->plane[0].src_pitch = 31;
}

static void e_pitch_zero(struct stream *st, struct lemnos_gsdma_job *j)
{
	j->plane[0].dst_pitch = 0;
}

static void e_pitch_wide(struct stream *st, struct lemnos_gsdma_job *j)
{
	j->plane[0].dst_pitch = 0x10000;
}

static void e_turned_pitch(struct stream *st, struct lemnos_gsdma_job *j)
{
	/* Turned, the destination's row is the source's height: 4 * 4 bytes is fine, 8 * 4 isn't. */
	j->rotation = LEMNOS_GSDMA_ROTATE_90;
	j->width = 8;
	j->height = 8;
	j->plane[0].dst_pitch = 16;
}

static void e_wrap(struct stream *st, struct lemnos_gsdma_job *j)
{
	j->plane[0].src_addr = ~0ull - 40;
}

static void e_overlap(struct stream *st, struct lemnos_gsdma_job *j)
{
	j->plane[0].dst_addr = st->region_addr + 64;
}

static void e_pool(struct stream *st, struct lemnos_gsdma_job *j)
{
	j->plane[0].dst_addr = st->pool_addr;
}

static void e_unregistered(struct stream *st, struct lemnos_gsdma_job *j)
{
	/* Memory nobody registered: its address is a guess, and every guess is outside. */
	j->plane[0].dst_addr = st->region_addr + 2 * REGION_BYTES;
}

static void e_two_turns(struct stream *st, struct lemnos_gsdma_job *j)
{
	j->rotation = LEMNOS_GSDMA_ROTATE_0 | LEMNOS_GSDMA_ROTATE_90;
}

static void e_no_turn(struct stream *st, struct lemnos_gsdma_job *j)
{
	j->rotation = LEMNOS_GSDMA_REFLECT_X;
}

static void e_unknown_bit(struct stream *st, struct lemnos_gsdma_job *j)
{
	j->rotation |= 1u << 6;
}

static void e_format(struct stream *st, struct lemnos_gsdma_job *j)
{
	j->fourcc = FOURCC('Y', 'U', 'Y', 'V');
}

static void e_planes(struct stream *st, struct lemnos_gsdma_job *j)
{
	j->planes = 2;
}

static void e_spare_plane(struct stream *st, struct lemnos_gsdma_job *j)
{
	j->plane[1].src_pitch = 1;
}

static void e_width0(struct stream *st, struct lemnos_gsdma_job *j)
{
	j->width = 0;
}

static void e_width_big(struct stream *st, struct lemnos_gsdma_job *j)
{
	j->width = 0x10000;
}

static void e_buffer(struct stream *st, struct lemnos_gsdma_job *j)
{
	j->buffer = BUFFERS;
}

static void e_nv12_overlap(struct stream *st, struct lemnos_gsdma_job *j)
{
	/* NV12 8x4: luma 32 bytes, chroma 2 rows of 8. Chroma's destination lands on luma's. */
	j->fourcc = NV12;
	j->planes = 2;
	j->plane[0].src_pitch = j->plane[0].dst_pitch = 8;
	j->plane[1].src_addr = st->region_addr + 4096;
	j->plane[1].dst_addr = st->region_addr + DST_OFFSET + 8;
	j->plane[1].src_pitch = j->plane[1].dst_pitch = 8;
}

static uint32_t px(uint32_t x, uint32_t y)
{
	return 0xa5000000u | y << 8 | x;
}

/* A good ARGB job, turned by `rotation`, checked pixel by pixel. */
static void good_argb(struct stream *st, uint32_t rotation, uint64_t identity)
{
	const uint32_t w = 8, h = 4;
	struct lemnos_gsdma_job j = argb_job(st, w, h, rotation);
	uint32_t *src = (uint32_t *)st->image, *dst = (uint32_t *)(st->image + DST_OFFSET);
	uint64_t got_id = 0;
	int bad = 0;

	for (uint32_t y = 0; y < h; y++)
		for (uint32_t x = 0; x < w; x++)
			src[y * w + x] = px(x, y);
	memset(dst, 0, 4 * w * h);
	j.identity = identity;
	post(st, &j);
	CHECK(kick(st) == 0);
	{
		long bytes = wait_frame(st, &got_id, 2000);
		unsigned int jobs, errors, word0[3];

		if (bytes != BUFFER_BYTES && board) {
			printf("FAIL frame of %ld bytes for job %llu\n", bytes,
			       (unsigned long long)identity);
			failures++;
		} else if (bytes != BUFFER_BYTES) {
			struct lemnos_gsdma_status b = block_status(st);
			struct lemnos_stream_status s = status_of(st);

			ran(&jobs, &errors, word0);
			printf("FAIL frame of %ld bytes for job %llu: finished %llu refused %llu "
			       "timeouts %llu; stream state %u fault %d jobs %llu frames %llu; "
			       "engine ran %u (%u errors) %x\n",
			       bytes, (unsigned long long)identity, (unsigned long long)b.finished,
			       (unsigned long long)b.refused, (unsigned long long)b.timeouts, s.state,
			       s.fault, (unsigned long long)s.jobs, (unsigned long long)s.frames,
			       jobs, errors, word0[0]);
			failures++;
		}
	}
	CHECK(got_id == identity);
	for (uint32_t y = 0; y < h; y++) {
		for (uint32_t x = 0; x < w; x++) {
			uint32_t v;

			if (rotation & LEMNOS_GSDMA_ROTATE_90)
				v = dst[x * h + (h - 1 - y)];
			else
				v = dst[y * w + x];
			bad += v != px(x, y);
		}
	}
	if (bad)
		printf("FAIL rotation %#x: %d pixels wrong\n", rotation, bad), failures++;
}

static void run_all(void)
{
	struct stream st;
	unsigned int jobs0, jobs, errors, word0[3];
	struct lemnos_gsdma_job j;
	uint64_t id = 0;

	/* Refusals, each ending its own stream. */
	refused(NULL, EFAULT, "a destination past every registered dma-buf", e_outside);
	refused(NULL, EFAULT, "a destination whose last row ends past the buffer", e_past_end);
	refused(NULL, EFAULT, "rows that step past the buffer", e_tall);
	refused(NULL, EFAULT, "a destination in memory never registered", e_unregistered);
	refused(NULL, EINVAL, "a pitch shorter than a row", e_pitch_short);
	refused(NULL, EINVAL, "a pitch of 0", e_pitch_zero);
	refused(NULL, EINVAL, "a pitch the descriptor can't hold", e_pitch_wide);
	refused(NULL, EINVAL, "a turned destination's pitch shorter than its row", e_turned_pitch);
	refused(NULL, EOVERFLOW, "a source whose end wraps", e_wrap);
	refused(NULL, ENOTUNIQ, "a destination over the source", e_overlap);
	refused(NULL, ENOTUNIQ, "a second plane's destination over the first's", e_nv12_overlap);
	refused(NULL, EACCES, "a destination in the pool", e_pool);
	refused(NULL, EINVAL, "two turns", e_two_turns);
	refused(NULL, EINVAL, "no turn", e_no_turn);
	refused(NULL, EINVAL, "an unknown rotation bit", e_unknown_bit);
	refused(NULL, EINVAL, "an unknown format", e_format);
	refused(NULL, EINVAL, "a plane count not the format's", e_planes);
	refused(NULL, EINVAL, "a non-zero plane the format hasn't", e_spare_plane);
	refused(NULL, EINVAL, "a width of 0", e_width0);
	refused(NULL, EINVAL, "a width the descriptor can't hold", e_width_big);
	refused(NULL, EINVAL, "a buffer past the stream's", e_buffer);

	/* Good jobs. */
	if (stream_open(&st)) {
		printf("FAIL stream for the good jobs: %s\n", strerror(errno));
		failures++;
		return;
	}
	CHECK(st.reach == 0x80000000ull);
	if (board)
		printf("reach %#llx, image at %#llx\n", (unsigned long long)st.reach,
		       (unsigned long long)st.region_addr);
	CHECK(board || !ran(&jobs0, &errors, word0));
	good_argb(&st, LEMNOS_GSDMA_ROTATE_0, 1);
	good_argb(&st, LEMNOS_GSDMA_ROTATE_90, 2);
	if (!board) {
		CHECK(!ran(&jobs, &errors, word0));
		CHECK(jobs == jobs0 + 2);
		CHECK(errors == 0);
		/* The last was one plane, a quarter turn, 32-bit elements, interrupt at its end. */
		CHECK(word0[0] == (1u | 3u << 8 | 1u << 30));
	}

	/* NV12, mirrored across: two descriptors, the interrupt on the second. */
	j = argb_job(&st, 8, 4, LEMNOS_GSDMA_ROTATE_0 | LEMNOS_GSDMA_REFLECT_X);
	e_nv12_overlap(&st, &j);
	j.plane[1].dst_addr = st.region_addr + DST_OFFSET + 4096;
	j.identity = 3;
	post(&st, &j);
	CHECK(kick(&st) == 0);
	CHECK(wait_frame(&st, &id, 2000) == BUFFER_BYTES);
	CHECK(id == 3);
	CHECK(block_status(&st).finished >= 3);
	if (board)
		goto one_stream;
	CHECK(!ran(&jobs, &errors, word0));
	CHECK(word0[0] == (1u << 2));
	CHECK(word0[1] == (1u << 2 | 1u << 8 | 1u << 30));

	/* A job that never finishes: stopped, published empty, and the stream goes on. */
	CHECK(!put_sys("hang", "20"));
	j = argb_job(&st, 8, 4, LEMNOS_GSDMA_ROTATE_0);
	j.identity = 4;
	post(&st, &j);
	CHECK(kick(&st) == 0);
	CHECK(wait_frame(&st, &id, 3000) == 0);
	CHECK(id == 4);
	CHECK(block_status(&st).timeouts == 1);
	CHECK(block_status(&st).wedged == 0);
	CHECK(status_of(&st).state == LEMNOS_RING_STATE_OPEN);
	CHECK(!put_sys("hang", "0"));
	good_argb(&st, LEMNOS_GSDMA_ROTATE_0, 5);
	CHECK(status_of(&st).state == LEMNOS_RING_STATE_OPEN);

one_stream:
	/* One stream at a time. */
	{
		struct stream second;

		CHECK(stream_open(&second) && errno == EBUSY);
		stream_close(&second);
	}
	stream_close(&st);
	/* Closed: the block takes a stream again. */
	CHECK(!stream_open(&st));
	good_argb(&st, LEMNOS_GSDMA_ROTATE_0, 6);
	stream_close(&st);
}

/* With the reach lowered below every buffer: every image is past it. */
static void run_reach(void)
{
	struct stream st;

	if (stream_open(&st)) {
		printf("FAIL stream: %s\n", strerror(errno));
		failures++;
		return;
	}
	CHECK(st.reach < st.region_addr);
	stream_close(&st);
	refused(NULL, ENXIO, "an image past the engine's reach", NULL);
}

/* An engine whose progress never stops: wedged, and its job's memory stays held. */
static void run_wedge(void)
{
	struct stream st;
	struct lemnos_gsdma_job j;
	int region;

	if (stream_open(&st)) {
		printf("FAIL stream: %s\n", strerror(errno));
		failures++;
		return;
	}
	CHECK(!put_sys("hang", "4294967295"));
	j = argb_job(&st, 8, 4, LEMNOS_GSDMA_ROTATE_0);
	post(&st, &j);
	CHECK(kick(&st) == 0);
	for (int t = 0; t < 200 && status_of(&st).state == LEMNOS_RING_STATE_OPEN; t++)
		usleep(5000);
	{
		struct lemnos_gsdma_status b = block_status(&st);
		struct lemnos_stream_status s = status_of(&st);
		unsigned int jobs, errors, word0[3];

		ran(&jobs, &errors, word0);
		if (s.state != LEMNOS_RING_STATE_FAULT || s.fault != EIO || b.wedged != 1)
			printf("FAIL not wedged: state %u fault %d jobs %llu frames %llu; wedged %u "
			       "finished %llu timeouts %llu; engine ran %u\n",
			       s.state, s.fault, (unsigned long long)s.jobs,
			       (unsigned long long)s.frames, b.wedged, (unsigned long long)b.finished,
			       (unsigned long long)b.timeouts, jobs), failures++;
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
		fprintf(stderr, "usage: %s /dev/lemnos <device> <heap> <run|board|reach|wedge>\n",
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
	if (!strcmp(argv[4], "run") || board)
		run_all();
	else if (!strcmp(argv[4], "reach"))
		run_reach();
	else if (!strcmp(argv[4], "wedge"))
		run_wedge();
	else
		return 2;
	printf("lemnos_gsdma_check %s: %d failed\n", argv[4], failures);
	return failures ? 1 : 0;
}
