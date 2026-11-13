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
| 18 | StreamReady | id u32, buffers u32, descriptors u8, buffer_bytes u32, frame_slots u32, job_slots u32, job_bytes u32 | to client |
| 19 | Event | device u32, state u8, health u8, reason str | to client |
| 20 | Refusal | id u32, status i32, text str | to client |
| 21 | StreamClose | id u32, device u32, name str | to host |
| 22 | StreamOpenHandles | id u32, device u32, name str | to host |
| 23 | StreamHandles | id u32, buffers u32, descriptors u8, buffer_bytes u32, frame_slots u32, job_slots u32, job_bytes u32, plane_bytes bytes | to client |
| 24 | Packages | id u32 | to host |
| 25 | Package | id u32, path str, driver str, reason str | to client |
| 26 | PackagesDone | id u32, mode u8, note str | to client |
| 27 | CallJob | id u32, device u32, contract u8, method u16, args bytes | to host |
| 28 | JobStarted | id u32, job u64 | to client |
| 29 | JobProgress | job u64, done u64, total u64 | to client |
| 30 | JobDone | job u64, status i32, bytes | to client |
| 31 | JobCancel | id u32, job u64 | to host |

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
| 0 | 1 | takes StreamOpenHandles, and may answer it with StreamHandles |
| 1 | 2 | takes Packages |
| 2 | 4 | takes CallJob and JobCancel |

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

`core hash: 0xa2b3f48271c1f428`

`stream-handles part hash: 0x0f65b93219892e68`

`packages part hash: 0xb3db2c33449c615d`

`jobs part hash: 0xe8ed57f23fc18516`

The hashes do not include the Lemnos version string. Two builds with the same core interoperate
whatever their Lemnos versions. What a client sees when the cores differ: the connection is refused
at connect with `LayoutMismatch`, whose text is `core protocol differs`, with both core hashes and
the host's Lemnos version. When a part differs: the connection is refused with `PartMismatch`, whose
text names the part and both hashes. The C API returns `VERSION_MISMATCH` with the same text. A
client that connects to a host from before this prefix existed sees a refusal from the host, not a
core error, because that host reads the first bytes of Hello as its version and refuses them.

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

StreamReady says how many file descriptors arrive with it, as `SCM_RIGHTS` on the same `sendmsg`
that carries the frame's first byte. Three, in order: the ring, the doorbell, the buffers. A
client reads the socket with `recvmsg` while a StreamOpen is outstanding, because a plain read
discards descriptors. Descriptors passed to the host are closed unread.

StreamHandles carries its descriptors the same way, in one message: the ring, the doorbell, then
one descriptor per plane of each buffer, buffer by buffer and each buffer's planes in order.
`plane_bytes` is each plane's length as a little-endian u32, at most 8 of them, adding up to
`buffer_bytes`. It is empty for a buffer of one plane, the usual case. `descriptors` is
`2 + buffers × planes`, and `buffers × planes` is at most 128, so at most 130 descriptors arrive.
A client that sends StreamOpenHandles gives its `recvmsg` room for 130 descriptors: what doesn't
fit is lost, and the stream with it. A client refuses a StreamHandles whose `buffers` is 0, whose
`plane_bytes` isn't whole lengths, more than 8 of them, a zero length, or a sum other than
`buffer_bytes`, whose `buffers × planes` is over 128, whose `descriptors` isn't
`2 + buffers × planes`, or that came with a different number of descriptors, and closes the
connection, as for any frame that can't be trusted.

## Streams

StreamOpen takes one stream of one device for this connection. `name` is the stream's name under
that device in the board file (`[devices.streams.<name>]`). A stream has one consumer: a second
StreamOpen gets Refusal with `BUSY`; an unknown device or name gets `NOT_FOUND`. StreamClose gives
it up; a connection closing gives up every stream it held. After StreamReady nothing about a frame
crosses the socket.

