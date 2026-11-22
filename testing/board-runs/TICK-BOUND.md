# TICK-BOUND board run

Status: untested on board. Nothing here has touched a board.

The defect: an install's finish put a whole boot file into the store in one tick (hash, copy and fsync of the
kernel, about 7.5 MB), and the U-Boot form wrote all of a slot's payloads into its bay in one call. On a slow card,
or under TCG, one tick ran past the host's 2000 ms call deadline, and the host ended the job interrupted.

The fix: every install tick has a time budget, a quarter of the call deadline (500 ms by default), checked after
each 64 KiB piece (`drivers/system-slots/src/budget.rs`). The boot-file store's put, the payload write into a bay
(both forms) and the read of a bundle from its file go by pieces; the fsync runs on the flush worker. The host gives
every driver its call deadline (`Config::call_deadline_ms`). The slow-period boot-file look (`boot_guard`), which
hashed the default's boot files on the tick (3.4 s on a 2 MB/s disk), runs on a thread of its own; a tick only
takes its answer.

Proven: unit tests with a clock on which each piece takes 100 ms (fail with the time check out); QEMU
(`testing/qemu/run-slot-install.sh`, kvm, the packaged unit, boot files that differ, the default deadline) with the
guest's disk writes held by QEMU's `throttling.bps-write`:

| writes held to | daemon | longest install tick | result |
|---|---|---|---|
| 4 MB/s | before (next 5305d2d4) | 1912 ms | FAIL (`MAX_TICK_US=1000000`) |
| 4 MB/s | after | 581 ms, 621 ms | PASS, PASS |
| 2 MB/s | before | past 2000 ms | FAIL: job interrupted by a tick |
| 2 MB/s | after | 564 ms | PASS once; twice a stream `write` call blocked over 2 s in ext4 |

The last row is not this change's: a call that writes can wait in the filesystem (see docs/slots.md, "Not bounded
yet: a call that writes").

## Checks to run on the board

1. On the slowest card at hand, install a bundle whose boot files differ from the stored ones (another kernel)
   with the daemon's start trace on: the longest `SLOT-TICK device slots` in the job stays under 1 s, and no
   `ran past its 2000 ms deadline` line.
2. The same on the K230D and the ZERO 3E (U-Boot form): the payloads go into the bay over several ticks.
