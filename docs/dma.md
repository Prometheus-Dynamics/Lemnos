# DMA buffers and transfers

Status: implemented in `lemnos-linux` (feature `dma`) and `lemnos-bus` (the transfer
interface); host-tested against the system dma-heap. Verified on the BCM2712 board (board #2,
kernel 7.2.9-v8-16k, 16K pages): the three dma-heaps, sync-file fences from a real driver
(DRM syncobj), and the SPI and I2C engines (numbers below). Not verified: a pending fence from
a running producer, and any RP1 or PCIe DMA engine (none exists; see "Planned").

## Scope

Lemnos is user-space hardware access. It allocates and synchronises DMA buffers, maps them,
and hands their descriptors to whoever reads or writes them. It is not a memory manager: it
does not pool buffers, decide which buffer holds which frame, choose a heap for a use, or
move data. A buffer is handed over as the same descriptor, never copied.

## Buffers (`lemnos_linux::dma`)

| Item | What it does |
|---|---|
| `DmaHeap::open(name)`, `open_path` | opens `/dev/dma_heap/<name>`; `list_heaps()` names the heaps present (empty without the directory) |
| `DmaHeap::allocate(len)` | `DMA_HEAP_IOCTL_ALLOC`: a `DmaBuf` of `len` bytes, read-write, close-on-exec. The kernel rounds the size up to pages; `len()` is what was asked for |
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

Planned, not implemented, each a new `TransferEngine`:

- **PCIe DMA**: descriptor rings over vfio, with dma-buf sources.
- **RP1 DMA**: the RP1 channels, for PIO and peripheral transfers. The controller is a DesignWare
  AXI DMAC (`dw_axi_dmac_platform`, 8 channels) at RP1 `0x1f00188000` (`dma2` in
  `/sys/kernel/debug/dmaengine`). Board #2 result (2026-11-03): the register window is
  reachable read-only through `/dev/mem` (`mmap` with `PROT_READ`, root only; plain `read` returns
  `EFAULT` because the address is above RAM). `COMPVER` (`+0x08`) reads `0x3130332a` and the
  channel-enable register (`+0x18`) reads 0, so the controller is idle. Channels 0 to 2 belong to
  the kernel's RP1 PIO driver (`pio:tx0`, `tx1`, `rx1`); 3 to 7 are unassigned. No engine is
  written. The blockers, all on board: (1) no userspace DMA interface (no dmaengine chardev, no
  `dmatest`); (2) channel registers written from user space would bypass the kernel driver that
  owns the controller and its interrupts; (3) a buffer's bus address in the RP1's view is not
  established (the PCIe `dma-ranges` and pagemap PFNs would have to be combined, and that is
  unverified). Write it when a kernel interface exists, or when the lead accepts a root-only
  raw-register engine on free channels.
- PCIe DMA: not written. No PCIe DMA device on either board is reachable from user space without
  vfio, so the interface would be untested.

`lemnosd` arbitrates ownership of an engine and is never in the data path; pools and
data-movement decisions stay with the consumer.

## Board results (BCM2712 board, board #2, 2026-11-03)

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
- Pending fences were not reproduced. The same export on an unsignalled syncobj fails with
  `EINVAL` on both `card0` and `renderD128`: a pending fence needs a submission (a v3d job with
  an out-syncobj), which this pass did not build. So the "signals later" path is untested on board.

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