| Descriptor | What it is |
|---|---|
| ring | a shared mapping of `ring_bytes`, read and written by both sides |
| doorbell | an eventfd. The host adds 1 after each frame. Waiting on it is optional |
| buffers | `buffers` buffers, one every `stride` bytes from offset 0. Map it read-only, or writable for a stream the consumer fills (below) |

### Streams whose buffers are dma-bufs

A board file can say a stream's buffers are dma-bufs (`memory = "dma-buf"`), one per buffer, so
that a driver can hand a buffer to hardware. Such a stream can't be answered with StreamReady,
which carries one descriptor for all the buffers.

- A client that can take one descriptor per buffer sends StreamOpenHandles in place of StreamOpen,
  and only to a host that set bit 0 of `features`. It means the same as StreamOpen, and the answer
  is StreamReady for a stream of shared memory and StreamHandles for a stream of dma-bufs.
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

`stride` is `buffer_bytes` rounded up to a multiple of 4096. The ring is 64-bit words, little-endian
on every platform this is built for:

| Word | Name | Written by |
|---|---|---|
| 0 | magic `LMNSRING` | host, once |
| 1 | layout version, 1 | host, once |
| 2 | buffers | host, once |
| 3 | buffer_bytes | host, once |
| 4 | frame_slots | host, once |
| 5 | job_slots | host, once |
| 6 | job_bytes | host, once |
| 8 | state: 0 open, 1 closed, 2 faulted | host |
| 16 | frames head | host |
| 24 | frames tail | consumer |
| 32 | returns head | consumer |
| 40 | returns tail | host |
| 48 | jobs head | consumer |
| 56 | jobs tail | host |
| 64 | frame slots, 5 words each, `frame_slots` of them | host |
| after | return slots, 1 word each, `frame_slots` of them | consumer |
| after | job slots, `1 + ceil(job_bytes / 8)` words each, `job_slots` of them | consumer |

`ring_bytes` is 8 times the words above, and a client checks the descriptor is exactly that long
before mapping it. Every head and tail counts up without wrapping; a slot is the count modulo the
queue's capacity, which is a power of two. A writer writes the slot, then stores the head with
release ordering. A reader loads the head with acquire ordering, then the slot.

