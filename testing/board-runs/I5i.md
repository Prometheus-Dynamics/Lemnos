# I5i: the boot partition's records, pulled out of power

Untested on board. Not run by the PR that wrote it: it needs the reference board with the power
switched off without warning, many times. This is the check of record for the two-copy records
(`lemnos_slot_store::twocopy`) and the raw placement (`lemnos_slot_store::bootfat`). The unit tests
cut every write point with a fake device; this run cuts a real one.

## Setup

- A board whose boot partition holds `TRIAL.REC` and `SECURE.REC`, pre-created by the image build
  (`twocopy::precreate`), and whose data partition is ext4.
- The board file names the boot partition by PARTUUID (`boot_partuuid`) and the udev rule that
  gives the daemon's group the one block node is installed.
- Record the board's `lemnos-ctl list` output before each run. Record the boot partition's first
  2 MiB with `dd if=<node> bs=512 count=4096 of=<scratch>/before.img iflag=direct` before the run
  (the scratch file is on the board's data partition, not on the boot partition).

## Procedure

1. Arm a trial on the board (`system.slots` `install` then `trial`). Confirm `list` shows the
   trial pending and `TRIAL.REC` reads with `attempts_left` as expected.
2. Start a `set_default` or a trial-attempt decrement (the write is the one under test). Cut power
   at a random moment during the write: pull the supply, wait ten seconds, reapply power.
3. Repeat step 2 at least 50 times in a row, with different delays from the start of the write
   (0 ms, 2 ms, 10 ms, 50 ms, and random up to 500 ms), and at least 10 times while the
   daemon is idle.
4. After each cut: boot the board. Record which value the boot loader took (`list` reports
   `trial` and `default`), and whether the board reached its daemon.
5. After every run: `dd` the first 2 MiB again to `after.img` and compare the sectors outside
   `TRIAL.REC` and `SECURE.REC` with `before.img`: they must be identical, every run.

## Pass criterion

- Every boot after every cut reads a value that is either the value before the write or the value
  the write was making. No boot reads neither, and no boot fails to reach the default slot.
- Every sector outside the record files is byte-identical before and after, in every run.
- Zero FAT or directory sectors change in any run (the placement test is the proof; this run is
  the hardware proof).

## Also measured

- The pull-the-plug count: the number of cuts, and the number of cuts that landed inside the
  record sector write (from the write's timestamp in the daemon's log, when there is one).
- Firmware behaviour on a torn or invalid pointer file (`trial.txt` made invalid on purpose,
  with the tryboot restart): what the firmware boots. This is the open item the design records.
- The U-Boot record on a U-Boot board: the same procedure, with the ladder in the environment.

## Not run

Nothing here has run. The scripted cut harness (`twocopy_tests.rs`) runs in the unit tests; that
is not the board's proof.
