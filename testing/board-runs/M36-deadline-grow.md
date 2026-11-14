# M36 board check: the good-boot deadline and the data-partition grow

Status: **untested on board.** Nothing here has run on a board. The host side is tested: the waiter
(`service/good-boot`), the deadline rule and its refusals (`lemnos_slot_store::goodboot`), the drop-in
(`lemnos-ctl tree --good-boot-unit`), the grow on a file standing for the disk, with every cut point
(`drivers/system-slots/src/grow/tests.rs`), and `systemd-analyze verify` on the unit with its drop-in.

## What the board must show

1. **Deadline expiry restarts the board, and the count rises.** On a confirmed default slot, with
   `good_boot_s` set to its minimum (30), make lemnosd never reach ready (for example a board file whose
   daemon fails at start, on a copy of the image). `lemnos-good-boot` must fail at 30 s, systemd must
   reboot the board with `FailureAction=reboot-force`, and the firmware's boot count must rise by one
   per restart (`/proc/device-tree/chosen/bootloader/count`).
2. **The count reaches the previous slot.** After the count reaches 4, the next boot reads
   `previous.txt` (the count's bit 2), and the boot runs the previous slot's bay. The state's error or
   the boot history names the count.
3. **A good boot clears nothing from the waiter.** With the normal daemon, the waiter exits 0 within a
   few seconds, and `systemctl status lemnos-good-boot` shows it succeeded.
4. **The grow.** On a board whose data partition is smaller than its disk: `lemnos-slot-init` boots,
   the grow pass runs from the driver tick, and partition 2 reaches the disk's end. A second boot changes
   nothing. A cut mid-pass (a power cut between the table write and the filesystem resize) is finished
   by the next boot.

## The return path, tested first (NOTICE 14)

Every step that can restart the board runs with the hardware watchdog armed, and the first run is a
deliberate hang that must return by itself before any other step. The plan for each step:

- Expiry: the unit fails and systemd reboots (`reboot-force`), so the board returns with nobody present.
  Who arms what, checked against the code: the watchdog is lemnosd's; a lemnosd that never starts never
  arms it, so the systemd reboot is the only return and it does not depend on lemnosd.
- A trial boot that never goes good falls back through tryboot, as before.
- The grow only writes four bytes of sector 0 and only on a layout that validates. A refused layout
  boots as it was. A torn write is refused on the next start.

A step that cannot meet this is not run on a bench board; it runs in QEMU.

## Run record

To be filled in by the run, with the output and the PC time of each step. Until then this file makes no claim.
