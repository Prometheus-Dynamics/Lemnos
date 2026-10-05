# Testing

Lemnos has three main testing surfaces: unit and integration tests in each crate, mock-hardware examples, and Linux-oriented validation helpers.

## Cargo Test Surface

Run the full workspace:

```bash
cargo test --workspace
```

Important coverage areas:

- crate-local tests under `crates/*/src/tests.rs`
- integration tests such as `crates/lemnos/tests/*`
- macro compile-time coverage in `crates/lemnos-macros-tests/tests/*`

## no_std Builds

`./scripts/check-nostd.sh` builds and lints the `no_std` crates (`lemnos-hal`, `lemnos-core`, `lemnos-driver-manifest` and the device driver crates, with and without `float`) for `thumbv7em-none-eabihf`, `riscv32imac-unknown-none-elf` and `wasm32-unknown-unknown`. Their host tests run against `lemnos-hal`'s embedded-hal mocks.

`./scripts/check-sizes.sh` tracks what Lemnos costs small targets: bare-metal firmware images using the device drivers (`testing/size/firmware`, Cortex-M4F and FPU-less RISC-V, with and without `float`) and stripped, size-optimized Linux binaries using the facade (`testing/size/linux`). It compares them with `testing/size/baseline.txt` and fails when an image grows beyond noise (max of 64 bytes and 1%; 5% for Linux binaries, whose linker and libc vary by host). There are no absolute budgets: when a change grows an image on purpose, or shrinks it, run `./scripts/check-sizes.sh --update` and commit the new baseline with the change.

## Mock-Based Validation

The `lemnos` examples exercise realistic flows without touching host hardware:

- `mock_gpio`
- `mock_gpio_async`
- `mock_gpio_explicit_backend`
- `mock_usb_hotplug`
- `mock_bmi088_driver`
- `mock_bmm150_driver`
- `mock_ina226_driver`
- `mock_power_sensor_driver`

These examples are good smoke tests for the facade, runtime, registry, driver SDK, macros, and mock backend working together.

## Linux-Oriented Helpers

Repository helper assets live under `testing/`:

- `testing/host/discover-runtime-proof-targets.sh`
- `testing/host/run-runtime-host-proofs.sh`
- `testing/device/run-linux-device-validator.sh`
- `testing/docker/lemnos-facade.Dockerfile`
- `testing/device/*.env`

The `linux_device_validator` example under `crates/lemnos/examples/` wires together Linux probes plus example drivers and is the main end-to-end Linux validation entry point.

`linux_hal_probe` exercises the pure-Rust Linux layer on a board: `discover` (inventory), `gpio-info [chip]` (GPIO v2 chip and line info), `gpio-read CHIP OFFSET` (reads a line without changing it), `i2c-read BUS ADDR REG LEN [8|16]` (register read; claims the address, never forces), `hotplug SECONDS` (prints watch events and the source in use). Cross-compile with `--target aarch64-unknown-linux-gnu`. The `lemnos-linux-sys` and `lemnos-linux` test binaries also run on the target (struct layouts, present chips and buses, the uevent socket).

## Practical Workflow

For day-to-day changes:

1. Run targeted crate tests first.
2. Run the relevant `lemnos` example when changing facade, runtime, Linux, or mock flows.
3. Run `cargo test --workspace` before publishing or cutting a release.
