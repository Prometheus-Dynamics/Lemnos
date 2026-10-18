#!/usr/bin/env bash
# Fails when a product name appears in a tracked file or path outside the
# allowlist (scripts/check-names.allow). Lemnos is a generic hardware library:
# product names belong in the products' own repositories, not here.
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root_dir"

allow_file="scripts/check-names.allow"

# Product names (case-insensitive, ERE). Add a name here when a product is
# added to the list of products Lemnos must not depend on.
product='photon(vision)?|helios|wpilib|\bfrc\b|raze|\bcm5\b|atlas|styx|eidos|networktables|\bpv\b|[Pp]v[._-]|/run/media|/home/[a-z]|sozo|pepservices'

file_globs=()
token_res=()
scoped_globs=()
scoped_res=()

# Allowlist format, one entry per line, tab-separated:
#   file    <glob>                 <reason>   whole file is exempt
#   token   <ERE>                  <reason>   matches are ignored everywhere
#   scoped  <glob>  <ERE>          <reason>   matches are ignored in matching files
if [[ -f "$allow_file" ]]; then
  while IFS=$'\t' read -r kind a b _reason; do
    [[ -z "${kind:-}" || "$kind" == \#* ]] && continue
    case "$kind" in
      file) file_globs+=("$a") ;;
      token) token_res+=("$a") ;;
      scoped) scoped_globs+=("$a"); scoped_res+=("$b") ;;
      *) echo "check-names: unknown allowlist kind '$kind'" >&2; exit 2 ;;
    esac
  done <"$allow_file"
fi

matches_glob() {
  # $1 path, $2.. globs (unquoted patterns)
  local path="$1" glob
  shift
  for glob in "$@"; do
    # shellcheck disable=SC2254
    case "$path" in $glob) return 0 ;; esac
  done
  return 1
}

# Removes the exempt matches from stdin, keeping the line count.
strip_exempt() {
  local path="$1" sed_args=(-E) re i
  for re in "${token_res[@]}"; do
    sed_args+=(-e "s#${re}# #gI")
  done
  for i in "${!scoped_globs[@]}"; do
    # shellcheck disable=SC2254
    case "$path" in ${scoped_globs[$i]}) sed_args+=(-e "s#${scoped_res[$i]}# #gI") ;; esac
  done
  sed "${sed_args[@]}"
}

violations=0
report() {
  echo "$1"
  violations=$((violations + 1))
}

while IFS= read -r -d '' path; do
  case "$path" in *Cargo.lock) continue ;; esac
  if matches_glob "$path" "${file_globs[@]+"${file_globs[@]}"}"; then
    continue
  fi

  # The path itself.
  if printf '%s\n' "$path" | strip_exempt "$path" | grep -q -i -E "$product"; then
    report "$path: product name in file path"
  fi

  # Text, skipping binary files.
  if ! grep -qI . -- "$path" 2>/dev/null; then
    continue
  fi
  while IFS= read -r line_no; do
    report "$path:$line_no: $(sed -n "${line_no}p" -- "$path")"
  done < <(strip_exempt "$path" <"$path" | grep -n -i -E "$product" | cut -d: -f1 || true)
done < <(git ls-files -z)

if [[ "$violations" -gt 0 ]]; then
  echo
  echo "check-names: $violations product-name occurrence(s) outside $allow_file" >&2
  exit 1
fi
echo "check-names: no product names outside the allowlist"
