// SPDX-License-Identifier: GPL-2.0
/*
 * Frame streams: the module is the producer of a stream, on the ring the
 * socket protocol's streams already use (lemnos.h; docs/wire.md). It publishes
 * frames, wakes the consumer, collects returned buffers and jobs, and checks
 * every address a job names against dma-bufs it has mapped and pinned.
 *
 * Everything the consumer can touch is hostile: the ring is mapped into its
 * process, so any word can change at any moment. Each value is read once into
 * a kernel local with READ_ONCE, checked, and only the local is used. Every
 * bound and capacity below is the module's own copy, made at creation. A
 * buffer's ownership is only ever in kernel memory (struct lem_stream_buf),
 * so the consumer can't claim a state it doesn't have. Any broken rule ends
 * the stream with an errno and state 2: its own stream, nothing else, and
 * nothing is reachable that warns or loops on what the consumer wrote.
 *
 * A stream is jobless until the first job is accepted: frames take free
 * buffers. From then on it is job-driven for good: a frame needs a queued job,
 * and nothing else takes a buffer, so a consumer can return a buffer and post
 * a job on it without losing it in between. The window before that first job
 * is the consumer's to close, by posting it before frames start (lemnos.h).
 *
 * Only a block's own memory is ever named: the pool and dma-bufs registered
 * here are pinned while the stream holds them, and removed from a stream only
 * when no job uses them.
 *
 * A stream made with LEMNOS_STREAM_CREATE_BUFFERS has no pool: buffer n's
 * memory is a dma-buf of its own (s->own[n]), given and swapped with
 * LEMNOS_STREAM_IOC_BUFFER, and pinned and mapped exactly as a pool is. A
 * buffer without memory is LEM_BUF_ABSENT, a state nothing takes a buffer
 * from, and a buffer's memory changes only while the buffer is free or with
 * the consumer, under s->lock: never while a job names it or the source has
 * it. lem_stream_buffer_addr() is the one place a buffer's device address is
 * computed, for either kind of stream.
 *
 * Locking: s->lock is the frame path's, taken by the source (a block's
 * interrupt handler, or the test source) and by KICK. s->ctl orders
 * registration and teardown and is never taken on the frame path. The source
 * keeps no lock while it writes memory; instead teardown stops it (the block's
 * stop callback, cancel_delayed_work_sync) before releasing anything, and the
 * regions a job names stay held (users) until the job finishes. Lock order:
 * lem_blocks_sem, lem_streams_lock, s->ctl, s->lock.
 *
 * A block attaches as the stream's producer and calls lem_stream_next() for
 * the buffer it arms and lem_stream_complete() when the block is finished with
 * it (lemnos_internal.h). Everything those touch is the kernel's own copy; the
 * block only ever sees device addresses this file computed from the pool, or
 * from the buffer's own dma-buf.
 *
 * Derived in part from the capture module's buffer queue on the dev branch
 * (a queued buffer names a dma-buf offset; the module keeps the mapping while
 * the buffer is in use). The ring protocol and everything checking it are new.
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <linux/anon_inodes.h>
#include <linux/device.h>
#include <linux/dma-buf.h>
#include <linux/dma-direction.h>
#include <linux/err.h>
#include <linux/eventfd.h>
#include <linux/fcntl.h>
#include <linux/file.h>
#include <linux/fs.h>
#include <linux/iosys-map.h>
#include <linux/jiffies.h>
#include <linux/ktime.h>
#include <linux/list.h>
#include <linux/log2.h>
#include <linux/math64.h>
#include <linux/mm.h>
#include <linux/module.h>
#include <linux/mutex.h>
#include <linux/overflow.h>
#include <linux/poll.h>
#include <linux/scatterlist.h>
#include <linux/slab.h>
#include <linux/spinlock.h>
#include <linux/uaccess.h>
#include <linux/file.h>
#include <linux/mm.h>
#include <linux/shmem_fs.h>
#include <linux/vmalloc.h>
#include <linux/wait.h>
#include <linux/workqueue.h>
#include <uapi/linux/fcntl.h>

#include "lemnos_internal.h"

/* Each stream is kernel memory, so their number has an end. */
#define LEM_STREAM_MAX 64
/*
 * The pool, then the dma-bufs the consumer registers. A range's region at or
 * past LEM_STREAM_REGIONS is a buffer's own dma-buf: s->own[region - this].
 */
#define LEM_STREAM_REGIONS 16
#define LEM_STREAM_POOL 0

/* Words a job occupies: its bytes, and nothing about what it means. */
#define LEM_JOB_WORDS (sizeof(struct lemnos_stream_job) / 8)

static bool test_stream_source;
module_param(test_stream_source, bool, 0444);
MODULE_PARM_DESC(test_stream_source,
		 "Test only: LEMNOS_STREAM_IOC_TEST_SOURCE produces frames with nothing behind them");

enum lem_buf_state {
	LEM_BUF_FREE,		/* the kernel has it, nobody asked for it */
	LEM_BUF_QUEUED,		/* a job names it */
	LEM_BUF_ACTIVE,		/* the source is filling it */
	LEM_BUF_CONSUMER,	/* published, due back with its fence */
	LEM_BUF_ABSENT,		/* no memory yet: only on a stream without a pool */
};

struct lem_stream_buf {
	u32 fence;		/* its current generation */
	u8 state;		/* enum lem_buf_state */
};

struct lem_stream_region {
	struct dma_buf *buf;		/* NULL: empty */
	struct dma_buf_attachment *att;
	struct sg_table *sgt;
	struct iosys_map map;		/* only with test_stream_source */
	u64 addr, end;			/* device addresses, addr < end */
	u64 size;
	u32 users;			/* job ranges not finished */
};

/*
 * A reader's marks page, pinned for the stream's life: the reader's own memfd,
 * mapped read-only here. Its words are shm/ring-holds' layout (marks.rs): the
 * version, then one mark per buffer, nonzero while the reader holds it.
 */
struct lem_reader {
	struct page **pages;
	unsigned int npages;
	u64 *marks;			/* vmap of pages, read-only; NULL: no reader in this slot */
	u32 max_held;			/* what the reader may hold at once; enforced by the cut-off */
};

struct lem_stream {
	struct list_head node;		/* on lem_streams */
	struct device *dev;		/* held until the stream is freed */
	struct mutex ctl;		/* regions, the source, teardown */
	spinlock_t lock;		/* everything from here down */
	wait_queue_head_t wait;
	struct eventfd_ctx *doorbell;
	u64 *ring;			/* vmalloc_user: mapped by the consumer */
	size_t ring_bytes;

	/* The module's copies. The ring's header is for the consumer only. */
	u32 buffers, buffer_bytes, frame_slots, job_slots, job_bytes;
	size_t returns_at, jobs_at, job_words;
	u64 stride;

	struct lem_stream_buf *buf;
	struct lem_job *queue;		/* job_slots validated jobs */
	u64 queue_head, queue_tail;
	struct lem_stream_region region[LEM_STREAM_REGIONS];
	struct lem_stream_region *own;	/* without a pool: each buffer's dma-buf; NULL with one */
	u32 cursor;			/* where the next jobless frame looks */

	/* The producer's own counts, never read back from the ring. */
	u64 frames_head, frames_seen, returns_tail, jobs_tail, sequence;
	u64 published, dropped, returned, jobs;

	int fault;			/* errno that ended it; the ring is left alone after */
	bool dead;			/* over: the block went or the file closed */

	struct delayed_work source;	/* test source */
	u32 period_us, left;
	struct work_struct gone;	/* the block going away, see lem_streams_block_gone */

	bool cpu_read;			/* registered regions keep a kernel mapping */
	struct lem_stream_producer producer;	/* the block feeding this stream, or stop NULL */
	u32 frame_bytes;		/* what the producer may write per buffer */
	bool wedged;			/* a block's DMA could not be seen to stop: nothing is unpinned */

	/* Where the control words are: the LEMNOS_RING_* ones, or the shm form's. */
	bool shm;			/* LEMNOS_STREAM_CREATE_SHM */
	size_t shm_total;		/* words of the shm ring */
	size_t shm_slots_at;		/* its slots */
	size_t page_at;			/* the pool page's first word (shm form) */
	size_t w_state, w_returns_head, w_returns_tail, w_jobs_head, w_jobs_tail;
	struct lem_reader reader[LEMNOS_STREAM_MAX_READERS];	/* shm form: the consumers' marks */
	u32 newest;			/* shm form: the buffer of the newest frame, kept for the frame path */
};

static const struct file_operations lem_stream_fops;
static int lem_region_vmap(struct lem_stream_region *r);

static LIST_HEAD(lem_streams);
static DEFINE_MUTEX(lem_streams_lock);
static u32 lem_stream_count;

/* --- the ring, as seen from here --- */

static u64 lem_ring_read(const struct lem_stream *s, size_t at)
{
	return READ_ONCE(s->ring[at]);
}

static u64 lem_ring_read_acquire(const struct lem_stream *s, size_t at)
{
	u64 v = READ_ONCE(s->ring[at]);

	smp_rmb();
	return v;
}

static void lem_ring_write(const struct lem_stream *s, size_t at, u64 v)
{
	WRITE_ONCE(s->ring[at], v);
}

