# DMA buffers and transfers

Status: implemented in `lemnos-linux` (feature `dma`) and `lemnos-bus` (the transfer
interface); host-tested against the system dma-heap. Verified on the BCM2712 board (board #2,
kernel 7.2.9-v8-16k, 16K pages): the three dma-heaps, sync-file fences from a real driver
(DRM syncobj), a pending fence from a running v3d job (2026-11-03), and the SPI and I2C engines
(numbers below).

## Scope

Lemnos is user-space hardware access. It allocates and synchronises DMA buffers, maps them,
and hands their descriptors to whoever reads or writes them. It is not a memory manager: it
does not pool buffers, decide which buffer holds which frame, choose a heap for a use, or
move data. A buffer is handed over as the same descriptor, never copied.

The RP1 memory-copy DMA engine is reached through kernel drivers, never its raw controller
registers from user space.

## Buffers (`lemnos_linux::dma`)

| Item | What it does |
|---|---|
| `DmaHeap::open(name)`, `open_path` | opens `/dev/dma_heap/<name>`; `list_heaps()` names the heaps present (empty without the directory) |
| `DmaHeap::allocate(len)` | `DMA_HEAP_IOCTL_ALLOC`: a `DmaBuf` of `len` bytes, read-write, close-on-exec. The kernel rounds the size up to pages; `len()` is what was asked for. A request above the heap's capacity fails with `OutOfMemory` before the kernel is asked (see "Capacity") |
| `DmaHeap::with_capacity(bytes)` | caps every allocation from that heap, below its own capacity |
| `heap_capacity(name)`, `heap_capacity_from_meminfo(name, text)` | the capacity now: a CMA heap (name contains `cma`) takes `CmaTotal`, its whole pool; any other heap takes a quarter of `MemAvailable` (`SYSTEM_HEAP_SHARE_DIVISOR`). A missing figure is an error, never "no cap" |
| `DmaBuf::from_fd(fd)` | wraps a descriptor from elsewhere (a V4L2 export, a received fd); the size comes from the kernel (`lseek`), and the descriptor's file offset is left where it was |
| `DmaBufRef::from_fd(fd: BorrowedFd)` | the same over a descriptor someone else owns (a frame's): `len()`, `identity()`, `export_fence`, `import_fence`, `map()`, with no dup and no close. `DmaBuf::borrow()` gives one for an owned buffer |
| `DmaBuf::map()` | a `Mapping` (`lemnos_linux::dma::Mapping`): shared, read-write, unmapped on drop |
| `begin_cpu_access(Access)`, `end_cpu_access(Access)` | `DMA_BUF_IOCTL_SYNC` start and end around CPU access to a mapping |
| `set_name(&str)` | `DMA_BUF_SET_NAME`: a debugging name (at most 31 bytes) |
| `export_fence(Access)` | `DMA_BUF_IOCTL_EXPORT_SYNC_FILE`: a `SyncFence` for the buffer's pending accesses |
| `import_fence(Access, &SyncFence)` | `DMA_BUF_IOCTL_IMPORT_SYNC_FILE`: attaches a fence (read or write) |
| `identity()` | the device and inode numbers, the same for every descriptor of one buffer (`fstat`, no dup) |
| `SyncFence::wait(timeout)`, `is_signaled()` | polls the sync file; a fence whose work failed is an error |
| `sync(fd: BorrowedFd, Phase::{Begin, End}, Access)` | brackets a CPU access (`DMA_BUF_IOCTL_SYNC` start or end) on a borrowed dma-buf; `DmaBufRef::begin_cpu_access` / `end_cpu_access` are the same on a borrowed view |
| `wait_fence(fd: BorrowedFd, timeout)`, `fence_is_signaled(fd)` (a descriptor that hangs up or is invalid without becoming readable is `InvalidInput`, not "not signalled") | the same over a borrowed descriptor (any pollable one works, a sync file included), no owned `SyncFence` needed |

`Access` is `Read`, `Write` or `ReadWrite`. A kernel without sync files reports
`io::ErrorKind::Unsupported` from the fence calls.

Ownership: `DmaBuf` and `SyncFence` own their descriptors; `as_fd()` lends them
(`BorrowedFd`), `into_fd()` gives them up. A queue or engine that takes a buffer takes the
borrowed descriptor and keeps its own reference (the kernel does, for V4L2 and dma-buf
importers), so the caller may close its copy once the buffer is queued.

Sizes and pools are the consumer's: a consumer that needs slack or alignment asks for it in
`allocate`.

## Transfers (`lemnos_bus::transfer`)

One interface starts, polls, waits for and cancels a transfer, over memory or dma-bufs:

```rust
pub trait TransferEngine {
    type Ticket<'a>;
    fn submit<'a>(&mut self, transfer: Transfer<'a>) -> TransferResult<Self::Ticket<'a>>;
    fn poll<'a>(&mut self, ticket: &mut Self::Ticket<'a>) -> TransferResult<Option<Completion>>;
    fn wait<'a>(&mut self, ticket: &mut Self::Ticket<'a>, timeout: Option<Duration>)
        -> TransferResult<Option<Completion>>;
    fn cancel<'a>(&mut self, ticket: Self::Ticket<'a>) -> TransferResult<()>;
}
```

A `Transfer` has an optional `Source` (memory or a dma-buf the device reads) and an optional
`Sink` (memory or a dma-buf the device writes). Both set is a duplex transfer on a bus, or a
copy on a DMA engine. The ticket borrows the buffers for its lifetime, so memory cannot be
freed or reused while the transfer is in flight. Drop a ticket only after it completed;
otherwise `cancel` it.

| Engine | Where | Behaviour |
|---|---|---|
| `SpidevEngine` (feature `spi`) | `lemnos_linux::transfer` | one `SPI_IOC_MESSAGE`; completes in `submit`; dma-bufs refused |
| `I2cEngine` (feature `i2c`) | `lemnos_linux::transfer` | one combined `I2C_RDWR` to one target address; completes in `submit`; dma-bufs refused |

The synchronous buses move bytes through the CPU, so they refuse dma-bufs with
`TransferError::Unsupported` and finish in `submit`. `TransferError::kind()` gives the
portable `ErrorKind`.

Implemented, `lemnos-linux::transfer` (feature `usb`): `UsbBulkEngine`, one bulk endpoint
of a `lemnos-usb` device. It is asynchronous: the ticket holds the URB, `poll` and `wait`
settle it, and `cancel` asks the kernel to drop it. The buffers are owned by the USB layer
while in flight, so the caller's slice is copied in at `submit` (OUT) or copied out when
the transfer settles (IN); a dropped or cancelled ticket leaves the caller's buffer alone.
Each submit allocates its own URB buffer (the one-shot form). Streams keep buffers and
reuse them (`lemnos-usb`'s `BulkReader` and `BulkWriter`). Host-tested on the settle
logic and the error mapping; the transfer itself is untested on hardware.

`lemnosd` arbitrates ownership of an engine and is never in the data path; pools and
data-movement decisions stay with the consumer.

## Capacity

Every `allocate` reads `/proc/meminfo` first and refuses a request larger than the heap can
hold, so a large request cannot start reclaim or a contiguous search that the kernel would
then fail. CMA heaps (`linux,cma`, `default_cma_region`) take `CmaTotal`, the whole 64 MiB pool
on the BCM2712 board. Not `CmaFree`: it counts only idle pages, and the kernel serves a request
beyond it by migrating movable pages out of the pool (on 2026-11-06 a board read `CmaFree` 0 of
64 MiB and gave eight 1.28 MB buffers). Until then the check took `CmaFree` and refused those.
The `system` heap takes `MemAvailable / 4`, so about 0.9 GiB
on that board. A caller may set a lower cap with `DmaHeap::with_capacity`. The check is a
bound, not a reservation: a request that fits can still fail in the kernel (CMA fragmentation),
and that failure is returned as `ENOMEM`. Host tests: the capacity figures from text, the
refusal before the ioctl, and the missing-figure error.

## Board results (BCM2712 board, board #2, 2026-11-03)

Correction, 2026-11-03: the heap rows below were measured before the capacity check existed. The
128 MiB and 512 MiB rows were requested directly, and board #2 later hung and reset while a
large CMA request failed. The 2 GiB request in that log was not made by this probe, and its
source is not established here. Board #2 is not to be used for further heap runs, and the
figures are kept for the record only.


Probe: a throwaway program on the board, built for `aarch64-unknown-linux-musl` against this
crate's `dma`, `spi` and `i2c` features. Not committed.

Heaps (`/dev/dma_heap/`: `system`, `linux,cma`, `default_cma_region`). Allocation latency is the
median of 10 `ALLOC` calls, the size is the requested length:

| Size | system | linux,cma | default_cma_region |
|---|---|---|---|
| 4 KiB | 1.6 us | 15 us | 4.4 ms (first-call outlier; 4 KiB later ~20 us) |
| 64 KiB | 3.6 us | 20 us | 21 us |
| 1 MiB | 60 us | 162 us | 165 us |
| 8 MiB | 1.6 ms | 1.8 ms | 1.2 ms |
| 32 MiB | 8.5 ms | 5.5 ms | 6.2 ms |
| 128 MiB | 35 ms | ENOMEM (errno 12) | ENOMEM |
| 512 MiB | 153 ms | ENOMEM | ENOMEM |

- The CMA pool is 64 MiB (`CmaTotal: 65536 kB`, from the `linux,cma` reservation); the two CMA
  heaps share it, so a single allocation larger than what is free fails with `ENOMEM` (the
  kernel's `OutOfMemory`). The `system` heap has no such cap here; the kernel command line sets
  `system_heap.max_order=0`, so it allocates order-0 (16 KiB) pages.
- Map, 1 MiB: `system` 53 us, `linux,cma` 21 us, `default_cma_region` 26 us. Write 0xa5 under
  `begin_cpu_access(Write)`/`end_cpu_access`, then read back under `Read`: every byte matched on
  all three heaps. `set_name` and `identity()` work on all three.
- Failure modes: an over-size request fails at `ALLOC` and leaves nothing mapped. The heap node
  is `root:root 0600`, so a non-root caller is refused at `open`; lemnosd needs its own access.

Fences (sync files, the kernel's fence path on this board):

- A signalled DRM syncobj on the v3d render node (`/dev/dri/renderD128`) exported with
  `DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD` (`EXPORT_SYNC_FILE`) gives a sync file that
  `is_signaled()` reads as `true` and `wait(0)` returns at once (1.6 us).
- `import_fence(Write, fence)` onto a `system` buffer succeeds; `export_fence(ReadWrite)` then
  reads back as signalled.
- A plain pipe passed as a fence is refused with `InvalidInput`, not treated as "not signalled".
- An unsignalled syncobj with no fence attached fails the export with `EINVAL` on both `card0` and
  `renderD128`, so a fence must come from a submission.
- Pending, verified 2026-11-03 on board #2 (`examples/v3d_pending_fence.rs`, five runs, v3d on
  `renderD128`). A 4 MiB byte copy runs as 1024 TFU jobs, one row of 4096 bytes each, submitted in
  order on one descriptor; the last carries the out-syncobj. The sync file exported straight after
  the submits reads unsignalled in every run (9,300 to 11,100 unsignalled polls before it signals),
  and it signals 4.9 to 5.0 ms after the first submit (run 1, with warm-up: 9.0 ms). The submits
  themselves take 1.5 ms. All 4,194,304 bytes of the destination match the source in every run.
  Untested: a job that fails, and a timeline syncobj.

### v3d TFU job (`lemnos_linux_sys::v3d`)

`DRM_IOCTL_V3D_SUBMIT_TFU` needs no shader. The uAPI header gives the struct and the handle order
(output buffer first), but not the `icfg` bit fields, and no register reference was on the host.
So the job's shape was found by a sweep on board #2, and the byte-for-byte copy is the check:

- `icfg` 0 (and 1 to 16, 64, 128 as tried): the first output row holds the input's last row when
  the job is given several rows, so the job is wrong for a copy as written.
- `icfg` 32: the output's first row is the input's first row, copied forward.
- The height in `ios` (`height << 16 | width`) is not honoured in the sweep: each job wrote one row,
  `width` bytes long (one byte per element with `icfg` 32; tried with widths 64 to 1024 and heights
  4 to 1024). A copy is therefore one job per row, `iia` and `ioa` advanced by the stride. With that
  shape the copy is byte-exact.
- In the sweep, every byte of the destination outside the written row stayed at its fill value.
  No GPU reset, and the dmesg tail after the sweep had no `v3d` line.

SPI (`SpidevEngine`): `/dev/spidev10.0` (`107d004000.spi`, the SoC SPI, DMA channels
`dma1chan0/1`, no device in the board file). A zero-length write and a zero-length duplex both
complete (`Ok`, 0 bytes), which exercises the `SPI_IOC_MESSAGE` path without clocking a bit.
No data transfer was run: no known-safe SPI device exists on this board.

I2C (`I2cEngine`): i2c-1, the `rp1` hardware bus (`i2c@74000`) that lemnosd uses for the
magnetometer and power monitor. One combined write-then-read: register `0x40` of the BMM150 at
`0x10` returned `0x32` (its chip id) in 435 us. Read only, one transaction. i2c-10 (the camera's
OV9782) and i2c-13/14 were not touched.

Not in the tree or not reachable on board #2:

- No dmaengine character device, no `dmatest` module, no `udmabuf` user. `udmabuf` is loaded but
  it is a dma-buf exporter, not a transfer engine.

## Tests

- Unit tests run on any Linux host: layouts and request numbers against the kernel headers
  (`lemnos-linux-sys`), the name and descriptor checks, `Mapping` on a temporary file.
- `the_system_heap_allocates_maps_and_syncs` uses `/dev/dma_heap/system`, and the fence part
  when the kernel has sync files; skipped where the heap is absent.
- The engines' refusals and errno classification are tested without hardware. The SPI and I2C
  transfers themselves need a device: written, untested on board.

## Migration notes

For a consumer with its own dma-heap code (the camera stack's `dma_heap` and `dmabuf_sync`
modules):

- `DmaHeap::open`, `allocate`, `begin_cpu_access`/`end_cpu_access`, `map` and `Access` keep
  their shape; `DmaBuf::from_fd` now returns `io::Result`.
- Not carried over, by design: heap-sizing policy (tail slack, page padding), sync statistics
  counters, and the test-only memfd stand-in. Keep those with the consumer.
- `dmabuf_size` is `DmaBuf::len()`; `dmabuf_id` is `DmaBuf::identity()` (`io::Result`).
- Errors are `io::Error`; the kind is `Unsupported` for missing sync files, `InvalidInput`
  for bad names and sizes.

## Offsets and sizes

A dma-buf has no read or write position to use, and it answers only `lseek(SEEK_END, 0)`
and `lseek(SEEK_SET, 0)`: `SEEK_CUR` is `EINVAL`. The size query therefore reads the
offset where it can. For a regular descriptor it reads the current offset, seeks to the
end for the size, and restores the old offset, so a caller's offset is unchanged. For a
dma-buf it seeks to the end and back to 0, which is where a fresh descriptor's offset
already is. Neither path duplicates the descriptor: a dup shares the open file
description, so a dup's seek would move the caller's offset too.

`lemnos_linux::dma::DmaBufRef` is the borrowed form, so a caller holding a per-frame
descriptor queries its size, identity and fences without a dup and without taking
ownership. Test coverage: the system heap (allocate, map, sync, name, fence, offset
behaviour) and a pipe for the borrowed fence polling. Untested on board: CMA and vendor
heaps and the board kernel's fences, as before.

