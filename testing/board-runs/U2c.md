# U2c board run: video input selection on the capture path

Status: untested on board.

The capture path selects the device's input before it negotiates the format. `lemnos-v4l2` gained
`VideoDevice::input` (`VIDIOC_G_INPUT`) and `VideoDevice::enum_input` (`VIDIOC_ENUMINPUT`) beside the
existing `set_input` (`VIDIOC_S_INPUT`). `drivers/sunxi-vin` passes its `input` option (default 0)
through `v4l2-capture`, whose `LinuxHardware::open` calls `set_input` before `set_format`.

No board was reachable from the session that wrote this. A local session checks the following on the
reference A733 board (see `E12.md` for the board lock and the build location).

## What to check

1. Open the sunxi-vin capture node with `input = 0` and with the sensor's input index. Expected: the
   open succeeds, and the format negotiated afterward is the one asked for.
2. Read `VIDIOC_G_INPUT` after the open. Expected: the index that was set.
3. Enumerate inputs with `enum_input` from index 0 until the driver answers `EINVAL`. Expected: one
   entry per sensor input, each with a non-empty name and kind 2 (`V4L2_INPUT_TYPE_CAMERA`).
4. Run the same open with `input` set to an index past the last one. Expected: the open fails with
   the driver's error, and no format is negotiated.

Report the output of each step, with the kernel's `dmesg` lines for the open.
