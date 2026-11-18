// SPDX-License-Identifier: GPL-2.0
/*
 * Frame streams: the module is the producer of a stream, on the ring the
 * socket protocol's streams already use (lemnos.h; docs/wire.md). It publishes
 * frames, wakes the consumer, takes back buffers no reader holds and collects jobs, and checks
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
 * and nothing else takes a buffer, so a consumer can let go of a buffer's marks
 * and post a job on it without losing it in between. The window before that first job
 * is the consumer's to close, by posting it before frames start (lemnos.h).
 *
 * Only a block's own memory is ever named: the pool and dma-bufs registered
 * here are pinned while the stream holds them, and removed from a stream only
 * when no job uses them.
 *
 * A stream made with LEMNOS_STREAM_CREATE_BUFFERS has no pool: buffer n's
 * memory is a dma-buf of its own (s->f.own[n]), given and swapped with
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
#include "lemnos_stream_priv.h"

/*
 * shmem_file() is an inline in the header through 7.3.3 and an unexported function from 7.3.5 on,
 * so the module cannot link it there (modpost: symbol 'shmem_file' undefined). Its body through
 * 7.3.3 was this test, and shmem_mapping() is exported in both, so the module keeps the test here.
 */
static bool lemnos_shmem_file(struct file *file)
{
	if (!IS_ENABLED(CONFIG_SHMEM))
		return false;
	if (!file || !file->f_mapping)
		return false;
	return shmem_mapping(file->f_mapping);
}

static bool test_stream_source;
module_param(test_stream_source, bool, 0444);
MODULE_PARM_DESC(test_stream_source,
		 "Test only: LEMNOS_STREAM_IOC_TEST_SOURCE produces frames with nothing behind them");

static const struct file_operations lem_stream_fops;
static const struct file_operations lem_reader_fops;
static void lem_stream_put(struct lem_stream *s);
static int lem_region_vmap(struct lem_stream_region *r);

static LIST_HEAD(lem_streams);
static DEFINE_MUTEX(lem_streams_lock);
static u32 lem_stream_count;

/* --- the ring, as seen from here --- */

/*
 * Publishes a head, a tail or the state. A full barrier, not smp_wmb: moving a
 * tail releases a slot this CPU has just read, so the earlier loads must be
 * ordered too, or the consumer can reuse the slot under the read. Not
 * smp_store_release, which doesn't build for a u64 on a 32-bit kernel.
 */
static void lem_ring_write_release(const struct lem_stream *s, size_t at, u64 v)
{
	smp_mb();
	WRITE_ONCE(s->g.ring[at], v);
}

/* The layout of a reader's marks page (marks.rs layout_of): zero in the top byte is layout 1. */
static u64 lem_marks_layout(const u64 *marks)
{
	u64 layout = READ_ONCE(marks[0]) >> LEM_MARKS_LAYOUT_SHIFT;

	return layout ? layout : 1;
}

/* The region a resolved range names: the table, or a buffer's own dma-buf. Lock held. */
static struct lem_stream_region *lem_stream_region_at(struct lem_stream *s, u32 region)
{
	if (region < LEM_STREAM_REGIONS)
		return &s->f.region[region];
	return &s->f.own[region - LEM_STREAM_REGIONS];
}

/*
 * What a block would do: writes the frame, and each range a job named. Only
 * kernel values reach this. No lock: s->lock must not be held across a large
 * copy, and the job's regions are held (users) and live until it completes;
 * teardown cancels this work and waits for it before releasing anything.
 */
