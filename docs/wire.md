# The Lemnos socket protocol

Version 1. A client written in any language needs only this file. The test vectors at the end are
checked against the code by `engine/wire`, in both directions.

## Transport

A Unix domain stream socket. Both sides send at any time. A connection carries one byte stream each
way, and frames are not aligned to reads. All integers and floats are little-endian.

## Frames

```text
offset  size  field
0       4     length: the bytes after this field
4       2     kind
6       ...   the kind's fields, in order
```

The largest frame is 1 MiB, header included. A sender refuses to build a larger one. A receiver
closes the connection on a larger length, without reading it.

| Type | Encoding |
|---|---|
| `u8` to `u64`, `i8` to `i64`, `f32`, `f64` | fixed width |
| `bool` | one byte, 0 or 1. Anything else is malformed |
| `str`, `bytes` | a `u32` length, then the bytes. A `str` is valid UTF-8 |

## The kinds

| Kind | Name | Fields | Direction |
|---|---|---|---|
| 1 | Hello | core u64, lemnos name (32 bytes), version u16, name str, events bool, parts (tail) | to host |
| 2 | Welcome | core u64, lemnos name (32 bytes), version u16, features u64, host str, parts (tail) | to client |
| 3 | List | id u32 | to host |
| 4 | Device | id u32, device u32, name str, driver str, tags str, state u8, health u8, reason str, link str | to client |
| 5 | ListDone | id u32 | to client |
| 6 | Open | id u32, device str, tag str | to host |
| 7 | Opened | id u32, device u32, contract u8 | to client |
| 8 | Describe | id u32, device u32 | to host |
| 9 | Description | id u32, bytes | to client |
| 10 | Call | id u32, device u32, contract u8, method u16, reply_capacity u32, args bytes | to host |
| 11 | Reply | id u32, bytes | to client |
| 12 | Subscribe | id u32, device u32, contract u8, method u16, period_us u64, only_changes bool | to host |
| 13 | Subscribed | id u32, subscription u64, period_us u64 | to client |
| 14 | Unsubscribe | id u32, subscription u64 | to host |
| 15 | Value | subscription u64, stamp_ns u64, bytes | to client |
| 16 | SetPower | id u32, device u32, on bool | to host |
| 17 | StreamOpen | id u32, device u32, name str | to host |
| 18 | StreamRings | id u32, buffers u32, descriptors u8, buffer_bytes u32, frame_slots u32, job_slots u32, job_bytes u32, working Tail u32, kind Tail u32 | to client |
| 19 | Event | device u32, state u8, health u8, reason str | to client |
| 20 | Refusal | id u32, status i32, text str | to client |
| 21 | StreamClose | id u32, device u32, name str | to host |
| 22 | StreamOpenHandles | id u32, device u32, name str | to host |
| 23 | StreamRingHandles | id u32, buffers u32, descriptors u8, buffer_bytes u32, frame_slots u32, job_slots u32, job_bytes u32, plane_bytes bytes, working Tail u32, kind Tail u32 | to client |
| 24 | Packages | id u32 | to host |
| 25 | Package | id u32, path str, driver str, reason str | to client |
| 26 | PackagesDone | id u32, mode u8, note str | to client |
| 27 | CallJob | id u32, device u32, contract u8, method u16, args bytes | to host |
| 28 | JobStarted | id u32, job u64 | to client |
| 29 | JobProgress | job u64, done u64, total u64 | to client |
| 30 | JobDone | job u64, status i32, bytes | to client |
| 31 | JobCancel | id u32, job u64 | to host |
| 32 | EndpointsOpen | id u32, device u32 | to host |
| 33 | Endpoints | id u32 | to client, with two descriptors |
| 34 | EndpointsClose | id u32, device u32 | to host |

`state`: 0 running, 1 backing off, 2 off. `health`: 0 ok, 1 degraded, 2 failed. `status` is a
`lemnos-abi` status: 0 is success, a negative Linux errno is that errno, and lower codes are Lemnos's.
`tags` is comma-separated. An empty `device` in Open means any device with that tag.

`link` in Device is the name of another device this one is wired to, as the board file's `link`
says, or empty when it names none. The host carries it and never reads it: what the wiring means
is between the two devices' contracts. A camera sensor's `link` names its receiver and the
receiver's names its ISP, so a client follows a sensor to both through List alone. `link` was
appended to Device (rule 8): a host from before it sends Device without it, which reads as empty.

`features` in Welcome is a set of bits, each saying the host can do one thing added to version 1
after its first form. A host leaves the bits it doesn't know clear, and a client ignores the bits
it doesn't know. A client uses what a bit stands for only when the host set it: an older host
closes the connection on a kind it doesn't know (rule 3).

| Bit | Value | The host |
|---|---|---|
| 0 | 1 | takes StreamOpenHandles, and may answer it with StreamRingHandles |
| 1 | 2 | takes Packages |
| 2 | 4 | takes CallJob and JobCancel |
| 4 | 16 | takes EndpointsOpen and EndpointsClose, and answers the first with Endpoints |
| 4 | 8 | frame streams are the shm rings of [`docs/ring.md`](ring.md), with the descriptors of this page's stream answers |

`contract` is the tag's position in the device's list, which Opened reports. `method` is the number
the contract declares. `args`, `bytes` and values mean something only to the contract. The host never
reads them, and no field names a kind of device.

`stamp_ns` in Value, like a frame's timestamp on a stream's ring, is nanoseconds on the host's
clock: `CLOCK_MONOTONIC` on Linux, the same clock as the kernel's `ktime_get_ns()`, so it compares
directly with a client's own `clock_gettime(CLOCK_MONOTONIC)`.

## The handshake

The first frame each side sends is Hello from the client and Welcome from the host. Both begin
with the same fixed-size prefix, 42 bytes, and nothing else:

```text
offset  size  field
6       8     core: the core hash of the sender's build (see below)
14      32    lemnos: the sender's Lemnos version, UTF-8, NUL-padded
46      2     version: the protocol version (1)
```

