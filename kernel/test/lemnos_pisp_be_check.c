// SPDX-License-Identifier: GPL-2.0
/*
 * The back end job checks, with a fake block in place of the hardware (test
 * block "lemnos-test-pisp", module parameter test_pb_fake_regs).
 *
 * Every field that names memory, every size and every index is tried out of
 * bounds, wrapped and disagreeing with its format, on a job that is otherwise
 * good, and the exact errno is required. Accepted jobs have their registers
 * read back: what the block is given has to be what was checked, even with
 * the config slot being rewritten while the job is in flight, and a config
 * flipped from valid to invalid while the check runs.
 *
 * Interrupt accounting: counters beyond the jobs held wedge the block rather
 * than finishing anything. Teardown: closing or unbinding with a job on the
 * block waits for it; a block that never finishes is wedged and its buffers are
 * never released (their reference count stays up after the stream is gone, seen
 * in /proc/self/fdinfo).
 *
 * Usage: lemnos_pisp_be_check /dev/lemnos <device> <bind> <unbind> <heap> <refuse|lifetime|wedge>
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <errno.h>
#include <fcntl.h>
#include <linux/dma-heap.h>
#include <poll.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include "pisp_be_config.h"
#include "../lemnos.h"
#include "../lemnos_pisp_be.h"
#include "find_block.h"
#include "lemnos_pisp_be_fixture.h"

/* The same numbers as drivers/pisp-sys's layout test, so the two sides are pinned to each other. */
_Static_assert(sizeof(struct lemnos_pisp_be_job) == 120, "job");
_Static_assert(offsetof(struct lemnos_pisp_be_job, addr) == 16, "job addr");
_Static_assert(sizeof(struct lemnos_pisp_be_create) == 80, "create");
_Static_assert(offsetof(struct lemnos_pisp_be_create, config_mmap_offset) == 64, "create offset");
_Static_assert(offsetof(struct lemnos_pisp_be_create, config_bytes) == 72, "create bytes");
_Static_assert(sizeof(struct lemnos_pisp_be_status) == 24, "status");
_Static_assert(LEMNOS_IOC_PISP_BE_CREATE == 0xc0504cb0u, "create request");
_Static_assert(LEMNOS_PISP_BE_IOC_STATUS == 0x80184cb1u, "status request");
_Static_assert(sizeof(struct pisp_be_tiles_config) == 16720, "config slot");

#define BUFFERS 4u
#define BUFFER_BYTES (256u * 1024)
#define REGION_BYTES (4u * 1024 * 1024)
#define FRAME_SLOTS 8u
#define JOB_SLOTS 4u
#define JOB_BYTES ((uint32_t)sizeof(struct lemnos_pisp_be_job))
#define SYS "/sys/bus/platform/devices/lemnos-test-pisp"
#define W 64
#define H 32

/* Registers the fake block exposes. */
#define BE_STATUS 0x10
#define BE_BATCH 0x14
#define BE_INT_STATUS 0x1c
#define BE_CONTROL 0x04
#define BE_TILE_LO 0x08
#define BE_CONFIG 0x40

static int failures, ctl_fd;
static __u32 block_id;
static char sysdir[256];

static void check(int ok, const char *what)
{
	if (!ok) {
		printf("FAIL %s\n", what);
		failures++;
	}
}

#define CHECK(c) check((c), #c)

static void poke(unsigned int off, unsigned int value)
{
	char path[300], buf[64];
	int fd;

	snprintf(path, sizeof(path), "%s/poke", sysdir);
	fd = open(path, O_WRONLY | O_CLOEXEC);
	int n = snprintf(buf, sizeof(buf), "%x %x", off, value);

	if (fd < 0 || write(fd, buf, n) != n)
		printf("FAIL poke %x\n", off), failures++;
	if (fd >= 0)
		close(fd);
}

static void raise_irq(void)
{
	char path[300];
	int fd;

	snprintf(path, sizeof(path), "%s/irq", sysdir);
	fd = open(path, O_WRONLY | O_CLOEXEC);
	if (fd < 0 || write(fd, "1", 1) != 1)
		printf("FAIL irq\n"), failures++;
	if (fd >= 0)
		close(fd);
}

static void read_regs(void *buf, size_t n)
{
	char path[300];
	int fd;

	snprintf(path, sizeof(path), "%s/regs", sysdir);
	fd = open(path, O_RDONLY | O_CLOEXEC);
	/* A binary attribute gives at most a page per read. */
	for (size_t at = 0; fd >= 0 && at < n;) {
		ssize_t got = pread(fd, (char *)buf + at, n - at, (off_t)at);

		if (got <= 0) {
			printf("FAIL regs\n"), failures++;
			break;
		}
		at += (size_t)got;
	}
	if (fd >= 0)
		close(fd);
}

static uint32_t reg32(const void *regs, unsigned int off)
{
	uint32_t v;

	memcpy(&v, (const char *)regs + off, 4);
	return v;
}

/* The address of the tiles the block was last given, from its registers. */
static uint64_t last_tiles(void)
{
	static char regs[16];

	read_regs(regs, sizeof(regs));
	return reg32(regs, BE_TILE_LO) | ((uint64_t)reg32(regs, BE_TILE_LO + 4) << 32);
}

/* The page's reference count: 0 once the kernel has freed it. */
static int page_refs(uint64_t addr)
{
	char path[320], text[32];
	int fd, n;

	snprintf(path, sizeof(path), "%s/page_refs", sysdir);
	fd = open(path, O_WRONLY | O_CLOEXEC);
	n = snprintf(text, sizeof(text), "%llx", (unsigned long long)addr);
	if (fd < 0 || write(fd, text, n) != n)
		return -2;
	close(fd);
	fd = open(path, O_RDONLY | O_CLOEXEC);
	n = fd < 0 ? -1 : read(fd, text, sizeof(text) - 1);
	if (n <= 0)
		return -2;
	close(fd);
	text[n] = 0;
	return atoi(text);
}


/* --- a stream --- */

struct stream {
	int fd, pool_fd, region_fd;
	uint64_t *ring;
	size_t ring_bytes;
	void *configs;
	size_t config_bytes;
	uint64_t pool_addr, region_addr;
	uint32_t region;
	size_t returns_at, jobs_at, job_words;
	uint64_t jobs_head, frames_tail, returns_head;
	/* What the fake hardware says it has done. */
	uint8_t hw_started, hw_done;
};

static int heap_alloc(int heap, uint64_t len)
{
	struct dma_heap_allocation_data d = { .len = len, .fd_flags = O_RDWR | O_CLOEXEC };

	return ioctl(heap, DMA_HEAP_IOCTL_ALLOC, &d) ? -1 : (int)d.fd;
}

static int heap_fd;

static int stream_open(struct stream *st)
{
	struct lemnos_pisp_be_create c;
	struct lemnos_stream_register reg;

	memset(st, 0, sizeof(*st));
	st->pool_fd = heap_alloc(heap_fd, (uint64_t)BUFFERS * BUFFER_BYTES);
	st->region_fd = heap_alloc(heap_fd, REGION_BYTES);
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
	if (ioctl(ctl_fd, LEMNOS_IOC_PISP_BE_CREATE, &c))
		return -1;
	st->fd = c.stream.fd;
	st->ring_bytes = c.stream.ring_bytes;
	st->pool_addr = c.stream.pool_addr;
	st->config_bytes = c.config_bytes;
	st->ring = mmap(NULL, st->ring_bytes, PROT_READ | PROT_WRITE, MAP_SHARED, st->fd, 0);
	st->configs = mmap(NULL, st->config_bytes, PROT_READ | PROT_WRITE, MAP_SHARED, st->fd,
			   c.config_mmap_offset);
	if (st->ring == MAP_FAILED || st->configs == MAP_FAILED)
		return -1;
	st->returns_at = LEMNOS_RING_FRAMES + FRAME_SLOTS * LEMNOS_STREAM_FRAME_WORDS;
	st->jobs_at = st->returns_at + FRAME_SLOTS;
	st->job_words = 1 + (JOB_BYTES + 7) / 8;

	memset(&reg, 0, sizeof(reg));
	reg.fd = st->region_fd;
	if (ioctl(st->fd, LEMNOS_STREAM_IOC_REGISTER, &reg))
		return -1;
	st->region_addr = reg.addr;
	st->region = reg.region;
	{
		/* The fake block's counters carry on across streams, as a real one's do. */
		static char regs[64];

		read_regs(regs, sizeof(regs));
		st->hw_done = (uint8_t)reg32(regs, BE_BATCH);
		st->hw_started = (uint8_t)(reg32(regs, BE_BATCH) >> 8);
	}
	return 0;
}

