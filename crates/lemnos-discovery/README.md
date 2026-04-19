# lemnos-discovery

`lemnos-discovery` provides probe-oriented discovery and inventory utilities.

## Scope

This crate includes:

- discovery context objects
- probe and enricher traits
- inventory snapshots and diffs
- run helpers for parallel probe execution
- probe reports and enrichment reports
- watch-event abstractions

## Features

- `test-utils`: enables fixture builders used in tests

Use this crate when implementing new discovery probes or when you need direct control over inventory collection outside the facade crate.
