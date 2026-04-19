# lemnos-driver-sdk

`lemnos-driver-sdk` is the main driver authoring surface for Lemnos.

## Scope

This crate contains:

- the `Driver` and `BoundDevice` traits
- bind contexts for opening typed sessions
- bus-specific IO helpers for GPIO, PWM, I2C, SPI, UART, and USB
- matching helpers and generic manifest helpers
- conformance utilities for driver testing
- state and telemetry helper functions

## When To Use It

Use this crate when writing built-in drivers, out-of-tree drivers, or custom bound-device implementations.

For less boilerplate, pair it with `lemnos-driver-manifest` and `lemnos-macros`.
