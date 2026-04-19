# lemnos-mock

`lemnos-mock` provides fake hardware and scripted fault helpers for tests and examples.

## Scope

This crate includes:

- `MockHardware` and its builder
- mock GPIO lines
- mock PWM channels
- mock I2C, SPI, UART, and USB devices
- fault scripting helpers

Use it to exercise discovery, driver binding, and runtime flows without physical hardware.
