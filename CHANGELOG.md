# Changelog

All notable changes to this workspace should be documented in this file.

The format is based on Keep a Changelog and this project follows Semantic Versioning.

## [1.0.0] - 2026-04-19

- Standardized the workspace around a shared root layout, toolchain, lint policy, and CI shape.
- Added repo-level development and testing guides plus Docker-backed facade validation.
- Added `scripts/check-file-sizes.sh`, `scripts/ci.sh`, and `scripts/repo-clean.sh`.
- Aligned dependency policy around `thiserror` for library errors and `tracing` for instrumentation.
