/* SPDX-License-Identifier: GPL-2.0 */
/*
 * The frame stream's state, while its code moves from lemnos_stream.c to Rust (rs/stream_*.rs): both
 * halves read these layouts, the Rust half through the module's bindgen. Gone with lemnos_stream.c.
 */
#ifndef LEMNOS_STREAM_PRIV_H
#define LEMNOS_STREAM_PRIV_H

#include <linux/dma-buf.h>
#include <linux/dma-direction.h>
#include <linux/eventfd.h>
#include <linux/iosys-map.h>
#include <linux/list.h>
#include <linux/mutex.h>
#include <linux/refcount.h>
#include <linux/spinlock.h>
#include <linux/wait.h>
#include <linux/workqueue.h>

#include "lemnos_internal.h"

/* Each stream is kernel memory, so their number has an end. */
#define LEM_STREAM_MAX 64
/*
 * The pool, then the dma-bufs the consumer registers. A range's region at or
 * past LEM_STREAM_REGIONS is a buffer's own dma-buf: s->own[region - this].
 */
#define LEM_STREAM_REGIONS 16
/* Unregistered lent regions kept pinned for the next register of the same dma-buf. */
#define LEM_STREAM_PINS 8
#define LEM_STREAM_POOL 0

/* Words a job occupies: its bytes, and nothing about what it means. */
#define LEM_JOB_WORDS (sizeof(struct lemnos_stream_job) / 8)

/*
 * The frame ring: shm/README.md's ring (layout 2),
 * one field, one sample per frame, and the pool page of
 * shm/ring-buffers/src/page.rs after it. Every number here is what the Rust
 * crates say (testing/crates/shm-ring-check reads what this writes, and its
 * own tests pin the derived shape): the word numbers, the slot size, the
 * entry, the stamps, and the pool page's ticket words.
 */
#define LEM_SHM_MAGIC		0x31474e49524d4853ull	/* "SHMRING1" */
#define LEM_SHM_LAYOUT		2ull	/* shm-ring's LAYOUT: a reader of another layout refuses the ring */
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
#define LEM_SHM_ENTRY_BYTES	48	/* generation, count, buffer, ticket, sequence, identity, bytes, fence, end time */
#define LEM_SHM_SLOT_WORDS	(LEM_SHM_SLOT_HEADER + LEM_SHM_ENTRY_BYTES / 8)
#define LEM_POOL_MAGIC		0x314c4f4f504d4853ull	/* "SHMPOOL1" */
#define LEM_POOL_LAYOUT		2ull	/* shm-ring-buffers' page::LAYOUT: a reader of another layout refuses the page */
#define LEM_POOL_LAYOUT_WORD	1
#define LEM_POOL_SHAPE_WORD	2
#define LEM_POOL_TICKETS	8	/* one word per buffer, from here */
#define LEM_POOL_READERS	64	/* shm-ring-buffers' MAX_READERS: cut-off words after the tickets */
/* The job block, from the control words (control = page_at + LEM_POOL_TICKETS + buffers + LEM_POOL_READERS). */
#define LEM_JOB_HEAD		2	/* jobs posted: the consumer's */
#define LEM_JOB_TAIL		3	/* jobs taken: the module's */
#define LEM_JOB_SLOTS		4	/* job slots from here, each 1 + job_bytes/8 words: the length, then the job */
#define LEM_MARKS_AT		1	/* a marks page's first mark: word 0 is its layout and version (marks.rs) */
#define LEM_MARKS_LAYOUT	2ull	/* shm-ring-holds' LAYOUT: a marks page of another layout is refused */
#define LEM_MARKS_LAYOUT_SHIFT	56	/* the layout is word 0's top byte; a layout 1 page has zero there */
/* How long a reader's marks may stay uncountable when it has no hold time of its own (table.rs). */
#define LEM_UNCOUNTED_NS	(10ull * NSEC_PER_SEC)

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
	enum dma_data_direction dir;	/* how the block may use it; the map and the unmap agree */
	bool lent;			/* a read-only input lent to the block (LEMNOS_STREAM_IOC_REGISTER_LENT) */
	u32 gen;			/* lent: the memory generation it was registered with */
	struct iosys_map map;		/* only with test_stream_source */
	u64 addr, end;			/* device addresses, addr < end */
	u64 size;
	u32 users;			/* job ranges not finished */
};

/*
 * The consumer's doorbell (shm/ring-doorbell's publisher side): the eventfd it sleeps on,
 * and its page, mapped read-only. Word 0 of the page is sleeps << 1 | asleep, written by the
 * consumer only. The module rings the eventfd once per sleep, after a publish that finds the
 * consumer asleep, and remembers the sleep it rang for, so no sleep is rung twice.
 */
struct lem_bell {
	struct eventfd_ctx *efd;	/* NULL: no bell */
	struct page *page;		/* the page, kept referenced for the stream's life */
	u64 *map;			/* vmap of page, read-only; set last, with release */
	atomic64_t rung;		/* the state the consumer was last rung for */
};

/*
 * A reader's marks page, pinned for the stream's life: the reader's own memfd,
 * mapped read-only here. Its words are shm/ring-holds' layout 2 (marks.rs): the
 * layout and version in word 0, one mark per buffer, nonzero while the reader
 * holds it, then one taken stamp per buffer.
 */
