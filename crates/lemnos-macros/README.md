# lemnos-macros

`lemnos-macros` provides proc macros that reduce driver and model boilerplate.

## Provided Macros

- `#[driver]`
- `#[interaction]`
- `#[enum_values(...)]`
- `#[derive(ConfiguredDevice)]`
- `#[derive(LemnosResource)]`
- `#[derive(LemnosDriver)]`

## Typical Use

This crate is most useful when paired with `lemnos-driver-sdk` and `lemnos-driver-manifest` for custom drivers and configured-device definitions.
