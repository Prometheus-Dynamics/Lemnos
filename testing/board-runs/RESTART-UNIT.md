# RESTART-UNIT board run

Status: untested on board. Nothing here has touched a board: the restart is proven in QEMU guests under systemd
(PID 1 of each guest, on the lander's kernel), not on the reference board.

## What a board must show

1. The daemon's restart of a trial board, on the board's own image, goes through `lemnos-restart.socket` and
   `lemnos-restart.service` as the packaged units start them (the helper answers `check` as the daemon's user and
   `reboot` is accepted, then the board stops and reboots into the trial).
2. The same board's restart with the helper stopped (or not installed) is refused before the trial is armed, and
   the trial's state says why (`refused`), not `rebooting`.
3. The BusyBox image's restart (no systemd): not run. See "Not covered".

## What the QEMU guests showed (this PC, host load about 8 to 20, KVM)

Helper, the probe as the daemon's user `lemnos` (uid 900 in the guest), 20 checks each, with the guest's own bus:

| Run | Helper | No Ping (control) |
|---|---|---|
| no load | 20 of 20 strict (`restart-probe: check: ok`) | 1 of 20 |
| two competitors pinned to one vCPU (CPU 1), 90 s | 20 of 20 strict | 0 of 20 |

The control is the same helper built without the Ping handshake (the handshake's one call removed in a scratch
copy). The new helper is the one with the Ping.

Daemon cases (`testing/qemu/run-lemnosd-restart.sh`, the packaged `lemnosd.service` and `lemnos-restart.*`,
a release daemon with `testing-boot-roots`, the trial's slot installed by the daemon's socket):

- d: a peer in group lemnos that is uid 901 is refused by its uid; a request that is not `<verb> <nonce>` is refused.
- c: `StartUnit` fails on the helper's connection (`TransactionOrderIsCyclic`); the helper answers `accepted`, then
  `failed`; the trial is disarmed; the state is `restart-failed` with the reason; the caller gets IO.
- a: the trial is accepted and the reboot begins: the daemon's own answer is cut off by its stop (SIGTERM), the slow
  unit stops for 5 s, QEMU exits 0 through the reboot, and the boot partition verifies. The next boot's state still
  has the trial armed (`slot_trial` install-1, state `rebooting`).

Fail-before on origin/next `52ed5c2d` (the daemon with `systemctl reboot` as the restart, no helper): c's state never becomes
`restart-failed`; a's trial is refused with IO, and no reboot starts.

## Not covered

- BusyBox (condition 3): no BusyBox guest was built. A static BusyBox 1.38.0 exists in the lander's scratch
  (`g/tmp/busybox/1.38.0/busybox-1.38.0/busybox`); it has not been booted with the helper's `--init` mode.
- Case b (accepted, then no stop within the bound): not reproducible on systemd. A reboot job starts the shutdown at
  once, so the helper sees `stopping` and the bound never expires. Holding the stop with a unit ordered before
  `reboot.target` or `systemd-reboot.service` did not hold it. The bound is covered only by the helper's unit tests
  with a stand-in manager.
- Install size: a 9.6 MB bundle's last 64 KiB write returned TIMED_OUT in this guest (the payload's commit is one
  driver tick past the call's deadline, the open item in SLOT-INSTALL-INT). The cases install a 1 MB stand-in.
- The boot count mailbox is absent in the QEMU boot chain (the state's error says the boot count can't be cleared).
  The firmware's trial boot is not run.

## Checks to run on the board

- `systemctl --user` is not used; the board's services are the packaged ones. Run `lemnos-restart-probe check` as
  `lemnos` on the board: `restart-probe: check: ok`, 20 of 20 with two competitors on the same core.
- Run a trial on the board and read `info` for `rebooting`, then after the reboot `list` for the trial slot.
- Run the same trial with `lemnos-restart.service` stopped: the trial must be refused before it is armed.
