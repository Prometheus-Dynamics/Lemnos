# WDT-ORDER: lemnosd's unit ordering, the watchdog descriptor store, and the ping on open

Status: **untested on board.** Nothing here has run on a board. The QEMU checks (`testing/qemu/run-watchdog.sh`,
modes `daemon-loop` and `daemon-loop-bind`, with the stand-in manager) are the only evidence so far.
No board was touched for this item.

## 0. Namespace under `DefaultDependencies=no`: QEMU guest, systemd 259.10 as PID 1

Run: a QEMU guest booted with the host kernel (`/boot/vmlinuz-7.3.4-201.nobara.fc44.x86_64`) and the host's systemd
as PID 1, in a private copy of the RST guest's initramfs (`testing/qemu/systemd-restart/` pattern, the RST worktree
not edited). The probe unit is `DefaultDependencies=no`, `ProtectSystem=strict`, `PrivateTmp=yes`,
`ProtectHome=yes`, and writes its results to `/dev/console` (`StandardOutput=tty`, `TTYPath=/dev/console`). A
`+`-prefixed pre-step, which runs outside the unit's sandbox, writes `/tmp/wdt-outside-marker`. Output, verbatim
from the clean run (ANSI codes removed):

```
WDT-GUEST NS unit-pid=90 user=root
WDT-GUEST NS write-under-usr=refused
WDT-GUEST NS tmp-sees-outside-marker=NO
WDT-GUEST NS tmp-write=ok
WDT-GUEST NS write-under-var=refused
WDT-GUEST SYSTEMD systemd 259 (259.10-1.fc44)
WDT-GUEST PROPS DefaultDependencies=no
WDT-GUEST PROPS PrivateTmp=yes
WDT-GUEST PROPS ProtectHome=yes
WDT-GUEST PROPS ProtectSystem=strict
WDT-GUEST DONE
WDT-GUEST OUTSIDE-MARKER=present
WDT-GUEST POWEROFF
```

What it shows: under `DefaultDependencies=no`, a write under `/usr` and under `/var` is refused (the sandbox's
read-only root), `/tmp` is private to the unit (the outside marker is not seen inside, and a write inside succeeds),
and the marker is present outside. So systemd applies the namespacing to this unit. The analyzer's score does not
credit it (`scripts/check-packaging.sh` scores around that). Caveat: a `wdt-ns-finish.service` status line
(`status=5/NOTINSTALLED`) appears after the poweroff line in the console; the poweroff happened, and that status
belongs to the probe's own finish helper, not to the claim above.

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
