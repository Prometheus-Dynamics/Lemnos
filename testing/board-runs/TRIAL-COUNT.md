# TRIAL-COUNT board run

Status: untested on board. Nothing here has touched a board.

The case: on the `tryboot` form (bcm2712), default A, previous Z, a trial of B with 3 attempts whose kernel never
boots. Each attempt is two boots: A's slot-init takes the attempt and restarts with `0 tryboot`, then B's kernel
hangs and the watchdog resets the board. The firmware's `boot_count` climbs on every boot, and only lemnosd cleared
it (at a good boot, before its own restarts). Run boot by boot in `a_trial_whose_kernel_never_boots_comes_back_to_the_default_and_the_count_stays_low`
(`service/slot-init/src/boot/tests/e2e.rs`: the firmware modelled with `firmware::pick`, slot-init's real code, the
daemon's `Tryboot` record), before the fix:

    TookAttempt(A, 1), TrialHung(2), TookAttempt(A, 3), TrialHung(4), TookAttempt(Z, 5), TrialHung(6), Landed(Z, 7)

The fifth boot read 5, above `[boot_count>3]`, so the firmware read `previous.txt`: the third attempt was taken from
Z and the board landed on Z. lemnosd then sees an arrival on the previous by the count (`count::arrive_at_start`)
against A, whose files match: with a warm-reset `rsts` (0x20 or 0x1020) that is an episode against A
(`arrival::on_arrival`), and at the default `demote_after_episodes = 2` a second such trial demotes A. A good
default was blamed for a trial's failure.

The fix: slot-init clears the firmware's count (mailbox tag `0x0003808d`) and sets the restart baseline to 0 before
its restart into a trial, on a board whose firmware has the count. After it:

    TookAttempt(A, 1), TrialHung(1), TookAttempt(A, 2), TrialHung(1), TookAttempt(A, 2), TrialHung(1), Landed(A, 2)

`arrival::before_own_restart` had no caller (dead code): lemnosd clears inline in its restart check (`guard_check`),
and slot-init now clears in its tryboot pass. It is removed.

Not known without a board: whether the count after a clear reads 0 or 1 on the next boot, and what `rsts` reads after
a `kernel_watchdog_timeout` reset (`testing/board-runs/I5i6.md` item 15).

## Checks to run on the board

1. Install a slot whose kernel never boots (a kernel image that is not one), trial it with 3 attempts.
2. Read the console or the kernel log of each of A's passes: `the firmware's boot count is cleared before the restart
   into the trial` each time, and `od -An -tx1 /proc/device-tree/chosen/bootloader/count` read in each A pass stays
   at 1 or 2.
3. After the third hang the board boots A (not Z), the trial is cleared (`lemnos-ctl slots list`: `trial` null,
   `resolved` fell-back), and `lemnos/boots` records no episode against A (`info`'s episodes for A: none).
