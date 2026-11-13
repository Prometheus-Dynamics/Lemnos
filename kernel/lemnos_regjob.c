// SPDX-License-Identifier: GPL-2.0
/*
 * Register jobs (lemnos_regjob.h): a block whose register writes can make it
 * reach memory is run by the module from a list of register values and the
 * caller's dma-bufs. What a caller may write, which registers take addresses
 * and how far the block reaches from each are the block's profile
 * (lemnos_regjob_<block>.c); this file is the part every profile shares.
 *
 * A job is copied into kernel memory once and only the copy is checked and
 * written. The module writes every address register itself, from a buffer
 * the session holds, after proving the job's offset and length lie inside it
 * and cover what the profile says the block will reach. Nothing a caller
 * writes reaches an address register, and nothing is clamped: a job that
 * fails a check is refused before any register is written.
 *
 * Buffer lifetime. A buffer is pinned (attached and mapped for the block)
 * from import to release. While the block may be using it, release is
 * refused: during a job, and, for a block that keeps using its addresses
 * after a job completes (a capture path), until a later job has completed
 * with other addresses or the block has been reset. Closing the session or
 * unbinding the block resets a block that may still be using a buffer before
 * anything is released. A block that can't be reset can't be shown to have
 * stopped: it is wedged, and what it may still use stays pinned for good.
 *
 * A job started without waiting (LEMNOS_REGJOB_RUN_NOWAIT) is collected by
 * LEMNOS_REGJOB_IOC_WAIT, which never sleeps: the deadline is checked there,
 * and a job past it is handled as a timeout. Until it is collected no other
 * job starts. The registers a job reads back are read in the handler, at the
 * job's end.
 *
 * Two roles. The frames session runs jobs; the configuration session (on a
 * profile that has one) writes and reads registers that take no address, at
 * once, and takes the block's other interrupt as events. Every register write
 * of either role is made under rj->lock, and on a profile that keeps bits a
 * register is read, merged with the writer's bits and written back there, so
 * a register both roles write (the K230 ISP's isp_ctrl) keeps each role's
 * bits. The handlers write only registers no table lists (interrupt status and
 * clear, the module's switch of a capture path), so they never race a merge.
 *
 * Locking: rj->lock (a mutex) covers the sessions, their buffers, the live
 * sets, a job's start and collection and every register write; rj->irq_lock
 * (a spinlock) the completion and the event queue, and is the only lock the
 * handlers take.
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#include <linux/anon_inodes.h>
#include <linux/bitops.h>
#include <linux/clk.h>
#include <linux/delay.h>
#include <linux/dma-buf.h>
#include <linux/dma-mapping.h>
#include <linux/err.h>
#include <linux/fs.h>
#include <linux/interrupt.h>
#include <linux/io.h>
#include <linux/kref.h>
#include <linux/module.h>
#include <linux/mutex.h>
#include <linux/overflow.h>
#include <linux/platform_device.h>
#include <linux/reset.h>
#include <linux/scatterlist.h>
#include <linux/timekeeping.h>
#include <linux/slab.h>
#include <linux/spinlock.h>
#include <linux/uaccess.h>
#include <linux/wait.h>

#include "lemnos_internal.h"
#include "lemnos_regjob_priv.h"

static char *a733_ve = "";
module_param(a733_ve, charp, 0444);
MODULE_PARM_DESC(a733_ve,
		 "Compatibles of the blocks run as the A733 video engine's encoder through register "
		 "jobs (vendor,name pairs); they must also be in owned=");

static char *k230_isp = "";
module_param(k230_isp, charp, 0444);
MODULE_PARM_DESC(k230_isp,
		 "Compatibles of the blocks run as the K230's ISP main path through register jobs "
		 "(vendor,name pairs); they must also be in owned=");

static bool test_rj_fake_regs;
module_param(test_rj_fake_regs, bool, 0444);
MODULE_PARM_DESC(test_rj_fake_regs,
		 "Test only: a register-job block's register window is ordinary memory (the test harness)");

struct lrj_buf {
	struct dma_buf *buf;		/* NULL: the index is free */
	struct dma_buf_attachment *att;
	struct sg_table *sgt;
	u64 addr;
	u64 bytes;
};

struct lrj_session;

struct lem_rj {
	struct device *dev;
	const struct lrj_profile *p;
	void __iomem *regs;
	struct reset_control *rst;	/* NULL: the block has none, and a timeout wedges it */
	int irq;
	struct kref ref;

	struct mutex lock;		/* everything to irq_lock */
	struct lrj_session *session;	/* the frames role */
	struct lrj_session *config;	/* the configuration role (written under irq_lock too) */
	int event_irq;			/* the configuration role's interrupt; 0: none of its own */
	bool gone;			/* unbound: nothing touches the block again */
	bool wedged;
	bool module_held;
	u64 cur, prev;			/* buffer bits the block may be using */
	u64 jobs, refused, timeouts, resets;

	/* A job started and not yet collected. */
	bool pending;
	u32 pending_reads;
	unsigned long deadline;		/* jiffies */
	u32 timeout_ms;

	spinlock_t irq_lock;		/* the completion */
	bool armed, done;
	bool latch_pending;		/* continuous: switch to the job's addresses at its end */
	struct lrj_session *job;	/* the session whose job is armed */
	u32 status;
	u64 done_ns;			/* when the handler completed it */
	wait_queue_head_t wq;
};

