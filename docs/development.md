# Development

Lemnos follows the shared Prometheus Dynamics workspace layout:

- `crates/`: facade, runtime, platform, driver, and support crates
- `docs/`: repository-level guidance
- `testing/`: host, device, and Docker validation assets
- `.github/workflows/`: GitHub Actions pipelines

## Validation Surface

Use these commands for the default local validation loop:

```bash
./scripts/repo-clean.sh
cargo fmt --check
./scripts/check-file-sizes.sh
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo doc --workspace --no-deps
```

Feature-matrix and Docker-specific validation are documented in [`testing/README.md`](../testing/README.md).

## Tooling

- Rust toolchain is pinned in [`rust-toolchain.toml`](../rust-toolchain.toml)
- Root dependency versions are aligned in [`Cargo.toml`](../Cargo.toml)
- Local validation entrypoint lives in [`scripts/ci.sh`](../scripts/ci.sh)
- Local cleanup entrypoint lives in [`scripts/repo-clean.sh`](../scripts/repo-clean.sh)
- CI entrypoints live in [`.github/workflows`](../.github/workflows)
