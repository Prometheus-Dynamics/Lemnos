# lemnos-drivers-i2c

`lemnos-drivers-i2c` provides the built-in generic I2C driver for Lemnos.

## Scope

This crate exports:

- `I2cDriver`
- a companion `manifest()` helper

It is primarily consumed through `lemnos` with the `builtin-drivers` feature enabled.