/*
 * One opener's session: its buffers, and room for one job's kernel copy; or,
 * for the configuration role, its event queue.
 */
struct lrj_session {
	struct lem_rj *rj;
	bool config;			/* the configuration role */
	/* LEMNOS_REGJOB_IOC_WRITE's reads: not a pending job's, which the handler fills. */
	u32 now_reads[LEMNOS_REGJOB_MAX_READS];
	u32 now_values[LEMNOS_REGJOB_MAX_READS];
	/* The configuration role's events, under irq_lock. */
	struct lemnos_regjob_event ev[LEMNOS_REGJOB_EVENTS];
	u32 ev_head, ev_count, ev_lost;
	struct lrj_buf buf[LEMNOS_REGJOB_MAX_BUFFERS];
	struct lemnos_regjob_write w[LEMNOS_REGJOB_MAX_WRITES];
	struct lemnos_regjob_addr a[LEMNOS_REGJOB_MAX_ADDRS];
	u32 reads[LEMNOS_REGJOB_MAX_READS];
	u32 values[LEMNOS_REGJOB_MAX_READS];
	struct lrj_ctx ctx;
	u64 need[LEMNOS_REGJOB_MAX_ADDRS];
};

u32 lrj_rd(struct lem_rj *rj, u32 offset)
{
	return readl(rj->regs + offset);
}

void lrj_wr(struct lem_rj *rj, u32 offset, u32 value)
{
	writel(value, rj->regs + offset);
}

int lrj_value(const struct lrj_ctx *c, u32 offset, u32 *value)
{
	u32 seen = 0;

	for (u32 i = 0; i < c->nw; i++) {
		if (c->w[i].offset != offset)
			continue;
		if (seen++)
			return -EINVAL;
		*value = c->w[i].value;
	}
	return seen ? 0 : -EINVAL;
}

static void lrj_free(struct kref *ref)
{
	kfree(container_of(ref, struct lem_rj, ref));
}

/* --- buffers --- */

static void lrj_buf_free(struct lrj_buf *b)
{
	dma_buf_unmap_attachment_unlocked(b->att, b->sgt, DMA_BIDIRECTIONAL);
	dma_buf_detach(b->buf, b->att);
	dma_buf_put(b->buf);
	memset(b, 0, sizeof(*b));
}

/* rj->lock held. */
static long lrj_import(struct lrj_session *s, struct lemnos_regjob_import *imp)
{
	struct lem_rj *rj = s->rj;
	struct lrj_buf *b = NULL;
	struct scatterlist *sg;
	dma_addr_t next;
	u64 bytes = 0, end;
	int ret, i;
	u32 n;

	for (n = 0; n < LEMNOS_REGJOB_MAX_BUFFERS; n++) {
		if (!s->buf[n].buf) {
			b = &s->buf[n];
			break;
		}
	}
	if (!b)
		return -ENOSPC;
	b->buf = dma_buf_get(imp->fd);
	if (IS_ERR(b->buf)) {
		ret = PTR_ERR(b->buf);
		b->buf = NULL;
		return ret;
	}
	b->att = dma_buf_attach(b->buf, rj->dev);
	if (IS_ERR(b->att)) {
		ret = PTR_ERR(b->att);
		goto put;
	}
	b->sgt = dma_buf_map_attachment_unlocked(b->att, DMA_BIDIRECTIONAL);
	if (IS_ERR(b->sgt)) {
		ret = PTR_ERR(b->sgt);
		goto detach;
	}
	/* One contiguous range in the block's address space, or refused. */
	next = sg_dma_address(b->sgt->sgl);
	for_each_sgtable_dma_sg(b->sgt, sg, i) {
		if (sg_dma_address(sg) != next) {
			ret = -EINVAL;
			goto unmap;
		}
		next += sg_dma_len(sg);
		bytes += sg_dma_len(sg);
	}
	b->addr = sg_dma_address(b->sgt->sgl);
	b->bytes = bytes;
	if (!bytes || check_add_overflow(b->addr, bytes, &end)) {
		ret = -EINVAL;
		goto unmap;
	}
	imp->index = n;
	imp->bytes = b->bytes;
	imp->addr = b->addr;
	return 0;
unmap:
	dma_buf_unmap_attachment_unlocked(b->att, b->sgt, DMA_BIDIRECTIONAL);
detach:
	dma_buf_detach(b->buf, b->att);
put:
	dma_buf_put(b->buf);
	memset(b, 0, sizeof(*b));
	return ret;
}

/* rj->lock held. */
static long lrj_release_buf(struct lrj_session *s, u32 index)
{
	struct lem_rj *rj = s->rj;

	if (index >= LEMNOS_REGJOB_MAX_BUFFERS || !s->buf[index].buf)
		return -ENOENT;
	if ((rj->cur | rj->prev) & BIT_ULL(index))
		return -EBUSY;
	lrj_buf_free(&s->buf[index]);
	return 0;
}

/* --- the configuration role's events --- */

