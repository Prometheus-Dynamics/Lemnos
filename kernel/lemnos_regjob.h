/* SPDX-License-Identifier: GPL-2.0 WITH Linux-syscall-note */
/*
 * Register jobs: a block whose register writes can make it reach memory, run
 * by lemnos.ko from a list of register values and the caller's dma-bufs. A
 * block listed in one of the module's register-job parameters (a733_ve=,
 * k230_isp=) is driven here: its registers can't be mapped and its interrupt isn't
 * forwarded.
 *
 * LEMNOS_IOC_REGJOB_OPEN (on /dev/lemnos) opens a session on such a block.
 * A block has two roles, each with its own session, one opener each (EBUSY for
 * a second):
 *
 * - The frames role (flags 0): the jobs below, which point the block at the
 *   caller's dma-bufs.
 * - The configuration role (LEMNOS_REGJOB_OPEN_CONFIG), on a profile that has
 *   one (EINVAL otherwise): register writes and reads that take no address,
 *   made at once (LEMNOS_REGJOB_IOC_WRITE), and the block's other interrupt as
 *   events (LEMNOS_REGJOB_IOC_EVENT). It never names a buffer: IMPORT,
 *   RELEASE, RUN and WAIT are refused on it (EINVAL), as WRITE is on a profile
 *   with no configuration role and EVENT on a frames session.
 *
 * The two roles of one block run side by side: the K230 ISP's frame path and
 * its tuning and statistics are two drivers' work. Each role writes only the
 * registers its own table lists, and a register in both tables (the K230
 * ISP's isp_ctrl) is written by read-modify-write under the module's lock,
 * with only the bits of the writer's mask changed, so neither role's bits are
 * lost to the other's.
 *
 * On the session's file:
 *
 * - LEMNOS_REGJOB_IOC_IMPORT: a dma-buf the caller holds, attached and mapped
 *   for the block and pinned until it is released. One contiguous range in the
 *   block's address space, or refused (EINVAL).
 * - LEMNOS_REGJOB_IOC_RELEASE: lets one go. Refused (EBUSY) while the block may
 *   still be using it.
 * - LEMNOS_REGJOB_IOC_RUN: one job: the module writes the addresses, the
 *   job's register values in order and the start value, waits for the block's
 *   interrupt and reads back the registers the job names, in the interrupt
 *   handler. With LEMNOS_REGJOB_RUN_NOWAIT it returns once the job is started,
 *   and LEMNOS_REGJOB_IOC_WAIT collects it.
 * - LEMNOS_REGJOB_IOC_WAIT: the started job's end, without waiting: EAGAIN
 *   while it runs, ETIMEDOUT (after the reset below) once its time is past.
 * - LEMNOS_REGJOB_IOC_INFO: the block's profile and counters.
 * - LEMNOS_REGJOB_IOC_WRITE: register writes, then reads, made at once under
 *   the module's lock, checked against the session's role's table as a job's
 *   writes are. Nothing is written unless every write passes.
 * - LEMNOS_REGJOB_IOC_EVENT (configuration role): the next of the block's
 *   other interrupts (the K230 ISP's core: frame in, frame out), as its
 *   status, read and cleared by the module's handler, and the time; EAGAIN
 *   when none is queued. The module enables that interrupt while a
 *   configuration session is open. It queues LEMNOS_REGJOB_EVENTS events and
 *   counts the ones it drops. A reset of the block by the frames role (after a
 *   timeout, or a frames session closed on a running block) undoes the
 *   configuration: it is queued as an event with LEMNOS_REGJOB_EVENT_RESET.
 *
 * What the module checks, before any register is written (the job is copied
 * into kernel memory first, and only the copy is looked at):
 *
 * - Every register a write names is one the block's profile lets the
 *   session's role set (EPERM otherwise), with only the bits it lets that role
 *   set (EPERM), inside the window and on a register boundary (EINVAL).
 *   Registers that take an address, and the interrupt and path enables the
 *   module keeps, are never in a table: a write to one is refused (EPERM).
 * - Every address register of the profile is named by exactly one address of
 *   the job (EINVAL otherwise), with a buffer this session imported (ENOENT),
 *   an offset and a length that lie inside that buffer, computed without
 *   overflow (EFAULT), and an alignment the register takes (EINVAL). The module
 *   writes the register itself, from the buffer's device address.
 * - Every address's length covers what the block will reach from it, worked
 *   out from the job's own geometry registers by the profile (EFAULT
 *   otherwise). A geometry register must be written exactly once in the job
 *   (EINVAL). Registers that bound how far the block goes from an address
 *   (an end address, a size) are written by the module from the checked
 *   length.
 * - The start value is one the profile allows (EPERM); the timeout is 1 to
 *   LEMNOS_REGJOB_MAX_TIMEOUT_MS (EINVAL); reads are on register boundaries
 *   inside the window (EINVAL); flags are known ones (EINVAL). A job while
 *   another is started and not collected is refused (EBUSY).
 *
 * A block that keeps using its addresses after a job completes (a capture
 * path, LEMNOS_REGJOB_PROFILE_K230_ISP) takes a job's addresses at the end of
 * the frame it is writing: the module makes the switch itself, in the
 * interrupt handler, and reads back what the block took. The job completes
 * there, and the buffers of the job before it are free from then on; the
 * frame the block just finished is in them. If the block did not take the
 * new addresses (the module reads back what it took), the status has
 * LEMNOS_REGJOB_STATUS_UNLATCHED and the old buffers stay held as well.
 *
 * A refused job leaves every register as it was. A job that doesn't finish in
 * time ends with ETIMEDOUT after the block is reset through its reset line; a
 * block with no reset line can't be shown to have stopped, so it is wedged:
 * every buffer it was given stays pinned until a reboot, lemnos.ko can't be
 * unloaded, and every later job is refused with EIO.
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#ifndef _UAPI_LEMNOS_REGJOB_H
#define _UAPI_LEMNOS_REGJOB_H

#include <linux/ioctl.h>
#include <linux/types.h>

#define LEMNOS_REGJOB_ABI 1

/* Profiles: which block's register map a session runs. */
#define LEMNOS_REGJOB_PROFILE_A733_VE 1	/* the A733's second video engine, its H.264 encoder */
#define LEMNOS_REGJOB_PROFILE_K230_ISP 2	/* the K230's ISP, its main path into memory */

