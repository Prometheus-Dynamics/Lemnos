# lemnos-drivers-linux

Lemnos device-model (`lemnos-device`) drivers over Linux kernel interfaces:

- `HwmonFan`: a hwmon fan (`pwm1`, `pwm1_enable`, `fan1_input`) as a `Fan` with channels
  `speed` (rpm), `duty` and `pwm_mode`, controls `duty` and `pwm_mode`, and
  `restore_automatic` (`pwm1_enable = 2`), the failsafe a userspace fan controller restores
  when it stops.
- `ThermalZone`: a thermal zone as a `Temperature` sensor (`temperature`, m°C).
- `KernelDevice`: the generic binding that serves a chip through its mainline IIO or hwmon
  driver, from the chip crate's `KernelBinding`. `KernelDevice::find` locates the kernel
  devices by name (and optionally I2C bus/address); reads produce exactly the channels the
  chip's userspace driver would.

All file IO happens under a configurable root (`SysRoot`), so tests run against a fake
sysfs tree. `lemnos-lite`, the runtime and `lemnosd` host these devices alike.
