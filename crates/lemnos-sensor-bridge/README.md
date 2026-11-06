# lemnos-sensor-bridge

Userspace client for the Lemnos sensor bridge kernel module
(`kernel-modules/lemnos-sensor-bridge`, GPL-2.0, out of tree). The module makes a camera sensor
that a userspace driver runs look like a V4L2 subdevice to the CSI-2 receiver. This crate is the
other half: it opens the bridge's subdev node, publishes the sensor's pad format and timing, and
answers the receiver's stream start and stop requests.

- `SensorBridge` opens a bridge subdev. `set_format`, `set_timing` and `set_power` configure it.
- `SensorBridge::serve` (or `next_request` with `acknowledge`) runs the start and stop handshake.
- `find_bridges` lists the bridges bound on this system, with the sensor name and I²C bus their
  device tree node gives.

The wire contract (the private event, the control ids and the acknowledgement encoding) is
`kernel-modules/lemnos-sensor-bridge/PROTOCOL.md`, protocol version 1. The V4L2 calls come from
`lemnos-v4l2`. This crate has no `unsafe` of its own.

## Licence and the kernel module

This crate is MIT OR Apache-2.0, like the rest of the workspace. It does not link the kernel
module and does not take code from it. It talks to the module only through the uAPI: the
controls, the events and the ioctls of the V4L2 subdevice interface, which the kernel exposes to
any program. The reasoning in `docs/kernel-module.md`, section 4, applies: the GPL-2.0 uAPI
headers are used under the Linux syscall note, and the module's own sources stay in
`kernel-modules/` under GPL-2.0.

## Status

Host tests cover the stream-request and acknowledgement encodings, the sysfs parsing and the
control ids. Nothing here has run against the module on a board yet.