struct lem_reader {
	struct page **pages;
	unsigned int npages;
	u64 *marks;			/* vmap of pages, read-only; NULL: no reader in this slot */
	u32 max_held;			/* what the reader may hold at once; 0 for no limit */
	u64 max_hold_ns;		/* how long one hold may last; 0 for no limit */
	bool cut_off;			/* over a limit: its marks hold nothing, and the pool page says so */
	u64 uncounted_since;		/* when its marks last could not be counted at one instant; 0 if they can */
	u64 *seen;			/* per buffer: the use it was first seen marked in (+1), and when */
	u64 id;				/* the registration's id; 0 when the slot is free */
};

/* What creation fixes: the ring and every size. Never written after; read without a lock. */
struct lem_geom {
	u64 *ring;			/* vmalloc_user: mapped by the consumer */
	size_t ring_bytes;
	/* The module's copies. The ring's header is for the consumer only. */
	u32 buffers, buffer_bytes, frame_slots, job_slots, job_bytes;
	size_t jobs_at, job_words;
	u64 stride;
	size_t shm_total;		/* words of the shm ring */
	size_t shm_slots_at;		/* its slots */
	size_t page_at;			/* the pool page's first word (shm form) */
	size_t w_state, w_jobs_head, w_jobs_tail;
};

/* The frame path's state: everything here is under s->lock. */
struct lem_frame {
	struct lem_stream_buf *buf;
	struct lem_job *queue;		/* job_slots validated jobs */
	u64 queue_head, queue_tail;
	struct lem_stream_region region[LEM_STREAM_REGIONS];
	struct lem_stream_region *own;	/* without a pool: each buffer's dma-buf; NULL with one */
	u32 cursor;			/* where the next jobless frame looks */

	/* The producer's own counts, never read back from the ring. */
	u64 frames_head, jobs_tail, sequence;
	u64 published, dropped, returned, jobs;

	int fault;			/* errno that ended it; the ring is left alone after */
	bool dead;			/* over: the block went or the file closed */
	u32 left;			/* test source: frames still to make */

	struct lem_stream_producer producer;	/* the block feeding this stream, or stop NULL */
	u32 frame_bytes;		/* what the producer may write per buffer */
	bool wedged;			/* a block's DMA could not be seen to stop: nothing is unpinned */

	struct lem_reader reader[LEMNOS_STREAM_MAX_READERS];	/* the consumers' marks */
	u32 newest;			/* the buffer of the newest frame, kept for the frame path */
	u64 newest_seq;			/* that frame's sequence */
	u64 reader_ids;			/* the last registration id given out */
};

struct lem_stream {
	struct list_head node;		/* on lem_streams */
	struct device *dev;		/* held until the stream is freed */
	struct mutex ctl;		/* regions, the source, teardown */
	spinlock_t lock;		/* f */
	wait_queue_head_t wait;
	struct eventfd_ctx *doorbell;
	struct lem_geom g;
	struct lem_frame f;
	/* Lent regions unregistered and kept pinned, by memory generation; see lem_stream_register_lent(). Under ctl. */
	struct lem_stream_region pins[LEM_STREAM_PINS];
	u32 pin_next;			/* the pin a full cache gives up next */
	bool cpu_read;			/* registered regions keep a kernel mapping */

	struct delayed_work source;	/* test source */
	u32 period_us;
	struct work_struct gone;	/* the block going away, see lem_streams_block_gone */
	struct lem_bell bell;		/* shm form: the consumer's doorbell, LEMNOS_STREAM_IOC_BELL */
	refcount_t refs;		/* the stream's file, and each reader file that holds the stream */
};

/*
 * What a reader's file holds: the registration it made. Closing the file (an
 * exit closes it too) or LEMNOS_READER_IOC_UNREGISTER frees the slot, if the
 * slot still carries this registration. The file holds a reference on the stream.
 */
struct lem_reader_handle {
	struct lem_stream *s;
	u32 slot;
	u64 id;
};

/*
 * The frame path, in Rust (rs/stream_ring.rs, rs/stream_frame.rs). Each is called with s->lock held,
 * as the C it replaced was.
 */
void lem_stream_fault_locked(struct lem_stream *s, int err);
int lem_stream_drain(struct lem_stream *s);
bool lem_stream_take(struct lem_stream *s, struct lem_job *job);
void lem_stream_finish_locked(struct lem_stream *s, const struct lem_job *job, u32 bytes, u64 sequence,
			      u64 timestamp_ns, u64 end_ns);
bool lem_stream_drop_frame_locked(struct lem_stream *s);
int lem_stream_check_frame(struct lem_stream *s, u32 bytes);
int lem_stream_buffer_addr(struct lem_stream *s, u32 buffer, u64 bytes, u64 *addr);
void lem_stream_enforce(struct lem_stream *s, u64 now);
bool lem_stream_reclaim(struct lem_stream *s, u32 buffer);
int lem_stream_resolve(struct lem_stream *s, u32 buffer, u64 addr, u64 len, struct lem_job_range *out);
void lem_stream_init_ring(struct lem_stream *s);
/* rs/stream_producer.rs, beside the calls of lemnos_internal.h. Never with s->lock held. */
void lem_stream_wake(struct lem_stream *s);

#endif