A reader checks `core` first, then reads `lemnos`, and compares nothing else until both have
been read. A `core` that is not its own is an error at once: the reader closes the connection,
whatever follows the prefix, and never decodes the rest. It can read the peer's Lemnos version
from the same prefix to say which revision it is. Only the prefix's place is fixed for every
version, so a later change to the rest of Hello or Welcome can't make this check misread a peer.

A host that refuses a Hello for its core still answers with its own Welcome, with the same
prefix, and then closes. So the client learns the host's core and Lemnos version and can name
both.

## Core and parts

The wire is split into units, and each has its own hash. A client, a host and a driver package
need agree only on the units they use. A minor change to one unit does not refuse a peer that uses
another. Each hash is a 64-bit FNV-1a computed at compile time, not a number kept by hand, and each
is folded from one declaration: the frame table, the constants and the description's framing are
generated from the same tokens as the encoder and decoder, so a change to any of them moves the
hash.

- **The core** is the handshake, framing, the call and reply envelopes, devices, the event envelope,
  stream framing (StreamOpen, StreamReady, StreamClose), the description's framing, the stream ring's
  layout and the constants of all of these. Its hash is `CORE_HASH`, and Hello and Welcome carry it
  in the prefix. It is compared at once.
- **The parts** are the optional wire, each behind its feature bit in Welcome: stream handles
  (StreamOpenHandles and StreamHandles, with their limits), packages (Packages, Package and
  PackagesDone, with the package modes) and jobs (CallJob, JobStarted, JobProgress, JobDone and
  JobCancel). A part has its own hash, `PART_HASHES`. Hello and Welcome carry all of them in their
  tail, `parts`, as three 8-byte little-endian hashes in the order the parts are numbered (stream
  handles, packages, jobs). They are compared once the core agrees. A peer whose tail is empty has
  none to compare.

A **base** is the part of a unit both sides must agree on. Two things are left out of a base, and
so cannot move its hash: a frame kind marked `added` in the table, and a field whose type is `Tail`
(a trailing value, one a peer from before it does not send). A structure whose field is appended
therefore keeps its hash. The values this build computes are:

`core hash: 0x40b5b5290df32c80`

`stream-handles part hash: 0x5b32fb2ec8dfa5ed`

`packages part hash: 0x818f26c404b4f968`

`jobs part hash: 0x353035866faf89f5`

`shared-rings part hash: 0x8b6a280430008fea`

`endpoints part hash: 0x0360342362a7f9d9`

The hashes do not include the Lemnos version string. Two builds with the same core interoperate
whatever their Lemnos versions. What a client sees when the cores differ: the connection is refused
at connect with `LayoutMismatch`, whose text is `core protocol differs`, with both core hashes and
the host's Lemnos version.

**Parts are negotiated by name.** Hello and Welcome carry one record for each part the peer knows:
its name length (one byte), its name, and its hash (eight bytes, little-endian). A part the peer
names that this build does not know is ignored. A part both builds know with the same hash is
agreed, and its feature bit is usable. A part both know with different hashes is refused by name:
the host clears that bit in Welcome, the client clears it from its own features, and calls that
need the part answer `PartMismatch`, whose text names the part and both hashes. Every other part
keeps working on the same connection. A part the peer does not list is neither agreed nor refused.
Malformed records close the connection. The C API returns `VERSION_MISMATCH` with the same text for
a refused part.

## Rules

Each of these closes a way the earlier protocol could go wrong.

1. **The version is checked.** The first frame is Hello. A host answers a different version with
   Refusal and closes. A client refuses a different version in Welcome. The core is checked
   before the version, then the parts: see The handshake.
2. **Every request gets an answer**, matched by `id`: its own reply or a Refusal. Unsubscribe,
   SetPower, StreamClose and JobCancel have no reply of their own and are answered with an empty Reply. Nothing is ignored in
   silence. `id` 0 is allowed and is not special.
3. **A malformed frame closes the connection** on either side, because the stream can no longer be
   trusted. That covers an impossible length, an unknown kind, a short field, a bad `bool` and bad
   UTF-8.
4. **Values are refused, never cut.** Something too large for a frame is an error to whoever tried
   to send it. No text is shortened.
5. **Nothing is unbounded.** A client that lets more than 4 MiB of answers queue up is closed. Value
   frames are dropped first when a client falls behind; replies never are.
6. **Closing ends everything.** Whatever a connection held ends when it closes, greeted or not:
   subscriptions, claims and streams.
7. **A repeated Hello changes nothing** and is answered with Welcome.
8. **Trailing bytes after the known fields are ignored.** This is how a field is appended
   compatibly: an older reader stops before it. A struct is a block (see Structs below), so the same
   holds inside one: its extra fields are skipped, and a field its writer did not send takes its
   default.
9. **The socket is mode 0660** after binding, whatever the umask.
10. **An addition is announced before it is used.** A kind added to version 1 is sent to a host
    only when its bit is set in Welcome's `features`, and to a client only in answer to a kind
    that says the client knows it. So neither side is ever sent a kind it would close on.

## Long jobs

Some methods start work that runs for a long time: an image written to storage, say. The contract
marks such a method as a job (the Description says which, below), and it is started with CallJob
in place of Call. CallJob is sent only to a host that set bit 2 of `features` (rule 10). A Call to
a job method gets Refusal with `NOT_SUPPORTED`, and a CallJob to any other method gets Refusal with
`INVALID_ARGUMENT`. A host refuses with `BUSY` while 64 jobs are open.

The answer is JobStarted, with the host's number for the job, or Refusal. From then on the host
sends this connection, and no other:

- JobProgress whenever the job has come further: `done` of `total`, in whatever unit the contract
  says (bytes, steps), `total` 0 while not known. Only the newest matters, and a host may drop one
  a client hasn't read yet, as it drops values (rule 5).
- JobDone, once, when the job is over. `status` 0 is success and `bytes` is then the result the
  contract defines; otherwise `bytes` is empty. A cancelled job ends with `ECANCELED` (-125), and
  a job whose driver was lost (a panic, a rebind, the device switched off) with `EINTR` (-4).
  JobDone is never dropped.

