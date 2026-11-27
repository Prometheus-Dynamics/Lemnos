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
#      IR (unoptimised, so nothing is inlined), and when every dependency resolves the same in the
#      two, the IR is compared function by function (scripts/feature-tests.py says how); the tests
#      it selects run from the shipped set's test program as `cargo test` builds it. When the
#      dependencies differ, all run.
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

# mark <phase>: with FEATURES_TIMES=<file>, appends "<phase> <wall seconds>" since the last mark.
mark_at=$(date +%s.%N)
mark() {
  [[ -n ${FEATURES_TIMES:-} ]] || return 0
  local now
  now=$(date +%s.%N)
  echo "$1 $(python3 -c 'import sys; print(round(float(sys.argv[1]) - float(sys.argv[2]), 2))' "$now" "$mark_at")" >>"$FEATURES_TIMES"
  mark_at=$now
}

graph() { RUSTC_BOOTSTRAP=1 cargo "$@" --locked --unit-graph -Z unstable-options; }


# The cache: one entry per crate and form under the target directory, so a tree's target keeps it
# from one gate to the next. An entry holds the key of what decided its result (see
# scripts/features-cache.py): an unchanged key reuses the pass, and a changed one runs that crate
# again. Only a pass is stored; a crate that fails, or whose key changed, loses its entry first.
# Absolute: the graphs are copied from inside the work directory, where a relative path (CI leaves
# CARGO_TARGET_DIR unset, so it was target/check-features) names nothing.
mkdir -p "${CARGO_TARGET_DIR:-target}/check-features"
cache="$(cd "${CARGO_TARGET_DIR:-target}/check-features" && pwd)"
toolchain_line=$(rustc -vV | tr '\n\t' '  ')
scripts_hash_line=$(sha256sum scripts/check-features.sh scripts/feature-tests.py scripts/features-cache.py | tr '\n\t' '  ')
ckey() { # ckey <crate> <dev|nodev> <entry> <text>...
  scripts/features-cache.py key "$work/meta-full.json" "$1" "$2" "$3" "$toolchain_line" "$scripts_hash_line" "${@:4}"
}
# store <entry> <crate> <messages> <lines...>: writes a passing entry, keyed on the texts in
# ${ktexts[@]} (the same the lookup used) and the files the build read (from the messages' dep-info).
store() {
  local entry="$1" crate="$2" messages="$3" k
  shift 3
  { printf '%s\n' "$@"; scripts/features-cache.py record "$messages" "$work/meta-full.json" "$crate" dev | sed 's/^/extra /'; } >"$entry.new"
  k=$(ckey "$crate" dev "$entry.new" "${ktexts[@]}")
  { echo "key $k"; cat "$entry.new"; } >"$entry"
  rm -f "$entry.new"
}
# hit <entry> <key>: whether the entry is there with this key.
hit() { [[ -f $1 ]] && [[ $(head -1 "$1") == "key $2" ]]; }
args=()
while read -r package _; do
  [[ -z "$package" || "$package" == \#* ]] && continue
  args+=(-p "$package")
done <packaging/shipped.txt
# The two whole-workspace graphs depend only on the manifests, the lock, the configuration and the
# toolchain: they are kept under a key of those.
graphs_key=$( { rustc -vV; sha256sum scripts/check-features.sh;
  git ls-files -z -co --exclude-standard -- '*Cargo.toml' Cargo.lock .cargo/config.toml rust-toolchain.toml packaging/shipped.txt |
    xargs -0 sha256sum; env | grep -E '^(RUSTFLAGS|CARGO_ENCODED_RUSTFLAGS|CARGO_PROFILE_[A-Z0-9_]+)=' | sort || true; } | sha256sum)
graph_files=(meta.json meta-full.json shipped.json tested.json)
if [[ -f $cache/graphs.key && $(cat "$cache/graphs.key") == "$graphs_key" ]] &&
  (cd "$cache" && ls "${graph_files[@]}" >/dev/null 2>&1); then
  (cd "$cache" && cp "${graph_files[@]}" "$work/")
else
  rm -f "$cache/graphs.key"
  cargo metadata --locked --format-version 1 --no-deps >"$work/meta.json"
  cargo metadata --locked --format-version 1 >"$work/meta-full.json"
  graph build --profile service "${args[@]}" >"$work/shipped.json"
  graph test --workspace --no-run >"$work/tested.json"
  (cd "$work" && cp "${graph_files[@]}" "$cache/")
  echo "$graphs_key" >"$cache/graphs.key"
fi
mark graphs

# The shipped build's lemnosd has orion-precheck on: the shipped set embeds the Orion precheck, and
# the embedding consumers turn it off (service/lemnosd/Cargo.toml). Read from the shipped unit graph.
python3 -I - "$work/meta.json" "$work/shipped.json" <<'PY' || { echo "check-features: the shipped build's lemnosd lacks the orion-precheck feature" >&2; exit 1; }
import json, sys
meta, shipped = (json.load(open(p)) for p in sys.argv[1:3])
ids = {p["id"] for p in meta["packages"] if p["name"] == "lemnosd"}
units = [u for u in shipped["units"] if u["pkg_id"] in ids and u["mode"] == "build"]
on = [u for u in units if "orion-precheck" in u["features"]]
print(f"features: lemnosd in the shipped build has {'orion-precheck' if on else 'no orion-precheck'}")
sys.exit(0 if units and on else 1)
PY

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

# ir_build <crate> <features> <out.ll>: the crate's library tests compiled to LLVM IR only, with
# no optimisation and one codegen unit: every function as its source has it, nothing inlined,
# so the comparison sees each difference where it is. The output path is new each time, so cargo
# always runs rustc for it.
ir_build() {
  local feat=()
  [[ $2 == - ]] || feat=(--features "$2")
  cargo rustc -q --locked -p "$1" --lib --profile test --no-default-features "${feat[@]}" \
    --message-format json-render-diagnostics -- --emit="llvm-ir=$3" -C opt-level=0 -C codegen-units=1 \
    </dev/null >>"$work/messages.json"
  [[ -s $3 ]]
}

# test_build <crate> <features>: the crate's library test program as `cargo test` builds it; prints
# its path. The selected tests run from it.
test_build() {
  local feat=()
  [[ $2 == - ]] || feat=(--features "$2")
  cargo test -q --locked --no-run -p "$1" --lib --no-default-features "${feat[@]}" \
    --message-format json-render-diagnostics </dev/null >"$work/build.json"
  cat "$work/build.json" >>"$work/messages.json"
  python3 -c 'import json,sys
for l in open(sys.argv[2]):
    m=json.loads(l)
    if m.get("reason")=="compiler-artifact" and m.get("executable") and m["target"]["name"]==sys.argv[1].replace("-","_"): print(m["executable"])' "$1" "$work/build.json"
}

python3 -c 'import json,sys
for p in json.load(open(sys.argv[1]))["packages"]: print(p["name"], p["manifest_path"].rsplit("/",1)[0])' "$work/meta.json" >"$work/dirs"

# Every lookup key at once (one process reads the metadata and hashes the files once).
: >"$work/requests"
if [[ $part != options ]]; then
  sets shipped | while read -r crate features tested_set; do
    printf '%s\tdev\t%s\t%s\t%s\t%s\t%s\t%s\n' "$crate" "$cache/$crate.shipped" "$toolchain_line" "$scripts_hash_line" \
      "$crate.shipped" "$features" "$tested_set"
  done >>"$work/requests"
fi
if [[ $part != shipped ]]; then
  sets options | while read -r crate flag; do
    printf '%s\tdev\t%s\t%s\t%s\t%s\t%s\n' "$crate" "$cache/$crate$flag.option" "$toolchain_line" "$scripts_hash_line" \
      "$crate$flag.option" "$flag"
  done >>"$work/requests"
fi
scripts/features-cache.py keys "$work/meta-full.json" "$work/requests" >"$work/keys"
paste <(cut -f3 "$work/requests") "$work/keys" >"$work/keyed"
lookup() { awk -F'\t' -v e="$1" '$1 == e { print $2 }' "$work/keyed"; }

failed=0
# For the summary line: every crate checked, and the crates with a set that was not cached.
declare -A seen=() rebuilt=()
# The sets are taken into variables before the loops: a failing `sets` inside `< <(...)` would give the
# loop no input and the step would pass having checked nothing.
if [[ $part != options ]]; then
  shipped_sets=$(sets shipped) || { echo "check-features: reading the shipped sets failed" >&2; exit 1; }
  [[ -n $shipped_sets ]] || { echo "check-features: no shipped set found: nothing to check is not a pass" >&2; exit 1; }
fi
if [[ $part != shipped ]]; then
  option_sets=$(sets options) || { echo "check-features: reading the library options failed" >&2; exit 1; }
fi
if [[ $part != options ]]; then
  while read -r crate features tested_set; do
    feat=()
    [[ $features == - ]] || feat=(--features "$features")
    dir=$(awk -v c="$crate" '$1 == c { print $2 }' "$work/dirs")
    entry="$cache/$crate.shipped"
    seen[$crate]=1
    ktexts=("$(basename "$entry")" "$features" "$tested_set")
    k=$(lookup "$entry")
    if hit "$entry" "$k"; then
      # The selection is reused; the tests it names run again from the program it was made from
      # (they are quick, and what they read at run time is not in the key). A program that is
      # gone or not the one the entry names is a miss.
      bin=$(sed -n 's/^bin //p' "$entry")
      if [[ -x $bin ]] && [[ $(sha256sum <"$bin") == "$(sed -n 's/^sum //p' "$entry")" ]]; then
        echo "    $crate, as shipped [$features]: $(sed -n 's/^why //p' "$entry") (cached)"
        mapfile -t selected < <(sed -n 's/^test //p' "$entry")
        if [[ ${#selected[@]} -gt 0 ]]; then
          (cd "$dir" && CARGO_MANIFEST_DIR="$dir" "$bin" -q --exact "${selected[@]}" </dev/null) || { failed=1; rm -f "$entry"; }
        fi
        mark "$crate:cached"
        continue
      fi
    fi
    rm -f "$entry"
    rebuilt[$crate]=1
    : >"$work/messages.json"
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
    mark "$crate:graphs"
    if [[ $tested_set != "?" && ${solo_unit%% |*} == "$tested_set" && ${shipped_unit#* |} == "${solo_unit#* |}" ]]; then
      ir_build "$crate" "$features" "$work/shipped.ll" && ir_build "$crate" "$tested_set" "$work/tested.ll" &&
        shipped_bin=$(test_build "$crate" "$features") && [[ -x $shipped_bin ]] || {
        echo "check-features: $crate: no IR or test build" >&2; failed=1; continue; }
      mark "$crate:builds"
      "$shipped_bin" --list --format terse >"$work/tests.txt" </dev/null || {
        echo "check-features: $crate: its test binary could not list its tests" >&2; failed=1; continue; }
      scripts/feature-tests.py "$crate" "$work/shipped.ll" "$work/tested.ll" "$work/tests.txt" >"$work/selected.txt" 2>"$work/why.txt" || {
        cat "$work/why.txt" >&2; failed=1; continue; }
      echo "    $crate, as shipped [$features]: $(cat "$work/why.txt")"
      mark "$crate:compare"
      ok=1
      if [[ -s $work/selected.txt ]]; then
        mapfile -t selected <"$work/selected.txt"
        # Run as cargo test runs it: from the package's directory, with its manifest directory set.
        (cd "$dir" && CARGO_MANIFEST_DIR="$dir" "$shipped_bin" -q --exact "${selected[@]}" </dev/null) || ok=0
      fi
      mark "$crate:tests"
      if [[ $ok == 1 ]]; then
        store "$entry" "$crate" "$work/messages.json" "bin $shipped_bin" "sum $(sha256sum <"$shipped_bin")" \
          "why $(cat "$work/why.txt")"
        sed 's/^/test /' "$work/selected.txt" >>"$entry"
      else
        failed=1
      fi
    else
      echo "    $crate, as shipped [$features]: its dependencies resolve differently in [$tested_set], all tests run"
      cargo test -q --locked -p "$crate" --lib --no-default-features "${feat[@]}" </dev/null || failed=1
    fi
  done <<<"$shipped_sets"
fi
if [[ $part != shipped ]]; then
  while read -r crate flag; do
    entry="$cache/$crate$flag.option"
    seen[$crate]=1
    ktexts=("$(basename "$entry")" "$flag")
    k=$(lookup "$entry")
    if hit "$entry" "$k"; then
      echo "    $crate $flag (cached)"
      mark "$crate$flag:cached"
      continue
    fi
    rm -f "$entry"
    rebuilt[$crate]=1
    echo "    $crate $flag"
    ok=1
    cargo clippy -q --locked -p "$crate" "$flag" --all-targets --message-format json-render-diagnostics \
      -- -D warnings </dev/null >"$work/messages.json" || ok=0
    mark "$crate$flag:clippy"
    cargo build -q --locked -p "$crate" "$flag" --lib --message-format json-render-diagnostics \
      </dev/null >>"$work/messages.json" || ok=0
    mark "$crate$flag:build"
    if [[ $ok == 1 ]]; then store "$entry" "$crate" "$work/messages.json"; else failed=1; fi
  done < <(printf '%s' "${option_sets:+$option_sets$'\n'}")
fi
# One line for the lander: a crate is cached when every set of it was a hit, rebuilt otherwise.
n=${#seen[@]} m=${#rebuilt[@]}
if [[ $m == 0 ]]; then how=hit; elif [[ $m == "$n" ]]; then how=full; else how=partial; fi
echo "features: $n crates, $((n - m)) cached, $m rebuilt ($how)"
[[ $failed == 0 ]] || { echo "check-features: failed; see above" >&2; exit 1; }
echo "check-features: every shipped set is tested and every library option builds clean"
