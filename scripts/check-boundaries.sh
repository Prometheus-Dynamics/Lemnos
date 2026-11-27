#!/usr/bin/env bash
# Enforces the layering rules in docs/design.md. Exits non-zero on a violation.
#
# With no argument: checks this repository, then runs every planted violation
# in testing/boundary/ and requires each one to fail. With a directory: checks
# only that tree.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
self="${BASH_SOURCE[0]}"

# Board-run records name the board a check was written for or run on. Nothing else names one.
records='^testing/board-runs/'
exempt='^(scripts/check-boundaries\.sh|testing/boundary/)'
product='photon(vision)?|helios|wpilib|\bfrc\b|\braze\b|\bcm5\b|\batlas\b|\bstyx\b|\beidos\b|networktables|/run/media|/home/[a-z]|sozo|pepservices'
hardware='camera|imu|sensor|gyro|gyroscope|accel|accelerometer|fan|light|led|isp'

list_files() {
  find . \( -name target -o -name .git -o -name target-docker -o -name .cargo-docker \) -prune \
    -o -type f ! -name Cargo.lock -print | sed 's|^\./||' | sort
}

# grep that finds nothing is not a failure here; grep that could not read is (status 2 and up).
grep_any() { grep "$@" || [ $? -eq 1 ]; }

# Each list a rule walks is taken into a variable first (`x=$(...)` under errexit stops on a failure),
# never read through `< <(...)`, whose status is lost: a list that failed would be a rule that checked
# nothing. lines_of prints a variable's lines with no empty one.
lines_of() { [[ -z "$1" ]] || printf '%s\n' "$1"; }

# Prints the crate directories of the given workspace folder.
crates_in() {
  [[ -d "$1" ]] || return 0
  find "$1" -mindepth 2 -maxdepth 2 -name Cargo.toml -printf '%h\n' | sort
}

# Normal and build dependencies; dev-dependencies are test wiring, and allowed.
runtime_deps() {
  awk '
    /^\[/ { active = ($0 ~ /dependencies\]/ && $0 !~ /dev-dependencies/) ; next }
    active { print }
  ' "$1"
}