JobProgress and JobDone for a job may come before its JobStarted, since the job runs on while the
answer is on its way: a client keeps them by job number until it knows the number.

JobCancel asks the driver to stop a job this connection started. It is answered with an empty
Reply, and the job's JobDone follows, with -125 or with how it really ended if it got there first.
A job another connection started is refused with `NOT_PERMITTED`, one that has finished or never
was with `NOT_FOUND`, and one an earlier run of the host started with `EINTR`: the high 32 bits of a
job number are different for each run of the host, so a number from before a restart is told from
one never given.

A job belongs to the connection that started it. When that connection closes, by any route, the
host cancels each of its jobs (rule 6). Nothing about a job survives the host: drivers keep no
state across a restart, so a client whose connection ended mid-job counts the job as interrupted.

## Driver packages

Packages asks what became of the driver packages the host was configured with. The host answers
with one Package for every file it looked at, in the order it looked, then PackagesDone. A Package
with a `driver` was loaded and that is its driver's name; one with an empty `driver` was refused
and `reason` says why, in words. `mode` in PackagesDone is 0 when no package directory is
configured, 1 when only signed packages run and 2 when any package runs. `note` is empty unless
the directory could not be used at all (signed mode with no keys, a directory that can't be read,
a host that can't load shared objects), and then says why. A host with packages off answers
PackagesDone alone.

A client sends Packages only to a host that set bit 1 of `features` (rule 10).

## Descriptors

**Declared break (descriptor layout).** The answers StreamRings and StreamRingHandles carry the
stream's shared-memory descriptors in the order of [`lemnos_wire::ring::fds`] (seven before the
buffers). The earlier answers, which carried three (or two plus buffers) with the older ring, are
gone: a host and a client built from different revisions cannot share a stream. What is additive:
the feature bit `SHARED_RINGS` (8), which is part 4 (`shared-rings`, whose hash folds the descriptor layout of [`ring::fds`](../engine/wire/src/ring.rs)): a client refuses to open a stream from a host that does not set it, and the shm layouts that the ring header and the marks page carry (`ring::layouts`). What is not: the
meaning of the descriptors of the answer kinds, which is why those kinds were renamed (the layout
hash moves with the names, so a mismatch is refused at the handshake rather than misread).

