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
  its NUL). The surface makes the slice from its pointer.
- `guard` / `guard_or` (feature `std`): runs an `extern "C"` body and returns `Status::PANIC`
  (or a given value) instead of unwinding into C.

No enum crosses the boundary: every type is `#[repr(C)]` or a fixed-size integer, so a value a
newer peer sends stays defined behaviour. Surfaces sit at per-frame or per-job granularity:
calls across a shared-library boundary are not inlined.

`no_std`. No dependencies unless feature `load` is on. Unsafe code only in `package` and `load`.

## Driver packages

`package` is the boundary between a compiled driver file (a package) and the host that loads it.
It has its own version, `PACKAGE_VERSION` (1.0), and its own layout hash, `PACKAGE_LAYOUT_HASH`,
which covers every type below and this crate's frozen hash. Its types hold pointers, so the hash
follows the target's pointer width; a package and its host are one process and share it. The
frozen 1.0 layouts above are untouched.

- A package exports one data symbol, `lemnos_package` (`PACKAGE_ENTRY`): a `PackageDescriptor` of
  `magic` (`PACKAGE_MAGIC`, the bytes `LEMNOSPK`), `version` (an `AbiVersion`) and `register`.
  A host reads the first two and calls nothing unless both are right.
- `register` hands the host one `PackageDriver`: a `PackageManifest` (the driver's name, its
  needs, and the contracts it provides with their methods and shapes) and a `DriverTable` (bind,
  describe, health, dispatch, tick, caller, caller_gone, drop).
- When a driver binds, the host gives it a `HostTable`: `provided` and `take` while it binds,
  then `release`, `device_call`, `device_health` and `i2c_transaction` on the `Handle`s it took.
- Pointers cross inside `Str`, `Slice`, `SliceMut` and `Out`, which are made only from Rust
  references. A null or misaligned pointer, or text that is not UTF-8, reads as empty.
- `driver_table::<T>()` (feature `std`) builds a `DriverTable` over a `PackageInstance`, and
  `host_table::<B, H>()` a `HostTable` over a `PackageBinding` and a `PackageHost`. Every function
  in both runs inside `guard`. `Loaded` is a package's bound driver as the host holds it.

`load` (feature `load`, Linux) gets a package into the process: `seal` copies a file's bytes into
a sealed memfd, and `load` has the dynamic loader map that memory and returns the descriptor. It
can look up the package entry and nothing else, and it is the only use of the dynamic loader in
Lemnos: for Lemnos driver packages, never for a vendor library. A package is never unloaded.
`lemnos-package` does the checking in between and is what a host uses.

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