/*
 * Publishes a head, a tail or the state. A full barrier, not smp_wmb: moving a
 * tail releases a slot this CPU has just read, so the earlier loads must be
 * ordered too, or the consumer can reuse the slot under the read. Not
 * smp_store_release, which doesn't build for a u64 on a 32-bit kernel.
 */
static void lem_ring_write_release(const struct lem_stream *s, size_t at, u64 v)
{
	smp_mb();
	WRITE_ONCE(s->ring[at], v);
}

/* The consumer sees an end once. It may scribble on this word; the kernel's copy is what counts. */
static void lem_stream_fault_locked(struct lem_stream *s, int err)
{
	if (s->fault)
		return;
	s->fault = err;
	lem_ring_write_release(s, s->w_state, LEMNOS_RING_STATE_FAULT);
}

/* Wakes whoever is waiting. Never under s->lock: the eventfd has its own. */
static void lem_stream_wake(struct lem_stream *s)
{
	wake_up_interruptible(&s->wait);
	if (s->doorbell)
		eventfd_signal(s->doorbell);
}

/*
 * The shm form (LEMNOS_STREAM_CREATE_SHM): shm/README.md's ring (layout 1),
 * one field, one sample per frame, and the pool page of
 * shm/ring-buffers/src/page.rs after it. Every number here is what the Rust
 * crates say (testing/crates/shm-ring-check reads what this writes, and its
 * own tests pin the derived shape): the word numbers, the slot size, the
 * entry, the stamps, and the pool page's ticket words.
 */
#define LEM_SHM_MAGIC		0x31474e49524d4853ull	/* "SHMRING1" */
#define LEM_SHM_LAYOUT		1ull
#define LEM_SHM_W_MAGIC		0
#define LEM_SHM_W_LAYOUT	1
#define LEM_SHM_W_SHAPE		2
#define LEM_SHM_W_SIZES		3
#define LEM_SHM_W_TOTAL		4
#define LEM_SHM_W_HEAD		8
#define LEM_SHM_W_END		10
#define LEM_SHM_LANE_HEAD	16	/* the one field's lane head */
#define LEM_SHM_INDEX		17	/* frame_slots index words */
#define LEM_SHM_SLOT_HEADER	4	/* stamp, time, meta, lane sequence */
#define LEM_SHM_ENTRY_BYTES	40	/* generation, count, buffer, ticket, sequence, identity, bytes, fence */
#define LEM_SHM_SLOT_WORDS	(LEM_SHM_SLOT_HEADER + LEM_SHM_ENTRY_BYTES / 8)
#define LEM_POOL_MAGIC		0x314c4f4f504d4853ull	/* "SHMPOOL1" */
#define LEM_POOL_TICKETS	8	/* one word per buffer, from here */
#define LEM_POOL_READERS	64	/* shm-ring-buffers' MAX_READERS: cut-off words after the tickets */
#define LEM_MARKS_AT		1	/* a marks page's first mark: word 0 is its version (marks.rs) */

static u64 lem_shm_stamp(u64 seq)
{
	return (seq + 1) << 1;
}

static u64 lem_shm_busy(u64 seq)
{
	return lem_shm_stamp(seq) | 1;
}

/*
 * Publishes frame `sample` (the frame ring's head before it) as the next sample
 * of the one field. The order is the one the readers rely on: busy stamp,
 * fence, time, meta, lane sequence, payload, finished stamp, index entry, lane
 * head, head. Lock held; the ring can't be full: a reader that falls behind
 * skips ahead, it doesn't hold the writer back.
 */
static void lem_shm_publish(struct lem_stream *s, u32 buffer, u32 bytes, u64 sequence,
			    u64 identity, u32 fence, u64 timestamp_ns)
{
	u64 sample = s->frames_head;
	u64 lane_head = lem_ring_read(s, LEM_SHM_LANE_HEAD);
	size_t lane_slot = lane_head & (s->frame_slots - 1);
	size_t at = s->shm_slots_at + lane_slot * LEM_SHM_SLOT_WORDS;
	u64 ticket = lem_ring_read(s, s->page_at + LEM_POOL_TICKETS + buffer);
	/* The entry: one buffer, its even ticket, the frame's sequence, identity, bytes and fence. */
	u64 entry[LEM_SHM_ENTRY_BYTES / 8] = {
		1ull | 1ull << 32,		/* generation 1, one buffer */
		buffer | (ticket & 0xffffffffull) << 32,
		sequence,
		identity,
		bytes | (u64)fence << 32,
	};

	lem_ring_write(s, at, lem_shm_busy(sample));
	smp_wmb();
	lem_ring_write(s, at + 1, timestamp_ns);
	lem_ring_write(s, at + 2, LEM_SHM_ENTRY_BYTES);	/* field 0, flags 0 */
	lem_ring_write(s, at + 3, lane_head);
	for (size_t i = 0; i < ARRAY_SIZE(entry); i++)
		lem_ring_write(s, at + LEM_SHM_SLOT_HEADER + i, entry[i]);
	lem_ring_write_release(s, at, lem_shm_stamp(sample));
	lem_ring_write(s, LEM_SHM_INDEX + (sample & (s->frame_slots - 1)), lane_slot << 16);
	lem_ring_write_release(s, LEM_SHM_LANE_HEAD, lane_head + 1);
	s->frames_head++;
	lem_ring_write_release(s, LEM_SHM_W_HEAD, s->frames_head);
}

/* Whether any reader's marks hold `buffer`. Lock held; `buffer` is below s->buffers. */
static bool lem_stream_held(const struct lem_stream *s, u32 buffer)
{
	for (u32 r = 0; r < LEMNOS_STREAM_MAX_READERS; r++) {
		const u64 *marks = s->reader[r].marks;

		if (marks && READ_ONCE(marks[LEM_MARKS_AT + buffer]))
			return true;
	}
	return false;
}

/*
 * Takes a published buffer back if no reader holds it (shm/ring-buffers'
 * writer: retire, check the holders, release). Retiring makes the ticket odd,
 * so no reader can newly take the frame; the full barrier orders that before
 * the marks are read, and pairs with the reader's fence. A held buffer stays
 * published. Lock held. True if the buffer is free now.
 */
static bool lem_stream_reclaim(struct lem_stream *s, u32 buffer)
{
	struct lem_stream_buf *b = &s->buf[buffer];
	size_t at = s->page_at + LEM_POOL_TICKETS + buffer;
	u64 ticket;

	if (b->state != LEM_BUF_CONSUMER)
		return b->state == LEM_BUF_FREE;
	ticket = lem_ring_read(s, at);
	if (!(ticket & 1))
		lem_ring_write(s, at, ticket | 1);
	smp_mb();
	if (lem_stream_held(s, buffer))
		return false;
	/* Odd to the next even value: release. */
	lem_ring_write_release(s, at, (lem_ring_read(s, at) | 1) + 1);
	b->state = LEM_BUF_FREE;
	s->returned++;
	return true;
}

/* Takes back buffers the consumer returned. Lock held; bounded by the kernel's capacity. */
static int lem_stream_collect_returns(struct lem_stream *s)
{
	u32 slots = s->frame_slots;

	/* The shm form has no returns queue: a buffer goes back when no reader holds it. */
	if (s->shm)
		return 0;
	for (u32 n = 0; n < slots; n++) {
		u64 head = lem_ring_read_acquire(s, s->w_returns_head);
		u64 count = head - s->returns_tail;
		u64 word;
		u32 buffer, fence;
		struct lem_stream_buf *b;

		if (!count)
			return 0;
		if (count > slots)
			return -ERANGE;
		word = lem_ring_read(s, s->returns_at + (s->returns_tail & (slots - 1)));
		buffer = lower_32_bits(word);
		fence = upper_32_bits(word);
		if (buffer >= s->buffers)
			return -EINVAL;
		b = &s->buf[buffer];
		if (b->state != LEM_BUF_CONSUMER || b->fence != fence)
			return -EPERM;
		b->state = LEM_BUF_FREE;
		s->returned++;
		s->returns_tail++;
		lem_ring_write_release(s, s->w_returns_tail, s->returns_tail);
	}
	return 0;
}

/* The region a resolved range names: the table, or a buffer's own dma-buf. Lock held. */
static struct lem_stream_region *lem_stream_region_at(struct lem_stream *s, u32 region)
{
	if (region < LEM_STREAM_REGIONS)
		return &s->region[region];
	return &s->own[region - LEM_STREAM_REGIONS];
}

/*
 * Where `buffer` is for the block, and that `bytes` from there are inside
 * memory this stream has pinned: its slice of the pool, or its own dma-buf.
 * The only place a buffer's address comes from. Lock held, or nothing running.
 */
static int lem_stream_buffer_addr(const struct lem_stream *s, u32 buffer, u64 bytes, u64 *addr)
{
	const struct lem_stream_region *reg = &s->region[LEM_STREAM_POOL];
	u64 offset = 0, end;

	if (buffer >= s->buffers)
		return -EINVAL;
	if (s->own)
		reg = &s->own[buffer];
	else if (check_mul_overflow((u64)buffer, s->stride, &offset))
		return -EFAULT;
	if (!reg->buf || check_add_overflow(reg->addr, offset, addr) ||
	    check_add_overflow(*addr, bytes, &end) || end > reg->end)
		return -EFAULT;
	return 0;
}

/*
 * Checks one range and resolves it to a region and offset. Touches nothing.
 * `addr` and `len` are kernel locals: the job was copied before this.
 */
