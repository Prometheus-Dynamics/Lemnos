/* SPDX-License-Identifier: GPL-2.0 */
/*
 * Register jobs inside the module: the engine (lemnos_regjob.c) and what a
 * block's profile gives it (lemnos_regjob_<block>.c).
 *
 * Copyright (c) 2026 the Lemnos authors.
 */
#ifndef LEMNOS_REGJOB_PRIV_H
#define LEMNOS_REGJOB_PRIV_H

#include <linux/types.h>

#include "lemnos_regjob.h"

struct device;
struct platform_device;
struct lem_rj;

/* Registers a caller may set: `count` registers from `offset`, only the bits in `mask`. */
struct lrj_plain {
	u32 offset;
	u32 count;
	u32 mask;
};

/* A register that takes an address: written only by the module, from a held buffer. */
struct lrj_slot {
	u32 reg;
	u32 align;		/* the address's alignment, a power of two */
};

/* The kernel's copy of a job, as a profile's check sees it. */
struct lrj_ctx {
	const struct lemnos_regjob_write *w;
	u32 nw;
	/* By slot, in the profile's order: the checked device address and length. */
	u64 addr[LEMNOS_REGJOB_MAX_ADDRS];
	u64 bytes[LEMNOS_REGJOB_MAX_ADDRS];
};

/*
 * The one value the job writes to `offset`, for a geometry check: -EINVAL when
 * the job doesn't write it or writes it more than once.
 */
int lrj_value(const struct lrj_ctx *c, u32 offset, u32 *value);

u32 lrj_rd(struct lem_rj *rj, u32 offset);
void lrj_wr(struct lem_rj *rj, u32 offset, u32 value);

struct lrj_profile {
	u32 id;			/* LEMNOS_REGJOB_PROFILE_* */
	const char *name;
	u32 window;		/* bytes of the register window jobs and the module use */
	const struct lrj_plain *plain;
	u32 nplain;
	const struct lrj_slot *slots;
	u32 nslots;
	u32 start_reg;
	/* Highest address + bytes a slot's register can hold (1 << 32 for a 32-bit register). */
	u64 addr_limit;
	/* The block keeps using its addresses after a job completes (a capture path). */
	bool continuous;

	/* Whether `value` may start a job. */
	bool (*start_ok)(u32 value);
	/*
	 * Fills need[slot]: the bytes the block will reach from each slot's
	 * address with this job's registers. 0, or -errno to refuse the job.
	 */
	int (*check)(const struct lrj_ctx *c, u64 *need);
	/* Writes the registers the module derives from the checked slots (ends, sizes). */
	void (*derive)(struct lem_rj *rj, const struct lrj_ctx *c);
	/* Before the start: clear and enable the completion interrupt. */
	void (*arm)(struct lem_rj *rj);
	/* In the handler: 1 with *status when the job completed, 0 when not this block's. */
	int (*irq)(struct lem_rj *rj, u32 *status);
	/* Interrupts off. */
	void (*quiet)(struct lem_rj *rj);
	/* After the reset line was pulsed (and at probe): the block's own reset sequence. */
	void (*reset_regs)(struct lem_rj *rj);
};

extern const struct lrj_profile lrj_a733_ve;

/* lemnos_core.c: whether any of the device's compatibles is in the comma-separated pairs. */
bool lem_device_listed(struct device *dev, const char *list);

/* The core's side: NULL when the device is not listed for register jobs. */
struct lem_rj *lem_rj_probe(struct platform_device *pdev);
void lem_rj_remove(struct lem_rj *rj);
long lem_rj_open(struct lem_rj *rj, struct lemnos_regjob_open *o);

#endif