static void lem_stream_fill(struct lem_stream *s, const struct lem_job *job, u64 sequence)
{
	u8 *p = s->f.own ? s->f.own[job->buffer].map.vaddr :
			 (u8 *)s->f.region[LEM_STREAM_POOL].map.vaddr + (u64)job->buffer * s->g.stride;
	u64 *w = (u64 *)p;
	size_t bytes = s->g.buffer_bytes;

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

static void lem_stream_source(struct work_struct *work)
{
	struct lem_stream *s = container_of(to_delayed_work(work), struct lem_stream, source);
	struct lem_job job;
	unsigned long flags;
	u64 sequence = 0;
	bool have = false, again;

	spin_lock_irqsave(&s->lock, flags);
	if (s->f.dead || s->f.fault || lem_stream_drain(s)) {
		spin_unlock_irqrestore(&s->lock, flags);
		lem_stream_wake(s);
		return;
	}
	have = lem_stream_take(s, &job);
	if (have)
		sequence = s->f.sequence++;
	else if (lem_stream_drop_frame_locked(s))
		/* A frame with no free buffer is dropped and counted; the source keeps running (docs/streams.md). */
		s->f.sequence++;
	spin_unlock_irqrestore(&s->lock, flags);

	if (have)
		lem_stream_fill(s, &job, sequence);

	spin_lock_irqsave(&s->lock, flags);
	if (have)
		lem_stream_finish_locked(s, &job, s->g.buffer_bytes, sequence, ktime_get_ns(),
					 ktime_get_ns());
	if (have && s->f.left != U32_MAX && s->f.left)
		s->f.left--;
	again = !s->f.dead && !s->f.fault && s->f.left;
	spin_unlock_irqrestore(&s->lock, flags);

	lem_stream_wake(s);
	if (again) {
		unsigned long delay = usecs_to_jiffies(READ_ONCE(s->period_us));

		/* Nothing to fill: don't spin until the consumer gives one back. */
		schedule_delayed_work(&s->source, have ? delay : max(delay, 1UL));
	}
}

/* --- a block as the producer --- */

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

/*
 * Registered regions keep a kernel mapping from here on, so a block can read
 * through lem_stream_range_read(). s->ctl, so never under the block's locks.
 */
int lem_stream_cpu_read_enable(struct lem_stream *s)
{
	int ret = 0;

	mutex_lock(&s->ctl);
	if (s->f.dead) {
		ret = -ENODEV;
		goto out;
	}
	s->cpu_read = true;
	for (u32 i = LEM_STREAM_POOL + 1; i < LEM_STREAM_REGIONS && !ret; i++) {
		struct lem_stream_region *r = &s->f.region[i];

		if (r->buf && iosys_map_is_null(&r->map))
			ret = lem_region_vmap(r);
	}
out:
	mutex_unlock(&s->ctl);
	return ret;
}

/* --- dma-bufs on the block --- */

static void lem_region_unmap(struct lem_stream_region *r)
{
	if (!r->buf)
		return;
	if (!iosys_map_is_null(&r->map))
		dma_buf_vunmap_unlocked(r->buf, &r->map);
	if (r->sgt)
		dma_buf_unmap_attachment_unlocked(r->att, r->sgt, r->dir);
	if (r->att)
		dma_buf_detach(r->buf, r->att);
	dma_buf_put(r->buf);
	memset(r, 0, sizeof(*r));
}

/*
 * Maps `buf`, a reference the caller gives, to the block for `dir`: one contiguous range in its
 * address space, or refused. Either way the reference is the region's, and it is dropped by
 * lem_region_unmap().
 */
static int lem_region_attach(struct device *dev, struct dma_buf *buf, enum dma_data_direction dir,
			     struct lem_stream_region *r)
{
	struct scatterlist *sg;
	dma_addr_t next;
	u64 size = 0, end;
	int ret, i;

	memset(r, 0, sizeof(*r));
	r->buf = buf;
	r->dir = dir;
	r->att = dma_buf_attach(r->buf, dev);
	if (IS_ERR(r->att)) {
		ret = PTR_ERR(r->att);
		r->att = NULL;
		goto fail;
	}
	r->sgt = dma_buf_map_attachment_unlocked(r->att, dir);
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

/* Pins `fd`, mapped to the block: one contiguous range in its address space, or refused. */
static int lem_region_map(struct device *dev, int fd, struct lem_stream_region *r)
{
	struct dma_buf *buf = dma_buf_get(fd);

	if (IS_ERR(buf)) {
		memset(r, 0, sizeof(*r));
		return PTR_ERR(buf);
	}
	return lem_region_attach(dev, buf, DMA_BIDIRECTIONAL, r);
}

/* A region's kernel mapping, for lem_stream_range_read(). Not for iomem: see lem_region_attach. */
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
	for (u32 i = 0; i < LEM_STREAM_REGIONS + (s->f.own ? s->g.buffers : 0); i++) {
		const struct lem_stream_region *other =
			i < LEM_STREAM_REGIONS ? &s->f.region[i] : &s->f.own[i - LEM_STREAM_REGIONS];

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
		if (!s->f.region[i].buf) {
			slot = i;
			break;
		}
	}
	if (slot == LEM_STREAM_REGIONS) {
		spin_unlock_irqrestore(&s->lock, flags);
		lem_region_unmap(&r);
		return -ENOSPC;
	}
	s->f.region[slot] = r;
	spin_unlock_irqrestore(&s->lock, flags);

	a->region = slot;
	a->addr = r.addr;
	a->size = r.size;
	return 0;
}

/*
 * Keeps an unregistered lent region pinned in the pin cache. A full cache gives up its next pin,
 * which is unmapped. Holds s->ctl.
 */
static void lem_stream_pin_keep(struct lem_stream *s, struct lem_stream_region *r)
{
	struct lem_stream_region out = { 0 };
	u32 at = LEM_STREAM_PINS;

	for (u32 i = 0; i < LEM_STREAM_PINS; i++) {
		if (!s->pins[i].buf) {
			at = i;
			break;
		}
	}
	if (at == LEM_STREAM_PINS) {
		at = s->pin_next;
		s->pin_next = (s->pin_next + 1) % LEM_STREAM_PINS;
		out = s->pins[at];
	}
	s->pins[at] = *r;
	memset(r, 0, sizeof(*r));
	lem_region_unmap(&out);
}

/*
 * LEMNOS_STREAM_IOC_REGISTER_LENT: pins a dma-buf the block only reads, and gives its range. A pin
 * kept for this dma-buf and generation is taken back as it is: its reference is the cache's, so
 * the one this call got is dropped and nothing is attached or mapped again. Pins of another
 * generation are no longer this stream's memory and are unmapped first. Holds s->ctl.
 */
static long lem_stream_register_lent(struct lem_stream *s, struct lemnos_stream_register_lent *a)
{
	struct lem_stream_region r;
	struct dma_buf *buf;
	unsigned long flags;
	u32 slot = LEM_STREAM_REGIONS;
	int ret, hit = -1;

	buf = dma_buf_get(a->fd);
	if (IS_ERR(buf))
		return PTR_ERR(buf);
	for (u32 i = 0; i < LEM_STREAM_PINS; i++) {
		struct lem_stream_region stale;

		if (!s->pins[i].buf || s->pins[i].gen == a->generation)
			continue;
		stale = s->pins[i];
		memset(&s->pins[i], 0, sizeof(s->pins[i]));
		lem_region_unmap(&stale);
	}
	for (u32 i = 0; i < LEM_STREAM_PINS; i++) {
		if (s->pins[i].buf == buf && s->pins[i].gen == a->generation)
			hit = (int)i;
	}
	if (hit >= 0) {
		r = s->pins[hit];
		memset(&s->pins[hit], 0, sizeof(s->pins[hit]));
		dma_buf_put(buf);	/* the pin keeps the reference it was mapped with */
	} else {
		ret = lem_region_attach(s->dev, buf, DMA_TO_DEVICE, &r);
		if (ret)
			return ret;
		r.lent = true;
		r.gen = a->generation;
	}
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
		if (!s->f.region[i].buf) {
			slot = i;
			break;
		}
	}
	if (slot == LEM_STREAM_REGIONS) {
		spin_unlock_irqrestore(&s->lock, flags);
		lem_region_unmap(&r);
		return -ENOSPC;
	}
	s->f.region[slot] = r;
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
	if (!s->f.region[region].buf) {
		spin_unlock_irqrestore(&s->lock, flags);
		return -ENOENT;
	}
	if (s->f.region[region].users) {
		spin_unlock_irqrestore(&s->lock, flags);
		return -EBUSY;
	}
	r = s->f.region[region];
	memset(&s->f.region[region], 0, sizeof(s->f.region[region]));
	spin_unlock_irqrestore(&s->lock, flags);
	/* A lent region is kept pinned for its generation; the rest is unmapped now. */
	if (r.lent && !s->f.dead)
		lem_stream_pin_keep(s, &r);
	else
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

	if (!s->f.own)
		return -EOPNOTSUPP;
	if (a->buffer >= s->g.buffers)
		return -EINVAL;
	ret = lem_region_map(s->dev, a->fd, &r);
	if (ret)
		return ret;
	/* Only what the buffer uses is reachable, however big the dma-buf. */
	if (r.size < s->g.buffer_bytes || (r.addr & (LEM_STREAM_DMA_ALIGN - 1))) {
		lem_region_unmap(&r);
		return -EINVAL;
	}
	r.size = s->g.buffer_bytes;
	r.end = r.addr + s->g.buffer_bytes;

	at = &s->f.own[a->buffer];
	spin_lock_irqsave(&s->lock, flags);
	state = s->f.buf[a->buffer].state;
	if (s->f.wedged) {
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
			s->f.buf[a->buffer].state = LEM_BUF_FREE;
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
	if (READ_ONCE(s->f.producer.stop))
		return -EBUSY;
	cancel_delayed_work_sync(&s->source);
	spin_lock_irqsave(&s->lock, flags);
	WRITE_ONCE(s->period_us, a->period_us);
	s->f.left = a->frames;
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
	kvfree(r->seen);
	memset(r, 0, sizeof(*r));
}

/*
 * LEMNOS_STREAM_IOC_READER: pins a reader's marks page and gives the reader a
 * slot, and a file (a.reader_fd) that holds the registration. The page must be
 * a shmem file, sealed against shrinking and growing, and big enough for the
 * buffers. Its pages are mapped read-only and never written. The slot is
 * published under s->lock, where the frame path reads it. Called with s->ctl
 * held, which orders the table's changes.
 */
static long lem_stream_reader_add(struct lem_stream *s, struct lemnos_stream_reader *a)
{
	size_t bytes = (LEM_MARKS_AT + 2 * (size_t)s->g.buffers) * sizeof(u64);
	const unsigned int want = F_SEAL_SHRINK | F_SEAL_GROW;
	struct lem_reader r = {};
	struct lem_reader_handle *h;
	struct file *marks, *file;
	unsigned long flags;
	u64 found;
	u32 slot;
	int fd;
	long ret = -EINVAL;

	for (slot = 0; slot < LEMNOS_STREAM_MAX_READERS; slot++)
		if (!s->f.reader[slot].marks)
			break;
	if (slot == LEMNOS_STREAM_MAX_READERS)
		return -ENOSPC;

	marks = fget(a->fd);
	if (!marks)
		return -EBADF;
	/* The page's size is checked after its layout: a page of an older layout is smaller. */
	if (!lemnos_shmem_file(marks) ||
	    (READ_ONCE(SHMEM_I(file_inode(marks))->seals) & want) != want ||
	    i_size_read(file_inode(marks)) < (loff_t)sizeof(u64))
		goto put;

	r.npages = DIV_ROUND_UP(min_t(loff_t, bytes, i_size_read(file_inode(marks))), PAGE_SIZE);
	r.pages = kcalloc(r.npages, sizeof(*r.pages), GFP_KERNEL);
	if (!r.pages) {
		ret = -ENOMEM;
		goto put;
	}
	for (unsigned int i = 0; i < r.npages; i++) {
		struct page *page = shmem_read_mapping_page(marks->f_mapping, i);

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
	/*
	 * The layout first: a page of another layout is refused as that, with both values in the
	 * caller's argument. unpin unmaps it.
	 */
	found = lem_marks_layout(r.marks);
	if (found != LEM_MARKS_LAYOUT) {
		pr_err("lemnos: a reader's marks page is layout %llu; this build reads layout %llu\n",
		       found, LEM_MARKS_LAYOUT);
		a->refused = LEMNOS_REFUSED_LAYOUT;
		a->layout = LEMNOS_LAYOUT_MARKS;
		a->found = found;
		a->expected = LEM_MARKS_LAYOUT;
		ret = -EPROTO;
		goto unpin;
	}
	if (i_size_read(file_inode(marks)) < (loff_t)bytes)
		goto unpin;
	r.max_held = a->max_held;
	r.max_hold_ns = a->max_hold_ns;
	r.seen = kvcalloc(2 * (size_t)s->g.buffers, sizeof(*r.seen), GFP_KERNEL);
	if (!r.seen) {
		ret = -ENOMEM;
		goto unpin;
	}

	h = kzalloc(sizeof(*h), GFP_KERNEL);
	if (!h) {
		ret = -ENOMEM;
		goto unpin;
	}
	fd = get_unused_fd_flags(O_CLOEXEC);
	if (fd < 0) {
		ret = fd;
		goto free_handle;
	}
	file = anon_inode_getfile("[lemnos-reader]", &lem_reader_fops, h, O_RDWR);
	if (IS_ERR(file)) {
		ret = PTR_ERR(file);
		goto put_fd;
	}

	/* A new slot's cut-off word says nothing about the reader it is given to. */
	lem_ring_write_release(s, s->g.page_at + LEM_POOL_TICKETS + s->g.buffers + slot, 0);
	h->s = s;
	h->slot = slot;
	refcount_inc(&s->refs);
	spin_lock_irqsave(&s->lock, flags);
	r.id = ++s->f.reader_ids;
	h->id = r.id;
	s->f.reader[slot] = r;
	spin_unlock_irqrestore(&s->lock, flags);

	a->slot = slot;
	a->reader_fd = fd;
	fd_install(fd, file);
	fput(marks);
	return 0;

put_fd:
	put_unused_fd(fd);
free_handle:
	kfree(h);
unpin:
	lem_reader_release(&r);
put:
	fput(marks);
	return ret;
}

/*
 * Frees a reader's slot if its handle still holds the registration: its marks
 * stop holding at once, and the slot can be given out again. Takes s->ctl, so
 * the caller must not hold it.
 */
static bool lem_reader_drop(struct lem_reader_handle *h)
{
	struct lem_stream *s = h->s;
	struct lem_reader r = {};
	unsigned long flags;
	bool found = false;

	mutex_lock(&s->ctl);
	spin_lock_irqsave(&s->lock, flags);
	if (h->id && s->f.reader[h->slot].id == h->id) {
		r = s->f.reader[h->slot];
		memset(&s->f.reader[h->slot], 0, sizeof(s->f.reader[h->slot]));
		found = true;
	}
	spin_unlock_irqrestore(&s->lock, flags);
	mutex_unlock(&s->ctl);
	if (found)
		lem_reader_release(&r);
	h->id = 0;
	return found;
}

static int lem_reader_file_release(struct inode *inode, struct file *file)
{
	struct lem_reader_handle *h = file->private_data;
	struct lem_stream *s = h->s;

	lem_reader_drop(h);
	kfree(h);
	lem_stream_put(s);
	return 0;
}

/* LEMNOS_READER_IOC_UNREGISTER: the explicit form of closing the file. */
static long lem_reader_file_ioctl(struct file *file, unsigned int cmd, unsigned long arg)
{
	struct lem_reader_handle *h = file->private_data;

	if (cmd != LEMNOS_READER_IOC_UNREGISTER)
		return -ENOTTY;
	return lem_reader_drop(h) ? 0 : -ENOENT;
}

static const struct file_operations lem_reader_fops = {
	.owner = THIS_MODULE,
	.release = lem_reader_file_release,
	.unlocked_ioctl = lem_reader_file_ioctl,
	.compat_ioctl = compat_ptr_ioctl,
	.llseek = noop_llseek,
};

/*
 * LEMNOS_STREAM_IOC_BELL: the consumer's doorbell, once per stream. The eventfd and the page
 * are both pinned here; the page is mapped read-only and never written. Called with s->ctl
 * held. The eventfd is set before the mapping is published (lem_stream_ring reads the mapping
 * first, with acquire).
 */
static long lem_stream_bell_set(struct lem_stream *s, const struct lemnos_stream_bell *a)
{
	struct eventfd_ctx *efd;
	struct file *file;
	struct page *page;
	unsigned long flags;
	u64 *map;
	long ret = -EINVAL;

	if (s->bell.map)
		return -EBUSY;
	efd = eventfd_ctx_fdget(a->eventfd);
	if (IS_ERR(efd))
		return PTR_ERR(efd);
	file = fget(a->page);
	if (!file) {
		eventfd_ctx_put(efd);
		return -EBADF;
	}
	if (!lemnos_shmem_file(file) || i_size_read(file_inode(file)) < (loff_t)sizeof(u64))
		goto put;
	page = shmem_read_mapping_page(file->f_mapping, 0);
	if (IS_ERR(page)) {
		ret = PTR_ERR(page);
		goto put;
	}
	map = vmap(&page, 1, VM_MAP, PAGE_KERNEL_RO);
	if (!map) {
		put_page(page);
		ret = -ENOMEM;
		goto put;
	}

	spin_lock_irqsave(&s->lock, flags);
	s->bell.efd = efd;
	s->bell.page = page;
	smp_store_release(&s->bell.map, map);
	spin_unlock_irqrestore(&s->lock, flags);
	fput(file);
	return 0;

put:
	fput(file);
	eventfd_ctx_put(efd);
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
		if (s->f.dead)
			ret = -ENODEV;
		else if (s->f.fault)
			ret = s->f.fault;
		else if (s->f.producer.kick)
			kick = s->f.producer.kick;
		else
			ret = lem_stream_drain(s);
		spin_unlock_irqrestore(&s->lock, flags);
		if (kick) {
			ret = kick(s->f.producer.priv);
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
		st.state = s->f.fault ? LEMNOS_RING_STATE_FAULT :
			   s->f.dead ? LEMNOS_RING_STATE_CLOSED : LEMNOS_RING_STATE_OPEN;
		st.fault = -s->f.fault;
		st.frames = s->f.published;
		st.dropped = s->f.dropped;
		st.returned = s->f.returned;
		st.jobs = s->f.jobs;
		spin_unlock_irqrestore(&s->lock, flags);
		return copy_to_user(uarg, &st, sizeof(st)) ? -EFAULT : 0;
	}

	if (cmd == LEMNOS_STREAM_IOC_REGISTER) {
		struct lemnos_stream_register a;

		if (copy_from_user(&a, uarg, sizeof(a)))
			return -EFAULT;
		mutex_lock(&s->ctl);
		ret = s->f.dead ? -ENODEV : s->f.fault ? s->f.fault : lem_stream_register(s, &a);
		mutex_unlock(&s->ctl);
		if (!ret && copy_to_user(uarg, &a, sizeof(a)))
			ret = -EFAULT;
		return ret;
	}

	if (cmd == LEMNOS_STREAM_IOC_REGISTER_LENT) {
		struct lemnos_stream_register_lent a;

		if (copy_from_user(&a, uarg, sizeof(a)))
			return -EFAULT;
		if (a.reserved)
			return -EINVAL;
		mutex_lock(&s->ctl);
		ret = s->f.dead ? -ENODEV : s->f.fault ? s->f.fault : lem_stream_register_lent(s, &a);
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
		ret = s->f.dead ? -ENODEV : lem_stream_unregister(s, a.region);
		mutex_unlock(&s->ctl);
		return ret;
	}

	if (cmd == LEMNOS_STREAM_IOC_BUFFER) {
		struct lemnos_stream_buffer a;

		if (copy_from_user(&a, uarg, sizeof(a)))
			return -EFAULT;
		mutex_lock(&s->ctl);
		ret = s->f.dead ? -ENODEV : s->f.fault ? s->f.fault : lem_stream_set_buffer(s, &a);
		mutex_unlock(&s->ctl);
		if (!ret && copy_to_user(uarg, &a, sizeof(a)))
			ret = -EFAULT;
		return ret;
	}

	/*
	 * Both sizes of the request: the argument is the caller's, so nothing is read or written past
	 * the size its number says, and the refusal fields stay zero for a caller that has none.
	 */
	if (cmd == LEMNOS_STREAM_IOC_READER || cmd == LEMNOS_STREAM_IOC_READER_V0) {
		size_t size = _IOC_SIZE(cmd);
		struct lemnos_stream_reader a = {};

		if (copy_from_user(&a, uarg, size))
			return -EFAULT;
		a.refused = LEMNOS_REFUSED_NONE;
		a.layout = 0;
		a.found = 0;
		a.expected = 0;
		mutex_lock(&s->ctl);
		ret = s->f.dead ? -ENODEV : s->f.fault ? s->f.fault : lem_stream_reader_add(s, &a);
		mutex_unlock(&s->ctl);
		/* A refusal is the one failure that carries an answer back. */
		if ((!ret || ret == -EPROTO) && copy_to_user(uarg, &a, size))
			ret = -EFAULT;
		return ret;
	}

	if (cmd == LEMNOS_STREAM_IOC_BELL) {
		struct lemnos_stream_bell a;

		if (copy_from_user(&a, uarg, sizeof(a)))
			return -EFAULT;
		mutex_lock(&s->ctl);
		ret = s->f.dead ? -ENODEV : s->f.fault ? s->f.fault : lem_stream_bell_set(s, &a);
		mutex_unlock(&s->ctl);
		return ret;
	}

	if (cmd == LEMNOS_STREAM_IOC_TEST_SOURCE) {
		struct lemnos_stream_test_source a;

		if (copy_from_user(&a, uarg, sizeof(a)))
			return -EFAULT;
		mutex_lock(&s->ctl);
		ret = s->f.dead ? -ENODEV : s->f.fault ? s->f.fault : lem_stream_test_source(s, &a);
		mutex_unlock(&s->ctl);
		return ret;
	}

	/* Under s->ctl, because a stream that's over may have lost its block. */
	mutex_lock(&s->ctl);
	if (!s->f.producer.ioctl)
		ret = -ENOTTY;
	else
		ret = s->f.dead ? -ENODEV : s->f.producer.ioctl(s->f.producer.priv, cmd, uarg);
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
	/* Frames come on the doorbell (lem_stream_wake), not on this file: only the end is readable here. */
	if (s->f.dead || s->f.fault)
		mask = EPOLLIN | EPOLLRDNORM | EPOLLHUP;
	spin_unlock_irqrestore(&s->lock, flags);
	return mask;
}

static int lem_stream_mmap(struct file *file, struct vm_area_struct *vma)
{
	struct lem_stream *s = file->private_data;
	unsigned long len = vma->vm_end - vma->vm_start;

	if (vma->vm_pgoff && s->f.producer.mmap)
		return s->f.producer.mmap(s->f.producer.priv, vma);
	if (!(vma->vm_flags & VM_SHARED) || vma->vm_pgoff || len != PAGE_ALIGN(s->g.ring_bytes))
		return -EINVAL;
	if (vma->vm_flags & VM_EXEC)
		return -EPERM;
	vm_flags_clear(vma, VM_MAYEXEC);
	return remap_vmalloc_range(vma, s->g.ring, 0);
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
	if (s->f.dead) {
		mutex_unlock(&s->ctl);
		return;
	}
	spin_lock_irqsave(&s->lock, flags);
	s->f.dead = true;
	producer = s->f.producer;
	s->f.producer.stop = NULL;
	if (!s->f.fault)
		lem_ring_write_release(s, s->g.w_state, state);
	spin_unlock_irqrestore(&s->lock, flags);

	cancel_delayed_work_sync(&s->source);

	/* The block's DMA stops here, before anything it could write to is released. */
	if (producer.stop)
		producer.stop(producer.priv, s);

	spin_lock_irqsave(&s->lock, flags);
	wedged = s->f.wedged;
	spin_unlock_irqrestore(&s->lock, flags);
	if (wedged) {
		/* DMA may still run: keep every pin for good. Nothing else is safe. */
		lem_stream_wake(s);
		mutex_unlock(&s->ctl);
		return;
	}

	spin_lock_irqsave(&s->lock, flags);
	for (u32 i = 0; i < LEM_STREAM_REGIONS + (s->f.own ? s->g.buffers : 0); i++) {
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
	if (s->f.producer.release)
		s->f.producer.release(s->f.producer.priv);
	if (s->doorbell)
		eventfd_ctx_put(s->doorbell);
	if (!s->f.wedged) {
		for (u32 i = 0; i < LEM_STREAM_REGIONS; i++)
			lem_region_unmap(&s->f.region[i]);
		for (u32 i = 0; i < LEM_STREAM_PINS; i++)
			lem_region_unmap(&s->pins[i]);
		for (u32 i = 0; s->f.own && i < s->g.buffers; i++)
			lem_region_unmap(&s->f.own[i]);
	}
	for (u32 r = 0; r < LEMNOS_STREAM_MAX_READERS; r++)
		lem_reader_release(&s->f.reader[r]);
	if (s->bell.map)
		vunmap(s->bell.map);
	if (s->bell.page)
		put_page(s->bell.page);
	if (s->bell.efd)
		eventfd_ctx_put(s->bell.efd);
	kvfree(s->f.own);
	kvfree(s->f.queue);
	kvfree(s->f.buf);
	vfree(s->g.ring);
	if (!s->f.wedged)
		put_device(s->dev);
	mutex_destroy(&s->ctl);
	kfree(s);
}

static void lem_stream_put(struct lem_stream *s)
{
	if (refcount_dec_and_test(&s->refs))
		lem_stream_free(s);
}

static int lem_stream_release(struct inode *inode, struct file *file)
{
	struct lem_stream *s = file->private_data;

	mutex_lock(&lem_streams_lock);
	list_del(&s->node);
	lem_stream_count--;
	mutex_unlock(&lem_streams_lock);
	lem_stream_shutdown(s, LEMNOS_RING_STATE_CLOSED);
	lem_stream_put(s);
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

	if (c->flags & ~LEMNOS_STREAM_CREATE_BUFFERS)
		return -EINVAL;
	/* The frame ring is the shared-memory ring, which needs a slot per frame in flight and a second for the reader. */
	if (c->frame_slots < 2)
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

/* Called under lem_blocks_sem for reading: the block can't go while this runs. */
static long lem_stream_create_with(struct device *dev, struct lemnos_stream_create *c,
				   const struct lem_stream_producer *producer,
				   struct lem_stream **out)
{
	struct lem_stream *s;
	struct file *file;
	size_t control, ring_words;
	u64 pool_bytes;
	int fd, ret;

	BUILD_BUG_ON(IS_ENABLED(CONFIG_CPU_BIG_ENDIAN));
	BUILD_BUG_ON(sizeof(struct lemnos_stream_job) % 8);
	BUILD_BUG_ON(LEM_SHM_ENTRY_BYTES % 8 || LEM_SHM_SLOT_WORDS != 10);
	BUILD_BUG_ON(sizeof(struct lemnos_stream_reader_v0) != 24);
	BUILD_BUG_ON(sizeof(struct lemnos_stream_reader) != 48);
	BUILD_BUG_ON(offsetof(struct lemnos_stream_reader, refused) != sizeof(struct lemnos_stream_reader_v0));
	BUILD_BUG_ON(offsetof(struct lemnos_stream_reader, max_hold_ns) != offsetof(struct lemnos_stream_reader_v0, max_hold_ns));
	BUILD_BUG_ON(sizeof(struct lemnos_stream_bell) != 8);
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
	refcount_set(&s->refs, 1);
	s->dev = get_device(dev);
	mutex_init(&s->ctl);
	spin_lock_init(&s->lock);
	init_waitqueue_head(&s->wait);
	INIT_DELAYED_WORK(&s->source, lem_stream_source);
	INIT_WORK(&s->gone, lem_stream_gone);

	s->g.buffers = c->buffers;
	s->g.buffer_bytes = c->buffer_bytes;
	s->g.frame_slots = c->frame_slots;
	s->g.job_slots = c->job_slots;
	s->g.job_bytes = c->job_bytes;
	s->g.stride = ALIGN((u64)c->buffer_bytes, LEMNOS_STREAM_BUFFER_ALIGN);
	s->f.newest = U32_MAX;
	s->g.job_words = 1 + DIV_ROUND_UP(s->g.job_bytes, 8);
	/*
	 * The words, in order: the frame ring (header, lane head, index, slots), the pool page
	 * (magic and shape, tickets, cut-off words), then the job block: its head and tail, and the
	 * job slots. A buffer goes back by its marks, so nothing returns through the ring.
	 */
	s->g.shm_slots_at = LEM_SHM_INDEX + s->g.frame_slots;
	s->g.shm_total = s->g.shm_slots_at + (size_t)s->g.frame_slots * LEM_SHM_SLOT_WORDS;
	s->g.page_at = s->g.shm_total;
	control = s->g.page_at + LEM_POOL_TICKETS + s->g.buffers + LEM_POOL_READERS;
	s->g.w_state = LEM_SHM_W_END;
	s->g.w_jobs_head = control + LEM_JOB_HEAD;
	s->g.w_jobs_tail = control + LEM_JOB_TAIL;
	s->g.jobs_at = control + LEM_JOB_SLOTS;
	ring_words = s->g.jobs_at + (size_t)s->g.job_slots * s->g.job_words;
	s->g.ring_bytes = ring_words * sizeof(u64);
	pool_bytes = (u64)s->g.buffers * s->g.stride;

	ret = -ENOMEM;
	s->f.buf = kvcalloc(s->g.buffers, sizeof(*s->f.buf), GFP_KERNEL);
	s->f.queue = kvcalloc(s->g.job_slots, sizeof(*s->f.queue), GFP_KERNEL);
	if (!s->f.buf || !s->f.queue)
		goto free;
	if (c->flags & LEMNOS_STREAM_CREATE_BUFFERS) {
		s->f.own = kvcalloc(s->g.buffers, sizeof(*s->f.own), GFP_KERNEL);
		if (!s->f.own)
			goto free;
		for (u32 i = 0; i < s->g.buffers; i++)
			s->f.buf[i].state = LEM_BUF_ABSENT;
	}
	s->g.ring = vmalloc_user(PAGE_ALIGN(s->g.ring_bytes));
	if (!s->g.ring)
		goto free;

	if (c->doorbell_fd != -1) {
		s->doorbell = eventfd_ctx_fdget(c->doorbell_fd);
		if (IS_ERR(s->doorbell)) {
			ret = PTR_ERR(s->doorbell);
			s->doorbell = NULL;
			goto free;
		}
	}
	if (!s->f.own) {
		ret = lem_region_map(dev, c->pool_fd, &s->f.region[LEM_STREAM_POOL]);
		if (ret)
			goto free;
		/* Only what the buffers use is reachable, however big the dma-buf. */
		if (s->f.region[LEM_STREAM_POOL].size < pool_bytes) {
			ret = -EINVAL;
			goto free;
		}
		s->f.region[LEM_STREAM_POOL].size = pool_bytes;
		s->f.region[LEM_STREAM_POOL].end = s->f.region[LEM_STREAM_POOL].addr + pool_bytes;
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

	c->ring_bytes = s->g.ring_bytes;
	c->pool_addr = s->f.region[LEM_STREAM_POOL].addr;
	c->fd = fd;
	if (out)
		*out = s;
	fd_install(fd, file);
	return 0;
free:
	/* It never existed: priv is still the caller's. */
	s->f.producer.release = NULL;
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
