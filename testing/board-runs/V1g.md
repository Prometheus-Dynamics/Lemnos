# V1g: the USB gadget's setup by the privileged helper, on a board

**Status: untested on board.** Nothing here has run on a board. The helper's behaviour was checked
against the stand-in tree and a fake helper (`backends/linux/src/gadget/tests.rs`), against the
vocabulary and its fuzz (`service/lemnosd/src/helper.rs`), the parent's re-fork loop with a fake process
(`service/lemnosd/src/supervisor.rs`), and in the QEMU harness on `dummy_hcd` (`testing/crates/gadget-check`),
including a controller that appears late. This file is the check to run on the reference
board (CM5) in a local session, under `/run/lock/board-gpu.lock`, with builds on NVMe or SSD only.

The board's gadget is the one `R1` checks. Its board file has a `usb-gadget` device whose `uses` names
`udc:NAME`. Install the gadget drop-in (`packaging/systemd/lemnosd.service.d/gadget.conf`) first.

## Checks

1. **The controller appears late.** Boot with the controller's UDC not yet bound (its driver unloaded or
   held back until after the daemon starts). Expect: the daemon starts; `lemnos-ctl list` shows the gadget
   failed with its reason (not there); the daemon's error output has one line per attempt, naming the
   gadget and the step. Then bind the controller's driver. Expect: within 8 s of the UDC appearing (its
   uevent brings the attempt forward), the gadget is up, with no daemon restart. Record the time from the
   bind to `up` in the report.
2. **Host role, then device role.** With the controller in host role, the gadget is not made and retries;
   switch the controller to device role (the role switch changes the UDC). Expect the same as check 1: up
   after the switch, on the uevent, without a restart. Record the controller's role switch and the time.
3. **configfs not mounted at start.** Start the daemon with `/sys/kernel/config` not mounted. Expect: the
   daemon starts, the gadget fails with its reason, the helper's `setup` answers `ENOENT`, and the daemon's
   error output names the step. Mount configfs. Expect: the gadget comes up on the driver's retry (on the
   timer, since a mount sends no uevent under `usb_gadget`), no restart.
4. **The two processes.** While the gadget is up, `ps -o pid,ppid,user,comm -C lemnosd-gadget,lemnosd-gsuper` shows
   `lemnosd-gsuper` (the root parent, the daemon's child) and `lemnosd-gadget` (the helper, the parent's child),
   both uid 0. In `/proc/<pid>/status`: the helper's `CapEff` and `CapPrm` are only bit 21 (`CAP_SYS_ADMIN`),
   `NoNewPrivs: 1`, `Seccomp: 2`; the parent's are bits 8 and 21 (`CAP_SETPCAP`,
   `CAP_SYS_ADMIN`: `supervisor::KEEP`, `CapEff` 0x200100), `Seccomp: 2`. The daemon's own `CapEff` shows only bit 17 (`CAP_SYS_RAWIO`).
   Record the values.
