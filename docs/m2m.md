# V4L2 memory-to-memory sessions

Status: implemented in `lemnos-linux` (`v4l2::m2m`, feature `v4l2-m2m`) over the device calls
in `lemnos-linux-sys::v4l2`. Host-tested against vim2m (scaler, MMAP and dma-buf input) and
vicodec (encoder, decoder, drain with LAST). Written, untested on board: the hardware M2M
devices (2D units, RGA-class scalers, stateful codecs on the board's SoCs).

## What a session is

One open `/dev/videoN` node that is memory-to-memory: frames go in on its *output* queue
(the source) and come out on its *capture* queue (the destination). A scaler or colour
converter, a stateful encoder, a stateful decoder all fit. A session holds the node with both
queues on it. Generic only: the session knows queues, formats, buffers, events and drains;
what the device does with the frames belongs to the consumer.

## API

```rust
let mut s = M2mSession::open("/dev/video0", output_setup, capture_setup)?;
for i in 0..s.capture_buffers() { s.queue_capture(i, &mmap_plane)?; }
s.stream_on()?;

let index = s.free_output().expect("a free source buffer");
// MMAP source: write the frame, then queue it. dma-buf source: queue the buffer's fd.
s.output_plane_mut(index, 0).unwrap().copy_from_slice(&frame);
s.queue_output(index, &[QueuePlane { memory: PlaneMemory::Mmap, length: 0, bytes_used, data_offset: 0 }], ts)?;

s.wait(Ready { capture: true, ..Default::default() }, Some(timeout))?;
if let Some(done) = s.dequeue_capture()? {
    let frame = &s.capture_plane(done.index, 0).unwrap()[..done.bytes_used[0] as usize];
    s.queue_capture(done.index, &mmap_plane)?;   // give it back
}
s.drain(Drain::Encoder)?;                         // or Drain::Decoder; last buffer comes back LAST
s.stream_off()?;                                  // every buffer comes back
```

- `QueueSetup { format, memory, buffers }` per queue. The format's variant (`Single` or
  `Multi`) and direction give the `VIDIOC` buffer type. `Memory::Mmap` or `Memory::Dmabuf`.
  Userptr is not supported.
- `open` sets the capture format first, then the output format (an encoder sizes its coded
  buffers from its source format, which is the order the stateful devices want), then
  `REQBUFS` on each queue. The formats the driver kept are `output_format()` and
  `capture_format()`: `S_FMT` adjusts a format it does not take rather than failing, so
  check the pixel format before relying on it.
- `Format`, `PixFormat`, `PixFormatMplane`, `PlaneFormat`, `fourcc(b"NV12")` are plain values.
- Buffers are tracked by index in a 64-bit mask, so a session has at most 64 per queue. A
  buffer is the caller's between `dequeue_*` and `queue_*`, and the device's in between.
- `queue_output` takes the timestamp; the capture buffer carries it back
  (`Dequeued::timestamp`).

## Ordering and events

- Capture buffers are queued before `stream_on` (the device needs a place for the first
  frame). `stream_on` starts the capture queue, then the output queue.
- `stream_off` stops both queues and clears the session's record of queued buffers.
- Waiting: `wait(Ready { output, capture, event }, timeout)` polls the device; `output` means a
  source buffer is free, `capture` means a frame is out, `event` means an event is pending.
  Dequeues never block: they return `None` when nothing is ready.
- `drain(Drain::Encoder)` (`ENC_CMD_STOP`) or `Drain::Decoder` (`DEC_CMD_STOP`): the queued
  frames finish, and the last capture buffer is flagged `BUF_FLAG_LAST`.
- `subscribe(event_type, id)` and `dequeue_event()`: control events take the control's id.
  A driver that does not produce an event type refuses its subscription (`EINVAL`); vim2m
  refuses end-of-stream and takes control events.

## Zero copies

- MMAP buffers are the driver's; the caller writes into them through `output_plane_mut` and
  reads captures through `capture_plane`. These are the only copies a session makes, and they
  are the caller's.
- dma-buf buffers are queued by descriptor. The device reads or writes them where they are.
  The device keeps its own reference from `queue_*`, so the caller may close its descriptor
  afterwards.
- Steady-state calls (queue, dequeue, wait) allocate nothing; mappings and queue records are
  made at `open`.

## Tests

- Without hardware: the queue bookkeeping (indices, planes, memory kinds, the free mask), the
  format layout and the queue type rules, refusal of a non-M2M node.
- With vim2m (`modprobe vim2m`): MMAP round trip with content and timestamps; dma-buf input
  from the system heap, read in place; the control-event subscription.
- With vicodec (`modprobe vicodec`): an encoder turns I420 into FWHT, a decoder turns it back,
  and each is drained: both return a `LAST` buffer.
- The vicodec and vim2m tests skip with a note where the module is absent.

## Migration notes

For a consumer with its own M2M code (the camera stack's `M2mSession`, and the 2D unit
binding):

- Per-slot holds (`M2mSession<H>`): not needed. The kernel keeps the dma-buf while it is
  queued. A consumer that wants its frame object to live until the device gives the buffer
  back keeps its own record per index: `queue_*` then `dequeue_*` marks when it may go.
- `open_importing` becomes `QueueSetup { memory: Memory::Dmabuf, .. }` on the source;
  `push_dmabuf` becomes `queue_output` with `PlaneMemory::Dmabuf`; `push` and `push_image`
  (copies into MMAP buffers) become `output_plane_mut` plus `queue_output`.
- `Span` and `ImageRows` (the colour-plane layout helpers) are image layout, not a session
  concern: they stay with the consumer.
- `pull`/`release` become `dequeue_capture`/`queue_capture`; `data(frame, plane)` becomes
  `capture_plane(index, plane)`, with the length from `Dequeued::bytes_used`.
- `drain()` becomes `drain(Drain::Encoder)`; decoders use `Drain::Decoder`.
- Heaps for the buffers: use `lemnos_linux::dma` (see [dma.md](dma.md)).
