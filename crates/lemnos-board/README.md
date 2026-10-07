# lemnos-board

Board definitions: a versioned TOML or JSON file that lists a board's devices, each built
from a generic Lemnos driver, and the factories that build them for any host.

```toml
format = "lemnos.board"
schema_version = 1

[board]
id = "raze"

[[devices]]
id = "imu"
driver = "bmi088"
bus = "i2c-1"
address = 0x18
config = { gyro_address = 0x68, accel_range = "6g" }
```

- `BoardDefinition` parses (`from_toml_str`, `from_json_str`, `from_path`) and validates
  against a `DriverRegistry` (unknown fields and keys are errors).
- `DriverRegistry::builtin()` knows `bmi088`, `bmm150`, `ina226`, `ina238`, `ina260`, `vcm`,
  `hwmon-fan` and `thermal-zone`; `build` returns a `lemnos_device::BoxedDevice`.
- `Buses` is what a host provides: I2C buses (`LinuxBuses` with feature `linux`, runtime
  sessions in the `lemnos` facade, doubles in tests) and the sysfs root.
- `backend = "auto" | "userspace" | "kernel"` picks the chip's userspace driver or its
  mainline IIO/hwmon driver through `lemnos-drivers-linux`'s generic binding; both report
  the same channels.

`examples/raze.toml` is the Raze; `cargo run -p lemnos-board --example validate -- file...`
checks files. The format is documented in `docs/board-definition.md` with a JSON Schema in
`docs/schemas/lemnos-board.schema.json`.
