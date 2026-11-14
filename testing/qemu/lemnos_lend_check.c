// SPDX-License-Identifier: GPL-2.0
/*
 * The dma-buf lend checks, against the fake PiSP back end block (test block "lemnos-test-pisp",
 * module parameter test_pb_fake_regs).
 *
 * A real dma-buf from the system heap is lent to a back end stream with LEMNOS_STREAM_IOC_REGISTER_LENT
 * (the module maps it DMA_TO_DEVICE). Checked:
 *   - a memfd, which is not a dma-buf, is refused with EINVAL and nothing is registered;
 *   - a lent dma-buf is pinned while registered, a job reads it, and unregistering it after the job
 *     ends is accepted (the release on finish); a second unregister is ENOENT;
 *   - a registration again with the same generation is a pin-cache hit: the same device address;
 *   - a normal stop (an idle, unwedged stream closed) releases the pinned dma-buf;
 *   - a stop with a lent dma-buf still registered, on a wedged block, reports the wedge and keeps the
 *     dma-buf pinned (the block may still read it).
 *
 * What is not checked here: the DMA direction. The module maps the lent region DMA_TO_DEVICE in
 * lem_stream_register_lent(); nothing a process can read shows the direction, so it is a code
 * property, not a run-time one.
 *
 * Usage: lemnos_lend_check /dev/lemnos <device> <bind> <unbind> <heap>
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
#include <sys/syscall.h>
#include <unistd.h>

#include "pisp_be_config.h"
#include "lemnos.h"
#include "lemnos_pisp_be.h"
#include "find_block.h"

#define BUFFERS 4u
#define BUFFER_BYTES (256u * 1024)
#define FRAME_SLOTS 8u
#define JOB_SLOTS 4u
#define JOB_BYTES ((uint32_t)sizeof(struct lemnos_pisp_be_job))
#define LENT_BYTES (256u * 1024)
#define SYS "/sys/bus/platform/devices/lemnos-test-pisp"
#define W 64
#define H 32
/* Longer than the module's job timeout. */
#define WEDGE_WAIT_US (1400 * 1000)

/* Registers the fake block exposes. */
#define BE_STATUS 0x10
#define BE_BATCH 0x14
#define BE_INT_STATUS 0x1c


static int failures, ctl_fd, heap_fd;
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
	int fd, n = snprintf(buf, sizeof(buf), "%x %x", off, value);

	snprintf(path, sizeof(path), "%s/poke", sysdir);
	fd = open(path, O_WRONLY | O_CLOEXEC);
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

/* --- a stream --- */