static int lem_stream_resolve(const struct lem_stream *s, u32 buffer, u64 addr, u64 len,
			      struct lem_job_range *out)
{
	const struct lem_stream_region *pool = &s->region[LEM_STREAM_POOL];
	u64 end, offset, index, in;

	if (!len)
		return -EINVAL;
	if (check_add_overflow(addr, len, &end))
		return -EOVERFLOW;

	/* Without a pool: inside one buffer's memory, which must be the job's own. */
	for (u32 b = 0; s->own && b < s->buffers; b++) {
		const struct lem_stream_region *reg = &s->own[b];

		if (!reg->buf || addr < reg->addr || end > reg->end)
			continue;
		if (b != buffer)
			return -EACCES;
		out->region = LEM_STREAM_REGIONS + b;
		out->offset = addr - reg->addr;
		out->len = len;
		return 0;
	}
	if (pool->buf && addr >= pool->addr && end <= pool->end) {
		offset = addr - pool->addr;
		index = div64_u64(offset, s->stride);
		in = offset - index * s->stride;
		if (index != buffer)
			return -EACCES;
		if (in + len > s->buffer_bytes)
			return -EFAULT;
		out->region = LEM_STREAM_POOL;
		out->offset = offset;
		out->len = len;
		return 0;
	}
	for (u32 r = LEM_STREAM_POOL + 1; r < LEM_STREAM_REGIONS; r++) {
		const struct lem_stream_region *reg = &s->region[r];

		if (reg->buf && addr >= reg->addr && end <= reg->end) {
			out->region = r;
			out->offset = addr - reg->addr;
			out->len = len;
			return 0;
		}
	}
	return -EFAULT;
}

/* Checks a job and, only when all of it is good, holds what it names and queues it. */
static int lem_stream_accept(struct lem_stream *s, const struct lemnos_stream_job *in)
{
	struct lem_job job = {};
	struct lem_stream_buf *b;
	u32 count = in->count;
	int ret;

	if (in->buffer >= s->buffers)
		return -EINVAL;
	/* A consumer that let go of the buffer's marks gets it back to post a job on it. */
	if (s->shm)
		lem_stream_reclaim(s, in->buffer);
	b = &s->buf[in->buffer];
	if (b->state == LEM_BUF_ABSENT)
		return -EFAULT;
	if (b->state == LEM_BUF_QUEUED || b->state == LEM_BUF_ACTIVE)
		return -EBUSY;
	if (b->state != LEM_BUF_FREE)
		return -EACCES;
	if (count > LEMNOS_STREAM_JOB_RANGES)
		return -EINVAL;
	for (u32 i = count; i < LEMNOS_STREAM_JOB_RANGES; i++)
		if (in->range[i].addr || in->range[i].len)
			return -EINVAL;

	job.identity = in->identity;
	job.buffer = in->buffer;
	job.count = count;
	for (u32 i = 0; i < count; i++) {
		ret = lem_stream_resolve(s, in->buffer, in->range[i].addr, in->range[i].len,
					 &job.range[i]);
		if (ret)
			return ret;
	}
	for (u32 i = 0; i < count; i++)
		lem_stream_region_at(s, job.range[i].region)->users++;
	b->state = LEM_BUF_QUEUED;
	s->queue[s->queue_head & (s->job_slots - 1)] = job;
	s->queue_head++;
	s->jobs++;
	return 0;
}

/* Copies each posted job once to a local and accepts it. Lock held. */
static int lem_stream_collect_jobs(struct lem_stream *s)
{
	u32 slots = s->job_slots;

	for (u32 n = 0; n < slots; n++) {
		u64 head = lem_ring_read_acquire(s, s->w_jobs_head);
		u64 count = head - s->jobs_tail;
		u64 words[LEM_JOB_WORDS];
		size_t at;
		u64 len;
		int ret;

		if (!count)
			return 0;
		if (count > slots)
			return -ERANGE;
		/* Full here: wait for one to finish rather than refuse it. */
		if (s->queue_head - s->queue_tail == slots)
			return 0;
		/*
		 * A consumer returns a buffer and then posts a job on it, and
		 * the head just read may be newer than the look at the returns
		 * before this loop. Each return moves a buffer out of
		 * LEM_BUF_CONSUMER, so this stays bounded by the buffers.
		 */
		ret = lem_stream_collect_returns(s);
		if (ret)
			return ret;

		at = s->jobs_at + (s->jobs_tail & (slots - 1)) * s->job_words;
		len = lem_ring_read(s, at);
		if (len != sizeof(struct lemnos_stream_job))
			return -EMSGSIZE;
		for (u32 i = 0; i < LEM_JOB_WORDS; i++)
			words[i] = lem_ring_read(s, at + 1 + i);
		ret = lem_stream_accept(s, (const struct lemnos_stream_job *)words);
		if (ret)
			return ret;
		s->jobs_tail++;
		lem_ring_write_release(s, s->w_jobs_tail, s->jobs_tail);
	}
	return 0;
}

/* Everything the consumer has posted, bounded. A refusal ends the stream. Lock held. */
static int lem_stream_drain(struct lem_stream *s)
{
	int ret = lem_stream_collect_returns(s);

	if (!ret)
		ret = lem_stream_collect_jobs(s);
	if (ret)
		lem_stream_fault_locked(s, ret);
	return ret;
}

/* Publishes one frame. 0, with *pushed saying if it fit; or -errno for a lying consumer. */
static int lem_stream_push_frame(struct lem_stream *s, u32 buffer, u32 bytes, u64 sequence,
				 u64 identity, u32 fence, u64 timestamp_ns, bool *pushed)
{
	u64 slots = s->frame_slots;
	u64 tail = lem_ring_read_acquire(s, LEMNOS_RING_FRAMES_TAIL);
	size_t at;

	*pushed = false;
	if (s->shm) {
		lem_shm_publish(s, buffer, bytes, sequence, identity, fence, timestamp_ns);
		*pushed = true;
		return 0;
	}
	if (tail - s->frames_seen > s->frames_head - s->frames_seen ||
	    s->frames_head - tail > slots)
		return -ERANGE;
	s->frames_seen = tail;
	if (s->frames_head - tail == slots)
		return 0;
	at = LEMNOS_RING_FRAMES + (s->frames_head & (slots - 1)) * LEMNOS_STREAM_FRAME_WORDS;
	lem_ring_write(s, at, (u64)buffer | (u64)bytes << 32);
	lem_ring_write(s, at + 1, sequence);
	lem_ring_write(s, at + 2, identity);
	lem_ring_write(s, at + 3, timestamp_ns);
	lem_ring_write(s, at + 4, fence);
	s->frames_head++;
	lem_ring_write_release(s, LEMNOS_RING_FRAMES_HEAD, s->frames_head);
	*pushed = true;
	return 0;
}

/*
 * The next job. With none queued, a stream that has never had a job takes the
 * next free buffer; one that has is job-driven and takes nothing, or a frame
 * could take the buffer its consumer just returned to post a job on. Lock held.
 */
static bool lem_stream_take(struct lem_stream *s, struct lem_job *job)
{
	if (s->queue_tail != s->queue_head) {
		*job = s->queue[s->queue_tail & (s->job_slots - 1)];
		s->queue_tail++;
		s->buf[job->buffer].state = LEM_BUF_ACTIVE;
		return true;
	}
	if (s->jobs)
		return false;
	for (u32 i = 0; i < s->buffers; i++) {
		u32 b = (s->cursor + i) % s->buffers;

		if (s->buf[b].state == LEM_BUF_FREE) {
			s->cursor = (b + 1) % s->buffers;
			s->buf[b].state = LEM_BUF_ACTIVE;
			memset(job, 0, sizeof(*job));
			job->buffer = b;
			return true;
		}
	}
	/* Shm: take back one no reader holds, but never the newest frame's, so a reader always has one. */
	if (s->shm) {
		for (u32 i = 0; i < s->buffers; i++) {
			u32 b = (s->cursor + i) % s->buffers;

			if (b == s->newest || s->buf[b].state != LEM_BUF_CONSUMER ||
			    !lem_stream_reclaim(s, b))
				continue;
			s->cursor = (b + 1) % s->buffers;
			s->buf[b].state = LEM_BUF_ACTIVE;
			memset(job, 0, sizeof(*job));
			job->buffer = b;
			return true;
		}
	}
	return false;
}

/*
 * What a block would do: writes the frame, and each range a job named. Only
 * kernel values reach this. No lock: s->lock must not be held across a large
 * copy, and the job's regions are held (users) and live until it completes;
 * teardown cancels this work and waits for it before releasing anything.
 */
static void lem_stream_fill(struct lem_stream *s, const struct lem_job *job, u64 sequence)
{
	u8 *p = s->own ? s->own[job->buffer].map.vaddr :
			 (u8 *)s->region[LEM_STREAM_POOL].map.vaddr + (u64)job->buffer * s->stride;
	u64 *w = (u64 *)p;
	size_t bytes = s->buffer_bytes;

	for (size_t i = 0; i < bytes / sizeof(u64); i++)
		w[i] = sequence;
	if (bytes % sizeof(u64))
		memset(p + (bytes & ~(sizeof(u64) - 1)), 0, bytes % sizeof(u64));
	for (u32 i = 0; i < job->count; i++) {
		const struct lem_job_range *r = &job->range[i];

		memset((u8 *)lem_stream_region_at(s, r->region)->map.vaddr + r->offset, 0xa5 ^ i,
		       r->len);
	}
}

