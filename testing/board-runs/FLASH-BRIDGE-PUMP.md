# FLASH-BRIDGE-PUMP: the bridge's pump never waits for the device; a broken push resumes

Status: **untested on board.** The proof below ran on the PC: a real Orion node, the real lemnosd with the
flash driver, and the bridge's threads, with Unix-socket proxies standing between them.

## The fault

The board's flasher lost the PC's push in three ways (Atlas's emulated runs, client side; the board kept no
logs). The bridge's files thread, the one that pumps its Orion session, also made the stream's lemnosd calls
and waited up to `JOB_WAIT` for the job's end, so a slow call or a long commit left the session unpumped and
the device's other files unanswered. The bridge built its stream state with each Orion session, so a broken
session lost the stream while its job stayed open. And a resumed begin of the same transfer started a new
stream at 0, which a sender resuming within its push reads as "a transfer that went backwards". The PC's own
reply bounds (Orion's `Reply::Timeout`, 5 s, and twelve of them for a commit) are the sender's to lift with
`Reply::Alive`.

## What is built

- `service/orion/src/stream.rs`: `StreamWorker`, a stream's lemnosd work on its own thread, its answers sent by
  the pump in order; chunks in flight bounded (64). `Streamer::begin` of the open stream answers the bytes
  taken; a chunk the device already has is answered with where the stream is; the last commit's answer is
  kept and answers a begin or commit of it again (`committed` kept, `rejected` told once). While the worker
  waits on a commit, the pump answers that transfer's begin with every byte taken.
- `service/orion/src/files.rs`: one worker per device for the bridge's life, across Orion sessions.
- `service/orion/tests/support/proxy.rs`: the proxy (hold one reply on the connection whose client sent given
  bytes; cut every connection).

## Proof

`cargo test -p lemnos-orion --test flash_stream`, four new tests. After: all pass, and the whole
`lemnos-orion` suite passes. Before (origin/next d9bea297 with only the tests and the proxy copied in), each
fails:

- `a_device_call_held_past_the_senders_bound_does_not_stop_the_bridge_answering_other_files`: the stream's
  first lemnosd reply held 4 s. Before: `the plain file was answered only after the held call was let go
  (3.039639388s)`.
- `a_stream_whose_bridge_session_breaks_resumes_at_the_bytes_taken_and_commits`: the bridge's node connection
  cut at a quarter of a 1 MiB image. Before: `the second push did not resume at the bytes taken (491520)`, its
  progress from 0.
- `a_stream_whose_sender_session_breaks_resumes_at_the_bytes_taken_and_commits`: the sender's connection cut;
  then, as a flasher's client does on every attempt, the same manifest pushed again (committed, and the open
  stream left alone: the device is still busy with it), then the image. Before: `the second push did not resume at the bytes taken (262144)`, from 0.
- `a_committed_image_pushed_again_is_answered_with_its_outcome_and_not_sent_again`: before, `chunks were sent
  again: [0, 32768, ... 262144]`.

## On the board

With the flasher image on this tree and a client that sets `Reply::Alive` (ceiling at least `JOB_WAIT`,
900 s): a push whose PC link drops mid-image, pushed again, resumes (its progress does not start at 0) and
commits; a large image's commit is answered after its job ends however long the final sync takes; the client
sees no `-19` on a push begun again. The chunk writes themselves still run in the driver's call, under the
2 s call deadline: FLASH-DATA-OFF-CALL is the next fault.
