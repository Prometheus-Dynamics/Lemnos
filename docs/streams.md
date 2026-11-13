# Frame streams

A call carries a few bytes. A camera frame, an image to scale or a block of USB data does not fit
in a call, and must not be copied through a socket. A frame stream is how those move: a set of
buffers and a small ring, shared between a driver and one program. After the stream is set up,
nothing about a frame crosses the socket.

The [socket protocol](wire.md#streams) is the exact specification. This page says how the parts
fit.

## The parts

A stream belongs to one device and has a name. The board file declares it:

```toml
[devices.streams.frames]
buffers = 8
buffer_bytes = 2097152
```

- **The buffers**: `buffers` of them, each `buffer_bytes` long. [Buffer length](#buffer-length)
  says how a consumer reads them back.
- **The rings**: shared memory, built from the shm crates ([the shared ring](ring.md)). *Frames* go
  from the driver to the program, one entry per frame. *Holds* say which buffers the program is
  reading, so the driver never reuses one of them. *Jobs* go from the program to the driver, and
  a page the driver writes says how many it has collected.
- **The doorbell**: an eventfd the daemon rings after each frame, for a program that wants to
  sleep until one arrives.

The driver is the **producer**. The program that opened the stream is the **consumer**. A stream
has one consumer at a time: a second open is refused with `BUSY`.

The daemon makes the memory, fresh for each consumer, and passes it over the socket as file
descriptors, in the order [Reading with the shm crates alone](#reading-with-the-shm-crates-alone)
gives. A consumer never sees what the one before it left, and a consumer that goes away takes
nothing with it: the buffers its driver held come back empty-handed.

The device's contract says which stream to open: a method such as `frame_stream` or `job_stream`
returns its name.

## Shared memory or dma-bufs

The board file chooses with `memory`:

| `memory` | The buffers are | Use it when |
|---|---|---|
| `"shared"` (default) | One piece of memory shared with the consumer | The driver fills the buffers in software: a USB camera's frames copied out of transfers |
| `"dma-buf"` | One dma-buf per buffer, from the dma-heap `heap` names | Hardware fills or reads the buffers itself: a receiver, an ISP, a scaler, a codec |

With dma-bufs the driver hands a buffer to its hardware by file descriptor, and the frame is in
the consumer's memory with no copy. A driver whose hardware takes memory only that way refuses a
`"shared"` stream when it binds, with `NOT_SUPPORTED`, so the mistake shows in the device list and
points at the board file.

If the heap is missing or refuses, the open is refused with the reason. The daemon never makes
such a stream out of shared memory instead. A stream of dma-bufs has at most 128 buffers.

### Planes

Some hardware writes one frame into several pieces of memory, a plane each: a V4L2 node of the
multi-plane API writing luma to one and chroma to another. A board gives such a stream
`planes = [..]`, the length of each plane, adding up to `buffer_bytes`. Each plane of each buffer
is then its own dma-buf, 1 to 8 planes per buffer and at most 128 dma-bufs in all. The consumer
is given the descriptors buffer by buffer, each buffer's planes in order, with the plane lengths.

The driver hands each plane to its hardware by its own descriptor, so a multi-plane frame lands
in the consumer's memory with no copy. A frame's length counts its planes end to end, and so do
`write` and `read`: an offset past the first plane is in the second. The client reads one plane
of a frame with `FrameStream::plane`. A driver whose hardware writes more planes than the stream
has copies each frame in instead, once.

### Standby

A stream's memory is made for each consumer and goes with it, so a consumer never sees what the
one before it left. That leaves hardware with nothing to fill while nobody is attached, and a
consumer that attaches waits for the next whole frame.

`standby = true` changes that. While no consumer is attached, the daemon makes the stream's
memory and the driver fills it as if one were. The newest frame is kept, and the rest go back to
be filled again. A consumer that attaches takes that memory over, nothing copied, and the kept
frame is the first it reads. When it leaves, its memory goes with it and fresh memory is made for
the next stretch with nobody attached, so the rule above still holds: the next consumer reads
only frames made after the last one left. Standby holds a stream's memory for as long as the
daemon runs, and keeps hardware busy that would otherwise idle, so a board turns it on only for
streams that must answer at once.

## Who fills what

Either side may be the one that puts bytes in a buffer. The contract decides:

| Contract | The consumer fills | The driver or its hardware fills |
|---|---|---|
| `camera.receiver/1`, `camera.frames/1` | | the frame stream |
| `camera.isp/1` | the config stream, the input stream | the statistics and output streams |
| `image.scaler/1`, `image.convert/1`, `image.blend/1` | a job's input images | a job's output image, in the same stream |
| `codec.block/1` | a job's work buffer and input | a job's result |
| `npu/1` | programs and input tensors | output tensors |
| `usb.function/1` | bytes to send | bytes received |

The rule for a buffer the consumer fills is the same everywhere: write it, then post the job that
names it, then leave it alone until that job's completion arrives.

The producer trusts nothing in a buffer. It reads what it needs once into its own memory and
checks the copy, because the consumer shares the memory and can change it at any time.

## Frames

The driver publishes a frame: a buffer number, a length, a sequence number, a timestamp in
nanoseconds and an identity. The identity is 64 bits the producer chose. Lemnos never reads it.
Every published frame carries its capture time, the driver's timestamp (`CLOCK_MONOTONIC` ns), as
the time of its ring batch (the shm sample time of the entry): not the time of publishing. A foreign
reader gets it from the batch's `time`, and `lemnos-client` from the frame.
The consumer reads the buffer, then releases it (dropping the frame it took). A buffer the
consumer holds is not written by anyone else.

**The kind of stream.** Who makes the frames is declared by the board author with `kind` in
`[devices.streams.NAME]`, and the reclaim rule below is a function of it:

- `kind = "free-running"` (the default): the frames arrive whether or not anyone reads them (a
  camera, a receiver, a front end's statistics). The rule for these is the replacement rule and
  the margin below.
- `kind = "job"`: each frame is the result of a job the consumer posted (a memory-to-memory
  device's output, an encoder, a scaler, the config and input of a job-driven block). See
  "Job-driven streams" below.

**The replacement rule, for a free-running stream.** A buffer the consumer has read and let go goes back to the driver as
soon as the driver asks for one: nothing is lost. A frame the consumer has not read yet stays in
its buffer for as long as the driver has a free buffer to fill. Only when the driver asks and none
is free is the oldest unread frame taken back, and the consumer sees it skipped. What the driver
holds of its own does not change this: a driver with a queue of buffers in its device is refilled
from the oldest unread frame like any other. The newest frame is never taken back while unread,
and a frame the consumer has read and not yet released is never taken back at all. A consumer
that holds every buffer stops the driver without a fault.

Whether a frame has been read is kept by the consumer's taken stamp on the ring, not by its holds:
a frame the consumer has read and let go counts as taken, and one it has not read counts as
unread, whatever the consumer holds of other buffers. Nothing else makes an unread frame
reclaimable.

**Job-driven streams.** On a `kind = "job"` stream an unread frame is never replaced, whatever the
driver's working set. Each frame is a result the consumer posted a job for, so replacing it loses
the consumer's work. The pressure goes the other way: a job whose output has no free buffer is not
started. The driver's ask finds none and the refusal is counted in `refused` (the stream's stats).
The block's own contract counters are not changed by this rule. A read result is taken back as usual, so a
consumer that keeps up is refused nothing, and no result is ever silently lost.

**The guarantee, per kind.**

- Free-running: a reader at most N frames behind loses none, with **N = B − 1 − W − granted holds**
  (below). Past N the oldest unread frame is replaced, counted in `dropped`, and the newest is never
  replaced.
- Job-driven: **no result is ever replaced.** Every posted job that gets a buffer publishes its
  result, which stays until the consumer reads it; a job that finds no free buffer is refused and
  counted in `refused`.

**The margin.** A reader that falls behind by up to a number of frames loses none of them. Call
the buffers in the pool `B` and the producer's working set `W`: the number of buffers the producer
holds in its own hands at the moment it asks for its next one. Then a reader N frames behind
loses nothing, where

> **N = B − 1 − W**

Why: when the producer asks, every buffer is either free, held by the producer (`W`), or holds an
unread or read-and-held frame. With `n` unread frames a free buffer exists while `W + n ≤ B − 1`.
A read frame is taken back at no cost, so only unread frames bound the margin. Past N, the
producer's ask takes back the oldest unread frame, which the consumer sees skipped and `dropped`
counts; the newest frame is never taken. A producer that is never refused a buffer loses no frame.

What `W` is, for each kind of producer:

- A driver that publishes each frame before it asks for the next buffer (a USB camera's tick, a
  codec filling one buffer at a time): `W = 0`, so `N = B − 1`.
- A driver that asks for its next buffer while it still holds the one it is filling: `W = 1`,
  so `N = B − 2`.
- A device with `K` buffers queued in its hardware, refilled as each frame completes: V4L2 capture
  (a node that takes the stream's dma-bufs, or whose buffers are mapped), the kernel module's
  blocks, and a memory-to-memory device with `K` queued. The device holds `K − 1` at the ask, so
  `W = K − 1` and `N = B − K`.

The board author sets `B` as `buffers` and `W` as `working` in `[devices.streams.NAME]` (the
driver's `W` for its kind; `working = 0` when left out, which is right for the first kind above
only). `working` must be less than `buffers`. A consumer reads them, and the stream's `kind`, from
the open answer: `FrameStream::buffer_count()`, `FrameStream::working()`, `FrameStream::margin()`
(which is N, for a free-running stream) and `FrameStream::job_driven()`. The host's stats carry
`refused`, the count of asks a job-driven stream could not answer.
A consumer that reads the shared memory with the shm crates alone is not told `W`, and must take it
from the board file.

What a reader that is late by at most N sees is every frame in order, each with the bytes the
producer wrote into it: nothing is refilled under a frame it has not released. A reader that is
later than N sees the newest frames only, in order, and each frame it misses is counted in
`dropped` (a read frame's buffer is never counted, and is never replaced).

Each frame a stream's consumer must hold while it works (the frames it has taken and not released,
and any hold granted against the pool to an outside reader) is one buffer out of the same count, so
the margin falls by one per held buffer: N = B − 1 − W − held.

A consumer that must not lose frames holds each frame it takes until it has finished with it, and
releases it then. While it holds them the driver has nothing to take back, and its publishes are
dropped and counted, which is the stop; the consumer is not faulted. A consumer that takes its
frames promptly never sees a replacement. Each frame replaced before the consumer took it counts in
the stream's `dropped` statistic, as does each frame the driver could not publish; a replaced
frame the consumer had read does not count. `returned` counts the buffers taken back. This is the shared ring's rule (see [`ring.md`](ring.md)), not a
Lemnos choice.

A frame carries nothing else. There is no frame type, pixel format, size in pixels or codec on
the ring, in the board file's stream, or in the daemon: what the bytes are is agreed by the
contract and the driver (see [Movers](design.md#movers-dont-know-what-they-move)).

### The timestamp's clock

On Linux a frame's timestamp is **`CLOCK_MONOTONIC` in nanoseconds**, whoever made the frame, so a
consumer compares it directly with its own `clock_gettime(CLOCK_MONOTONIC)`:

- A stream made by the [kernel module](kernel-module.md) is stamped with `ktime_get_ns()`, which
  is `CLOCK_MONOTONIC`. A receiver's frame takes the time of its frame start.
- A driver that publishes from its tick (a USB camera, a scaler's or codec's completions) stamps
  with the `now_ns` it is given. `lemnosd`'s clock is `CLOCK_MONOTONIC`.
- A driver reading a V4L2 node passes on the buffer's own timestamp when the node flags it
  `V4L2_BUF_FLAG_TIMESTAMP_MONOTONIC`, and otherwise the time it took the buffer off the queue.
  A memory-to-memory node copies its input's timestamp, which is the caller's and not a time.
- A driver reading a vendor kernel's interrupt events passes on their time, which those kernels
  take with `ktime_get_ns()`.

The clock stops while the system is suspended. A timestamp of 0 means the producer had no time for
the frame: a job that failed, or one dropped before it ran. On a microcontroller the timestamp is on
the firmware's monotonic clock, and on the simulated board on the simulated clock.

A camera receiver is a producer that takes no jobs, so it has nothing to name a frame by: its
frames carry identity 0 and the sequence number is the frame's name. Its sequence counts from the
receiver's start.

### Embedded data

A sensor may carry the register values in force for each frame (`camera.sensor/1`,
`SensorInfo::embedded_data`). A receiver whose sensor does publishes them on a second stream, named
by `embedded_stream` (`camera.receiver/1`): one buffer per line, carrying the sequence of the frame
it came with, in the sensor's own layout, which the sensor's library crate decodes. A frame with no
embedded line publishes nothing, so the sequence numbers skip it. A receiver that captures none
answers an empty name.

On the `rp1-csi2` receiver the board asks for it with a stream named `embedded` on the device, and
`config.embedded_bytes`, the length of a line (`docs/boards.md`). The kernel module takes the line
on the block's second DMA channel, data type 0x12 on the image's virtual channel, and the stream
gets one buffer per line. Untested on a board.

`rkcif` takes the lines from a stock metadata node instead, when the board names one
(`config.embedded`, with `embedded_format` and `embedded_bytes`): each line is copied onto the
`embedded` stream with the node's sequence. The vendor kernel exposes no such node, so there the
path stays off; a kernel that does expose one serves it. `sunxi-vin` captures none. No sensor on a
board in the tree carries embedded data, so on hardware the path is unproven.

## Buffer length

A consumer learns the buffer length from the stream it opened, with nothing from the contract:
`FrameStream::buffer_len()` is the bytes in each buffer and `buffer_count()` the buffers. Both come
from the stream's open reply, which carries the board's `buffer_bytes` and `buffers`, so they are the
same before and after the first frame. A frame's `len()` is at most `buffer_len()`.

The contract says how many bytes a frame takes, so a consumer can check it against that length
before reading. Where each contract gives it:

| Contract | Stream | Where the frame's size is |
|---|---|---|
| `camera.receiver/1` | `frame_stream` | `layout` (method 11), or what `configure` returned: `FrameLayout::size`, the most bytes a frame takes. `layout` answers `None` before `configure` |
| `camera.frames/1` | `frame_stream` | `format`: `FrameFormat::max_frame_bytes` |
| `camera.isp/1` | `config_stream` | `config_layout`: `ConfigLayout::bytes` |
| `camera.isp/1` | `statistics_stream` | `info`: `IspInfo::statistics_bytes` |
| `camera.isp/1` | `output_stream` | `set_output` returns `OutputLayout`, and `output_layout` (method 14) answers it again: `stride` and `size`, the most bytes an image takes, padding included. `None` while the output is off |
| `camera.isp/1` | `input_stream` | `input_layout` (method 13): `stride` and `size` of a raw frame, from `set_input`. `NOT_SUPPORTED` when the frame comes straight from the receiver |

The ISP's configuration rules (which buffer a job may name, when it is read, when it is given
back) are in the `camera.isp/1` contract's module docs, under "Configuration buffers". The camera
ISP's job sequence, step by step, as the reference board runs it, is in
[`drivers/pisp/README.md`](../drivers/pisp/README.md#running-a-job-live).

The job streams (`image.scaler/1`, `image.convert/1`, `image.blend/1`, `codec.block/1`, `npu/1`,
`usb.function/1`) have no frame size to state: each job names its own layout, offsets and lengths
within the buffers, and the driver checks every one against `buffer_len()` before hardware sees it.

A receiver's frame layout is single-plane. Its `stride` is the bytes per line the block writes,
padding included, and `size` the frame: `stride` times the lines. `stride` is what the kernel reports
for the frame or what the block is programmed to write; `sunxi-vin` refuses to start when the kernel
grants another, and `rkcif` writes its vendor compact lines by rule, which no register reports.

The C API has no frame streams yet, so a C consumer has no buffer length to read.

## Jobs

A job is a small message from the consumer, at most `job_bytes` long. At most `job_slots` can
wait. What the bytes mean is between the driver and the consumer, and the contract defines it: a
scaler's job names an input buffer, an output buffer and the sizes; an ISP's job names the
configuration for one frame.

A job names buffers by number, never by address. The driver checks every number, offset and length
against the buffers the consumer really holds before any of it reaches hardware.

On a block that takes jobs, each job's completion comes back as a frame on the same stream, with
the job's identity. Every job the block reads gets exactly one completion.

A job the hardware cannot run completes as failed, or is counted as dropped, as its contract says.
It is not run in software and not run again.

The camera ISP's job sequence, step by step, as the reference board runs it, is in
[`drivers/pisp/README.md`](../drivers/pisp/README.md#running-a-job-live).

## Faults

Neither side trusts the ring. Each value is read once and checked.

- A consumer that posts more jobs than the job ring holds without the driver collecting them, or
  whose holds are stuck (its marks cannot be read at one instant, or it holds past the hold time),
  **loses that stream**. Its state becomes *faulted* and the doorbell rings. The connection, the
  device and every other stream carry on. Opening the stream again starts clean, on new memory.
  A consumer cannot corrupt the driver's side of the rings: it has read-only access to the frame
  ring and the pool, and the only words it writes are its own holds, its doorbell page and the
  job ring.
- A consumer that closes its connection gives up every stream it held.

## Giving a stream back

A consumer gives a stream back with `close_stream`, or by dropping it. Dropping a stream the
consumer never closed does the same: its close is queued in the client, and sent with the client's
next request. Nothing waits for that close. The next open of the same stream on the same client
sends the close first, so it is never refused for a stream the client already let go.

When the service ends a stream it does the following, however the consumer went:

- the buffers the consumer still held go back to the free pool, and nothing it held is kept;
- the ring's end word is set (closed, or faulted), so a consumer that is still reading it sees the
  stream has ended and why;
- until a consumer attaches again, each frame the producer publishes is counted as dropped, and
  `stream_stats` reports `attached` as false. A driver learns the consumer is gone from those
  counts and from its own `publish` results, not from a callback.

A second open while a consumer still holds the stream is refused with `BUSY` straight away. An
open that the service doesn't answer fails with `TimedOut` once the client's deadline passes (five
seconds by default, `set_timeout` changes it); it never waits without one.

## In a program

`lemnos-client` opens a stream with `open_stream`, or `open_stream_writable` for one the consumer
fills. Both take a `StreamPlatform`, which does what only the operating system can: receive the
descriptors, and tell the kernel around each access to a dma-buf. On Linux that is `LinuxStreams`
from `lemnos-linux-stream` (`backends/linux-stream`). The rings and the buffers are mapped by the
shm crates, so the client has no `unsafe` of its own. A program with no client reads the stream
with the shm crates alone: see [Reading with the shm crates alone](#reading-with-the-shm-crates-alone).

```rust
let devices = client.list()?;
let camera = devices.iter().find(|d| d.name == "webcam").ok_or("no webcam")?;
let mut stream = client.open_stream(camera.index, "frames", Arc::new(LinuxStreams))?;
while let Some(frame) = stream.next_frame(Duration::from_secs(1))? {
    let bytes = stream.bytes(&frame);
    // ... use the frame ...
    stream.release(frame)?;
}
```

`post_job` posts a job, `write` fills a buffer of a writable stream, `read` reads one back, and
`try_next` takes a frame without waiting. On a stream of dma-bufs, `buffer_handle` gives a buffer's
own descriptor, for handing the frame to something else that takes memory that way. The
[C API](client.md#the-c-api) has the same calls.

## Where a buffer's memory is

`region(buffer, plane)` says where one plane of a buffer is in memory, for a consumer that maps
the memory itself (a GPU import, an encoder, or a second process of its own). A `Region` gives the
descriptor (`fd`, borrowed), `kind` (`SharedMemory`: a memfd of the pool; `DmaBuf`: a dma-buf of its
own), `index` (the pool index, `buffer * planes + plane`, which is the number the ring's frames
name), `offset` (where the plane starts in `fd`), `payload_bytes` (what the contract may use: the
buffer's length, or the plane's), `allocated_bytes` (what exists from `offset`: a shared pool's
stride, up to the next buffer, or a dma-buf's whole size, which a heap may round up), `tail_bytes`
(slack past the payload that an importer must be given; always 0 today, and it will come from the
board's per-stream `tail_bytes` key for dma-bufs, in I2d), and `writable`. `pool_page()` gives a
read-only descriptor of the pool page, and `generation()` the pool's generation.

`writable` is what the descriptor allows: whether a writable mapping of it succeeds, probed when the
stream opens. The consumer maps the buffers writable only when it took the stream with
`open_stream_writable`, and that open fails with `StreamBuffersReadOnly` ("this stream's buffers
are read-only") when any buffer's descriptor allows only read-only mappings, so the mismatch shows
at the open and not at the first write. A stream taken with `open_stream` maps read-only even where
the descriptors allow writes. Today the host hands out read-write descriptors for every stream. The
outside-reader work (I2d) will make a stream whose buffers a block fills read-only from allocation;
the same probe reports it, with no change to the client.

The stream's answer carries no image layout (width, height, packing, stride). The stream layer
moves buffers and does not know what a frame is. A frame's layout comes only from the contract: the
`layout` method on the receiver, or `output_layout` on an ISP. A consumer reads it from the contract
after it opens the stream, and again after each reopen (a new generation).

A frame's buffers are the same memory `region` describes: the held frame's `buffer()` gives its
buffer, and `plane(frame, p)` the bytes of plane `p` that the frame holds.

### Lifetimes, and a second process

- The descriptors live as long as the stream. A `Region` borrows the stream, so it can't outlive it.
- A frame's hold keeps its buffer from being reused: the producer does not reuse a buffer while a
  held frame of it exists, and the hold ends when the frame is released or dropped.
- A copy of a descriptor (`try_clone`, or the descriptor of a region handed to another process over
  `SCM_RIGHTS`) keeps the memory alive after the stream closes, but nothing protects the content from
  reuse then. Reuse is only prevented while the stream's own hold on a buffer exists.
- The pool page, the descriptors and the region offsets are enough for a second process to map the
  buffers (`shm_ring_buffers::PoolView::attach`). That process holds no reader slot: it must read
  frames it is given only while the first process holds them, and it must not mark the hold table
  itself. Its protection against reuse is only what the first process's hold gives. Giving another
  process its own reader slot is I2d.
- A `generation` changes whenever the host makes a new pool for the stream: a reopen, or any
  reallocation. A lease that has an older generation is on a pool that is gone. A consumer that gets
  The frame reader reports a pool change as `RingError::Broken`: the stream is then reopened, and the
  consumer takes its regions, descriptors and layout again from the new stream. A consumer that
  holds an old lease's frames keeps them valid until it releases them, but must not use the old
  regions for new frames.

The daemon's tests run every case on this page against drivers made for the purpose: frames, jobs,
a consumer that fills buffers, dma-bufs, and each fault.

```sh slow
cargo test -q -p lemnosd --test stream --test stream_fill --test stream_dma
```

## Reading with the shm crates alone

A program that has the descriptors of a stream can read its frames with the shm crates
(`shm-ring`, `shm-ring-buffers`, `shm-ring-holds`, `shm-ring-doorbell`) and nothing of Lemnos. The
descriptors come in this order (`lemnos_wire::ring::fds`):

| Index | Descriptor | Map it as |
|---|---|---|
| 0 | frame ring | `shm_ring::Reader::attach_from_start`: the ring, read-only |
| 1 | pool page | with the pool's buffer descriptors: `shm_ring_buffers::PoolView::attach` |
| 2 | job ring | `shm_ring::Writer::attach`, if the program posts jobs |
| 3 | job acks | `shm_ring::ReadMap::new`: one word, how many jobs the driver collected |
| 4 | doorbell eventfd | `shm_ring_doorbell::Doorbell::attach(eventfd, page)`, with 5 |
| 5 | doorbell page | writable: the program's own page |
| 6 | holds page | writable: `shm_ring_holds::Marks::over(WriteMap::new(fd), buffers, 0, 0)` |
| 7 and on | pool buffers | one per buffer for shared memory (one descriptor), or one per plane of each buffer for dma-bufs |

Then `shm_ring_buffers::BufferReader::new(reader, FieldId(0), pool_view, marks)` gives frames:
`next()` returns a batch whose `held` keeps its buffers until dropped, and whose `meta` is the
frame's metadata (sequence, identity and length, 20 little-endian bytes: see
`lemnos_wire::ring::FrameInfo`). The frame's time is the batch's time (`CLOCK_MONOTONIC`). The
ring's end word (`Reader::end`) is 0 while the stream runs, 1 once it closed, 2 once it faulted.

The descriptors of a stream of planes name each plane as its own pool buffer: buffer `b`'s plane `p`
is pool buffer `b × planes + p`, and a frame names all of them in its entry. `FrameInfo` and the
frame's first pool buffer are the whole frame as far as the shm crates are concerned.

## Streams made by the kernel module

A block whose DMA must be checked in the kernel has its stream's producer there. The
[kernel module](kernel-module.md) is given the stream's dma-bufs, pins them, and lets the block
write only inside them. The receiver and both halves of the ISP on the BCM2712 work this way. The
module's own ring keeps the protocol it had, which the driver that runs the block reads; it is not
yet the shm ring a consumer reads, and [`TODO.md`](../TODO.md) lists it.

## What is not proven

No stream has carried a frame from real hardware yet. The dma-buf paths are tested against test
allocators, and against a real dma-heap when `LEMNOS_TEST_DMA_HEAP` names one
(`/dev/dma_heap/system`), never against a block. See [Hardware status](hardware-status.md).