/*
 * Publishes a filled buffer, or gives it back when the stream has ended or the
 * ring has no room. `bytes` is the kernel's own figure; 0 says the block
 * flagged the frame as bad. Lock held; bounded.
 */
static void lem_stream_finish_locked(struct lem_stream *s, const struct lem_job *job, u32 bytes,
				     u64 sequence, u64 timestamp_ns)
{
	struct lem_stream_buf *b = &s->buf[job->buffer];
	bool pushed;
	int ret;

	for (u32 i = 0; i < job->count; i++)
		lem_stream_region_at(s, job->range[i].region)->users--;
	if (s->dead || s->fault || (!bytes && !s->jobs)) {
		/*
		 * With no job behind it nobody is waiting for this buffer. A
		 * job-driven stream's consumer is told, so it isn't left
		 * waiting on an identity that never comes.
		 */
		b->state = LEM_BUF_FREE;
		return;
	}

	b->fence++;
	ret = lem_stream_push_frame(s, job->buffer, bytes, sequence, job->identity, b->fence,
				    timestamp_ns, &pushed);
	if (ret) {
		b->state = LEM_BUF_FREE;
		lem_stream_fault_locked(s, ret);
	} else if (pushed) {
		b->state = LEM_BUF_CONSUMER;
		s->newest = job->buffer;
		s->published++;
	} else {
		b->state = LEM_BUF_FREE;
		s->dropped++;
	}
}

static void lem_stream_source(struct work_struct *work)
{
	struct lem_stream *s = container_of(to_delayed_work(work), struct lem_stream, source);
	struct lem_job job;
	unsigned long flags;
	u64 sequence = 0;
	bool have = false, again;

	spin_lock_irqsave(&s->lock, flags);
	if (s->dead || s->fault || lem_stream_drain(s)) {
		spin_unlock_irqrestore(&s->lock, flags);
		lem_stream_wake(s);
		return;
	}
	have = lem_stream_take(s, &job);
	if (have)
		sequence = s->sequence++;
	spin_unlock_irqrestore(&s->lock, flags);

	if (have)
		lem_stream_fill(s, &job, sequence);

	spin_lock_irqsave(&s->lock, flags);
	if (have)
		lem_stream_finish_locked(s, &job, s->buffer_bytes, sequence, ktime_get_ns());
	if (have && s->left != U32_MAX && s->left)
		s->left--;
	again = !s->dead && !s->fault && s->left;
	spin_unlock_irqrestore(&s->lock, flags);

	lem_stream_wake(s);
	if (again) {
		unsigned long delay = usecs_to_jiffies(READ_ONCE(s->period_us));

		/* Nothing to fill: don't spin until the consumer gives one back. */
		schedule_delayed_work(&s->source, have ? delay : max(delay, 1UL));
	}
}

/* --- a block as the producer --- */

/*
 * The checks every programmed frame must pass, against the module's own copies:
 * the frame's end, in device addresses, must lie inside the buffer. A frame is
 * bytes here: what lines or planes fill them is the producer's to check. Lock held
 * or the values not yet in use. Without a pool each buffer was held to
 * buffer_bytes and the alignment when it was given its memory, and is again
 * each time it is handed out.
 */
static int lem_stream_check_frame(const struct lem_stream *s, u32 bytes)
{
	const struct lem_stream_region *pool = &s->region[LEM_STREAM_POOL];
	u64 last, end;

	if (!bytes)
		return -EINVAL;
	if (bytes > s->buffer_bytes || (u64)bytes > s->stride)
		return -EINVAL;
	if (s->own)
		return 0;
	if (!pool->buf || (pool->addr & (LEM_STREAM_DMA_ALIGN - 1)))
		return -EINVAL;
	if (check_mul_overflow((u64)s->buffers - 1, s->stride, &last) ||
	    check_add_overflow(pool->addr, last, &last) ||
	    check_add_overflow(last, (u64)bytes, &end) || end > pool->end)
		return -EFAULT;
	return 0;
}

struct lem_stream *lem_stream_from_file(struct file *file, struct device *dev)
{
	struct lem_stream *s;

	if (file->f_op != &lem_stream_fops)
		return ERR_PTR(-EINVAL);
	s = file->private_data;
	if (s->dev != dev)
		return ERR_PTR(-EXDEV);
	return s;
}

int lem_stream_attach(struct lem_stream *s, const struct lem_stream_producer *producer, u32 bytes)
{
	unsigned long flags;
	int ret;

	spin_lock_irqsave(&s->lock, flags);
	if (s->dead)
		ret = -ENODEV;
	else if (s->fault)
		ret = s->fault;
	else if (s->producer.stop || s->left)
		ret = -EBUSY;
	else if (producer->job_bytes && producer->job_bytes != s->job_bytes)
		ret = -EINVAL;
	else
		ret = lem_stream_check_frame(s, bytes);
	if (!ret) {
		s->producer = *producer;
		s->frame_bytes = bytes;
	}
	spin_unlock_irqrestore(&s->lock, flags);
	return ret;
}

bool lem_stream_detach(struct lem_stream *s)
{
	unsigned long flags;
	bool was;

	spin_lock_irqsave(&s->lock, flags);
	was = s->producer.stop;
	s->producer.stop = NULL;
	spin_unlock_irqrestore(&s->lock, flags);
	return was;
}

int lem_stream_next(struct lem_stream *s, struct lem_stream_slot *slot)
{
	unsigned long flags;
	int ret;

	spin_lock_irqsave(&s->lock, flags);
	if (s->dead) {
		ret = -ENODEV;
	} else if (s->fault) {
		ret = s->fault;
	} else {
		ret = lem_stream_drain(s);
		if (!ret && !lem_stream_take(s, &slot->job))
			ret = -ENOBUFS;
		if (!ret) {
			u64 addr;

			if (lem_stream_buffer_addr(s, slot->job.buffer, s->frame_bytes, &addr)) {
				/* The bounds were checked when the memory came; never hand this out. */
				lem_stream_finish_locked(s, &slot->job, 0, 0, 0);
				ret = -EFAULT;
			} else {
				slot->addr = addr;
				slot->bytes = s->frame_bytes;
			}
		}
	}
	spin_unlock_irqrestore(&s->lock, flags);
	if (ret && ret != -ENOBUFS && ret != -ENODEV)
		lem_stream_wake(s);
	return ret;
}

void lem_stream_complete(struct lem_stream *s, const struct lem_stream_slot *slot, u32 bytes,
			 u64 sequence, u64 timestamp_ns)
{
	unsigned long flags;

	spin_lock_irqsave(&s->lock, flags);
	/* Never more than this buffer was given for, whatever the block says. */
	lem_stream_finish_locked(s, &slot->job, min(bytes, slot->bytes), sequence, timestamp_ns);
	spin_unlock_irqrestore(&s->lock, flags);
	lem_stream_wake(s);
}

/* The frame never ran. Returns its buffer and what its job held, without publishing. */
void lem_stream_cancel(struct lem_stream *s, const struct lem_stream_slot *slot)
{
	unsigned long flags;

	spin_lock_irqsave(&s->lock, flags);
	for (u32 i = 0; i < slot->job.count; i++)
		lem_stream_region_at(s, slot->job.range[i].region)->users--;
	s->buf[slot->job.buffer].state = LEM_BUF_FREE;
	spin_unlock_irqrestore(&s->lock, flags);
}

/* The registered region a range names, or NULL for the pool or an empty slot. Lock held. */
static const struct lem_stream_region *lem_stream_range_region(const struct lem_stream *s,
							       const struct lem_job_range *r)
{
	const struct lem_stream_region *reg;
	u64 end;

	if (r->region == LEM_STREAM_POOL || r->region >= LEM_STREAM_REGIONS)
		return NULL;
	reg = &s->region[r->region];
	if (!reg->buf || !reg->users || check_add_overflow(r->offset, r->len, &end) ||
	    end > reg->size)
		return NULL;
	return reg;
}

int lem_stream_range_read(struct lem_stream *s, const struct lem_job_range *r, void *dst,
			  size_t bytes)
{
	const struct lem_stream_region *reg;
	unsigned long flags;
	int ret = -EFAULT;

	spin_lock_irqsave(&s->lock, flags);
	reg = lem_stream_range_region(s, r);
	if (reg && !iosys_map_is_null(&reg->map)) {
		if (r->len == bytes) {
			memcpy(dst, (const u8 *)reg->map.vaddr + r->offset, bytes);
			ret = 0;
		} else {
			ret = -EINVAL;
		}
	}
	spin_unlock_irqrestore(&s->lock, flags);
	return ret;
}

int lem_stream_range_addr(struct lem_stream *s, const struct lem_job_range *r, u64 *addr)
{
	const struct lem_stream_region *reg;
	unsigned long flags;
	int ret = -EACCES;

	spin_lock_irqsave(&s->lock, flags);
	reg = lem_stream_range_region(s, r);
	if (reg && !check_add_overflow(reg->addr, r->offset, addr))
		ret = 0;
	spin_unlock_irqrestore(&s->lock, flags);
	return ret;
}

