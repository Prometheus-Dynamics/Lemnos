# U2d board run: listing the capture node's video inputs

Status: untested on board.

`lemnos-v4l2` lists a node's inputs with `VideoDevice::inputs` (`VIDIOC_ENUMINPUT` from index 0 until
the driver answers `EINVAL`) and reads the selected one with `VideoDevice::input` (`VIDIOC_G_INPUT`).
The `camera.receiver/1` contract answers both as methods 13 (`inputs`, records of `INPUT_RECORD`
bytes, read with `read_input`) and 14 (`input`). `sunxi-vin` serves them from its capture node
through `v4l2-capture`. Only the mock and a regular-file node (every ioctl `ENOTTY`) were run here.

No board was reachable from the session that wrote this. A local session checks the following on the
reference A733 board (see `E12.md` for the board lock and the build location).

## What to check

1. Call `inputs` on the sunxi-vin receiver with the node closed (before `start`). Expected: one record
   per sensor input, in index order, each with a non-empty name and kind 2 (`V4L2_INPUT_TYPE_CAMERA`).
   Its `kind` and `name` match `v4l2-ctl --list-inputs` on the same node.
2. Call `input` before and after `start` with the `input` option set to 0, then to the sensor's
   index. Expected: the index selected, each time.
3. Call `inputs` on a receiver whose node is missing. Expected: `NO_DEVICE`, not an empty list.
4. Report the output of each step, with the kernel's `dmesg` lines for any refused ioctl.
