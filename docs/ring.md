# The shared-memory ring

Lemnos hosts one shared-memory ring for the whole stack, as a family of standalone crates under
[`shm/`](../shm). They are the bottom layer: they depend on nothing in Lemnos (only `libc`, and
`serde` as an option), and Lemnos's own crates will use them exactly as an outside project does.
Other projects depend on them at a locked revision of this repository.

| Crate | What it is |
| --- | --- |
| `shm-sample-time` | `SampleTime`: nanoseconds of `CLOCK_MONOTONIC`, and `SampleTime::now()`. `no_std` |
| `shm-ring` | One writer, many readers in a sealed memfd. A publish is memory writes only |
| `shm-ring-latest` | The newest value of each field, and whether it has expired |
| `shm-ring-time` | The value of a field at or before a sample time, and a bounded wait for it |
| `shm-ring-doorbell` | An eventfd per reader, rung only when that reader is asleep |
| `shm-ring-buffers` | Buffer topics: a pool of shared buffers handed over once, entries naming them |
| `shm-ring-holds` | Hold marks and their limits: a buffer a reader holds is never reused |
| `shm-fdpass` | Descriptors over a Unix socket, each set tied to one message |

`shm-measure`, `shm-ring-model` and `shm-ring-bench` are their tests' support, the `loom`
models and the bench; they are not published.

The family is Linux first. `shm-sample-time`, `shm-ring` and `shm-ring-time` also build for
Windows and macOS, and the gate checks that (`scripts/gate.sh`, the list `portable_crates`). The
portable parts are the layout, the heap-backed ring (`Heap` with `Reader::over` and
`Writer::over`), `at_or_before` and `SampleTime::now()`; the memfd ring, the descriptor
constructors and `wait_at_or_before` are Linux only. A time from one machine does not compare with
a time from another, on any OS.

## What it promises

- A publish makes no system call and allocates nothing. Waking a sleeping reader is one `write`.
- A reader maps the ring read-only, checks everything it reads and cannot disturb the writer or
  another reader. A hostile writer can only spoil its own ring.
- The header carries a magic and a layout number. A reader refuses a ring whose layout it does
  not read, so two processes built at different revisions fail cleanly instead of misreading.
- Lemnos's rules hold here too: `unsafe` only in the modules that need it (mappings, eventfds,
  descriptor passing, the clock), one operation per block with a `SAFETY` comment, and
  `scripts/check-boundaries.sh` refuses any dependency from `shm/` on the rest of the repository.

[`shm/README.md`](../shm/README.md) is the reference: every word of the layout, the publish and
read orders, why a reader never sees a torn sample, doorbells, buffer topics and hold marks.

## Where Lemnos stands

Lemnos's frame streams are this ring. The ring's layout is 2: word 10 of the header is the end code, which
layout 1 kept reserved and zero, and a layout 1 reader would take a closed or faulted topic for a running one.

The end word's values, which the host writes once, with the doorbell ringing after:

| End word | Name | Meaning |
|---|---|---|
| 0 | open | The stream is running. |
| 1 | closed | The host ended the stream: a stop, a close, or the host going away. Nothing went wrong with the stream. |
| 2 | consumer fault | The stream ended because this consumer broke the rules (stuck holds, too many jobs). |
| 3 | device failed | The stream will not continue on this attachment: the device producing it is turned off for good. A failure the host retries does not end the stream (see [streams.md](streams.md#producer-state)). |

An added value in the end word is not a layout change: the ring header's layout number stays 2.
A reader that does not know a code treats the stream as ended, kind unknown, never as running.

The producer's state (running, or failed and retried) is words 1 to 5 of the job-ack page, which is 64 bytes now (it was 8): [streams.md](streams.md#producer-state). It is not part of the ring header and moves no layout or hash.

Each structure's layout value is in the table at the end of [`shm/README.md`](../shm/README.md#layout-values).
A stream is a frame ring (`shm-ring`), a buffer pool and
its page (`shm-ring-buffers`), a hold page per consumer (`shm-ring-holds`), a doorbell
(`shm-ring-doorbell`) and a job ring with an acknowledgement page, all made by the host and
handed to the consumer as descriptors. [Frame streams](streams.md) says which descriptor is
which, and [Reading with the shm crates alone](streams.md#reading-with-the-shm-crates-alone) shows
a program that maps a stream with these crates and nothing else.

A kernel module stream is on this ring when it is created with the shm flag (`LEMNOS_STREAM_CREATE_SHM`);
the module writes layout 2 and refuses a reader's marks page of another layout. It checks the layout
before the page's size, as the shm crates do, because a layout 1 page is smaller. The refusal reaches the
program that registered the reader typed, as the same `Layout { which: Marks, found, expected }` as user
space's own refusal: `LEMNOS_STREAM_IOC_READER` fails with `EPROTO` and writes the refusal into its
argument (`refused`, `layout`, `found`, `expected`; [`kernel-module.md`](kernel-module.md#the-modules-own-streams)),
and `lemnos_linux_sys::stream::add_reader` returns it as `ReaderError::Layout(LayoutRefusal { which, found, expected })`.
A module from before those fields gives a bare `EPROTO`, which `add_reader` returns as `ReaderError::Os`.
The only caller of that ioctl in the tree is the QEMU harness's holds check (`layout` mode proves the
refusal on the pinned kernel). Neither lemnosd nor the client library registers a kernel reader today
(kernel-made rings reach clients through the daemon's own ring), so there is no daemon device state
or client `Error::Stream(RingError::Layout { .. })` for this path yet; whichever component first
registers a kernel reader on a client's behalf (outside readers, TODO I2d) converts a `LayoutRefusal` into
`RingError::Layout` and `Structure::Marks` into `SharedLayout::Marks`, which are the same three values.

## Measured

On the reference board, 10.5 µs median from publish to the reader holding a frame at 120 Hz with
hold marks, no system call by the publisher while the reader is awake, no allocation.
[`testing/board-runs/I1.md`](../testing/board-runs/I1.md) has the figures, where they come from, and
the check that repeats them on this repository's build.
