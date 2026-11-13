/* SPDX-License-Identifier: GPL-2.0 */
/* What the module's files share. Copyright (c) 2026 the Lemnos authors. */
#ifndef LEMNOS_INTERNAL_H
#define LEMNOS_INTERNAL_H

#include <linux/fs.h>
#include <linux/poll.h>
#include <linux/types.h>
#include <linux/wait.h>

#include "lemnos.h"

struct device;

long lem_claim(struct lemnos_claim *c);
/* Whether `fd` is a claim on exactly `resource`. Allocates nothing. */
bool lem_claim_held(int fd, const char *resource);

/*
 * What a claim's file is bound to: read and poll may be NULL; release runs
 * when the file's last copy closes.
 */
struct lem_claim_ops {
	void (*release)(void *data);
	ssize_t (*read)(void *data, struct file *file, char __user *buf, size_t len);
	__poll_t (*poll)(void *data, struct file *file, poll_table *wait);
};

/* Binds `data` to the claim `fd`, which must be on exactly `resource`. Once per claim. */
int lem_claim_attach(int fd, const char *resource, const struct lem_claim_ops *ops, void *data);
/* What `file` is bound to with `ops`, or NULL. `file` may be NULL. Allocates nothing. */
void *lem_claim_attached(struct file *file, const struct lem_claim_ops *ops);

long lem_gpio_request(const struct lemnos_gpio_request *r);
long lem_gpio_config(const struct lemnos_gpio_config *c);
long lem_gpio_get(struct lemnos_gpio_value *v);
long lem_gpio_set(const struct lemnos_gpio_value *v);
long lem_pwm_request(struct lemnos_pwm_request *r);
long lem_pwm_apply(const struct lemnos_pwm_state *s);
long lem_pwm_get(struct lemnos_pwm_state *s);

/* The event stream's queue. Interrupts have their own list in the core. */
extern wait_queue_head_t lem_wait;
void lem_event_post(u32 kind, u32 block, u32 index, s64 value);
bool lem_event_pending(void);
bool lem_event_take(struct lemnos_event *e);
long lem_thermal_read(struct lemnos_thermal *t);
long lem_thermal_watch(struct lemnos_thermal_watch *w);
/* Ends the watches and empties the queue: the file closed. */
void lem_events_reset(void);

/* A block's clocks, supplies and resets. */
struct lem_power;
struct lem_power *lem_power_create(struct device *dev);
void lem_power_set_block(struct lem_power *p, u32 block);
long lem_power_request(struct lem_power *p, struct lemnos_power *r);
void lem_power_destroy(struct lem_power *p);

#ifdef LEMNOS_HAVE_PIO
long lem_pio_open(const struct lemnos_pio_open *o);
long lem_pio_call(struct lemnos_pio_call *c);
long lem_pio_config_xfer(const struct lemnos_pio_config_xfer *c);
long lem_pio_xfer(const struct lemnos_pio_xfer *x);
#else
static inline long lem_pio_open(const struct lemnos_pio_open *o) { return -ENOTTY; }
static inline long lem_pio_call(struct lemnos_pio_call *c) { return -ENOTTY; }
static inline long lem_pio_config_xfer(const struct lemnos_pio_config_xfer *c) { return -ENOTTY; }
static inline long lem_pio_xfer(const struct lemnos_pio_xfer *x) { return -ENOTTY; }
#endif

int lem_bus_init(void);
void lem_bus_exit(void);
long lem_i2c_transfer(const struct lemnos_i2c_transfer *t);
long lem_i2c_smbus(struct lemnos_i2c_smbus *s);
long lem_spi_transfer(const struct lemnos_spi_transfer *t);
long lem_spi_info(struct lemnos_spi_info *t);

struct device;
struct file;
struct lem_stream;
struct lem_stream_producer;
struct vm_area_struct;

/* Whether `dev` is one of the blocks the owned= parameter lists. */
bool lem_block_owned(struct device *dev);

long lem_stream_create(struct device *dev, struct lemnos_stream_create *c);
/*
 * lem_stream_create() for a block whose jobs are its own: the producer is
 * attached, with a frame of the whole buffer, before the fd exists, so the
 * consumer never holds a stream without it. *out is set before the fd is
 * installed. On failure there's no stream and producer->priv is still the
 * caller's; on success the stream owns it, through producer->release.
 */
long lem_stream_create_producer(struct device *dev, struct lemnos_stream_create *c,
				const struct lem_stream_producer *producer,
				struct lem_stream **out);
void lem_streams_block_gone(struct device *dev);

/* A device address a block is programmed with is aligned this much. */
#define LEM_STREAM_DMA_ALIGN 16u

/* A validated range: what the source uses; nothing the consumer can change. */
struct lem_job_range {
	u32 region;
	u64 offset;
	u64 len;
};

/*
 * What a job can hold at once: more than a posted struct lemnos_stream_job can
 * name, for a block whose jobs name more memory (the PiSP back end: 13 images).
 */
#define LEM_JOB_HOLDS 13u

struct lem_job {
	u64 identity;
	u32 buffer;
	u32 count;
	struct lem_job_range range[LEM_JOB_HOLDS];
};

/*
 * One frame a block holds, from lem_stream_next() until lem_stream_complete()
 * or lem_stream_cancel(). The buffer and every region its job names stay pinned
 * for that long. `addr` is the start of that buffer's memory (its slice of
 * the pool, or its own dma-buf), computed here with overflow checks from what
 * the module mapped; `bytes` are the frame's,
 * checked against the buffer when the producer attached.
 */