/* Test only, see lemnos_internal.h. */
void lem_stream_range_poke(struct lem_stream *s, const struct lem_job_range *r, size_t offset,
			   u32 val)
{
	const struct lem_stream_region *reg;
	unsigned long flags;

	spin_lock_irqsave(&s->lock, flags);
	reg = lem_stream_range_region(s, r);
	if (reg && !iosys_map_is_null(&reg->map) && offset + sizeof(val) <= r->len)
		WRITE_ONCE(*(u32 *)((u8 *)reg->map.vaddr + r->offset + offset), val);
	spin_unlock_irqrestore(&s->lock, flags);
}

/*
 * Registered regions keep a kernel mapping from here on, so a block can read
 * through lem_stream_range_read(). s->ctl, so never under the block's locks.
 */
int lem_stream_cpu_read_enable(struct lem_stream *s)
{
	int ret = 0;

	mutex_lock(&s->ctl);
	if (s->dead) {
		ret = -ENODEV;
		goto out;
	}
	s->cpu_read = true;
	for (u32 i = LEM_STREAM_POOL + 1; i < LEM_STREAM_REGIONS && !ret; i++) {
		struct lem_stream_region *r = &s->region[i];

		if (r->buf && iosys_map_is_null(&r->map))
			ret = lem_region_vmap(r);
	}
out:
	mutex_unlock(&s->ctl);
	return ret;
}

void lem_stream_wedge(struct lem_stream *s)
{
	unsigned long flags;

	spin_lock_irqsave(&s->lock, flags);
	s->wedged = true;
	spin_unlock_irqrestore(&s->lock, flags);
}

int lem_stream_job_peek(struct lem_stream *s, void *job, size_t bytes)
{
	u64 *words = job;
	unsigned long flags;
	u32 slots = s->job_slots;
	u64 head, count, len;
	size_t at;
	int ret;

	if (bytes != s->producer.job_bytes)
		return -EINVAL;
	spin_lock_irqsave(&s->lock, flags);
	if (s->dead || s->fault) {
		ret = s->dead ? -ENODEV : s->fault;
		goto out;
	}
	/* First, so a buffer returned just before its job was posted is usable. */
	ret = lem_stream_collect_returns(s);
	if (ret)
		goto out;
	head = lem_ring_read_acquire(s, s->w_jobs_head);
	count = head - s->jobs_tail;
	if (!count) {
		ret = 0;
		goto out;
	}
	if (count > slots) {
		ret = -ERANGE;
		goto out;
	}
	at = s->jobs_at + (s->jobs_tail & (slots - 1)) * s->job_words;
	len = lem_ring_read(s, at);
	if (len != bytes) {
		ret = -EMSGSIZE;
		goto out;
	}
	for (size_t i = 0; i < bytes / sizeof(u64); i++)
		words[i] = lem_ring_read(s, at + 1 + i);
	ret = 1;
out:
	spin_unlock_irqrestore(&s->lock, flags);
	return ret;
}

int lem_stream_job_take(struct lem_stream *s, u32 buffer, u64 identity,
			const struct lemnos_stream_range *range, u32 count,
			struct lem_stream_slot *slot)
{
	struct lem_stream_buf *b;
	unsigned long flags;
	u64 addr;
	int ret = 0;

	if (count > LEM_JOB_HOLDS)
		return -EINVAL;
	memset(&slot->job, 0, sizeof(slot->job));
	spin_lock_irqsave(&s->lock, flags);
	if (s->dead || s->fault) {
		ret = s->dead ? -ENODEV : s->fault;
		goto out;
	}
	if (buffer >= s->buffers) {
		ret = -EINVAL;
		goto out;
	}
	if (s->shm)
		lem_stream_reclaim(s, buffer);
	for (u32 i = 0; i < count; i++) {
		ret = lem_stream_resolve(s, buffer, range[i].addr, range[i].len, &slot->job.range[i]);
		if (ret)
			goto out;
	}
	b = &s->buf[buffer];
	if (b->state == LEM_BUF_ABSENT) {
		ret = -EFAULT;
		goto out;
	}
	if (b->state == LEM_BUF_QUEUED || b->state == LEM_BUF_ACTIVE) {
		ret = -EBUSY;
		goto out;
	}
	if (b->state != LEM_BUF_FREE) {
		ret = -EACCES;
		goto out;
	}
	if (lem_stream_buffer_addr(s, buffer, s->frame_bytes, &addr)) {
		ret = -EFAULT;
		goto out;
	}
	for (u32 i = 0; i < count; i++)
		lem_stream_region_at(s, slot->job.range[i].region)->users++;
	slot->job.identity = identity;
	slot->job.buffer = buffer;
	slot->job.count = count;
	slot->addr = addr;
	slot->bytes = s->frame_bytes;
	b->state = LEM_BUF_ACTIVE;
	s->jobs++;
	s->jobs_tail++;
	lem_ring_write_release(s, s->w_jobs_tail, s->jobs_tail);
out:
	spin_unlock_irqrestore(&s->lock, flags);
	return ret;
}

void lem_stream_fault(struct lem_stream *s, int err)
{
	unsigned long flags;

	spin_lock_irqsave(&s->lock, flags);
	lem_stream_fault_locked(s, err);
	spin_unlock_irqrestore(&s->lock, flags);
	lem_stream_wake(s);
}

/* --- dma-bufs on the block --- */

static void lem_region_unmap(struct lem_stream_region *r)
{
	if (!r->buf)
		return;
	if (!iosys_map_is_null(&r->map))
		dma_buf_vunmap_unlocked(r->buf, &r->map);
	if (r->sgt)
		dma_buf_unmap_attachment_unlocked(r->att, r->sgt, DMA_BIDIRECTIONAL);
	if (r->att)
		dma_buf_detach(r->buf, r->att);
	dma_buf_put(r->buf);
	memset(r, 0, sizeof(*r));
}

/* Pins `fd`, mapped to the block: one contiguous range in its address space, or refused. */
static int lem_region_map(struct device *dev, int fd, struct lem_stream_region *r)
{
	struct scatterlist *sg;
	dma_addr_t next;
	u64 size = 0, end;
	int ret, i;

	memset(r, 0, sizeof(*r));
	r->buf = dma_buf_get(fd);
	if (IS_ERR(r->buf)) {
		ret = PTR_ERR(r->buf);
		r->buf = NULL;
		return ret;
	}
	r->att = dma_buf_attach(r->buf, dev);
	if (IS_ERR(r->att)) {
		ret = PTR_ERR(r->att);
		r->att = NULL;
		goto fail;
	}
	r->sgt = dma_buf_map_attachment_unlocked(r->att, DMA_BIDIRECTIONAL);
	if (IS_ERR(r->sgt)) {
		ret = PTR_ERR(r->sgt);
		r->sgt = NULL;
		goto fail;
	}
	ret = -EINVAL;
	if (!r->sgt->nents)
		goto fail;
	next = sg_dma_address(r->sgt->sgl);
	r->addr = next;
	for_each_sgtable_dma_sg(r->sgt, sg, i) {
		if (sg_dma_address(sg) != next)
			goto fail;
		if (check_add_overflow(next, (dma_addr_t)sg_dma_len(sg), &next))
			goto fail;
		size += sg_dma_len(sg);
	}
	if (!size || check_add_overflow(r->addr, size, &end))
		goto fail;
	r->size = size;
	r->end = end;
	if (test_stream_source) {
		ret = dma_buf_vmap_unlocked(r->buf, &r->map);
		if (ret)
			goto fail;
		ret = -EINVAL;
		if (r->map.is_iomem)
			goto fail;
	}
	return 0;
fail:
	lem_region_unmap(r);
	return ret;
}

/* A region's kernel mapping, for lem_stream_range_read(). Not for iomem: see lem_region_map. */
static int lem_region_vmap(struct lem_stream_region *r)
{
	int ret = dma_buf_vmap_unlocked(r->buf, &r->map);

	if (!ret && r->map.is_iomem) {
		dma_buf_vunmap_unlocked(r->buf, &r->map);
		iosys_map_clear(&r->map);
		return -EINVAL;
	}
	return ret;
}

/* --- the stream's ioctls --- */

/*
 * Whether `r` is, or overlaps, memory the stream already has, other than
 * `except`: a region overlapping another would be another path past the
 * checks. Lock held.
 */
static bool lem_stream_region_taken(const struct lem_stream *s, const struct lem_stream_region *r,
				    const struct lem_stream_region *except)
{
	for (u32 i = 0; i < LEM_STREAM_REGIONS + (s->own ? s->buffers : 0); i++) {
		const struct lem_stream_region *other =
			i < LEM_STREAM_REGIONS ? &s->region[i] : &s->own[i - LEM_STREAM_REGIONS];

		if (other == except || !other->buf)
			continue;
		if (other->buf == r->buf || (r->addr < other->end && other->addr < r->end))
			return true;
	}
	return false;
}

