# H5a board run: 16-bit capture, frames in planes, and capture on standby

Status: untested on board. Everything here passes its tests against stand-in nodes and sealed
memfds standing in for dma-bufs (`drivers/rkcif/src/tests/raw.rs`,
`drivers/sunxi-vin/src/tests/direct.rs`, `engine/host/tests/stream_handles.rs`,
`engine/client/tests/stream.rs`). No video node has been opened by this code, and no private
`rkcif` request has reached a vendor kernel.

Run in a local session after this PR merges. Take the board lock named in `AGENTS.md`. Build on
NVMe or SSD, never on the Github HDD. Module loads, unbinds and rebinds are allowed; report each.

## Source of the interface facts

The vendor kernel, rockchip-linux/kernel branch `develop-6.1` (GPL-2.0; facts only, no code
copied):

- `include/uapi/linux/rkcif-config.h`: `RKCIF_CMD_GET_CSI_MEMORY_MODE` is
  `_IOR('V', BASE_VIDIOC_PRIVATE + 0, int)` (0x800456c0), `RKCIF_CMD_SET_CSI_MEMORY_MODE` is
  `_IOW('V', BASE_VIDIOC_PRIVATE + 1, int)` (0x400456c1); compact 0, low-aligned word 1,
  high-aligned word 2.
- `drivers/media/platform/rockchip/cif/capture.c`: `rkcif_ioctl_default` (the set request does
  not work the format out again, so it comes before `VIDIOC_S_FMT`) and `rkcif_set_fmt` (line
  lengths, compact and not).

## 1. Unpacked16 on an `rkcif` capture node

On a Rockchip board running the vendor kernel, with `rkcif` bound, a camera whose kernel sensor
driver probes at 10 or 12 bits, and the device's `capture` naming the routed channel's video node.

1. Before Lemnos: `v4l2-ctl -d <node> --get-fmt-video` and record the stride (compact).
2. A board file with the receiver's `frames` stream (`memory = "dma-buf"`, `buffer_bytes` at
   least `ALIGN(width * 2, 8) * height`), then `configure` with `Packing::Unpacked16` and
   `start`. Pass: the start succeeds; `strace -e ioctl -p <lemnosd>` shows `0x400456c1` with 1
   before `VIDIOC_S_FMT`; `v4l2-ctl --get-fmt-video` while running shows the 16-bit format and a
   stride of `ALIGN(width * 2, 8)`.
3. A consumer reads a frame and checks every 16-bit word is below `1 << depth` (the samples sit
   in the low bits). Pass: no word has a bit set above the depth; the picture is the sensor's.
4. `configure` with `Packing::Packed`, `start` again. Pass: `0x400456c1` with 0, the stride is
   `ALIGN(width * depth / 8, 256)`, and frames are compact.
5. On a chip without compact on the MIPI path, step 4 is refused with `NOT_SUPPORTED` and the
   reason, never a frame of the wrong layout. Record which chip.
6. A kernel without the request (`ENOTTY`): `Packed` starts, `Unpacked16` is refused. Only if
   such a kernel is at hand.

## 2. A frame in planes, straight into the stream

On a board whose capture node writes a frame in more than one memory plane (`v4l2-ctl
--get-fmt-video-mplane` shows `num_planes` above 1, for example a YUV format on the vendor
`vin` node), with the receiver's `frames` stream given `memory = "dma-buf"` and
`planes = [..]` equal to the node's `sizeimage` per plane.

1. Pass: the device binds; `strace` shows one `VIDIOC_QBUF` per slot with `length` equal to the
   plane count, `memory` `V4L2_MEMORY_DMABUF`, and one dma-buf descriptor per plane, which are
   the stream's own (compare with `/proc/<lemnosd>/fdinfo`). No `V4L2_MEMORY_MMAP` request is
   made, so nothing is copied.
2. A consumer reads plane 0 and plane 1 with `FrameStream::plane` and checks each against a
   frame grabbed with `v4l2-ctl --stream-mmap` at the same settings (same luma, same chroma).
3. Give the stream one plane fewer than the node: the device runs by copy (`strace` shows
   `VIDIOC_REQBUFS` with `V4L2_MEMORY_MMAP`), and frames are still whole.
4. Give a plane a length one byte short of the node's: nothing is queued, `errors` rises, the
   stream stays empty, and the device keeps running.

## 3. Standby

The same receiver, its `frames` stream with `standby = true`.

1. Start the device with no consumer attached. Pass: `counters` shows frames delivered while
   nobody is attached, and `/proc/<lemnosd>/fdinfo` shows the stream's dma-bufs held.
2. Attach a consumer. Pass: its first `try_next` after attaching returns a frame at once, its
   timestamp within one frame period of the attach, and the node still runs on
   `V4L2_MEMORY_DMABUF` with no `VIDIOC_REQBUFS` between the attach and that frame.
3. Detach and attach again. Pass: the second consumer's first frame is newer than anything the
   first one read; the first consumer's dma-bufs are let go (`fdinfo`) once the node has been
   queued on fresh memory.
4. Leave it running with nobody attached for ten minutes. Pass: `lemnosd`'s resident memory
   does not grow.
