# S-CAM: does Raze #2 have a CSI camera sensor? (read-only, 2026-11-21)

Board: Raze #2 (CM5 Rev 1.0), serial a317bcbee5226d57, boot b34fee0e-2dc9-4da1-a77f-791d01632baf, kernel 7.2.9-v8-16k.
Rev: origin/next 0d37cc55 (the worktree for pr/RAZE2-SWEEP). Both board locks (`/run/lock/raze-gpu.lock`,
`/run/lock/board-gpu.lock`) were taken with flock over ssh for the read. Nothing was written, no module loaded or
unbound, no service stopped, no reboot.

## Hypothesis (written before the read)

Raze #2 has no camera sensor that answers. The sensor check finds no sensor node, no media device with a sensor
entity, and no OV9782 chip-id read.

## Reads and results

| Read | Result |
|---|---|
| Live device tree (`/proc/device-tree`, camera and CSI names) | `rp1_firmware`, `cam0_clk`, `cam1_clk`, `cam0_reg`, `cam_dummy_reg` are present. The `csi` and `i2c@88000` nodes sit under `/axi/pcie@1000120000/rp1`, and the kernel log shows `ov9782@60` under `i2c@88000` and `csi@110000`. |
| `ls /dev/media* /dev/video*` | `/dev/media0` to `/dev/media3`, `/dev/v4l-subdev0` to `2`, `/dev/video0` to `/dev/video35` exist (rp1-cfe nodes). |
| `media-ctl -p` | Not run: `media-ctl` is not on the image. |
| dmesg and journal (sensor probe) | `ov9282 10-0060: OV9782 compatible reports OV9281 chip id`; `rp1-cfe 1f00110000.csi: found subdevice /axi/pcie@1000120000/rp1/i2c@88000/ov9782@60`; `Using sensor ov9782 10-0060 for capture`. Journal of an earlier boot (its clock was wrong, see below): `capture started camera="ov9782 (kernel driver i2c 10-0060)" mode=NV12 1280x800 fps=30.0` on 2026-11-07. |
| i2c buses and drivers (`/sys/bus/i2c/devices`) | `10-0060 name=ov9782 driver=ov9282`. The address is owned by a kernel driver. |
| Direct chip-id read on the bus | **Not done.** The address is owned by driver `ov9282`, so the bus was not touched, per the ruling: a driver owns the address, read via the driver instead. The driver's own log line above is the chip-id read that exists. |

## Finding

**A sensor is present and bound on Raze #2.** The earlier rule that neither Raze has a camera is wrong for this
board. The image is a Helios image (slot `helios-2026.2.0-dev.993-styx-e1097ec7`), and the camera stack (rp1-cfe,
ov9282 driver for the OV9782) is loaded at boot.

Caveat: the board clock has no RTC and reads 2026-11-08 (board `date -u` on this boot), so the journal dates above
are the board's clock, not the PC's.

## What the camera rows still need (not run, reported first)

Per the ruling, nothing below was run in this sweep. On today's image:

- **PISP-RUN** (row 22) and **R-PASS** (row 23): need a streaming session on `/dev/video0` through the rp1-cfe
  chain with the frames read back. Needs a stopped or shared capture path (the image's `helios-engine` owns the
  capture while it runs), so the image's service must be stopped behind a failsafe or the frames read by it. Report
  first.
- **M34 lent chain** (CZ-MOD lent-frame check): needs the same streaming session plus the M34 CRC reference. Report
  first.
- Also held on the same session: F2g1, F2h3, FLAKE-CSI2, U4, M32 (rows listed in RAZE2-SWEEP.md).

Board state: unchanged. Locks released when each ssh command exited.
