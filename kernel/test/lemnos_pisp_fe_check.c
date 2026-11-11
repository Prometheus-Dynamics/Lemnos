// SPDX-License-Identifier: GPL-2.0
/*
 * The PiSP front end on the fake block in the test module: every configuration
 * field the hardware could act on is refused when it's wrong, what reaches the
 * registers is only the module's checked copy, the register sequence is the
 * one ported from the stock driver, and stopping (STOP, closing either file,
 * unbinding) waits for the block before anything is released.
 *
 * Usage: lemnos_pisp_fe_check /dev/lemnos <csi2 device> <unbind> <heap>
 *    or: lemnos_pisp_fe_check wedge <csi2 device> <unbind> <heap>
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
#include "../lemnos_pisp_fe.h"
#include "../lemnos_rp1_csi2.h"
#include "lemnos_test_csi2.h"
#include "lemnos_pisp_fe_fixture.h"
#include <pisp_fe_config.h>

#define BUFFERS 4u
#define BUFFER_BYTES 32768u
#define FRAME_SLOTS 8u
#define JOB_SLOTS 4u
#define JOB_BYTES ((uint32_t)sizeof(struct lemnos_stream_job))

#define W 64u
#define H 4u
#define STRIDE 128u
#define OUT0_BYTES (STRIDE * H)
#define CFG_SIZE ((uint32_t)sizeof(struct pisp_fe_config))
#define STATS_SIZE ((uint32_t)sizeof(struct pisp_statistics))
#define CFG_SLOTS 8u

/* Registers of the fake front end, as lemnos_pisp_fe.c writes them. */
#define FE_CONTROL 0x004
#define FE_INT_EN 0x018
#define FE_INT_STATUS 0x01c
#define FE_CONFIG 0x040
#define C_QUEUE 1u
#define C_ABORT 2u
#define C_RESET 4u
#define C_LATCH 8u
#define INT_EOF 1u
#define INT_SOF 2u
#define INT_LINES 0x300u
#define CH_CTRL_FORCE (1u << 3)
#define CH_CTRL_AUTO_ARM (1u << 4)
#define CH_CTRL_MODE_FE (3u << 1)
#define OFF(field) (FE_CONFIG + (unsigned int)offsetof(struct pisp_fe_config, field))

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

struct region {
	int fd, handle;
	uint64_t addr;
	void *map;
	uint64_t size;
};

static int ctl, hw;
static int pool_fd;
static struct region cfg_r, stats_r, out_r;
static uint8_t *pool_cpu;
static size_t stats_map_bytes, out_map_bytes = 8192;
/* A wedged run earlier in the same boot leaves its pins on purpose. */
static int attached_before;
static uint32_t pool_stride = BUFFER_BYTES;

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

static int make_buf(const char *heap, uint64_t size)
{
	int h = open(heap, O_RDONLY | O_CLOEXEC);
	struct dma_heap_allocation_data alloc = {
		.len = size,
		.fd_flags = O_RDWR | O_CLOEXEC,
	};

	if (h < 0 || ioctl(h, DMA_HEAP_IOCTL_ALLOC, &alloc))
		return -1;
	close(h);
	return (int)alloc.fd;
}

static struct lemnos_pisp_fe_info fe_info(void)
{
	struct lemnos_pisp_fe_info i;

	memset(&i, 0xff, sizeof(i));
	CHECK(!ioctl(ctl, LEMNOS_PISP_FE_IOC_INFO, &i), "INFO");
	return i;
}

static uint32_t peek(uint32_t off)
{
	struct tcsi_fe_peek p = { .offset = off };

	ioctl(hw, TCSI_IOC_FE_PEEK, &p);
	return p.value;
}

static void fe_fire(uint32_t status)
{
	CHECK(!ioctl(hw, TCSI_IOC_FE_FIRE, &status), "fire %x", status);
	sleep_ms(4);
}

static void fe_busy(uint32_t ms)
{
	ioctl(hw, TCSI_IOC_FE_BUSY, &ms);
}

static void fe_set_status(uint32_t v)
{
	ioctl(hw, TCSI_IOC_FE_STATUS, &v);
}

/* Set for a stream with no pool: each buffer a dma-buf of its own. */
static int own_buffers;

