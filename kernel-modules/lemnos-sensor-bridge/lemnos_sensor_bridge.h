/* SPDX-License-Identifier: GPL-2.0 WITH Linux-syscall-note */
/*
 * Lemnos sensor bridge: userspace ABI.
 *
 * The bridge is a V4L2 subdevice that stands in for a camera sensor driver.
 * Userspace drives the sensor over I2C and uses this ABI to keep the CSI-2
 * receiver's start/stop order. See PROTOCOL.md next to this file.
 *
 * Mirrored in Rust by lemnos-sensor-bridge::bridge; keep both in sync.
 */
#ifndef _UAPI_LEMNOS_SENSOR_BRIDGE_H
#define _UAPI_LEMNOS_SENSOR_BRIDGE_H

#include <linux/types.h>
#include <linux/videodev2.h>

#define LEMNOS_BRIDGE_PROTOCOL_VERSION	1

/* Private event: the receiver started or stopped the stream. */
#define LEMNOS_BRIDGE_EVENT_STREAM	(V4L2_EVENT_PRIVATE_START + 0x5354)

/* struct lemnos_bridge_stream_event.action */
#define LEMNOS_BRIDGE_ACTION_START	1
#define LEMNOS_BRIDGE_ACTION_STOP		2

/* struct lemnos_bridge_stream_event.flags */
#define LEMNOS_BRIDGE_FLAG_CONTINUOUS_CLOCK	(1U << 0)

/*
 * Payload of LEMNOS_BRIDGE_EVENT_STREAM, in v4l2_event.u.data (64 bytes,
 * little-endian on the targets we support). Snapshot of the active pad format
 * and timing controls at the moment the receiver asked for the stream.
 */
struct lemnos_bridge_stream_event {
	__u32 version;		/* LEMNOS_BRIDGE_PROTOCOL_VERSION */
	__u32 action;		/* LEMNOS_BRIDGE_ACTION_* */
	__u32 sequence;		/* echo in the acknowledgement */
	__u32 timeout_ms;	/* how long the bridge waits for the ack */
	__s64 link_freq;	/* Hz, current V4L2_CID_LINK_FREQ entry */
	__s64 pixel_rate;	/* pixels per second, V4L2_CID_PIXEL_RATE */
	__u32 code;		/* media bus code of the source pad */
	__u32 width;
	__u32 height;
	__s32 hblank;
	__s32 vblank;
	__u32 data_lanes;
	__u32 flags;		/* LEMNOS_BRIDGE_FLAG_* */
	__u32 reserved;
};

/* Private controls (user class). */
#define LEMNOS_CID_BASE			(V4L2_CID_USER_BASE + 0x1f00)
/*
 * Integer64, write-only in practice (execute on write). Acknowledges a
 * stream event: bits 0..31 = sequence, bits 32..63 = status (0 = done,
 * otherwise a positive errno describing why the sensor failed).
 */
#define LEMNOS_CID_STREAM_ACK		(LEMNOS_CID_BASE + 0)
/* Integer, read-only, volatile: enum lemnos_bridge_stream_state. */
#define LEMNOS_CID_STREAM_STATE		(LEMNOS_CID_BASE + 1)
/* Integer, 10..10000 ms: how long start/stop wait for userspace. */
#define LEMNOS_CID_ACK_TIMEOUT_MS		(LEMNOS_CID_BASE + 2)
/* Boolean: enable (1) or disable (0) the DT supplies and clock. */
#define LEMNOS_CID_POWER			(LEMNOS_CID_BASE + 3)
/* Integer, read-only, volatile: sequence of the last stream event. */
#define LEMNOS_CID_STREAM_SEQUENCE	(LEMNOS_CID_BASE + 4)

enum lemnos_bridge_stream_state {
	LEMNOS_BRIDGE_STATE_IDLE = 0,
	LEMNOS_BRIDGE_STATE_STARTING = 1,
	LEMNOS_BRIDGE_STATE_STREAMING = 2,
	LEMNOS_BRIDGE_STATE_STOPPING = 3,
	/*
	 * A start failed (no or a failing acknowledgement) and the receiver was
	 * told it succeeded (report_start_errors=0): the receiver streams, the
	 * sensor does not. Userspace stops the receiver; that ends it (no stop
	 * request).
	 */
	LEMNOS_BRIDGE_STATE_START_FAILED = 4,
};

#define LEMNOS_BRIDGE_ACK(seq, status) \
	((((__u64)(__u32)(status)) << 32) | (__u64)(__u32)(seq))

#endif /* _UAPI_LEMNOS_SENSOR_BRIDGE_H */