#define LEMNOS_REGJOB_MAX_WRITES 1024
#define LEMNOS_REGJOB_MAX_ADDRS 16
#define LEMNOS_REGJOB_MAX_READS 64
#define LEMNOS_REGJOB_MAX_BUFFERS 64
#define LEMNOS_REGJOB_MAX_TIMEOUT_MS 10000

/* lemnos_regjob_open.flags */
#define LEMNOS_REGJOB_OPEN_CONFIG (1u << 0)	/* the configuration role */

/* Events a configuration session can have queued. */
#define LEMNOS_REGJOB_EVENTS 64

/* An event status bit the module sets: the block was reset, its configuration is gone. */
#define LEMNOS_REGJOB_EVENT_RESET (1u << 31)

/* lemnos_regjob_run.flags */
#define LEMNOS_REGJOB_RUN_NOWAIT (1u << 0)	/* return once started; collect with WAIT */

/* A bit the module adds to a completion status (a capture path's): the new addresses weren't taken. */
#define LEMNOS_REGJOB_STATUS_UNLATCHED (1u << 31)

struct lemnos_regjob_open {
	__u32 block;		/* in: the block's number */
	__u32 flags;		/* in: LEMNOS_REGJOB_OPEN_* */
	__s32 fd;		/* out: the session, close-on-exec */
	__u32 profile;		/* out: LEMNOS_REGJOB_PROFILE_* */
	__u32 window_bytes;	/* out: the register window jobs may name */
	__u32 abi;		/* out: LEMNOS_REGJOB_ABI */
};

struct lemnos_regjob_import {
	__s32 fd;		/* in: a dma-buf the caller holds */
	__u32 index;		/* out: what a job's address names */
	__u64 bytes;		/* out: its size in the block's address space */
	__u64 addr;		/* out: its device address, for information; never taken back */
};

