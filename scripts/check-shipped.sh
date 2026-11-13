#!/usr/bin/env bash
# No C in the shipped programs. For each program in packaging/shipped.txt, its normal and build
# dependency tree (including build scripts) must hold no crate that compiles C or runs a C
# generator, and no `-sys` crate from outside the workspace (a foreign `-sys` crate is where C
# usually comes from; each one needs review before it is allowed here). The lock is taken from
# Cargo.lock when it is committed; when there is none, one is resolved locally first.
# Also checks that docs/building.md's table names each shipped program.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

# Crates that compile C, run a C compiler or generate C bindings at build time.
forbidden='^(cc|cmake|bindgen|bindgen-cli|pkg-config|vcpkg|autotools|cbindgen|cxx|cxx-build|system-deps|cmake-rs|pkgconfig)$'

[[ -f Cargo.lock ]] || cargo metadata --format-version 1 >/dev/null

# Foreign -sys crates reviewed and allowed: generated Rust bindings to the Linux uapi, with no build
# script (rustix's default backend, through std). Nothing else is allowed without review.
allowed_sys='^linux-raw-sys$'

# The workspace's own crates: their directories (drivers/*-sys, backends/linux-sys, ...).
workspace=$(cargo metadata --locked --format-version 1 --no-deps |
  python3 -c 'import json,sys; [print(p["name"]) for p in json.load(sys.stdin)["packages"]]')

failed=0
while read -r package binary targets; do
  [[ -z "$package" || "$package" == \#* ]] && continue
  tree=$(cargo tree --locked -e normal,build -p "$package" --prefix none --format '{p}' |
    awk '{print $1}' | sort -u)
  pulled=$(grep -E "$forbidden" <<<"$tree" || true)
  if [[ -n "$pulled" ]]; then
    echo "$package ($binary) pulls in C tooling: $(tr '\n' ' ' <<<"$pulled")" >&2
    failed=1
  fi
  while read -r crate; do
    [[ "$crate" == *-sys ]] || continue
    grep -qx "$crate" <<<"$workspace" && continue
    grep -qE "$allowed_sys" <<<"$crate" && continue
    echo "$package ($binary): foreign -sys crate $crate; review it for C before allowing it" >&2
    failed=1
  done <<<"$tree"
  if ! grep -q "\`$package\`" docs/building.md || ! grep -q "\`$binary\`" docs/building.md; then
    echo "docs/building.md does not list $package ($binary) in its table" >&2
    failed=1
  fi
  echo "    $package ($binary): $(wc -l <<<"$tree") crates, no C tooling"
done < packaging/shipped.txt

[[ $failed == 0 ]] || { echo "check-shipped: failed; see above" >&2; exit 1; }
echo "check-shipped: holds"
