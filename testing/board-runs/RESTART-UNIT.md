# RESTART-UNIT board run

Status: untested on board. Nothing here has touched a board: the restart is proven in QEMU guests under systemd
(PID 1 of each guest, on the lander's kernel), not on the reference board.

## What a board must show

1. The daemon's restart of a trial board, on the board's own image, goes through `lemnos-restart.socket` and
   `lemnos-restart.service` as the packaged units start them (the helper answers `check` as the daemon's user and
   `reboot` is accepted, then the board stops and reboots into the trial).
2. The same board's restart with the helper stopped (or not installed) is refused before the trial is armed, and
   the trial's state says why (`refused`), not `rebooting`.
3. The BusyBox image's restart (no systemd): run in a QEMU guest with BusyBox init as PID 1 (see below). Not on the board.

## What the QEMU guests showed (this PC, host load about 8 to 20, KVM)

Helper, the probe as the daemon's user `lemnos` (uid 900 in the guest), 20 checks each, with the guest's own bus:

| Run | Helper | No Ping (control) |
|---|---|---|
| no load | 20 of 20 strict (`restart-probe: check: ok`) | 1 of 20 |
| two competitors pinned to one vCPU (CPU 1), 90 s | 20 of 20 strict | 0 of 20 |

The control is the same helper built without the Ping handshake (the handshake's one call removed in a scratch
copy). The new helper is the one with the Ping.

Daemon cases, the earlier run (`testing/qemu/run-lemnosd-restart.sh`, run 7, base `52ed5c2d`, lower load): every case
passed. This is the evidence for that base only; the current base does not reach cases a, b and c (next section).

- d: a peer in group lemnos that is uid 901 is refused by its uid; a request that is not `<verb> <nonce>` is refused.
- c: `StartUnit` fails on the helper's connection (`TransactionOrderIsCyclic`); the helper answers `accepted`, then
  `failed`; the trial is disarmed; the state is `restart-failed` with the reason; the caller gets IO.
- a: the trial is accepted and the reboot begins: the daemon's own answer is cut off by its stop (SIGTERM), the slow
  unit stops for 5 s, QEMU exits 0 through the reboot, and the boot partition verifies. The next boot's state still
  has the trial armed (`slot_trial` install-1, state `rebooting`).

Fail-before on origin/next `52ed5c2d` (the daemon with `systemctl reboot` as the restart, no helper): c's state never becomes
`restart-failed`; a's trial is refused with IO, and no reboot starts.

## The current tree (rebased on origin/next `6e13aa1a`, 2026-11-21, host load 46 to 80)

- Old path, as lemnos, in the lemnosd guest (packaged units, PID 1 on the system bus: `dbus.socket` and `dbus.service`
  active, `busctl` lists `org.freedesktop.systemd1`): `systemctl reboot` is refused by PID 1 with `Access denied`,
  rc 4, after systemctl's first attempt at login1 (`was not provided`, which this guest does not run). lemnosd stays up.
  PASS. Console: `OLD-PATH systemctl reboot as lemnos rc=4` ... `Failed to start reboot.target: Access denied`.
- d: PASS (uid 901 refused by uid; the raw forms refused).
- Install, the first step of a, b and c: FAILS at this load, in two ways. (1) Runs 8 and 9: the last 64 KiB write of the
  3,128,832-byte bundle returned TIMED_OUT (-110) at 3,080,192 bytes, the payload commit past the call deadline (the open
  item in SLOT-INSTALL-INT, not on origin/next). The same bundle passed earlier. (2) Run 10: the daemon refused the job
  before any byte was written (`the job could not be started`, status -19, from `start_job`); the cause is not confirmed,
  the cases start the install as soon as the socket is up. With no install, the trial is refused and a, b and c are not
  evaluated. Their earlier results above stand for the earlier base only.
- BusyBox (below): PASS.

## BusyBox init (QEMU, not on the board)

A static BusyBox 1.38.0 built with init and ash from the pinned tarball (`testing/qemu/build-busybox-init.sh`; the binary's
sha256 is `d6bf2ebd6cb2ad4f62114a28831a7be7c8f375362ead70c9fbc1d56e7c12c243`) is PID 1 of the guest; its `/etc/inittab`
starts `lemnos-restart --user lemnos --init`, the packaged BusyBox init script's form. `testing/qemu/run-busybox-restart.sh`:

- p0 PID 1 is init. d1 uid 901 in group lemnos is refused by uid. d2 a raw request that is not verb and nonce is refused.
- p1 uid 900 gets the helper's check: ok.
- r the probe's `accepted` line after the request, then `reboot: Restarting system` from the kernel (the helper's SIGTERM to
  PID 1); QEMU exits 0 under `-no-reboot`.

## Not covered

- Case b (accepted, then no stop within the bound): not reproducible on systemd. A reboot job starts the shutdown at
  once, so the helper sees `stopping` and the bound never expires. Holding the stop with a unit ordered before
  `reboot.target` or `systemd-reboot.service` did not hold it. The bound is covered only by the helper's unit tests
  with a stand-in manager.
- Install under load: the bundle's last 64 KiB write returns TIMED_OUT at high host load (see the current tree above), and
  a 9.6 MB bundle timed out at any load. The payload's commit is one driver tick past the call's deadline: the open item
  in SLOT-INSTALL-INT, which is not on origin/next. Until it lands, cases a, b and c are not evaluated on the current tree.
- The boot count mailbox is absent in the QEMU boot chain (the state's error says the boot count can't be cleared).
  The firmware's trial boot is not run.

## Checks to run on the board

- `systemctl --user` is not used; the board's services are the packaged ones. Run `lemnos-restart-probe check` as
  `lemnos` on the board: `restart-probe: check: ok`, 20 of 20 with two competitors on the same core.
- Run a trial on the board and read `info` for `rebooting`, then after the reboot `list` for the trial slot.
- Run the same trial with `lemnos-restart.service` stopped: the trial must be refused before it is armed.
