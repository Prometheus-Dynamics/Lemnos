# lemnos-core

`lemnos-core` holds the stable vocabulary shared across the workspace.

## Scope

This crate provides:

- device IDs, capability IDs, and issue codes
- device descriptors and control-surface metadata
- interface and device-kind enums
- interaction request and response types for all supported buses
- device state, operation records, events, and values

## Features

- `serde`: enables serialization support for the shared model types

If a type needs to move between discovery, drivers, runtime, and applications, it usually belongs here.
