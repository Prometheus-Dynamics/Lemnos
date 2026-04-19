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

## Practical Workflow

For day-to-day changes:

1. Run targeted crate tests first.
2. Run the relevant `lemnos` example when changing facade, runtime, Linux, or mock flows.
3. Run `cargo test --workspace` before publishing or cutting a release.
