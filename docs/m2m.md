# V4L2 memory-to-memory sessions

Status: implemented in `lemnos-linux` (`v4l2::m2m`, feature `v4l2-m2m`, and `v4l2::request` for
stateless codecs) over the device calls in `lemnos-linux-sys::v4l2` and `media`. Host-tested against vim2m (scaler, MMAP and dma-buf input) and
vicodec (encoder, decoder, drain with LAST). On the BCM2712 boards (2026-11-03) the one M2M node
found opens and grants its queues; no stream has run on it, and no board device supports the
crop selection (see "Board results").

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

- `QueueSetup { format, memory, buffers, crop }` per queue. `crop: Option<Rect>` is
  `VIDIOC_S_SELECTION` with `V4L2_SEL_TGT_CROP`, sent after `S_FMT` and before `REQBUFS`.
  The rectangle the driver kept is `output_crop()` / `capture_crop()`. A device that does not
  crop that queue refuses the open with the driver's error. The format's variant (`Single` or
  `Multi`) and direction give the `VIDIOC` buffer type. `Memory::Mmap` or `Memory::DmaBuf`.
  Userptr is not supported.
- Order at open: the output (source) format, then the capture (coded) format, then the
  capture format is read back, then `REQBUFS` on each queue. The stateful order: an encoder
  sizes its coded buffers (`sizeimage`) from its source format, and it refuses a source format
  (`EBUSY`) once its capture buffers exist, so the source must come first. The stored
  `capture_format()` is the driver's answer for that sequence. The formats the driver kept are
  `output_format()` and `capture_format()`: `S_FMT` adjusts a format it does not take rather
  than failing, so check the pixel format before relying on it.
- `from_file(file, output, capture)` takes an open descriptor (`File` or `OwnedFd`) and sets
  up the same queues; `open(path, ..)` is `from_file` over the node. The descriptor is made
  non-blocking. V4L2 memory-to-memory state is per file handle, so a caller that sets controls
  (bitrate, profile, GOP) does it through the session's `AsFd` (`VIDIOC_S_EXT_CTRLS` on that
  descriptor): the session keeps the descriptor it was given.
- `Format`, `PixFormat`, `PixFormatMplane`, `PlaneFormat`, `fourcc(b"NV12")` are plain values.
- `set_output_crop(rect)` changes the source crop between frames (a scaler's source window).
  It returns the rectangle the driver kept. The driver refuses it while the queue streams
  (`EBUSY` on vim2m); the tests check that.
- Discovery without a session: `lemnos_linux::v4l2::query(path)` returns the node's
  `Capability` (driver name, `is_m2m()`, `is_m2m_multiplanar()`), opened read-only.
  `lemnos_linux::v4l2::find_m2m("rockchip-rga")` scans `/dev/video*` in numeric order and
  returns the first M2M node of that driver with its capability, or `None`. Nodes that
  fail `QUERYCAP` are skipped. Pick `Format::Single` or `Format::Multi` from
  `is_m2m_multiplanar()` before opening.
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
- Stateful decoders (the source-change flow): `from_file` with a capture `QueueSetup` of
  `buffers: 0` defers the capture buffers, because a decoder's capture format follows the
  stream. The session subscribes to `EVENT_SOURCE_CHANGE` at open (a driver without it refuses,
  which is no error). The loop: `stream_on()` starts the output queue only; `wait` with `event`
  (and `capture` once the capture queue streams: a driver reports an error for a queue that is
  not streaming); `take_source_change()` returns the change (`SourceChange::resolution()`);
  then `adopt_capture(buffers)` reads the new capture format and visible size
  (`capture_visible()`) and allocates the buffers, the caller queues them, and
  `start_capture()` streams the capture queue. On a resolution change once the capture buffers
  exist: `drain(Drain::Decoder)`, dequeue until LAST (`capture_drained()`), then
  `reconfigure_capture(buffers)` (the capture queue stops, its buffers are freed and
  re-requested; the output queue is not touched), queue the buffers, `start_capture()`.
- `drain(Drain::Encoder)` (`ENC_CMD_STOP`) or `Drain::Decoder` (`DEC_CMD_STOP`): the queued
  frames finish, and the last capture buffer is flagged `BUF_FLAG_LAST`. After that buffer,
  `dequeue_capture` returns `None` (the device reports `EPIPE`: nothing more is coming).
- `subscribe(event_type, id)` and `dequeue_event()`: control events take the control's id.
  A driver that does not produce an event type refuses its subscription (`EINVAL`); vim2m
  refuses end-of-stream and takes control events.

## Controls

- `M2mSession::set_control(id, value)` (`VIDIOC_S_CTRL`) returns the value the driver kept (the
  kernel writes it back; a driver may clamp it), and `control(id)` reads it (`VIDIOC_G_CTRL`).
  Free functions `lemnos_linux::v4l2::set_control(fd, id, value)` and `control(fd, id)` take any
  descriptor of the device: a control can be set before the session is built, on the descriptor
  the session then takes (`from_file`). Controls are per file handle.
- Plain integer controls only: the flips (`CID_HFLIP`, `CID_VFLIP`), `CID_ROTATE`,
  `CID_BG_COLOR`, and a codec's bitrate or GOP. A control with a payload needs the extended form
  (`VIDIOC_S_EXT_CTRLS`), which is not wrapped yet.
- vim2m tests: each flip combination mirrors the converted frame, and the value read back is the
  value kept.

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
- The tests below need `LEMNOS_TEST_DEVICE_NODES=1` as well as the module: finding a node opens and queries every video and media node on the machine, so a plain test run leaves them alone.
- With vim2m (`modprobe vim2m`): MMAP round trip with content and timestamps; dma-buf input
  from the system heap, read in place; the control-event subscription.
- With vicodec (`modprobe vicodec`): an encoder turns I420 into FWHT and is drained (its
  `LAST` buffer ends the stream). The capture size follows the source: the stored format
  equals the driver's answer after the source is set.
- With vicodec's decoder: the round trip (a coded stream in, I420 out, the first source change
  allocating the capture buffers), and a mid-stream resolution change (640x368, then 1280x720
  on the same decoder: the second change is reconfigured after a drain, and the output queue
  keeps running).
