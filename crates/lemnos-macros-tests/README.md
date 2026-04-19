# lemnos-macros-tests

`lemnos-macros-tests` is an internal test crate for macro behavior.

## Scope

This crate contains:

- integration tests for configured-device and driver code generation
- compile-fail coverage built on `trybuild`

## Notes

- `publish = false`
- intended for workspace verification only

Most users do not need this crate directly, but it is part of the repository's macro safety net.
