#!/usr/bin/env bash
# The linker of the emulation runs in check-blob.sh: clang driving the toolchain's rust-lld, for a static Linux
# target named by LINK_TARGET. Rust passes the C library and the startup files (its self-contained musl set), so
# clang adds none of its own (-nostdlib). Two arguments change for the driver: a bare `-z` option (Rust passes these
# for the GNU linker) goes behind `-Wl,`, and `-zmax-page-size` is dropped, since it does not change what runs.
# LD_LLD is an `ld.lld` link to the toolchain's rust-lld (check-blob.sh makes it); clang wants that name.
set -euo pipefail
LLD="${LD_LLD:?LD_LLD names the ld.lld link to rust-lld}"
args=()
for arg in "$@"; do
  case "$arg" in
    -zmax-page-size=*) ;;
    -z*) args+=("-Wl,$arg") ;;
    *) args+=("$arg") ;;
  esac
done
exec clang --target="$LINK_TARGET" -static -nostdlib -fuse-ld=lld --ld-path="$LLD" "${args[@]}"
