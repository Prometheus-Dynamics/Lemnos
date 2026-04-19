# lemnos

`lemnos` is the consumer-facing facade crate for the workspace.

It bundles the discovery, registry, runtime, and optional backend or driver crates behind a simpler builder-oriented API intended for applications.

## What It Re-exports

- `lemnos::bus`
- `lemnos::core`
- `lemnos::discovery`
- `lemnos::driver`
- `lemnos::prelude`
- optional `lemnos::linux`
- optional `lemnos::mock`
- optional built-in drivers under `lemnos::drivers`

## Features

- `builtin-drivers`: enables the generic built-in driver crates
- `linux-backend`: enables `lemnos-linux`
- `linux`: enables the common Linux feature bundle
- `macros`: re-exports `lemnos-macros`
- `mock`: enables `lemnos-mock`
- `tracing`: forwards tracing support into runtime and Linux backend layers
- `tokio`: enables async runtime support
- `full`: enables the common bundled stack

## Examples

This crate contains the repository's primary runnable examples, including:

- mock GPIO flows
- async facade usage
- mock USB hotplug
- example out-of-tree sensor drivers
- Linux LED and hwmon fan drivers
- Linux device validation

Use `cargo run -p lemnos --example <name> --features "<features>"` to execute them.
