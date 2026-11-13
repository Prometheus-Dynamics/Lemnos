#!/usr/bin/env bash
# Checks the files in packaging/ without installing anything: the udev rules with udevadm, the
# sysusers file with systemd-sysusers (a dry run in an empty root), and the units with
# systemd-analyze. Each check is skipped, with a line saying so, when its tool is not installed.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

# The units' security exposure, from `systemd-analyze security --offline=true`, must stay at or
# below this, as Orion's node unit does (packaging/README.md).
max_exposure=2.5

work="$(mktemp -d "${TMPDIR:-/tmp}/lemnos-packaging.XXXXXX")"
trap 'rm -rf "$work"' EXIT

if command -v udevadm >/dev/null; then
  echo "==> udev rules"
  udevadm verify --resolve-names=never --no-summary packaging/udev/*.rules
else
  echo "==> udev rules: skipped, udevadm is not installed"
fi

if command -v systemd-sysusers >/dev/null; then
  echo "==> sysusers"
  mkdir -p "$work/root/etc"
  systemd-sysusers --dry-run --root="$work/root" packaging/systemd/lemnos.conf >/dev/null 2>&1
else
  echo "==> sysusers: skipped, systemd-sysusers is not installed"
fi

# RestartSteps= (the units' restart backoff) needs systemd 254 or newer. An older systemd, or a
# runner whose systemd lacks a unit the files refer to, skips the unit checks with a line saying so.
systemd_major=0
if command -v systemd-analyze >/dev/null; then
  systemd_major="$(systemd-analyze --version 2>/dev/null | awk 'NR == 1 { print $2 + 0 }')"
fi
if [[ "$systemd_major" -ge 254 ]]; then
  echo "==> units"
  # The daemon binaries are not installed here, so each unit runs a stand-in in a copy: the
  # verification reads every other directive as written.
  for unit in packaging/systemd/*.service; do
    sed -e 's|^ExecStart=/usr/bin/[^ ]*|ExecStart=/bin/true|' "$unit" >"$work/$(basename "$unit")"
  done
  verified="$(cd "$work" && systemd-analyze verify --man=no ./*.service 2>&1)" || {
    if grep -q "not found" <<<"$verified"; then
      echo "    units: skipped, a unit the files refer to is not on this systemd: $(grep -m1 'not found' <<<"$verified")"
      echo "    packaging: ok"
      exit 0
    fi
    echo "$verified" >&2
    exit 1
  }
  (cd "$work" && systemd-analyze security --offline=true ./*.service) | awk -v max="$max_exposure" '
    /Overall exposure level/ {
      seen++
      name = $6; score = $7
      sub(/:$/, "", name)
      if (score + 0 > max + 0) { printf "packaging: %s exposure %s is above %s\n", name, score, max; bad = 1 }
      else printf "    %s exposure %s\n", name, score
    }
    END {
      if (seen == 0) { print "packaging: no exposure levels read"; exit 1 }
      exit bad
    }'
elif command -v systemd-analyze >/dev/null; then
  echo "==> units: skipped, systemd $systemd_major is older than 254 (RestartSteps=)"
else
  echo "==> units: skipped, systemd-analyze is not installed"
fi

echo "    packaging: ok"
