#!/usr/bin/env bash
# A seed of third-party build artifacts, shared by every worktree that builds the same lock.
#
#   seed-target.sh key  <target-dir>     print the key this checkout would use
#   seed-target.sh save <target-dir>     build (if needed) and save the seed from that target dir
#   seed-target.sh load <target-dir>     fill an empty target dir from the seed
#
# The seed lives in $LEMNOS_CARGO_SEED_DIR (required: nothing is written anywhere by default).
# It is keyed by the content that decides what cargo can reuse: Cargo.lock, the rustc and cargo
# versions (rustc -vV, cargo -V), the profile (debug: dev and test builds) and the host triple.
#
# HARD RULE: the seed holds registry and git dependency artifacts ONLY. A workspace member's
# artifacts carry this checkout's absolute paths (CARGO_MANIFEST_DIR, file!(), build-script OUT_DIR),
# so a seeded member would be reused as fresh while reading another checkout's files. Anything that
# resolves to a workspace path (including git dependencies patched to workspace copies, root
# Cargo.toml [patch]) is a workspace member here and is never copied. `save` picks artifacts by the
# hash in each unit's file names, from the units whose package_id is registry+ or git+; it refuses to
# write a seed in which any hash also belongs to a path package. The package list it writes is the
# proof: MANIFEST names each seeded package with its version and source.
#
# Safety: `save` and `load` take flock on $LEMNOS_CARGO_SEED_DIR/.lock. A load holds it shared for
# the whole copy, so several worktrees can load at once. A save copies into a private temporary
# directory without the lock, and holds it exclusive only for the swap, so a load never reads a
# half-replaced seed.
#
# Copies use cp --reflink=auto: a reflink where the filesystem supports one (btrfs, xfs), otherwise
# a plain copy. The mode is printed by load and save.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
die() { echo "seed-target: $*" >&2; exit 1; }

cmd="${1:-}"
target_arg="${2:-}"
case "$cmd" in
  key | save | load) ;;
  *) die "usage: seed-target.sh key|save|load <target-dir>" ;;
esac
[[ -n "$target_arg" ]] || die "missing <target-dir>"
command -v flock >/dev/null || die "flock is required"

# The key: everything that decides whether an artifact is reusable as a whole.
compute_key() {
  local lock rustc_info triple lockhash desc
  [[ -f "$repo/Cargo.lock" ]] || die "no Cargo.lock in $repo"
  lockhash="$(sha256sum "$repo/Cargo.lock" | cut -d' ' -f1)"
  rustc_info="$(cd "$repo" && rustc -vV)"
  triple="$(sed -n 's/^host: //p' <<<"$rustc_info")"
  [[ -n "$triple" ]] || die "rustc -vV printed no host triple"
  desc="lock=$lockhash
rustc=$(sha256sum <<<"$rustc_info" | cut -d' ' -f1)
cargo=$(cd "$repo" && cargo -V)
profile=debug
triple=$triple"
  KEY_TEXT="$desc"
  KEY="$(printf '%s' "$desc" | sha256sum | cut -c1-32)"
}

seed_root() {
  [[ -n "${LEMNOS_CARGO_SEED_DIR:-}" ]] || die "LEMNOS_CARGO_SEED_DIR is not set"
  mkdir -p "$LEMNOS_CARGO_SEED_DIR"
  (cd "$LEMNOS_CARGO_SEED_DIR" && pwd -P)
}

# Says whether a copy into $1 gets reflinks: a file from the seed's filesystem, cloned there.
copy_mode() {
  local probe="$1/.seed-probe-$$" mode=copy
  : >"$probe.src"
  if cp --reflink=always "$probe.src" "$probe" 2>/dev/null; then mode=reflink; fi
  rm -f "$probe.src" "$probe"
  echo "$mode"
}

compute_key
if [[ "$cmd" == key ]]; then
  echo "$KEY"
  echo "$KEY_TEXT" >&2
  exit 0
fi

target="$target_arg"
[[ -d "$target" ]] || die "target dir $target does not exist"
target="$(cd "$target" && pwd -P)"
seeds="$(seed_root)"
seed_dir="$seeds/$KEY"

if [[ "$cmd" == load ]]; then
  if [[ -d "$target/debug" ]] && [[ -n "$(ls -A "$target/debug" 2>/dev/null)" ]]; then
    die "target $target/debug is not empty; load fills an empty target only"
  fi
  exec 9>"$seeds/.lock"
  flock -s 9
  [[ -f "$seed_dir/KEY" ]] || die "no seed for key $KEY in $seeds"
  [[ "$(cat "$seed_dir/KEY")" == "$KEY" ]] || die "seed at $seed_dir has key $(cat "$seed_dir/KEY"), not $KEY: refused"
  mode="$(copy_mode "$target")"
  mkdir -p "$target"
  cp -a --reflink=auto "$seed_dir/payload/debug" "$target/"
  # Build-script outputs are replayed by cargo into rustc, and they name the OUT_DIR of the
  # target the seed was saved from (cargo:rustc-link-search). Point them at this target; the
  # mtime is kept, so cargo's freshness check does not change.
  rewritten="$(python3 - "$(cat "$seed_dir/SOURCE-TARGET")" "$target" "$target/debug/build" <<'PY2'
import os, sys
src, dst, build = sys.argv[1], sys.argv[2], sys.argv[3]
n = 0
for pkg in os.listdir(build) if os.path.isdir(build) else []:
    for name in ("output", "root-output"):
        f = os.path.join(build, pkg, name)
        if not os.path.isfile(f):
            continue
        data = open(f, "rb").read()
        if src.encode() not in data:
            continue
        st = os.stat(f)
        open(f, "wb").write(data.replace(src.encode(), dst.encode()))
        os.utime(f, ns=(st.st_atime_ns, st.st_mtime_ns))
        n += 1
