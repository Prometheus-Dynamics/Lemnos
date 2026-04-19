# lemnos-registry

`lemnos-registry` handles driver registration and match selection.

## Scope

This crate provides:

- `DriverRegistry`
- candidate and summary types
- driver IDs
- ranking and reporting helpers
- registry error types

It sits between discovery output and runtime binding, deciding which driver should own a discovered device.
