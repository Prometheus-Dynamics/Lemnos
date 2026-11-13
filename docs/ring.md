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

Lemnos's frame streams are this ring. Word 10 of the ring header, the end code, was reserved zero in
layout 1; giving it a meaning that a reader may ignore is additive, so the layout number stays 1. A stream is a frame ring (`shm-ring`), a buffer pool and
its page (`shm-ring-buffers`), a hold page per consumer (`shm-ring-holds`), a doorbell
(`shm-ring-doorbell`) and a job ring with an acknowledgement page, all made by the host and
handed to the consumer as descriptors. [Frame streams](streams.md) says which descriptor is
which, and [Reading with the shm crates alone](streams.md#reading-with-the-shm-crates-alone) shows
a program that maps a stream with these crates and nothing else.

The kernel module's own streams are not on this ring yet: the module keeps the stream protocol it
had, which the driver that runs it reads. That is listed in [`TODO.md`](../TODO.md).

## Measured

On the reference board, 10.5 µs median from publish to the reader holding a frame at 120 Hz with
hold marks, no system call by the publisher while the reader is awake, no allocation.
[`testing/board-runs/I1.md`](../testing/board-runs/I1.md) has the figures, where they come from, and
the check that repeats them on this repository's build.
