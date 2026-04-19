# lemnos-drivers-gpio

`lemnos-drivers-gpio` provides the built-in generic GPIO driver for Lemnos.

## Scope

This crate exports:

- `GpioDriver`
- a companion `manifest()` helper

It is intended for the `builtin-drivers` feature path in the facade crate and for tests that need a generic GPIO implementation.
