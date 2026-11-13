# Q2g: boot holds in the overlay, the boot fragment and the U-Boot fragment

Status: untested on board. Nothing was loaded on a board, and no meter or scope was attached to this PC.
The checks below run on a board, on the CM5 (Raze #2) in a local session, after this PR merges.

## What changed

- Each boot hold (a `[[lines]]` entry with `boot`, and a device port's VBUS held at its off level) is
  written to the boot fragment (unchanged format), to a U-Boot fragment (`lemnos-ctl tree --uboot`,
  `gpio set NAME` / `gpio clear NAME`), and, when no run-time user needs the line, to the overlay as a
  `gpio-hog` under the chip's node.
- A hog is made for a pure hold. A line a device uses at run time, or a pin set's pad (the reference board's
  fan line is the PWM output's pad), is held by the boot path only.
- A device port's VBUS regulator is disabled by the overlay when its line is held, so the regulator does
  not request the line the hog holds.
- The start check reads a hogged line from `/sys/kernel/debug/gpio` and names it when it is not listed.

## Checks on the board

Run each check with the overlay and the boot fragment applied, from power-on. Record the level on the meter
at the pin (or the port's VBUS) and the kernel's view, in the order below.

1. **Reference board, fan line (GPIO 45 of RP1), boot path only.** Power on with the firmware reading
   `boot.txt`. The pad must read high from the firmware's first write until the PWM's pin set takes it at the
   fan driver's probe. No hog is made. Expect: `gpio=45=op,dh` applied; `/sys/kernel/debug/gpio` lists
   GPIO 45 only after the PWM takes it (the pin set is not a GPIO request).
2. **RK3588, the OTG port's VBUS (GPIO4 line 11, GPIO 139), a hog.** Build the image with the overlay merged
   by `fdtoverlay`, and the U-Boot fragment run before `booti`. From power-on the pin must read low until the
   kernel's chip probe, and low after it. Read it as `/sys/kernel/debug/gpio`: `gpio-139 (VBUS of usb-otg0,
   off low) out lo`. The base regulator `vcc5v0-otg-vbus` must be absent from the probe list (disabled).
   With the port in device mode, VBUS on the host side must stay off (a meter on the host's 5 V pin).
3. **The U-Boot window, RK3588.** Stop the boot in U-Boot before `gpio clear E11`: the pin's level is the reset
   state (record it: this is the open window). After `gpio clear E11`, read the pin: low.
4. **Hog at run time (overlay applied after the chip probe).** Apply the overlay on a running system with
   the chip already registered. The kernel's `of_gpio_notify` path should add the hog: the line must then
   be listed as `out lo`. Record whether it is listed; a missing listing is a finding.
5. **The start check.** `lemnos-ctl validate` with the live tree: VBUS of the RK3588 port reads `lo`, no problem
   named. Then force the line high with a debugfs-visible driver, and the check must name the level.

## Not verified

- The Rockchip pad mux: a hog or `gpio set` on a pad whose reset function is not GPIO is not known to move
  it (`docs/device-tree.md`, "The pad's mux"). Check 2 and 3 record the pad's level; a pad that does not move
  is the finding.
- The U-Boot naming for RK3588 is assumed from the RK3566 driver (`docs/device-tree.md`, "U-Boot naming").
- The Raspberry Pi kernel's hog path (`rpi-7.2.y`) was not compared with v6.12 line by line.
