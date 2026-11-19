#!/usr/bin/env bash
# The feature sets the workspace tests never build. `cargo test --workspace` unifies each crate's
# features across every member, test features included, so a crate is tested only in the union of
# what anything asks of it. Two kinds of set fall outside that union, and each gets its own check:
#
#   1. Shipped. The set each workspace crate has in the shipped programs (packaging/shipped.txt,
#      `service` profile, built together; check-products.sh holds that solo builds agree). Where it
#      differs from the set the workspace tests use, the crate's library tests run in exactly that
#      set. Before they run, cargo's unit graph must show the test build resolving to that set:
#      if a dev-dependency pulls a feature back in, the step fails rather than test a different set.
#   2. Library options. Every member that declares features is linted (clippy --all-targets,
#      -D warnings) and its library built with --no-default-features and with --all-features,
#      unless that set is the one the workspace lint already covers. No test pass: no shipped
#      build uses these sets.
#
# The sets come from `cargo build --unit-graph` (unstable, so RUSTC_BOOTSTRAP is set, as in
# check-products.sh; the toolchain is pinned in rust-toolchain.toml). Host target.
# PART=shipped or PART=options runs one half; LIST=1 prints the sets and runs nothing.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
work=$(mktemp -d "${TMPDIR:-/tmp}/check-features.XXXXXX")
trap 'rm -rf "$work"' EXIT
part="${PART:-all}"

graph() { RUSTC_BOOTSTRAP=1 cargo "$@" --locked --unit-graph -Z unstable-options; }

cargo metadata --locked --format-version 1 --no-deps >"$work/meta.json"
args=()
while read -r package _; do
  [[ -z "$package" || "$package" == \#* ]] && continue
  args+=(-p "$package")
done <packaging/shipped.txt
graph build --profile service "${args[@]}" >"$work/shipped.json"
graph test --workspace --no-run >"$work/tested.json"

# Prints "<package> <features, comma-separated or ->" for each workspace library whose shipped set
# differs from its tested set (sets: ...-shipped), or each set to lint (sets: ...-options).
sets() {
  python3 - "$work" "$1" <<'PY'
import json, sys
work, which = sys.argv[1], sys.argv[2]
meta = json.load(open(f"{work}/meta.json"))
members = {p["id"]: p for p in meta["packages"]}

def lib_features(path, modes):
    out = {}
    for unit in json.load(open(path))["units"]:
        if unit["pkg_id"] in members and unit["mode"] in modes and unit["target"]["kind"][0] in ("lib", "rlib"):
            # `default` is a name for other features; the sets compare by what they turn on.
            out.setdefault(members[unit["pkg_id"]]["name"], set()).add(frozenset(unit["features"]) - {"default"})
    return out

shipped = lib_features(f"{work}/shipped.json", ("build",))
tested = lib_features(f"{work}/tested.json", ("test",))
linted = lib_features(f"{work}/tested.json", ("build", "check", "test"))

if which == "shipped":
    for name in sorted(shipped):
        for fs in shipped[name]:
            if fs not in tested.get(name, set()):
                print(name, ",".join(sorted(fs)) or "-")
else:
    for p in sorted(members.values(), key=lambda p: p["name"]):
        feats = p["features"]
        if not feats or not any(set(t["kind"]) & {"lib", "rlib"} for t in p["targets"]):
            continue
        def closure(start):
            seen, todo = set(), list(start)
            while todo:
                f = todo.pop()
                if f in seen or f not in feats:
                    continue
                seen.add(f)
                todo += [d for d in feats[f] if "/" not in d and not d.startswith("dep:")]
            return frozenset(seen) - {"default"}
        for flag, fs in (("--no-default-features", closure([])), ("--all-features", closure(feats))):
            if fs not in linted.get(p["name"], set()):
                print(p["name"], flag)
PY
}

if [[ ${LIST:-} == 1 ]]; then
  sets shipped | sed 's/^/shipped: /'
  sets options | sed 's/^/option: /'
  exit 0
fi

failed=0
if [[ $part != options ]]; then
  while read -r crate features; do
    feat=()
    [[ $features == - ]] || feat=(--features "$features")
    got=$(graph test -p "$crate" --lib --no-default-features "${feat[@]}" </dev/null |
      python3 -c 'import json,sys
n=sys.argv[1].replace("-","_")
print(",".join(sorted({f for u in json.load(sys.stdin)["units"] if u["mode"]=="test" and u["target"]["name"]==n for f in u["features"]} - {"default"})) or "-")' "$crate")
    if [[ $got != "$features" ]]; then
      echo "check-features: $crate ships with [$features] but its tests alone build [$got]" >&2
      failed=1
      continue
    fi
    echo "    $crate, as shipped [$features]"
    cargo test -q --locked -p "$crate" --lib --no-default-features "${feat[@]}" </dev/null || failed=1
  done < <(sets shipped)
fi
if [[ $part != shipped ]]; then
  while read -r crate flag; do
    echo "    $crate $flag"
    cargo clippy -q --locked -p "$crate" "$flag" --all-targets -- -D warnings </dev/null || failed=1
    cargo build -q --locked -p "$crate" "$flag" --lib </dev/null || failed=1
  done < <(sets options)
fi
[[ $failed == 0 ]] || { echo "check-features: failed; see above" >&2; exit 1; }
echo "check-features: every shipped set is tested and every library option builds clean"
