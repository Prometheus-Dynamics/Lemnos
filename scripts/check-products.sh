#!/usr/bin/env bash
# The shipped programs agree by design (service/shipped-features). lemnosd, lemnos-ctl, lemnos-orion
# and lemnos-slot-init, built in one cargo run, must be byte-identical to each built alone, and every
# crate they share must resolve to the same features in both. Cargo unifies features across the
# packages of one run, so a feature one program requests would reach the others only in the merged
# build; this fails on it, whether or not the binary bytes happen to change.
#
# Two checks, both on the `service` profile (what the images ship), host target:
#   1. features: `cargo build --unit-graph` (an unstable flag, so RUSTC_BOOTSTRAP is set; the
#      toolchain is pinned in rust-toolchain.toml) for the merged run and for each solo run. Each
#      compiled unit's features must match between the two, package by package.
#   2. bytes: the merged run's four binaries and each solo build's, compared with cmp.
# Cost: a cold build of the four programs with fat LTO; warm, a relink per program. The gate runs it.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
export CARGO_INCREMENTAL=0

products=(lemnosd lemnos-ctl lemnos-orion lemnos-slot-init)
args=()
for p in "${products[@]}"; do args+=(-p "$p"); done
work=$(mktemp -d "${TMPDIR:-/tmp}/check-products.XXXXXX")
trap 'rm -rf "$work"' EXIT

# 1. Features.
RUSTC_BOOTSTRAP=1 cargo build --locked --profile service --unit-graph -Z unstable-options "${args[@]}" >"$work/ug-merged.json"
for p in "${products[@]}"; do
  RUSTC_BOOTSTRAP=1 cargo build --locked --profile service --unit-graph -Z unstable-options -p "$p" >"$work/ug-solo-$p.json"
done
python3 - "$work" "${products[@]}" <<'PY'
import collections, json, sys
work, products = sys.argv[1], sys.argv[2:]

def units(path):
    out = collections.defaultdict(set)  # package id -> {(target kind, features)}
    for unit in json.load(open(path))["units"]:
        kind = unit["target"]["kind"][0]
        if unit["mode"] != "build" or kind not in ("lib", "rlib", "bin", "proc-macro", "custom-build"):
            continue
        out[unit["pkg_id"]].add((kind, tuple(sorted(unit["features"]))))
    return out

merged = units(f"{work}/ug-merged.json")
differing = 0
for p in products:
    solo = units(f"{work}/ug-solo-{p}.json")
    for pkg in sorted(solo):
        if pkg not in merged:
            print(f"features: {p}: {pkg} is not in the merged build", file=sys.stderr)
            differing += 1
        elif {f for _, f in solo[pkg]} != {f for _, f in merged[pkg]}:
            name = pkg.split("#")[-1]
            print(f"features: {p}: {name} solo {sorted({f for _, f in solo[pkg]})} "
                  f"merged {sorted({f for _, f in merged[pkg]})}", file=sys.stderr)
            differing += 1
if differing:
    sys.exit(f"check-products: {differing} unit(s) resolve to different features in a merged build")
print(f"    features: the four programs resolve the same features, merged and solo")
PY

# 2. Bytes. The merged run's binaries first, then each program built alone.
cargo build --locked --profile service "${args[@]}"
mkdir "$work/merged"
for p in "${products[@]}"; do cp "target/service/$p" "$work/merged/$p"; done
failed=0
for p in "${products[@]}"; do
  cargo build --locked --profile service -p "$p"
  cp "target/service/$p" "$work/solo-$p"
  if cmp -s "$work/merged/$p" "$work/solo-$p"; then
    echo "    $p: merged and solo identical ($(stat -c %s "$work/solo-$p") bytes)"
  else
    echo "check-products: $p differs between the merged and the solo build" >&2
    failed=1
  fi
done
[[ $failed == 0 ]] || { echo "check-products: failed; see above" >&2; exit 1; }
echo "check-products: the four programs agree"