static void stream_close(struct stream *st)
{
	if (st->ring != NULL && st->ring != MAP_FAILED)
		munmap(st->ring, st->ring_bytes);
	if (st->configs != NULL && st->configs != MAP_FAILED)
		munmap(st->configs, st->config_bytes);
	if (st->fd > 0)
		close(st->fd);
	if (st->region_fd > 0)
		close(st->region_fd);
	if (st->pool_fd > 0)
		close(st->pool_fd);
	memset(st, 0, sizeof(*st));
}

static void put_config(struct stream *st, uint32_t slot, const struct pisp_be_tiles_config *c)
{
	memcpy((char *)st->configs + (size_t)slot * sizeof(*c), c, sizeof(*c));
}

/* Posts `job` as the next slot of the ring. */
static void post(struct stream *st, const struct lemnos_pisp_be_job *job)
{
	size_t at = st->jobs_at + (st->jobs_head & (JOB_SLOTS - 1)) * st->job_words;
	uint64_t words[sizeof(*job) / 8];

	memcpy(words, job, sizeof(*job));
	st->ring[at] = sizeof(*job);
	for (size_t i = 0; i < sizeof(*job) / 8; i++)
		st->ring[at + 1 + i] = words[i];
	st->jobs_head++;
	__atomic_thread_fence(__ATOMIC_RELEASE);
	st->ring[LEMNOS_RING_JOBS_HEAD] = st->jobs_head;
}

static int kick(struct stream *st)
{
	return ioctl(st->fd, LEMNOS_STREAM_IOC_KICK) ? errno : 0;
}

static int status_of(struct stream *st, struct lemnos_stream_status *out)
{
	return ioctl(st->fd, LEMNOS_STREAM_IOC_STATUS, out);
}

/* The hardware says it started and finished this job. */
static void complete_job(struct stream *st, int started, int done)
{
	st->hw_started += started;
	st->hw_done += done;
	poke(BE_BATCH, (uint32_t)st->hw_done | ((uint32_t)st->hw_started << 8));
	poke(BE_INT_STATUS, 1);
	raise_irq();
}

/* The next frame: its buffer, or -1 if none arrives in time. */
static int wait_frame(struct stream *st, uint64_t *identity)
{
	for (int tries = 0; tries < 1000; tries++) {
		uint64_t head = __atomic_load_n(&st->ring[LEMNOS_RING_FRAMES_HEAD], __ATOMIC_ACQUIRE);

		if (head != st->frames_tail) {
			size_t at = LEMNOS_RING_FRAMES +
				    (st->frames_tail & (FRAME_SLOTS - 1)) * LEMNOS_STREAM_FRAME_WORDS;
			uint64_t word = st->ring[at];
			uint64_t fence = st->ring[at + 4];

			if (identity)
				*identity = st->ring[at + 2];
			st->frames_tail++;
			__atomic_store_n(&st->ring[LEMNOS_RING_FRAMES_TAIL], st->frames_tail,
					 __ATOMIC_RELEASE);
			/* Give it back so the next job can have it. */
			uint32_t buffer = (uint32_t)word;
			size_t ret_at = st->returns_at + (st->returns_head & (FRAME_SLOTS - 1));

			st->ring[ret_at] = (uint64_t)buffer | fence << 32;
			st->returns_head++;
			__atomic_store_n(&st->ring[LEMNOS_RING_RETURNS_HEAD], st->returns_head,
					 __ATOMIC_RELEASE);
			return (int)buffer;
		}
		usleep(2000);
	}
	return -1;
}

/* The reference count of a dma-buf, from its fdinfo: above 1 means the kernel still holds it. */
static long dmabuf_count(int fd)
{
	char path[64], text[256];
	int f;

	snprintf(path, sizeof(path), "/proc/self/fdinfo/%d", fd);
	f = open(path, O_RDONLY | O_CLOEXEC);
	if (f < 0)
		return -1;
	ssize_t n = read(f, text, sizeof(text) - 1);

	close(f);
	if (n <= 0)
		return -1;
	text[n] = 0;
	char *p = strstr(text, "count:");

	return p ? strtol(p + 6, NULL, 10) : -1;
}

/* --- good configs --- */

/* An RGB888 image in, an RGB888 image out, one tile. Slots: input 0 and output 0. */
static void cfg_rgb(struct pisp_be_tiles_config *c, uint64_t *addr, const struct stream *st)
{
	struct pisp_tile *t = &c->tiles[0];

	memset(c, 0, sizeof(*c));
	memset(addr, 0, sizeof(uint64_t) * LEMNOS_PISP_BE_ADDRS);
	c->num_tiles = 1;
	c->config.global.bayer_enables = 0;
	c->config.global.rgb_enables = PISP_BE_RGB_ENABLE_INPUT | PISP_BE_RGB_ENABLE_OUTPUT0;
	c->config.input_format = (struct pisp_image_format_config){
		.width = W, .height = H,
		.format = PISP_IMAGE_FORMAT_THREE_CHANNEL | PISP_IMAGE_FORMAT_BPS_8,
		.stride = 192,
	};
	c->config.output_format[0].image = (struct pisp_image_format_config){
		.width = W, .height = H,
		.format = PISP_IMAGE_FORMAT_THREE_CHANNEL | PISP_IMAGE_FORMAT_BPS_8,
		.stride = 192,
	};
	t->input_width = W;
	t->input_height = H;
	t->output_width[0] = W;
	t->output_height[0] = H;
	addr[0] = st->region_addr;
	addr[LEMNOS_PISP_BE_ADDR_OUTPUT(0, 0)] = st->pool_addr;
}

/* Bayer in with TDN and stitch, YUV420 planar on both outputs: every slot but input 1, 2 used. */
static void cfg_bayer_full(struct pisp_be_tiles_config *c, uint64_t *addr, const struct stream *st)
{
	struct pisp_image_format_config bayer = {
		.width = W, .height = H,
		.format = PISP_IMAGE_FORMAT_BPS_16,
		.stride = 128,
	};
	struct pisp_image_format_config yuv = {
		.width = W, .height = H,
		.format = PISP_IMAGE_FORMAT_THREE_CHANNEL | PISP_IMAGE_FORMAT_BPS_8 |
			  PISP_IMAGE_FORMAT_PLANARITY_PLANAR | PISP_IMAGE_FORMAT_SAMPLING_420,
		.stride = 64,
		.stride2 = 32,
	};
	struct pisp_tile *t = &c->tiles[0];
	uint64_t r = st->region_addr;

	memset(c, 0, sizeof(*c));
	memset(addr, 0, sizeof(uint64_t) * LEMNOS_PISP_BE_ADDRS);
	c->num_tiles = 1;
	c->config.global.bayer_enables = PISP_BE_BAYER_ENABLE_INPUT | PISP_BE_BAYER_ENABLE_TDN_INPUT |
					 PISP_BE_BAYER_ENABLE_TDN | PISP_BE_BAYER_ENABLE_TDN_OUTPUT |
					 PISP_BE_BAYER_ENABLE_STITCH_INPUT | PISP_BE_BAYER_ENABLE_STITCH |
					 PISP_BE_BAYER_ENABLE_STITCH_OUTPUT;
	c->config.global.rgb_enables = PISP_BE_RGB_ENABLE_OUTPUT0 | PISP_BE_RGB_ENABLE_OUTPUT1;
	c->config.input_format = bayer;
	c->config.tdn_input_format = bayer;
	c->config.tdn_output_format = bayer;
	c->config.stitch_input_format = bayer;
	c->config.stitch_output_format = bayer;
	c->config.output_format[0].image = yuv;
	c->config.output_format[1].image = yuv;
	t->input_width = W;
	t->input_height = H;
	t->output_width[0] = W;
	t->output_height[0] = H;
	t->output_width[1] = W;
	t->output_height[1] = H;

	uint64_t bayer_bytes = 128 * H, y_bytes = 64 * H, c_bytes = 32 * (H + 1) / 2;

#define NEXT(bytes) ({ uint64_t a = r; r += ((bytes) + 15) & ~15ull; a; })
	addr[0] = NEXT(bayer_bytes);
	addr[3] = NEXT(bayer_bytes);
	addr[4] = NEXT(bayer_bytes);
	addr[5] = NEXT(bayer_bytes);
	addr[6] = NEXT(bayer_bytes);
	/* Output 0 in the job's pool buffer, output 1 in the region. */
	addr[LEMNOS_PISP_BE_ADDR_OUTPUT(0, 0)] = st->pool_addr;
	addr[LEMNOS_PISP_BE_ADDR_OUTPUT(0, 1)] = st->pool_addr + y_bytes;
	addr[LEMNOS_PISP_BE_ADDR_OUTPUT(0, 2)] = st->pool_addr + y_bytes + ((c_bytes + 15) & ~15ull);
	addr[LEMNOS_PISP_BE_ADDR_OUTPUT(1, 0)] = NEXT(y_bytes);
	addr[LEMNOS_PISP_BE_ADDR_OUTPUT(1, 1)] = NEXT(c_bytes);
	addr[LEMNOS_PISP_BE_ADDR_OUTPUT(1, 2)] = NEXT(c_bytes);
}

