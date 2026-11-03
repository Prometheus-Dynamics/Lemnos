# lemnos-drivers-linux

Lemnos device-model (`lemnos-device`) drivers over Linux kernel interfaces:

- `HwmonFan`: a hwmon fan (`pwm1`, `pwm1_enable`, `fan1_input`) as a `Fan` with channels
  `speed` (rpm), `duty` and `pwm_mode`, controls `duty` and `pwm_mode`, and
  `FanRestore`, the failsafe a userspace fan controller applies when it stops. A `pwm-fan`
  fan (or any fan with a linked thermal cooling device) gets back the `pwm1_enable` read at bind, its cooling device gets back the governor's state from just before the controller's first write, and the thermal zones bound to it re-evaluate now (their `policy` written back) (`pwm-fan` has no
  automatic `pwm1_enable` mode). Other fans get their automatic mode (`pwm1_enable = 2` by default).
- `ThermalZone`: a thermal zone as a `Temperature` sensor (`temperature`, m°C).
- `KernelDevice`: the generic binding that serves a chip through its mainline IIO or hwmon
  driver, from the chip crate's `KernelBinding`. `KernelDevice::find` locates the kernel
  devices by name (and optionally I2C bus/address); reads produce exactly the channels the
  chip's userspace driver would.
- `KernelDevice::buffered`: a buffered read of an IIO part through `/dev/iio:deviceN`, with
  `BufferConfig` (trigger, length, watermark, character device). The scan elements are
  enabled from the binding's channels; each record decodes with the `_type` descriptors
  (endianness, signedness, bits, shift, repeat) and the kernel's alignment, and the IIO
  timestamp is the IRQ time on `CLOCK_BOOTTIME` microseconds. `IioBuffer::read` polls for
  the watermark (it blocks without spinning) and returns the values in canonical units.
  Dropping the buffer stops it and restores what setup changed. Written, untested on board.

All file IO happens under a configurable root (`SysRoot`), so tests run against a fake
sysfs tree; a buffered read's character device is a path the caller gives (a file in tests). `lemnos-lite`, the runtime and `lemnosd` host these devices alike.
