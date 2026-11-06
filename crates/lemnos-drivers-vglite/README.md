# lemnos-drivers-vglite

The K230's VeriSilicon VGLite 2.5D GPU (GCNanoUltraV; the K230D's 2D engine) on `/dev/vg_lite`,
driven directly: the kernel request is made from Rust, and the vendor's `libvg_lite.so` is
neither loaded nor linked.

**Untested on board.** Nothing here has run on a K230 or K230D. The host tests check the command
words against the vendor library's own output; the device tests run only where `/dev/vg_lite`
exists and skip elsewhere.

## Operations

Ported (phase 1):

| Operation | Stream | Notes |
|---|---|---|
| `resize_*` | blit, bilinear (or the half-phase 2x for `Filter::Area`) | Area is only 2x: VGLite has no averaging filter |
| `crop_*` | point blit with a translation | |
| `transform_*` | point blit with a mirror or rotation matrix | a quarter turn sets the rotated-blit transparency bit |
| `fill_*` | clear: pixel-engine clear for a full-width band from the corner, rectangle otherwise | L8 and Index8 are `Unsupported` (VGLite converts luma with a float sum) |
| `blend_*` | blit with SRC, SRC_OVER or DST_OVER, straight alpha | premultiplied SRC_OVER and DST_OVER are `Unsupported` |
| `palette_*` | 256-entry colour table, then an identity blit of an `Index8` source | |

Ported (phase 2):

