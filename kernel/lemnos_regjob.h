/* SPDX-License-Identifier: GPL-2.0 WITH Linux-syscall-note */
/*
 * Register jobs: a block whose register writes can make it reach memory, run
 * by lemnos.ko from a list of register values and the caller's dma-bufs. A
 * block listed in one of the module's register-job parameters (a733_ve=) is
 * driven here: its registers can't be mapped and its interrupt isn't
 * forwarded.
 *
 * LEMNOS_IOC_REGJOB_OPEN (on /dev/lemnos) opens a session on such a block, one
 * at a time. On the session's file:
 *
 * - LEMNOS_REGJOB_IOC_IMPORT: a dma-buf the caller holds, attached and mapped
 *   for the block and pinned until it is released. One contiguous range in the
 *   block's address space, or refused (EINVAL).
 * - LEMNOS_REGJOB_IOC_RELEASE: lets one go. Refused (EBUSY) while the block may
 *   still be using it.
 * - LEMNOS_REGJOB_IOC_RUN: one job, run to its end: the module writes the
 *   addresses, the job's register values in order and the start value, waits
 *   for the block's interrupt and reads back the registers the job names.
 * - LEMNOS_REGJOB_IOC_INFO: the block's profile and counters.
 *
 * What the module checks, before any register is written (the job is copied
 * into kernel memory first, and only the copy is looked at):
 *
 * - Every register a write names is one the block's profile lets a caller set
 *   (EPERM otherwise), with only the bits it lets a caller set (EPERM), inside
 *   the window and on a register boundary (EINVAL). Registers that take an
 *   address are never in that list: a write to one is refused (EPERM).
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
 *   inside the window (EINVAL).
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

#define LEMNOS_REGJOB_MAX_WRITES 1024
#define LEMNOS_REGJOB_MAX_ADDRS 16
#define LEMNOS_REGJOB_MAX_READS 64
#define LEMNOS_REGJOB_MAX_BUFFERS 64
#define LEMNOS_REGJOB_MAX_TIMEOUT_MS 10000

struct lemnos_regjob_open {
	__u32 block;		/* in: the block's number */
	__u32 flags;		/* in: 0 */
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
	__u64 reads;		/* pointer to nreads __u32 offsets, read after the job */
	__u64 values;		/* pointer to nreads __u32, out */
	__u32 nwrites;
	__u32 naddrs;
	__u32 nreads;
	__u32 start;		/* written to the profile's start register, last */
	__u32 timeout_ms;
	__u32 status;		/* out: the completion status the block's interrupt gave */
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

#endif
