# FLASH-DATA-OFF-CALL: the first-flash write runs off the call

Status: **untested on board.** The proof below ran on the PC in an arm64 QEMU guest (`testing/flasher/run.sh`)
with the real daemon as its own user, and in the driver's tests.

## The fault

`system-flash`'s `data` decoded its piece and wrote it to the target inside the call. A target's write can stall
(writeback, an eMMC's own housekeeping), and a call has the host's deadline (2000 ms by default): a stall past it
ended the job interrupted (`-110`), and the stream broke. The last piece also ran the image's `fsync` and checks
inside its call.

## What is built

- `drivers/system-flash/src/lib.rs`: `write` starts the writer, a thread that owns the decoder, the target and the
  hash. `data` copies its piece into one of four reused buffers and queues it, or answers `BUSY` at once when all
  four are with the writer. The last piece queues the end; the image's `fsync`, hash and record checks run on the
  writer, and the job's end is read at the host's tick. The writer's failure ends the job at the next `data` or at
  the job's end, with `the write failed at byte <n> of the image: <reason>; the target holds a partial image`. A
  cancel stops the writer after the piece it is on and drops the queued ones; a new `write` is refused `BUSY` until
  the writer has ended. `info` adds `behind` (the `BUSY` answers) and `longest_data_us` (the longest `data` call,
  measured inside the driver).
- `service/orion/src/stream.rs`: the bridge offers a chunk again while the device answers `BUSY` (2 ms doubling to
  200 ms). A failed job carries no result, so a rejected commit (or a chunk refused after the write failed)
  carries the device's `info` `error` after the status: the sender sees the reason and the byte.
- `testing/flasher/run.sh`: `WRITE_BPS` throttles the guest eMMC's writes (QEMU `throttling.bps-write`).
- `testing/crates/flasher-check`: the guest prints the longest `data` call and checks it under 100 ms.

## Proof

Kernel 7.3.5 arm64 (`testing/flasher/build-kernel.sh`), `lt/slot-run.sh 1 testing/flasher/run.sh`, the default
2000 ms call deadline, the daemon as uid and gid 990.

- Before (origin/next c0ee5f04), `WRITE_BPS=1048576`: `a call ran past its 2000 ms deadline`, the job `ended
  interrupted`, `chunk at 786432: Rejected the device refused the data: refused (-110)`, `flasher: failed (2 of 9
  checks)`. Unthrottled, the same tree failed once with `-110` (a chunk of 2514 ms at 5111808) and passed once (a
  chunk answer of 2143 ms): there it is a race with writeback, and the throttled run is the one that tells.
- After, `WRITE_BPS=1048576`: `the longest data call took 8672 us inside the driver; 198 answered BUSY`, committed
  `raw_bytes=8388608 outcome=1`, verified, the image read back, the boot areas unchanged: `flasher: passed (14
  checks)`.
  Again on the final tree (with the bridge's reason in a rejected answer): `the longest data call took 6460 us
  inside the driver; 244 answered BUSY`, `flasher: passed (14 checks)`. Its longest chunk answer was 6315 ms: on
  a store this slow the bridge's answer to the sender waits that long while no call does, which is past
  Orion's default reply bound (5 s). A sender waits with `Reply::Alive` (`docs/slots.md`, "What a sender must
  do"), as the guest's sender does.
- After, unthrottled: `the longest data call took 23285 us inside the driver; 223 answered BUSY`, `flasher: passed
  (14 checks)`. The longest chunk answer, 1731 ms, is the bridge's answer to the sender and includes its waits
  after `BUSY`; no call took it.
- Driver tests (`cargo test -p lemnos-driver-system-flash`):
  `a_writer_that_fails_part_way_ends_the_job_naming_the_byte_and_nothing_reports_success` damages a four-block xz
  image three quarters in. The job's status is not OK and gives no `raw_bytes`; `info` reads `failed` and names
  the byte (inside the image, past 0) and the partial image; `verify` is refused; the target holds the image's
  bytes up to that byte and none after it. The other write tests take `BUSY` and the job's end at a tick.
- Bridge tests (`cargo test -p lemnos-orion --test flash_stream`, 18 pass):
  `an_image_damaged_mid_stream_is_rejected_naming_the_byte_the_write_failed_at` (the push is `rejected` with
  `the write failed at byte ` and `the target holds a partial image`, no `raw_bytes`); the hash and record tests
  now read the device's reason in full, e.g. `the write ended with status -22: the image's SHA-256 is not the
  manifest's`.
- `testing/qemu/run.sh` (the lander's kernel): `lemnos-qemu: passed (0 failed)`.

## On the board

With the flasher image on this tree, flash the eMMC at the default deadline: the stream commits, and `info` after
the write gives `longest_data_us` well under the deadline (the guest's bound is 100 ms) and `behind` counts the
times the eMMC was behind.
