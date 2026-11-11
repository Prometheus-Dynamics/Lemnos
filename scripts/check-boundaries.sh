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

  # The engine never imports what it hosts.
  while read -r crate; do
    if runtime_deps "$crate/Cargo.toml" | grep -Eq 'path *= *"[^"]*(contracts|drivers|backends|service)/'; then
      fail "$crate depends on contracts, drivers, backends or service"
    fi
  done < <(crates_in engine)

  # The engine and the service don't know kinds of hardware.
  local tree
  for tree in engine service; do
    [[ -d "$tree" ]] || continue
    while read -r hit; do fail "named after hardware: $hit"; done < <(
      { grep -rHnoP --include='*.rs' '\b(?:struct|enum|trait|type|union|mod)\s+\K\w+' "$tree" || true; } |
        perl -ne '
          chomp; my ($f, $l, $n) = split /:/;
          (my $w = $n) =~ s/([a-z0-9])([A-Z])/$1 $2/g; $w =~ s/_/ /g;
          print "$f:$l: $n\n" if lc($w) =~ /\b('"$hardware"')s?\b/;
        '
    )
    while read -r hit; do fail "file named after hardware: $hit"; done < <(
      find "$tree" -name '*.rs' | perl -ne 'chomp; my ($b) = m{([^/]+)\.rs$}; (my $w = $b) =~ s/_/ /g; print "$_\n" if lc($w) =~ /\b('"$hardware"')s?\b/;'
    )
  done

  # unsafe lives in the named crates only, and the lint that enforces it stays on.
  local folder
  for folder in engine backends service contracts drivers testing/crates; do
    while read -r crate; do
      may_use_unsafe "$crate" && continue
      manifest="$crate/Cargo.toml"
      if ! grep -Eq '^workspace *= *true' <(awk '/^\[lints\]/{a=1;next} /^\[/{a=0} a' "$manifest"); then
        fail "$crate must inherit the workspace lints"
      fi
      while read -r hit; do fail "unsafe outside the allowed crates: $hit"; done < <(
        grep -rlE --include='*.rs' '\bunsafe\s*(\{|fn\b|impl\b|trait\b|extern\b)' "$crate" || true
      )
    done < <(crates_in "$folder")
  done

  # No product names, in paths or contents.
  local file
  while read -r file; do
    [[ "$file" =~ $exempt || "$file" =~ $records ]] && continue
    if grep -Eqi "$product" <<<"$file"; then fail "product name in path: $file"; fi
    if grep -IEqi "$product" "$file"; then
      fail "product name in $file: $(grep -IEin "$product" "$file" | head -1 | cut -c1-120)"
    fi
  done < <(list_files)

  exit "$failed"
)

if [[ $# -eq 1 ]]; then
  check_tree "$1"
  exit
fi

check_tree "$repo"

for fixture in "$repo"/testing/boundary/*/; do
  if "$self" "$fixture" >/dev/null 2>&1; then
    echo "boundary: planted violation was not caught: ${fixture#"$repo"/}" >&2
    exit 1
  fi
done
echo "boundaries hold"
