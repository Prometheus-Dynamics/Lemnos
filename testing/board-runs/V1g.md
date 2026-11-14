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
6. **Stop takes the gadget down.** Stop the daemon (`systemctl stop lemnosd`). Expect the host to see the
   gadget disconnect, and `/sys/kernel/config/usb_gadget/lemnos-NAME` to be gone, with
   `/dev/usb-ffs/NAME` unmounted. Start again: the tree is made again.
7. **The unit's filter.** With the drop-in, `systemd-analyze security lemnosd` scores as the packaging
   check expects (`scripts/check-packaging.sh`). The helper runs under the same filter as the daemon:
   confirm the helper's `setup` and `bind` succeed under it (checks 1 to 3 do).

## What to record

For each check: the output of the command, the daemon's error output lines for the step, and the times
asked for above, in CM5 terms only. A check that does not pass is reported with its output, not
re-run until it does.
