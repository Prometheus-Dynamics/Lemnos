# lemnos-driver-manifest

`lemnos-driver-manifest` defines driver metadata and compatibility rules.

## Scope

This crate provides:

- `DriverManifest`
- interaction manifests
- driver version metadata
- match rules and match conditions
- validation and manifest error types

## Features

- `serde`: enables manifest serialization support

This crate is the contract between discovery output and driver selection.