- With vim2m, `from_file` opens the same queues as the path form, and the descriptor is kept.
- The vicodec and vim2m tests skip with a note where the module is absent.

## Board results (BCM2712 boards, 2026-11-03)

Stateless, board #1 (`rpi-hevc-dec`, `/dev/video19`, media `/dev/media2`), 2026-11-03: one IDR frame
decoded through the request API and matched byte for byte against a host decode of the same
frame (`examples/hevc_idr_decode.rs`, feature `v4l2-hevc`):

- Stream: a 320x240 IDR (one slice, 3,977 bytes), encoded on the host with GStreamer's VA-API
  H.265 encoder (`vah265enc`); the reference is the host's VA-API decode (`vah265dec`) of the
  same stream, NV12. The host has no software HEVC decoder in ffmpeg.
- Headers: `lemnos-hevc` parses the VPS, SPS, PPS and the slice header (`lemnos-hevc`
  unit tests use the same fixture). The controls: SPS, PPS, SLICE_PARAMS, DECODE_PARAMS, with
  `DECODE_MODE` frame-based (the driver accepts only 1) and `START_CODE` Annex B. No scaling
  matrix (scaling lists are off in this stream).
- Driver details found on the board: `bit_size` is the slice data in bits from
  `data_byte_offset` to the end of the buffer (the whole buffer gives "data_byte_offset 7 +
  bits 31064 > bytesused 3883" in dmesg). Asking for NV12 with two planes gives `Nc12` back
  (`V4L2_PIX_FMT_NV12`'s 128-byte-column layout, luma and chroma each in columns of 128 bytes,
  stride 384 for 320 columns); a linear read of that layout is wrong, the column read matches.
- Result: 115,200 of 115,200 bytes equal the host reference (0 differ), in three runs. Request
  run 181 to 192 us (from queueing to completion), queue to decoded frame 188 to 200 us,
  `open` 0.9 to 1.3 ms, `stream on` 1.1 to 1.2 ms, request controls 13 to 32 us. Run under the
  board's camera device lock (taken with flock); no dmesg error, `vcgencmd get_throttled` 0x0.

Discovery was run on both boards with `v4l2::query`, `find_m2m` and the probe's `M2mSession::open`:

- Board #1 (`rpi-hevc-dec`, `/dev/video19`, the only M2M node on that board): `find_m2m("rpi-hevc-dec")`
  finds it. `M2mSession::open` succeeds with output `S265` (HEVC parsed slice data, 1 MiB source
  buffers, 2 granted) and a capture queue deferred (`buffers: 0`), and reports `cap_fmt` 1920x1080
  2 planes (2,073,600 and 1,036,800 bytes, 1920 bytes per line). No frame was streamed: this
  decoder is stateless (its output format is `S265`, with the request API and extended HEVC
  controls), and the session has neither requests nor extended controls.
- Board #2 (`pispbe`, `/dev/video20`..`35`): no node is M2M. Every node reports only
  `OUTPUT_MPLANE`, `CAPTURE_MPLANE` or metadata, and `find_m2m("pispbe")` returns `None`.
  `M2mSession::open` on a pispbe node is refused: "the descriptor is not a memory-to-memory
  device (driver pispbe)", `InvalidInput`. The ISP runs jobs through media requests across its
  per-direction nodes, and the session does not do requests.
  - `/dev/video20`..`27` are held open by the camera stack, and `S_FMT` on
    `/dev/video20` returns `EBUSY`. Nothing was changed there.
  - `S_FMT` on idle `/dev/video28` succeeds (`RG16` 1920x1080, `size_image` 4,147,200). No
    buffers were requested and nothing was streamed.

Crop (`VIDIOC_S_SELECTION` / `G_SELECTION`), the success path that `QueueSetup::crop` and
`set_output_crop` depend on: no device on either board supports it.

- Board #2: `G_SELECTION` and `S_SELECTION` return `ENOTTY` for every target (`CROP`,
  `CROP_DEFAULT`, `CROP_BOUNDS`, `COMPOSE`, `COMPOSE_DEFAULT`, `COMPOSE_BOUNDS`) on both the
  output and the capture queue of `/dev/video28` and `/dev/video31` (and `video35`), so the
  crop path is refused at the driver. The ISP takes its crop in its config metadata, not
  through selection.