/* Holds s->ctl. */
static long lem_stream_register(struct lem_stream *s, struct lemnos_stream_register *a)
{
	struct lem_stream_region r;
	unsigned long flags;
	u32 slot = LEM_STREAM_REGIONS;
	int ret;

	ret = lem_region_map(s->dev, a->fd, &r);
	if (ret)
		return ret;
	if (s->cpu_read && iosys_map_is_null(&r.map)) {
		ret = lem_region_vmap(&r);
		if (ret) {
			lem_region_unmap(&r);
			return ret;
		}
	}

	spin_lock_irqsave(&s->lock, flags);
	if (lem_stream_region_taken(s, &r, NULL)) {
		spin_unlock_irqrestore(&s->lock, flags);
		lem_region_unmap(&r);
		return -EBUSY;
	}
	for (u32 i = LEM_STREAM_POOL + 1; i < LEM_STREAM_REGIONS; i++) {
		if (!s->region[i].buf) {
			slot = i;
			break;
		}
	}
	if (slot == LEM_STREAM_REGIONS) {
		spin_unlock_irqrestore(&s->lock, flags);
		lem_region_unmap(&r);
		return -ENOSPC;
	}
	s->region[slot] = r;
	spin_unlock_irqrestore(&s->lock, flags);

	a->region = slot;
	a->addr = r.addr;
	a->size = r.size;
	return 0;
}

/* Holds s->ctl. */
static long lem_stream_unregister(struct lem_stream *s, u32 region)
{
	struct lem_stream_region r;
	unsigned long flags;

	if (region == LEM_STREAM_POOL || region >= LEM_STREAM_REGIONS)
		return -EINVAL;
	spin_lock_irqsave(&s->lock, flags);
	if (!s->region[region].buf) {
		spin_unlock_irqrestore(&s->lock, flags);
		return -ENOENT;
	}
	if (s->region[region].users) {
		spin_unlock_irqrestore(&s->lock, flags);
		return -EBUSY;
	}
	r = s->region[region];
	memset(&s->region[region], 0, sizeof(s->region[region]));
	spin_unlock_irqrestore(&s->lock, flags);
	lem_region_unmap(&r);
	return 0;
}

/*
 * LEMNOS_STREAM_IOC_BUFFER: gives buffer a->buffer its memory, or swaps it.
 * The new dma-buf is pinned and checked first; it takes the buffer's place in
 * one step under s->lock, only while nothing can be writing the buffer; the
 * old one is unpinned after. Holds s->ctl.
 */
static long lem_stream_set_buffer(struct lem_stream *s, struct lemnos_stream_buffer *a)
{
	struct lem_stream_region r, old = {};
	struct lem_stream_region *at;
	unsigned long flags;
	long ret = 0;
	u8 state;

	if (!s->own)
		return -EOPNOTSUPP;
	if (a->buffer >= s->buffers)
		return -EINVAL;
	ret = lem_region_map(s->dev, a->fd, &r);
	if (ret)
		return ret;
	/* Only what the buffer uses is reachable, however big the dma-buf. */
	if (r.size < s->buffer_bytes || (r.addr & (LEM_STREAM_DMA_ALIGN - 1))) {
		lem_region_unmap(&r);
		return -EINVAL;
	}
	r.size = s->buffer_bytes;
	r.end = r.addr + s->buffer_bytes;

	at = &s->own[a->buffer];
	spin_lock_irqsave(&s->lock, flags);
	state = s->buf[a->buffer].state;
	if (s->wedged) {
		ret = -EIO;
	} else if (at->buf == r.buf) {
		/* What it already has: nothing changes, and the new pin is dropped below. */
		a->addr = at->addr;
		a->size = at->size;
		old = r;
	} else if (state == LEM_BUF_QUEUED || state == LEM_BUF_ACTIVE || at->users ||
		   lem_stream_region_taken(s, &r, at)) {
		ret = -EBUSY;
	} else {
		old = *at;
		*at = r;
		if (state == LEM_BUF_ABSENT)
			s->buf[a->buffer].state = LEM_BUF_FREE;
		a->addr = r.addr;
		a->size = r.size;
	}
	spin_unlock_irqrestore(&s->lock, flags);
	if (ret)
		lem_region_unmap(&r);
	else
		lem_region_unmap(&old);
	return ret;
}

/* Holds s->ctl. */
static long lem_stream_test_source(struct lem_stream *s, const struct lemnos_stream_test_source *a)
{
	unsigned long flags;

	if (!test_stream_source)
		return -EPERM;
	if (READ_ONCE(s->producer.stop))
		return -EBUSY;
	cancel_delayed_work_sync(&s->source);
	spin_lock_irqsave(&s->lock, flags);
	WRITE_ONCE(s->period_us, a->period_us);
	s->left = a->frames;
	spin_unlock_irqrestore(&s->lock, flags);
	if (a->frames)
		schedule_delayed_work(&s->source, 0);
	return 0;
}

/* Unpins a reader's marks page, whatever part of it was pinned. */
static void lem_reader_release(struct lem_reader *r)
{
	if (r->marks)
		vunmap(r->marks);
	for (unsigned int i = 0; i < r->npages; i++)
		put_page(r->pages[i]);
	kfree(r->pages);
	memset(r, 0, sizeof(*r));
}

/*
 * LEMNOS_STREAM_IOC_READER: pins a reader's marks page and gives the reader a
 * slot. The page must be a shmem file, sealed against shrinking and growing,
 * and big enough for the buffers. Its pages are mapped read-only and never
 * written. Called with s->ctl held, which orders the table's changes; the new
 * entry is published under s->lock, where the frame path reads it.
 */
static long lem_stream_reader_add(struct lem_stream *s, struct lemnos_stream_reader *a)
{
	size_t bytes = (LEM_MARKS_AT + 2 * (size_t)s->buffers) * sizeof(u64);
	const unsigned int want = F_SEAL_SHRINK | F_SEAL_GROW;
	struct lem_reader r = {};
	struct file *file;
	unsigned long flags;
	u32 slot;
	long ret = -EINVAL;

	if (!s->shm || a->reserved || a->max_hold_ns)
		return -EINVAL;
	for (slot = 0; slot < LEMNOS_STREAM_MAX_READERS; slot++)
		if (!s->reader[slot].marks)
			break;
	if (slot == LEMNOS_STREAM_MAX_READERS)
		return -ENOSPC;

	file = fget(a->fd);
	if (!file)
		return -EBADF;
	if (!shmem_file(file) ||
	    (READ_ONCE(SHMEM_I(file_inode(file))->seals) & want) != want ||
	    i_size_read(file_inode(file)) < (loff_t)bytes)
		goto put;

	r.npages = DIV_ROUND_UP(bytes, PAGE_SIZE);
	r.pages = kcalloc(r.npages, sizeof(*r.pages), GFP_KERNEL);
	if (!r.pages) {
		ret = -ENOMEM;
		goto put;
	}
	for (unsigned int i = 0; i < r.npages; i++) {
		struct page *page = shmem_read_mapping_page(file->f_mapping, i);

		if (IS_ERR(page)) {
			ret = PTR_ERR(page);
			r.npages = i;
			goto unpin;
		}
		r.pages[i] = page;
	}
	r.marks = vmap(r.pages, r.npages, VM_MAP, PAGE_KERNEL_RO);
	if (!r.marks) {
		ret = -ENOMEM;
		goto unpin;
	}
	r.max_held = a->max_held;

	spin_lock_irqsave(&s->lock, flags);
	s->reader[slot] = r;
	spin_unlock_irqrestore(&s->lock, flags);
	a->slot = slot;
	fput(file);
	return 0;

unpin:
	lem_reader_release(&r);
put:
	fput(file);
	return ret;
}