static int stream_open(struct stream *s)
{
	struct lemnos_stream_create c = {
		.flags = own_buffers ? LEMNOS_STREAM_CREATE_BUFFERS : 0,
		.buffers = BUFFERS,
		.buffer_bytes = BUFFER_BYTES,
		.frame_slots = FRAME_SLOTS,
		.job_slots = JOB_SLOTS,
		.job_bytes = JOB_BYTES,
		.pool_fd = own_buffers ? -1 : pool_fd,
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
	return s->r == MAP_FAILED ? -errno : 0;
}

static void stream_close(struct stream *s)
{
	munmap((void *)s->r, s->ring_bytes);
	close(s->fd);
	s->fd = -1;
}

static int reg(const struct stream *s, struct region *r)
{
	struct lemnos_stream_register a = { .fd = r->fd };

	if (ioctl(s->fd, LEMNOS_STREAM_IOC_REGISTER, &a))
		return -errno;
	r->handle = (int)a.region;
	r->addr = a.addr;
	r->size = a.size;
	return 0;
}

static int stream_setup(struct stream *s)
{
	int ret = stream_open(s);

	if (ret)
		return ret;
	if (reg(s, &cfg_r) || reg(s, &stats_r) || reg(s, &out_r)) {
		printf("register failed: %s\n", strerror(errno));
		return -1;
	}
	return 0;
}

static struct pisp_fe_config *cfg_slot(uint32_t i)
{
	return (struct pisp_fe_config *)((char *)cfg_r.map + i * CFG_SIZE);
}

static void valid(struct pisp_fe_config *c)
{
	memset(c, 0, sizeof(*c));
	c->global.enables = PISP_FE_ENABLE_INPUT | PISP_FE_ENABLE_OUTPUT0 | PISP_FE_ENABLE_AWB_STATS |
			    PISP_FE_ENABLE_OUTPUT1;
	c->global.bayer_order = 2;
	c->input.streaming = 1;
	c->input.format.width = W;
	c->input.format.height = H;
	c->input.format.format = PISP_IMAGE_FORMAT_BPS_16;
	c->output_axi.maxlen_flags = 0xaf;
	c->output_axi.cache_prot = 0x32;
	c->output_axi.qos = 0x8410;
	c->output_axi.thresh = 0x0140;
	c->output_axi.throttle = 0x4100;
	c->ch[0].output.format.width = W;
	c->ch[0].output.format.height = H;
	c->ch[0].output.format.stride = STRIDE;
	c->ch[0].output.format.format = PISP_IMAGE_FORMAT_BPS_16;
	c->ch[1].output.format = c->ch[0].output.format;
	c->awb_stats.size_x = 2;
	c->awb_stats.size_y = 2;
}

static struct lemnos_pisp_fe_start good_start(const struct stream *s)
{
	return (struct lemnos_pisp_fe_start){
		.stream_fd = s->fd,
		.lanes = 2,
		.mbps = 800,
		.dt = 0x2b,
		.width = W,
		.height = H,
		.output_width = W,
		.output_height = H,
		.output_stride = STRIDE,
		.output_size = OUT0_BYTES,
		.output_format = PISP_IMAGE_FORMAT_BPS_16,
	};
}

static int start(const struct lemnos_pisp_fe_start *a)
{
	return ioctl(ctl, LEMNOS_PISP_FE_IOC_START, a) ? -errno : 0;
}

static int stop(void)
{
	return ioctl(ctl, LEMNOS_PISP_FE_IOC_STOP) ? -errno : 0;
}

static int kick(const struct stream *s)
{
	return ioctl(s->fd, LEMNOS_STREAM_IOC_KICK) ? -errno : 0;
}

/* The ranges the job's configuration in `slot` calls for, built from its own enables. */
static struct lemnos_stream_job job_for(uint32_t slot, uint32_t buffer)
{
	const struct pisp_fe_config *c = cfg_slot(slot);
	uint32_t n = 0;
	struct lemnos_stream_job j;

	memset(&j, 0, sizeof(j));
	j.identity = 0x1000 + slot;
	j.buffer = buffer;
	j.range[n].addr = cfg_r.addr + slot * CFG_SIZE;
	j.range[n++].len = CFG_SIZE;
	if (c->global.enables & 0x1fe0) {
		j.range[n].addr = stats_r.addr;
		j.range[n++].len = STATS_SIZE;
	}
	if (c->global.enables & PISP_FE_ENABLE_OUTPUT1) {
		j.range[n].addr = out_r.addr;
		j.range[n++].len = OUT0_BYTES;
	}
	j.count = n;
	return j;
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

static int pop_frame(struct stream *s, uint32_t *buffer, uint32_t *bytes, uint64_t *identity)
{
	uint64_t head = load(s, LEMNOS_RING_FRAMES_HEAD);
	size_t at;
	uint64_t pair;

	if (head == s->frames_tail)
		return 0;
	at = LEMNOS_RING_FRAMES + (s->frames_tail & (FRAME_SLOTS - 1)) * LEMNOS_STREAM_FRAME_WORDS;
	pair = load(s, at);
	*buffer = (uint32_t)pair;
	*bytes = (uint32_t)(pair >> 32);
	*identity = load(s, at + 2);
	s->frames_tail++;
	store(s, LEMNOS_RING_FRAMES_TAIL, s->frames_tail);
	return 1;
}

static uint64_t status_fault(const struct stream *s)
{
	struct lemnos_stream_status st;

	memset(&st, 0, sizeof(st));
	ioctl(s->fd, LEMNOS_STREAM_IOC_STATUS, &st);
	return (uint64_t)(uint32_t)st.fault;
}

/* --- the validator, against crafted configurations --- */

typedef void (*mut_fn)(struct pisp_fe_config *c);

struct bad_case {
	const char *name;
	mut_fn fn;
	int err;		/* expected errno */
	int count_delta;	/* range count wrong by this much */
};

#define CASE(n, e, body)                                 \
	static void m_##n(struct pisp_fe_config *c)      \
	{                                                \
		(void)c;                                 \
		body;                                    \
	}
#define E_(n, e) { #n, m_##n, e, 0 }

CASE(addr_stats_lo, 0, c->stats_buffer.addr_lo = 0x1000)
CASE(addr_stats_hi, 0, c->stats_buffer.addr_hi = 1)
CASE(addr_out0_lo, 0, c->output_buffer[0].addr_lo = 0x1000)
CASE(addr_out0_hi, 0, c->output_buffer[0].addr_hi = 1)
CASE(addr_out1_lo, 0, c->output_buffer[1].addr_lo = 0x1000)
CASE(addr_out1_hi, 0, c->output_buffer[1].addr_hi = 1)
CASE(addr_input_lo, 0, c->input_buffer.addr_lo = 0x1000)
CASE(addr_input_hi, 0, c->input_buffer.addr_hi = 1)
CASE(input_frame_id, 0, c->input_buffer.frame_id = 7)
CASE(input_buf_pad, 0, c->input_buffer.pad = 1)
CASE(ilines0, 0, c->ch[0].output.ilines = 100)
CASE(ilines1, 0, c->ch[1].output.ilines = 100)
CASE(disabled_output1_addr, 0, {
	c->global.enables &= ~(uint32_t)PISP_FE_ENABLE_OUTPUT1;
	c->output_buffer[1].addr_lo = 0x4000;
})
CASE(disabled_stats_addr, 0, {
	c->global.enables &= ~(uint32_t)PISP_FE_ENABLE_AWB_STATS;
	c->stats_buffer.addr_lo = 0x4000;
})
CASE(disabled_block_left_on, 0, {
	c->global.enables &= ~(uint32_t)PISP_FE_ENABLE_OUTPUT1;
	c->global.enables |= PISP_FE_ENABLE_DOWNSCALE1;
})
CASE(unknown_enable_bit, 0, c->global.enables |= 0x2000u)
CASE(no_input, 0, c->global.enables &= ~(uint32_t)PISP_FE_ENABLE_INPUT)
CASE(no_output0, 0, c->global.enables &= ~(uint32_t)PISP_FE_ENABLE_OUTPUT0)
CASE(not_streaming, 0, c->input.streaming = 0)
CASE(input_format_not_16, 0, c->input.format.format = PISP_IMAGE_FORMAT_BPS_8)
CASE(input_width_differs, 0, c->input.format.width = W + 2)
CASE(input_height_differs, 0, c->input.format.height = H + 2)
CASE(input_wider_than_start_cropped_back, 0, {
	c->input.format.width = W + 16;
	c->global.enables &= ~(uint32_t)PISP_FE_ENABLE_OUTPUT1;
	c->global.enables |= PISP_FE_ENABLE_CROP0;
	c->ch[0].crop.width = W;
	c->ch[0].crop.height = H;
})
CASE(input_taller_than_start_cropped_back, 0, {
	c->input.format.height = H + 2;
	c->global.enables &= ~(uint32_t)PISP_FE_ENABLE_OUTPUT1;
	c->global.enables |= PISP_FE_ENABLE_CROP0;
	c->ch[0].crop.width = W;
	c->ch[0].crop.height = H;
})
CASE(input_stride, 0, c->input.format.stride = 16)
CASE(input_axi, 0, c->input.axi.qos = 1)
CASE(input_holdoff, 0, c->input.holdoff = 1)
CASE(input_pad, 0, c->input.pad2[1] = 1)
CASE(global_pad, 0, c->global.pad[2] = 1)
CASE(bad_bayer, 0, c->global.bayer_order = 7)
CASE(decompand_pad, 0, c->decompand.pad = 1)
CASE(spare1, 0, c->spare1 = 1)
CASE(bla_pad, 0, c->bla.pad[0] = 1)
CASE(ch_pad, 0, c->ch[0].pad = 1)
CASE(ch_downscale_pad, 0, c->ch[0].downscale.pad[1] = 1)
CASE(output_axi_qos, 0, c->output_axi.qos = 1)
CASE(output_axi_thresh, 0, c->output_axi.thresh = 1)
CASE(out0_width, 0, c->ch[0].output.format.width = W - 2)
CASE(out0_height, 0, c->ch[0].output.format.height = H + 2)
CASE(out0_stride_small, 0, c->ch[0].output.format.stride = STRIDE - 16)
CASE(out0_stride_unaligned, 0, c->ch[0].output.format.stride = STRIDE + 4)
CASE(out0_stride_negative, 0, c->ch[0].output.format.stride = -(int)STRIDE)
CASE(out0_stride2, 0, c->ch[0].output.format.stride2 = 4)
CASE(out0_format_extra, 0, c->ch[0].output.format.format |= PISP_IMAGE_FORMAT_PLANARITY_PLANAR)
CASE(out0_format_10bit, 0, c->ch[0].output.format.format = PISP_IMAGE_FORMAT_BPS_10)
CASE(out0_compress_no_format, 0, c->global.enables |= PISP_FE_ENABLE_COMPRESS0)
CASE(out0_format_no_compress, 0,
     c->ch[0].output.format.format = PISP_IMAGE_FORMAT_COMPRESSION_MODE_1)
CASE(out1_width, 0, c->ch[1].output.format.width = W + 2)
CASE(out1_stride_short, 0, c->ch[1].output.format.stride = 16)
CASE(out1_stride_negative, 0, c->ch[1].output.format.stride = -16)
CASE(out1_huge, 0, c->ch[1].output.format.stride = 0x7ffffff0)
CASE(out1_stride_wraps_32, 0, c->ch[1].output.format.stride = 0x40000000)
CASE(out0_smaller_than_start, 0, {
	c->global.enables |= PISP_FE_ENABLE_CROP0;
	c->ch[0].crop.width = W - 8;
	c->ch[0].crop.height = H;
	c->ch[0].output.format.width = W - 8;
})
CASE(out0_stride_not_start, 0, c->ch[0].output.format.stride = STRIDE + 16)
CASE(out0_format_not_start, 0, {
	/* Good on its own: only START's format refuses it. */
	c->global.enables |= PISP_FE_ENABLE_COMPRESS0;
	c->ch[0].compress.mode = 1;
	c->ch[0].output.format.format = PISP_IMAGE_FORMAT_COMPRESSION_MODE_1;
})
CASE(out1_stride_below_16bit, 0, c->ch[1].output.format.stride = 96)
CASE(decompress, 0, c->global.enables |= PISP_FE_ENABLE_DECOMPRESS)
CASE(out1_compress_no_format, 0, {
	c->global.enables |= PISP_FE_ENABLE_COMPRESS1;
	c->ch[1].compress.mode = 1;
})
CASE(out1_format_no_compress, 0,
     c->ch[1].output.format.format = PISP_IMAGE_FORMAT_COMPRESSION_MODE_1)
#define COMPRESSED1(fmt, m)                                  \
	do {                                                 \
		c->global.enables |= PISP_FE_ENABLE_COMPRESS1; \
		c->ch[1].output.format.format = (fmt);       \
		c->ch[1].compress.mode = (m);                \
	} while (0)
CASE(compress_mode_zero, 0, COMPRESSED1(PISP_IMAGE_FORMAT_COMPRESSION_MODE_1, 0))
CASE(compress_mode_mismatch, 0, COMPRESSED1(PISP_IMAGE_FORMAT_COMPRESSION_MODE_2, 1))
CASE(compress_mode_too_big, 0, COMPRESSED1(PISP_IMAGE_FORMAT_COMPRESSION_MODE_3, 4))
CASE(compressed_at_16bit, 0,
     COMPRESSED1(PISP_IMAGE_FORMAT_COMPRESSION_MODE_1 | PISP_IMAGE_FORMAT_BPS_16, 1))
CASE(compressed_at_10bit, 0,
     COMPRESSED1(PISP_IMAGE_FORMAT_COMPRESSION_MODE_1 | PISP_IMAGE_FORMAT_BPS_10, 1))
CASE(compressed_stride_short, 0, {
	COMPRESSED1(PISP_IMAGE_FORMAT_COMPRESSION_MODE_1, 1);
	c->ch[1].output.format.stride = 48;
})
CASE(out1_8bit, 0, c->ch[1].output.format.format = PISP_IMAGE_FORMAT_BPS_8)
CASE(out1_10bit, 0, c->ch[1].output.format.format = PISP_IMAGE_FORMAT_BPS_10)
CASE(out1_12bit, 0, c->ch[1].output.format.format = PISP_IMAGE_FORMAT_BPS_12)
CASE(decimate_crop_narrow, 0, {
	c->global.enables |= PISP_FE_ENABLE_STATS_CROP | PISP_FE_ENABLE_DECIMATE;
	c->stats_crop.width = 2;
	c->stats_crop.height = H;
})
CASE(decimate_crop_short, 0, {
	c->global.enables |= PISP_FE_ENABLE_STATS_CROP | PISP_FE_ENABLE_DECIMATE;
	c->stats_crop.width = W;
	c->stats_crop.height = 2;
})
#define AGC_GOOD                                           \
	do {                                               \
		c->global.enables |= PISP_FE_ENABLE_AGC_STATS; \
		c->agc_stats.size_x = 2;                   \
		c->agc_stats.size_y = 2;                   \
		c->agc_stats.row_size_x = 2;               \
		c->agc_stats.row_size_y = 2;               \
	} while (0)
#define CDAF_GOOD                                           \
	do {                                                \
		c->global.enables |= PISP_FE_ENABLE_CDAF_STATS; \
		c->cdaf_stats.size_x = 2;                   \
		c->cdaf_stats.size_y = 2;                   \
	} while (0)
CASE(agc_size_x, 0, { AGC_GOOD; c->agc_stats.size_x = 0; })
CASE(agc_size_y, 0, { AGC_GOOD; c->agc_stats.size_y = 0; })
CASE(agc_row_size_x, 0, { AGC_GOOD; c->agc_stats.row_size_x = 0; })
CASE(agc_row_size_y, 0, { AGC_GOOD; c->agc_stats.row_size_y = 1; })
CASE(awb_size_x, 0, c->awb_stats.size_x = 0)
CASE(awb_size_y, 0, c->awb_stats.size_y = 1)
CASE(cdaf_size_x, 0, { CDAF_GOOD; c->cdaf_stats.size_x = 0; })
CASE(cdaf_size_y, 0, { CDAF_GOOD; c->cdaf_stats.size_y = 1; })
CASE(crop_zero_width, 0, {
	c->global.enables |= PISP_FE_ENABLE_CROP0;
	c->ch[0].crop.width = 0;
	c->ch[0].crop.height = H;
})
CASE(crop_offset_outside, 0, {
	c->global.enables |= PISP_FE_ENABLE_CROP0;
	c->ch[0].crop.offset_x = W;
	c->ch[0].crop.width = W;
	c->ch[0].crop.height = H;
})
CASE(crop_end_outside, 0, {
	c->global.enables |= PISP_FE_ENABLE_CROP0;
	c->ch[0].crop.offset_y = 2;
	c->ch[0].crop.width = W;
	c->ch[0].crop.height = H;
})
CASE(crop_size_vs_output, 0, {
	c->global.enables |= PISP_FE_ENABLE_CROP0;
	c->ch[0].crop.width = W - 8;
	c->ch[0].crop.height = H;
})
CASE(downscale_ratio, 0, {
	c->global.enables |= PISP_FE_ENABLE_DOWNSCALE0;
	c->ch[0].downscale.xin = 1;
	c->ch[0].downscale.xout = 2;
	c->ch[0].downscale.yin = 1;
	c->ch[0].downscale.yout = 1;
})
CASE(downscale_output_width, 0, {
	c->global.enables |= PISP_FE_ENABLE_DOWNSCALE0;
	c->ch[0].downscale.xin = 2;
	c->ch[0].downscale.xout = 1;
	c->ch[0].downscale.yin = 1;
	c->ch[0].downscale.yout = 1;
	c->ch[0].downscale.output_width = 8;
	c->ch[0].downscale.output_height = H;
})
CASE(downscale_zero_input, 0, {
	c->global.enables |= PISP_FE_ENABLE_DOWNSCALE0;
	c->ch[0].downscale.xin = 0;
	c->ch[0].downscale.xout = 1;
	c->ch[0].downscale.yin = 1;
	c->ch[0].downscale.yout = 1;
})
CASE(stats_crop_zero, 0, {
	c->global.enables |= PISP_FE_ENABLE_STATS_CROP;
	c->stats_crop.width = 0;
	c->stats_crop.height = H;
})
CASE(stats_crop_outside, 0, {
	c->global.enables |= PISP_FE_ENABLE_STATS_CROP;
	c->stats_crop.offset_x = W - 2;
	c->stats_crop.width = W;
	c->stats_crop.height = H;
})

static void m_nop(struct pisp_fe_config *c)
{
	(void)c;
}

static void m_no_output1(struct pisp_fe_config *c)
{
	c->global.enables &= ~(uint32_t)PISP_FE_ENABLE_OUTPUT1;
}

static const struct bad_case BAD[] = {
	E_(addr_stats_lo, EINVAL), E_(addr_stats_hi, EINVAL), E_(addr_out0_lo, EINVAL),
	E_(addr_out0_hi, EINVAL), E_(addr_out1_lo, EINVAL), E_(addr_out1_hi, EINVAL),
	E_(addr_input_lo, EINVAL), E_(addr_input_hi, EINVAL), E_(input_frame_id, EINVAL),
	E_(input_buf_pad, EINVAL), E_(ilines0, EINVAL), E_(ilines1, EINVAL),
	E_(disabled_output1_addr, EINVAL), E_(disabled_stats_addr, EINVAL),
	E_(disabled_block_left_on, EINVAL), E_(unknown_enable_bit, EINVAL),
	E_(no_input, EINVAL), E_(no_output0, EINVAL), E_(not_streaming, EINVAL),
	E_(input_format_not_16, EINVAL), E_(input_width_differs, EINVAL),
	E_(input_height_differs, EINVAL), E_(input_wider_than_start_cropped_back, EINVAL),
	E_(input_taller_than_start_cropped_back, EINVAL), E_(input_stride, EINVAL), E_(input_axi, EINVAL),
	E_(input_holdoff, EINVAL), E_(input_pad, EINVAL), E_(global_pad, EINVAL),
	E_(bad_bayer, EINVAL), E_(decompand_pad, EINVAL), E_(spare1, EINVAL),
	E_(bla_pad, EINVAL), E_(ch_pad, EINVAL), E_(ch_downscale_pad, EINVAL),
	E_(output_axi_qos, EINVAL), E_(output_axi_thresh, EINVAL),
	E_(out0_width, EINVAL), E_(out0_height, EINVAL), E_(out0_stride_small, EINVAL),
	E_(out0_stride_unaligned, EINVAL), E_(out0_stride_negative, EINVAL),
	E_(out0_stride2, EINVAL), E_(out0_format_extra, EINVAL), E_(out0_format_10bit, EINVAL),
	E_(out0_compress_no_format, EINVAL), E_(out0_format_no_compress, EINVAL),
	E_(out1_width, EINVAL), E_(out1_stride_short, EINVAL), E_(out1_stride_negative, EINVAL),
	E_(out1_huge, EINVAL), E_(out1_stride_wraps_32, EINVAL), E_(out0_smaller_than_start, EINVAL),
	E_(out0_stride_not_start, EINVAL), E_(out0_format_not_start, EINVAL), E_(crop_zero_width, EINVAL), E_(crop_offset_outside, EINVAL),
	E_(crop_end_outside, EINVAL), E_(crop_size_vs_output, EINVAL), E_(downscale_ratio, EINVAL),
	E_(downscale_output_width, EINVAL), E_(downscale_zero_input, EINVAL),
	E_(stats_crop_zero, EINVAL), E_(stats_crop_outside, EINVAL),
	E_(out1_stride_below_16bit, EINVAL), E_(decompress, EOPNOTSUPP),
	E_(out1_compress_no_format, EINVAL), E_(out1_format_no_compress, EINVAL),
	E_(compress_mode_zero, EINVAL), E_(compress_mode_mismatch, EINVAL),
	E_(compress_mode_too_big, EINVAL), E_(compressed_at_16bit, EINVAL),
	E_(compressed_at_10bit, EINVAL), E_(compressed_stride_short, EINVAL),
	E_(out1_8bit, EINVAL), E_(out1_10bit, EINVAL), E_(out1_12bit, EINVAL),
	E_(decimate_crop_narrow, EINVAL), E_(decimate_crop_short, EINVAL),
	E_(agc_size_x, EINVAL), E_(agc_size_y, EINVAL), E_(agc_row_size_x, EINVAL),
	E_(agc_row_size_y, EINVAL), E_(awb_size_x, EINVAL), E_(awb_size_y, EINVAL),
	E_(cdaf_size_x, EINVAL), E_(cdaf_size_y, EINVAL),
};

/* Each format bit that changes what a line is, on each output. */
static const struct { const char *name; uint32_t bit; } FORMAT_BITS[] = {
	{ "semi_planar", PISP_IMAGE_FORMAT_PLANARITY_SEMI_PLANAR },
	{ "planar", PISP_IMAGE_FORMAT_PLANARITY_PLANAR },
	{ "sampling_422", PISP_IMAGE_FORMAT_SAMPLING_422 },
	{ "sampling_420", PISP_IMAGE_FORMAT_SAMPLING_420 },
	{ "order_swapped", PISP_IMAGE_FORMAT_ORDER_SWAPPED },
	{ "shift_1", PISP_IMAGE_FORMAT_SHIFT_1 },
	{ "shift_8", PISP_IMAGE_FORMAT_SHIFT_8 },
	{ "bpp_32", PISP_IMAGE_FORMAT_BPP_32 },
	{ "hog_signed", PISP_IMAGE_FORMAT_HOG_SIGNED },
	{ "hog_unsigned", PISP_IMAGE_FORMAT_HOG_UNSIGNED },
	{ "integral_image", PISP_IMAGE_FORMAT_INTEGRAL_IMAGE },
	{ "wallpaper_roll", PISP_IMAGE_FORMAT_WALLPAPER_ROLL },
	{ "three_channel", PISP_IMAGE_FORMAT_THREE_CHANNEL },
	{ "top_bit", 0x80000000u },
};

static uint32_t format_bit;
static unsigned int format_branch;

static void m_format_bit(struct pisp_fe_config *c)
{
	c->ch[format_branch].output.format.format |= format_bit;
}

/* One bad configuration, in a fresh stream: refused by START, no register touched. */
static void one_bad(const char *name, mut_fn fn, int err, int count_delta)
{
	struct stream s;
	struct lemnos_pisp_fe_start a;
	struct lemnos_stream_job j;
	int ret;

	CHECK(!stream_setup(&s), "%s: stream", name);
	valid(cfg_slot(0));
	fn(cfg_slot(0));
	j = job_for(0, 0);
	if (count_delta > 0) {
		/* A range the configuration has no use for. */
		j.range[j.count].addr = out_r.addr;
		j.range[j.count].len = OUT0_BYTES;
		j.count += 1;
	} else if (count_delta < 0) {
		/* The ring refuses a non-zero range past the count, so this one is blanked. */
		j.count -= 1;
		memset(&j.range[j.count], 0, sizeof(j.range[0]));
	}
	post(&s, &j);
	ret = kick(&s);
	CHECK(!ret, "%s: the ring's checks pass (%d)", name, ret);
	CHECK(peek(FE_CONTROL) == 0, "%s: not written before", name);
	a = good_start(&s);
	ret = start(&a);
	CHECK(ret == -err, "%s: refused with -%d (got %d)", name, err, ret);
	CHECK(peek(FE_CONTROL) == 0 && peek(OFF(global.enables)) == 0,
	      "%s: no register touched by a refused configuration", name);
	CHECK(status_fault(&s) == (uint64_t)err, "%s: the stream ended with the errno", name);
	stream_close(&s);
}

static void test_validator(void)
{
	for (size_t i = 0; i < sizeof(BAD) / sizeof(BAD[0]); i++)
		one_bad(BAD[i].name, BAD[i].fn, BAD[i].err, BAD[i].count_delta);
	/* A job whose ranges don't match what the enables need. */
	one_bad("count_too_few", m_nop, EINVAL, -1);
	one_bad("count_too_many", m_no_output1, EINVAL, 1);
	for (size_t i = 0; i < sizeof(FORMAT_BITS) / sizeof(FORMAT_BITS[0]); i++) {
		for (format_branch = 0; format_branch < 2; format_branch++) {
			char name[64];

			format_bit = FORMAT_BITS[i].bit;
			snprintf(name, sizeof(name), "out%u_%s", format_branch,
				 FORMAT_BITS[i].name);
			one_bad(name, m_format_bit, EINVAL, 0);
		}
	}
}

/*
 * --- every variant the validator still allows, each started for real ---
 * `start_fix`, when set, makes START agree with a smaller or differently
 * formatted output 0: what must be accepted is the pair.
 */
struct good_case {
	const char *name;
	mut_fn fn;
	void (*start_fix)(struct lemnos_pisp_fe_start *a);
};

static void one_good(const struct good_case *g)
{
	struct stream s;
	struct lemnos_pisp_fe_start a;
	struct lemnos_stream_job j;
	uint32_t enables;
	int ret;

	CHECK(!stream_setup(&s), "%s: stream", g->name);
	valid(cfg_slot(0));
	g->fn(cfg_slot(0));
	enables = cfg_slot(0)->global.enables;
	j = job_for(0, 0);
	post(&s, &j);
	CHECK(!kick(&s), "%s: job accepted", g->name);
	a = good_start(&s);
	if (g->start_fix)
		g->start_fix(&a);
	ret = start(&a);
	CHECK(!ret, "%s: accepted (got %d)", g->name, ret);
	CHECK(peek(FE_CONTROL) == C_QUEUE, "%s: queued", g->name);
	CHECK(peek(OFF(global.enables)) == enables, "%s: its enables reached the block", g->name);
	CHECK(status_fault(&s) == 0, "%s: the stream carries no fault", g->name);
	CHECK(!stop(), "%s: stopped", g->name);
	stream_close(&s);
}

#define OK_(n, body)                                 \
	static void g_##n(struct pisp_fe_config *c)  \
	{                                            \
		body;                                \
	}

#define ONLY_OUT0 (c->global.enables &= ~(uint32_t)PISP_FE_ENABLE_OUTPUT1)
#define COMPRESSED(i, fmt, m)                                    \
	do {                                                     \
		c->global.enables |= PISP_FE_ENABLE_COMPRESS(i); \
		c->ch[i].output.format.format = (fmt);           \
		c->ch[i].compress.mode = (m);                    \
	} while (0)
#define NO_STATS (c->global.enables &= ~(uint32_t)PISP_FE_ENABLE_AWB_STATS)
#define STATS_ON(bit) do { NO_STATS; c->global.enables |= (bit); } while (0)

OK_(plain_16bit, ONLY_OUT0; NO_STATS)
OK_(two_outputs_16bit, (void)c)
OK_(out0_mode1, { ONLY_OUT0; COMPRESSED(0, PISP_IMAGE_FORMAT_COMPRESSION_MODE_1, 1); })
OK_(out0_mode2, { ONLY_OUT0; COMPRESSED(0, PISP_IMAGE_FORMAT_COMPRESSION_MODE_2, 2); })
OK_(out0_mode3, { ONLY_OUT0; COMPRESSED(0, PISP_IMAGE_FORMAT_COMPRESSION_MODE_3, 3); })
OK_(out1_mode1, COMPRESSED(1, PISP_IMAGE_FORMAT_COMPRESSION_MODE_1, 1))
OK_(out1_mode2, COMPRESSED(1, PISP_IMAGE_FORMAT_COMPRESSION_MODE_2, 2))
OK_(out1_mode3, COMPRESSED(1, PISP_IMAGE_FORMAT_COMPRESSION_MODE_3, 3))
OK_(out1_compressed_tightest_stride, {
	COMPRESSED(1, PISP_IMAGE_FORMAT_COMPRESSION_MODE_1, 1);
	c->ch[1].output.format.stride = 64;
})
OK_(crop_both_full, {
	c->global.enables |= PISP_FE_ENABLE_CROP0 | PISP_FE_ENABLE_CROP1;
	c->ch[0].crop.width = W;
	c->ch[0].crop.height = H;
	c->ch[1].crop.width = W;
	c->ch[1].crop.height = H;
})
OK_(crop1_smaller_offset, {
	c->global.enables |= PISP_FE_ENABLE_CROP1;
	c->ch[1].crop.offset_x = 8;
	c->ch[1].crop.offset_y = 2;
	c->ch[1].crop.width = W - 8;
	c->ch[1].crop.height = H - 2;
	c->ch[1].output.format.width = W - 8;
	c->ch[1].output.format.height = H - 2;
})
OK_(crop0_smaller, {
	ONLY_OUT0;
	c->global.enables |= PISP_FE_ENABLE_CROP0;
	c->ch[0].crop.width = W - 8;
	c->ch[0].crop.height = H;
	c->ch[0].output.format.width = W - 8;
})
OK_(downscale1, {
	c->global.enables |= PISP_FE_ENABLE_DOWNSCALE1;
	c->ch[1].downscale.xin = 2;
	c->ch[1].downscale.xout = 1;
	c->ch[1].downscale.yin = 1;
	c->ch[1].downscale.yout = 1;
	c->ch[1].downscale.output_width = W / 2;
	c->ch[1].downscale.output_height = H;
	c->ch[1].output.format.width = W / 2;
})
OK_(downscale0, {
	ONLY_OUT0;
	c->global.enables |= PISP_FE_ENABLE_DOWNSCALE0;
	c->ch[0].downscale.xin = 2;
	c->ch[0].downscale.xout = 1;
	c->ch[0].downscale.yin = 1;
	c->ch[0].downscale.yout = 1;
	c->ch[0].downscale.output_width = W / 2;
	c->ch[0].downscale.output_height = H;
	c->ch[0].output.format.width = W / 2;
})
OK_(stats_crop_smallest, {
	c->global.enables |= PISP_FE_ENABLE_STATS_CROP;
	c->stats_crop.width = 2;
	c->stats_crop.height = 2;
})
OK_(stats_crop_decimate_smallest, {
	c->global.enables |= PISP_FE_ENABLE_STATS_CROP | PISP_FE_ENABLE_DECIMATE;
	c->stats_crop.width = 4;
	c->stats_crop.height = 4;
})
OK_(decimate_no_crop, c->global.enables |= PISP_FE_ENABLE_DECIMATE)
OK_(bayer_0, c->global.bayer_order = 0)
OK_(bayer_1, c->global.bayer_order = 1)
OK_(bayer_3, c->global.bayer_order = 3)
OK_(bayer_mono, c->global.bayer_order = 128)
OK_(decompand, c->global.enables |= PISP_FE_ENABLE_DECOMPAND)
OK_(bla, c->global.enables |= PISP_FE_ENABLE_BLA)
OK_(dpc, c->global.enables |= PISP_FE_ENABLE_DPC)
OK_(stats_blc, STATS_ON(PISP_FE_ENABLE_BLC))
OK_(stats_rgby, STATS_ON(PISP_FE_ENABLE_RGBY))
OK_(stats_lsc, STATS_ON(PISP_FE_ENABLE_LSC))
OK_(stats_awb, (void)c)
OK_(stats_cdaf, { NO_STATS; CDAF_GOOD; })
OK_(stats_agc, { NO_STATS; AGC_GOOD; })
OK_(larger_stride, { ONLY_OUT0; c->ch[0].output.format.stride = STRIDE + 16; })
OK_(tightest_stride_out1, c->ch[1].output.format.stride = 128)

static void s_mode1(struct lemnos_pisp_fe_start *a)
{
	a->output_format = PISP_IMAGE_FORMAT_COMPRESSION_MODE_1;
}

static void s_mode2(struct lemnos_pisp_fe_start *a)
{
	a->output_format = PISP_IMAGE_FORMAT_COMPRESSION_MODE_2;
}

static void s_mode3(struct lemnos_pisp_fe_start *a)
{
	a->output_format = PISP_IMAGE_FORMAT_COMPRESSION_MODE_3;
}

static void s_narrower(struct lemnos_pisp_fe_start *a)
{
	a->output_width = W - 8;
}

static void s_half(struct lemnos_pisp_fe_start *a)
{
	a->output_width = W / 2;
}

static void s_stride(struct lemnos_pisp_fe_start *a)
{
	a->output_stride = STRIDE + 16;
	a->output_size = (STRIDE + 16) * H;
}

#define G_(n, f) { #n, g_##n, f }
static const struct good_case GOOD[] = {
	G_(plain_16bit, NULL), G_(two_outputs_16bit, NULL),
	G_(out0_mode1, s_mode1), G_(out0_mode2, s_mode2), G_(out0_mode3, s_mode3),
	G_(out1_mode1, NULL), G_(out1_mode2, NULL), G_(out1_mode3, NULL),
	G_(out1_compressed_tightest_stride, NULL), G_(crop_both_full, NULL),
	G_(crop1_smaller_offset, NULL), G_(crop0_smaller, s_narrower),
	G_(downscale1, NULL), G_(downscale0, s_half),
	G_(stats_crop_smallest, NULL), G_(stats_crop_decimate_smallest, NULL),
	G_(decimate_no_crop, NULL), G_(bayer_0, NULL), G_(bayer_1, NULL), G_(bayer_3, NULL),
	G_(bayer_mono, NULL), G_(decompand, NULL), G_(bla, NULL), G_(dpc, NULL),
	G_(stats_blc, NULL), G_(stats_rgby, NULL), G_(stats_lsc, NULL), G_(stats_awb, NULL),
	G_(stats_cdaf, NULL), G_(stats_agc, NULL), G_(larger_stride, s_stride),
	G_(tightest_stride_out1, NULL),
};

/* An input of 64x2: only START can agree to it, and decimating leaves no statistics. */
static void m_short_input(struct pisp_fe_config *c)
{
	c->input.format.height = 2;
	c->ch[0].output.format.height = 2;
	c->ch[1].output.format.height = 2;
}

static void m_short_input_decimate(struct pisp_fe_config *c)
{
	m_short_input(c);
	c->global.enables |= PISP_FE_ENABLE_DECIMATE;
}

static void s_short(struct lemnos_pisp_fe_start *a)
{
	a->height = 2;
	a->output_height = 2;
	a->output_size = STRIDE * 2;
}

static void test_allowed_variants(void)
{
	const struct good_case short_input = { "short_input", m_short_input, s_short };
	struct stream s;
	struct lemnos_pisp_fe_start a;
	struct lemnos_stream_job j;
	struct pisp_fe_config *c;
	int ret;

	for (size_t i = 0; i < sizeof(GOOD) / sizeof(GOOD[0]); i++)
		one_good(&GOOD[i]);
	one_good(&short_input);

	CHECK(!stream_setup(&s), "short input decimated: stream");
	valid(cfg_slot(0));
	m_short_input_decimate(cfg_slot(0));
	j = job_for(0, 0);
	post(&s, &j);
	CHECK(!kick(&s), "short input decimated: job accepted");
	a = good_start(&s);
	s_short(&a);
	ret = start(&a);
	CHECK(ret == -EINVAL, "an input that decimates to nothing is refused (got %d)", ret);
	stream_close(&s);

	/* Windows of a block that's off aren't live: only its enable makes them count. */
	CHECK(!stream_setup(&s), "stale sub-blocks: stream");
	valid(cfg_slot(0));
	c = cfg_slot(0);
	for (unsigned int i = 0; i < 2; i++) {
		c->ch[i].crop.offset_x = 0xbeef;
		c->ch[i].crop.width = 999;
		c->ch[i].downscale.xin = 9;
		c->ch[i].downscale.output_width = 777;
		c->ch[i].compress.offset = 0x5555;
		c->ch[i].compress.mode = 3;
	}
	j = job_for(0, 0);
	post(&s, &j);
	CHECK(!kick(&s), "stale sub-blocks: job accepted");
	a = good_start(&s);
	ret = start(&a);
	CHECK(!ret, "stale fields in disabled sub-blocks don't refuse it (got %d)", ret);
	for (unsigned int i = 0; i < 2; i++) {
		CHECK(peek(OFF(ch[i].crop.offset_x)) == 0 && peek(OFF(ch[i].crop.width)) == 0,
		      "branch %u: a disabled crop doesn't reach the registers", i);
		CHECK(peek(OFF(ch[i].downscale.xin)) == 0 &&
			      peek(OFF(ch[i].downscale.output_width)) == 0,
		      "branch %u: a disabled downscale doesn't reach the registers", i);
		CHECK(peek(OFF(ch[i].compress)) == 0,
		      "branch %u: a disabled compress doesn't reach the registers", i);
	}
	CHECK(!stop(), "stale sub-blocks: stopped");
	stream_close(&s);
}

/* START's own format: refused before the stream or the job is looked at. */
static void test_start_format(void)
{
	static const struct { const char *name; uint32_t format; } F[] = {
		{ "8-bit", PISP_IMAGE_FORMAT_BPS_8 },
		{ "10-bit", PISP_IMAGE_FORMAT_BPS_10 },
		{ "12-bit", PISP_IMAGE_FORMAT_BPS_12 },
		{ "compressed at 16", PISP_IMAGE_FORMAT_COMPRESSION_MODE_1 | PISP_IMAGE_FORMAT_BPS_16 },
		{ "swapped", PISP_IMAGE_FORMAT_BPS_16 | PISP_IMAGE_FORMAT_ORDER_SWAPPED },
		{ "planar", PISP_IMAGE_FORMAT_BPS_16 | PISP_IMAGE_FORMAT_PLANARITY_PLANAR },
		{ "wallpaper", PISP_IMAGE_FORMAT_BPS_16 | PISP_IMAGE_FORMAT_WALLPAPER_ROLL },
	};
	struct stream s;
	struct lemnos_pisp_fe_start a;
	struct lemnos_stream_job j;

	CHECK(!stream_setup(&s), "START formats: stream");
	valid(cfg_slot(0));
	j = job_for(0, 0);
	post(&s, &j);
	CHECK(!kick(&s), "START formats: job accepted");
	for (size_t i = 0; i < sizeof(F) / sizeof(F[0]); i++) {
		uint32_t control = peek(FE_CONTROL), enables = peek(OFF(global.enables));
		int ret;

		a = good_start(&s);
		a.output_format = F[i].format;
		ret = start(&a);
		CHECK(ret == -EINVAL, "START %s: refused with -EINVAL (got %d)", F[i].name, ret);
		CHECK(peek(FE_CONTROL) == control && peek(OFF(global.enables)) == enables,
		      "START %s: no register touched", F[i].name);
		CHECK(status_fault(&s) == 0, "START %s: the stream is untouched", F[i].name);
	}
	stream_close(&s);
}

/* The range checks of the stream, on the job ranges: the ring refuses these. */
static void one_bad_range(const char *name, unsigned int i, uint64_t addr, uint64_t len, int err)
{
	struct stream s;
	struct lemnos_stream_job j;
	int ret;

	CHECK(!stream_setup(&s), "%s: stream", name);
	valid(cfg_slot(0));
	j = job_for(0, 0);
	j.range[i].addr = addr;
	j.range[i].len = len;
	post(&s, &j);
	ret = kick(&s);
	CHECK(ret == -err, "%s: the ring refuses it with -%d (got %d)", name, err, ret);
	stream_close(&s);
}

static void test_ranges(void)
{
	struct stream s;
	struct lemnos_pisp_fe_start a;
	struct lemnos_stream_job j;
	int ret;

	/* Each address field reaches the block only through these. */
	one_bad_range("config_out_of_bounds", 0, 0x1000, CFG_SIZE, EFAULT);
	one_bad_range("config_overflow", 0, UINT64_MAX - 4, CFG_SIZE, EOVERFLOW);
	one_bad_range("stats_out_of_bounds", 1, 0x1000, STATS_SIZE, EFAULT);
	one_bad_range("stats_overflow", 1, UINT64_MAX - 4, STATS_SIZE, EOVERFLOW);
	one_bad_range("output1_out_of_bounds", 2, 0x1000, OUT0_BYTES, EFAULT);
	one_bad_range("output1_overflow", 2, UINT64_MAX - 4, OUT0_BYTES, EOVERFLOW);

	/* The statistics range: must be in a registered buffer, not the pool. */
	CHECK(!stream_setup(&s), "stats in the pool: stream");
	valid(cfg_slot(0));
	j = job_for(0, 0);
	j.range[1].addr = s.pool_addr;
	j.range[1].len = STATS_SIZE;
	post(&s, &j);
	CHECK(!kick(&s), "the ring accepts the range");
	a = good_start(&s);
	ret = start(&a);
	CHECK(ret == -EACCES, "statistics in the pool is refused (got %d)", ret);
	stream_close(&s);

	CHECK(!stream_setup(&s), "output 1 in the pool: stream");
	valid(cfg_slot(0));
	j = job_for(0, 0);
	j.range[2].addr = s.pool_addr;
	post(&s, &j);
	CHECK(!kick(&s), "the ring accepts the range");
	a = good_start(&s);
	ret = start(&a);
	CHECK(ret == -EACCES, "output 1 in the pool is refused (got %d)", ret);
	stream_close(&s);

	CHECK(!stream_setup(&s), "unaligned output 1: stream");
	valid(cfg_slot(0));
	j = job_for(0, 0);
	j.range[2].addr = out_r.addr + 8;
	post(&s, &j);
	CHECK(!kick(&s), "the ring accepts the range");
	a = good_start(&s);
	ret = start(&a);
	CHECK(ret == -EINVAL, "unaligned output 1 address refused (got %d)", ret);
	stream_close(&s);

	CHECK(!stream_setup(&s), "bad buffer index: stream");
	valid(cfg_slot(0));
	j = job_for(0, 0);
	j.buffer = BUFFERS;
	post(&s, &j);
	CHECK(kick(&s) == -EINVAL, "a pool buffer index past the pool is refused by the ring");
	stream_close(&s);

	CHECK(!stream_setup(&s), "wrong stats size: stream");
	valid(cfg_slot(0));
	j = job_for(0, 0);
	j.range[1].len = STATS_SIZE - 16;
	post(&s, &j);
	CHECK(!kick(&s), "the ring accepts the range");
	a = good_start(&s);
	ret = start(&a);
	CHECK(ret == -EINVAL, "statistics buffer of the wrong size refused (got %d)", ret);
	CHECK(peek(FE_CONTROL) == 0, "nothing touched");
	stream_close(&s);

	CHECK(!stream_setup(&s), "config in the pool: stream");
	valid(cfg_slot(0));
	j = job_for(0, 0);
	j.range[0].addr = s.pool_addr;
	post(&s, &j);
	CHECK(!kick(&s), "the ring accepts it");
	a = good_start(&s);
	ret = start(&a);
	CHECK(ret == -EFAULT, "configuration in the pool refused (got %d)", ret);
	stream_close(&s);

	CHECK(!stream_setup(&s), "wrong config size: stream");
	valid(cfg_slot(0));
	j = job_for(0, 0);
	j.range[0].len = CFG_SIZE - 8;
	post(&s, &j);
	CHECK(!kick(&s), "the ring accepts it");
	a = good_start(&s);
	ret = start(&a);
	CHECK(ret == -EINVAL, "configuration of the wrong size refused (got %d)", ret);
	stream_close(&s);

	CHECK(!stream_setup(&s), "short output 1: stream");
	valid(cfg_slot(0));
	j = job_for(0, 0);
	j.range[2].len = OUT0_BYTES - 16;
	post(&s, &j);
	CHECK(!kick(&s), "the ring accepts it");
	a = good_start(&s);
	ret = start(&a);
	CHECK(ret == -EINVAL, "output 1 buffer too small refused (got %d)", ret);
	stream_close(&s);

	CHECK(!stream_setup(&s), "unaligned stats: stream");
	valid(cfg_slot(0));
	j = job_for(0, 0);
	j.range[1].addr = stats_r.addr + 4;
	post(&s, &j);
	CHECK(!kick(&s), "the ring accepts it");
	a = good_start(&s);
	ret = start(&a);
	CHECK(ret == -EINVAL, "unaligned statistics address refused (got %d)", ret);
	stream_close(&s);

	/* Unrelated START arguments, refused before the stream is touched. */
	CHECK(!stream_setup(&s), "bad starts: stream");
	valid(cfg_slot(0));
	j = job_for(0, 0);
	post(&s, &j);
	kick(&s);
	a = good_start(&s);
	a.output_stride = 8;
	CHECK(start(&a) == -EINVAL, "START: stride not a multiple of 16");
	a = good_start(&s);
	a.output_size = STRIDE;
	CHECK(start(&a) == -EINVAL, "START: size below stride * height");
	a = good_start(&s);
	a.lanes = 9;
	CHECK(start(&a) == -EINVAL, "START: too many lanes");
	a = good_start(&s);
	a.vc = 4;
	CHECK(start(&a) == -EINVAL, "START: bad virtual channel");
	a = good_start(&s);
	a.flags = 1;
	CHECK(start(&a) == -EINVAL, "START: nonzero flags");
	a = good_start(&s);
	a.output_size = 0x7ffffff0;
	CHECK(start(&a) == -EINVAL, "START: frame bigger than a buffer");
	a = good_start(&s);
	a.stream_fd = pool_fd;
	CHECK(start(&a) == -EINVAL, "START: a descriptor that isn't a stream");
	stream_close(&s);
}

/* --- what reaches the registers --- */

static struct lemnos_pisp_fe_trace_entry trace[4000];

static uint32_t read_trace(void)
{
	struct lemnos_pisp_fe_test_trace t = {
		.entries = (uint64_t)(uintptr_t)trace,
		.max = sizeof(trace) / sizeof(trace[0]),
	};

	CHECK(!ioctl(ctl, LEMNOS_PISP_FE_IOC_TEST_TRACE, &t), "trace read");
	return t.count;
}

static void expect_addr(const char *what, unsigned int reg, uint64_t want)
{
	uint64_t got = (uint64_t)peek(reg + 4) << 32 | peek(reg);

	CHECK(got == want, "%s: register holds %#llx (want %#llx)", what, (unsigned long long)got,
	      (unsigned long long)want);
}

static void test_good_and_addresses(void)
{
	struct stream s;
	struct lemnos_pisp_fe_start a;
	struct lemnos_stream_job j;
	uint32_t buf, bytes;
	uint64_t ident;
	int ret;

	CHECK(!stream_setup(&s), "good config: stream");
	valid(cfg_slot(0));
	/* Garbage in a disabled branch's configuration, which the kernel must blank. */
	cfg_slot(0)->global.enables &= ~(uint32_t)PISP_FE_ENABLE_OUTPUT1;
	cfg_slot(0)->ch[1].crop.width = 999;
	cfg_slot(0)->ch[1].output.format.stride = 9999;
	j = job_for(0, 1);
	post(&s, &j);
	CHECK(!kick(&s), "job accepted");
	a = good_start(&s);
	ret = start(&a);
	CHECK(!ret, "a valid configuration starts (got %d)", ret);
	CHECK(peek(FE_CONTROL) == C_QUEUE, "the first job was queued");
	expect_addr("stats", OFF(stats_buffer), stats_r.addr);
	expect_addr("output 0", OFF(output_buffer[0]), s.pool_addr + 1 * pool_stride);
	expect_addr("output 1", OFF(output_buffer[1]), 0);
	CHECK(peek(OFF(ch[1].crop.width)) == 0 && peek(OFF(ch[1].output.format.stride)) == 0,
	      "a disabled branch doesn't reach the registers");
	CHECK(peek(OFF(ch[0].output.ilines)) == 0x80, "ilines is the module's own value");
	/* Frame path: start of frame latches, end publishes the pool buffer. */
	fe_fire(TFE_SOF);
	fe_fire(TFE_EOF);
	CHECK(pop_frame(&s, &buf, &bytes, &ident) == 1, "a frame was published");
	CHECK(buf == 1 && bytes == OUT0_BYTES && ident == 0x1000,
	      "buffer %u, %u bytes, identity %#llx", buf, bytes, (unsigned long long)ident);
	CHECK(fe_info().written == 1, "one frame written");
	CHECK(!stop(), "stopped");
	stream_close(&s);

	/* Full job: statistics and output 1 both named, addresses from the ranges. */
	CHECK(!stream_setup(&s), "full job: stream");
	valid(cfg_slot(0));
	j = job_for(0, 0);
	post(&s, &j);
	CHECK(!kick(&s), "job accepted");
	a = good_start(&s);
	CHECK(!start(&a), "full job starts");
	expect_addr("stats (full)", OFF(stats_buffer), stats_r.addr);
	expect_addr("output 0 (full)", OFF(output_buffer[0]), s.pool_addr);
	expect_addr("output 1 (full)", OFF(output_buffer[1]), out_r.addr);
	memset(pool_cpu, 0, BUFFERS * pool_stride);
	memset(stats_r.map, 0, stats_map_bytes);
	memset(out_r.map, 0, out_map_bytes);
	fe_fire(TFE_SOF);
	fe_fire(TFE_EOF);
	CHECK(pop_frame(&s, &buf, &bytes, &ident) == 1 && buf == 0 && bytes == OUT0_BYTES,
	      "the frame was published");
	CHECK(pool_cpu[0] && pool_cpu[OUT0_BYTES - 1] && !pool_cpu[OUT0_BYTES],
	      "the mock's output 0 filled exactly the frame's bytes of the job's pool buffer");
	CHECK(!pool_cpu[pool_stride], "the next pool buffer was not written");
	CHECK(((uint8_t *)stats_r.map)[0] && ((uint8_t *)stats_r.map)[STATS_SIZE - 1],
	      "statistics landed in the registered buffer");
	CHECK(((uint8_t *)out_r.map)[0] && ((uint8_t *)out_r.map)[OUT0_BYTES - 1] &&
		      !((uint8_t *)out_r.map)[OUT0_BYTES],
	      "output 1 filled exactly stride times height");
	CHECK(!stop(), "stopped");
	stream_close(&s);

	/* No statistics: no buffer, so a floating region must not reach the block. */
	CHECK(!stream_setup(&s), "no stats: stream");
	valid(cfg_slot(0));
	cfg_slot(0)->global.enables &= ~(uint32_t)(0x1fe0 | PISP_FE_ENABLE_OUTPUT1);
	cfg_slot(0)->floating_stats.regions[0].size_x = 64;
	j = job_for(0, 0);
	post(&s, &j);
	CHECK(!kick(&s), "job accepted");
	a = good_start(&s);
	CHECK(!start(&a), "a job without statistics starts");
	expect_addr("stats (none)", OFF(stats_buffer), 0);
	CHECK(peek(OFF(floating_stats.regions[0].size_x)) == 0,
	      "with no statistics a floating region doesn't reach the registers");
	CHECK(!stop(), "stopped");
	stream_close(&s);

	/* A compressed output 0 and an enabled crop: the validator's other paths. */
	CHECK(!stream_setup(&s), "compressed: stream");
	valid(cfg_slot(0));
	cfg_slot(0)->global.enables &= ~(uint32_t)PISP_FE_ENABLE_OUTPUT1;
	cfg_slot(0)->global.enables |= PISP_FE_ENABLE_COMPRESS0;
	cfg_slot(0)->ch[0].output.format.format = PISP_IMAGE_FORMAT_COMPRESSION_MODE_1;
	cfg_slot(0)->ch[0].compress.mode = 1;
	j = job_for(0, 0);
	post(&s, &j);
	CHECK(!kick(&s), "job accepted");
	a = good_start(&s);
	a.output_format = PISP_IMAGE_FORMAT_COMPRESSION_MODE_1;
	ret = start(&a);
	CHECK(!ret, "a compressed output starts (got %d)", ret);
	CHECK(!stop(), "stopped");
	stream_close(&s);
}

/* The receiver and the front end share one channel: never both. */
static void test_exclusive(void)
{
	struct stream s, s2;
	struct lemnos_pisp_fe_start a;
	struct lemnos_rp1_csi2_start r;
	struct lemnos_stream_job j;
	int ret;

	CHECK(!stream_setup(&s), "exclusive: stream");
	CHECK(!stream_setup(&s2), "exclusive: second stream");
	valid(cfg_slot(0));
	cfg_slot(0)->global.enables &= ~(uint32_t)PISP_FE_ENABLE_OUTPUT1;
	j = job_for(0, 0);
	post(&s, &j);
	CHECK(!kick(&s), "job");
	a = good_start(&s);
	CHECK(!start(&a), "front end started");
	memset(&r, 0, sizeof(r));
	r.stream_fd = s2.fd;
	r.lanes = 2;
	r.mbps = 800;
	r.dt = 0x2b;
	r.width = W;
	r.height = H;
	r.stride = STRIDE;
	r.size = OUT0_BYTES;
	ret = ioctl(ctl, LEMNOS_RP1_CSI2_IOC_START, &r) ? -errno : 0;
	CHECK(ret == -EBUSY, "the receiver is refused while the front end runs (%d)", ret);
	CHECK(!stop(), "front end stopped");
	stream_close(&s);

	ret = ioctl(ctl, LEMNOS_RP1_CSI2_IOC_START, &r) ? -errno : 0;
	CHECK(!ret, "the receiver starts once it is free (%d)", ret);
	CHECK(!stream_setup(&s), "exclusive: third stream");
	valid(cfg_slot(0));
	cfg_slot(0)->global.enables &= ~(uint32_t)PISP_FE_ENABLE_OUTPUT1;
	j = job_for(0, 0);
	post(&s, &j);
	CHECK(!kick(&s), "job");
	a = good_start(&s);
	ret = start(&a);
	CHECK(ret == -EBUSY, "the front end is refused while the receiver runs (%d)", ret);
	CHECK(!ioctl(ctl, LEMNOS_RP1_CSI2_IOC_STOP), "receiver stopped");
	stream_close(&s);
	stream_close(&s2);
}

/* What the configuration library really builds gets through the module's checks. */
static void test_library_config(void)
{
	struct stream s;
	struct lemnos_pisp_fe_start a;
	struct lemnos_stream_job j;
	int ret;

	CHECK(sizeof(lemnos_pisp_fe_fixture) == CFG_SIZE, "the fixture is one configuration");
	CHECK(!stream_setup(&s), "library config: stream");
	memcpy(cfg_slot(0), lemnos_pisp_fe_fixture, CFG_SIZE);
	j = job_for(0, 0);
	CHECK(j.count == 2, "it asks for the statistics buffer");
	post(&s, &j);
	CHECK(!kick(&s), "job");
	a = good_start(&s);
	ret = start(&a);
	CHECK(!ret, "the library's configuration starts (got %d)", ret);
	expect_addr("library config's stats", OFF(stats_buffer), stats_r.addr);
	CHECK(!stop(), "stopped");
	stream_close(&s);
}

static int contains_write(uint32_t n, uint32_t offset, uint32_t value)
{
	for (uint32_t i = 0; i < n; i++)
		if (trace[i].offset == offset && trace[i].value == value)
			return (int)i;
	return -1;
}

static int contains_offset(uint32_t n, uint32_t offset)
{
	for (uint32_t i = 0; i < n; i++)
		if (trace[i].offset == offset)
			return (int)i;
	return -1;
}

static void test_sequence(void)
{
	struct stream s;
	struct lemnos_pisp_fe_start a;
	struct lemnos_stream_job j;
	uint32_t n;
	int ch, reset, ints, en, first, last, queue, open_rx;

	CHECK(!stream_setup(&s), "sequence: stream");
	valid(cfg_slot(0));
	valid(cfg_slot(1));
	cfg_slot(1)->global.enables &= ~(uint32_t)PISP_FE_ENABLE_OUTPUT1;
	j = job_for(0, 0);
	post(&s, &j);
	CHECK(!kick(&s), "first job");
	a = good_start(&s);
	(void)read_trace();
	CHECK(!start(&a), "started");
	n = read_trace();
	CHECK(n > 160, "the start's writes were logged (%u)", n);
	ch = contains_write(n, LEMNOS_PISP_FE_MARK_CHANNEL_START, 0);
	reset = contains_write(n, FE_CONTROL, C_RESET);
	ints = contains_write(n, FE_INT_STATUS, 0xffffffffu);
	en = contains_offset(n, FE_INT_EN);
	first = contains_offset(n, FE_CONFIG);
	last = contains_offset(n, FE_CONFIG + 0x270 - 4);
	queue = contains_write(n, FE_CONTROL, C_QUEUE);
	open_rx = contains_write(n, LEMNOS_PISP_FE_MARK_OPEN_RX, 0);
	CHECK(ch >= 0 && ch < reset && reset < ints && ints < en && en < first && first < last &&
		      last < queue && queue < open_rx,
	      "order: channel %d, reset %d, int clear %d, int en %d, config %d..%d, queue %d, open %d",
	      ch, reset, ints, en, first, last, queue, open_rx);
	CHECK(open_rx == (int)n - 1, "the link is opened last (%d of %u)", open_rx, n);
	{
		struct tcsi_regs regs;

		memset(&regs, 0, sizeof(regs));
		CHECK(!ioctl(hw, TCSI_IOC_PEEK, &regs), "channel registers read");
		CHECK((regs.ctrl & CH_CTRL_MODE_FE) == CH_CTRL_MODE_FE && (regs.ctrl & CH_CTRL_AUTO_ARM),
		      "the channel hands its frames to the front end (%#x)", regs.ctrl);
		CHECK(regs.frame_size == (H << 16 | W), "the channel's frame is START's (%#x)",
		      regs.frame_size);
	}
	CHECK(trace[en].value == (INT_EOF | INT_SOF | INT_LINES),
	      "INT_EN is what the stock driver enables (%#x)", trace[en].value);
	/* Every configuration register exactly once, in order, up to the dirty flags. */
	{
		uint32_t expect = FE_CONFIG, words = 0;

		for (uint32_t i = (uint32_t)first; i <= (uint32_t)last; i++) {
			if (trace[i].offset != expect)
				break;
			expect += 4;
			words++;
		}
		CHECK(words == 0x270 / 4, "%u words written, expected %u", words, 0x270 / 4);
		CHECK(trace[(uint32_t)first + offsetof(struct pisp_fe_config, stats_buffer.addr_lo) / 4]
				      .value == (uint32_t)stats_r.addr,
		      "a config word is the validated value");
	}

	/* A second job armed in the interrupt: latch, ack, config, queue. */
	{
		struct lemnos_stream_job j2 = job_for(1, 1);
		uint32_t m;
		int l, q;

		post(&s, &j2);
		CHECK(!kick(&s), "second job posted");
		fe_fire(TFE_SOF);
		m = read_trace();
		l = contains_write(m, FE_CONTROL, C_LATCH);
		q = contains_write(m, FE_CONTROL, C_QUEUE);
		CHECK(l >= 0 && q > l, "interrupt: latch %d then queue %d", l, q);
		CHECK(contains_write(m, FE_INT_STATUS, TFE_SOF) > l, "the interrupt is acknowledged");
		expect_addr("second job's output", OFF(output_buffer[0]), s.pool_addr + pool_stride);
	}

	/* A previous job not yet consumed: nothing is queued over it. */
	{
		uint64_t st0 = fe_info().stalled;
		uint32_t m;
		struct lemnos_stream_job j3;

		valid(cfg_slot(2));
		cfg_slot(2)->global.enables &= ~(uint32_t)PISP_FE_ENABLE_OUTPUT1;
		j3 = job_for(2, 2);
		post(&s, &j3);
		CHECK(!kick(&s), "third job posted");
		fe_set_status(1);
		fe_fire(TFE_SOF);
		m = read_trace();
		CHECK(contains_write(m, FE_CONTROL, C_QUEUE) < 0,
		      "a queued job isn't overwritten (%u writes)", m);
		CHECK(fe_info().stalled > st0, "reported as a stall");
		fe_set_status(0);
		fe_fire(TFE_EOF);
		m = read_trace();
		CHECK(contains_write(m, FE_CONTROL, C_QUEUE) >= 0, "armed once the block is ready");
	}

	/* Stop order: channel, then INT_EN 0, then ABORT. */
	{
		uint32_t m;
		int c, ie, ab;

		(void)read_trace();
		CHECK(!stop(), "stopped");
		m = read_trace();
		c = contains_write(m, LEMNOS_PISP_FE_MARK_CHANNEL_STOP, 0);
		ie = contains_write(m, FE_INT_EN, 0);
		ab = contains_write(m, FE_CONTROL, C_ABORT);
		CHECK(c >= 0 && c < ie && ie < ab, "stop: channel %d, ints off %d, abort %d", c, ie, ab);
		{
			struct tcsi_regs regs;

			memset(&regs, 0, sizeof(regs));
			CHECK(!ioctl(hw, TCSI_IOC_PEEK, &regs), "channel registers read");
			CHECK(regs.ctrl & CH_CTRL_FORCE, "the channel really was forced (%#x)", regs.ctrl);
		}
	}
	stream_close(&s);
}

/* The consumer's copy changes after submission: the module's copy doesn't. */
static void test_toctou(void)
{
	struct stream s;
	struct lemnos_pisp_fe_start a;
	struct lemnos_stream_job j;
	struct pisp_fe_config *user;
	int ret;

	/* Overwritten by the kernel at the exact moment after the copy. */
	CHECK(!stream_setup(&s), "scribble: stream");
	valid(cfg_slot(0));
	j = job_for(0, 0);
	post(&s, &j);
	CHECK(!kick(&s), "job");
	ret = ioctl(ctl, LEMNOS_PISP_FE_IOC_TEST_SCRIBBLE);
	CHECK(!ret, "scribble armed (%d)", ret);
	a = good_start(&s);
	ret = start(&a);
	CHECK(!ret, "validated the copy, not the scribbled memory (got %d)", ret);
	CHECK(cfg_slot(0)->ch[0].output.format.stride == 0x7ffffff0,
	      "the consumer's memory was changed after the copy");
	CHECK(peek(OFF(ch[0].output.format.stride)) == STRIDE,
	      "the block got the module's copy (%u)", peek(OFF(ch[0].output.format.stride)));
	CHECK(!stop(), "stopped");
	stream_close(&s);

	/* The consumer changes a job already armed: nothing reaches the registers. */
	CHECK(!stream_setup(&s), "after arm: stream");
	valid(cfg_slot(0));
	cfg_slot(0)->global.enables &= ~(uint32_t)PISP_FE_ENABLE_OUTPUT1;
	j = job_for(0, 0);
	post(&s, &j);
	CHECK(!kick(&s), "job");
	a = good_start(&s);
	CHECK(!start(&a), "started");
	user = cfg_slot(0);
	user->ch[0].output.format.stride = 0x7ffffff0;
	user->stats_buffer.addr_lo = 0xdead0000;
	fe_fire(TFE_SOF);
	fe_fire(TFE_EOF);
	CHECK(peek(OFF(ch[0].output.format.stride)) == STRIDE, "the armed job kept its stride");
	expect_addr("stats after a change in memory", OFF(stats_buffer), stats_r.addr);
	CHECK(fe_info().written == 1, "the frame completed");
	CHECK(!stop(), "stopped");
	stream_close(&s);

	/* The consumer changes a job not yet read: refused when read, not after. */
	CHECK(!stream_setup(&s), "before arm: stream");
	valid(cfg_slot(0));
	cfg_slot(0)->global.enables &= ~(uint32_t)PISP_FE_ENABLE_OUTPUT1;
	valid(cfg_slot(1));
	cfg_slot(1)->global.enables &= ~(uint32_t)PISP_FE_ENABLE_OUTPUT1;
	post(&s, &(struct lemnos_stream_job){ .identity = 1, .buffer = 0, .count = 1,
			.range = { { cfg_r.addr, CFG_SIZE } } });
	/* A second job, with no statistics or output 1 asked for. */
	cfg_slot(1)->global.enables &= ~(uint32_t)PISP_FE_ENABLE_AWB_STATS;
	cfg_slot(0)->global.enables &= ~(uint32_t)PISP_FE_ENABLE_AWB_STATS;
	post(&s, &(struct lemnos_stream_job){ .identity = 2, .buffer = 1, .count = 1,
			.range = { { cfg_r.addr + CFG_SIZE, CFG_SIZE } } });
	CHECK(!kick(&s), "jobs");
	a = good_start(&s);
	CHECK(!start(&a), "started");
	/* The second job's memory goes bad before the frame start reads it. */
	cfg_slot(1)->ch[0].output.format.stride = -16;
	fe_fire(TFE_SOF);
	CHECK(status_fault(&s) == EINVAL, "the change is seen by the one read and refused");
	expect_addr("the bad job reached no register", OFF(output_buffer[0]), s.pool_addr);
	CHECK(!stop(), "stopped");
	stream_close(&s);
}

/* A hostile interrupt status: nothing beyond the known bits is ever acted on. */
static void test_hostile_hw(void)
{
	struct stream s;
	struct lemnos_pisp_fe_start a;
	struct lemnos_stream_job j;
	uint64_t f0;

	CHECK(!stream_setup(&s), "hostile: stream");
	valid(cfg_slot(0));
	cfg_slot(0)->global.enables &= ~(uint32_t)PISP_FE_ENABLE_OUTPUT1;
	j = job_for(0, 0);
	post(&s, &j);
	CHECK(!kick(&s), "job");
	a = good_start(&s);
	CHECK(!start(&a), "started");
	f0 = fe_info().frames;
	fe_fire(0xffffffffu);
	CHECK(fe_info().frames == f0 + 1, "all bits set: one frame start counted");
	fe_fire(0xfffffffcu);
	CHECK(fe_info().frames == f0 + 1, "unknown bits ignored");
	fe_fire(TFE_SOF);
	CHECK(fe_info().errors == 0 && fe_info().frames == f0 + 2, "next frame, nothing armed");
	fe_fire(TFE_SOF);
	CHECK(fe_info().errors == 0 && fe_info().dropped >= 1, "start with no frame end is dropped");
	CHECK(!stop(), "stopped");
	stream_close(&s);
}

/* --- stopping --- */

/* How many attachments the debugfs list shows for the device; each is one line. */
static int bufinfo_attached(const char *dev)
{
	FILE *f = fopen("/sys/kernel/debug/dma_buf/bufinfo", "r");
	char line[256];
	int n = 0;

	if (!f)
		return -1;
	while (fgets(line, sizeof(line), f))
		n += strstr(line, dev) != NULL;
	fclose(f);
	return n;
}

/* --- output 0 into a buffer's own dma-buf (LEMNOS_STREAM_CREATE_BUFFERS) --- */

struct own {
	int fd;
	uint64_t addr;
	uint8_t *cpu;
};

static struct own own_make(const char *heap)
{
	struct own o = { .fd = make_buf(heap, BUFFER_BYTES) };

	if (o.fd < 0)
		return o;
	o.cpu = mmap(NULL, BUFFER_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, o.fd, 0);
	if (o.cpu == MAP_FAILED) {
		close(o.fd);
		o.fd = -1;
	} else {
		memset(o.cpu, 0, BUFFER_BYTES);
	}
	return o;
}

static void own_drop(struct own *o)
{
	if (o->fd < 0)
		return;
	munmap(o->cpu, BUFFER_BYTES);
	close(o->fd);
	o->fd = -1;
}

static int own_set(const struct stream *s, uint32_t b, struct own *o)
{
	struct lemnos_stream_buffer a = { .buffer = b, .fd = o->fd };

	if (ioctl(s->fd, LEMNOS_STREAM_IOC_BUFFER, &a))
		return -errno;
	o->addr = a.addr;
	return 0;
}

static void simple(struct pisp_fe_config *c)
{
	valid(c);
	c->global.enables &= ~(uint32_t)(0x1fe0 | PISP_FE_ENABLE_OUTPUT1);
}

static void test_own_buffers(const char *heap, const char *dev)
{
	struct stream s;
	struct lemnos_pisp_fe_start a;
	struct lemnos_stream_job j;
	struct own o[2], fresh;
	uint32_t buf, bytes;
	uint64_t ident, t;
	int ret, before = bufinfo_attached(dev);

	own_buffers = 1;
	o[0] = own_make(heap);
	o[1] = own_make(heap);
	fresh = own_make(heap);
	CHECK(o[0].fd >= 0 && o[1].fd >= 0 && fresh.fd >= 0, "own buffers: three dma-bufs");

	/* A job whose buffer has no memory never reaches the block. */
	CHECK(!stream_setup(&s) && s.pool_addr == 0, "own buffers: a stream with no pool");
	simple(cfg_slot(0));
	j = job_for(0, 1);
	post(&s, &j);
	ret = kick(&s);
	CHECK(ret == -EFAULT, "a job on a buffer with no memory is refused by the ring (got %d)", ret);
	a = good_start(&s);
	ret = start(&a);
	CHECK(ret == -EFAULT && !fe_info().streaming, "and START on that stream is refused (got %d)", ret);
	stream_close(&s);

	CHECK(!stream_setup(&s), "own buffers: stream");
	CHECK(!own_set(&s, 0, &o[0]) && !own_set(&s, 1, &o[1]) && o[0].addr != o[1].addr,
	      "buffers 0 and 1 get memory of their own");
	simple(cfg_slot(0));
	j = job_for(0, 1);
	post(&s, &j);
	CHECK(!kick(&s), "job on buffer 1 accepted");
	CHECK(own_set(&s, 1, &fresh) == -EBUSY, "a buffer a job names can't change memory: EBUSY");
	a = good_start(&s);
	ret = start(&a);
	CHECK(!ret, "it starts (got %d)", ret);
	expect_addr("output 0 is buffer 1's own dma-buf", OFF(output_buffer[0]), o[1].addr);
	CHECK(own_set(&s, 1, &fresh) == -EBUSY, "nor once the block has it: EBUSY");
	fe_fire(TFE_SOF);
	fe_fire(TFE_EOF);
	CHECK(pop_frame(&s, &buf, &bytes, &ident) == 1 && buf == 1 && bytes == OUT0_BYTES,
	      "the frame was published on buffer 1");
	CHECK(o[1].cpu[0] && o[1].cpu[OUT0_BYTES - 1] && !o[1].cpu[OUT0_BYTES],
	      "the block's output 0 filled exactly the frame's bytes of buffer 1's own dma-buf");
	CHECK(!o[0].cpu[0] && !fresh.cpu[0], "and nothing else");

	/* The consumer holds buffer 1: it may be given new memory, and the next job writes there. */
	CHECK(!own_set(&s, 1, &fresh) && fresh.addr != o[1].addr, "a held buffer is given new memory");
	memset(o[1].cpu, 0, BUFFER_BYTES);
	{
		size_t at = s.returns_at + (s.returns_head & (FRAME_SLOTS - 1));

		store(&s, at, 1ull | (uint64_t)load(&s, LEMNOS_RING_FRAMES + 4) << 32);
		s.returns_head++;
		store(&s, LEMNOS_RING_RETURNS_HEAD, s.returns_head);
	}
	simple(cfg_slot(1));
	j = job_for(1, 1);
	post(&s, &j);
	CHECK(!kick(&s), "the next job on buffer 1 accepted");
	/* This frame start has no job latched; it queues the new one for the next. */
	fe_fire(TFE_SOF);
	expect_addr("output 0 follows the swap", OFF(output_buffer[0]), fresh.addr);
	fe_fire(TFE_SOF);
	fe_fire(TFE_EOF);
	CHECK(pop_frame(&s, &buf, &bytes, &ident) == 1 && buf == 1 && fresh.cpu[0] &&
		      fresh.cpu[OUT0_BYTES - 1],
	      "its frame is in the new memory");
	CHECK(!o[1].cpu[0] && !o[1].cpu[OUT0_BYTES - 1], "and the memory it had is not written again");

	/* Statistics may not be a buffer's memory any more than the pool. */
	stream_close(&s);
	CHECK(!stream_setup(&s) && !own_set(&s, 0, &o[0]), "stats in a buffer: stream");
	valid(cfg_slot(0));
	cfg_slot(0)->global.enables &= ~(uint32_t)PISP_FE_ENABLE_OUTPUT1;
	j = job_for(0, 0);
	j.range[1].addr = o[0].addr;
	j.range[1].len = STATS_SIZE;
	post(&s, &j);
	CHECK(!kick(&s), "the ring accepts the range in the job's own buffer");
	a = good_start(&s);
	ret = start(&a);
	CHECK(ret == -EACCES, "statistics in a buffer's own dma-buf is refused (got %d)", ret);
	stream_close(&s);

	/* Stopping with a frame in flight waits, and the pins end with the stream. */
	CHECK(!stream_setup(&s) && !own_set(&s, 0, &o[0]), "stop in flight: stream");
	simple(cfg_slot(0));
	j = job_for(0, 0);
	post(&s, &j);
	a = good_start(&s);
	CHECK(!kick(&s) && !start(&a), "started");
	fe_fire(TFE_SOF);
	fe_busy(40);
	t = now_ms();
	stream_close(&s);
	CHECK(now_ms() - t >= 30, "closing it with a frame in flight waits for the block (%llu ms)",
	      (unsigned long long)(now_ms() - t));
	fe_busy(0);
	CHECK(!fe_info().streaming, "and stops it");
	sleep_ms(5);
	CHECK(bufinfo_attached(dev) == before, "every pin released (%d left)",
	      bufinfo_attached(dev) - before);
	own_drop(&o[0]);
	own_drop(&o[1]);
	own_drop(&fresh);
	own_buffers = 0;
}

/* Stops with a frame in flight and the block busy a while: every path waits. */
static void test_stop_in_flight(const char *node, const char *unbind, const char *dev)
{
	struct stream s;
	struct lemnos_pisp_fe_start a;
	struct lemnos_stream_job j;
	uint64_t t;
	uint32_t ms = 60;

	/* STOP. */
	CHECK(!stream_setup(&s), "stop: stream");
	valid(cfg_slot(0));
	cfg_slot(0)->global.enables &= ~(uint32_t)PISP_FE_ENABLE_OUTPUT1;
	j = job_for(0, 0);
	post(&s, &j);
	CHECK(!kick(&s), "job");
	a = good_start(&s);
	CHECK(!start(&a), "started");
	fe_fire(TFE_SOF);	/* frame in flight */
	fe_busy(ms);
	t = now_ms();
	CHECK(!stop(), "STOP with a frame in flight");
	CHECK(now_ms() - t >= ms - 20, "STOP waited for the block (%llu ms)",
	      (unsigned long long)(now_ms() - t));
	stream_close(&s);

	/* Closing the stream's file. */
	CHECK(!stream_setup(&s), "close stream: stream");
	valid(cfg_slot(0));
	cfg_slot(0)->global.enables &= ~(uint32_t)PISP_FE_ENABLE_OUTPUT1;
	j = job_for(0, 0);
	post(&s, &j);
	CHECK(!kick(&s), "job");
	a = good_start(&s);
	CHECK(!start(&a), "started");
	fe_fire(TFE_SOF);
	fe_busy(ms);
	t = now_ms();
	close(s.fd);	/* s.r stays mapped; the stream ends on the last reference */
	munmap((void *)s.r, s.ring_bytes);
	CHECK(now_ms() - t >= ms - 20, "closing the stream waited (%llu ms)",
	      (unsigned long long)(now_ms() - t));
	CHECK(fe_info().streaming == 0, "not streaming");

	/* Closing the node. */
	CHECK(!stream_setup(&s), "close node: stream");
	valid(cfg_slot(0));
	cfg_slot(0)->global.enables &= ~(uint32_t)PISP_FE_ENABLE_OUTPUT1;
	j = job_for(0, 0);
	post(&s, &j);
	CHECK(!kick(&s), "job");
	a = good_start(&s);
	CHECK(!start(&a), "started");
	fe_fire(TFE_SOF);
	fe_busy(ms);
	t = now_ms();
	close(ctl);
	CHECK(now_ms() - t >= ms - 20, "closing the node waited (%llu ms)",
	      (unsigned long long)(now_ms() - t));
	ctl = open(node, O_RDWR | O_CLOEXEC);
	CHECK(ctl >= 0, "node reopened");
	stream_close(&s);

	/* Unbinding with a frame in flight. */
	CHECK(!stream_setup(&s), "unbind: stream");
	valid(cfg_slot(0));
	cfg_slot(0)->global.enables &= ~(uint32_t)PISP_FE_ENABLE_OUTPUT1;
	j = job_for(0, 0);
	post(&s, &j);
	CHECK(!kick(&s), "job");
	a = good_start(&s);
	CHECK(!start(&a), "started");
	fe_fire(TFE_SOF);
	fe_busy(ms);
	t = now_ms();
	{
		int fd = open(unbind, O_WRONLY | O_CLOEXEC);

		CHECK(fd >= 0 && write(fd, dev, strlen(dev)) == (ssize_t)strlen(dev),
		      "unbound with a frame in flight");
		if (fd >= 0)
			close(fd);
	}
	CHECK(now_ms() - t >= ms - 20, "unbinding waited (%llu ms)",
	      (unsigned long long)(now_ms() - t));
	CHECK(load(&s, LEMNOS_RING_STATE) == LEMNOS_RING_STATE_CLOSED,
	      "the stream ended closed");
	stream_close(&s);
	sleep_ms(5);
	CHECK(bufinfo_attached(dev) == attached_before, "every mapping released after the unbind");
}

static int open_all(const char *node, const char *heap)
{
	attached_before = bufinfo_attached(node + strlen("/dev/lemnos-rp1-csi2-"));
	ctl = open(node, O_RDWR | O_CLOEXEC);
	hw = open("/dev/lemnos-test-csi2", O_RDWR | O_CLOEXEC);
	pool_fd = make_buf(heap, (uint64_t)BUFFERS * pool_stride);
	cfg_r.fd = make_buf(heap, CFG_SLOTS * CFG_SIZE + 4096);
	stats_r.fd = make_buf(heap, (STATS_SIZE + 4095) & ~4095u);
	out_r.fd = make_buf(heap, 8192);
	if (ctl < 0 || hw < 0 || pool_fd < 0 || cfg_r.fd < 0 || stats_r.fd < 0 || out_r.fd < 0)
		return -1;
	cfg_r.map = mmap(NULL, CFG_SLOTS * CFG_SIZE + 4096, PROT_READ | PROT_WRITE, MAP_SHARED,
			 cfg_r.fd, 0);
	stats_map_bytes = (STATS_SIZE + 4095) & ~4095u;
	stats_r.map = mmap(NULL, stats_map_bytes, PROT_READ | PROT_WRITE, MAP_SHARED, stats_r.fd, 0);
	out_r.map = mmap(NULL, out_map_bytes, PROT_READ | PROT_WRITE, MAP_SHARED, out_r.fd, 0);
	pool_cpu = mmap(NULL, BUFFERS * pool_stride, PROT_READ | PROT_WRITE, MAP_SHARED, pool_fd, 0);
	return cfg_r.map == MAP_FAILED || stats_r.map == MAP_FAILED || out_r.map == MAP_FAILED ||
	       pool_cpu == MAP_FAILED ? -1 : 0;
}

/* DMA that never stops: buffers stay pinned for good and START is refused. */
static int run_wedge(const char *node, const char *unbind, const char *dev, const char *heap)
{
	struct stream s;
	struct lemnos_pisp_fe_start a;
	struct lemnos_stream_job j;
	int ret, fd;

	CHECK(!open_all(node, heap), "wedge: set up");
	CHECK(!stream_setup(&s), "wedge: stream");
	valid(cfg_slot(0));
	cfg_slot(0)->global.enables &= ~(uint32_t)PISP_FE_ENABLE_OUTPUT1;
	j = job_for(0, 0);
	post(&s, &j);
	CHECK(!kick(&s), "job");
	a = good_start(&s);
	CHECK(!start(&a), "started");
	fe_fire(TFE_SOF);
	fe_busy(UINT32_MAX);
	ret = stop();
	CHECK(!ret, "STOP returns even when the block never goes idle (%d)", ret);
	fe_busy(0);
	CHECK(fe_info().flags & LEMNOS_PISP_FE_INFO_WEDGED, "wedged is reported");
	{
		struct lemnos_stream_unregister u = { .region = (uint32_t)cfg_r.handle };

		ret = ioctl(s.fd, LEMNOS_STREAM_IOC_UNREGISTER, &u) ? -errno : 0;
		CHECK(ret == -EBUSY, "the job's buffers stay held (%d)", ret);
	}
	a = good_start(&s);
	ret = start(&a);
	CHECK(ret == -EIO, "START refused while wedged (%d)", ret);
	/* Every buffer the DMA could reach stays pinned, even with nobody holding it. */
	stream_close(&s);
	munmap(cfg_r.map, CFG_SLOTS * CFG_SIZE + 4096);
	munmap(stats_r.map, stats_map_bytes);
	munmap(out_r.map, out_map_bytes);
	munmap(pool_cpu, BUFFERS * pool_stride);
	close(cfg_r.fd);
	close(stats_r.fd);
	close(out_r.fd);
	close(pool_fd);
	sleep_ms(5);
	ret = bufinfo_attached(dev);
	CHECK(ret == attached_before + 4,
	      "pool, configuration, statistics and output 1 stay attached (%d more)",
	      ret - attached_before);
	fd = open(unbind, O_WRONLY | O_CLOEXEC);
	CHECK(fd >= 0 && write(fd, dev, strlen(dev)) == (ssize_t)strlen(dev), "unbound while wedged");
	if (fd >= 0)
		close(fd);
	printf("lemnos_pisp_fe_check wedge: %d failures\n", failures);
	return failures ? 1 : 0;
}

int main(int argc, char **argv)
{
	char node[128];
	const char *dev, *unbind, *heap;

	setvbuf(stdout, NULL, _IONBF, 0);
	if (argc == 5 && !strcmp(argv[1], "wedge")) {
		snprintf(node, sizeof(node), "/dev/lemnos-rp1-csi2-%s", argv[2]);
		return run_wedge(node, argv[3], argv[2], argv[4]);
	}
	if (argc != 5)
		return 2;
	dev = argv[2];
	unbind = argv[3];
	heap = argv[4];
	snprintf(node, sizeof(node), "/dev/lemnos-rp1-csi2-%s", dev);
	CHECK(!open_all(node, heap), "set up");
	test_validator();
	test_ranges();
	test_allowed_variants();
	test_start_format();
	test_good_and_addresses();
	test_library_config();
	test_exclusive();
	test_sequence();
	test_toctou();
	test_hostile_hw();
	test_own_buffers(heap, dev);
	test_stop_in_flight(node, unbind, dev);
	printf("lemnos_pisp_fe_check: %d failures\n", failures);
	return failures ? 1 : 0;
}
