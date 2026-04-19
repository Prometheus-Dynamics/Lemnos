# Testing

Lemnos splits validation into default workspace checks, feature-matrix coverage, and Docker-backed example validation.

## Default Surface

- `./scripts/check-file-sizes.sh`
- `cargo fmt --check`
- `cargo test --workspace`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`

## Feature Matrix

The default CI workflow also checks targeted feature sets for `lemnos` and `lemnos-core` so feature combinations stay coherent.

## Docker Surface

- `cargo test -p lemnos --test docker_facade_examples -- --ignored --nocapture`

The Docker suite uses [`testing/docker/lemnos-facade.Dockerfile`](docker/lemnos-facade.Dockerfile).

File-size linting is warning-only, supports `FILE_SIZE_EXCLUDE_DIRS=path1:path2`, and tracks current exceptions through `testing/ci/file-size-baseline.txt`.
