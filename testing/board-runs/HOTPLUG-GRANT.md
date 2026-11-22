# HOTPLUG-GRANT: late hwmon and PWM grants, and the driver's retry

Status: **untested on board.** The guest run and the unit tests hold (see below). No board has run the late grant: the
hwmon fan and the PWM channel a board names have not been plugged in or exported after the daemon started on a board.

## What is built

- `packaging/udev/70-lemnos.rules` and `packaging/init/mdev.conf.fragment`: on the add event of a hwmon device that has
  `pwm1` (and `pwm1_enable`), and of an exported PWM channel (`pwmchipN/pwmM`), the daemon's group gets the attributes it
  writes after its drop, mode 0660. `export` and `unexport` stay root's. The rules are per kind; which fan or channel the daemon
  uses is the board file's.
- The root phase (`service/lemnosd/src/privilege.rs`) no longer refuses a device whose fan or channel is not there at start.
- The drivers retry the bind under the failure rule (cap 8 s, `engine/host`): `drivers/hwmon-fan` `start` and
  `backends/linux` `SysfsPwm::open` refuse a bind while the daemon may not write the attributes (`EACCES`), and the bind
  succeeds once the grant is made. No restart.

## The check still to run on a board

On a board whose board file names a `hwmon-fan` device, with lemnosd running and no restart:

1. Load the hwmon driver that owns the fan after lemnosd started. Expect the root phase's `not there yet` line for it (not a
   refusal), `stat -c '%U:%G %a'` of `pwm1` and `pwm1_enable` to read `root:lemnos 660`, and the device to bind on its next
   retry (`lemnos-ctl` shows it running, its retry count and the wait between tries, at most 8 s).
2. Record the same for a PWM channel: exported by the image or by the board's own step after start. Expect its four attributes
   `root:lemnos 660` and its driver bound. Record whether the channel's `pwmN` device raises a `ACTION=add` uevent on export
   (`udevadm monitor --kernel --subsystem-match=pwm` on the board), since the rule depends on it.
3. On the BusyBox image, the same two checks with `mdev` as the hotplug helper.
4. Measure nothing else here: the duty writes and the baseline hand-back are the fan's own check (UNIT-GRANTS.md).

## What it does not do

- It never exports a PWM channel: export is root's, and a `pwmchip` add event does not name the channels a board uses.
  A channel must be exported by the image, by the board's step, or by the root phase at the next start.
- The udev and mdev PWM lines are not verified on a kernel. The guest has no PWM chip: `udevadm test` and `systemd-udevd`
  refuse a sysfs path that is not on a real sysfs, and the kernel's PWM chip API needs a driver the guest does not build.
