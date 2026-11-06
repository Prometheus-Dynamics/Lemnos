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

`no_std`, no dependencies, no unsafe code. Host-tested only; no C caller exists yet (the C
header comes with Lemnos's own surface).
