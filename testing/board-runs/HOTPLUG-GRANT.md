# HOTPLUG-GRANT: late grants for a board's own fans and PWM channels

Status: **untested on board.** The guest runs and the unit tests hold (below). No board has run a late grant: a fan or a
PWM channel the board names has not been plugged in or exported after lemnosd started on a board.

## What is built

- `lemnos-ctl tree BOARD --udev` and `--mdev` write, after the board's node rules, one late-grant rule set per fan or PWM
  channel the board names (`service/devtree/src/permissions.rs` `late_grants`, `udev.rs` `late_rules`, `mdev.rs` `late_line`).
  Only enabled devices the daemon drives: none for `owner = "host"`. A fan is matched by its `name` (or its `hwmon` directory);
  a channel by its controller's device-tree node (`pwm:NODE:CH`). The packaged `70-lemnos.rules` and `mdev.conf.fragment`
  hold none.
- A hwmon fan's `pwm1` and `pwm1_enable` are granted on the fan's `add` (udev: `ATTR{name}`, `TEST`). A channel's `period`,
  `duty_cycle`, `polarity` and `enable` are granted on the `change` the kernel sends its chip when the channel is exported
  (`EXPORT=pwmN`). The rules grant and never export; `export` and `unexport` stay root's.
- The root phase (`service/lemnosd/src/privilege.rs`) does not refuse a fan or channel that is not there at start. The drivers
  retry the bind under the failure rule (cap 8 s): `drivers/hwmon-fan` `start` and `backends/linux` `SysfsPwm::open` refuse a
  bind while the daemon may not write the attributes (`EACCES`).

## The kernel's event, read from the source and then seen on a real chip

- `linux-7.3.5/drivers/pwm/core.c`: `pwm_export_child` registers `pwmN` with no bus and no class (`device_register`, line 1351)
  and sends the chip `KOBJ_CHANGE` with `EXPORT=pwmN` (lines 1358 and 1360). `pwm_unexport_child` sends `UNEXPORT=pwmN`
  (lines 1383 and 1385). `drivers/base/core.c` `dev_uevent_filter` (line 2673) sends no uevent for a device with no bus or class,
  so the channel's own `add` never happens.
- Guest (QEMU, kernel 7.3.4-202, out-of-tree test module `lemnos_fake_pwm.ko`, a real `pwmchip_add` with four channels):
  `udevadm monitor` captured `ACTION=change`, `DEVPATH=/devices/platform/lemnos-fake-pwm/pwm/pwmchip0`, `SUBSYSTEM=pwm`,
  `EXPORT=pwm1` on the export, and `UNEXPORT=pwm1` on the unexport; no `add` for `pwm1`. A rule with `ACTION=="add"` (the earlier
  wildcard line) did not grant `pwm1` on this event.

## The check still to run on a board

On a board whose board file names a `hwmon-fan` (by `name`) and a `pwm-channel-fan` on a `pwm:NODE:CH` controller, with lemnosd
running and no restart:

1. Load the fan's driver after lemnosd started (or plug its hwmon in). Expect `stat -c '%U:%G %a'` of `pwm1` and `pwm1_enable`
   to read `root:lemnos 660`, and the device to bind on its next retry (at most 8 s).
2. Export the board's channel through the chip's `export` file. Expect its four attributes `root:lemnos 660` and the channel to
   bind. Expect a channel the board does not name to stay `root:root 644`. Capture the real event with
   `udevadm monitor --kernel --property` and record it.
3. The channel's device-tree match on this board: the rule and the mdev line read the chip's parent `device/of_node` link. Check
   that `readlink -f /sys/class/pwm/pwmchipN/device/of_node` on the board is the board's controller node
   (`/sys/firmware/devicetree/base/...`), and that the match fires only for it.
4. The same two checks on the BusyBox image, with `mdev` as the hotplug helper, on a BusyBox built with `FEATURE_MDEV_EXEC` (its
   `@` and `*` commands). `*` is needed for the channel: BusyBox's `change` event runs only a `*` command.

## What it does not do

- It never exports a PWM channel. A channel is granted only once root (the image, the board's step or the root phase at the next
  start) exports it.
- Only `pwm:NODE:CH` channels get a grant. The board's generator refuses `pwmchipN:CH` names (the kernel's number is probe order),
  so no grant is made for them.
- The device-tree match is not run here: the guest has no device tree. The guest test replaces it with a chip-name match, so it
  checks the grant, the event and the negatives, not the `of_node` comparison.