struct stream {
	int fd, pool_fd;
	uint64_t *ring;
	size_t ring_bytes;
	void *configs;
	size_t config_bytes;
	uint64_t pool_addr;
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

/* The fdinfo line that names the exporter: "exp_name:\tsystem" for a system heap buffer. */
static int exported_by(int fd, const char *name)
{
	char path[64], text[512];
	int f, n;

	snprintf(path, sizeof(path), "/proc/self/fdinfo/%d", fd);
	f = open(path, O_RDONLY | O_CLOEXEC);
	if (f < 0)
		return 0;
	n = (int)read(f, text, sizeof(text) - 1);
	close(f);
	if (n <= 0)
		return 0;
	text[n] = 0;
	char want[80];

	snprintf(want, sizeof(want), "exp_name:\t%s\n", name);
	return strstr(text, want) != NULL;
}

/* The dma-buf's reference count, from its fdinfo: above 1 means the kernel still holds it. */
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

static int stream_open(struct stream *st)
{
	struct lemnos_pisp_be_create c;

	memset(st, 0, sizeof(*st));
	st->pool_fd = heap_alloc(heap_fd, (uint64_t)BUFFERS * BUFFER_BYTES);
	if (st->pool_fd < 0)
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

/* The hardware says it started and finished this job. */
static void complete_job(struct stream *st, int started, int done)
{
	st->hw_started += started;
	st->hw_done += done;
	poke(BE_BATCH, (uint32_t)st->hw_done | ((uint32_t)st->hw_started << 8));
	poke(BE_INT_STATUS, 1);
	raise_irq();
}

/* The next frame's buffer, or -1 if none arrives in time; gives the buffer back. */
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

/* An RGB888 image in and out, one tile. The input is at `input`, the output in the pool. */
static void cfg_lent(struct pisp_be_tiles_config *c, uint64_t *addr, uint64_t input, uint64_t output)
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
	addr[0] = input;
	addr[LEMNOS_PISP_BE_ADDR_OUTPUT(0, 0)] = output;
}

/* Posts a job of `identity` whose input is `input` (a lent region's address), config slot 0. */
static void post_job(struct stream *st, uint64_t identity, uint64_t input)
{
	struct pisp_be_tiles_config c;
	uint64_t addr[LEMNOS_PISP_BE_ADDRS];
	struct lemnos_pisp_be_job job = { .identity = identity, .buffer = 0, .config = 0 };

	cfg_lent(&c, addr, input, st->pool_addr);
	put_config(st, 0, &c);
	memcpy(job.addr, addr, sizeof(addr));
	post(st, &job);
}

static int lend(struct stream *st, int fd, uint32_t generation, struct lemnos_stream_register_lent *a)
{
	memset(a, 0, sizeof(*a));
	a->fd = fd;
	a->generation = generation;
	return ioctl(st->fd, LEMNOS_STREAM_IOC_REGISTER_LENT, a);
}

/* A lend that must succeed: says what it got when it doesn't. */
static int must_lend(struct stream *st, int fd, uint32_t generation, struct lemnos_stream_register_lent *a,
		     const char *what)
{
	int r = lend(st, fd, generation, a);

	if (r)
		printf("FAIL %s: REGISTER_LENT returned %d: %s\n", what, r, strerror(errno)), failures++;
	return r;
}

static int unregister(struct stream *st, uint32_t region)
{
	struct lemnos_stream_unregister u = { .region = region };

	return ioctl(st->fd, LEMNOS_STREAM_IOC_UNREGISTER, &u);
}

/* The module refuses a memfd, which is not a dma-buf, and registers nothing. */
static void refuse_memfd(void)
{
	struct stream st;
	struct lemnos_stream_register_lent a;
	struct lemnos_stream_status status;
	int m = (int)syscall(SYS_memfd_create, "lend-refused", 0);
	int r;

	CHECK(m >= 0);
	CHECK(!stream_open(&st));
	r = lend(&st, m, 1, &a);
	if (r == -1 && errno == EINVAL)
		printf("ok   memfd refused: %s (not a dma-buf), nothing registered\n", strerror(errno));
	else
		printf("FAIL memfd lend: returned %d errno %s (want -1 %s)\n", r,
		       strerror(errno), strerror(EINVAL)), failures++;
	CHECK(r == -1 && errno == EINVAL);
	CHECK(a.addr == 0 && a.size == 0);
	/* The stream still runs after a refusal. */
	CHECK(!ioctl(st.fd, LEMNOS_STREAM_IOC_STATUS, &status) && status.state == LEMNOS_RING_STATE_OPEN);
	close(m);
	stream_close(&st);
}

/* The system heap: the kernel lends a dma-buf only as one contiguous DMA range, and a system heap
 * buffer is not one in general. Whatever the kernel answers is checked: a registration that
 * succeeds is released at once, and one that is refused is refused with EINVAL and nothing is held. */
static void system_heap(const char *system)
{
	struct stream st;
	struct lemnos_stream_register_lent a;
	int sys_heap = open(system, O_RDONLY | O_CLOEXEC);
	int sys = sys_heap < 0 ? -1 : heap_alloc(sys_heap, LENT_BYTES);
	int r;

	CHECK(sys >= 0);
	CHECK(exported_by(sys, "system"));
	CHECK(!stream_open(&st));
	r = lend(&st, sys, 1, &a);
	if (r == 0) {
		printf("ok   system-heap dma-buf is one contiguous range here: addr 0x%llx size %llu\n",
		       (unsigned long long)a.addr, (unsigned long long)a.size);
		CHECK(!unregister(&st, a.region));
	} else {
		CHECK(r == -1 && errno == EINVAL);
		printf("ok   system-heap dma-buf refused: %s (not one contiguous DMA range), nothing registered\n",
		       strerror(errno));
	}
	stream_close(&st);
	close(sys);
	if (sys_heap >= 0)
		close(sys_heap);
}

/* A CMA dma-buf, one contiguous range: lent, read by a job, released on finish; the same generation
 * hits the pin cache; a normal stop releases it. */
static void lend_release(const char *cma)
{
	struct stream st;
	struct lemnos_stream_register_lent a, b;
	struct lemnos_pisp_be_status pb;
	uint64_t identity = 0;
	int lent = heap_alloc(heap_fd, LENT_BYTES);
	long before, held, after;
	int buffer;

	CHECK(lent >= 0);
	before = dmabuf_count(lent);
	CHECK(!stream_open(&st));
	if (must_lend(&st, lent, 1, &a, "lend cma")) {
		stream_close(&st);
		close(lent);
		return;
	}
	CHECK(a.addr != 0 && a.size >= LENT_BYTES);
	held = dmabuf_count(lent);
	CHECK(held > before);
	printf("ok   lent a CMA dma-buf: addr 0x%llx size %llu, count %ld -> %ld\n",
	       (unsigned long long)a.addr, (unsigned long long)a.size, before, held);

	post_job(&st, 5, a.addr);
	CHECK(!kick(&st));
	complete_job(&st, 1, 1);
	buffer = wait_frame(&st, &identity);
	CHECK(buffer == 0 && identity == 5);

	/* The job ended: the lent region may be let go. */
	CHECK(!unregister(&st, a.region));
	CHECK(unregister(&st, a.region) == -1 && errno == ENOENT);
	/* Unregistered, the module keeps the pin for its generation. */
	CHECK(dmabuf_count(lent) > before);

	/* The same generation again: a pin-cache hit, the same device address. */
	if (!must_lend(&st, lent, 1, &b, "lend cma again")) {
		CHECK(b.addr == a.addr);
		CHECK(!unregister(&st, b.region));
	}

	/* A normal stop: idle and unwedged, so the stream closes and the pin is released. */
	CHECK(!ioctl(st.fd, LEMNOS_PISP_BE_IOC_STATUS, &pb) && pb.wedged == 0);
	stream_close(&st);
	after = dmabuf_count(lent);
	CHECK(after == before);
	printf("ok   a job finished, the lend was released, a normal stop released the pin: count %ld\n",
	       after);
	close(lent);
}

/* A stop with a lent dma-buf still registered on a wedged block: the wedge is reported, and the
 * dma-buf stays pinned. */
static void lend_wedge(void)
{
	struct stream st;
	struct lemnos_stream_register_lent a;
	struct lemnos_stream_status status;
	struct lemnos_pisp_be_status pb;
	int lent = heap_alloc(heap_fd, LENT_BYTES);
	long before, pinned;

	CHECK(lent >= 0);
	before = dmabuf_count(lent);
	CHECK(!stream_open(&st));
	if (must_lend(&st, lent, 1, &a, "lend cma for the wedge")) {
		stream_close(&st);
		close(lent);
		return;
	}
	post_job(&st, 7, a.addr);
	CHECK(!kick(&st));
	/* Busy, never finishing: the module times the job out and wedges the block. */
	poke(BE_STATUS, 2);
	usleep(WEDGE_WAIT_US);
	CHECK(!ioctl(st.fd, LEMNOS_STREAM_IOC_STATUS, &status) && status.state == LEMNOS_RING_STATE_FAULT &&
	      status.fault == ETIMEDOUT);
	/* The wedge flag, read before the stream stops. */
	CHECK(!ioctl(st.fd, LEMNOS_PISP_BE_IOC_STATUS, &pb) && pb.wedged == 1);
	stream_close(&st);
	pinned = dmabuf_count(lent);
	CHECK(pinned > before);
	printf("ok   wedged stop: wedge flag 1, lent dma-buf kept pinned, count %ld -> %ld\n", before,
	       pinned);
	close(lent);
}

/* Unbinds and binds the block: the fake block's registers and wedge go back to a fresh block. */
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
	if (fd >= 0)
		close(fd);
}

int main(int argc, char **argv)
{
	if (argc != 7) {
		printf("usage: lemnos_lend_check /dev/lemnos <device> <bind> <unbind> <cma heap> <system heap>\n");
		return 2;
	}
	const char *node = argv[1], *device = argv[2], *bind = argv[3], *unbind = argv[4];
	const char *heap_path = argv[5], *system = argv[6];

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
	refuse_memfd();
	system_heap(system);
	lend_release(heap_path);
	lend_wedge();
	/* The wedge is the block's: a fresh block is what the next check gets. */
	close(ctl_fd);
	rebind(bind, unbind, device);
	close(heap_fd);
	if (failures)
		printf("FAILED: %d checks\n", failures);
	else
		printf("ok   lemnos_lend_check: all checks passed\n");
	return failures ? 1 : 0;
}
