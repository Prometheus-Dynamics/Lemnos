# lemnos-ov9782

`lemnos-ov9782.ko`: Lemnos's own V4L2 sensor-subdevice driver for the OmniVision OV9782. GPL-2.0,
like the rest of `kernel-modules/`.

## Where it comes from

A fork of the kernel's `drivers/media/i2c/ov9282.c` (7.2.9) with the OV9782 variant patches the
camera images carried on top of it. The fork keeps only the OV9782: its compatible
(`ovti,ov9782`), registers, modes and controls are exactly as the patched driver had them. RAW8
stays at 96 MHz (`0x030D` = `0x60`). Only the module name (`lemnos-ov9782`), the driver name and
the module description differ.

The baseline was checked: the patched upstream source gives the same `srcversion` as the stock
module on the board (`srcversion` hashes the source only), so the fork starts from what runs today.

## Status

Builds clean (`W=1`, 31 440 B unstripped, 16 469 B of text and data) against the image's own
built kernel tree. Not shipped in any image yet.

**Parity with the stock module, board #1** (7.2.9-v8-16k, 2026-11-08, two passes): the module
probes, the subdevice is named `ov9782 10-0060`, and the receiver takes it with no kernel warnings.
Every control (range, default, flags, value), mbus code, frame size, active format and selection
is identical to the stock module's, compared with both in the same state: after the camera
service had configured the sensor for 1280x800 8-bit at 120 fps.

| Driver | Frames | Seconds | fps | Delivered | Sequence gaps | Corrupted | ISP skipped |
|---|---|---|---|---|---|---|---|
| stock `ov9282` | 17 443 | 145.38 | 119.98 | 17 390 | 0 | 0 | 0 |
| `lemnos-ov9782` | 4 191 | 34.94 | 119.96 | 4 171 | 0 | 0 | 0 |

These are separate runs of different lengths through the camera service. Not compared: pixel
content, per-frame timing, and the register writes themselves (the board has no regmap debugfs for
this device). Before streaming, a freshly probed sensor has the driver's defaults (10-bit, 160 MHz
pixel rate), so a dump from then does not match one taken after the service ran.

Build against the tree that produced the image, not another tree of the same release. A build
against a differently configured 7.2.9 tree had the same vermagic and was still refused at load
(`.gnu.linkonce.this_module section size must match`, `insmod`: `Invalid module format`).

## Parity procedure

One board, under the board lock, restoring by reboot.

Do not unbind and rebind the sensor under a loaded CSI-2 receiver (`rp1-cfe`). Measured on board
#1: one rebind logs `media_device_register_entity` warnings (`mc-device.c:619`) as the receiver
registers its nodes again, and the next one is a kernel NULL dereference under
`__v4l2_async_register_subdev_sensor`. That is with the stock sensor driver and the stock receiver,
so it says nothing about this module, but it rules out swapping the two back and forth.

In one pass from a fresh boot, with the camera service having run:

1. Load test: `insmod lemnos-ov9782.ko` then `rmmod` it. The stock driver keeps the device, so
   nothing probes and a bad build is refused here with nothing changed.
2. Stop the camera service and dump the subdevice.
3. `rmmod` the receiver, then the stock sensor driver (the receiver holds a reference on it).
4. `insmod lemnos-ov9782.ko`, then load the receiver again, so its notifier starts fresh.
5. Start the camera service, let it capture, stop it, dump the subdevice and compare.
6. Reboot to restore.

## Next, after parity

- Modes and registers from one description (the description-only driver form), so the camera
  stack's copy of the tables goes away.
- 2x2 binning at 640x400, fractional exposure, group hold (`0x3208`), a test-pattern control,
  and an OTP read (read-only).

## Build

```
make KDIR=<the image's built kernel tree> ARCH=arm64 CROSS_COMPILE=<the image's toolchain prefix>
```
