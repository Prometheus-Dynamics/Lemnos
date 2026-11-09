/* SPDX-License-Identifier: GPL-2.0 WITH Linux-syscall-note */
/*
 * A fake camera receiver, /dev/lemnos-test-csi2, in the test block module: the
 * registers are memory the guest set aside, and it latches, writes frames and
 * raises interrupts when told to. Copyright (c) 2026 the Lemnos authors.
 */
#ifndef LEMNOS_TEST_CSI2_H
#define LEMNOS_TEST_CSI2_H

#include <linux/ioctl.h>
#include <linux/types.h>

/* The front end's window is 4K at csi2_phys + 12K: tcsi_fe_peek reads a register of it. */
struct tcsi_fe_peek {
	__u32 offset;	/* in */
	__u32 value;	/* out */
};

struct tcsi_regs {
	__u32 addr0, addr1, length, stride, ctrl, frame_size, debug, reserved;
};

#define TCSI_FS		0x1u
#define TCSI_FE_ACK	0x100u

/* FIRE: sets the status word the handler will read, and raises the interrupt. */
#define TCSI_IOC_FIRE	_IOW('T', 1, __u32)
#define TCSI_IOC_PEEK	_IOR('T', 2, struct tcsi_regs)
/* After FORCE, the channel's line counter moves for this many ms. ~0: never stops. */
#define TCSI_IOC_BUSY	_IOW('T', 3, __u32)

#define TFE_EOF		0x1u
#define TFE_SOF		0x2u

/*
 * The front end's interrupt: a start of frame latches the addresses the
 * configuration registers hold, an end of frame writes them, bounded by the
 * configuration's own sizes, as the block would.
 */
#define TCSI_IOC_FE_FIRE	_IOW('T', 4, __u32)
#define TCSI_IOC_FE_PEEK	_IOWR('T', 5, struct tcsi_fe_peek)
/* Sets the front end's STATUS register directly. */
#define TCSI_IOC_FE_STATUS	_IOW('T', 6, __u32)
/* STATUS reads busy for this many ms. ~0: never idle. */
#define TCSI_IOC_FE_BUSY	_IOW('T', 7, __u32)

#endif