A frame slot's words are: buffer in the low 32 bits with the length in the high 32, sequence,
identity, timestamp in `CLOCK_MONOTONIC` ns (on Linux; see [Frames](streams.md#frames)), fence.
Identity is the producer's and is never read. A return slot is
the buffer in the low 32 bits and the frame's fence in the high 32. A job slot is its length, then
its bytes, little-endian, the last word zero-padded.

### What each side checks

The peer is not trusted. Every value below is read once from the shared memory and checked against
limits that side already holds.

- A head more than the queue's capacity ahead of its tail is an error. A tail behind the last one
  seen or ahead of its head is an error. An unknown state word reads as faulted.
- A frame's buffer must be below `buffers`, its length at most `buffer_bytes`, and its buffer must
  not already be held.
- A returned buffer must be below `buffers`, published and not yet returned, with the fence of
  its frame. A job must be at most `job_bytes`.

A consumer that breaks these loses the stream: the host marks it faulted (state 2) and rings the
doorbell. Nothing else is affected: not the connection, not other streams. A fresh StreamOpen starts
over on new memory.

The ring carries a job's bytes without reading them, so whether they are a job is for the driver to
say. A driver whose contract calls bytes that are no job a broken consumer ends the stream the same
way (`StreamEndpoint::fault_consumer`): state 2, the doorbell, and the jobs still queued are gone.
A job that decodes but can't be run is not that: it completes as failed, or is counted as dropped,
as its contract says, and the stream stays open.

## Description

The `bytes` of a Description, in order: a `u16` contract count, then for each contract a
contract record (its tag str, its encoding hash u64, see Contract hashes below, and a `u16` method
count), then for each method a method record (its id u16, name str, a `u16` parameter count, and a
has-out-buffer bool), then for each parameter a parameter record (its name str) followed by its
shape. A method's returned shape follows its parameters. Then a `u16` item count, and for each item
an item record: its kind u8 (0 value, 1 control, 2 action), name str, unit str, exponent i8, min and
max (each a presence bool, then an i64 when present), contract u8, method u16, and field (a presence
bool, then a str when present). Then, appended after the first form, a `u16` count of job methods
and for each a job record: its contract u8 and method u16. Those are started with CallJob. A host
from before job methods ends at the items, and a reader takes that as no job methods. A job method
that names no method above makes the description malformed.

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

### Contract hashes

Each contract in a Description carries its encoding hash: a 64-bit FNV-1a over the tag, every
method's number and name, each parameter's name and shape, the result's shape, and whether it fills
a buffer or starts a job. The shapes are the ones the argument and result types declare, so the
hash moves when a method is added, renamed or renumbered, or when an argument or result struct
gains, loses, renames or retypes a field. It is computed, never written by hand: the `#[contract]`
macro and the `Encode` derive generate it.

A typed client checks it when it opens a contract (`Client::open`), reading the device's
description once. A device whose hash differs is refused with `ContractMismatch`, which names the
contract and both hashes; nothing is called on it. Calls after the open are not checked again. A
device that doesn't list the contract's tag is not refused by the check (the open itself already
found the tag).

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

`Hello { core: 0xa2b3f48271c1f428, lemnos: "1.0.0", version: 1, name: "tool", events: true, parts: the three part hashes }`

<!-- vector: hello -->
```hex
51 00 00 00 01 00 28 f4 c1 71 82 f4 b3 a2 31 2e 30 2e 30 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 01 00 04 00 00 00 74 6f 6f 6c 01 18 00 00 00 68 2e 89 19 32 b9 65 0f 5d 61 9c 44 33 2c db b3 16 85 c1 3f f2 57 ed e8
```

### welcome

`Welcome { core: 0xa2b3f48271c1f428, lemnos: "1.0.0", version: 1, features: 0, host: "h", parts: the three part hashes }`

<!-- vector: welcome -->
```hex
55 00 00 00 02 00 28 f4 c1 71 82 f4 b3 a2 31 2e 30 2e 30 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 01 00 00 00 00 00 00 00 00 00 01 00 00 00 68 18 00 00 00 68 2e 89 19 32 b9 65 0f 5d 61 9c 44 33 2c db b3 16 85 c1 3f f2 57 ed e8
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

`StreamReady { id: 11, buffers: 4, descriptors: 3, buffer_bytes: 256, frame_slots: 4, job_slots: 2, job_bytes: 8 }`

<!-- vector: stream-ready -->
```hex
1b 00 00 00 12 00 0b 00 00 00 04 00 00 00 03 00 01 00 00 04 00 00 00 02 00 00 00 08 00 00 00
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

`StreamHandles { id: 13, buffers: 4, descriptors: 6, buffer_bytes: 256, frame_slots: 4, job_slots: 2, job_bytes: 8, plane_bytes: [] }`

<!-- vector: stream-handles -->
```hex
1f 00 00 00 17 00 0d 00 00 00 04 00 00 00 06 00 01 00 00 04 00 00 00 02 00 00 00 08 00 00 00 00 00 00 00
```

### stream-handles-planes

`StreamHandles { id: 14, buffers: 2, descriptors: 6, buffer_bytes: 256, frame_slots: 2, job_slots: 1, job_bytes: 0, plane_bytes: [192 as u32 LE, 64 as u32 LE] }`

<!-- vector: stream-handles-planes -->
```hex
27 00 00 00 17 00 0e 00 00 00 02 00 00 00 06 00 01 00 00 02 00 00 00 01 00 00 00 00 00 00 00 08 00 00 00 c0 00 00 00 40 00 00 00
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

### refusal

`Refusal { id: 12, status: -2, text: "no" }`

<!-- vector: refusal -->
```hex
10 00 00 00 14 00 0c 00 00 00 fe ff ff ff 02 00 00 00 6e 6f
```