/* Queues an event for the configuration session, if one is open. irq_lock held. */
static void lrj_event_push(struct lem_rj *rj, u32 status, u64 now_ns)
{
	struct lrj_session *c = rj->config;
	struct lemnos_regjob_event *e;

	if (!c)
		return;
	if (c->ev_count == LEMNOS_REGJOB_EVENTS) {
		c->ev_lost++;
		return;
	}
	e = &c->ev[(c->ev_head + c->ev_count) % LEMNOS_REGJOB_EVENTS];
	e->status = status;
	e->lost = c->ev_lost;
	e->timestamp_ns = now_ns;
	c->ev_lost = 0;
	c->ev_count++;
}

/* The next queued event: 0, or -EAGAIN when none is. */
static long lrj_event_take(struct lrj_session *s, struct lemnos_regjob_event *e)
{
	struct lem_rj *rj = s->rj;
	unsigned long flags;
	long ret = -EAGAIN;

	spin_lock_irqsave(&rj->irq_lock, flags);
	if (rj->gone || rj->config != s) {
		ret = -ENODEV;
	} else if (s->ev_count) {
		*e = s->ev[s->ev_head];
		s->ev_head = (s->ev_head + 1) % LEMNOS_REGJOB_EVENTS;
		s->ev_count--;
		ret = 0;
	}
	spin_unlock_irqrestore(&rj->irq_lock, flags);
	return ret;
}

/* The events' interrupt, after anything turned the block's interrupts off. irq_lock held. */
static void lrj_events_on(struct lem_rj *rj)
{
	if (rj->config && !rj->gone)
		rj->p->event_arm(rj, true);
}

/* --- stopping the block --- */

/* The block can't be shown to have stopped: what it may use stays pinned for good. */
static void lrj_wedge(struct lem_rj *rj)
{
	if (!rj->wedged)
		dev_crit(rj->dev,
			 "block did not finish and has no reset: its buffers stay held until a reboot\n");
	rj->wedged = true;
	if (!rj->module_held) {
		rj->module_held = true;
		__module_get(THIS_MODULE);
	}
}

/*
 * Stops whatever the block is doing: interrupts off, the reset line pulsed,
 * the block's own reset sequence. After it nothing is live. rj->lock held.
 */
static void lrj_stop(struct lem_rj *rj)
{
	unsigned long flags;
	bool reset = false;
	int ret;

	spin_lock_irqsave(&rj->irq_lock, flags);
	rj->armed = false;
	rj->latch_pending = false;
	rj->job = NULL;
	rj->p->quiet(rj);
	spin_unlock_irqrestore(&rj->irq_lock, flags);
	rj->pending = false;
	if (!rj->rst) {
		lrj_wedge(rj);
		goto events;
	}
	ret = reset_control_reset(rj->rst);
	if (ret == -ENOTSUPP || ret == -EOPNOTSUPP) {
		ret = reset_control_assert(rj->rst);
		if (!ret) {
			udelay(10);
			ret = reset_control_deassert(rj->rst);
		}
	}
	if (ret) {
		dev_err(rj->dev, "reset failed (%d)\n", ret);
		lrj_wedge(rj);
		goto events;
	}
	rj->resets++;
	rj->p->reset_regs(rj);
	rj->p->quiet(rj);
	rj->cur = 0;
	rj->prev = 0;
	reset = true;
events:
	/* The configuration role keeps its interrupt, and learns that its registers are gone. */
	spin_lock_irqsave(&rj->irq_lock, flags);
	if (rj->config) {
		lrj_events_on(rj);
		if (reset)
			lrj_event_push(rj, LEMNOS_REGJOB_EVENT_RESET, ktime_get_ns());
	}
	spin_unlock_irqrestore(&rj->irq_lock, flags);
}

/*
 * The session ends: the block is stopped if it may be using a buffer, and
 * every buffer is released except, on a wedged block, the ones it may use.
 * rj->lock held.
 */
static void lrj_end_session(struct lem_rj *rj, struct lrj_session *s)
{
	if (!rj->wedged && (rj->cur | rj->prev))
		lrj_stop(rj);
	for (u32 n = 0; n < LEMNOS_REGJOB_MAX_BUFFERS; n++) {
		if (!s->buf[n].buf)
			continue;
		if (rj->wedged && ((rj->cur | rj->prev) & BIT_ULL(n)))
			continue;	/* kept, with its references, until a reboot */
		lrj_buf_free(&s->buf[n]);
	}
}

/* --- a job --- */

static u32 lrj_mask(const struct lrj_plain *table, u32 n, u32 offset, bool *listed)
{
	for (u32 i = 0; i < n; i++) {
		const struct lrj_plain *r = &table[i];

		if (offset >= r->offset && (offset - r->offset) / 4 < r->count) {
			*listed = true;
			return r->mask;
		}
	}
	*listed = false;
	return 0;
}

/* Every write in a role's table, with only its bits, on a register boundary in the window. */
static int lrj_check_writes(const struct lrj_profile *p, const struct lrj_plain *table, u32 n,
			    const struct lemnos_regjob_write *w, u32 nw)
{
	for (u32 i = 0; i < nw; i++) {
		bool listed;
		u32 mask;

		if (w[i].offset % 4 || w[i].offset > p->window - 4)
			return -EINVAL;
		mask = lrj_mask(table, n, w[i].offset, &listed);
		if (!listed || (w[i].value & ~mask))
			return -EPERM;
	}
	return 0;
}

/*
 * One checked write. On a profile that keeps bits, the bits outside the
 * writer's mask keep their value. rj->lock held: every merge is under it.
 */
