# lemnos-runtime

`lemnos-runtime` provides the embeddable runtime used by the facade crate.

## Scope

This crate contains:

- refresh and rebind orchestration
- runtime configuration
- diagnostic and failure records
- event subscription and polling
- optional async runtime support

## Features

- `gpio-cdev`: forwards GPIO character-device support into the Linux layer
- `tracing`: enables tracing-aware runtime paths
- `tokio`: enables the async runtime surface

Applications that need lower-level control than `lemnos` can integrate this crate directly.