struct lem_stream_slot {
	struct lem_job job;
	u64 addr;
	u32 bytes;
};

/*
 * A block feeding a stream. stop() is called once with the stream ending (file
 * closed, or the block unbound), from process context, never with s->lock held:
 * it must stop the block and not return until the block can no longer touch
 * any buffer. A block that can't be sure calls lem_stream_wedge(), and this
 * stream's pins are then kept for good.
 */
struct lem_stream_producer {
	void (*stop)(void *priv, struct lem_stream *s);
	void *priv;

	/*
	 * The rest is for a block whose jobs are its own (the PiSP back end), and
	 * stays zero for one taking frames with lem_stream_next().
	 *
	 * job_bytes: nonzero says the ring's jobs are this block's format and it
	 * reads them, with lem_stream_job_peek() and _take(); the stream must have
	 * been created with exactly this size.
	 * kick: LEMNOS_STREAM_IOC_KICK, no lock held, the stream not yet over when
	 * called; its errno, other than ENODEV, ends the stream.
	 * ioctl: requests the stream doesn't know, s->ctl held, stream not over.
	 * mmap: a mapping at a nonzero offset; no lock held.
	 * release: the stream's file is gone and nothing can reach priv any more.
	 * Called once, wedged or not, and it's for release() to keep what a wedged
	 * block may still be using.
	 */
	u32 job_bytes;
	long (*kick)(void *priv);
	long (*ioctl)(void *priv, unsigned int cmd, void __user *arg);
	int (*mmap)(void *priv, struct vm_area_struct *vma);
	void (*release)(void *priv);
};

/* The stream open as `file`, if it was created on `dev`; an ERR_PTR otherwise. */
struct lem_stream *lem_stream_from_file(struct file *file, struct device *dev);
/*
 * One producer per stream. `bytes` is what it may write per buffer. The stream
 * knows bytes only: a producer whose frames are lines or planes checks that
 * they fit in `bytes` itself, before attaching.
 */
int lem_stream_attach(struct lem_stream *s, const struct lem_stream_producer *producer, u32 bytes);
/* Removes the producer; true when it was there and this call took it off. */
bool lem_stream_detach(struct lem_stream *s);
/* The next frame's buffer, any context: 0, or -ENOBUFS when none is free. */
int lem_stream_next(struct lem_stream *s, struct lem_stream_slot *slot);
/* Done with a slot: publish `bytes` of it, or 0 to say the frame went wrong. */
void lem_stream_complete(struct lem_stream *s, const struct lem_stream_slot *slot, u32 bytes,
			 u64 sequence, u64 timestamp_ns);
/* Done with a slot that was never written. */
void lem_stream_cancel(struct lem_stream *s, const struct lem_stream_slot *slot);
/* The block's DMA can't be shown to have stopped: keep everything pinned for good. */
void lem_stream_wedge(struct lem_stream *s);
/*
 * Reads a job's range through the kernel's mapping of its registered region,
 * any context, exactly `bytes`. The pool and a buffer's own dma-buf aren't
 * readable here. Needs a stream
 * that has had lem_stream_cpu_read_enable() and a job still held.
 */
int lem_stream_range_read(struct lem_stream *s, const struct lem_job_range *r, void *dst,
			  size_t bytes);
/* A held job's range as a device address, computed here. The pool and a buffer's own dma-buf are refused. */
int lem_stream_range_addr(struct lem_stream *s, const struct lem_job_range *r, u64 *addr);
/* Test only: writes a word through a held range's kernel mapping, for the module's checks. */
void lem_stream_range_poke(struct lem_stream *s, const struct lem_job_range *r, size_t offset, u32 val);
int lem_stream_cpu_read_enable(struct lem_stream *s);

/*
 * For a producer with job_bytes set. Reads the job at the front of the ring
 * into `job` (job_bytes), once: 1 with a job, 0 with none, -errno for a lying
 * consumer. Doesn't take it: the block copies and checks what it names first.
 */
int lem_stream_job_peek(struct lem_stream *s, void *job, size_t bytes);
/*
 * The job at the front of the ring passed the block's checks. Resolves every
 * range against job's buffer and the registered regions and, only if all
 * resolve and the buffer is free, takes the job and holds them, in one step:
 * nothing can be unregistered between the check and the hold. `slot` is then
 * as from lem_stream_next() and ends with lem_stream_complete() or _cancel().
 */
int lem_stream_job_take(struct lem_stream *s, u32 buffer, u64 identity,
			const struct lemnos_stream_range *range, u32 count,
			struct lem_stream_slot *slot);
/* The block found it can't go on: the consumer learns with `err` and the stream ends. */
void lem_stream_fault(struct lem_stream *s, int err);

struct platform_device;
struct lem_pb;
struct lem_pb *lem_pb_probe(struct platform_device *pdev);
void lem_pb_remove(struct lem_pb *l);
long lem_pb_create(struct lem_pb *l, struct device *dev, void *args);

struct lem_gd;
struct lem_gd *lem_gd_probe(struct platform_device *pdev);
void lem_gd_remove(struct lem_gd *g);
long lem_gd_create(struct lem_gd *g, struct device *dev, void *args);

int lem_rp1_csi2_register(void);
void lem_rp1_csi2_unregister(void);

#endif
