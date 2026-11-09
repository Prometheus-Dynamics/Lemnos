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

#endif
