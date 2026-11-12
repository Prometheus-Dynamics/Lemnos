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

- **The buffers**: `buffers` of them, each `buffer_bytes` long.
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
`try_next` takes a frame without waiting.

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
