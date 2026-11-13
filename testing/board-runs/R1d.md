# R1d: the gadget's unit hardening, to try on a board

Status: untested on board. The QEMU harness (`testing/crates/gadget-check`) runs the daemon's own
privileged start in a guest, checks the capability sets after the drop, and binds the gadget on
dummy hardware. It does not run systemd, so the unit's sandbox is not exercised by it.

The packaging check (`scripts/check-packaging.sh`) holds the base `lemnosd.service` at 2.5, and the
gadget fragment `packaging/systemd/lemnosd.service.d/gadget.conf` at the base plus the two listed
additions (`CAP_SYS_ADMIN` 0.3, `mount`/`umount2` 0.2). Three more hardenings were considered and
are NOT in the unit. They are checks to run on a board before anyone adds them:

1. **`DeviceAllow=char-* rwm` and `DeviceAllow=block-* rwm`** (device ACL, the `DeviceAllow=` row).
   Offline, with `systemd-analyze security --offline=true`, the overall score did not move (2.9 with
   and without it), so the row's scoring is not what the candidate would buy. Check on a board:
   which device nodes the daemon opens (`/dev/lemnos`, `/dev/usb-ffs/*`, `/dev/bus/usb/*`, the dma
   heaps, the media, I2C, SPI and GPIO nodes) are all `char-` or `block-` classes, that a gadget and a
   block driver still bind with the ACL in place, and what `systemd-analyze security` reports after.

2. **`SystemCallFilter=~@resources`** (the `~@resources` row, 0.2). Offline it lowers the score by
   0.1 (2.9 to 2.8). On a board: run every driver the board file names with the filter and check
   for `EPERM` from `prlimit64`, `setrlimit`, `ioprio_set` or the other `@resources` calls. The daemon
   sets none of them itself; a driver built into the daemon might.

3. **`UMask=0077`** (the `UMask=` row). Offline the row stayed (2.9 with `UMask=0077`). The daemon's
   socket and its runtime directory are the files this changes: `/run/lemnos` is `0750` and the socket
   must be connectable by group `lemnos` clients. Check on a board that the socket's mode is set
   explicitly rather than by the umask, before this is tried.

Each candidate is an addition to the unit, so the check would need its own score in the list if one
were kept: none is in `scripts/check-packaging.sh` now.

What only a board can show for the drop itself (the QEMU check shows it on dummy hardware):

- that `CapBnd`, `CapPrm`, `CapEff`, `CapInh` and `CapAmb` of the running daemon hold no
  `CAP_SYS_ADMIN` and only `CAP_SYS_RAWIO` in `CapBnd`, after the gadget's mount;
- that a driver's `mount(2)` on the FunctionFS mount point fails with `EPERM` in the running daemon.
