# Architecture Crate Map

Lemnos is intentionally layered. Higher-level crates depend on lower-level vocabulary and contracts rather than reaching directly across the workspace.

## Layering

1. `lemnos-core`
   Shared device descriptors, interaction requests and responses, state snapshots, events, issues, and value types.
2. `lemnos-bus`
   Bus-specific session traits and backend contracts for GPIO, PWM, I2C, SPI, UART, and USB access.
3. `lemnos-discovery`
   Discovery probes, inventory snapshots, probe reports, diffing, and watch events.
4. `lemnos-driver-manifest`
   Driver identity, compatibility rules, interaction manifests, and match metadata.
5. `lemnos-driver-sdk`
   Driver authoring surface: bind contexts, bus IO helpers, conformance harnesses, state helpers, and transport adapters.
6. `lemnos-registry`
   Driver registration and best-match selection.
7. `lemnos-runtime`
   Refresh, bind, runtime state, diagnostics, subscriptions, and optional async support.
8. `lemnos`
   Consumer-facing facade that re-exports the public building blocks and composes optional features.

## Backend And Driver Crates

- `lemnos-linux` implements Linux discovery probes, Linux transports, path handling, and optional hotplug watching.
- `lemnos-drivers-gpio`
- `lemnos-drivers-pwm`
- `lemnos-drivers-i2c`
- `lemnos-drivers-spi`
- `lemnos-drivers-uart`
- `lemnos-drivers-usb`

These built-in driver crates provide generic drivers and manifests for common interface kinds.

## Authoring And Test Support

- `lemnos-macros` reduces boilerplate for configured-device and driver definitions.
- `lemnos-mock` provides fake hardware for examples and tests.
- `lemnos-macros-tests` holds compile-time and integration coverage for macro behavior and is not published.

## Typical Flow

1. A backend or probe populates `lemnos-discovery` inventory using `lemnos-core` descriptors.
2. A driver manifest is matched against discovered devices.
3. The registry selects a candidate driver.
4. The runtime opens typed sessions through `lemnos-bus`.
5. The bound driver performs interactions and updates runtime state.
6. Applications use the `lemnos` facade or the lower-level crates directly, depending on how much control they need.
