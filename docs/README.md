# Documentation

This directory holds repository-level documentation for the Lemnos workspace.

## Guides

- [development.md](development.md): repository layout, validation commands, and CI expectations
- [architecture-crate-map.md](architecture-crate-map.md): explains how the workspace crates fit together
- [testing.md](testing.md): describes test entry points, helper scripts, and validation flows
- [foundation.md](foundation.md): Lemnos 2.0 layering (embedded-hal, `lemnos-hal`, one Linux implementation) and the Styx migration
- [board-definition.md](board-definition.md): board definitions (TOML/JSON), the built-in drivers, generating them, and HeliOS adoption
- [compact-model.md](compact-model.md): the `no_std` compact device model (`lemnos-device`), the lite table (`lemnos-lite`) and how the runtime consumes them

## Where To Start

- Building an application: start with the root [README.md](../README.md) and [`crates/lemnos/README.md`](../crates/lemnos/README.md)
- Writing a driver: read [`crates/lemnos-driver-sdk/README.md`](../crates/lemnos-driver-sdk/README.md), [`crates/lemnos-driver-manifest/README.md`](../crates/lemnos-driver-manifest/README.md), and [`crates/lemnos-macros/README.md`](../crates/lemnos-macros/README.md)
- Integrating Linux hardware: read [`crates/lemnos-linux/README.md`](../crates/lemnos-linux/README.md)
- Testing without real hardware: read [`crates/lemnos-mock/README.md`](../crates/lemnos-mock/README.md)
