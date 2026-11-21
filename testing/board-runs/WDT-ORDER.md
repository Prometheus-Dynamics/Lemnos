# WDT-ORDER: lemnosd's unit ordering, the watchdog descriptor store, and the ping on open

Status: **untested on board.** Nothing here has run on a board. The QEMU checks (`testing/qemu/run-watchdog.sh`,
modes `daemon-loop` and `daemon-loop-bind`, with the stand-in manager) and the host's own systemd user manager
are the only evidence so far. No board was touched for this item.

## 0. The namespace still applies under `DefaultDependencies=no` (host, not a board)

Run on the host's user manager (systemd 259), not a board and not the system manager, with a transient unit:
`systemd-run --user --wait --collect -p DefaultDependencies=no -p ProtectSystem=strict -p PrivateTmp=yes
-p ReadWritePaths=/run/user/1000/wdt-ns <probe>`. The probe writes under `/usr`, reads `/tmp` for a marker the
host wrote, and writes into `/tmp`. Its output, verbatim:

```
inside: whoami=sozo pid=1934760
/run/user/1000/wdt-ns/probe.sh: line 5: /usr/wdt-probe-write: Read-only file system
inside: write under /usr: refused
inside: /tmp sees the host marker: NO (private)
inside: wrote /tmp/wdt-unit-tmp-probe.txt (private /tmp)
```

Afterwards the host has neither `/usr/wdt-probe-write` nor `/tmp/wdt-unit-tmp-probe.txt`. So the namespace applies
(`/usr` is read-only, `/tmp` is private) under `DefaultDependencies=no`. The analyzer's score does not show it, which
is why `scripts/check-packaging.sh` scores that unit without the line. This is evidence for the user manager only;
the system manager on a board is still to be checked with the board run below.

## 1. Checks for the board


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