static void lrj_write_checked(struct lem_rj *rj, const struct lrj_plain *table, u32 n,
			      const struct lemnos_regjob_write *w)
{
	bool listed;
	u32 mask = lrj_mask(table, n, w->offset, &listed);
	u32 value = w->value;

	if (rj->p->keep_bits && mask != U32_MAX)
		value |= lrj_rd(rj, w->offset) & ~mask;
	lrj_wr(rj, w->offset, value);
}

/* Every check, on the kernel's copy, before any register is written. rj->lock held. */
static int lrj_check(struct lem_rj *rj, struct lrj_session *s, const struct lemnos_regjob_run *r,
		     u64 *used)
{
	const struct lrj_profile *p = rj->p;
	struct lrj_ctx *c = &s->ctx;
	u32 seen = 0;
	int ret;

	BUILD_BUG_ON(LEMNOS_REGJOB_MAX_BUFFERS > 64);
	BUILD_BUG_ON(LEMNOS_REGJOB_MAX_ADDRS > 32);

	ret = lrj_check_writes(p, p->plain, p->nplain, s->w, r->nwrites);
	if (ret)
		return ret;

	if (r->naddrs != p->nslots)
		return -EINVAL;
	*used = 0;
	for (u32 i = 0; i < r->naddrs; i++) {
		const struct lemnos_regjob_addr *a = &s->a[i];
		const struct lrj_buf *b;
		u64 end, addr, top;
		u32 k;

		for (k = 0; k < p->nslots; k++)
			if (p->slots[k].reg == a->reg)
				break;
		if (k == p->nslots || (seen & BIT(k)))
			return -EINVAL;
		seen |= BIT(k);
		if (a->buffer >= LEMNOS_REGJOB_MAX_BUFFERS || !s->buf[a->buffer].buf)
			return -ENOENT;
		b = &s->buf[a->buffer];
		if (!a->bytes)
			return -EINVAL;
		if (check_add_overflow(a->offset, a->bytes, &end) || end > b->bytes)
			return -EFAULT;
		if (check_add_overflow(b->addr, a->offset, &addr) ||
		    check_add_overflow(addr, a->bytes, &top))
			return -EFAULT;
		if (addr & (p->slots[k].align - 1))
			return -EINVAL;
		if (top > p->addr_limit)
			return -EINVAL;	/* the register can't hold it */
		c->addr[k] = addr;
		c->bytes[k] = a->bytes;
		*used |= BIT_ULL(a->buffer);
	}

	for (u32 i = 0; i < r->nreads; i++)
		if (s->reads[i] % 4 || s->reads[i] > p->window - 4)
			return -EINVAL;
	if (!p->start_ok(r->start))
		return -EPERM;

	c->w = s->w;
	c->nw = r->nwrites;
	memset(s->need, 0, sizeof(s->need));
	ret = p->check(c, s->need);
	if (ret)
		return ret;
	for (u32 k = 0; k < p->nslots; k++)
		if (s->need[k] > c->bytes[k])
			return -EFAULT;
	return 0;
}

static bool lrj_done(struct lem_rj *rj)
{
	unsigned long flags;
	bool done;

	spin_lock_irqsave(&rj->irq_lock, flags);
	done = rj->done;
	spin_unlock_irqrestore(&rj->irq_lock, flags);
	return done;
}

/* Writes the checked job and starts it. rj->lock held. */
static void lrj_start(struct lem_rj *rj, struct lrj_session *s, const struct lemnos_regjob_run *r,
		      u64 used)
{
	const struct lrj_profile *p = rj->p;
	const struct lrj_ctx *c = &s->ctx;
	unsigned long flags;
	bool idle = !(rj->cur | rj->prev);

	/* The addresses first, from the copy: nothing the caller wrote reaches them. */
	for (u32 k = 0; k < p->nslots; k++)
		lrj_wr(rj, p->slots[k].reg, lower_32_bits(c->addr[k]));
	p->derive(rj, c);
	for (u32 i = 0; i < r->nwrites; i++)
		lrj_write_checked(rj, p->plain, p->nplain, &s->w[i]);

	/*
	 * What the jobs before left on a block that keeps going stays live until
	 * this one completes; on any other block nothing is left by now.
	 */
	rj->prev |= rj->cur;
	rj->cur = used;
	rj->pending = true;
	rj->pending_reads = r->nreads;
	rj->timeout_ms = r->timeout_ms;
	rj->deadline = jiffies + msecs_to_jiffies(r->timeout_ms);

	spin_lock_irqsave(&rj->irq_lock, flags);
	rj->done = false;
	rj->status = 0;
	rj->job = s;
	rj->latch_pending = false;
	if (p->continuous) {
		/* An idle block takes the addresses now; a running one at its frame's end. */
		if (idle)
			rj->status = p->latch(rj, c);
		else
			rj->latch_pending = true;
	}
	rj->armed = true;
	p->arm(rj);
	spin_unlock_irqrestore(&rj->irq_lock, flags);
	lrj_wr(rj, p->start_reg, r->start);
}

/*
 * The started job's end, if it has come: 1 with *status, 0 while it runs, or
 * -ETIMEDOUT once it is past its deadline (the block is reset). rj->lock held.
 */