/* YUV420 planar in on the RGB side: the only way input planes 1 and 2 are used. */
static void cfg_yuv_in(struct pisp_be_tiles_config *c, uint64_t *addr, const struct stream *st)
{
	struct pisp_image_format_config yuv = {
		.width = W, .height = H,
		.format = PISP_IMAGE_FORMAT_THREE_CHANNEL | PISP_IMAGE_FORMAT_BPS_8 |
			  PISP_IMAGE_FORMAT_PLANARITY_PLANAR | PISP_IMAGE_FORMAT_SAMPLING_420,
		.stride = 64,
		.stride2 = 32,
	};

	cfg_rgb(c, addr, st);
	c->config.input_format = yuv;
	addr[0] = st->region_addr;
	addr[1] = st->region_addr + 64 * H;
	addr[2] = st->region_addr + 64 * H + 32 * (H + 1) / 2 + 16;
	addr[2] &= ~15ull;
}

/* --- the refusals --- */

typedef void (*mutate_fn)(struct pisp_be_tiles_config *c, uint64_t *addr, const struct stream *st);

/* Posts a job built from `base` with `mutate` applied and requires this errno from KICK. */
static void refuse(const char *what, mutate_fn base, mutate_fn mutate, int expected)
{
	struct pisp_be_tiles_config c;
	uint64_t addr[LEMNOS_PISP_BE_ADDRS];
	struct stream st;
	struct lemnos_pisp_be_job job = { .identity = 1, .buffer = 0, .config = 0 };

	if (stream_open(&st)) {
		printf("FAIL %s: stream_open %s\n", what, strerror(errno));
		failures++;
		return;
	}
	base(&c, addr, &st);
	mutate(&c, addr, &st);
	put_config(&st, 0, &c);
	memcpy(job.addr, addr, sizeof(addr));
	post(&st, &job);
	int got = kick(&st);

	if (got != expected) {
		printf("FAIL %s: errno %d (%s), wanted %d (%s)\n", what, got, strerror(got), expected,
		       strerror(expected));
		failures++;
	} else {
		printf("ok   refused: %s\n", what);
	}
	stream_close(&st);
}

/* A refusal test as a statement on the config `c` and addresses `addr` of an otherwise good job. */
static struct pisp_be_tiles_config refuse_cfg;

static int refuse_begin(struct stream *st, const char *what)
{
	if (!stream_open(st))
		return 1;
	printf("FAIL %s: stream_open %s\n", what, strerror(errno));
	failures++;
	return 0;
}

static void refuse_end(struct stream *st, const uint64_t *addr, const char *what, int expected)
{
	struct lemnos_pisp_be_job job = { .identity = 1, .buffer = 0, .config = 0 };

	put_config(st, 0, &refuse_cfg);
	memcpy(job.addr, addr, sizeof(job.addr));
	post(st, &job);
	int got = kick(st);

	if (got != expected) {
		printf("FAIL %s: errno %d (%s), wanted %d (%s)\n", what, got, strerror(got), expected,
		       strerror(expected));
		failures++;
	} else {
		printf("ok   refused: %s\n", what);
	}
	stream_close(st);
}

