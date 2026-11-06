# lemnos-v4l2

Safe Rust interfaces to Linux camera devices, with no C libraries:

- `v4l2`: video nodes (`/dev/videoN`): capabilities, formats, frame sizes and intervals,
  selections, MMAP and dma-buf buffer queues, streaming, extended controls, and a queue
  wrapper that maps every buffer (`MmapQueue`).
- `subdev`: subdevices (`/dev/v4l-subdevN`): media bus codes and formats, frame sizes and
  intervals, selections, routing, and controls.
- `media`: the media controller (`/dev/mediaN`): device info, the topology graph (entities,
  pads, links, interfaces), link setup, the lock holder, and requests.
- `event`: V4L2 event subscription and dequeue, shared by video nodes and subdevices.

Every device owns its descriptor, opened non-blocking, and implements `AsFd`, so an async
reactor can wait on it. Dequeue calls return `Ok(None)` when nothing is ready.

The system calls and uAPI structures come from `lemnos-linux-sys`; this crate has no
`unsafe` code. Errors name the call that failed and its errno. The memory-to-memory session
API is in `lemnos-linux` (`v4l2::m2m`), not here.

Linux only: the crate compiles to nothing on other targets.