print(n)
PY2
)"
  {
    echo "seeded-from-key=$KEY"
    echo "seed-dir=$seed_dir"
    echo "loaded-at=$(date -u +%FT%TZ)"
    echo "copy-mode=$mode"
  } >"$target/.lemnos-seed"
  flock -u 9
  echo "seed-target: loaded key $KEY into $target ($(find "$target/debug" -type f | wc -l) files, $mode; $rewritten build output file(s) re-pointed)"
  exit 0
fi

# save
work="$(mktemp -d "${TMPDIR:-/tmp}/seed-save.XXXXXX")"
trap 'rm -rf "$work"' EXIT
cd "$repo"
export CARGO_TARGET_DIR="$target"
# Both commands fail only on a build error; on a warm target they only print the units.
cargo clippy --locked --workspace --all-targets --message-format=json >"$work/clippy.json" 2>"$work/clippy.err" ||
  { cat "$work/clippy.err" >&2; die "cargo clippy failed"; }
cargo test --locked --workspace --no-run --message-format=json >"$work/test.json" 2>"$work/test.err" ||
  { cat "$work/test.err" >&2; die "cargo test --no-run failed"; }
cargo metadata --locked --format-version 1 >"$work/metadata.json"

python3 - "$work" "$target" "$KEY" <<'PY'
import json, os, re, sys

work, target, key = sys.argv[1], sys.argv[2], sys.argv[3]
meta = json.load(open(f"{work}/metadata.json"))
members = set(meta["workspace_members"])
packages = {p["id"]: p for p in meta["packages"]}
HASH = re.compile(r"-([0-9a-f]{16})(?![0-9a-f])")

seedable = {}   # hash -> "name version source" of the registry or git package that owns it
ws_hashes = set()
ws_pkgs = set()
for path in (f"{work}/clippy.json", f"{work}/test.json"):
    for line in open(path):
        line = line.strip()
        if not line.startswith("{"):
            continue
        msg = json.loads(line)
        reason = msg.get("reason")
        if reason == "compiler-artifact":
            pkg_id = msg["package_id"]
            paths = list(msg.get("filenames", [])) + ([msg["executable"]] if msg.get("executable") else [])
        elif reason == "build-script-executed":
            pkg_id = msg["package_id"]
            paths = [msg["out_dir"]]
        else:
            continue
        # Only the part under the target dir: the registry's own path (index.crates.io-<16 hex>) must not count.
        rel = [p[len(target) + 1:] if p.startswith(target + "/") else os.path.basename(p) for p in paths]
        hashes = {h for p in rel for h in HASH.findall(p)}
        pkg = packages[pkg_id] if pkg_id in packages else None
        if pkg is None:
            continue
        if pkg.get("source") is None:  # path package: a workspace member or a patch of one
            ws_hashes |= hashes
            ws_pkgs.add(f"{pkg['name']} {pkg['version']}")
            continue
        label = f"{pkg['name']} {pkg['version']} {pkg['source'].split('#')[0]}"
        for h in hashes:
            seedable.setdefault(h, label)

# The refusal: a hash owned by a path package is never seeded.
both = set(seedable) & ws_hashes
if both:
    sys.exit(f"seed-target: {len(both)} unit hash(es) belong to a path package too: refusing")
# The package list, for the proof: no member may appear in it.
member_names = {packages[m]["name"] for m in members}
listed = sorted({v for v in seedable.values()})
seeded_names = {v.split()[0] for v in listed}
clash = sorted(seeded_names & member_names)
if clash:
    sys.exit(f"seed-target: workspace member names would be seeded: {', '.join(clash)}")

debug = os.path.join(target, "debug")
want = []   # relative paths under debug/ for the seeded hashes, dirs as whole entries
for sub in ("deps", ".fingerprint", "build"):
    base = os.path.join(debug, sub)
    if not os.path.isdir(base):
        continue
    for name in sorted(os.listdir(base)):
        found = HASH.findall(name)
        if len(found) == 1 and found[0] in seedable:
            want.append(os.path.join(sub, name))
with open(f"{work}/copy.list", "w") as f:
    for w in want:
        f.write(w + "\0")
with open(f"{work}/MANIFEST", "w") as f:
    f.write(f"# key {key}\n# seeded packages (registry or git), one per line: name version source\n")
    for v in listed:
        f.write(v + "\n")
    f.write(f"# workspace packages excluded: {len(ws_pkgs)} (not in this list)\n")
print(f"seed-target: {len(listed)} package version(s), {len(seedable)} unit hash(es), {len(want)} entries")
PY

# The entries the list names must exist; a save of a partial target is refused.
[[ -s "$work/copy.list" ]] || die "no seedable artifacts in $target/debug (empty build?)"

tmp="$seeds/.tmp-$KEY-$$"
rm -rf "$tmp"
mkdir -p "$tmp/payload/debug"
mode="$(copy_mode "$tmp")"
(cd "$target/debug" && xargs -0 -r cp -a --reflink=auto --parents -t "$tmp/payload/debug/") <"$work/copy.list"
cp "$work/MANIFEST" "$tmp/MANIFEST"
printf '%s\n' "$KEY" >"$tmp/KEY"
printf '%s\n' "$target" >"$tmp/SOURCE-TARGET"
printf '%s\n' "$KEY_TEXT" >"$tmp/KEY.text"

exec 9>"$seeds/.lock"
flock -x 9
rm -rf "$seed_dir"
mv "$tmp" "$seed_dir"
flock -u 9
echo "seed-target: saved key $KEY to $seed_dir ($mode, $(du -sh "$seed_dir" | cut -f1))"
