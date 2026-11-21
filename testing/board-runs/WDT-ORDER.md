# WDT-ORDER: lemnosd's unit ordering, the watchdog descriptor store, and the ping on open

Status: **untested on board.** Nothing here has run on a board. The QEMU checks (`testing/qemu/run-watchdog.sh`,
modes `daemon-loop` and `daemon-loop-bind`, with the stand-in manager) and the host's own systemd user manager
are the only evidence so far. No board was touched for this item.

## What the board check must show

On a slot board built with the image contract for `lemnosd.service` (`DefaultDependencies=no`,
`After=systemd-sysusers.service`, `FileDescriptorStoreMax=4`, no `RuntimeDirectoryPreserve=`):

1. The first boot from the flash: the gap from slot-init's last feed to lemnosd's root-phase feed is at most a third
   of the applied period (5 s at 15 s). The stamps are the start trace's (`LEMNOSD_START_TRACE=1` in
   `/etc/lemnos/lemnosd.env`) and the slot-init kernel log. The bound is `HANDOVER_GAP_DIVISOR` in the QEMU check.
   The systemd part before lemnosd's exec (switch_root to the exec) is measured here, not in QEMU.
2. A cold boot with first-boot sysusers and a `/data` journal replay: lemnosd starts, the store keeps one
   descriptor (`lemnosd: root phase: ...: kept in the service's descriptor store`), and the board stays up.
3. `systemctl kill -s SIGKILL lemnosd` while the board is healthy: the restart logs `adopted, not fed`, the board
   does not reset, and the watchdog is fed by the first healthy tick.
4. `systemctl restart lemnosd` (a stop then a start): the store is emptied, the start is a first start and logs
   `fed`. The board stays up.
5. Each Atlas check named in REMAINING.md for item 3: the image's `fstab` has no `fsck` pass for any mount lemnosd
   uses, and the image mounts lemnosd's paths before `switch_root` (the slot data partition).

## Record

Write the result under `## Result` with: the board, the image's `lemnosd.service` (the output of
`systemctl cat lemnosd.service`), the gap pieces from the start trace, the restart's log lines for checks 3 and 4,
and the `systemd-analyze verify` result for the image's units. Do not record a result that was not run.

## Result

Not run.
