# H3a board run: the description form for temperature, power monitors and inertial chips

Status: untested on board.

The generic description drivers (`drivers/generic-sensor`) have run only on the simulated bus.
This run puts one of them beside a hand-written driver on the same real chip and compares.

Setup: a board with an INA226 on an I2C bus Lemnos owns, a build of `lemnosd` with the
`generic-sensor` crate's `std` feature, and this file as `/etc/lemnos/chips/ina226.chip`. Its facts
are the chip's datasheet register map: 16-bit registers behind an 8-bit pointer, high byte first.

```text
identity 0xfe 0x54                      # manufacturer ID, high byte
init 0x00 0x4127 16 big                 # configuration: the reset value, continuous
init 0x05 0x0200 16 big                 # calibration for 0.01 ohm and 1 mA per count

value shunt_v 0x01 16 big signed 0.0000025
value bus_v 0x02 16 big unsigned 0.00125
value power_w 0x03 16 big unsigned 0.025
value current_a 0x04 16 big signed 0.001
```

```toml
[[devices]]
id = "rail-generic"
driver = "generic-power-monitor"
bus = "<the bus>"
config = { address = 0x40, description_file = "/etc/lemnos/chips/ina226.chip" }
```

1. Bind it and read 100 times with the rail under a steady load.
   Pass: health `ok`; bus volts within 1 % of a meter on the rail.
2. Bind `ina2xx` (`chip = "ina226"`, the same shunt) to the same chip instead, and read 100 times
   under the same load.
   Pass: the means of each of the four values agree to 1 %.
3. Change `0xfe 0x54` to `0xfe 0x55` in the file and bind again.
   Pass: a read answers `NOT_SUPPORTED` with health `failed` (`identity mismatch`), and nothing is
   written to the chip (bus trace: only the address write and a one-byte read).
4. Pull the chip's power or SDA for 10 s while reading once a second, then restore it.
   Pass: reads fail with health `failed` (`bus error`) while it is gone, and the first read after
   it is back writes the configuration and calibration again and answers.
5. Repeat 1 with a temperature sensor or an IMU on the board, described in a file of its own
   (`generic-temperature`, `generic-accelerometer`, `generic-gyroscope`), against its datasheet
   or its own driver.
   Pass: the values agree to the chip's stated accuracy.
