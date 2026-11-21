# WDT-CHAIN board check: the watchdog's chain from probe to steady feeding

**Untested on board.** This record is the plan for the on-board check of the watchdog chain
(`docs/slots.md`, "The watchdog chain"). Nothing here has run on a board. The host side is tested: the
fake-device tests of `service/slot-init/src/dog.rs` and `drivers/hw-watchdog`, and the QEMU check
(`testing/qemu/run-watchdog.sh`: the `normal` mode's hand-over gap with a stand-in, and the `daemon*` modes, which
run the real lemnosd under its unit behind the hook and read its start trace). The reference board is the one the
planning files name; this record says "the reference board" as the rest of the repository does.

Purpose. Confirm on the reference board the things QEMU cannot show: the open timeout covers the
initramfs start, the device rounds the 15 s request to a period under the bound, the hand-over gap
from slot-init's exec to lemnosd's root-phase feed is under a third of the applied period, and a refused
device is named in the kernel log.

## Rules for this run

- Board: the reference board only (the compute module, on its base board). Take `/run/lock/board-gpu.lock`
  with `flock` for everything that touches the board. Builds go on the solid-state disk, never on the
  hard disk.
- Report every module load, unbind, slot change and reboot the run makes. Timings are quoted from the
  reference board only.
- **No hands-only exit (hard rule).** Before any step that can reset the board, the watchdog must be
  armed, the return path must be run once (a reset that the firmware boots the default slot from, with no
  one present), and this plan must be read against the code. A step that can leave a state only a person
  can undo is not run.
- The board ends in the state it started in: the default slot confirmed, the boot counters at the
  baseline. If a reset changed them, record the change before anything else.

## Stamps to record

Each stamp is a line in the board's kernel log (`dmesg`, with `printk.time=1`) or the slot's own log, and
the time it was read. Write each stamp into the run's record with its source.

| Stamp | Where it comes from | What it proves |
| --- | --- | --- |
| S1. Watchdog driver probe | the kernel log line of the SoC watchdog driver at probe (the bcm2835 or DesignWare driver) | the start of the kernel-fed window (window 2) |
| S2. slot-init's open and arm | `slot-init: watchdog armed: N s (asked for 15 s)` in `board/slots/slot-init.log` (or the kernel log), with the time | the probe-to-open time, against the 30 s open timeout; the period read back |
| S3. The period read back at each open | `/sys/class/watchdog/watchdogN/timeout` read at S2 (through the run's own log), and again at S5 | the rounding of the 15 s request (the DesignWare rounds up; the bcm2835 does not exceed 15 s) |
| S4. slot-init's last feed before the exec | the `booted` marker write before `switch_root` (`board/slots/booted`), with the time, and the feed before the exec, which leaves no log line | the hand-over gap starts here |
| S5. lemnosd's root-phase feed | `lemnosd: root phase: <id>: watchdog: /dev/watchdogN: held across the drop and fed`, and with `LEMNOSD_START_TRACE=1` in `/etc/lemnos/lemnosd.env` the stamped `lemnosd: start trace: <s> root phase: <id> fed` (CLOCK_MONOTONIC, the kernel log's clock); the holder of the node (`ls -l /proc/<lemnosd pid>/fd`, read only) | the second opener; the hand-over gap ends here (window 4) |
| S6. lemnosd's first tick | `lemnosd: start trace: <s> first tick <id> returned`, or lemnosd's log at the first healthy tick | window 5: within the applied period of S5 |
| S7. The gap | S5 minus S4, from the two stamps; the start trace's `exec` stamp splits it into systemd's part (S4 to exec) and lemnosd's (exec to S5) | must be at most applied / 3 (the bound: `testing/qemu/run-watchdog.sh`, `HANDOVER_GAP_DIVISOR`) |
| S8. Any refusal | `WATCHDOG REFUSED (watchdog-period-outside-bound)`, `(watchdog-period-zero)`, `(watchdog-period-unreadable)`, or `NO WATCHDOG (watchdog-node-absent)` in slot-init's log | whether slot-init refused the device; a refusal is a failed check unless it was the test's purpose |
| S9. The steady feed | the lemnosd feed interval over 60 s at steady running (half the `timeout_s` on the device, read back) | window 6 |

## The checks

1. **Probe to open.** S2 minus S1 is under 30 s, with the slowest initramfs start the first boot can show.
   Record the number. If it is over 30 s, the board is not confirmed: the kernel stops feeding before
   slot-init opens, and the board resets (`docs/kernel-config.md`, "Watchdog").
2. **The period.** S3 is 15 s, or the next period the device applies, under 30 s (the bound in
   `service/slot-init/src/dog.rs`). Record the period. The DesignWare clock is read on the board (the
   watchdog's clock rate) and written here: the formula `2^(16+i) / clk` gives the period.
3. **The gap.** S7 is at most a third of the applied period. Record S4, S6 and S7.
4. **The refusal.** S8 has no line, unless the run was the refusal test. Record the S8 line when it is there.
5. **Steady feeding.** S9 is at half the timeout the device reads back, and the board is not reset in 60 s.

## Refusal test (only in a run that is planned for it)

A board whose watchdog cannot apply a period under 30 s is refused by name at the image's build
(`lemnos-ctl validate`, `watchdog-period-outside-bound`). On the board, the refusal test checks the
slot-init side only: a device that applies more than 30 s is held and fed, with S8 naming the reason.
The test is run only after the return path above has been run once, with the watchdog armed.

## Stop

The run directory on the board is removed as the last step of the script, after its stamps are copied
into this record. Nothing else on the board is removed.

## Read on Raze #2, 2026-11-21 (boot b34fee0e-2dc9-4da1-a77f-791d01632baf, rev origin/next 0d37cc55)

Read-only, both locks held by flock over ssh. `/dev/watchdog` and `/dev/watchdog0` were listed, never opened.

The journal of this boot shows: `bcm2835-wdt`: Broadcom BCM2835 watchdog timer; `slot-init: starting slot
helios-2026.2.0-dev.993-styx-e1097ec7 (helios, trial 0)`; systemd `Using hardware watchdog /dev/watchdog0`,
`Watchdog running with a hardware timeout of 15s`; and `orion-node ... watchdog_interval_ms=15000`.

The hand-over gap (slot-init's exec to lemnosd's first feed) has no stamp in the journal: no line from lemnosd's
root-phase feed was found. Board clock is wrong (2026-11-08 at the read), so no interval can be computed.

Result: partly read. The chain's start (probe, the 15 s period, slot-init's start) is in the log; the gap and the
steady feeding are **not measured**. Reboot-dependent steps are not run.