static int lrj_collect(struct lem_rj *rj, u32 *status, u64 *done_ns)
{
	unsigned long flags;
	bool done;

	spin_lock_irqsave(&rj->irq_lock, flags);
	done = rj->done;
	if (done) {
		rj->armed = false;
		rj->job = NULL;
		*status = rj->status;
		*done_ns = rj->done_ns;
	}
	spin_unlock_irqrestore(&rj->irq_lock, flags);

	if (!done) {
		if (time_before(jiffies, rj->deadline))
			return 0;
		rj->timeouts++;
		dev_warn(rj->dev, "job did not finish in %u ms: resetting the block\n",
			 rj->timeout_ms);
		lrj_stop(rj);
		return -ETIMEDOUT;
	}
	rj->pending = false;
	rj->jobs++;
	if (!rj->p->continuous)
		rj->cur = 0;	/* the block is done with every buffer */
	else if (!(*status & LEMNOS_REGJOB_STATUS_UNLATCHED))
		rj->prev = 0;	/* this job's addresses are the ones in use now */
	/* Not taken: the block may use either set, so both stay live. */
	return 1;
}

static long lrj_run(struct lrj_session *s, struct lemnos_regjob_run *r)
{
	struct lem_rj *rj = s->rj;
	u64 used = 0, done_ns;
	long ret;

	mutex_lock(&rj->lock);
	if (rj->gone || rj->session != s) {
		ret = -ENODEV;
		goto out;
	}
	if (rj->wedged) {
		ret = -EIO;
		goto out;
	}
	if (rj->pending) {
		ret = -EBUSY;
		goto out;
	}
	if (r->nwrites > LEMNOS_REGJOB_MAX_WRITES || r->naddrs > LEMNOS_REGJOB_MAX_ADDRS ||
	    r->nreads > LEMNOS_REGJOB_MAX_READS || !r->timeout_ms ||
	    r->timeout_ms > LEMNOS_REGJOB_MAX_TIMEOUT_MS ||
	    (r->flags & ~LEMNOS_REGJOB_RUN_NOWAIT) || r->reserved)
		ret = -EINVAL;
	/* The copy: what is checked and what runs. */
	else if (copy_from_user(s->w, u64_to_user_ptr(r->writes), r->nwrites * sizeof(s->w[0])) ||
		 copy_from_user(s->a, u64_to_user_ptr(r->addrs), r->naddrs * sizeof(s->a[0])) ||
		 copy_from_user(s->reads, u64_to_user_ptr(r->reads),
				r->nreads * sizeof(s->reads[0])))
		ret = -EFAULT;
	else
		ret = lrj_check(rj, s, r, &used);
	if (ret) {
		rj->refused++;
		goto out;
	}
	lrj_start(rj, s, r, used);
	if (r->flags & LEMNOS_REGJOB_RUN_NOWAIT) {
		ret = 0;
		goto out;
	}
	/* Not interruptible: until it ends or is reset, the block is using the buffers. */
	wait_event_timeout(rj->wq, lrj_done(rj), msecs_to_jiffies(r->timeout_ms));
	if (!lrj_done(rj))
		rj->deadline = jiffies;	/* its time is up */
	ret = lrj_collect(rj, &r->status, &done_ns);
	if (ret == 1)
		ret = copy_to_user(u64_to_user_ptr(r->values), s->values,
				   r->nreads * sizeof(s->values[0])) ? -EFAULT : 0;
	else if (!ret)
		ret = -ETIMEDOUT;	/* unreachable: a deadline in the past times out */
out:
	mutex_unlock(&rj->lock);
	return ret;
}

static long lrj_wait(struct lrj_session *s, struct lemnos_regjob_wait *w)
{
	struct lem_rj *rj = s->rj;
	long ret;

	mutex_lock(&rj->lock);
	if (rj->gone || rj->session != s)
		ret = -ENODEV;
	else if (!rj->pending)
		ret = -EINVAL;
	else if (w->nreads != rj->pending_reads)
		ret = -EINVAL;
	else
		ret = lrj_collect(rj, &w->status, &w->timestamp_ns);
	if (ret == 1)
		ret = copy_to_user(u64_to_user_ptr(w->values), s->values,
				   w->nreads * sizeof(s->values[0])) ? -EFAULT : 0;
	else if (!ret)
		ret = -EAGAIN;
	mutex_unlock(&rj->lock);
	return ret;
}

/* Whether `s` is still its block's session in its role. rj->lock held. */
static bool lrj_current(struct lrj_session *s)
{
	struct lem_rj *rj = s->rj;

	return !rj->gone && (s->config ? rj->config : rj->session) == s;
}

