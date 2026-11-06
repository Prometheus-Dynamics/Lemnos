# lemnos-drivers-rga

Rockchip's RGA 2D engine: scaling, cropping, pixel-format conversion, flips, quarter turns, fills,
masks, blends, palettes and colour keys. Linux only. Byte buffers work as well as dma-bufs.

**Untested on board.** The V4L2 route is tested against the kernel's `vim2m` test driver; the
multi-RGA route is tested against scalar references when `/dev/rga` exists. Neither ran on a
Rockchip board. Each device test skips where its node is absent.

## Routes

- **V4L2 (`Rga`):** the mainline `rockchip-rga` mem2mem driver, through `lemnos_linux::v4l2::m2m`.
  Convert, scale (area downscale, bilinear upscale), crop, h/v flip and rotate 90/180/270. No
  fill, ROP, blend, palette, colour key, gray format, or user-level filter control: the mainline
  driver stores `V4L2_CID_BG_COLOR` but never programs it.
- **Multi-RGA (`MultiRga`, `/dev/rga`):** the vendor kernel's BSP driver, through `RGA_BLIT_SYNC`
  (`0x5017`) with `struct rga_req`. Covers fill (`MultiRga::fill`), mask ROP (`rop`), blend
  (`blend`), 8-bit palette (`palette`), colour key (`color_key`), gray to RGBA (`gray_to_rgba`),
  one channel of RGBA8888 to gray (`channel_to_gray`, below), and flips and turns (`transform`). Serves no resize. The request layout comes from the librga
  release at commit 331d5750, Apache-2.0 (`lemnos_linux_sys::rga`), with the struct sizes and
  offsets pinned to the header by tests.
- **VGLite (`Vglite`, `/dev/vg_lite`, K230D):** `lemnos-drivers-vglite`, untested on board. Served
  for its rows: clear (fill), blits (resize: bilinear, or the 2x area downscale; crop; flips and
  quarter turns), the blends SRC, SRC_OVER and DST_OVER, and palette. Premultiplied blends and
  gray formats are not served. Lines are not routed: `lemnos-drivers-vglite`'s `line_bytes` draws
  them on the GPU when called directly, but the match to Bresenham is unverified on board (its
  README names the device test that turns the route on).
- **Channel to gray (`MultiRga::channel_to_gray`):** `dst[i] = src[4 * i + c]` for R, G or B,
  meant to be bit-exact, not luma. It programs the request's user colour-space conversion
  (`full_csc`, RGA2 cores only; the vendor driver gives RGA3 no `RGA_FULL_CSC`, so on an RK3588
  the scheduler picks the RGA2 core) with a Y row of 1023/1024 on the channel and an offset of
  383/1024. That is exact for every byte whether the hardware truncates or rounds half up, and
  the clip is the kernel's default 0 to 255. Untested on board, and `caps` does not route it
  (`FixedOp::Channel`) until `tests/multirga.rs`'s `a_channel_to_gray_is_exact_for_every_byte`
  passes on one. The mainline V4L2 driver cannot do it: it has no gray or Y-only format, and its
  colour conversion is the fixed BT.601 or BT.709 mode.
- **Not served:** Y-only output as luma (RGA's fixed RGB-to-YUV modes do not fit the request's
  one-byte `yuv2rgb_mode`, so no field is verified for them), line drawing (on the multi-RGA node, and on
  VGLite until a board run matches Bresenham), and resizes on the multi-RGA node. A caller sends
  these to the CPU.
- **CPU (`cpu`):** what a caller does when no route serves. Four operations are written so far:
  - `channel_to_gray`, `gray_to_rgba`, `color_key` (giving an 8-bit mask) and `rop`;
  - all over strided images, as scalar loops.

  On riscv64 Linux whose kernel offers the V extension, each row runs as RVV code from
  `lemnos_linux_sys::rvv` instead, and `cpu::vector()` says so. The RVV rows were written from the
  RVV 1.0 specification for the K230's C908. `scripts/rvv-qemu.sh` runs them under qemu at VLEN
  128, 256 and 512 against the scalar loops. Untested on board.

  Not written yet:
  - conversions and resizes, which need their colour matrices and rounding defined first;
  - lines, which stay with the caller: one pixel a step gives a vector unit nothing to do.

## Choosing a route

`caps::route_for(soc, op, src, dst)` answers which route serves an operation from the SoC table.
Formats and sizes are checked for the V4L2 route; the multi-RGA route checks its own formats when
it runs. `route_on_system` also needs the device node. `None` means the CPU.

## Unverified field mappings

These are marked in the docs of each operation. The header does not state what the field means,
so the value is the crate's assumption until a board run confirms it:

- **Fill:** colour byte order (`0xAABBGGRR`), and whether the active rectangle is the fill area.
- **Mask ROP:** whether `rop_code` takes the ROP3 values of `im2d_type.h`.
- **Blend:** the Porter-Duff values of `pd_mode` (`SrcOver` 1, `Src` 0, `DstOver` 2), and the
  encoding of the premultiplied flag in `alpha_rop_mode`.
- **Palette:** the 256-entry table's entry layout (`[r, g, b, a]`).
- **Colour key:** the enable bit and inversion mode (the header gives the range only).
- **Gray to RGBA:** the chroma of a Y-only source is neutral; `yuv2rgb_mode` 1 is full range.
- **Channel to gray:** which `full_csc` coefficient meets the first byte of RGBA8888 (the vendor
  table names it R), the coefficient register's width (librga never sends more than 1023), and
  the rounding.
- **Transforms:** which mirror is horizontal, and the turn direction (clockwise assumed).
- **Buffers:** a dma-buf descriptor goes in the address field with the MMU disabled; a byte buffer
  goes in at its virtual address with the MMU enabled. The header gives the flags but not this use.
- **Fences:** unused fence descriptors are `-1`.

## Filters

`Filter::Area` is an integer downscale (each output the rounded mean of its block, a trailing
partial block cropped away). `Filter::Bilinear` is a bilinear resize. The mainline driver has no
filter control, so it refuses a bilinear downscale and an area resize that is not an integer factor
with `Unsupported` instead of substituting a filter.

## Names

`Librga` is a deprecated alias of `MultiRga`, kept for one release. The vendor library it named is
no longer loaded: nothing here calls it, and there is no `dlopen`.

The V4L2 session is Lemnos's (`lemnos-linux`, `v4l2::m2m::M2mSession`).
