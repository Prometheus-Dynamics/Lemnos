# lemnos-bus

`lemnos-bus` defines the typed bus backend and session contracts used by Lemnos drivers and runtimes.

## Scope

This crate contains:

- backend traits for bus providers
- session traits for claimed or shared device access
- bus-specific request and stream types
- shared bus error types

## Bus Surfaces

- GPIO
- PWM
- I2C
- SPI
- UART
- USB

Most applications will use this crate indirectly through `lemnos` or `lemnos-driver-sdk`, but custom backends and advanced drivers may use it directly.