/* LEMNOS_REGJOB_IOC_WRITE: checked writes, then reads, at once. */
static long lrj_regs(struct lrj_session *s, struct lemnos_regjob_regs *r)
{
	struct lem_rj *rj = s->rj;
	const struct lrj_profile *p = rj->p;
	const struct lrj_plain *table = s->config ? p->config : p->plain;
	u32 n = s->config ? p->nconfig : p->nplain;
	long ret;

	if (!p->nconfig)
		return -EINVAL;	/* no second role: the block's registers move with its jobs */
	if (r->nwrites > LEMNOS_REGJOB_MAX_WRITES || r->nreads > LEMNOS_REGJOB_MAX_READS)
		return -EINVAL;
	mutex_lock(&rj->lock);
	if (!lrj_current(s)) {
		ret = -ENODEV;
		goto out;
	}
	if (rj->wedged) {
		ret = -EIO;
		goto out;
	}
	/* The copy: what is checked and what is written. */
	if (copy_from_user(s->w, u64_to_user_ptr(r->writes), r->nwrites * sizeof(s->w[0])) ||
	    copy_from_user(s->now_reads, u64_to_user_ptr(r->reads),
			   r->nreads * sizeof(s->now_reads[0]))) {
		ret = -EFAULT;
		goto out;
	}
	ret = lrj_check_writes(p, table, n, s->w, r->nwrites);
	for (u32 i = 0; !ret && i < r->nreads; i++)
		if (s->now_reads[i] % 4 || s->now_reads[i] > p->window - 4)
			ret = -EINVAL;
	if (ret) {
		rj->refused++;
		goto out;
	}
	for (u32 i = 0; i < r->nwrites; i++)
		lrj_write_checked(rj, table, n, &s->w[i]);
	for (u32 i = 0; i < r->nreads; i++)
		s->now_values[i] = lrj_rd(rj, s->now_reads[i]);
	if (copy_to_user(u64_to_user_ptr(r->values), s->now_values,
			 r->nreads * sizeof(s->now_values[0])))
		ret = -EFAULT;
out:
	mutex_unlock(&rj->lock);
	return ret;
}

/* --- the session's file --- */

static void lrj_info(struct lem_rj *rj, struct lemnos_regjob_info *info)
{
	memset(info, 0, sizeof(*info));
	info->profile = rj->p->id;
	info->window_bytes = rj->p->window;
	info->wedged = rj->wedged;
	info->live = hweight64(rj->cur | rj->prev);
	info->jobs = rj->jobs;
	info->refused = rj->refused;
	info->timeouts = rj->timeouts;
	info->resets = rj->resets;
}

static long lrj_ioctl(struct file *file, unsigned int cmd, unsigned long arg)
{
	struct lrj_session *s = file->private_data;
	struct lem_rj *rj = s->rj;
	void __user *uarg = (void __user *)arg;
	union {
		struct lemnos_regjob_import imp;
		struct lemnos_regjob_run run;
		struct lemnos_regjob_wait wait;
		struct lemnos_regjob_info info;
		struct lemnos_regjob_regs regs;
		struct lemnos_regjob_event event;
		u32 index;
	} u;
	long ret;

	/* A configuration session names no buffer; a frames session has no events. */
	switch (cmd) {
	case LEMNOS_REGJOB_IOC_RUN:
	case LEMNOS_REGJOB_IOC_WAIT:
	case LEMNOS_REGJOB_IOC_IMPORT:
	case LEMNOS_REGJOB_IOC_RELEASE:
		if (s->config)
			return -EINVAL;
		break;
	case LEMNOS_REGJOB_IOC_EVENT:
		if (!s->config)
			return -EINVAL;
		break;
	}

	switch (cmd) {
	case LEMNOS_REGJOB_IOC_WRITE:
		if (copy_from_user(&u.regs, uarg, sizeof(u.regs)))
			return -EFAULT;
		return lrj_regs(s, &u.regs);
	case LEMNOS_REGJOB_IOC_EVENT:
		ret = lrj_event_take(s, &u.event);
		if (!ret && copy_to_user(uarg, &u.event, sizeof(u.event)))
			return -EFAULT;
		return ret;
	case LEMNOS_REGJOB_IOC_RUN:
		if (copy_from_user(&u.run, uarg, sizeof(u.run)))
			return -EFAULT;
		ret = lrj_run(s, &u.run);
		if (!ret && copy_to_user(uarg, &u.run, sizeof(u.run)))
			return -EFAULT;
		return ret;
	case LEMNOS_REGJOB_IOC_WAIT:
		if (copy_from_user(&u.wait, uarg, sizeof(u.wait)))
			return -EFAULT;
		ret = lrj_wait(s, &u.wait);
		if (!ret && copy_to_user(uarg, &u.wait, sizeof(u.wait)))
			return -EFAULT;
		return ret;
	case LEMNOS_REGJOB_IOC_IMPORT:
		if (copy_from_user(&u.imp, uarg, sizeof(u.imp)))
			return -EFAULT;
		mutex_lock(&rj->lock);
		ret = rj->gone || rj->session != s ? -ENODEV : lrj_import(s, &u.imp);
		mutex_unlock(&rj->lock);
		/* A failed copy leaves the buffer to the close: the caller has lost its index. */
		if (!ret && copy_to_user(uarg, &u.imp, sizeof(u.imp)))
			return -EFAULT;
		return ret;
	case LEMNOS_REGJOB_IOC_RELEASE:
		if (copy_from_user(&u.index, uarg, sizeof(u.index)))
			return -EFAULT;
		mutex_lock(&rj->lock);
		ret = rj->gone || rj->session != s ? -ENODEV : lrj_release_buf(s, u.index);
		mutex_unlock(&rj->lock);
		return ret;
	case LEMNOS_REGJOB_IOC_INFO:
		mutex_lock(&rj->lock);
		lrj_info(rj, &u.info);
		mutex_unlock(&rj->lock);
		return copy_to_user(uarg, &u.info, sizeof(u.info)) ? -EFAULT : 0;
	default:
		return -ENOTTY;
	}
}