StreamRings says how many file descriptors arrive with it, as `SCM_RIGHTS` on the same `sendmsg`
that carries the frame's first byte: the seven of the stream's rings and pages, in the order
[`lemnos_wire::ring::fds`](../engine/wire/src/ring.rs) gives (the frame ring, the pool page, the job
ring, the job acks, the doorbell's eventfd and page, and the holds page), then the pool's buffers.
For a shared stream the buffers are one descriptor, so `descriptors` is 8. A client reads the
socket with `recvmsg` while a StreamOpen is outstanding, because a plain read discards
descriptors. Descriptors passed to the host are closed unread.

StreamRingHandles carries its descriptors the same way, in one message: the seven, then one descriptor
per plane of each buffer, buffer by buffer and each buffer's planes in order. `plane_bytes` is each
plane's length as a little-endian u32, at most 8 of them, adding up to `buffer_bytes`. It is empty
for a buffer of one plane, the usual case. `descriptors` is `7 + buffers × planes`, and
`buffers × planes` is at most 128, so at most 135 descriptors arrive. A client that sends
StreamOpenHandles gives its `recvmsg` room for 135 descriptors: what doesn't fit is lost, and the
stream with it. A client refuses a StreamRingHandles whose `buffers` is 0, whose `plane_bytes` isn't
whole lengths, more than 8 of them, a zero length, or a sum other than `buffer_bytes`, whose
`buffers × planes` is over 128, whose `descriptors` isn't `7 + buffers × planes`, or that came
with a different number of descriptors, and closes the connection, as for any frame that can't be
trusted.

## Streams

StreamOpen takes one stream of one device for this connection. `name` is the stream's name under
that device in the board file (`[devices.streams.<name>]`). A stream has one consumer: a second
StreamOpen gets Refusal with `BUSY`; an unknown device or name gets `NOT_FOUND`. StreamClose gives
it up; a connection closing gives up every stream it held. After StreamRings nothing about a frame
crosses the socket.

| Descriptor | What it is |
|---|---|
| frame ring | a sealed memfd holding a `shm-ring` ring of one field. Map it read-only, with `Reader::attach_from_start` |
| pool page | a sealed page of `shm-ring-buffers`: the buffers' tickets and the reader cut-offs. Read-only |
| job ring | a `shm-ring` ring the consumer writes, one field, a job per sample. Write end |
| job acks | a 64-byte page the host writes: word 0 is how many jobs it has collected; words 1 to 5 are the producer's state ([`streams.md`](streams.md#producer-state)). A reader that knows only word 0 ignores the rest. Read-only |
| doorbell eventfd | rung by the host after each frame. Waiting on it is optional |
| doorbell page | the consumer's own `shm-ring-doorbell` page, writable |
| holds page | the consumer's own `shm-ring-holds` page of marks, writable |
| buffers | the pool's buffers, one descriptor for shared memory, one per plane of each buffer for dma-bufs. Map read-only, or writable for a stream the consumer fills (below) |

The frame ring, the pool and the holds are the [shm family's](ring.md) layout, word for word; the
protocol does not restate it. Its end word says whether the stream closed (1), faulted (2) or ended because the device producing it failed (3). A value not listed is an end of unknown kind, not a running stream (an added value is not a layout change; [`streams.md`](streams.md#why-a-stream-ended)). Code 3 is declared outside the constants the core hash folds, so adding it moved no hash and a peer built before it still connects. The job-ack page also carries the producer's state (words 1 to 5, [`streams.md`](streams.md#producer-state)); its size is in no hash.

### Streams whose buffers are dma-bufs

A board file can say a stream's buffers are dma-bufs (`memory = "dma-buf"`), one per buffer, so
that a driver can hand a buffer to hardware. Such a stream can't be answered with StreamRings,
which carries one descriptor for all the buffers.

- A client that can take one descriptor per buffer sends StreamOpenHandles in place of StreamOpen,
  and only to a host that set bit 0 of `features`. It means the same as StreamOpen, and the answer
  is StreamRings for a stream of shared memory and StreamRingHandles for a stream of dma-bufs.
- StreamOpen for a stream of dma-bufs gets Refusal with `NOT_SUPPORTED` (-95), and the stream is
  left untaken. This is what a client from before this addition sees.
- StreamOpenHandles to a host from before this addition is an unknown kind and closes the
  connection, which is why a client looks at `features` first. Such a host has no streams of
  dma-bufs, and StreamOpen takes every stream it has.
- When the host can't get the dma-bufs (no such heap, or the heap refuses), the answer is Refusal
  with the reason as a status: `NOT_FOUND` for a heap that isn't there, `ACCESS`, `NO_MEMORY`, or
  `IO`. The host never makes such a stream out of shared memory instead.

StreamClose is the same for both.

| Descriptor | What it is |
|---|---|
| ring | as above |
| doorbell | as above |
| buffer 0 to `buffers - 1` | one dma-buf each, holding that buffer from offset 0 and at least `buffer_bytes` long. A heap rounds up, so it may be longer |

A client maps the first `buffer_bytes` of each, after
checking that the descriptor is at least that long and that its size can't change: it is a
dma-buf (`fstatfs` gives `DMA_BUF_MAGIC`), or it is sealed against shrinking. The ring, its words
and every check below are the same as for a stream of shared memory; only where buffer `n`'s bytes
are differs. The same memory is what the driver's hardware is given, so a frame is in the buffer
with no copy.

### Streams the consumer fills

On some streams the consumer puts bytes in buffers for the producer: a job's input image, a
configuration, a program. Which streams those are is the device's contract's to say, and the
consumer learns a stream's role from the contract method that named it:

| Contract | The consumer fills | The producer alone fills |
|---|---|---|
| `image.convert/1`, `image.scaler/1`, `image.blend/1` | the job stream (a job's input images) | |
| `npu/1` | the job stream (programs, tensors) | |
| `codec.block/1` | the job stream (a job's work and input buffers) | |
| `usb.function/1` | the data stream (bytes to send) | |
| `camera.isp/1` | the config stream; the input stream | the statistics and output streams |
| `camera.receiver/1`, `camera.frames/1` | | the frame stream |

A consumer maps the buffers of a stream it fills writable, and of any other read-only. Nothing in
the request or the answer differs: the buffers' memory is made for one attachment, only that
consumer and the producer ever see it, and the producer takes nothing in it on trust (it reads a
buffer once, into memory of its own, and checks the copy). `lemnos-client` does this with
`open_stream_writable` and `FrameStream::write`.

When a buffer is the consumer's to write is the contract's rule too. In every contract above: the
consumer writes a buffer before it posts the job that names it, and leaves it alone until that
job's completion has arrived; a buffer that arrived as a frame is the consumer's until it returns
it. A write at any other time races the producer or its hardware, and costs the consumer its own
result and nothing else.

On a stream of dma-bufs a device shares the memory, so the consumer tells the kernel around each
access by the processor (`DMA_BUF_IOCTL_SYNC`): start and end of a write around the copy, and
start and end of a read from when a frame is taken to when it is returned. The host does the same
around its own accesses.

`stride` is `buffer_bytes` rounded up to a multiple of 4096: a shared pool holds buffer `b` at
`b × stride`. A frame is an entry naming the pool buffers it occupies (one per plane), with a
20-byte metadata payload: sequence, identity and length, little-endian. Its time is the frame's
`CLOCK_MONOTONIC` time. A job is one sample of the job ring, whose bytes are the job.

The job acks word is the number of jobs the host has collected. The consumer posts a job only
while fewer than `job_slots` are waiting, which it knows from that word, so the job ring can't
overrun: a job posted beyond that is refused, not sent.

### What each side checks

The peer is not trusted. Everything the host reads from the consumer is read through the shm
crates, which check every index, length and stamp against the shape they read once, and never
trust a word twice.

- A frame's entry must name pool buffers the consumer's pool has, and its metadata must be exactly
  the 20 bytes. A frame's length must be at most the stream's frame size.
- The host reads the consumer's holds at one instant and cuts off a consumer whose marks are stuck
  past the hold time. A cut-off consumer's stream faults.
- A job is one sample of the job ring. A job the reader lost because the consumer published past
  the ring's capacity is an error: the consumer broke the rules.

A consumer that breaks these loses the stream: the host sets its end word to faulted (2) and rings
the doorbell. Nothing else is affected: not the connection, not other streams. A fresh StreamOpen
starts over on new memory.

The ring carries a job's bytes without reading them, so whether they are a job is for the driver to
say. A driver whose contract calls bytes that are no job a broken consumer ends the stream the same
way (`StreamEndpoint::fault_consumer`): state 2, the doorbell, and the jobs still queued are gone.
A job that decodes but can't be run is not that: it completes as failed, or is counted as dropped,
as its contract says, and the stream stays open.

## Endpoints

A USB function's two bulk endpoints (a board's gadget with a FunctionFS function, see
the gadget example in [boards](boards.md)) can be handed to one client, which reads and writes them itself. The
daemon keeps the function, its control endpoint and the bus; the client gets only the bulk pair.
EndpointsOpen is sent only to a host that set bit 4 (value 16) of `features` (rule 10). The three
frames are `added` to the core: they are left out of the core hash, and a host without them refuses
the kind alone with `UnknownKind`.

EndpointsOpenEnd is the same request for a client that wants the END descriptor too. It is sent only
to a host that set `ENDPOINTS_END` (value 64) in `features`: a host without that bit is never
sent it, and a client that wants it refuses by name, `NOT_SUPPORTED`, without sending anything. The
answer is EndpointsEnd, with three descriptors over `SCM_RIGHTS`: OUT, IN, then END. END is the read
end of a pipe whose write end the host keeps for this hold. The host closes that write end when the
hold is over for good, so END reads EOF or hangs up: on EndpointsClose, when the connection goes, when
the device is taken off or reloaded, and when the driver says its endpoints have ended. A holder
that sees END readable closes all three and asks again; `Endpoints` (kind 33) is unchanged and gets
no END.

EndpointsOpen takes the pair for this connection. The answer is Endpoints, with two descriptors over
`SCM_RIGHTS` in one message: the OUT endpoint's first, then the IN endpoint's. A client reads them
with `recvmsg`, as for StreamHandles, and closes the connection on an Endpoints that came with other
than two descriptors. The descriptors are the function's endpoint files, opened `O_NONBLOCK`: a
read, write or AIO submit made while the function is down gets `EAGAIN`, not a wait (see What a
holder sees). The host keeps its own copies, which it never reads while the pair is held.

| Refusal | Why |
|---|---|
| `BUSY` | another connection holds the pair |
| `NO_DEVICE` | the function is not enabled |
| `NOT_SUPPORTED` | the device's driver can't lend them, or the function isn't one bulk OUT and one bulk IN |
| `NOT_FOUND` | no such device |

Asking again on the connection that holds the pair gets new descriptors for the same hold. `BUSY`
is the only answer to another connection while a hold stands.

EndpointsClose gives the pair up and is answered with an empty Reply; `NOT_FOUND` when this
connection holds none. A connection that closes gives it up (rule 6). While a connection holds the
pair, the host serves no bytes on it itself: the data stream of `usb.function/1` waits, and the
control requests on endpoint 0 are still answered by the host, as they are without a client.

The pair outlives a host disconnect, a suspend, a bus reset and a configuration change made by the
host: the same descriptors work again once the host is back, and nothing is handed over again. While
the function is down, reads, writes and AIO submits get `EAGAIN`, and a transfer in flight when it
went down fails with `ESHUTDOWN` once. A holder keeps its descriptors and retries.

The hold ends for good only when the host ends it, and END (below) says so. The daemon ends it when
the holder releases the pair or goes, when the device is switched off, reloaded or shut down, and
when its driver says its endpoints have ended. After that the descriptors are dead, and a holder
closes all three and asks again.

### What a holder sees

Measured on the QEMU harness (`testing/crates/gadget-check`, the holder checks) with the pinned
7.3.3 kernel, with the holder a client of lemnosd (`usb_endpoints_end`) and the daemon its own
process. Rows: (a) a blocking read in flight, (b) a queued AIO read, (c) a write after the event, (d)
a new read afterwards. Each cell is the code the holder gets. ESHUTDOWN is returned once; every later
call while the function is down gets EAGAIN.

| Case | (a) read in flight | (b) AIO in flight | (c) write | (d) new read | What the holder does |
|---|---|---|---|---|---|
| 1 host disconnect (port off) | ESHUTDOWN | ESHUTDOWN | EAGAIN | EAGAIN | keeps the descriptors; the same ones work after on and bulk; END stays open |
| 2a suspend, resume | keeps waiting, no code | not measured | not measured | waits, then takes the host's bytes | nothing: the same descriptors; END stays open |
| 2b bus reset | ESHUTDOWN | not measured | not measured | waits for the host to configure, then works | keeps them; the host reconfigures; END stays open |
| 3 configuration 0, then 1 | ESHUTDOWN | new AIO EAGAIN | EAGAIN | EAGAIN | keeps them; they work again after configuration 1; END stays open |
| 4 UDC unbound; the daemon binds it again | ESHUTDOWN | ESHUTDOWN | EAGAIN, not ENODEV | EAGAIN, not ENODEV | keeps them; the daemon keeps ep0 open and binds again, and the same descriptors work; END stays open |
| 5 daemon switches the device off (client `set_power`) | not measured as sync | ESHUTDOWN | EAGAIN | EAGAIN; descriptors live | keeps its copies: umount and remount are EBUSY until it closes them; END hangs up |
| halted OUT endpoint (host SET_FEATURE halt) | keeps waiting, no code | not measured | not measured | takes the bytes once the host clears the halt | nothing |
| pending control request on ep0 (no data stream: the daemon stalls it at once) | keeps waiting, no code (the request is not on the pair) | not measured | not measured | not measured | the host sees the stall (EPIPE); the pair sees nothing |
| 6 the daemon's process dies with the pair held | keeps waiting, no code; then takes the host's bytes and writes them back | not measured | waits (write), not measured as an errno | waits, then takes the host's bytes | keeps its copies. END hangs up at once. A new daemon does not bind the gadget while the copies are open; once the holder closes them, it lends the pair and END again |

Notes on case 6: the endpoints stay enabled while the holder's copies are open, so the host still sees
the device and the reads and writes wait for it rather than failing. The new daemon's enable reads ep0
again, and the kernel refuses the descriptors while the endpoint files are open (EBADFD, the daemon
reports `could not be enabled: retrying`). END is the only signal that the old daemon is gone.

Two things can mislead. `ENODEV` never reaches a holder that keeps a file open: the function stays
enabled while any copy is open, so the code alone cannot tell case 1 (host away) from case 4 (gadget
unbound). Only END, and the function's `state` method, tell them apart. A write in the wrong
direction gets `EBADMSG` and halts the endpoint.

### The END descriptor

END is the third descriptor of `EndpointsEnd` (above). The host writes nothing to it. It reads EOF,
or hangs up, once the host ends the hold for good: a release, the holder going away, the device
switched off or reloaded, or the driver's endpoints ended. A holder that sees END readable closes all
three descriptors and asks again. A holder that sees only `ESHUTDOWN` or `EAGAIN` keeps its
descriptors and retries: the function is down for now.

A holder that keeps its copies blocks a remount of the function's mount (EBUSY), so the daemon's
setup of a gadget that a previous holder still has open cannot remount until that holder closes them.
END is how the holder learns that it must. A new daemon's enable does not reattach ep0 while those
copies are open: the kernel refuses the descriptors, and the daemon retries (with its backoff) until
the holder closes them. A controller that lets go of the gadget does not end the hold: the daemon
keeps ep0 open and binds the gadget again, and the holder's copies carry on (case 4 above).

### The bus state

The host reports the bus as it sees it on endpoint 0 through the function's `state` method (method
4 of `usb.function/1`). It returns a record of four bools in order: `up`, `configured` (the host
chose the configuration), `suspended` and `request_pending`. A client subscribes to it with
Subscribe, `only_changes` true, and receives a Value whenever one of them changes. That is how a
client learns of a connect, a disconnect, a suspend and a resume. Health reports the gadget
itself: `failed`, with its reason, while the function is down.

## Description

The `bytes` of a Description, in order: a `u16` contract count, then for each contract a
contract record (its tag str, its base hash u64, see Contract hashes below, and a `u16` method
count), then for each method a method record (its id u16, name str, a `u16` parameter count, a
has-out-buffer bool, and, appended to the record, its method hash u64 and an `added` bool), then for
each parameter a parameter record (its name str) followed by its shape. A method's returned shape
follows its parameters. Then a `u16` item count, and for each item
an item record: its kind u8 (0 value, 1 control, 2 action), name str, unit str, exponent i8, min and
max (each a presence bool, then an i64 when present), contract u8, method u16, and field (a presence
bool, then a str when present). Then, appended after the first form, a `u16` count of job methods
and for each a job record: its contract u8 and method u16. Those are started with CallJob. A host
from before job methods ends at the items, and a reader takes that as no job methods. A job method
that names no method above makes the description malformed. Then the trailer described below, and last
the added fields: a `u16` count, and for each a record (contract u8, method u16, the field's hash u64;
see Contract hashes). A host from before the added fields ends at the trailer, and a reader takes that
as no added fields. The method hashes, the `added` bools and the added-field records are new in T1d:
they moved the core hash, which is allowed while nothing is released.

A shape is written as a code u8 (0 unit, 1 bool, 2 u8, 3 u16, 4 u32, 5 u64, 6 i8, 7 i16, 8 i32,
9 i64, 10 f32, 11 f64, 12 str, 13 bytes, 14 option, followed by its shape, 15 array, a u32 length
then its shape, 16 record, a u16 field count, then each field's name str and shape). The codes and
the records are declared where the code is written, and the core hash folds them, so a change to
either moves the core.

**Structs.** A value of a struct type (`#[derive(Encode, Decode)]`, which every argument, result and
record of a contract is) is a block: a u32 byte length, then its fields in order. A reader takes
the block and reads the fields it knows. Bytes left after them are skipped. A field the block ends
before takes its default, so a field a writer from before it did not send reads as the default.
Every field type of such a struct implements `Default`.

Then, appended after the job list, the trailer: three parts. A reader from before them stops at the
job list, and takes the identity as unknown (an empty label, no state, no kind, no reading for any item).

- The identity, one struct block: `label` str (the board's `label`, or the device id when the board
  names none), `bus` str (the bus or block the board names, empty when none), `state` (a presence
  bool, then a u8: 0 running, 1 retrying, 2 off), `reason` str (why the device is not running, or its
  last failure; empty when none), `address` (a presence bool, then a u8: the 7-bit I2C address the
  driver named), `kind` str (the first contract kind that is not `other`, else `other`), and
  `location` str (where the driver says the device is, in words, for what an address cannot say;
  empty when none), and `not_live` (a bool: true when the device's driver is not running, so the description
  is the manifest's alone, with no items and no address or location; absent reads as false). `not_live` is the
  last field of the block: a reader from before it skips it, and a host from before it sends none.
- A `u16` contract count, then one block per contract: its `kind` str (`sensor`, `actuator`, ...,
  `other`).
- A `u16` item count, then one block per item, in the order of the items above: `reading` u8 (0
  number, 1 range, 2 switch, 3 text, 4 colour array, 5 three axes; the item's own reading, else its
  method's, else one its shape suggests), `step` (a presence bool, then an i64), `warn_low` and
  `warn_high` (each a presence bool, then an i64), and `group` (a presence bool, then a str: the
  items of one group are one vector, each part named by its `field`), and `unit_state` (a presence
  bool, then a u8: 0 known, 1 dimensionless, 2 unknown), and then `real_min`, `real_max` and
  `real_step` (each a presence bool, then an f64: the range and the step as real numbers in the
  item's unit, for a float value). Those three are the last fields of the block. A host from before
  the unit state sends none of them, and its item's state is decided by its unit string: unknown when
  the string is empty, known otherwise. An item with no real range reads the integer one.

A reading code this build does not know reads as no reading. Every i64 is in the item's exponent
(`raw * 10^exponent`, in its unit), like `min` and `max`. The identity is an addition (a `Tail` in
the code), so it is left out of the core hash: a host that adds it does not refuse a client built
without it.

The added fields come after the trailer, the last bytes of a description. Their count is a `u16`,
then one record per added field of every method, in order: its contract u8, its method u16, and the
field's hash u64. The hash is the one in [Contract hashes](#contract-hashes). A host from before them
ends at the trailer.

### Contract hashes

A contract's **base hash** is a 64-bit FNV-1a over its tag and each of its **base** methods'
hashes, in order. A method's **method hash** is over its number, name, its base parameters (names and
shapes), its base result shape, and whether it fills a buffer or starts a job. A base item is one
not marked `added` in the contract, or marked `#[breaking]` as well (see the
[driver guide](writing-a-driver.md#base-shape-and-additions)). A field marked `added` is left out of
every shape it sits in: a method whose argument struct gained a field keeps its method hash, and the
field has a hash of its own, from its name and shape under its path. The description carries the
base hash of each contract, each method's hash and `added` flag, and the hash of each added field.
They are computed by the `#[contract]` macro and the `Encode` derive, never written by hand.

A typed client checks them when it binds a contract (`Client::open`, `Session::open_typed`,
`AsyncClient::open_typed`), reading the device's description once. The check is in
[the client's typed bind](client.md#the-typed-bind): the base hash must be equal, and every method
the client has (base or added) must be on the device with the same method hash, with every added
field it uses. A refusal names the contract and the method or field, and nothing is called on it.
Calls after the open are not checked again.

No per-contract additions level is written: the per-method check gives the same guarantee and names
the method that is missing. A device that doesn't list the contract's tag is not refused by the
check (the open itself already found the tag).

A shape is a u8 code: 0 unit, 1 bool, 2 u8, 3 u16, 4 u32, 5 u64, 6 i8, 7 i16, 8 i32, 9 i64, 10 f32,
11 f64, 12 str, 13 bytes, 14 option (followed by its shape), 15 array (a u32 length, then its shape),
16 record (a u16 count, then each field's name str and shape). Shapes nest at most 8 deep.

## Events

A host sends an Event to a client that said `events: true` in its Hello, from that Hello onward.
The rules a client can rely on:

- Each change to a device's state, reason or health is sent as an Event. The Event carries the
  device's state and health as they are at send time, so it carries every change made before it.
- A change of health needs no change of state: the daemon looks at every device's health about
  four times a second, and a device whose health differs from what a client was last sent gets an
  Event within about a quarter of a second. Health is not sent again while it stays the same.
- A device whose driver is in a call is not looked at until the call returns, so its Event can
  come a little later than the change.
- Changes between two sends may come as one Event. An Event for a state change may repeat health
  the client already has (a device that retries sends one per retry), and a flapping health is
  sent at most once per look, so a client that reads Events sees each change once and never a
  flood. The newest Event for a device is its current state and health.
- A client that listed the devices and keeps reading gets every later change of state or reason on
  each of them. It does not need to list again to learn of a device that comes up later.

## Test vectors

Each vector is one complete frame.

### hello

`Hello { core: 0x7b015eb7597b20bc, lemnos: "1.0.0", version: 1, name: "tool", events: true, parts: the five part records }`

<!-- vector: hello -->
```hex
95 00 00 00 01 00 80 2c f3 0d 29 b5 b5 40 31 2e 30 2e 30 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 01 00 04 00 00 00 74 6f 6f 6c 01 5c 00 00 00 0e 73 74 72 65 61 6d 2d 68 61 6e 64 6c 65 73 ed a5 df c8 2e fb 32 5b 08 70 61 63 6b 61 67 65 73 68 f9 b4 04 c4 26 8f 81 04 6a 6f 62 73 f5 89 af 6f 86 35 30 35 0c 73 68 61 72 65 64 2d 72 69 6e 67 73 ea 8f 00 30 04 28 6a 8b 09 65 6e 64 70 6f 69 6e 74 73 d9 f9 a7 62 23 34 60 03
```

### welcome

`Welcome { core: 0x7b015eb7597b20bc, lemnos: "1.0.0", version: 1, features: 0, host: "h", parts: the five part records }`

<!-- vector: welcome -->
```hex
99 00 00 00 02 00 80 2c f3 0d 29 b5 b5 40 31 2e 30 2e 30 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 01 00 00 00 00 00 00 00 00 00 01 00 00 00 68 5c 00 00 00 0e 73 74 72 65 61 6d 2d 68 61 6e 64 6c 65 73 ed a5 df c8 2e fb 32 5b 08 70 61 63 6b 61 67 65 73 68 f9 b4 04 c4 26 8f 81 04 6a 6f 62 73 f5 89 af 6f 86 35 30 35 0c 73 68 61 72 65 64 2d 72 69 6e 67 73 ea 8f 00 30 04 28 6a 8b 09 65 6e 64 70 6f 69 6e 74 73 d9 f9 a7 62 23 34 60 03
```

### list

`List { id: 1 }`

<!-- vector: list -->
```hex
06 00 00 00 03 00 01 00 00 00
```

### device

`Device { id: 1, device: 2, name: "a", driver: "b", tags: "c/1", state: 1, health: 2, reason: "r", link: "d" }`

<!-- vector: device -->
```hex
27 00 00 00 04 00 01 00 00 00 02 00 00 00 01 00 00 00 61 01 00 00 00 62 03 00 00 00 63 2f 31 01 02 01 00 00 00 72 01 00 00 00 64
```

A host from before `link` sends the same frame without its last five bytes (length `22`), and a
client reads that as `link: ""`.

### list-done

`ListDone { id: 1 }`

<!-- vector: list-done -->
```hex
06 00 00 00 05 00 01 00 00 00
```

### open

`Open { id: 2, device: "", tag: "c/1" }`

<!-- vector: open -->
```hex
11 00 00 00 06 00 02 00 00 00 00 00 00 00 03 00 00 00 63 2f 31
```

### opened

`Opened { id: 2, device: 3, contract: 1 }`

<!-- vector: opened -->
```hex
0b 00 00 00 07 00 02 00 00 00 03 00 00 00 01
```

### describe

`Describe { id: 3, device: 3 }`

<!-- vector: describe -->
```hex
0a 00 00 00 08 00 03 00 00 00 03 00 00 00
```

### description

`Description { id: 3, bytes: [9] }`

<!-- vector: description -->
```hex
0b 00 00 00 09 00 03 00 00 00 01 00 00 00 09
```

### call

`Call { id: 7, device: 3, contract: 1, method: 2, reply_capacity: 16, args: [aa, bb] }`

<!-- vector: call -->
```hex
17 00 00 00 0a 00 07 00 00 00 03 00 00 00 01 02 00 10 00 00 00 02 00 00 00 aa bb
```

### reply

`Reply { id: 7, bytes: [1] }`

<!-- vector: reply -->
```hex
0b 00 00 00 0b 00 07 00 00 00 01 00 00 00 01
```

### subscribe

`Subscribe { id: 8, device: 3, contract: 0, method: 1, period_us: 5000, only_changes: true }`

<!-- vector: subscribe -->
```hex
16 00 00 00 0c 00 08 00 00 00 03 00 00 00 00 01 00 88 13 00 00 00 00 00 00 01
```

### subscribed

`Subscribed { id: 8, subscription: 4, period_us: 5000 }`

<!-- vector: subscribed -->
```hex
16 00 00 00 0d 00 08 00 00 00 04 00 00 00 00 00 00 00 88 13 00 00 00 00 00 00
```

### unsubscribe

`Unsubscribe { id: 9, subscription: 4 }`

<!-- vector: unsubscribe -->
```hex
0e 00 00 00 0e 00 09 00 00 00 04 00 00 00 00 00 00 00
```

### value

`Value { subscription: 4, stamp_ns: 1000, bytes: [5, 6] }`

<!-- vector: value -->
```hex
18 00 00 00 0f 00 04 00 00 00 00 00 00 00 e8 03 00 00 00 00 00 00 02 00 00 00 05 06
```

### set-power

`SetPower { id: 10, device: 3, on: false }`

<!-- vector: set-power -->
```hex
0b 00 00 00 10 00 0a 00 00 00 03 00 00 00 00
```

### stream-open

`StreamOpen { id: 11, device: 3, name: "s" }`

<!-- vector: stream-open -->
```hex
0f 00 00 00 11 00 0b 00 00 00 03 00 00 00 01 00 00 00 73
```

### stream-ready

`StreamRings { id: 11, buffers: 4, descriptors: 8, buffer_bytes: 256, frame_slots: 4, job_slots: 2, job_bytes: 8, working: 1, kind: 0 }`

<!-- vector: stream-rings -->
```hex
23 00 00 00 12 00 0b 00 00 00 04 00 00 00 08 00 01 00 00 04 00 00 00 02 00 00 00 08 00 00 00 01 00 00 00 00 00 00 00
```

### stream-close

`StreamClose { id: 12, device: 3, name: "s" }`

<!-- vector: stream-close -->
```hex
0f 00 00 00 15 00 0c 00 00 00 03 00 00 00 01 00 00 00 73
```

### stream-open-handles

`StreamOpenHandles { id: 13, device: 3, name: "s" }`

<!-- vector: stream-open-handles -->
```hex
0f 00 00 00 16 00 0d 00 00 00 03 00 00 00 01 00 00 00 73
```

### stream-handles

`StreamRingHandles { id: 13, buffers: 4, descriptors: 11, buffer_bytes: 256, frame_slots: 4, job_slots: 2, job_bytes: 8, plane_bytes: [], working: 1, kind: 0 }`

<!-- vector: stream-ring-handles -->
```hex
27 00 00 00 17 00 0d 00 00 00 04 00 00 00 0b 00 01 00 00 04 00 00 00 02 00 00 00 08 00 00 00 00 00 00 00 01 00 00 00 00 00 00 00
```

### stream-handles-planes

`StreamRingHandles { id: 14, buffers: 2, descriptors: 11, buffer_bytes: 256, frame_slots: 2, job_slots: 1, job_bytes: 0, plane_bytes: [192 as u32 LE, 64 as u32 LE], working: 0, kind: 0 }`

<!-- vector: stream-ring-handles-planes -->
```hex
2f 00 00 00 17 00 0e 00 00 00 02 00 00 00 0b 00 01 00 00 02 00 00 00 01 00 00 00 00 00 00 00 08 00 00 00 c0 00 00 00 40 00 00 00 00 00 00 00 00 00 00 00
```

### packages

`Packages { id: 14 }`

<!-- vector: packages -->
```hex
06 00 00 00 18 00 0e 00 00 00
```

### package

`Package { id: 14, path: "/p", driver: "d", reason: "" }`

<!-- vector: package -->
```hex
15 00 00 00 19 00 0e 00 00 00 02 00 00 00 2f 70 01 00 00 00 64 00 00 00 00
```

### packages-done

`PackagesDone { id: 14, mode: 1, note: "" }`

<!-- vector: packages-done -->
```hex
0b 00 00 00 1a 00 0e 00 00 00 01 00 00 00 00
```

### event

`Event { device: 3, state: 0, health: 0, reason: "" }`

<!-- vector: event -->
```hex
0c 00 00 00 13 00 03 00 00 00 00 00 00 00 00 00
```

### call-job

`CallJob { id: 15, device: 3, contract: 0, method: 4, args: [aa] }`

<!-- vector: call-job -->
```hex
12 00 00 00 1b 00 0f 00 00 00 03 00 00 00 00 04 00 01 00 00 00 aa
```

### job-started

`JobStarted { id: 15, job: 0x100000002 }`

<!-- vector: job-started -->
```hex
0e 00 00 00 1c 00 0f 00 00 00 02 00 00 00 01 00 00 00
```

### job-progress

`JobProgress { job: 0x100000002, done: 5, total: 10 }`

<!-- vector: job-progress -->
```hex
1a 00 00 00 1d 00 02 00 00 00 01 00 00 00 05 00 00 00 00 00 00 00 0a 00 00 00 00 00 00 00
```

### job-done

`JobDone { job: 0x100000002, status: 0, bytes: [7] }`

<!-- vector: job-done -->
```hex
13 00 00 00 1e 00 02 00 00 00 01 00 00 00 00 00 00 00 01 00 00 00 07
```

### job-cancel

`JobCancel { id: 16, job: 0x100000002 }`

<!-- vector: job-cancel -->
```hex
0e 00 00 00 1f 00 10 00 00 00 02 00 00 00 01 00 00 00
```

### endpoints-open

`EndpointsOpen { id: 17, device: 3 }`

<!-- vector: endpoints-open -->
```hex
0a 00 00 00 20 00 11 00 00 00 03 00 00 00
```

### endpoints

`Endpoints { id: 17 }`, with two descriptors.

<!-- vector: endpoints -->
```hex
06 00 00 00 21 00 11 00 00 00
```

### endpoints-close

`EndpointsClose { id: 18, device: 3 }`

<!-- vector: endpoints-close -->
```hex
0a 00 00 00 22 00 12 00 00 00 03 00 00 00
```

### endpoints-open-end

`EndpointsOpenEnd { id: 17, device: 3 }`

<!-- vector: endpoints-open-end -->
```hex
0a 00 00 00 28 00 11 00 00 00 03 00 00 00
```

### endpoints-end

`EndpointsEnd { id: 17 }`, with three descriptors: OUT, IN, then END.

<!-- vector: endpoints-end -->
```hex
06 00 00 00 29 00 11 00 00 00
```

### refusal

`Refusal { id: 12, status: -2, text: "no" }`

<!-- vector: refusal -->
```hex
10 00 00 00 14 00 0c 00 00 00 fe ff ff ff 02 00 00 00 6e 6f
```
