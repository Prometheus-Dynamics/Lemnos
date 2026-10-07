# Lemnos

Lemnos is a Rust workspace for hardware discovery, driver matching, and runtime interaction across GPIO, PWM, I2C, SPI, UART, and USB surfaces.

Lemnos 2.0 is also the hardware foundation underneath [Styx](https://github.com/Prometheus-Dynamics/Styx): `embedded-hal` 1.0 is the bus vocabulary, device drivers are `no_std` and run on Linux and microcontrollers alike, and Linux is reached through one pure-Rust implementation with no C libraries. See [docs/foundation.md](docs/foundation.md).

The repository is split into small crates so applications, custom drivers, Linux backends, built-in generic drivers, and test helpers can evolve independently.

## Workspace Layout

- `crates/lemnos`: consumer-facing facade and builder API
- `crates/lemnos-hal`: `no_std` hardware vocabulary: embedded-hal 1.0 (re-exported), register maps over I2C/SPI, regulators, clocks, `ErrorKind`, mocks
- `crates/lemnos-device`: `no_std` compact device model: device classes, quantities and units, fixed-point channels, controls, `Sensor`/`Control` traits
- `crates/lemnos-lite`: `no_std` static device table with status and polling, for firmware and small Linux images
- `crates/lemnos-core`: shared types, requests, state, and descriptors (`no_std` + `alloc`)
- `crates/lemnos-bus`: typed bus/session traits for hardware access
- `crates/lemnos-discovery`: discovery probes, inventory snapshots, and diffing
- `crates/lemnos-driver-manifest`: driver metadata and matching rules
- `crates/lemnos-driver-sdk`: driver authoring helpers and bind-time utilities
- `crates/lemnos-registry`: driver registration, ranking, and selection
- `crates/lemnos-runtime`: embeddable runtime for refresh, bind, and state
- `crates/lemnos-linux`: Linux discovery, transports, hotplug, and embedded-hal implementations (i2c-dev, spidev, GPIO v2), in pure Rust
- `crates/lemnos-linux-sys`: the only crate with `unsafe`: Linux uAPI structs and safe syscall wrappers
- `crates/lemnos-drivers-*`: built-in generic runtime drivers for common bus classes
- `crates/lemnos-drivers-vcm`: `no_std` voice-coil motor (camera focus) drivers over embedded-hal
- `crates/lemnos-drivers-bmi088`, `crates/lemnos-drivers-bmm150`, `crates/lemnos-drivers-ina2xx`: `no_std` IMU, magnetometer and power monitor drivers over embedded-hal
- `crates/lemnos-macros`: proc macros for configured devices and driver boilerplate
- `crates/lemnos-mock`: fake hardware for tests and examples

Additional repository notes live under [docs/README.md](docs/README.md).

## Getting Started

Add the facade crate for most applications:

```toml
[dependencies]
lemnos = "2.0.0"
```

Typical feature sets:

- `builtin-drivers`: bundles the generic GPIO/PWM/I2C/SPI/UART/USB drivers
- `linux`: enables the Linux backend and Linux-specific feature flags
- `macros`: re-exports `lemnos-macros`
- `mock`: enables mock hardware support for tests and examples
- `tokio`: enables the async runtime surface
- `full`: enables the common bundled experience

Example:

```toml
[dependencies]
lemnos = { version = "2.0.0", features = ["builtin-drivers", "linux", "macros"] }
```

## Device Drivers On embedded-hal

Device drivers depend on `lemnos-hal` (or plain `embedded-hal`) only and build without `std`:

```rust
use lemnos_hal::{AddressWidth, I2cRegisters, RegisterBus};

// On Linux: lemnos::linux::hal::I2cBus::open(10)?; on an MCU: the HAL's I2C.
fn chip_id<I: lemnos_hal::i2c::I2c>(i2c: I) -> Option<u32> {
    I2cRegisters::new(i2c, 0x60, AddressWidth::Bits16).read(0x300a, 2).ok()
}
```

The drivers also implement the compact device model (`lemnos-device`), so generic code reads any of them the same way, and `lemnos-lite` keeps a static table of them with status and polling:

```rust
let mut imu = lemnos_drivers_bmi088::Bmi088::new(i2c);
let mut devices = lemnos_lite::Devices::new([lemnos_lite::sensor("imu", &mut imu).every(10)]);
devices.init_all(&mut delay);
devices.poll(now_ms, &mut delay, &mut buf, |r| { /* r.info.channels, r.values */ });
```

Inside the runtime, `lemnos::bus::hal` turns any session into an embedded-hal bus or pin, so the same drivers run on whatever backend the runtime opened.

## Examples

The facade crate includes examples for both mock and Linux-backed flows:

- `cargo run -p lemnos --example mock_gpio --features "mock builtin-drivers"`
- `cargo run -p lemnos --example mock_gpio_async --features "mock builtin-drivers tokio"`
- `cargo run -p lemnos --example linux_led_class_driver --features "linux"`
- `cargo run -p lemnos --example linux_device_validator --features "builtin-drivers linux macros"`
- `cargo run -p lemnos --example linux_hal_probe --features linux -- discover` (also `gpio-info`, `gpio-read`, `i2c-read`, `hotplug`)

## Development

Common workspace commands:

```bash
./scripts/repo-clean.sh
./scripts/check-file-sizes.sh
cargo test --workspace
cargo clippy --workspace --all-targets --all-features
cargo doc --workspace --no-deps
./scripts/check-nostd.sh   # no_std crates for thumbv7em, riscv32imac, wasm32
```

Targeted helper scripts live under `testing/`.

## Documentation Index

- [docs/README.md](docs/README.md): repo documentation index
- [docs/development.md](docs/development.md): repo layout, commands, and validation conventions
- [docs/architecture-crate-map.md](docs/architecture-crate-map.md): crate responsibilities and relationships
- [docs/testing.md](docs/testing.md): test surfaces, scripts, and example validation flows
- [docs/foundation.md](docs/foundation.md): Lemnos 2.0 layering, embedded-hal foundation, and the Styx migration
- [CHANGELOG.md](CHANGELOG.md): release history and notable workspace changes
- [testing/README.md](testing/README.md): local and CI validation entry points
- [scripts/ci.sh](scripts/ci.sh): shared local CI entry point
- [scripts/repo-clean.sh](scripts/repo-clean.sh): pre-commit cleanup and verification entry point

## License

Licensed under either:

- Apache-2.0
- MIT

at your option.