static int lrj_file_release(struct inode *inode, struct file *file)
{
	struct lrj_session *s = file->private_data;
	struct lem_rj *rj = s->rj;

	unsigned long flags;

	mutex_lock(&rj->lock);
	if (rj->session == s) {
		lrj_end_session(rj, s);
		rj->session = NULL;
	}
	if (rj->config == s) {
		/* Its interrupt off; what it wrote stays, as the frames role's writes do. */
		spin_lock_irqsave(&rj->irq_lock, flags);
		if (!rj->gone && !rj->wedged)
			rj->p->event_arm(rj, false);
		rj->config = NULL;
		spin_unlock_irqrestore(&rj->irq_lock, flags);
	}
	mutex_unlock(&rj->lock);
	kvfree(s);
	kref_put(&rj->ref, lrj_free);
	return 0;
}

static const struct file_operations lrj_fops = {
	.owner = THIS_MODULE,
	.release = lrj_file_release,
	.unlocked_ioctl = lrj_ioctl,
	.compat_ioctl = compat_ptr_ioctl,
	.llseek = noop_llseek,
};

long lem_rj_open(struct lem_rj *rj, struct lemnos_regjob_open *o)
{
	bool config = o->flags & LEMNOS_REGJOB_OPEN_CONFIG;
	struct lrj_session **role = config ? &rj->config : &rj->session;
	struct lrj_session *s;
	unsigned long flags;
	int fd;

	if (o->flags & ~LEMNOS_REGJOB_OPEN_CONFIG)
		return -EINVAL;
	if (config && !rj->p->nconfig)
		return -EINVAL;
	s = kvzalloc(sizeof(*s), GFP_KERNEL);
	if (!s)
		return -ENOMEM;
	s->rj = rj;
	s->config = config;
	mutex_lock(&rj->lock);
	if (rj->gone) {
		fd = -ENODEV;
		goto fail;
	}
	if (*role) {
		fd = -EBUSY;
		goto fail;
	}
	kref_get(&rj->ref);
	fd = anon_inode_getfd("lemnos-regjob", &lrj_fops, s, O_RDWR | O_CLOEXEC);
	if (fd < 0) {
		kref_put(&rj->ref, lrj_free);	/* the probe's reference is still held */
		goto fail;
	}
	/*
	 * Set before the lock is let go: a close of the new fd from another
	 * thread waits for the lock, then finds the session in its role.
	 */
	spin_lock_irqsave(&rj->irq_lock, flags);
	*role = s;
	if (config && !rj->wedged)
		rj->p->event_arm(rj, true);
	spin_unlock_irqrestore(&rj->irq_lock, flags);
	mutex_unlock(&rj->lock);
	o->fd = fd;
	o->profile = rj->p->id;
	o->window_bytes = rj->p->window;
	o->abi = LEMNOS_REGJOB_ABI;
	return 0;
fail:
	mutex_unlock(&rj->lock);
	kvfree(s);
	return fd;
}

/* --- the block --- */

/* The configuration role's interrupt, if pending: read, cleared and queued. irq_lock held. */
static bool lrj_event_irq(struct lem_rj *rj)
{
	u32 status = rj->p->event(rj);

	if (!status)
		return false;
	lrj_event_push(rj, status, ktime_get_ns());
	return true;
}

static irqreturn_t lrj_event_isr(int irq, void *data)
{
	struct lem_rj *rj = data;
	bool handled;

	spin_lock(&rj->irq_lock);
	handled = lrj_event_irq(rj);
	spin_unlock(&rj->irq_lock);
	return handled ? IRQ_HANDLED : IRQ_NONE;
}

static irqreturn_t lrj_isr(int irq, void *data)
{
	struct lem_rj *rj = data;
	irqreturn_t ret = IRQ_NONE;
	u32 status;

	spin_lock(&rj->irq_lock);
	/* One line for both roles: the events are this handler's too. */
	if (rj->p->nconfig && !rj->event_irq && lrj_event_irq(rj))
		ret = IRQ_HANDLED;
	if (rj->p->irq(rj, &status)) {
		ret = IRQ_HANDLED;
		/* A completion with no job armed is stale, and only cleared. */
		if (rj->armed && !rj->done && rj->job) {
			struct lrj_session *s = rj->job;

			for (u32 i = 0; i < rj->pending_reads; i++)
				s->values[i] = lrj_rd(rj, s->reads[i]);
			if (rj->latch_pending) {
				status |= rj->p->latch(rj, &s->ctx);
				rj->latch_pending = false;
			}
			rj->done = true;
			rj->done_ns = ktime_get_ns();
			rj->status |= status;
			wake_up(&rj->wq);
		}
	}
	spin_unlock(&rj->irq_lock);
	return ret;
}

static const struct lrj_profile *lrj_profile_for(struct device *dev)
{
	if (lem_device_listed(dev, a733_ve))
		return &lrj_a733_ve;
	if (lem_device_listed(dev, k230_isp))
		return &lrj_k230_isp;
	return NULL;
}

static void lrj_clk_off(void *data)
{
	struct clk_bulk_data *clks = data;
	int n = 0;

	while (clks[n].clk)
		n++;
	clk_bulk_disable_unprepare(n, clks);
}

