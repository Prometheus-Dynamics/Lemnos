#!/usr/bin/env bash
# The gate's overlay check: the generated overlays are compiled with the device-tree compiler the
# contract names (contracts/overlay-dtc.toml, `minimum_dtc`), built from its pinned release tarball by
# testing/devtree/build-dtc.sh, and applied with that release's fdtoverlay and fdtget.
#
#   check.sh                    (DTC_DIR: where the build goes; default target/dtc)
#
# The consumer's overlay step compiles with the same release (its host dtc is 1.7.2). A newer dtc
# can lack a class the contract lists (1.8.1 has no graph_child_address), so this check never runs
# a newer one: the newer one is probed by hand and the result goes in the PR.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
target="${CARGO_TARGET_DIR:-$repo/target}"
dtc_dir="${DTC_DIR:-$target/dtc}"
version="$(python3 -c 'import tomllib,sys;print(tomllib.load(open(sys.argv[1],"rb"))["minimum_dtc"])' "$repo/contracts/overlay-dtc.toml")"
src="$(bash "$here/build-dtc.sh" "$dtc_dir" "$version")"
export PATH="$src:$PATH"
export LEMNOS_DTC="$src/dtc"
cd "$repo"
cargo test -q -p lemnos-devtree --test apply
echo "devtree overlay check: passed with dtc $version"
