# Lemnos

Lemnos is a Rust workspace for hardware discovery, driver matching, and runtime interaction across GPIO, PWM, I2C, SPI, UART, and USB surfaces.

The repository is split into small crates so applications, custom drivers, Linux backends, built-in generic drivers, and test helpers can evolve independently.

## Workspace Layout

- `crates/lemnos`: consumer-facing facade and builder API
- `crates/lemnos-core`: shared types, requests, state, and descriptors
- `crates/lemnos-bus`: typed bus/session traits for hardware access
- `crates/lemnos-discovery`: discovery probes, inventory snapshots, and diffing
- `crates/lemnos-driver-manifest`: driver metadata and matching rules
- `crates/lemnos-driver-sdk`: driver authoring helpers and bind-time utilities
- `crates/lemnos-registry`: driver registration, ranking, and selection
- `crates/lemnos-runtime`: embeddable runtime for refresh, bind, and state
- `crates/lemnos-linux`: Linux discovery and transport backends
- `crates/lemnos-drivers-*`: built-in generic drivers for common bus classes
- `crates/lemnos-macros`: proc macros for configured devices and driver boilerplate
- `crates/lemnos-mock`: fake hardware for tests and examples

Additional repository notes live under [docs/README.md](docs/README.md).

## Getting Started

Add the facade crate for most applications:

```toml
[dependencies]
lemnos = "1.0.0"
```

Typical feature sets:

- `builtin-drivers`: bundles the generic GPIO/PWM/I2C/SPI/UART/USB drivers
- `linux`: enables the Linux backend and Linux-specific feature flags
- `macros`: re-exports `lemnos-macros`
- `mock`: enables mock hardware support for tests and examples
- `tokio`: enables the async runtime surface
- `full`: enables the common bundled experience

Example:

```toml
[dependencies]
lemnos = { version = "1.0.0", features = ["builtin-drivers", "linux", "macros"] }
```

## Examples

The facade crate includes examples for both mock and Linux-backed flows:

- `cargo run -p lemnos --example mock_gpio --features "mock builtin-drivers"`
- `cargo run -p lemnos --example mock_gpio_async --features "mock builtin-drivers tokio"`
- `cargo run -p lemnos --example linux_led_class_driver --features "linux"`
- `cargo run -p lemnos --example linux_device_validator --features "builtin-drivers linux macros"`

## Development

Common workspace commands:

```bash
./scripts/repo-clean.sh
./scripts/check-file-sizes.sh
cargo test --workspace
cargo clippy --workspace --all-targets --all-features
cargo doc --workspace --no-deps
```

Targeted helper scripts live under `testing/`.

## Documentation Index

- [docs/README.md](docs/README.md): repo documentation index
- [docs/development.md](docs/development.md): repo layout, commands, and validation conventions
- [docs/architecture-crate-map.md](docs/architecture-crate-map.md): crate responsibilities and relationships
- [docs/testing.md](docs/testing.md): test surfaces, scripts, and example validation flows
- [CHANGELOG.md](CHANGELOG.md): release history and notable workspace changes
- [testing/README.md](testing/README.md): local and CI validation entry points
- [scripts/ci.sh](scripts/ci.sh): shared local CI entry point
- [scripts/repo-clean.sh](scripts/repo-clean.sh): pre-commit cleanup and verification entry point

## License

Licensed under either:

- Apache-2.0
- MIT

at your option.
