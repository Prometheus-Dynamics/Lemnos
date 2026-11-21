# UNIT-GRANTS: the root-phase grants for a hwmon fan, and the grants table

Status: **untested on board.** The grant is built and unit-tested on a real directory (chown and mode on the attributes, the
refusal when `pwm1` is missing). No board has run it, and the guest has no hwmon fan. The package's grants table and checker
(`packaging/systemd/grants.toml`, `scripts/check-unit-grants.py`) hold on the tree; they say nothing about the kernel's answer
on a board.

## What is granted, and how

- `drivers/hwmon-fan/src/grant.rs` `grant_before_drop`: the fan's directory (found as the driver finds it, `fan_dir`), its `pwm1`
  (required) and `pwm1_enable` (granted when present) go to `root:lemnos` mode 0660, before the drop.
- `service/lemnosd/src/privilege.rs` step `hwmon fans`: runs it for each non-refused `hwmon-fan` device. A fan it cannot grant is
  refused by name; the daemon keeps running.
- The daemon stays `--user lemnos`; the unit is unchanged. `/sys` is writable under the base unit (PWM-SYSFS-UNIT's guest run).

## The check on a board

Run on a board with a `hwmon-fan` device (`fan/1` in the board file), after the unit has started:

1. `systemctl show lemnosd.service -p ExecStart -p ReadWritePaths -p ProtectKernelTunables -p User` (no `User=`; no
   drop-in for the fan).
2. The root phase's log line: `lemnosd: root phase: <device>: fan: <dir>: granted to 0:<gid>`. A refusal names the device and says why.
3. `stat -c '%U:%G %a' <dir>/pwm1 <dir>/pwm1_enable`: `root:lemnos 660` for both (the second only when the kernel has it).
4. As the daemon's user (`setpriv --reuid=<lemnos> --regid=<lemnos> --clear-groups`), write `pwm1` with the current value: it
   succeeds. The same write to `fan1_input` (read-only) or a file the step did not grant is refused.
5. The fan's duty changes when the client sets it; `pwm1` read back as root shows the new value.

Record the systemd version, the `systemctl show` lines, the stat lines, the log line, and each outcome. Record the load average with
the run.

## Not run

- A real hwmon fan, on any board, and the systemd unit in a QEMU guest with a stand-in fan.