struct lemnos_regjob_write {
	__u32 offset;		/* bytes into the window */
	__u32 value;
};

struct lemnos_regjob_addr {
	__u32 reg;		/* an address register of the profile */
	__u32 buffer;		/* an imported buffer's index */
	__u64 offset;		/* bytes into the buffer */
	__u64 bytes;		/* what the job may reach from there */
};

struct lemnos_regjob_run {
	__u64 writes;		/* pointer to nwrites struct lemnos_regjob_write */
	__u64 addrs;		/* pointer to naddrs struct lemnos_regjob_addr */
	__u64 reads;		/* pointer to nreads __u32 offsets, read at the job's end */
	__u64 values;		/* pointer to nreads __u32, out (not with NOWAIT) */
	__u32 nwrites;
	__u32 naddrs;
	__u32 nreads;
	__u32 start;		/* written to the profile's start register, last */
	__u32 timeout_ms;
	__u32 status;		/* out: the completion status (not with NOWAIT) */
	__u32 flags;		/* LEMNOS_REGJOB_RUN_* */
	__u32 reserved;		/* 0 */
};

/* Collects a job started with LEMNOS_REGJOB_RUN_NOWAIT. */
struct lemnos_regjob_wait {
	__u64 values;		/* pointer to the job's nreads __u32, out */
	__u32 nreads;		/* in: the job's nreads */
	__u32 status;		/* out: the completion status */
	__u64 timestamp_ns;	/* out: when the handler completed it, on the monotonic clock */
};

/* Register writes and reads made at once (LEMNOS_REGJOB_IOC_WRITE). */
struct lemnos_regjob_regs {
	__u64 writes;		/* pointer to nwrites struct lemnos_regjob_write */
	__u64 reads;		/* pointer to nreads __u32 offsets, read after the writes */
	__u64 values;		/* pointer to nreads __u32, out */
	__u32 nwrites;		/* at most LEMNOS_REGJOB_MAX_WRITES */
	__u32 nreads;		/* at most LEMNOS_REGJOB_MAX_READS */
};

/* One interrupt for the configuration role (LEMNOS_REGJOB_IOC_EVENT). */
struct lemnos_regjob_event {
	__u32 status;		/* the interrupt's status, or LEMNOS_REGJOB_EVENT_RESET */
	__u32 lost;		/* events dropped since the last one taken: the queue was full */
	__u64 timestamp_ns;	/* when, on the monotonic clock */
};

struct lemnos_regjob_info {
	__u32 profile;
	__u32 window_bytes;
	__u32 wedged;		/* nonzero: buffers are held until a reboot */
	__u32 live;		/* buffers the block may still be using */
	__u64 jobs;		/* jobs finished */
	__u64 refused;		/* jobs refused by the checks */
	__u64 timeouts;		/* jobs that did not finish in time */
	__u64 resets;		/* resets after a timeout */
};

/* On /dev/lemnos. */
#define LEMNOS_IOC_REGJOB_OPEN _IOWR('L', 0xf0, struct lemnos_regjob_open)
/* On the session. */
#define LEMNOS_REGJOB_IOC_IMPORT _IOWR('L', 0xf1, struct lemnos_regjob_import)
#define LEMNOS_REGJOB_IOC_RELEASE _IOW('L', 0xf2, __u32)
#define LEMNOS_REGJOB_IOC_RUN _IOWR('L', 0xf3, struct lemnos_regjob_run)
#define LEMNOS_REGJOB_IOC_INFO _IOR('L', 0xf4, struct lemnos_regjob_info)
#define LEMNOS_REGJOB_IOC_WAIT _IOWR('L', 0xf5, struct lemnos_regjob_wait)
#define LEMNOS_REGJOB_IOC_WRITE _IOW('L', 0xf6, struct lemnos_regjob_regs)
#define LEMNOS_REGJOB_IOC_EVENT _IOR('L', 0xf7, struct lemnos_regjob_event)

#endif