static long lem_stream_ioctl(struct file *file, unsigned int cmd, unsigned long arg)
{
	struct lem_stream *s = file->private_data;
	void __user *uarg = (void __user *)arg;
	unsigned long flags;
	long ret;

	if (cmd == LEMNOS_STREAM_IOC_KICK) {
		long (*kick)(void *priv) = NULL;

		spin_lock_irqsave(&s->lock, flags);
		if (s->dead)
			ret = -ENODEV;
		else if (s->fault)
			ret = s->fault;
		else if (s->producer.kick)
			kick = s->producer.kick;
		else
			ret = lem_stream_drain(s);
		spin_unlock_irqrestore(&s->lock, flags);
		if (kick) {
			ret = kick(s->producer.priv);
			if (ret && ret != -ENODEV)
				lem_stream_fault(s, ret);
			return ret;
		}
		if (ret && ret != -ENODEV)
			lem_stream_wake(s);
		return ret;
	}

	if (cmd == LEMNOS_STREAM_IOC_STATUS) {
		struct lemnos_stream_status st;

		memset(&st, 0, sizeof(st));
		spin_lock_irqsave(&s->lock, flags);
		st.state = s->fault ? LEMNOS_RING_STATE_FAULT :
			   s->dead ? LEMNOS_RING_STATE_CLOSED : LEMNOS_RING_STATE_OPEN;
		st.fault = -s->fault;
		st.frames = s->published;
		st.dropped = s->dropped;
		st.returned = s->returned;
		st.jobs = s->jobs;
		spin_unlock_irqrestore(&s->lock, flags);
		return copy_to_user(uarg, &st, sizeof(st)) ? -EFAULT : 0;
	}

	if (cmd == LEMNOS_STREAM_IOC_REGISTER) {
		struct lemnos_stream_register a;

		if (copy_from_user(&a, uarg, sizeof(a)))
			return -EFAULT;
		mutex_lock(&s->ctl);
		ret = s->dead ? -ENODEV : s->fault ? s->fault : lem_stream_register(s, &a);
		mutex_unlock(&s->ctl);
		if (!ret && copy_to_user(uarg, &a, sizeof(a)))
			ret = -EFAULT;
		return ret;
	}

	if (cmd == LEMNOS_STREAM_IOC_UNREGISTER) {
		struct lemnos_stream_unregister a;

		if (copy_from_user(&a, uarg, sizeof(a)))
			return -EFAULT;
		if (a.reserved)
			return -EINVAL;
		mutex_lock(&s->ctl);
		ret = s->dead ? -ENODEV : lem_stream_unregister(s, a.region);
		mutex_unlock(&s->ctl);
		return ret;
	}

	if (cmd == LEMNOS_STREAM_IOC_BUFFER) {
		struct lemnos_stream_buffer a;

		if (copy_from_user(&a, uarg, sizeof(a)))
			return -EFAULT;
		mutex_lock(&s->ctl);
		ret = s->dead ? -ENODEV : s->fault ? s->fault : lem_stream_set_buffer(s, &a);
		mutex_unlock(&s->ctl);
		if (!ret && copy_to_user(uarg, &a, sizeof(a)))
			ret = -EFAULT;
		return ret;
	}

	if (cmd == LEMNOS_STREAM_IOC_READER) {
		struct lemnos_stream_reader a;

		if (copy_from_user(&a, uarg, sizeof(a)))
			return -EFAULT;
		mutex_lock(&s->ctl);
		ret = s->dead ? -ENODEV : s->fault ? s->fault : lem_stream_reader_add(s, &a);
		mutex_unlock(&s->ctl);
		if (!ret && copy_to_user(uarg, &a, sizeof(a)))
			ret = -EFAULT;
		return ret;
	}

	if (cmd == LEMNOS_STREAM_IOC_TEST_SOURCE) {
		struct lemnos_stream_test_source a;

		if (copy_from_user(&a, uarg, sizeof(a)))
			return -EFAULT;
		mutex_lock(&s->ctl);
		ret = s->dead ? -ENODEV : s->fault ? s->fault : lem_stream_test_source(s, &a);
		mutex_unlock(&s->ctl);
		return ret;
	}

	/* Under s->ctl, because a stream that's over may have lost its block. */
	mutex_lock(&s->ctl);
	if (!s->producer.ioctl)
		ret = -ENOTTY;
	else
		ret = s->dead ? -ENODEV : s->producer.ioctl(s->producer.priv, cmd, uarg);
	mutex_unlock(&s->ctl);
	return ret;
}

static __poll_t lem_stream_poll(struct file *file, poll_table *wait)
{
	struct lem_stream *s = file->private_data;
	unsigned long flags;
	__poll_t mask = 0;

	poll_wait(file, &s->wait, wait);
	spin_lock_irqsave(&s->lock, flags);
	/* A tail the consumer scribbled on only wakes the consumer. */
	if (s->dead || s->fault)
		mask = EPOLLIN | EPOLLRDNORM | EPOLLHUP;
	else if (!s->shm && s->frames_head != READ_ONCE(s->ring[LEMNOS_RING_FRAMES_TAIL]))
		mask = EPOLLIN | EPOLLRDNORM;
	spin_unlock_irqrestore(&s->lock, flags);
	return mask;
}

static int lem_stream_mmap(struct file *file, struct vm_area_struct *vma)
{
	struct lem_stream *s = file->private_data;
	unsigned long len = vma->vm_end - vma->vm_start;

	if (vma->vm_pgoff && s->producer.mmap)
		return s->producer.mmap(s->producer.priv, vma);
	if (!(vma->vm_flags & VM_SHARED) || vma->vm_pgoff || len != PAGE_ALIGN(s->ring_bytes))
		return -EINVAL;
	if (vma->vm_flags & VM_EXEC)
		return -EPERM;
	vm_flags_clear(vma, VM_MAYEXEC);
	return remap_vmalloc_range(vma, s->ring, 0);
}

/*
 * Ends the stream: nothing runs on the frame path after this, every region is
 * unpinned, and the consumer learns. Once only; waits for work in flight.
 */
static void lem_stream_shutdown(struct lem_stream *s, u64 state)
{
	struct lem_stream_producer producer;
	unsigned long flags;
	bool wedged;

	mutex_lock(&s->ctl);
	if (s->dead) {
		mutex_unlock(&s->ctl);
		return;
	}
	spin_lock_irqsave(&s->lock, flags);
	s->dead = true;
	producer = s->producer;
	s->producer.stop = NULL;
	if (!s->fault)
		lem_ring_write_release(s, s->w_state, state);
	spin_unlock_irqrestore(&s->lock, flags);

	cancel_delayed_work_sync(&s->source);

	/* The block's DMA stops here, before anything it could write to is released. */
	if (producer.stop)
		producer.stop(producer.priv, s);

	spin_lock_irqsave(&s->lock, flags);
	wedged = s->wedged;
	spin_unlock_irqrestore(&s->lock, flags);
	if (wedged) {
		/* DMA may still run: keep every pin for good. Nothing else is safe. */
		lem_stream_wake(s);
		mutex_unlock(&s->ctl);
		return;
	}

	spin_lock_irqsave(&s->lock, flags);
	for (u32 i = 0; i < LEM_STREAM_REGIONS + (s->own ? s->buffers : 0); i++) {
		struct lem_stream_region *at = lem_stream_region_at(s, i);
		struct lem_stream_region r = *at;

		if (!r.buf)
			continue;
		memset(at, 0, sizeof(*at));
		spin_unlock_irqrestore(&s->lock, flags);
		lem_region_unmap(&r);
		spin_lock_irqsave(&s->lock, flags);
	}
	spin_unlock_irqrestore(&s->lock, flags);

	lem_stream_wake(s);
	mutex_unlock(&s->ctl);
}

static void lem_stream_free(struct lem_stream *s)
{
	if (s->producer.release)
		s->producer.release(s->producer.priv);
	if (s->doorbell)
		eventfd_ctx_put(s->doorbell);
	if (!s->wedged) {
		for (u32 i = 0; i < LEM_STREAM_REGIONS; i++)
			lem_region_unmap(&s->region[i]);
		for (u32 i = 0; s->own && i < s->buffers; i++)
			lem_region_unmap(&s->own[i]);
	}
	for (u32 r = 0; r < LEMNOS_STREAM_MAX_READERS; r++)
		lem_reader_release(&s->reader[r]);
	kvfree(s->own);
	kvfree(s->queue);
	kvfree(s->buf);
	vfree(s->ring);
	if (!s->wedged)
		put_device(s->dev);
	mutex_destroy(&s->ctl);
	kfree(s);
}

static int lem_stream_release(struct inode *inode, struct file *file)
{
	struct lem_stream *s = file->private_data;

	mutex_lock(&lem_streams_lock);
	list_del(&s->node);
	lem_stream_count--;
	mutex_unlock(&lem_streams_lock);
	lem_stream_shutdown(s, LEMNOS_RING_STATE_CLOSED);
	lem_stream_free(s);
	return 0;
}

static void lem_stream_gone(struct work_struct *work)
{
	lem_stream_shutdown(container_of(work, struct lem_stream, gone), LEMNOS_RING_STATE_CLOSED);
}

static const struct file_operations lem_stream_fops = {
	.owner = THIS_MODULE,
	.release = lem_stream_release,
	.poll = lem_stream_poll,
	.mmap = lem_stream_mmap,
	.unlocked_ioctl = lem_stream_ioctl,
	.compat_ioctl = compat_ptr_ioctl,
	.llseek = noop_llseek,
};

/* The limits both sides of the protocol enforce (Layout::checked). */
static int lem_stream_check_layout(const struct lemnos_stream_create *c)
{
	u64 stride;

	if (c->flags & ~(LEMNOS_STREAM_CREATE_BUFFERS | LEMNOS_STREAM_CREATE_SHM))
		return -EINVAL;
	if ((c->flags & LEMNOS_STREAM_CREATE_SHM) && c->frame_slots < 2)
		return -EINVAL;
	if (!c->buffers || c->buffers > LEMNOS_STREAM_MAX_BUFFERS)
		return -EINVAL;
	/* No pool: no pool descriptor either, and a table of dma-bufs with an end. */
	if ((c->flags & LEMNOS_STREAM_CREATE_BUFFERS) &&
	    (c->pool_fd != -1 || c->buffers > LEMNOS_STREAM_MAX_OWN_BUFFERS))
		return -EINVAL;
	if (!c->buffer_bytes || c->buffer_bytes > LEMNOS_STREAM_MAX_BUFFER_BYTES)
		return -EINVAL;
	if (!is_power_of_2(c->frame_slots) || c->frame_slots < c->buffers ||
	    c->frame_slots > 2 * LEMNOS_STREAM_MAX_BUFFERS)
		return -EINVAL;
	if (!is_power_of_2(c->job_slots) || c->job_slots > LEMNOS_STREAM_MAX_JOB_SLOTS)
		return -EINVAL;
	if (c->job_bytes < sizeof(struct lemnos_stream_job) ||
	    c->job_bytes > LEMNOS_STREAM_MAX_JOB_BYTES)
		return -EINVAL;
	stride = ALIGN((u64)c->buffer_bytes, LEMNOS_STREAM_BUFFER_ALIGN);
	if ((u64)c->buffers * stride > LEMNOS_STREAM_MAX_POOL_BYTES)
		return -EINVAL;
	return 0;
}

