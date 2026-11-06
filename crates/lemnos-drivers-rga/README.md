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
  and flips and turns (`transform`). Serves no resize. The request layout comes from the librga
  release at commit 331d5750, Apache-2.0 (`lemnos_linux_sys::rga`), with the struct sizes and
  offsets pinned to the header by tests.
- **VGLite (`Vglite`, `/dev/vg_lite`, K230D):** `lemnos-drivers-vglite`, untested on board. Served
  for its rows: clear (fill), blits (resize: bilinear, or the 2x area downscale; crop; flips and
  quarter turns), the blends SRC, SRC_OVER and DST_OVER, and palette. Premultiplied blends and
  gray formats are not served. Lines are not routed: `lemnos-drivers-vglite`'s `line_bytes` draws
  them on the GPU when called directly, but the match to Bresenham is unverified on board (its
  README names the device test that turns the route on).
- **Not served:** Y-only output (RGA's RGB-to-YUV modes do not fit the request's one-byte
  `yuv2rgb_mode`, so no field is verified for them), line drawing (on the multi-RGA node, and on
  VGLite until a board run matches Bresenham), and resizes on the multi-RGA node. A caller sends
  these to the CPU.

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
