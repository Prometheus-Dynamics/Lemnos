# lemnos-abi

The C-ABI building blocks a surface over Lemnos needs, written once so the surfaces and their
wrappers do not each invent their own.

- `Status`: an `i32`. 0 is success, -1 to -4095 a negated Linux errno (a kernel error passes
  through as is), `Status::LEMNOS_BASE` (-4096) and below this crate's own codes
  (`STALE_HANDLE`, `VERSION_MISMATCH`, `BUFFER_TOO_SMALL`, `PANIC`), positive a count.
- `Handle`: a `u64` with a slot index and a generation; 0 is null. `Slab` (feature `alloc`)
  issues them, reuses released slots with a new generation and refuses stale handles.
- `BufferDesc` (up to four `PlaneDesc`s: fd, offset, stride, size; DRM fourcc and modifier) and
  `FenceDesc` (none, sync file, or syncobj with a timeline point), with `validate`.
- `MessageSink`: a C callback that takes a message, such as an error's text.
- `AbiVersion`: major, minor, struct size and a layout hash (`layout_hash`, FNV-1a over sizes,
  alignments and field offsets, at compile time). `accepts` checks a caller against a surface.
- `Features`: a surface's optional features, a `u64` bit set the surface returns from its own
  `<prefix>_features()` call after the handshake (`contains`, `require`). It is not a field of
  `AbiVersion`, so the handshake's layout and every layout hash stay as they are.
- `write_out` / `write_text`: a value into a buffer the caller supplies, all or nothing, with the
  length it needs and `Status::BUFFER_TOO_SMALL` when it does not fit (text also needs room for
  its NUL). The surface makes the slice from its pointer; this crate has no unsafe code.
- `guard` / `guard_or` (feature `std`): runs an `extern "C"` body and returns `Status::PANIC`
  (or a given value) instead of unwinding into C.

No enum crosses the boundary: every type is `#[repr(C)]` or a fixed-size integer, so a value a
newer peer sends stays defined behaviour. Surfaces sit at per-frame or per-job granularity:
calls across a shared-library boundary are not inlined.

`no_std`, no dependencies, no unsafe code.

## C header

`include/lemnos/abi.h` declares these types for C: `LemnosStatus` and the `LEMNOS_E_*` codes,
`LemnosHandle`, `LemnosFeatures`, `LemnosAbiVersion`, `LemnosMessageSink`, `LemnosPlaneDesc`,
`LemnosBufferDesc` and `LemnosFenceDesc`, with `LEMNOS_ABI_TYPES_{MAJOR,MINOR,LAYOUT_HASH}`. Every
Lemnos surface's header includes it, so a program using two surfaces sees each type once. The
header lives at `include/lemnos/abi.h` in the repository root, and this crate's tests check every
value, size and offset in it against the Rust types with a C compiler.

## Stability (version 1.0, frozen 2026-11-06)

- **Frozen layouts.** The layouts in the hash (`Status`, `Handle`, `PlaneDesc`, `BufferDesc`,
  `FenceDesc`, `AbiVersion`) are frozen at version 1.0, layout hash `0xa02cfb4a8dd4545b`.
  `src/version.rs` asserts that hash at compile time, so any change to them fails the build.
- **Major version.** Changing a frozen layout, renaming or renumbering a status code, flag or
  fence kind, or changing what an existing value means is a new major. That is a new
  `LEMNOS_ABI_TYPES_MAJOR`, and every surface that embeds these types takes a new major too.
- **Minor version.** Additions are a minor: a new status code below `PANIC`, a new
  `BufferDesc` flag or fence kind, or a new type. Peers already accept values they do not know.
  A receiver refuses a fence kind it does not know, and passes through a status code it does not
  know.
- **Growing a struct.** Only the structs with a `struct_size` field (`BufferDesc`,
  `AbiVersion`) may grow, by fields appended at the end, as a minor. A receiver reads up to the
  size the sender gave.
- **No enums.** No Rust enum ever crosses the boundary.
- **Pointer-width types.** `MessageSink` is outside the hash because its size follows the
  pointer width. Its shape is frozen with the rest.
- **Each surface's own version.** A surface has its own `AbiVersion` (for `lemnos-capi`,
  `LEMNOS_ABI_MAJOR` and `LEMNOS_LAYOUT_HASH`), and its layout hash covers this crate's hash. So
  a change here shows up in every surface's handshake.