static void lem_stream_init_ring(struct lem_stream *s)
{
	if (s->shm) {
		/* The pool page's shape, then its magic, then the ring's header: each magic last. */
		lem_ring_write(s, s->page_at + 1, 1ull | (u64)s->buffers << 32);
		lem_ring_write_release(s, s->page_at, LEM_POOL_MAGIC);
		lem_ring_write(s, LEM_SHM_W_LAYOUT, LEM_SHM_LAYOUT);
		lem_ring_write(s, LEM_SHM_W_SHAPE, 1ull | (u64)s->frame_slots << 32);
		lem_ring_write(s, LEM_SHM_W_SIZES, LEM_SHM_ENTRY_BYTES);
		lem_ring_write(s, LEM_SHM_W_TOTAL, s->shm_total);
		lem_ring_write_release(s, LEM_SHM_W_MAGIC, LEM_SHM_MAGIC);
		return;
	}
	lem_ring_write(s, LEMNOS_RING_VERSION, LEMNOS_RING_LAYOUT_VERSION);
	lem_ring_write(s, LEMNOS_RING_BUFFERS, s->buffers);
	lem_ring_write(s, LEMNOS_RING_BUFFER_BYTES, s->buffer_bytes);
	lem_ring_write(s, LEMNOS_RING_FRAME_SLOTS, s->frame_slots);
	lem_ring_write(s, LEMNOS_RING_JOB_SLOTS, s->job_slots);
	lem_ring_write(s, LEMNOS_RING_JOB_BYTES, s->job_bytes);
	lem_ring_write(s, LEMNOS_RING_STATE, LEMNOS_RING_STATE_OPEN);
	lem_ring_write_release(s, LEMNOS_RING_MAGIC, LEMNOS_RING_MAGIC_VALUE);
}

/* Called under lem_blocks_sem for reading: the block can't go while this runs. */
static long lem_stream_create_with(struct device *dev, struct lemnos_stream_create *c,
				   const struct lem_stream_producer *producer,
				   struct lem_stream **out)
{
	struct lem_stream *s;
	struct file *file;
	size_t ring_words;
	u64 pool_bytes;
	int fd, ret;

	BUILD_BUG_ON(IS_ENABLED(CONFIG_CPU_BIG_ENDIAN));
	BUILD_BUG_ON(sizeof(struct lemnos_stream_job) % 8);
	BUILD_BUG_ON(LEM_SHM_ENTRY_BYTES % 8 || LEM_SHM_SLOT_WORDS != 9);
	BUILD_BUG_ON(sizeof(struct lemnos_stream_reader) != 24);
	BUILD_BUG_ON(LEMNOS_STREAM_MAX_READERS != LEM_POOL_READERS);

	ret = lem_stream_check_layout(c);
	if (ret)
		return ret;
	mutex_lock(&lem_streams_lock);
	if (lem_stream_count >= LEM_STREAM_MAX) {
		mutex_unlock(&lem_streams_lock);
		return -ENOSPC;
	}
	lem_stream_count++;
	mutex_unlock(&lem_streams_lock);

	s = kzalloc(sizeof(*s), GFP_KERNEL);
	if (!s) {
		ret = -ENOMEM;
		goto drop_count;
	}
	s->dev = get_device(dev);
	mutex_init(&s->ctl);
	spin_lock_init(&s->lock);
	init_waitqueue_head(&s->wait);
	INIT_DELAYED_WORK(&s->source, lem_stream_source);
	INIT_WORK(&s->gone, lem_stream_gone);

	s->buffers = c->buffers;
	s->buffer_bytes = c->buffer_bytes;
	s->frame_slots = c->frame_slots;
	s->job_slots = c->job_slots;
	s->job_bytes = c->job_bytes;
	s->stride = ALIGN((u64)c->buffer_bytes, LEMNOS_STREAM_BUFFER_ALIGN);
	s->shm = c->flags & LEMNOS_STREAM_CREATE_SHM;
	s->newest = U32_MAX;
	s->job_words = 1 + DIV_ROUND_UP(s->job_bytes, 8);
	if (s->shm) {
		size_t control;

		s->shm_slots_at = LEM_SHM_INDEX + s->frame_slots;
		s->shm_total = s->shm_slots_at + (size_t)s->frame_slots * LEM_SHM_SLOT_WORDS;
		s->page_at = s->shm_total;
		control = s->page_at + LEM_POOL_TICKETS + s->buffers + LEM_POOL_READERS;
		s->w_state = LEM_SHM_W_END;
		s->w_returns_head = control;
		s->w_returns_tail = control + 1;
		s->w_jobs_head = control + 2;
		s->w_jobs_tail = control + 3;
		s->returns_at = control + 4;
		s->jobs_at = s->returns_at + s->frame_slots;
		ring_words = s->jobs_at + (size_t)s->job_slots * s->job_words;
	} else {
		s->w_state = LEMNOS_RING_STATE;
		s->w_returns_head = LEMNOS_RING_RETURNS_HEAD;
		s->w_returns_tail = LEMNOS_RING_RETURNS_TAIL;
		s->w_jobs_head = LEMNOS_RING_JOBS_HEAD;
		s->w_jobs_tail = LEMNOS_RING_JOBS_TAIL;
		s->returns_at = LEMNOS_RING_FRAMES + (size_t)s->frame_slots * LEMNOS_STREAM_FRAME_WORDS;
		s->jobs_at = s->returns_at + s->frame_slots;
		ring_words = s->jobs_at + (size_t)s->job_slots * s->job_words;
	}
	s->ring_bytes = ring_words * sizeof(u64);
	pool_bytes = (u64)s->buffers * s->stride;

	ret = -ENOMEM;
	s->buf = kvcalloc(s->buffers, sizeof(*s->buf), GFP_KERNEL);
	s->queue = kvcalloc(s->job_slots, sizeof(*s->queue), GFP_KERNEL);
	if (!s->buf || !s->queue)
		goto free;
	if (c->flags & LEMNOS_STREAM_CREATE_BUFFERS) {
		s->own = kvcalloc(s->buffers, sizeof(*s->own), GFP_KERNEL);
		if (!s->own)
			goto free;
		for (u32 i = 0; i < s->buffers; i++)
			s->buf[i].state = LEM_BUF_ABSENT;
	}
	s->ring = vmalloc_user(PAGE_ALIGN(s->ring_bytes));
	if (!s->ring)
		goto free;

	if (c->doorbell_fd != -1) {
		s->doorbell = eventfd_ctx_fdget(c->doorbell_fd);
		if (IS_ERR(s->doorbell)) {
			ret = PTR_ERR(s->doorbell);
			s->doorbell = NULL;
			goto free;
		}
	}
	if (!s->own) {
		ret = lem_region_map(dev, c->pool_fd, &s->region[LEM_STREAM_POOL]);
		if (ret)
			goto free;
		/* Only what the buffers use is reachable, however big the dma-buf. */
		if (s->region[LEM_STREAM_POOL].size < pool_bytes) {
			ret = -EINVAL;
			goto free;
		}
		s->region[LEM_STREAM_POOL].size = pool_bytes;
		s->region[LEM_STREAM_POOL].end = s->region[LEM_STREAM_POOL].addr + pool_bytes;
	}

	lem_stream_init_ring(s);

	if (producer) {
		ret = lem_stream_attach(s, producer, c->buffer_bytes);
		if (ret)
			goto free;
	}

	fd = get_unused_fd_flags(O_CLOEXEC);
	if (fd < 0) {
		ret = fd;
		goto free;
	}
	file = anon_inode_getfile("[lemnos-stream]", &lem_stream_fops, s, O_RDWR);
	if (IS_ERR(file)) {
		ret = PTR_ERR(file);
		put_unused_fd(fd);
		goto free;
	}

	mutex_lock(&lem_streams_lock);
	list_add(&s->node, &lem_streams);
	mutex_unlock(&lem_streams_lock);

	c->ring_bytes = s->ring_bytes;
	c->pool_addr = s->region[LEM_STREAM_POOL].addr;
	c->fd = fd;
	if (out)
		*out = s;
	fd_install(fd, file);
	return 0;
free:
	/* It never existed: priv is still the caller's. */
	s->producer.release = NULL;
	lem_stream_free(s);
drop_count:
	mutex_lock(&lem_streams_lock);
	lem_stream_count--;
	mutex_unlock(&lem_streams_lock);
	return ret;
}

long lem_stream_create(struct device *dev, struct lemnos_stream_create *c)
{
	return lem_stream_create_with(dev, c, NULL, NULL);
}

long lem_stream_create_producer(struct device *dev, struct lemnos_stream_create *c,
				const struct lem_stream_producer *producer,
				struct lem_stream **out)
{
	return lem_stream_create_with(dev, c, producer, out);
}

/*
 * The block is going: every stream on it ends here, and its mappings with it.
 * From a worker, because here a dma_buf_put would land on the unbind's task
 * work, which runs only when the unbind returns, and the unbind is waiting
 * for that very release.
 */
void lem_streams_block_gone(struct device *dev)
{
	struct lem_stream *s;

	mutex_lock(&lem_streams_lock);
	list_for_each_entry(s, &lem_streams, node) {
		if (s->dev != dev)
			continue;
		schedule_work(&s->gone);
		flush_work(&s->gone);
	}
	mutex_unlock(&lem_streams_lock);
}
