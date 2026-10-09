# Cross-builds lemnosd for aarch64-unknown-linux-musl as a static binary,
# linked by rust-lld with Rust's self-contained musl (no C cross toolchain).
# The linker settings live in the repository's .cargo/config.toml, so the
# same build runs on any host with the musl target installed (see
# lemnosd-host.toml); this image only provides that toolchain.
FROM rust:1.99-slim
RUN rustup target add aarch64-unknown-linux-musl
