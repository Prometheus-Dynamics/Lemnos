#!/usr/bin/env bash
# The feature sets the workspace tests never build. `cargo test --workspace` unifies each crate's
# features across every member, test features included, so a crate is tested only in the union of
# what anything asks of it. Two kinds of set fall outside that union, and each gets its own check:
#
#   1. Shipped. The set each workspace crate has in the shipped programs (packaging/shipped.txt,
#      `service` profile, built together; check-products.sh holds that solo builds agree). Where it
#      differs from the set the workspace tests use, the crate's library tests run in exactly that
#      set. Cargo's unit graph must show the test build resolving to that set: if a dev-dependency
#      pulls a feature back in, the step fails rather than test a different set. Each crate answers
#      for its own code (a dependency whose set differs is a line of its own). A test that reaches
#      the same code of the crate in both sets ran it in the workspace tests, so only the tests
#      that reach a difference run: the crate's tests are built alone in both sets with their LLVM
#      IR, and when every dependency resolves the same in the two, the IR is compared function by
#      function (scripts/feature-tests.py says how). When the dependencies differ, all run.
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
            if fs not in tested.get(name, set()) and len(tested.get(name, ())) == 1:
                (ts,) = tested[name]
                print(name, ",".join(sorted(fs)) or "-", ",".join(sorted(ts)) or "-")
            elif fs not in tested.get(name, set()):
                print(name, ",".join(sorted(fs)) or "-", "?")
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

# test_unit <crate>: reads a unit graph, prints the crate's library test unit as one line:
# its features, then each dependency unit (package, mode, features) it is built against.
test_unit() {
  python3 -c 'import json,sys
g=json.load(sys.stdin)["units"]; n=sys.argv[1].replace("-","_")
(t,)=[u for u in g if u["mode"]=="test" and u["target"]["name"]==n and u["target"]["kind"][0] in ("lib","rlib")]
seen,todo,deps=set(),[d["index"] for d in t["dependencies"]],[]
while todo:
    i=todo.pop()
    if i in seen: continue
    seen.add(i); u=g[i]; deps.append(u["pkg_id"]+" "+u["mode"]+" "+",".join(u["features"]))
    todo+=[d["index"] for d in u["dependencies"]]
print(",".join(sorted(set(t["features"])-{"default"})) or "-", *sorted(deps), sep=" | ")' "$1"
}

# ir_build <crate> <features>: the crate's library tests built once, with their LLVM IR (one
# codegen unit, which --emit gives). Prints the test program's path; its IR is beside it, from
# the same compile. A program cargo holds as current but whose IR is gone is removed and built
# again, so the two always come from one compile.
ir_build() {
  local feat=() bin try
  [[ $2 == - ]] || feat=(--features "$2")
  for try in 1 2; do
    bin=$(cargo rustc -q --locked -p "$1" --lib --profile test --no-default-features "${feat[@]}" \
      --message-format json-render-diagnostics -- --emit=llvm-ir,link </dev/null |
      python3 -c 'import json,sys
for l in sys.stdin:
    m=json.loads(l)
    if m.get("reason")=="compiler-artifact" and m.get("executable") and m["target"]["name"]==sys.argv[1].replace("-","_"): print(m["executable"])' "$1")
    [[ -n $bin && -f $bin.ll && $bin.ll -nt $bin.d ]] && break
    rm -f -- "$bin"
  done
  echo "$bin"
}

failed=0
if [[ $part != options ]]; then
  while read -r crate features tested_set; do
    feat=()
    [[ $features == - ]] || feat=(--features "$features")
    shipped_unit=$(graph test -p "$crate" --lib --no-default-features "${feat[@]}" </dev/null | test_unit "$crate")
    if [[ ${shipped_unit%% |*} != "$features" ]]; then
      echo "check-features: $crate ships with [$features] but its tests alone build [${shipped_unit%% |*}]" >&2
      failed=1
      continue
    fi
    # The workspace tests ran this crate's own code in [$tested_set]. This step answers for the
    # crate's own code in its shipped set; each dependency answers for its own (it is a line of
    # this step when its shipped set differs, and the workspace tests run it otherwise). So the
    # crate's tests are built alone in both sets: when every dependency resolves the same in the
    # two, only the crate's own code differs, and its IR is compared. A test that reaches none of
    # the difference ran the same code of this crate in the workspace tests, and is not run again;
    # the rest run (scripts/feature-tests.py). When the dependencies differ, all run.
    tfeat=()
    [[ $tested_set == - ]] || tfeat=(--features "$tested_set")
    solo_unit=$(graph test -p "$crate" --lib --no-default-features "${tfeat[@]}" </dev/null | test_unit "$crate")
    if [[ $tested_set != "?" && ${solo_unit%% |*} == "$tested_set" && ${shipped_unit#* |} == "${solo_unit#* |}" ]]; then
      shipped_bin=$(ir_build "$crate" "$features") && tested_bin=$(ir_build "$crate" "$tested_set") &&
        [[ -x $shipped_bin && -f ${shipped_bin}.ll && -f ${tested_bin}.ll ]] || {
        echo "check-features: $crate: no IR build" >&2; failed=1; continue; }
      "$shipped_bin" --list --format terse >"$work/tests.txt" </dev/null
      scripts/feature-tests.py "$crate" "${shipped_bin}.ll" "${tested_bin}.ll" "$work/tests.txt" >"$work/selected.txt" 2>"$work/why.txt" || {
        cat "$work/why.txt" >&2; failed=1; continue; }
      echo "    $crate, as shipped [$features]: $(cat "$work/why.txt")"
      if [[ -s $work/selected.txt ]]; then
        mapfile -t selected <"$work/selected.txt"
        # Run as cargo test runs it: from the package's directory, with its manifest directory set.
        dir=$(python3 -c 'import json,sys
print([p for p in json.load(open(sys.argv[1]))["packages"] if p["name"]==sys.argv[2]][0]["manifest_path"].rsplit("/",1)[0])' "$work/meta.json" "$crate")
        (cd "$dir" && CARGO_MANIFEST_DIR="$dir" "$shipped_bin" -q --exact "${selected[@]}" </dev/null) || failed=1
      fi
    else
      echo "    $crate, as shipped [$features]: its dependencies resolve differently in [$tested_set], all tests run"
      cargo test -q --locked -p "$crate" --lib --no-default-features "${feat[@]}" </dev/null || failed=1
    fi
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