5. **The helper dies, and is forked again.** This was not run in the QEMU guest (the re-fork is proven there with a
   fake process, and the filter stack with forked children), so it is a board-only check. The operator reads
   the helper's PID once from `ps -o pid,ppid,user,comm -C lemnosd-gadget` (parent PID = the `lemnosd-gsuper` PID of
   check 4), writes it to a file in the run's own directory together with `/proc/<pid>/cmdline` and the parent
   PID, and stops that PID and only it, after re-reading `/proc/<pid>/cmdline` and the parent PID and finding
   them unchanged (never by name or pattern; the run's stop-by-recorded-PID tool does exactly this). Watch the
   parent fork it again and the new helper adopt the tree. Expect: the parent
   writes `the gadget helper (pid N) was killed by signal ...; starting it again in 1000 ms (restart 1)`; within
   a second a new `lemnosd-gadget` has a new PID; a gadget that was up stays up (nothing asked the helper);
   asking for something that needs the helper (stop the gadget's device with `lemnos-ctl power off` and on again)
   is served by the new one, which adopts the tree the old one left (`the tree that was there is the board
   file's: adopted` on the error output) and does not mount again. Stop the new helper three more times in
   a row: the waits are 2, 4 and 8 s, and a fifth is 8 s again. Record the log lines and the times.
5b. **The helper is asked by someone else.** Not reachable from outside: the socket has no name. The test is in
   `service/lemnosd/src/helper.rs` (`a_peer_that_is_not_the_daemon...`); nothing to run on the board.
5c. **The serial is not readable yet.** (The health note follows it: `lemnos-ctl list` shows the gadget with
   the note `no USB serial string: the board serial could not be read` while the daemon's own read at start failed
   and the gadget came up without the serial; once a setup reports `ok serial` the gadget is `ok` and the note is gone.) On a board whose gadget derives its addresses from an nvmem serial, start
   the daemon with the nvmem driver not yet probed. Expect: the gadget retrying with `the board serial the
   gadget's addresses come from is not readable yet: retrying`, the helper's reason
   (`the board serial is not readable yet: ...`) on the error output; load the nvmem driver; the gadget comes
   up on the next attempt with no restart, and its USB serial and NCM addresses are the derived ones.
6. **Stop takes the gadget off the bus.** Stop the daemon (`systemctl stop lemnosd`). The service manager
   empties its store, the gadget's last FunctionFS files close, and the host sees the gadget disconnect.
   The tree `/sys/kernel/config/usb_gadget/lemnos-NAME` stays (a stopping daemon tears nothing down), and
   the mount goes with the unit's namespace. Start again: the tree is adopted, mounted again and bound.
   A restart (not a stop) keeps the gadget on the bus: [`GADGET-KEEP.md`](GADGET-KEEP.md).
7. **The unit's filter.** With the drop-in, `systemd-analyze security lemnosd` scores as the packaging
   check expects (`scripts/check-packaging.sh`). The helper runs under the same filter as the daemon:
   confirm the helper's `setup` and `bind` succeed under it (checks 1 to 3 do).

## What to record

For each check: the output of the command, the daemon's error output lines for the step, and the times
asked for above, in CM5 terms only. A check that does not pass is reported with its output, not
re-run until it does.

## Detached form (image where lemnosd owns the USB link)

On that image every check here that changes the gadget cuts the runner's link, whether lemnosd
stops or not: the gadget is the only route. Which steps do what, as `scripts/checks/V1g.sh` runs
them, in this order:

- 4: read only, first (the processes found by parent PID from lemnosd's main PID, not by name).
- 5: the helper stopped five times by its recorded PID, after `/proc/<pid>/cmdline` and the parent
  PID are read again and found unchanged; the waits to a new helper are timed. lemnosd stays up and
  the gadget with it. Between the first and second kill the gadget's device is powered off and on
  (`lemnos-ctl power`): the link goes for that time. Timed restore: 60 s (the waits 1 + 2 + 4 + 8 + 8
  s and the power cycle); it powers the device on again.
- 6: `systemctl stop lemnosd`, a snapshot (tree and FunctionFS mount), start. One lemnosd down
  window, 5 s restore.
- 3: lemnosd stopped, configfs unmounted, lemnosd started: one lemnosd down window (5 s restore).
  Then the gadget's failure and the error lines are recorded, configfs is mounted again, and the
  time to the gadget `running` on the driver's own retry is measured (want at most 8 s). A 40 s
  restore around the whole check mounts configfs again whatever happens. If configfs cannot be
  unmounted, check 3 is recorded NOT-RUN and nothing else changes.
- 1: "boot with the controller held back" needs a reboot, which is not allowed; it runs as a
  restart instead: lemnosd stopped, the controller unbound from its driver
  (`/sys/class/udc/NAME/device/driver/unbind`, reported), lemnosd started (one down window, 5 s
  restore), then bound again and the time to `running` measured (want at most 8 s, on the uevent).
  The 40 s restore binds it again.
- 2: only if the controller has a `usb_role` switch (none is known on the board): `host`, then
  `device`, timed; the 40 s restore sets the role back. Otherwise NOT-RUN.
- 5c and 7 are unchanged (5c needs an nvmem driver held back at start; 7 is a host check).

Running it (shared by every detached form; `scripts/lemnos-check.sh`'s header has the details):

1. Upload `scripts/lemnos-check.sh`, `scripts/detached-lib.sh` and `scripts/checks/` to
   `/data/board/checks/bin/` over the session (not `/tmp`: the session's `/tmp` may be private to the
   comm core's unit, and the transient units would not see it).
2. Hold no board lock in the session: the check takes `/run/lock/raze-gpu.lock` and
   `/run/lock/board-gpu.lock` itself (10 s wait) and refuses, stopping nothing, if either is held.
3. As root: `bash /data/board/checks/bin/lemnos-check.sh start V1g [STEPS="4 5 6 3 1 2"]`. It copies the
   scripts into `/data/board/checks/V1g/<UTC stamp>/bin/`, starts the check in the transient unit
   `lemnos-check-V1g` and returns. The link then goes; nothing on the board depends on the session.
4. The check logs its hypothesis, a baseline and every step (CLOCK_REALTIME stamp and seconds since
   boot) to `log` in the run directory, verdicts to `result`. Before each stop it arms a timed
   restore in its own transient unit (`lemnos-restore-V1g-NAME`) and sees it running; at the bound,
   uncancelled, the restore kills the check's unit, undoes what the check changed, starts lemnosd
   and the comm core (`COMM_UNIT`, default `orion-comm.service`) with backoff 1, 2, 4, 8, 8 ... s, never giving up, and checks the end state.
   The end state, checked by the check or the restore: lemnosd active, the `hw-watchdog` device
   `running ok`, the comm core and the bridge as before, the gadget bound to the same controller, no
   drop-in of ours, the link's addresses back, the same boot id. The address on `lemnos0` is
   systemd-networkd's (a runtime `.network` file written at boot, applied again when the interface
   comes back): the end check waits 30 s for it. One still missing then is added back by hand so
   the board stays reachable, and the run is FAILED: a recovery never lets a run pass. Nothing touches config.txt, the default slot,
   `mem=`, `.reboot-ok` or the watchdog.
5. Reconnect after 3 min and run `bash /data/board/checks/bin/lemnos-check.sh collect V1g`: the
   log, the restore logs, the results, whether each restore was cancelled or fired, and whether the
   board reset since the baseline (`THE BOARD RESET`, from the boot id). Its first and last lines
   are the run's verdict: `RUN PASSED`; `RUN INCOMPLETE` when a step was not run; `RUN FAILED` with
   the reasons when a step failed, the board was not as found (an address re-added by hand
   included), the board reset, or a restore had to finish the check. Paste it here.

The watchdog: once slot-init has exec'd away, lemnosd's `hw-watchdog` driver is the only feeder of
`/dev/watchdog`. It feeds every half period and never writes the magic close. When lemnosd stops or
dies the kernel pings once on the close and logs `watchdog did not stop!`
(`drivers/watchdog/watchdog_dev.c`, `watchdog_release`); the board resets one period (15 s) after
that unless a new lemnosd feeds. A stop that hangs before it drops the driver leaves only what was
left of the last feed: half a period (7.5 s) at worst. Every lemnosd down window is therefore bounded
by its timed restore at `DOWN_BOUND_S` = 5 s, plus 2 s for lemnosd's start, under that 7.5 s; the
check refuses to start if the device's own `timeout` makes this false. The log says for each window
whether the close ping was seen and how long lemnosd was down. A lemnosd that cannot be started
inside the period (the backoff running past it) resets the board: expected, recorded by `collect`.

Status of the detached form: untested on board. Rehearsed on the PC against the stand-in board
(`scripts/rehearse/rehearse.sh ID CASE`: fake systemd with `Restart=always` and starts that fail
like a failed start job, the kernel's close ping, a 15 s watchdog that resets the stand-in): the
normal run; the check killed right after it stops lemnosd (the stand-in's `systemctl stop` freezes
the check at that exact point); the check frozen there; two failed starts (the restore retries 1
and 2 s apart and the board stays up); five failed starts (the backoff passes the period and the
stand-in resets, which `collect` reports as RUN FAILED); the link's address put back by networkd
(RUN PASSED) and not put back (re-added by hand, RUN FAILED). Three failed starts are the most a
restore survives inside one period.
