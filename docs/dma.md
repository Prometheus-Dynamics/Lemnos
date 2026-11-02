# DMA buffers and transfers

Status: implemented in `lemnos-linux` (feature `dma`) and `lemnos-bus` (the transfer
interface); host-tested against the system dma-heap. Written, untested on board: the
board-side heaps (CMA, vendor heaps) and the fence paths of the board's kernel.

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
| `wait_fence(fd: BorrowedFd, timeout)`, `fence_is_signaled(fd)` | the same over a borrowed descriptor (any pollable one works, a sync file included), no owned `SyncFence` needed |

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
- **RP1 DMA**: the RP1 channels, for PIO and peripheral transfers.

`lemnosd` arbitrates ownership of an engine and is never in the data path; pools and
data-movement decisions stay with the consumer.

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

