# I2d: outside readers on a stream's own buffers, on the reference board

Status: untested on board. Reader tickets (`docs/streams.md`, "Reader tickets") are built and tested on
this PC with daemon-made streams: shared memory, and dma-bufs from the system heap. Nothing has run on
the board's hardware yet.

What has run on the PC: `lemnos-wire`, `lemnos-host`, the shm crates (`shm-ring`, `shm-ring-buffers`,
`shm-ring-holds`, `shm-ring-doorbell`, `lemnos-linux-sys`), all of `lemnosd` (including the reader
processes in `service/lemnosd/tests/stream_readers.rs`: the third-process read, the attack routes on the
ring, the pool page and the buffers, the cheat and hold limits, the dead reader, the slots and BUSY, the
grant budget, the import policy, the cut-off reason and time), and the driver suites and camera proof
listed in the PR. Kernel-made streams are not ticketed here: the module's ring is still on its own
layout, and its read-only dma-buf export and tail-slack parameter are I2b's work.

Before running: take `/run/lock/board-gpu.lock`, use the CM5 (Raze #2), and build on NVMe or SSD, never on
the Github HDD. Record the date, image and commit at the end.

## 1. Import of a read-only dma-buf on the reference board's GPU

Another team has measured this already (V3D 7.1.10.2, kernel 7.2.9-v8-16k, as root): a read-only heap
dma-buf imports into the GPU, its bytes read back correctly, and a device write is seen through the host's
read-only mapping. Confirm it with this PR's build: a daemon stream on the system heap with `writer =
"hardware"` and `readers = 1`, a reader ticket with `import = true` on a board whose `reader_import = true`,
and the reader imports the buffer into the GPU and reads its bytes back. Expected: the same as the measured
result. Record the imported bytes and the write seen.

## 2. Tickets on the camera chain's streams

On the reference board's camera chain (the streams a driver fills in hardware: the RP1 CSI-2 frames and the
ISP outputs, once their boards set `readers`): take a reader ticket on the consumer's stream while the
camera runs, read 120 frames at 120 frames a second in a separate process, and check that each frame's
bytes are its own (validated after the read) and the consumer is not faulted. Expected: every frame read
validates, the consumer's `faults` stays 0. A kernel-made stream needs I2b before it can be ticketed.

## 3. Reader arrival and departure during the camera run

With the camera running and the consumer keeping up, start and kill a reader process 20 times, by the PID
the test starts. Expected: the consumer's frame count and its frame timing do not change beyond noise, and
each killed reader's slot is free within 2 seconds (the liveness pipe hangs up).

## Record

Date, image, commit, and for each section: the command, the output, and any refusal with its text.