may_use_unsafe() {
  case "$1" in
    engine/abi | engine/capi | engine/measure | backends/linux-sys | drivers/*-sys) return 0 ;;
    # The shm family: shared mappings, eventfds, descriptor passing, the clock.
    shm/sample-time | shm/ring | shm/ring-buffers | shm/ring-holds | shm/ring-doorbell) return 0 ;;
    shm/fdpass | shm/measure) return 0 ;;
    *) return 1 ;;
  esac
}

check_tree() (
  cd "$1"
  local failed=0 crate manifest
  fail() {
    echo "boundary: $*" >&2
    failed=1
  }

  # The engine never imports what it hosts. One exception: the C library is a
  # client program for C callers, and a client program brings the operating
  # system's half of a frame stream, backends/linux-stream (docs/streams.md).
  local crates
  crates=$(crates_in engine)
  while read -r crate; do
    if runtime_deps "$crate/Cargo.toml" |
      if [[ $crate == engine/capi ]]; then grep -v 'path *= *"\.\./\.\./backends/linux-stream"'; else cat; fi |
      grep -Eq 'path *= *"[^"]*(contracts|drivers|backends|service)/'; then
      fail "$crate depends on contracts, drivers, backends or service"
    fi
  done < <(lines_of "$crates")

  # The engine and the service don't know kinds of hardware.
  local tree hits
  for tree in engine service; do
    [[ -d "$tree" ]] || continue
    hits=$(grep_any -rHnoP --include='*.rs' '\b(?:struct|enum|trait|type|union|mod)\s+\K\w+' "$tree" |
        perl -ne '
          chomp; my ($f, $l, $n) = split /:/;
          (my $w = $n) =~ s/([a-z0-9])([A-Z])/$1 $2/g; $w =~ s/_/ /g;
          print "$f:$l: $n\n" if lc($w) =~ /\b('"$hardware"')s?\b/;
        ')
    while read -r hit; do fail "named after hardware: $hit"; done < <(lines_of "$hits")
    hits=$(find "$tree" -name '*.rs' | perl -ne 'chomp; my ($b) = m{([^/]+)\.rs$}; (my $w = $b) =~ s/_/ /g; print "$_\n" if lc($w) =~ /\b('"$hardware"')s?\b/;')
    while read -r hit; do fail "file named after hardware: $hit"; done < <(lines_of "$hits")
  done

  # The shm family stands alone: no crate in it depends on anything outside shm/, dev and
  # build dependencies included, so another project can take it at a locked revision.
  local dep deps
  crates=$(crates_in shm)
  while read -r crate; do
    deps=$(grep_any -oP 'path *= *"\K[^"]+' "$crate/Cargo.toml")
    while read -r dep; do
      case "$(realpath -m "$crate/$dep")" in
        "$PWD"/shm/*) ;;
        *) fail "$crate depends on a path outside shm/: $dep" ;;
      esac
    done < <(lines_of "$deps")
    if grep -Eq '^ *lemnos' "$crate/Cargo.toml"; then
      fail "$crate depends on a lemnos crate"
    fi
  done < <(lines_of "$crates")

  # unsafe lives in the named crates only, and the lint that enforces it stays on.
  local folder
  for folder in engine backends service contracts drivers testing/crates shm; do
    crates=$(crates_in "$folder")
    while read -r crate; do
      may_use_unsafe "$crate" && continue
      manifest="$crate/Cargo.toml"
      if ! grep -Eq '^workspace *= *true' <(awk '/^\[lints\]/{a=1;next} /^\[/{a=0} a' "$manifest"); then
        fail "$crate must inherit the workspace lints"
      fi
      hits=$(grep_any -rlE --include='*.rs' '\bunsafe\s*(\{|fn\b|impl\b|trait\b|extern\b)' "$crate")
      while read -r hit; do fail "unsafe outside the allowed crates: $hit"; done < <(lines_of "$hits")
    done < <(lines_of "$crates")
  done

  # No product names, in paths or contents.
  local file files
  files=$(list_files)
  [[ -n "$files" ]] || { fail "no files listed in $1: the product-name rule would check nothing"; exit 1; }
  while read -r file; do
    [[ "$file" =~ $exempt || "$file" =~ $records ]] && continue
    if grep -Eqi "$product" <<<"$file"; then fail "product name in path: $file"; fi
    local rc=0
    grep -IEqi "$product" "$file" || rc=$?
    if [[ $rc -eq 0 ]]; then
      fail "product name in $file: $(grep -IEin "$product" "$file" | head -1 | cut -c1-120)"
    elif [[ $rc -ne 1 ]]; then
      fail "could not read $file (grep status $rc)"
    fi
  done < <(lines_of "$files")

  exit "$failed"
)

# --fixtures: the planted-violation self-test alone (testing/gate-lanes/check.sh). The gate runs both.
fixtures_only=0
if [[ "${1:-}" == --fixtures ]]; then
  fixtures_only=1
elif [[ $# -eq 1 ]]; then
  check_tree "$1"
  exit
fi

[[ $fixtures_only == 1 ]] || check_tree "$repo"

# Each planted violation must be caught by the rule it plants: its exit is 1 and its message is
# the one that rule gives. Any other failure (a crash, a different rule) is not a catch.
declare -A expect=(
  [engine-imports-driver]="boundary: engine/x depends on contracts, drivers, backends or service"
  [hardware-file-name]="boundary: file named after hardware: engine/x/src/imu.rs"
  [hardware-module-name]="boundary: named after hardware: service/x/src/lib.rs:1: fan_loop"
  [hardware-type-name]="boundary: named after hardware: engine/x/src/lib.rs:1: CameraState"
  [lints-dropped]="boundary: drivers/x must inherit the workspace lints"
  [product-in-path]="boundary: product name in path: docs/"
  [product-name]="boundary: product name in docs/note.md: 1:"
  [shm-depends-out]="boundary: shm/x depends on a path outside shm/: ../../engine/core"
  [unsafe-outside]="boundary: unsafe outside the allowed crates: drivers/x/src/lib.rs"
)
seen=0
for fixture in "$repo"/testing/boundary/*/; do
  name=$(basename "$fixture")
  want=${expect[$name]:-}
  [[ -n "$want" ]] || { echo "boundary: fixture $name has no expected message in this script" >&2; exit 1; }
  rc=0
  got=$("$self" "$fixture" 2>&1 >/dev/null) || rc=$?
  if [[ $rc -ne 1 ]] || ! grep -qF -- "$want" <<<"$got"; then
    echo "boundary: planted violation $name was not caught by its rule (exit $rc): ${got:-no message}" >&2
    exit 1
  fi
  seen=$((seen + 1))
done
[[ $seen -eq ${#expect[@]} ]] || { echo "boundary: $seen fixtures ran, ${#expect[@]} expected" >&2; exit 1; }
echo "boundaries hold"