static int lrj_clocks_on(struct device *dev)
{
	struct clk_bulk_data *clks, *table;
	int n, ret;

	n = devm_clk_bulk_get_all(dev, &clks);
	if (n < 0)
		return n;
	if (!n)
		return 0;
	/* One spare entry, zeroed, ends the table for the action. */
	table = devm_kcalloc(dev, n + 1, sizeof(*table), GFP_KERNEL);
	if (!table)
		return -ENOMEM;
	memcpy(table, clks, n * sizeof(*table));
	ret = clk_bulk_prepare_enable(n, table);
	if (ret)
		return ret;
	return devm_add_action_or_reset(dev, lrj_clk_off, table);
}

struct lem_rj *lem_rj_probe(struct platform_device *pdev)
{
	const struct lrj_profile *p = lrj_profile_for(&pdev->dev);
	struct device *dev = &pdev->dev;
	struct resource *res;
	struct lem_rj *rj;
	int ret;

	if (!p)
		return NULL;
	rj = kzalloc(sizeof(*rj), GFP_KERNEL);
	if (!rj)
		return ERR_PTR(-ENOMEM);
	rj->dev = dev;
	rj->p = p;
	kref_init(&rj->ref);
	mutex_init(&rj->lock);
	spin_lock_init(&rj->irq_lock);
	init_waitqueue_head(&rj->wq);

	res = platform_get_resource(pdev, IORESOURCE_MEM, 0);
	if (!res || resource_size(res) < p->window) {
		ret = -EINVAL;
		goto fail;
	}
	if (test_rj_fake_regs) {
		void *mem = devm_memremap(dev, res->start, resource_size(res), MEMREMAP_WB);

		if (IS_ERR(mem)) {
			ret = PTR_ERR(mem);
			goto fail;
		}
		rj->regs = (void __iomem *)mem;
		dev_warn(dev, "test_rj_fake_regs: registers are ordinary memory\n");
	} else {
		rj->regs = devm_ioremap_resource(dev, res);
		if (IS_ERR(rj->regs)) {
			ret = PTR_ERR(rj->regs);
			goto fail;
		}
	}
	ret = dma_set_mask_and_coherent(dev, DMA_BIT_MASK(fls64(p->addr_limit - 1)));
	if (ret)
		goto fail;
	ret = lrj_clocks_on(dev);
	if (ret)
		goto fail;
	rj->rst = devm_reset_control_get_optional_exclusive(dev, NULL);
	if (IS_ERR(rj->rst)) {
		ret = PTR_ERR(rj->rst);
		goto fail;
	}
	if (rj->rst) {
		ret = reset_control_deassert(rj->rst);
		if (ret)
			goto fail;
	} else {
		dev_warn(dev, "no reset line: a job that doesn't finish wedges the block\n");
	}
	p->reset_regs(rj);
	p->quiet(rj);
	rj->irq = platform_get_irq(pdev, p->irq_index);
	if (rj->irq <= 0) {
		ret = rj->irq ?: -EINVAL;
		goto fail;
	}
	ret = devm_request_irq(dev, rj->irq, lrj_isr, 0, dev_name(dev), rj);
	if (ret)
		goto fail;
	if (p->nconfig) {
		int irq = platform_get_irq(pdev, p->event_irq_index);

		if (irq <= 0) {
			ret = irq ?: -EINVAL;
			goto fail_irq;
		}
		/* The same line as the jobs': lrj_isr takes both. */
		if (irq != rj->irq) {
			ret = devm_request_irq(dev, irq, lrj_event_isr, 0, dev_name(dev), rj);
			if (ret)
				goto fail_irq;
			rj->event_irq = irq;
		}
	}
	dev_info(dev, "register jobs as %s%s\n", p->name,
		 p->nconfig && !rj->event_irq ? ", both roles on one interrupt" : "");
	return rj;
fail_irq:
	devm_free_irq(dev, rj->irq, rj);
fail:
	kfree(rj);
	return ERR_PTR(ret);
}

/*
 * The block is being unbound: a running job ends first (its timeout bounds
 * that), then the block is stopped if it may be using a buffer, every buffer
 * of the frames session is released, the configuration session gets nothing
 * more (its calls answer ENODEV), and nothing touches the registers again.
 */
void lem_rj_remove(struct lem_rj *rj)
{
	unsigned long flags;

	mutex_lock(&rj->lock);
	/* The configuration session goes first: a reset below is no news to it. */
	spin_lock_irqsave(&rj->irq_lock, flags);
	if (rj->config && !rj->wedged)
		rj->p->event_arm(rj, false);
	rj->config = NULL;
	spin_unlock_irqrestore(&rj->irq_lock, flags);
	if (rj->session) {
		lrj_end_session(rj, rj->session);
		rj->session = NULL;
	}
	rj->gone = true;
	spin_lock_irqsave(&rj->irq_lock, flags);
	rj->armed = false;
	rj->job = NULL;
	if (!rj->wedged)
		rj->p->quiet(rj);
	spin_unlock_irqrestore(&rj->irq_lock, flags);
	mutex_unlock(&rj->lock);
	devm_free_irq(rj->dev, rj->irq, rj);
	if (rj->event_irq)
		devm_free_irq(rj->dev, rj->event_irq, rj);
	if (rj->rst && !rj->wedged)
		reset_control_assert(rj->rst);
	kref_put(&rj->ref, lrj_free);
}