| Operation | Stream | Notes |
|---|---|---|
| `line_*` | stroke draw (`vg_lite_draw`'s stroke branch, quality LOW: no antialiasing) of an outline from `stroke.rs` | a width-1 square-capped stroke from pixel centre to pixel centre, both ends included. Not shown to equal Bresenham on board; not routed by `lemnos-drivers-rga` (see below) |

The stroke port (`src/stroke.rs`) is the vendor's `vg_lite_update_stroke` for open polylines with
BUTT and SQUARE caps and MITER and BEVEL joins, closed subpaths, and the straight-line case. The
line operation uses one segment (its public API has no width, caps or joins, so the crate fixes
them: width 1, square caps, and a miter limit of 4, which does not matter for two points).
Not ported, and refused as `Unsupported` where they would be needed: round caps and joins
(`_convert_circle_arc`, the pie areas of `_draw_swing_pie_area`), curved and arc input, dashes, and
the swing handling of curved joints (`need_to_handle_swing`, which straight input never sets).

**Path data is inline.** The vendor's stroke draw passes the outline inline in the command
buffer (`push_data`), not by address; the port does the same, so no GPU memory is allocated for
it. The outline is bounded by the command buffer (128 KiB; a larger one is `OutOfMemory`). The
vendor's uploaded-path branch (`push_call`) is not used. In that branch the vendor draws the
fill data for a stroke, which this port does not reproduce, since it never uploads a path.

**Fresh state.** The vendor skips a register write it believes unchanged, and every second
parallel window it adds a `0x0E02`/`0x0F00` flush (`path_counter`). Both depend on what earlier
draws left in the context. The port writes every value in full and no flush, and each
`Context::run` waits for the GPU; the goldens are the vendor's fresh-state streams.

Not in the port, and no operation here needs them: path fill (`vg_lite_draw` of a fill path),
the linear and Gaussian filters, YUV and planar formats, 24-bit RGB, tiled and compressed
buffers, scissor, global alpha, the mirror and gamma state, and the flexa and multi-context
calls. Each is either absent from the crate's operations or refused as `Unsupported`.

## How a call runs

Each operation takes the process's one context (the vendor library also keeps one), builds its
surfaces, encodes its stream (checked as it is built, below), runs it with the flush and the end
command and waits for the GPU, and copies a staged result out. A contiguous dmabuf is mapped in place; a byte buffer, a
scattered dmabuf, or a row whose stride VGLite cannot take is copied through a contiguous
staging buffer, and `Transfer::staged_bytes` says how much was copied. An operation that draws
nothing (its matrix maps it outside the target) runs nothing and leaves the output alone.

The target is programmed in full on every operation. The vendor library skips that when the
target is the one it programmed last; the state is the same, and the words differ only in that
repetition.

## Unsafe code and the command stream

The crate has none (`#![forbid(unsafe_code)]`). The unsafe surface is in
`lemnos-linux-sys::vg_lite`:

- `Device::open` opens `/dev/vg_lite` and maps its 32 MiB window at offset zero, as the vendor
  library does before its first request.
- `Device::initialize`, `identification`, `allocate`, `map_dmabuf`, `Context::run`, and the
  release calls (`Free`, `Unmap`, `Terminate`) are the `VG_LITE_IOCTL` requests. Each pointer a
  request hands the kernel is a value the call owns (the kernel context storage, freed with the
  context) or memory the call borrows, with its length checked.
- `Context::run` copies a stream's words into a command buffer the kernel mapped, cleans them,
  submits them and waits for the GPU before it returns. The wait is inside the call, so every
  buffer the stream names is still borrowed while the GPU may touch it.
- `Memory` is freed or unmapped when dropped, and `map_dmabuf` documents that the kernel keeps
  the logical address until then, so the caller drops the `Memory` before the mapping.
- `clean_invalidate_dcache` runs the C910's cache maintenance on riscv64 and does nothing
  elsewhere.

A `Stream` is the only way to submit, and it is checked as it is built:

- **Addresses come from buffers.** `render_target` and `source_surface` take a `Buffer` (the
  kernel's `Memory`, or the host-only `Scratch`, and the trait is sealed so no other type can
  name an address) and a `Layout`. The stream refuses a layout whose rows do not fit the stride,
  whose `stride * height` exceeds the buffer, or whose span runs past the 32-bit GPU address
  space. Offsets are not part of the API: a buffer's address is its first byte.
- **Registers.** `plain` writes only the registers the encoder writes with a caller's value;
  `zero` only the registers the vendor writes as zero (0x0A27, 0x0A2D, 0x0A34, 0x0A39, 0x0AD1). The address-carrying registers (0x0A11,
  0x0A29, the tessellation addresses, the colour table) are written only by the typed methods.
- **Tessellation** addresses come from the context's own tessellation buffer, not the caller.
- **Rectangles and clears** must lie inside the render target's size, and a pixel-engine clear
  may not exceed its extent. Whether the GPU also clips its writes to that size, not only to the
  extent of the memory, is unverified on board; this crate relies on the extent check.
- **Raw words** (`push_raw`) accept only plain or zero-valued register writes and the flush
  sequence. A data header, a colour-table run, an end command, an address register or an unknown
  opcode is refused, and a refused slice adds nothing.
- **Source reads** are bounded by the source's declared size and stride. The GPU's sampling at
  the edges of a source (bilinear taps, a rotated matrix's corners) is assumed to stay inside that
  size, as its width and height registers say; unverified on board.
- **Inline path data** (`path_data`) is whole words, non-empty, and fits in the command buffer
  with its header: a larger outline is `OutOfMemory` and adds nothing. It carries no address.
- **Tessellation window** (`tessellation_window`) lies inside the render target, and its size
  register takes the context's own tessellation buffer, not the caller's.
- **Running** (`Context::run`) refuses a stream that is not finished, one that names a `Scratch`,
  and one larger than a command buffer.

## Unverified values

Stroke draws add these, unmeasured on a K230: the path tessellation control word (0x0A34: format
0x300000 for FP32, quality 3 for `VG_LITE_HIGH`, stroke bit 0x200), the path scale and bias
(0x0A3B, 0x0A3C, named so in the vendor), the path matrix (0x0A40 to 0x0A45), the matrix's
translation words (0x0ACD, 0x0ACE: the vendor pushes them; their meaning is not in the source),
the window's origin and size (0x0A39, 0x0A3A) and tessellation size per window (0x0A3D, the
context's buffer in 64-byte units). The stroke geometry is the vendor's, exact on the host; what
the GPU draws with it, including whether a line's pixels equal Bresenham's on each slope, and the clipping of a line that runs
past the target, is not measured.

Each of these was read from the vendor source or inferred, and was not confirmed on a K230. The
source-level meaning is the vendor's own; the hardware meaning is not.

- **Register addresses and bit fields** in the streams are the vendor's. Those with no name in
  `vg_lite.c` (0x0A27, 0x0A2D, 0x0A34, 0x0A39, 0x0AD1, and the bits of 0x0A00 and 0x0A25 beyond
  those the source names) are used for what the code around them does, unmeasured.
- **Chip identity:** the registers 0x20 (id), 0x24 (revision) and 0x30 (CID), and the values
  0x265, 0x1003 and 0x417 that `vg_lite_options.h` builds for. Those are compared at open.
- **The kernel side of every request** is not in the vendor tree: `VGLiteKernel` has only the
  user side of `vg_lite_kernel`. What the kernel does with `CHECK` (only the three identification registers are asked for), with the window mapping, with
  `INITIALIZE`'s returned sizes, and with `ALLOCATE`'s flags (the port sets them to zero; the
  vendor leaves them unset) is unverified.
- **Cache maintenance:** the `th.dcache.civa` encoding `0x0275000b` is carried from the earlier
  crate; the vendor's source uses the mnemonic and this host cannot assemble it.
- **Float steps:** the golden streams come from an x86-64 build with FMA contraction off. The
  K230's build may contract multiply-adds in the step set-up (GCC's default is fast contraction),
  which could move a step by one unit in the last place.
- **Limits:** a stream larger than one command buffer (128 KiB) is refused with
  `OutOfMemory`; the vendor splits it. No operation here comes near that (the palette stream is
  about 1.3 KiB).

## Lines and Bresenham

The line op's contract is Bresenham's pixels, both ends included. The GPU draw is binary (quality
LOW), so an axis or 45-degree line should cover exactly those pixels, and the stroke's pixel
centres are on the line. Other slopes may cover one pixel more than Bresenham (a neighbour whose
centre is within half a width of the line), so the draw is not proven equal to Bresenham there.
The line op is therefore not routed by `lemnos-drivers-rga`'s caps. To turn it on, run
`tests/lines.rs` (`gpu_lines_against_bresenham_by_slope_class`) on a K230: it draws every line of
Chebyshev radius 1 to 64 from the frame centre, in both directions, and asserts axes and 45
degrees exactly, printing the mismatch count for other slopes. Serve lines when that count is
zero for every class. The reference there is the variant documented in that file (its tie rule
and the direction dependence).

## Tests

- `src/stroke_tests.rs`: the outlines of 34 fixtures (open and closed, BUTT, SQUARE, MITER, BEVEL,
  the miter fallback and the bevel controls) against the vendor's `vg_lite_update_stroke` output,
  byte for byte, and the draw streams (`DRAW_O_*`) of the open ones, word for word. The
  round-cap and round-join fixtures are checked to be `Unsupported`. The oracle was built from
  the vendor sources unmodified, with the kernel ioctl replaced by a recorder, `-O2` and `-O0`,
  `-ffp-contract=off`; both gave the same bytes.
- `tests/lines.rs`: the reference Bresenham on hand-checked lines (host), and the device comparison
  described above (skips without `/dev/vg_lite`).
- `src/tests.rs`: the streams of init, clear, blit (identity, crop, flip, quarter turn, bilinear
  half-scale), blend (SRC_OVER, DST_OVER) and palette, compared word for word with the library's
  output for small fixtures. Words that come from reading the C rather than from the recording
  are checked separately.
- `src/plan.rs`: the matrix, sampling, format and stride rules.
- `tests/vglite.rs`: on a device, crop, flips, quarter turns, fills, palettes and an opaque SRC_OVER
  against local references; a flat source through each resize filter; dmabufs on the `system` and
  CMA heaps; an axis-aligned line on exact pixels, and an L8 line `Unsupported`. Skips without
  `/dev/vg_lite`.

## Provenance

The command streams and the open and finish flows are ported from the K230 Linux SDK v0.6.1's
`package/vg_lite/VGLite` (Vivante, MIT). The notice is kept in `src/command.rs`. The source crate
and its commit are recorded in `docs/provenance.md`.
