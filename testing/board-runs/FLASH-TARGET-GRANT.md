# FLASH-TARGET-GRANT: the first-flash target through the root phase's hand-over

Status: **untested on board.** The guest proof below ran on the PC in an arm64 QEMU guest. The board's flasher image
(its daemon dropped to its own user, the eMMC's node `root:root`) failed the stream's begin with `-19` ("the device
did not start the write") before this change; that run is not repeated here.

## What is built

- `drivers/system-flash/src/lib.rs` `root_phase`: run as root before the drop (`ROOT_PHASES` in
  `service/lemnosd/src/privilege.rs`). The target as the settings name it (`path`, or the one whole disk under
  `controller` then) is opened read-write and handed over. Not handed over, each with its reason on the root phase's
  line: a regular-file target, a target the running system is mounted from, and one that isn't there yet.
- `drivers/system-flash/src/target.rs`: the held file (`hand_over`); `find` and `open` take a copy of it, and an
  open by name refused by the node's owner says the root phase did not hand the target over. Every read and write
  is positional, so the copies' shared file position is never used.
- `packaging/systemd/grants.toml`: the first-flash target's row, `how = "root-phase"`.
- `testing/crates/flasher-check/src/bin/lemnos-flasher-guest.rs`: the daemon runs as uid and gid 990 (checked from
  `/proc/<pid>/status`), the eMMC's node is checked root's with no world bits, every chunk's answer is timed, and the
  partition re-read's refusal (`BLKRRPART` needs `CAP_SYS_ADMIN`, which the daemon does not keep) is checked by name
  in `info`; the kernel keeps the blank disk's table until the restart.

## Proof

Kernel 7.3.5 arm64 (`testing/flasher/build-kernel.sh`), `lt/slot-run.sh 1 testing/flasher/run.sh`.

- Before (origin/next a6251ecf with only the new guest, daemon as 990): `the bridge's stream began: Rejected the
  device did not start the write: refused (-19): the job could not be started`, `flasher: failed (2 of 8 checks)`:
  the board's failure.
- After, with the guest's `[host] call_deadline_ms = 60000` for the run only (not committed):
  `lemnosd: root phase: flash: /dev/mmcblk0: handed over read-write`, `Uid 990 990 990 990 Gid 990 990 990 990`,
  committed `raw_bytes=8388608 outcome=1`, info `the image is written, but the kernel did not re-read its partition
  table (Permission denied (os error 13))`, verify `raw_bytes=8388608`, the user area's SHA-256 the image's, both boot
  areas byte-identical, no `mmcblk0p1`: `flasher: passed (13 checks)`; host side `emmc: boot areas unchanged (2097152
  bytes); user area holds the image (8388608 bytes); the rest unchanged`.
- After, at the default 2000 ms deadline: the stream begins and is written through the held node, then one chunk's
  answer runs past the deadline (2514 ms at 5111808 of a stream whose chunks otherwise take 80 to 290 ms), the job
  ends interrupted, `flasher: failed (2 of 9 checks)`. The same stall happens with the daemon as root on
  origin/next (a chunk past the deadline at 6422528), so it is not this change's: it is the flasher's next fault,
  a chunk's write held in the driver's call (FLASH-BRIDGE-PUMP).

## On the board

With the flasher image on this tree: the daemon's log has `lemnosd: root phase: flash: /dev/mmcblk0: handed over
read-write` (or the eMMC's node), the stream's begin answers `Receiving`, and `info`'s `error` after a whole write
names the partition re-read the daemon's user can't do; the restart that follows boots the new table.
