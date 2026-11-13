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
- **The ring**: shared memory with three queues. *Frames* go from the driver to the program.
  *Returns* give buffers back. *Jobs* go from the program to the driver.
- **The doorbell**: an eventfd the daemon rings after each frame, for a program that wants to
  sleep until one arrives.

The driver is the **producer**. The program that opened the stream is the **consumer**. A stream
has one consumer at a time: a second open is refused with `BUSY`.

The daemon makes the memory, fresh for each consumer, and passes it over the socket as file
descriptors. A consumer never sees what the one before it left, and a consumer that goes away
takes nothing with it: the buffers its driver held come back empty-handed.

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
The consumer reads the buffer, then returns it. A buffer the consumer holds is not written by
anyone else.

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
`SensorInfo::embedded_data`). A receiver whose sensor does publishes them on a second stream,
named by `embedded_stream` (`camera.receiver/1`): one buffer per frame, with that frame's sequence
number, in the sensor's own layout, which the sensor's library crate decodes. A receiver that
captures none answers an empty name. In the tree, no receiver captures embedded lines yet
(`rp1-csi2`, `rkcif`, `sunxi-vin`, `k230-capture`), and no sensor on a board carries them, so the
path is exercised only by the simulated camera. See [Hardware status](hardware-status.md).

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
| `camera.isp/1` | `output_stream` | `set_output` returns `OutputLayout::size`, the most bytes an image takes |
| `camera.isp/1` | `input_stream` | Not stated by the contract yet: `set_input` takes only the frame's size and bit depth, not its packing |

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

## Faults

Neither side trusts the ring. Each value is read once and checked.

- A consumer that corrupts the ring, returns a buffer it does not hold or posts bytes that are no
  job **loses that stream**. Its state becomes *faulted* and the doorbell rings. The connection,
  the device and every other stream carry on. Opening the stream again starts clean, on new
  memory.
- A consumer that closes its connection gives up every stream it held.

## Giving a stream back

A consumer gives a stream back with `close_stream`, or by dropping it. Dropping a stream the
consumer never closed does the same: its close is queued in the client, and sent with the client's
next request. Nothing waits for that close. The next open of the same stream on the same client
sends the close first, so it is never refused for a stream the client already let go.

When the service ends a stream it does the following, however the consumer went:

- the buffers the consumer still held go back to the free pool, and nothing it held is kept;
- the ring is marked closed, so a consumer that is still reading it sees the stream has ended;
- until a consumer attaches again, each frame the producer publishes is counted as dropped, and
  `stream_stats` reports `attached` as false. A driver learns the consumer is gone from those
  counts and from its own `publish` results, not from a callback.

A second open while a consumer still holds the stream is refused with `BUSY` straight away. An
open that the service doesn't answer fails with `TimedOut` once the client's deadline passes (five
seconds by default, `set_timeout` changes it); it never waits without one.

## In a program

`lemnos-client` opens a stream with `open_stream`, or `open_stream_writable` for one the consumer
fills. Both take a `StreamPlatform`, which does what only the operating system can: receive the
descriptors and map them. On Linux that is `LinuxStreams` from `lemnos-linux-stream`
(`backends/linux-stream`). It also tells the kernel around each access to a dma-buf. With its
default features off it is the mappings alone, over `lemnos-wire`'s ring, for a program that has
no client: see [Used as libraries](drivers.md#used-as-libraries).

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

The daemon's tests run every case on this page against drivers made for the purpose: frames, jobs,
a consumer that fills buffers, dma-bufs, and each fault.

```sh slow
cargo test -q -p lemnosd --test stream --test stream_fill --test stream_dma
```

## Streams made by the kernel module

A block whose DMA must be checked in the kernel has its stream's producer there. The
[kernel module](kernel-module.md) is given the stream's dma-bufs, pins them, and lets the block
write only inside them. The ring is the same ring and the consumer cannot tell the difference. The
receiver and both halves of the ISP on the BCM2712 work this way.

## What is not proven

No stream has carried a frame from real hardware yet. The dma-buf paths are tested against test
allocators, and against a real dma-heap when `LEMNOS_TEST_DMA_HEAP` names one
(`/dev/dma_heap/system`), never against a block. See [Hardware status](hardware-status.md).
