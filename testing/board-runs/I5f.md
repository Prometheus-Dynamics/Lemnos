# I5f board run: a trial resolves on the CM5, and the cost of the tryboot pass

Status: untested on board. How a trial resolves (auto-confirm, the deadline, the attempts, the
`resolved` record) has run only in unit tests with fakes: a loader model, a board view and a
temporary directory standing in for both partitions. The tryboot pass has run only as a decision
table and as the `slot-init` tests over a fake `/`. Nothing has restarted a board to test them.

Run in a local session on the CM5 (Raze #2). Take `/run/lock/board-gpu.lock` for the whole run.
Build on NVMe or SSD, never on the Github HDD. Restarts, slot changes and writes to the boot
partition are allowed; report each one. Keep a serial console attached: `slot-init` logs as
`slot-init: ...`, and the driver's state is in `board/slots/` on the data partition.

Report CM5 timings only.

## Setup

1. Install two slots, `A` (the default) and `B`, from two bundles built as `docs/slots.md` says,
   with the `tryboot` record. Both carry the same `lemnosd` and a board file whose `system-slots`
   has `trial_attempts = 3`, `confirm_uptime_s = 30`, `confirm_deadline_s = 600` and a
   `health = ["device:<a device on this board>"]` source.
2. Before each step, keep copies of `/boot/firmware/{default.txt,trial.txt,trial-attempts,config.txt}`
   and of `/data/board/slots/`.
3. Give `B` a boot record that differs from `A`'s in a way the console shows (its kernel command
   line names it), so `starting slot B` is visible.

## Checks

1. **A healthy trial confirms by itself.** `trial B`. Expect a restart, the trial boot to log
   `starting slot B (on trial)`, and no call to `confirm`. Within about 30 s of the daemon's start
   `list` must show `state: confirmed`, `slot_default: B`, `trial: null` and
   `resolved: {"outcome":"confirmed","by":"auto",...}`. `trial-attempts` must be gone.
2. **Power cut before the uptime.** `trial B`, then cut power 10 s after the daemon starts, on three
   boots in a row. Expect each boot to start `B` again (`trial.txt`
   names it), `list` to show `trial.attempts` with three entries, `last` of
   `daemon-reached-not-yet-confirmed`, and `attempts_left` of 3 after each. The fourth clean boot
   must confirm `B` by itself.
3. **Power cut before the daemon.** Make the trial's boot fail before `lemnosd` starts (for
   example a bad root hash in its command line). Expect the default's kernel to take an attempt on
   each boot (`trial-attempts` going 3, 2, 1, 0 across boots), the trial to be tried each time, and
   on the first boot with 0 left `A` to start with `resolved` `fell-back` and the reason
   `slot B ... did not reach a running daemon in 3 boots` (or the slot's own reason, when the
   trial recorded one).
4. **A health source that never goes good.** Point `health` at a `file:` that is not written this
   boot. Expect no confirm at 30 s, the reason in `list`'s `trial.reason` naming the file, and at
   600 s a restart into `A` with `resolved` `fell-back` and the reason. Repeat with
   `reboot_at_deadline = false`: the board must not restart, and the trial stays pending.
5. **`auto_confirm = false`.** Repeat check 1 with the setting off. Nothing confirms or restarts by
   itself. A `confirm` call during the wait confirms it, and `resolved` says `by: call` with the
   caller.

## Measured: the extra tryboot pass

The `tryboot` form runs the default's firmware, kernel and initramfs once before a trial boots.
Measure both paths on the same board and the same bundles. Report each time in seconds.

- **Without the pass**: `tryboot` restarts straight into `B` (the way the driver did before this
  PR: `systemctl reboot --reboot-argument="0 tryboot"`). Time from the restart to `lemnosd`
  running in `B` (the `starting slot B` line, then the daemon's first `list` answering).
- **With the pass** (this PR): `trial B`, plain restart, the default's initramfs spends the
  attempt and restarts with the tryboot argument. Time from the same restart to `lemnosd` running
  in `B`.

Do five runs of each, alternating, and report the median and the range. The difference is the
cost of one extra pass. Note the time the firmware takes before the first kernel line, which both
paths share.

## Record

For each check: the commands, the console lines, the `list` output (the `trial` and `resolved`
fields), the contents of `board/slots/trial.env` and `board/slots/resolved.env`, and whether it
matched. Any restart, slot change or boot-partition write is listed. A check that fails is
reported with its output, not explained away.
