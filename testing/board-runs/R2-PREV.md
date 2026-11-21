# R2-PREV: the Pi "previous before flashing mode" (untested on board; the count rung is sim-only)

Not run on a board. No board was touched for this record. The count rung of the Pi form cannot be
reached on Raze #2 without changing the default slot's boot files, which the no-hands rule and
`docs/slots.md` do not allow, so this check is written down and left for a scheduled run.

## Why a trial slot is not used for the count rung

The brief offered "a trial slot, or else sim-only". A trial does not go through the path being checked:

- The count rung is `config.txt`: `include default.txt`, then `[boot_count>3]` / `include previous.txt` (the threshold; `[boot_count&4]` was the earlier form, see I5i3.md's correction)
  (`drivers/slot-store/src/firmware.rs`, `CONFIG_TXT`). It is read on every ordinary boot.
- A trial is `tryboot.txt`: `include trial.txt` with no count filter (`TRYBOOT_TXT`, and the test
  `pi_firmware_text_is_the_fixed_fallback_text` asserts it has no `boot_count`). It runs only when the
  firmware's tryboot flag is set.

So an unreadable kernel on a trial exercises the tryboot fallback, not `previous.txt` picked by the
count. So the count-picked previous is marked sim-only, as PARKED.md says for R2-PREV. This file records what a board would have to show.

## What the sim covers (run on the PC, see the PR body)

| What | Where |
|---|---|
| The firmware text: count filter, previous pointer, no count on tryboot | `drivers/slot-store/src/firmware_tests.rs::pi_firmware_text_is_the_fixed_fallback_text` |
| Which pointer a count picks: 0-3 and 8-11 default, 4-7 and 12-15 previous, for ever | `drivers/slot-store/src/firmware_tests.rs::the_count_pass_picks_default_for_0_to_3_and_previous_for_4_to_7_for_ever` |
| A fresh image's previous names the default's bay (no empty bay) | `drivers/slot-store/src/boot_raw_tests.rs::a_fresh_image_at_count_4_boots_the_default_because_previous_names_the_default_bay` |
| The previous started at counts 4 to 7 is a trial, asks for no flashing mode, does not mark the default failed | `service/slot-init/src/boot/tests.rs::a_pi_count_picked_previous_starts_as_a_trial_and_asks_for_no_flashing_mode` (new in this PR) |
| Default's files do not match: the default pointer moves to the previous, one raw write | `service/slot-init/src/boot/tests.rs::default_boot_files_that_do_not_match_fall_back_to_previous` |
| At the limit (8 restarts) the flashing mode is asked, no record changed | `service/slot-init/src/boot/tests.rs::refusals_at_the_limit_go_to_the_flashing_mode_and_change_no_record` |

The firmware's own pick is the one thing the sim cannot run: `config.txt` is read by the Pi firmware,
which is not in the sim. Only the board can show it.

## Checks that need a board (none run)

Hypothesis: on the M2 image, with the default's kernel unreadable and the previous slot intact, a boot
whose count has bit 2 set (4 to 7) starts the previous slot, which slot-init records as a trial
(`trial-init`); a count below 8 asks for no flashing mode; at 8 slot-init asks for RPIBOOT once and restarts.

1. Precondition (blocked): the M2 image on Raze #2 (R2-IMG), a previous slot in its own bay that
   `previous.txt` names, and `config.txt`, `default.txt`, `previous.txt`, `tryboot.txt` hashed as found.
   Locks: `flock /run/lock/raze-gpu.lock` and `/run/lock/board-gpu.lock` for the whole run.
2. Arm the dead-man first: a transient systemd timer that restores the default pointer and the
   default's files, then restarts, and that runs its own verify. Its self-test runs before any change.
3. Make the default's kernel unreadable. This is the step that is not allowed on the default slot as
   the rules stand. It is the reason the check is blocked, not a plan to run it.
4. Restart and read `/proc/device-tree/chosen/bootloader/count` and the slot's `booted` mark. Expect
   the previous's cmdline, `trial-init` naming the previous, no `flash mode` in the log.
5. Repeat the restart to count 8 and read the RPIBOOT request in the log. Expect download mode to be
   asked once and nothing else changed.

## Stop and return path

- Return first, tested first: the host flasher (`lemnos-ctl` or the RPIBOOT loader) reflashes the
  default's bay from the PC, which is the exit that needs no board action. The dead-man timer does the
  same restore on its own clock if the PC is gone.
- Nothing in steps 1 to 5 changes the data partition's state beyond the count and `trial-init`.
- The watchdog: the board's systemd feeds the hardware watchdog (L1 note); the check does not touch it.
  If the dead-man's verify fails and the reboot-locked helper does not restart the board, nothing resets
  it; that residual is recorded in `testing/board-runs/` L1 and applies here.

## The previous slot is a trial (ruled)

Ruled by the coordinator: the previous slot booted from the count rung is a trial and stays unconfirmed until it
proves healthy; if it also fails, the count keeps rising to the rescue at 8. `docs/slots.md` ("Previous before
flashing mode") now says so. The sim test `a_pi_count_picked_previous_starts_as_a_trial_and_asks_for_no_flashing_mode`
asserts `trial-init` for the count path.
