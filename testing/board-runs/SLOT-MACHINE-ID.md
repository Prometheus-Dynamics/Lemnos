# SLOT-MACHINE-ID board check: the machine id survives reboots on a slot

**Untested on board.** This record is the plan for the on-board check of the machine id (`docs/slots.md`,
"The machine id"). Nothing here has run on a board. The host side is tested: the derivation and its vector
(`lemnos_identity::machine_id`), the refusals and the bind with the fake `Ops` (`service/slot-init/src/boot/tests.rs`),
the driver's check of the id against the serial (`drivers/system-identity/src/tests.rs`). The QEMU guest cannot show
it: the slot boot takes its boot source from the device tree, and the x86 guest has none, so no slot reaches the
hand-over there. The reference board is the one the planning files name.

Run in a local session on the user's machine, under `/run/lock/board-gpu.lock`, on the CM5 (Raze #2) only. Builds go on
NVMe or SSD, never the Github HDD.

**Hypothesis.** On a booted slot, `/etc/machine-id` is the read-only bind of slot-init's file, holding
`derive(serial)` (32 lowercase hex digits and a newline). The same board gives the same id after a reboot, so the
persistent journal directory `/var/log/journal/<id>` on `/data` stays one directory. `system.identity/1`'s
`machine_id` answers that id, and the same id comes back after a reboot.

**Return path first (no-hands-only exit).** Before any step, run one full reboot of the default slot with nothing
changed, and check that the board comes back on its default slot by itself. The watchdog must be armed (the boot log
says `slot-init: watchdog armed`). Only then start the steps below.

**Steps.**

1. Boot the default slot. Read `/etc/machine-id` (`cat /etc/machine-id`). Expect 32 lowercase hex digits and a newline.
   Record it as ID1.
2. Check it is read-only mounted: `findmnt -no OPTIONS /etc/machine-id` must list `ro`. Also `touch /etc/machine-id`
   must fail with `Read-only file system`. Record the output.
3. Check the boot log: `journalctl -k | grep -E 'slot-init: (machine id|MACHINE-ID)'` must show
   `machine id bound read-only over /etc/machine-id` and no `MACHINE-ID REFUSED` line. Record it.
4. Compare with the contract: the bridge's `.call machine_id 64` on the identity device (Orion's action for the device,
   as `docs/daemon.md` describes it). The answer must equal ID1. Record it.
5. Check the journal directory: `ls /var/log/journal/` must list a directory named ID1 (systemd keeps the journal under
   the machine id). Record the listing.
6. Reboot the board (a plain reboot, from the default slot). After it comes back, repeat steps 1 to 5. ID2 must equal ID1,
   and the `/var/log/journal/` listing must show the same single directory, not a new one.
7. Refusal check (optional, only after 1 to 6 pass): the identity device's health is `Ok` in the same boot. A `transient:`
   degradation would mean the id on the node is not the derivation of this board's serial.

**Pass.** ID1 equals ID2, equals `machine_id`, is read-only mounted, the journal directory is the same single directory
after the reboot, and the boot log has no `MACHINE-ID REFUSED` line. Any other outcome is a failure: record the output of
every step, do not retry it, and report it.

**Restore.** Nothing is changed on the board beyond the normal slot boots. Leave the default slot as it was. The
watchdog is left armed. If a boot does not come back on its own, the board's recovery is the one in `docs/slots.md`
(the previous slot, then the boot order), not a manual edit of `/etc/machine-id`.

**Record.** Save the output of each step under `testing/board-runs/` in a new record, and do not change this plan's
steps when you run them.