#define REFUSE(base, statement, expected) do {						\
	struct pisp_be_tiles_config *c = &refuse_cfg;					\
	uint64_t addr[LEMNOS_PISP_BE_ADDRS];						\
	struct stream one;								\
											\
	if (refuse_begin(&one, #statement)) {						\
		base(c, addr, &one);							\
		statement;								\
		refuse_end(&one, addr, #statement, expected);				\
	}										\
} while (0)

/* Mutations, one per kind of field. */
static int g_slot;
static uint64_t g_value;
static void set_slot(struct pisp_be_tiles_config *c, uint64_t *addr, const struct stream *st)
{
	(void)c; (void)st;
	addr[g_slot] = g_value;
}

static void slot_out_of_region(struct pisp_be_tiles_config *c, uint64_t *addr, const struct stream *st)
{
	(void)c;
	addr[g_slot] = st->region_addr + REGION_BYTES + (1ull << 33);	/* past all of RAM */
}

static void slot_overrun_region(struct pisp_be_tiles_config *c, uint64_t *addr, const struct stream *st)
{
	(void)c;
	addr[g_slot] = st->region_addr + REGION_BYTES - 16;
}

static void slot_other_buffer(struct pisp_be_tiles_config *c, uint64_t *addr, const struct stream *st)
{
	(void)c;
	addr[g_slot] = st->pool_addr + BUFFER_BYTES;
}

static void slot_misaligned(struct pisp_be_tiles_config *c, uint64_t *addr, const struct stream *st)
{
	(void)c; (void)st;
	addr[g_slot] += 1;
}

static void slot_wraps(struct pisp_be_tiles_config *c, uint64_t *addr, const struct stream *st)
{
	(void)c; (void)st;
	addr[g_slot] = ~(uint64_t)0 - 15;	/* aligned, but addr + len wraps */
}

static void slot_zero(struct pisp_be_tiles_config *c, uint64_t *addr, const struct stream *st)
{
	(void)c; (void)st;
	addr[g_slot] = 0;
}

static void slot_valid(struct pisp_be_tiles_config *c, uint64_t *addr, const struct stream *st)
{
	(void)c;
	addr[g_slot] = st->region_addr;
}

/* The tile fields, picked by g_slot. */
static void tile_offset(struct pisp_be_tiles_config *c, uint64_t *addr, const struct stream *st)
{
	struct pisp_tile *t = &c->tiles[0];

	(void)addr; (void)st;
	switch (g_slot) {
	case 0: t->input_addr_offset = (uint32_t)g_value; break;
	case 1: t->input_addr_offset2 = (uint32_t)g_value; break;
	case 3: t->tdn_input_addr_offset = (uint32_t)g_value; break;
	case 4: t->stitch_input_addr_offset = (uint32_t)g_value; break;
	case 5: t->tdn_output_addr_offset = (uint32_t)g_value; break;
	case 6: t->stitch_output_addr_offset = (uint32_t)g_value; break;
	case 7: t->output_addr_offset[0] = (uint32_t)g_value; break;
	case 8: t->output_addr_offset2[0] = (uint32_t)g_value; break;
	case 10: t->output_addr_offset[1] = (uint32_t)g_value; break;
	case 11: t->output_addr_offset2[1] = (uint32_t)g_value; break;
	}
}

static const int used_slots[] = { 0, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12 };
static const int yuv_slots[] = { 0, 1, 2 };
static const int tile_slots[] = { 0, 1, 3, 4, 5, 6, 7, 8, 10, 11 };

static void refuse_addresses(void)
{
	char what[80];

	for (size_t i = 0; i < sizeof(used_slots) / sizeof(*used_slots); i++) {
		g_slot = used_slots[i];
		snprintf(what, sizeof(what), "addr[%d] past the region", g_slot);
		refuse(what, cfg_bayer_full, slot_out_of_region, EFAULT);
		snprintf(what, sizeof(what), "addr[%d] extent overruns the region", g_slot);
		refuse(what, cfg_bayer_full, slot_overrun_region, EFAULT);
		snprintf(what, sizeof(what), "addr[%d] missing", g_slot);
		refuse(what, cfg_bayer_full, slot_zero, EINVAL);
		snprintf(what, sizeof(what), "addr[%d] wraps", g_slot);
		refuse(what, cfg_bayer_full, slot_wraps, EOVERFLOW);
		snprintf(what, sizeof(what), "addr[%d] misaligned", g_slot);
		refuse(what, cfg_bayer_full, slot_misaligned, EINVAL);
	}
	/* Input planes 1 and 2, which only a YUV input has. */
	for (size_t i = 0; i < sizeof(yuv_slots) / sizeof(*yuv_slots); i++) {
		g_slot = yuv_slots[i];
		snprintf(what, sizeof(what), "yuv input addr[%d] past the region", g_slot);
		refuse(what, cfg_yuv_in, slot_out_of_region, EFAULT);
		snprintf(what, sizeof(what), "yuv input addr[%d] extent overruns", g_slot);
		refuse(what, cfg_yuv_in, slot_overrun_region, EFAULT);
	}

	/* Disabled stages and planes a format doesn't have: never live. */
	static const int unused_in_rgb[] = { 1, 2, 3, 4, 5, 6, 8, 9, 10, 11, 12 };

	for (size_t i = 0; i < sizeof(unused_in_rgb) / sizeof(*unused_in_rgb); i++) {
		g_slot = unused_in_rgb[i];
		snprintf(what, sizeof(what), "addr[%d] on a disabled stage", g_slot);
		refuse(what, cfg_rgb, slot_valid, EINVAL);
	}

	/* A range in another buffer of the pool. */
	g_slot = LEMNOS_PISP_BE_ADDR_OUTPUT(0, 0);
	refuse("output in another pool buffer", cfg_rgb, slot_other_buffer, EACCES);
	g_value = ~(uint64_t)0 - 15;
	refuse("output addr wraps", cfg_rgb, set_slot, EOVERFLOW);
}

static void refuse_tiles(void)
{
	char what[80];

	for (size_t i = 0; i < sizeof(tile_slots) / sizeof(*tile_slots); i++) {
		g_slot = tile_slots[i];
		g_value = 0xffffff00u;
		mutate_fn base = g_slot == 1 ? cfg_yuv_in : cfg_bayer_full;

		snprintf(what, sizeof(what), "tile offset for addr[%d] past its plane", g_slot);
		refuse(what, base, tile_offset, EFAULT);
	}
	/* Just past the last byte the tile would touch, at the bottom of the plane. */
	g_slot = 0;
	g_value = 128;
	refuse("input tile hangs off the plane by a line", cfg_bayer_full, tile_offset, EFAULT);
}

static void refuse_formats(void)
{
	REFUSE(cfg_rgb, c->config.input_format.stride = 96, EINVAL);	/* less than a row */
	REFUSE(cfg_rgb, c->config.input_format.stride = 193, EINVAL);	/* misaligned */
	REFUSE(cfg_rgb, c->config.input_format.stride = -192, EOPNOTSUPP);
	REFUSE(cfg_rgb, c->config.input_format.stride = 0, EINVAL);
	REFUSE(cfg_rgb, c->config.input_format.stride2 = 64, EINVAL);	/* a plane it doesn't have */
	REFUSE(cfg_rgb, c->config.input_format.width = 0, EINVAL);
	REFUSE(cfg_rgb, c->config.input_format.height = 0, EINVAL);
	REFUSE(cfg_rgb, c->config.input_format.format |= 1u << 31, EINVAL);
	REFUSE(cfg_rgb, c->config.input_format.format |= PISP_IMAGE_FORMAT_WALLPAPER_ROLL, EOPNOTSUPP);
	REFUSE(cfg_rgb, c->config.input_format.format |= PISP_IMAGE_FORMAT_HOG_SIGNED, EOPNOTSUPP);
	REFUSE(cfg_rgb, c->config.input_format.format |= PISP_IMAGE_FORMAT_INTEGRAL_IMAGE, EOPNOTSUPP);
	REFUSE(cfg_rgb, c->config.input_format.format = PISP_IMAGE_FORMAT_THREE_CHANNEL |
		PISP_IMAGE_FORMAT_SAMPLING_420, EOPNOTSUPP);	/* interleaved 4:2:0 */
	REFUSE(cfg_rgb, c->config.input_format.format = PISP_IMAGE_FORMAT_PLANARITY_PLANAR, EINVAL);	/* planar mono */
	REFUSE(cfg_rgb, c->config.input_format.format = PISP_IMAGE_FORMAT_BPS_16 |
		PISP_IMAGE_FORMAT_COMPRESSION_MODE_1, EOPNOTSUPP);	/* compressed needs 8 */
	REFUSE(cfg_yuv_in, c->config.input_format.stride2 = 0, EINVAL);	/* the second plane unset */
	REFUSE(cfg_yuv_in, c->config.input_format.stride2 = -32, EOPNOTSUPP);
	REFUSE(cfg_yuv_in, c->config.input_format.stride2 = 16, EINVAL);	/* shorter than a chroma row */
	REFUSE(cfg_rgb, c->config.output_format[0].image.stride = 193, EINVAL);	/* outputs need 16 */
	REFUSE(cfg_rgb, c->config.output_format[0].image.stride = 96, EINVAL);
	/* A taller image is a bigger extent: the image no longer fits its buffer's bytes. */
	REFUSE(cfg_rgb, c->config.input_format.height = 60000, EFAULT);
	REFUSE(cfg_rgb, c->config.output_format[0].image.stride = (1 << 30) - 64 + 64, EFAULT);
	REFUSE(cfg_rgb, c->tiles[0].input_width = 0, EINVAL);
	REFUSE(cfg_rgb, c->tiles[0].input_height = 0, EINVAL);
	REFUSE(cfg_rgb, c->tiles[0].input_offset_x = 1, EINVAL);	/* past the image width */
	REFUSE(cfg_rgb, c->tiles[0].input_offset_y = 1, EINVAL);
	REFUSE(cfg_rgb, c->tiles[0].output_width[0] = W + 16, EINVAL);
	REFUSE(cfg_rgb, c->tiles[0].output_offset_y[0] = 8, EINVAL);
	REFUSE(cfg_rgb, c->tiles[0].output_height[0] = 0, EINVAL);	/* width without height */
}

static void refuse_enables(void)
{
	REFUSE(cfg_rgb, c->num_tiles = 0, EINVAL);
	/* Every real tile good, so only the count can refuse it (a walk past the array is KASAN's). */
	REFUSE(cfg_rgb, for (int i = 1; i < PISP_BACK_END_NUM_TILES; i++) c->tiles[i] = c->tiles[0];
		c->num_tiles = PISP_BACK_END_NUM_TILES + 1, EINVAL);
	REFUSE(cfg_rgb, c->num_tiles = 0x80000000u, EINVAL);
	REFUSE(cfg_rgb, c->config.global.rgb_enables &= ~PISP_BE_RGB_ENABLE_INPUT, EINVAL);
	REFUSE(cfg_rgb, c->config.global.bayer_enables = PISP_BE_BAYER_ENABLE_INPUT, EINVAL);	/* both */
	REFUSE(cfg_rgb, c->config.global.bayer_enables = PISP_BE_BAYER_ENABLE_TDN, EINVAL);	/* Bayer stage, no Bayer input */
	REFUSE(cfg_rgb, c->config.global.rgb_enables |= 0x40u, EINVAL);		/* a bit that isn't there */
	REFUSE(cfg_rgb, c->config.global.rgb_enables |= PISP_BE_RGB_ENABLE_HOG, EOPNOTSUPP);
	REFUSE(cfg_bayer_full, c->config.global.bayer_enables &= ~PISP_BE_BAYER_ENABLE_TDN, EINVAL);	/* TDN input without TDN */
	REFUSE(cfg_bayer_full, c->config.global.bayer_enables &= ~PISP_BE_BAYER_ENABLE_TDN_INPUT, EINVAL);	/* TDN, no input, not reset */
	REFUSE(cfg_bayer_full, c->config.tdn.reset = 1, EINVAL);	/* reset with an input */
	REFUSE(cfg_bayer_full, c->config.global.bayer_enables &= ~PISP_BE_BAYER_ENABLE_STITCH, EINVAL);
	REFUSE(cfg_bayer_full, c->config.global.bayer_enables &= ~PISP_BE_BAYER_ENABLE_STITCH_INPUT, EINVAL);
	REFUSE(cfg_bayer_full, c->config.global.bayer_enables |= PISP_BE_BAYER_ENABLE_STITCH_COMPRESS;
		c->config.global.bayer_enables &= ~PISP_BE_BAYER_ENABLE_STITCH_OUTPUT, EINVAL);
	/* Compressing is in 8-pixel units: the format has to say so or the sizes are wrong. */
	REFUSE(cfg_bayer_full, c->config.global.bayer_enables |= PISP_BE_BAYER_ENABLE_DECOMPRESS, EINVAL);
	REFUSE(cfg_bayer_full, c->config.input_format.format = PISP_IMAGE_FORMAT_BPS_8 |
		PISP_IMAGE_FORMAT_COMPRESSION_MODE_1, EINVAL);	/* compressed without DECOMPRESS */
	REFUSE(cfg_bayer_full, c->config.input_format.format = PISP_IMAGE_FORMAT_BPS_8, EOPNOTSUPP);	/* stalls the block */
	REFUSE(cfg_bayer_full, c->config.global.bayer_enables |= PISP_BE_BAYER_ENABLE_TDN_DECOMPRESS, EINVAL);
	REFUSE(cfg_bayer_full, c->config.global.bayer_enables |= PISP_BE_BAYER_ENABLE_TDN_COMPRESS, EINVAL);
	REFUSE(cfg_bayer_full, c->config.global.bayer_enables |= PISP_BE_BAYER_ENABLE_STITCH_DECOMPRESS, EINVAL);
	REFUSE(cfg_bayer_full, c->config.global.bayer_enables |= PISP_BE_BAYER_ENABLE_STITCH_COMPRESS, EINVAL);
	/* A vertical flip walks up from the offset, which the extent check doesn't cover. */
	REFUSE(cfg_rgb, c->config.output_format[0].transform = PISP_BE_TRANSFORM_VFLIP, EOPNOTSUPP);
	REFUSE(cfg_rgb, c->config.output_format[0].transform = 0x80, EOPNOTSUPP);
	/* Fields the module doesn't drive, even zero-valued fields it won't write. */
	REFUSE(cfg_rgb, ((uint8_t *)c)[0] = 1, EINVAL);		/* the addresses of older layouts */
	REFUSE(cfg_rgb, ((uint8_t *)c)[100] = 1, EINVAL);
	REFUSE(cfg_rgb, ((uint8_t *)c)[108] = 1, EINVAL);	/* the HOG address */
	REFUSE(cfg_rgb, c->config.axi.r_qos = 1, EINVAL);
	REFUSE(cfg_rgb, ((uint8_t *)&c->config)[sizeof(c->config) - 1] = 1, EINVAL);	/* the tail */
}

static void refuse_ring(void)
{
	struct pisp_be_tiles_config c;
	uint64_t addr[LEMNOS_PISP_BE_ADDRS];
	struct stream st;
	struct lemnos_pisp_be_job job = { .identity = 1, .buffer = 0, .config = 0 };
	struct lemnos_stream_status status;
	int got;

	/* A config slot past the last. */
	CHECK(!stream_open(&st));
	cfg_rgb(&c, addr, &st);
	put_config(&st, 0, &c);
	memcpy(job.addr, addr, sizeof(addr));
	job.config = JOB_SLOTS;
	post(&st, &job);
	got = kick(&st);
	CHECK(got == EINVAL);
	stream_close(&st);

	/* A buffer past the last. */
	CHECK(!stream_open(&st));
	cfg_rgb(&c, addr, &st);
	put_config(&st, 0, &c);
	memcpy(job.addr, addr, sizeof(addr));
	job.config = 0;
	job.buffer = BUFFERS;
	post(&st, &job);
	got = kick(&st);
	CHECK(got == EINVAL);
	stream_close(&st);

	/* A slot length that isn't the job's. */
	CHECK(!stream_open(&st));
	cfg_rgb(&c, addr, &st);
	put_config(&st, 0, &c);
	job.buffer = 0;
	post(&st, &job);
	st.ring[st.jobs_at] = sizeof(job) - 8;
	got = kick(&st);
	CHECK(got == EMSGSIZE);
	CHECK(!status_of(&st, &status) && status.state == LEMNOS_RING_STATE_FAULT &&
	      status.fault == EMSGSIZE);
	stream_close(&st);

	/* A head that can't be. */
	CHECK(!stream_open(&st));
	st.ring[LEMNOS_RING_JOBS_HEAD] = 1000;
	got = kick(&st);
	CHECK(got == ERANGE);
	stream_close(&st);

	/* A job naming a buffer already queued on the block. */
	CHECK(!stream_open(&st));
	cfg_rgb(&c, addr, &st);
	put_config(&st, 0, &c);
	job.buffer = 0;
	post(&st, &job);
	CHECK(!kick(&st));
	post(&st, &job);
	got = kick(&st);
	CHECK(got == EBUSY);
	complete_job(&st, 1, 1);
	CHECK(wait_frame(&st, NULL) == -1);	/* faulted streams publish nothing */
	stream_close(&st);

	/* The ring and config-slot mappings are shared; a private one is refused. */
	CHECK(!stream_open(&st));
	void *m = mmap(NULL, st.config_bytes, PROT_READ, MAP_PRIVATE, st.fd,
		       (off_t)0x1000 << 12);
	CHECK(m == MAP_FAILED);
	m = mmap(NULL, st.config_bytes / 2, PROT_READ, MAP_SHARED, st.fd, (off_t)0x1000 << 12);
	CHECK(m == MAP_FAILED);	/* partial mapping */
	m = mmap(NULL, st.config_bytes, PROT_READ | PROT_EXEC, MAP_SHARED, st.fd, (off_t)0x1000 << 12);
	CHECK(m == MAP_FAILED);
	stream_close(&st);

	/* Registers can't be mapped from a block this module drives. */
	struct lemnos_info info = { .block = block_id };

	CHECK(!ioctl(ctl_fd, LEMNOS_IOC_INFO, &info));
	CHECK(!(info.flags & LEMNOS_INFO_MMIO_MAPPABLE));
	CHECK(info.irqs == 0);
}

/* --- jobs that run --- */

static void test_runs_and_registers(void)
{
	static char regs[8192], regs2[8192];
	struct pisp_be_tiles_config c;
	uint64_t addr[LEMNOS_PISP_BE_ADDRS];
	struct stream st;
	struct lemnos_pisp_be_job job = { .identity = 0xabcd0001, .buffer = 2, .config = 1 };
	struct lemnos_stream_status status;
	uint64_t id;

	CHECK(!stream_open(&st));
	cfg_bayer_full(&c, addr, &st);
	addr[LEMNOS_PISP_BE_ADDR_OUTPUT(0, 0)] = st.pool_addr + 2 * (uint64_t)BUFFER_BYTES;
	addr[LEMNOS_PISP_BE_ADDR_OUTPUT(0, 1)] = st.pool_addr + 2 * (uint64_t)BUFFER_BYTES + 64 * H;
	addr[LEMNOS_PISP_BE_ADDR_OUTPUT(0, 2)] = st.pool_addr + 2 * (uint64_t)BUFFER_BYTES + 64 * H + 512;
	c.config.global.bayer_order = 0x5a;
	c.config.input_format.stride = 256;	/* wider than the image, fine */
	put_config(&st, 1, &c);
	memcpy(job.addr, addr, sizeof(addr));
	post(&st, &job);
	CHECK(kick(&st) == 0);

	read_regs(regs, sizeof(regs));
	for (int i = 0; i < 13; i++) {
		uint64_t got = reg32(regs, BE_CONFIG + 8 * i) |
			       (uint64_t)reg32(regs, BE_CONFIG + 8 * i + 4) << 32;

		if (got != addr[i]) {
			printf("FAIL slot %d: wrote %llx, asked %llx\n", i,
			       (unsigned long long)got, (unsigned long long)addr[i]);
			failures++;
		}
	}
	CHECK(reg32(regs, 0xb0) == c.config.global.bayer_enables);
	CHECK(reg32(regs, 0xb4) == c.config.global.rgb_enables);
	CHECK(reg32(regs, BE_CONFIG + offsetof(struct pisp_be_config, input_format) + 8) == 256);
	CHECK((reg32(regs, BE_CONFIG + offsetof(struct pisp_be_config, global) + 8) & 0xff) == 0x5a);
	CHECK(reg32(regs, BE_CONTROL) == (3u | (1u << 16)));	/* copy, queue, one tile */
	CHECK(reg32(regs, BE_TILE_LO) != 0);

	/* The config changes under the job: nothing the block has changes. */
	memset(st.configs, 0xff, sizeof(c) * JOB_SLOTS);
	read_regs(regs2, sizeof(regs2));
	CHECK(!memcmp(regs, regs2, sizeof(regs)));
	/* Neither do the ring and the addresses the job named. */
	for (size_t i = 0; i < st.job_words; i++)
		st.ring[st.jobs_at + i] = ~(uint64_t)0;
	read_regs(regs2, sizeof(regs2));
	CHECK(!memcmp(regs, regs2, sizeof(regs)));

	/* It finishes: a frame comes out with the job's identity and buffer. */
	complete_job(&st, 1, 1);
	CHECK(wait_frame(&st, &id) == 2);
	CHECK(id == 0xabcd0001);
	CHECK(!status_of(&st, &status) && status.state == LEMNOS_RING_STATE_OPEN &&
	      status.frames == 1 && status.jobs == 1);
	stream_close(&st);
}

/* What the checks above must not refuse: compression that says so, a horizontal flip. */
static void test_accepted_variants(void)
{
	struct pisp_be_tiles_config c;
	uint64_t addr[LEMNOS_PISP_BE_ADDRS];
	struct stream st;
	struct lemnos_pisp_be_job job = { .identity = 9, .buffer = 0, .config = 0 };
	int got;

	CHECK(!stream_open(&st));
	cfg_bayer_full(&c, addr, &st);
	c.config.global.bayer_enables |= PISP_BE_BAYER_ENABLE_DECOMPRESS |
					 PISP_BE_BAYER_ENABLE_TDN_COMPRESS;
	c.config.input_format.format = PISP_IMAGE_FORMAT_BPS_8 | PISP_IMAGE_FORMAT_COMPRESSION_MODE_1;
	c.config.tdn_output_format.format = PISP_IMAGE_FORMAT_BPS_8 |
					    PISP_IMAGE_FORMAT_COMPRESSION_MODE_1;
	c.config.output_format[0].transform = PISP_BE_TRANSFORM_HFLIP;
	put_config(&st, 0, &c);
	memcpy(job.addr, addr, sizeof(addr));
	post(&st, &job);
	got = kick(&st);
	if (got) {
		printf("FAIL compressed, mirrored job refused: %s\n", strerror(got));
		failures++;
	} else {
		printf("ok   compressed, mirrored job accepted\n");
	}
	complete_job(&st, 1, 1);
	CHECK(got || wait_frame(&st, NULL) == 0);
	stream_close(&st);
}

/* Two jobs: the second queued while the first runs, done and started moving separately. */
static void test_counters_two_jobs(void)
{
	struct pisp_be_tiles_config c;
	uint64_t addr[LEMNOS_PISP_BE_ADDRS];
	struct stream st;
	struct lemnos_pisp_be_job job = { .identity = 7, .buffer = 0, .config = 0 };
	uint64_t id;

	CHECK(!stream_open(&st));
	cfg_rgb(&c, addr, &st);
	put_config(&st, 0, &c);
	memcpy(job.addr, addr, sizeof(addr));
	post(&st, &job);
	CHECK(!kick(&st));
	/* First started, second job goes on the block while it runs. */
	complete_job(&st, 1, 0);
	addr[LEMNOS_PISP_BE_ADDR_OUTPUT(0, 0)] = st.pool_addr + BUFFER_BYTES;
	memcpy(job.addr, addr, sizeof(addr));
	job.buffer = 1;
	job.identity = 8;
	put_config(&st, 0, &c);
	post(&st, &job);
	CHECK(!kick(&st));
	/* The first finishes, the second starts and finishes together: done +2, started +1. */
	complete_job(&st, 1, 2);
	CHECK(wait_frame(&st, &id) == 0 && id == 7);
	CHECK(wait_frame(&st, &id) == 1 && id == 8);
	stream_close(&st);
}

/* A job the hardware never interrupts for, but reports idle and done: the watchdog finishes it. */
static void test_lost_interrupt(void)
{
	struct pisp_be_tiles_config c;
	uint64_t addr[LEMNOS_PISP_BE_ADDRS];
	struct stream st;
	struct lemnos_pisp_be_job job = { .identity = 9, .buffer = 0, .config = 0 };
	struct lemnos_stream_status status;

	CHECK(!stream_open(&st));
	cfg_rgb(&c, addr, &st);
	put_config(&st, 0, &c);
	memcpy(job.addr, addr, sizeof(addr));
	post(&st, &job);
	CHECK(!kick(&st));
	st.hw_started++;
	st.hw_done++;
	poke(BE_BATCH, (uint32_t)st.hw_done | ((uint32_t)st.hw_started << 8));
	/* No interrupt. The watchdog (1 s) sees the block idle and finishes it. */
	CHECK(wait_frame(&st, NULL) == 0);
	CHECK(!status_of(&st, &status) && status.state == LEMNOS_RING_STATE_OPEN);
	stream_close(&st);
}

/* A status that says a job is still queued keeps the next one off the block. */
static void test_queued_bit_blocks_writing(void)
{
	static char regs[8192];
	struct pisp_be_tiles_config c;
	uint64_t addr[LEMNOS_PISP_BE_ADDRS];
	struct stream st;
	struct lemnos_pisp_be_job job = { .identity = 3, .buffer = 0, .config = 0 };

	CHECK(!stream_open(&st));
	cfg_rgb(&c, addr, &st);
	put_config(&st, 0, &c);
	memcpy(job.addr, addr, sizeof(addr));
	poke(BE_STATUS, 1);	/* the block says something is queued */
	poke(BE_CONTROL, 0);
	post(&st, &job);
	CHECK(!kick(&st));
	read_regs(regs, sizeof(regs));
	CHECK(reg32(regs, BE_CONTROL) == 0);	/* not written */
	poke(BE_STATUS, 0);
	/* Once the block is idle (watchdog in 1 s) the job goes on it. */
	usleep(1300 * 1000);
	read_regs(regs, sizeof(regs));
	CHECK(reg32(regs, BE_CONTROL) & 1);
	complete_job(&st, 1, 1);
	CHECK(wait_frame(&st, NULL) == 0);
	stream_close(&st);
}

/* A consumer racing the config while it is being read: never an unchecked value on the block. */
static atomic_int race_stop;
static uint8_t *race_stride;

static void *race_thread(void *arg)
{
	(void)arg;
	while (!atomic_load(&race_stop)) {
		/* One byte, so a copy can't tear it into a third value: 0xc1 is misaligned. */
		__atomic_store_n(race_stride, 0xc1, __ATOMIC_RELAXED);
		__atomic_store_n(race_stride, 0xc0, __ATOMIC_RELAXED);
	}
	return NULL;
}

static void test_config_race(void)
{
	static char regs[8192];
	struct pisp_be_tiles_config c;
	uint64_t addr[LEMNOS_PISP_BE_ADDRS];
	struct stream st;
	pthread_t thread;
	int ran = 0, refused = 0;

	CHECK(!stream_open(&st));
	cfg_rgb(&c, addr, &st);
	put_config(&st, 0, &c);
	race_stride = (uint8_t *)&((struct pisp_be_tiles_config *)st.configs)->config.input_format.stride;
	atomic_store(&race_stop, 0);
	CHECK(!pthread_create(&thread, NULL, race_thread, NULL));

	for (int i = 0; i < 200 && failures < 5; i++) {
		struct lemnos_pisp_be_job job = { .identity = (uint64_t)i, .buffer = 0, .config = 0 };

		memcpy(job.addr, addr, sizeof(addr));
		post(&st, &job);
		int got = kick(&st);

		if (got) {
			if (got == EINVAL) {
				refused++;
				/* A refused job ends the stream; start again, with the writer stopped. */
				atomic_store(&race_stop, 1);
				pthread_join(thread, NULL);
				stream_close(&st);
				CHECK(!stream_open(&st));
				cfg_rgb(&c, addr, &st);
				put_config(&st, 0, &c);
				race_stride = (uint8_t *)&((struct pisp_be_tiles_config *)st.configs)->config.input_format.stride;
				atomic_store(&race_stop, 0);
				CHECK(!pthread_create(&thread, NULL, race_thread, NULL));
				continue;
			}
			printf("FAIL race: errno %d\n", got);
			failures++;
			break;
		}
		read_regs(regs, sizeof(regs));
		int stride = (int)reg32(regs, BE_CONFIG + offsetof(struct pisp_be_config, input_format) + 8);

		if (stride != 192) {
			printf("FAIL race: the block was given stride %d\n", stride);
			failures++;
		}
		ran++;
		complete_job(&st, 1, 1);
		CHECK(wait_frame(&st, NULL) == 0);
	}
	atomic_store(&race_stop, 1);
	pthread_join(thread, NULL);
	printf("ok   config race: %d ran with a checked value, %d refused\n", ran, refused);
	CHECK(ran > 0);
	stream_close(&st);
}

/* A real tile layout built by the config library is accepted, and runs. */
static void test_fixture_config(void)
{
	static struct pisp_be_tiles_config c;
	uint64_t addr[LEMNOS_PISP_BE_ADDRS];
	struct stream st;
	struct lemnos_pisp_be_job job = { .identity = 5, .buffer = 0, .config = 0 };
	uint64_t pool, region;

	CHECK(!stream_open(&st));
	pool = st.pool_addr;
	region = st.region_addr;
	fixture_fill(&c, addr, pool, region);
	put_config(&st, 0, &c);
	memcpy(job.addr, addr, sizeof(addr));
	post(&st, &job);
	int got = kick(&st);

	if (got) {
		printf("FAIL fixture config refused: %s\n", strerror(got));
		failures++;
	} else {
		printf("ok   the library's config (%u tiles) accepted\n", c.num_tiles);
	}
	complete_job(&st, 1, 1);
	CHECK(got || wait_frame(&st, NULL) == 0);
	stream_close(&st);
}

static void refuse_all(void)
{
	refuse_addresses();
	refuse_tiles();
	refuse_formats();
	refuse_enables();
	refuse_ring();
	test_runs_and_registers();
	test_accepted_variants();
	test_counters_two_jobs();
	test_config_race();
	test_fixture_config();
}

/* --- teardown --- */

/* A thread, not a child: a child would hold copies of the files whose closing is being tested. */
struct later_completion {
	pthread_t thread;
	uint8_t started, done;
};

static void *later_body(void *arg)
{
	struct later_completion *l = arg;

	usleep(300 * 1000);
	poke(BE_BATCH, (uint32_t)l->done | ((uint32_t)l->started << 8));
	poke(BE_INT_STATUS, 1);
	raise_irq();
	return NULL;
}

static void complete_later(struct later_completion *l, const struct stream *st)
{
	l->started = st->hw_started + 1;
	l->done = st->hw_done + 1;
	if (pthread_create(&l->thread, NULL, later_body, l))
		printf("FAIL pthread_create\n"), failures++;
}

/* Registered memory can't be released under a job. */
static void test_unregister_busy(void)
{
	struct pisp_be_tiles_config c;
	uint64_t addr[LEMNOS_PISP_BE_ADDRS];
	struct stream st;
	struct lemnos_pisp_be_job job = { .identity = 3, .buffer = 0, .config = 0 };
	struct lemnos_stream_unregister u = { 0 };

	CHECK(!stream_open(&st));
	cfg_rgb(&c, addr, &st);
	put_config(&st, 0, &c);
	memcpy(job.addr, addr, sizeof(addr));
	post(&st, &job);
	CHECK(!kick(&st));
	u.region = st.region;
	CHECK(ioctl(st.fd, LEMNOS_STREAM_IOC_UNREGISTER, &u) == -1 && errno == EBUSY);
	complete_job(&st, 1, 1);
	CHECK(wait_frame(&st, NULL) == 0);
	CHECK(!ioctl(st.fd, LEMNOS_STREAM_IOC_UNREGISTER, &u));
	{
		/* What the wedged case must not do is what the healthy case does. */
		uint64_t tiles = last_tiles();

		CHECK(page_refs(tiles) > 0);
		stream_close(&st);
		CHECK(page_refs(tiles) == 0);
	}
}

/* Closing waits for what's on the block and then lets go of everything. */
static void test_close_waits(void)
{
	struct pisp_be_tiles_config c;
	uint64_t addr[LEMNOS_PISP_BE_ADDRS];
	struct stream st;
	struct lemnos_pisp_be_job job = { .identity = 3, .buffer = 0, .config = 0 };
	int pool, region;
	struct later_completion later;
	struct timespec t0, t1;

	CHECK(!stream_open(&st));
	cfg_rgb(&c, addr, &st);
	put_config(&st, 0, &c);
	memcpy(job.addr, addr, sizeof(addr));
	post(&st, &job);
	CHECK(!kick(&st));
	pool = dup(st.pool_fd);
	region = dup(st.region_fd);
	complete_later(&later, &st);
	clock_gettime(CLOCK_MONOTONIC, &t0);
	munmap(st.ring, st.ring_bytes);
	munmap(st.configs, st.config_bytes);
	close(st.fd);
	clock_gettime(CLOCK_MONOTONIC, &t1);
	pthread_join(later.thread, NULL);
	double ms = (double)(t1.tv_sec - t0.tv_sec) * 1000 + (double)(t1.tv_nsec - t0.tv_nsec) / 1e6;

	printf("ok   close took %.0f ms with a job on the block\n", ms);
	CHECK(ms >= 250);	/* it waited for the job */
	CHECK(ms < 1500);	/* and not for the timeout */

	/* Nothing held any more: those dups are the last references. */
	close(st.region_fd);
	close(st.pool_fd);
	memset(&st, 0, sizeof(st));
	CHECK(dmabuf_count(pool) == 1);
	CHECK(dmabuf_count(region) == 1);
	close(pool);
	close(region);
}

/* An idle stream closes at once and releases everything. */
/* The stream can't get its fd, with the producer already attached: priv stays the caller's. */
static void test_create_without_fd(void)
{
	struct lemnos_pisp_be_create c;
	struct rlimit old, low;
	struct stream st;
	int pool = heap_alloc(heap_fd, (uint64_t)BUFFERS * BUFFER_BYTES);
	int next = dup(pool);

	close(next);
	memset(&c, 0, sizeof(c));
	c.stream.block = block_id;
	c.stream.buffers = BUFFERS;
	c.stream.buffer_bytes = BUFFER_BYTES;
	c.stream.frame_slots = FRAME_SLOTS;
	c.stream.job_slots = JOB_SLOTS;
	c.stream.job_bytes = JOB_BYTES;
	c.stream.pool_fd = pool;
	c.stream.doorbell_fd = -1;
	getrlimit(RLIMIT_NOFILE, &old);
	low = old;
	low.rlim_cur = next;
	setrlimit(RLIMIT_NOFILE, &low);
	errno = 0;
	check(ioctl(ctl_fd, LEMNOS_IOC_PISP_BE_CREATE, &c) && errno == EMFILE,
	      "no fd for the stream: refused");
	setrlimit(RLIMIT_NOFILE, &old);
	close(pool);
	check(!stream_open(&st), "the block is free again after a stream that never existed");
	stream_close(&st);
}

static void test_close_idle(void)
{
	struct stream st;
	int pool, region;

	CHECK(!stream_open(&st));
	pool = dup(st.pool_fd);
	region = dup(st.region_fd);
	stream_close(&st);
	CHECK(dmabuf_count(pool) == 1);
	CHECK(dmabuf_count(region) == 1);
	close(pool);
	close(region);
}

/* Unbinding with a job on the block waits for it and ends the stream. */
static void test_unbind_waits(const char *bind, const char *unbind, const char *device)
{
	struct pisp_be_tiles_config c;
	uint64_t addr[LEMNOS_PISP_BE_ADDRS];
	struct stream st;
	struct lemnos_pisp_be_job job = { .identity = 3, .buffer = 0, .config = 0 };
	struct lemnos_stream_status status;
	struct later_completion later;
	int fd;

	CHECK(!stream_open(&st));
	cfg_rgb(&c, addr, &st);
	put_config(&st, 0, &c);
	memcpy(job.addr, addr, sizeof(addr));
	post(&st, &job);
	CHECK(!kick(&st));
	complete_later(&later, &st);
	fd = open(unbind, O_WRONLY | O_CLOEXEC);
	CHECK(fd >= 0 && write(fd, device, strlen(device)) == (ssize_t)strlen(device));
	close(fd);
	pthread_join(later.thread, NULL);
	/* The stream's over: no use-after-free in the module's status path. */
	CHECK(!status_of(&st, &status) && status.state == LEMNOS_RING_STATE_CLOSED);
	CHECK(kick(&st) == ENODEV);
	struct lemnos_pisp_be_status pb;

	CHECK(ioctl(st.fd, LEMNOS_PISP_BE_IOC_STATUS, &pb) == -1 && errno == ENODEV);
	stream_close(&st);
	/* Re-bind. */
	fd = open(bind, O_WRONLY | O_CLOEXEC);
	CHECK(fd >= 0 && write(fd, device, strlen(device)) == (ssize_t)strlen(device));
	close(fd);
}

/* --- wedging --- */

static void close_and_expect_pinned(struct stream *st, const char *what)
{
	int pool = dup(st->pool_fd), region = dup(st->region_fd);

	munmap(st->ring, st->ring_bytes);
	munmap(st->configs, st->config_bytes);
	close(st->fd);
	close(st->region_fd);
	close(st->pool_fd);
	memset(st, 0, sizeof(*st));
	/*
	 * Our dups plus the kernel's references: the stream is gone but what it
	 * held is not. The pool's reference stays too: the job might be writing it.
	 */
	if (dmabuf_count(pool) < 2 || dmabuf_count(region) < 2) {
		printf("FAIL %s: buffers released (count %ld, %ld)\n", what, dmabuf_count(pool),
		       dmabuf_count(region));
		failures++;
	}
	close(pool);
	close(region);
}

static void test_wedge_timeout(void)
{
	struct pisp_be_tiles_config c;
	uint64_t addr[LEMNOS_PISP_BE_ADDRS];
	struct stream st;
	struct lemnos_pisp_be_job job = { .identity = 3, .buffer = 0, .config = 0 };
	struct lemnos_stream_status status;
	struct lemnos_pisp_be_status pb;

	CHECK(!stream_open(&st));
	cfg_rgb(&c, addr, &st);
	put_config(&st, 0, &c);
	memcpy(job.addr, addr, sizeof(addr));
	post(&st, &job);
	CHECK(!kick(&st));
	poke(BE_STATUS, 2);	/* busy, never finishing */
	usleep(1400 * 1000);
	CHECK(!status_of(&st, &status) && status.state == LEMNOS_RING_STATE_FAULT &&
	      status.fault == ETIMEDOUT);
	CHECK(!ioctl(st.fd, LEMNOS_PISP_BE_IOC_STATUS, &pb) && pb.wedged == 1);
	{
		/* The block says it finished, too late to be believed: nothing is let go. */
		struct lemnos_stream_unregister u = { .region = st.region };
		uint64_t tiles = last_tiles();

		complete_job(&st, 1, 1);
		usleep(50 * 1000);
		CHECK(ioctl(st.fd, LEMNOS_STREAM_IOC_UNREGISTER, &u) == -1 && errno == EBUSY);
		close_and_expect_pinned(&st, "wedged by timeout");
		/* The block may still read its tiles. */
		CHECK(page_refs(tiles) > 0);
	}

	/* Nothing new on a wedged block. */
	CHECK(stream_open(&st) == -1 && errno == EIO);
	stream_close(&st);
}

/* Hardware counters that say more happened than we gave it. */
static void test_wedge_counters(void)
{
	struct pisp_be_tiles_config c;
	uint64_t addr[LEMNOS_PISP_BE_ADDRS];
	struct stream st;
	struct lemnos_pisp_be_job job = { .identity = 3, .buffer = 0, .config = 0 };
	struct lemnos_stream_status status;
	struct lemnos_pisp_be_status pb;

	CHECK(!stream_open(&st));
	cfg_rgb(&c, addr, &st);
	put_config(&st, 0, &c);
	memcpy(job.addr, addr, sizeof(addr));
	post(&st, &job);
	CHECK(!kick(&st));
	/* Claims two done with one job, and three started. Nothing is finished. */
	poke(BE_BATCH, 2u | (3u << 8));
	poke(BE_INT_STATUS, 1);
	raise_irq();
	usleep(50 * 1000);
	CHECK(!status_of(&st, &status) && status.state == LEMNOS_RING_STATE_FAULT &&
	      status.frames == 0);
	CHECK(!ioctl(st.fd, LEMNOS_PISP_BE_IOC_STATUS, &pb) && pb.wedged == 1);
	CHECK(st.ring[LEMNOS_RING_FRAMES_HEAD] == 0);
	close_and_expect_pinned(&st, "wedged by counters");
}

static void rebind(const char *bind, const char *unbind, const char *device)
{
	int fd = open(unbind, O_WRONLY | O_CLOEXEC);

	if (fd >= 0) {
		CHECK(write(fd, device, strlen(device)) == (ssize_t)strlen(device));
		close(fd);
	}
	poke(BE_BATCH, 0);
	poke(BE_STATUS, 0);
	poke(BE_INT_STATUS, 0);
	fd = open(bind, O_WRONLY | O_CLOEXEC);
	CHECK(fd >= 0 && write(fd, device, strlen(device)) == (ssize_t)strlen(device));
	close(fd);
}

int main(int argc, char **argv)
{
	if (argc != 7) {
		printf("usage\n");
		return 2;
	}
	const char *node = argv[1], *device = argv[2], *bind = argv[3], *unbind = argv[4];
	const char *heap_path = argv[5], *mode = argv[6];

	snprintf(sysdir, sizeof(sysdir), SYS);
	setvbuf(stdout, NULL, _IONBF, 0);
	ctl_fd = open(node, O_RDWR | O_CLOEXEC);
	heap_fd = open(heap_path, O_RDONLY | O_CLOEXEC);
	if (ctl_fd < 0 || heap_fd < 0) {
		printf("FAIL open %s/%s: %s\n", node, heap_path, strerror(errno));
		return 1;
	}
	block_id = find_block(ctl_fd, device);
	if (!block_id) {
		printf("FAIL no block %s\n", device);
		return 1;
	}

	if (!strcmp(mode, "refuse")) {
		refuse_all();
	} else if (!strcmp(mode, "lifetime")) {
		test_lost_interrupt();
		test_queued_bit_blocks_writing();
		test_unregister_busy();
		test_create_without_fd();
		test_close_idle();
		test_close_waits();
		test_unbind_waits(bind, unbind, device);
		block_id = find_block(ctl_fd, device);
	} else if (!strcmp(mode, "wedge")) {
		test_wedge_counters();
		close(ctl_fd);
		rebind(bind, unbind, device);
		ctl_fd = open(node, O_RDWR | O_CLOEXEC);
		block_id = find_block(ctl_fd, device);
		test_wedge_timeout();
	}
	close(heap_fd);
	close(ctl_fd);
	if (failures)
		printf("FAILED: %d checks\n", failures);
	else
		printf("ok   %s: all checks passed\n", mode);
	return failures ? 1 : 0;
}
