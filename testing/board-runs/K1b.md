# K1b board run: the JPEG encoder (the VPU720 class) through `rk-mpp` (`lemnos-driver-rkjpege`)

Status: untested on board. Nothing here was read or written on a device. The file is pinned to the
vendor header with gcc and clang (`drivers/rkjpege/src/tests/pins.rs`), and the job is checked on
rk-mpp's played kernel (`drivers/rkjpege/tests/builder.rs`, `drivers/rk-jpeg/src/encoder_tests.rs`).

The run needs a board whose vendor kernel exposes the JPEG encoder on `/dev/mpp_service`, bound to
the encoder's platform driver (`rockchip,rkv-jpeg-encoder-v1`, device type 20, 79 registers). Build
on NVMe or SSD, never on the Github HDD. Module loads and unbinds are allowed; report each.

The job is built by `RegisterSet` in this order, which the vendor HAL (`hal_jpege_vpu720.c`) also
follows: the picture (`set_picture`, encoded size rounded up to 16), the source (`set_source`, with
its strides), the quantisation table, the bitstream's four pointers (top at offset 0, bottom below
the buffer's size, read at 0, start at the header's length), and then `job`. The kernel sets the
start bit itself, last.

## Checks

1. **Identity.** Read `reg000_version` (word 0, `IP version`) through the service once the claim is
   bound, and record it with the kernel version and the device tree compatible. The sources give no
   expected value, so this check passes when the read completes and the value is recorded; a value
   that does not match the one the board's vendor kernel reports (its hardware id) stops the run.
2. **One encode.** Write the header bytes of a baseline JPEG (SOI through SOS, the caller's own
   writer) into the bitstream from top up to the start pointer, and fill the quantisation table
   buffer with 384 bytes as `hal_jpege_vpu720.c` writes its `qtbl_sw_buf` (three tables of 64
   entries, 16 bits each, the 0x8000 divided by the quality table, transposed). Encode a 64 x 48
   4:2:0 picture (`Source::Yuv420Sp`, luma stride 64, chroma stride 64) from a sealed buffer
   holding a flat grey picture. Pass: the job completes with no fault, and the interrupt state
   (word 7, read back) shows `fenc_done` (bit 0) and none of the error bits (`0x340`).
3. **Stream length.** The bitstream's write pointer (`st_adr_jbsbw`, word 67, read back) minus the
   top address is the stream's length. Pass: the length is greater than the header length, is less
   than the bitstream buffer's size, and the two bytes before the end are `FF D9` (EOI, which the
   block adds on the last ECS, `pic_last_ecs`). The header bytes before the start pointer are
   unchanged.

## Not checked here

- The ECS length buffer (`reg021_adr_ecs_len`) is not placed: the vendor HAL leaves it unset on the
  frame-by-frame path. A run with `out_mode` set (low latency, one interrupt per ECS) would need it.
- How far the block reads past the encoded picture (the frame's padding rows) is not in any source.
  The builder checks the encoded picture only, so a run passes only if the block stays inside it.

Record each result with the kernel version and the board's device tree compatible string.