- Board #1: the same `ENOTTY` on every target of `/dev/video19`.
- The host's vim2m, the only device that has run the success path, also reports `ENOTTY`.

Consequence: `QueueSetup::crop` and `set_output_crop` have no verified success path on the
boards. The crop code is unchanged and still host-tested only for the refusal. A device with a
real selection, such as a scaler, is needed to verify it.

## Requests (stateless codecs)

A stateless device takes each frame's parameters with its buffers as one request on the media
device. `lemnos_linux::v4l2::request`:

```rust
let media = request::media_device_for("/dev/video19")?;          // the node's /dev/mediaN
let req = Request::allocate(&File::open(media)?)?;               // MEDIA_IOC_REQUEST_ALLOC
s.set_request_controls(&req, &[ExtControl { id, value: ExtValue::Int(1) }])?;  // or ExtValue::Bytes(struct)
s.queue_output_in_request(index, &planes, ts, &req)?;            // QBUF with REQUEST_FD
req.queue()?;                                                    // MEDIA_REQUEST_IOC_QUEUE: runs it
req.wait(Some(timeout))?;                                        // POLLPRI when completed
// ... dequeue as usual, then:
req.reinit()?;                                                   // MEDIA_REQUEST_IOC_REINIT
```

`media_device_for` lists each `/dev/mediaN` and matches the node's device numbers against its
entities (`MEDIA_IOC_ENUM_ENTITIES`). `set_request_controls` is `VIDIOC_S_EXT_CTRLS` with
`which = V4L2_CTRL_WHICH_REQUEST_VAL` on the session's descriptor; a compound control's payload
is the struct's bytes, copied by the kernel. A refused control is named by its index in the
error. Controls are per file handle, so the session's descriptor is the one that is set.

Host-tested against vicodec's stateless decoder (`stateless-decoder`, found by its sysfs name):
a coded frame from the stateful encoder decodes through a request. Its FWHT parameters are a
request control, read from the frame header. The frame's data follows the 44-byte header: with
the header in the buffer, the driver flags both buffers in error. The decoded I420 frame is the
right size and not blank.

## Migration notes

For a consumer with its own M2M code (the camera stack's `M2mSession`, and the 2D unit
binding):

- Per-slot holds (`M2mSession<H>`): not needed. The kernel keeps the dma-buf while it is
  queued. A consumer that wants its frame object to live until the device gives the buffer
  back keeps its own record per index: `queue_*` then `dequeue_*` marks when it may go.
- `open_importing` becomes `QueueSetup { memory: Memory::DmaBuf, .. }` on the source;
  `push_dmabuf` becomes `queue_output` with `PlaneMemory::Dmabuf`; `push` and `push_image`
  (copies into MMAP buffers) become `output_plane_mut` plus `queue_output`.
- `Span` and `ImageRows` (the colour-plane layout helpers) are image layout, not a session
  concern: they stay with the consumer.
- `pull`/`release` become `dequeue_capture`/`queue_capture`; `data(frame, plane)` becomes
  `capture_plane(index, plane)`, with the length from `Dequeued::bytes_used`.
- `drain()` becomes `drain(Drain::Encoder)`; decoders use `Drain::Decoder`.
- Heaps for the buffers: use `lemnos_linux::dma` (see [dma.md](dma.md)).
