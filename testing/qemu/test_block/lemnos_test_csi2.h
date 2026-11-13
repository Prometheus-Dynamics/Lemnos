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
/* The embedded data channel (channel 1): its start and end, in the same status word. */
#define TCSI_EMB_FS	0x2u
#define TCSI_EMB_FE_ACK	0x200u

/* FIRE: sets the status word the handler will read, and raises the interrupt. */
#define TCSI_IOC_FIRE	_IOW('T', 1, __u32)
#define TCSI_IOC_PEEK	_IOR('T', 2, struct tcsi_regs)
/* After FORCE, the channel's line counter moves for this many ms. ~0: never stops. */
#define TCSI_IOC_BUSY	_IOW('T', 3, __u32)

/* The embedded channel's registers, read as the image channel's (TCSI_IOC_PEEK). */
#define TCSI_IOC_PEEK_EMB	_IOR('T', 11, struct tcsi_regs)
/*
 * How many embedded lines the next embedded frame end adds to the channel's
 * line count (one, as a meta node's frame is one line, when not set). 0: the
 * frame had no embedded line, and its buffer is not written.
 */
#define TCSI_IOC_EMB_LINES	_IOW('T', 10, __u32)

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
/*
 * A frame end adds the lines the channel wrote to the line count (the low half
 * of DEBUG), the frame size's height when armed and none when not, as the
 * block does. LINES makes the next armed frame end add this many instead.
 */
#define TCSI_IOC_LINES	_IOW('T', 8, __u32)
/*
 * The front end caught mid-frame: STATUS reads queued and active until a
 * RESET is written to CONTROL, or, with TFE_ABORT_WHILE_FED, until an ABORT
 * is written while the channel isn't forced (still feeding the block). 0 ends
 * it.
 */
#define TFE_RESET_ONLY		0x1u
#define TFE_ABORT_WHILE_FED	0x2u
#define TCSI_IOC_FE_MIDFRAME	_IOW('T', 9, __u32)

#endif
