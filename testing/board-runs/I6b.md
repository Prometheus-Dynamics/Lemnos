# I6b board run: the bridge on Orion 6dfb990, and what a board must show

Status: untested on board. The PC tests of `service/orion` run against a real Orion comm core and node
logic at Orion `6dfb990` (the revision `service/orion/Cargo.toml` pins), in-process, with lemnosd on the
simulated board. Nothing here has run on a board.

What only a board can show, run under the board lock (`/run/lock/board-gpu.lock`) with the bridge built
for aarch64 on the NVMe or SSD:

1. Start the bridge against the board's Orion comm core and check `orion resource list lemnos.` against
   `lemnos-ctl list`, as in `B20.md`. Pass: the same devices, one resource each.
2. Time. Read one value with `orion topic read lemnos.<device>.values` and read the board's clock with
   `orion time`. Pass: each sample's time is on `CLOCK_MONOTONIC` (it is no larger than the monotonic
   clock now, and far below the wall clock), and `monotonic_to_wall_ns` turns it into a wall time within
   the time sync's error.
3. Facts. Boot a slot, then change the boot marker, and remove the marker. Pass: each `hello.*` fact that
   comes is declared once, one that goes is withdrawn once, and `orion topic watch lemnos.<device>.values`
   on an unrelated device shows no gap, no `publisher gone`, and no sequence reset during either change.
4. Callers. Run a long action of a driver that tracks callers from `orion action run` with a second
   Orion client, and stop that client during the run. Pass: the driver's `caller_gone` runs once for that
   caller, and its connection to lemnosd is closed (the driver's holdings for that caller go).
5. Idle. Start the comm core with `comm.idle_ms` at 2 s, leave the bridge running for at least 10 s with
   nothing changing. Pass: the resource stays `Ready`, and no reconnect is logged. Stop the comm core and
   check that the bridge logs a loss and connects again when it is back within the idle time and then
   some.
6. Restart. Restart the comm core while values are moving. Pass: every resource, topic and action is said
   again, and the first run after the return is answered (a run in the restore window may be refused as
   not found: that is Orion's, and it is retried by the caller).

Timings on the CM5 are measured only in this run, and reported with the CM5 as the board.
