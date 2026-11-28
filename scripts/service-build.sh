#!/usr/bin/env bash
# Builds with the `service` profile (what the images ship) with the builder's paths mapped out of
# the binaries: the cargo home becomes /cargo and this checkout /lemnos in every path rustc writes
# (panic locations in registry, git and vendored crates). The shipped bytes are then the same
# whoever builds them, and in whatever home and directory (scripts/check-shipped-paths.sh checks
# two homes). Cargo's own trim-paths option would do this, but it is not stable on the pinned
# toolchain.
#
#   scripts/service-build.sh [cargo build arguments...]    e.g. --target aarch64-unknown-linux-musl -p lemnosd
#
# The maps go in through `--config` for every target, which cargo adds to the rustflags the
# configuration gives (.cargo/config.toml's musl link flags). RUSTFLAGS or CARGO_ENCODED_RUSTFLAGS
# in the environment replace every configured rustflag, so with either set this refuses: the maps
# would be lost, and the musl link flags with them.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
home="${CARGO_HOME:-$HOME/.cargo}"
if [ -n "${RUSTFLAGS:-}" ] || [ -n "${CARGO_ENCODED_RUSTFLAGS:-}" ]; then
  echo "service-build: RUSTFLAGS or CARGO_ENCODED_RUSTFLAGS is set; it would replace the path maps and the configured link flags" >&2
  exit 2
fi
maps=()
# The paths as given and as resolved, so a home or checkout reached through a symlink is mapped too.
for pair in "$home=/cargo" "$(cd "$home" 2>/dev/null && pwd -P || echo "$home")=/cargo" \
            "$root=/lemnos" "$(cd "$root" && pwd -P)=/lemnos"; do
  case " ${maps[*]-} " in *" \"--remap-path-prefix=$pair\" "*) ;; *) maps+=("\"--remap-path-prefix=$pair\"") ;; esac
done
flags="$(IFS=,; echo "${maps[*]}")"
cd "$root"
exec cargo build --locked --profile service --config "target.'cfg(all())'.rustflags=[$flags]" "$@"
