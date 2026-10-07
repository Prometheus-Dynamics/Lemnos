# Cross-builds lemnosd for aarch64-unknown-linux-musl as a static binary,
# linked by rust-lld with Rust's self-contained musl (no C cross toolchain).
FROM rust:1.99-slim
RUN rustup target add aarch64-unknown-linux-musl
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C target-feature=+crt-static -C link-self-contained=yes -C link-arg=-zmax-page-size=0x10000"
