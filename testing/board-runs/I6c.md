# I6c board run: the bridge's cancel, restart and unrelated-topic checks

Status: untested on board. The PC tests of `service/orion` run against a real Orion comm core and node
logic at Orion `455a956e` (the revision `service/orion/Cargo.toml` pins), in-process, with lemnosd on the
simulated board. Nothing here has run on a board.

What only a board can show, run under the board lock (`/run/lock/board-gpu.lock`) with the bridge built
for aarch64 on the NVMe or SSD. It replaces check 4 of `I6b.md` (`caller_gone` on a caller's stop): at this
revision a stopped caller's run is cancelled and the driver is not told the caller is gone.

1. Restart the comm core while values are moving. Pass: every resource, topic and action is said again,
   and a run asked as soon as `orion resource list lemnos.<device>` shows the resource Ready is answered,
   not refused as not found.
2. Facts and a device switched off and on (`lemnos-ctl off <device>`, then `on <device>`), with
   `orion topic watch lemnos.<device>.values` on an unrelated device. Pass: no gap, no `publisher gone`,
   and no sequence reset during either change.
3. A long action run from `orion action run` with a second client, cancelled by that client. Pass: the run
   ends `Cancelled`, the driver's job is cancelled, and the driver's holds for that caller are unchanged.

Timings on the CM5 are measured only in this run, and reported with the CM5 as the board.
