# V1c: the unit state in packages, the real ranges, the location fallback and the sim walk

Untested on board. These are description values and the simulated board only: no hardware path,
register or bus transaction changed.

## The check

1. `lemnos-ctl show` for the thermal zone and the hwmon fan: the `location` line is the sysfs
   directory (`/sys/class/thermal/thermal_zone0`, `/sys/class/hwmon/hwmonN`), as before.
2. `lemnos-ctl show` for a generic accelerometer or gyroscope on the reference board's chip: its
   `range` is the chip's span from the description (the bits and the scale), in m/s² or rad/s, and
   `step` is one count.
3. `lemnos-ctl show` for the orientation device: roll and yaw range `-3.14159...` to `3.14159...`, pitch
   `-1.5707...` to `1.5707...`.
4. A device with no `location` in the driver but a board `uses` entry (the status light, the fan
   control): `location` reads `pixels light` (the board's own words).

## What to record

The panel output for those devices, pasted here. Nothing has run on a board yet.
