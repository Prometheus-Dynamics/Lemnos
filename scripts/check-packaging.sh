#!/usr/bin/env bash
# Checks the files in packaging/ without installing anything: the udev rules with udevadm, the
# sysusers file with systemd-sysusers (a dry run in an empty root), and the units with
# systemd-analyze. Each check is skipped, with a line saying so, when its tool is not installed.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

# The units' security exposure, from `systemd-analyze security --offline=true`. A unit stays at or
# below max_exposure (the baseline, as Orion's node unit does), in tenths of a point. The one
# exception is a board's optional fragment (packaging/systemd/lemnosd.service.d/gadget.conf), checked
# with the base unit it extends: it may add exactly the items in `additions`, each at the score
# systemd-analyze gives it. An addition the unit no longer has fails the check, so the list cannot
# outlive what it names, and any other exposure fails it.
max_exposure=2.5
max_tenths=25
# name as systemd-analyze prints it | score in tenths | reason
additions=(
  "CapabilityBoundingSet=~CAP_SYS_ADMIN|3|the root phase's FunctionFS mount: mount(2) of functionfs needs CAP_SYS_ADMIN. The daemon drops it with the rest after the root phase"
  "SystemCallFilter=~@mount|2|the root phase's FunctionFS mount and its unmount. Without CAP_SYS_ADMIN they fail with EPERM"
)
fragment=packaging/systemd/lemnosd.service.d/gadget.conf
combined=lemnosd-gadget.service

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
  # The base unit with the gadget fragment inserted after its [Service] line: the unit a gadget
  # board runs. Checked as one unit, against the additions.
  [[ -f "$fragment" ]] || { echo "packaging: $fragment is missing"; exit 1; }
  awk -v frag="$fragment" '
    { print }
    /^\[Service\]/ { while ((getline line < frag) > 0) if (line !~ /^\[Service\]/ && line !~ /^#/) print line }
  ' "$work/lemnosd.service" >"$work/$combined"
  sed -i '/^$/d' "$work/$combined"
  verified="$(cd "$work" && systemd-analyze verify --man=no ./*.service 2>&1)" || {
    echo "$verified" >&2
    exit 1
  }
  # Every unit's rows and overall score, one unit at a time.
  bad=0
  for unit in "$work"/*.service; do
    name="$(basename "$unit")"
    out="$(systemd-analyze security --offline=true "$unit" 2>&1)"
    score="$(awk '/Overall exposure level/ { print $7 }' <<<"$out")"
    [[ -n "$score" ]] || { echo "packaging: no exposure level read for $name"; exit 1; }
    rows="$(awk '/^✗/ { print $2 "|" $NF }' <<<"$out")"
    limit=$max_tenths
    if [[ "$name" == "$combined" ]]; then
      for entry in "${additions[@]}"; do
        item="${entry%%|*}"
        rest="${entry#*|}"
        listed="${rest%%|*}"
        found="$(grep -F "$item|" <<<"$rows" | head -1 | cut -d'|' -f2 || true)"
        if [[ -z "$found" ]]; then
          echo "packaging: $item is listed as an addition but $name no longer has it: remove it from additions"
          bad=1
          continue
        fi
        got="$(awk -v s="$found" 'BEGIN { printf "%d", s * 10 + 0.5 }')"
        if [[ "$got" != "$listed" ]]; then
          echo "packaging: $item scores $found in $name; the list says $listed tenths"
          bad=1
        fi
        limit=$((limit + listed))
      done
    else
      while IFS='|' read -r item found; do
        for entry in "${additions[@]}"; do
          if [[ "${entry%%|*}" == "$item" ]]; then
            echo "packaging: $item is an addition, so it belongs in $fragment, not in $name"
            bad=1
          fi
        done
      done <<<"$rows"
    fi
    got="$(awk -v s="$score" 'BEGIN { printf "%d", s * 10 + 0.5 }')"
    if [[ "$got" -gt "$limit" ]]; then
      echo "packaging: $name exposure $score is above $(awk -v l="$limit" 'BEGIN { printf "%.1f", l / 10 }') (the baseline plus its listed additions)"
      bad=1
    else
      echo "    $name exposure $score (limit $(awk -v l="$limit" 'BEGIN { printf "%.1f", l / 10 }'))"
    fi
  done
  if [[ "$bad" -ne 0 ]]; then exit 1; fi
elif command -v systemd-analyze >/dev/null; then
  echo "==> units: skipped, systemd $systemd_major is older than 254 (RestartSteps=)"
else
  echo "==> units: skipped, systemd-analyze is not installed"
fi

# The root phase: lemnosd and the bridge start as root and drop themselves (`--user`, LEMNOS_ORION_USER),
# so no unit may give them a user or an ambient capability. The daemon's unit must pass its drop.
echo "==> root phase"
for unit in packaging/systemd/*.service; do
  if grep -Eq '^(User|Group|AmbientCapabilities)=' "$unit"; then
    echo "packaging: $unit gives a user or an ambient capability; the daemon drops itself" >&2
    exit 1
  fi
done
grep -q -- '--user lemnos --group lemnos' packaging/systemd/lemnosd.service || {
  echo "packaging: lemnosd.service does not pass --user lemnos --group lemnos" >&2
  exit 1
}
grep -q 'lemnos-orion.env' packaging/systemd/lemnos-orion.service || {
  echo "packaging: lemnos-orion.service does not read /etc/lemnos/lemnos-orion.env" >&2
  exit 1
}

# The hardware watchdog belongs to lemnosd alone, and a restart delay is constant: no unit may set
# a systemd watchdog, or a restart delay that grows (packaging/README.md, "The hardware watchdog").
echo "==> watchdog and restart"
for unit in packaging/systemd/*.service; do
  if grep -Eq '^(RuntimeWatchdogSec|ShutdownWatchdogSec|WatchdogSec|RestartSteps|RestartMaxDelaySec)=' "$unit"; then
    echo "packaging: $unit sets a watchdog or a growing restart delay" >&2
    exit 1
  fi
done

# The BusyBox init scripts: parsed, never run. sh is always here; busybox and shellcheck are optional.
echo "==> init scripts"
for script in packaging/init/S5*; do
  sh -n "$script"
  if command -v busybox >/dev/null; then
    busybox sh -n "$script"
  else
    echo "    busybox sh -n $script: skipped, busybox is not installed"
  fi
  if command -v shellcheck >/dev/null; then
    shellcheck --shell=sh "$script"
  else
    echo "    shellcheck $script: skipped, shellcheck is not installed"
  fi
done
# The settings files are sourced by the scripts, so they must parse as shell too.
for env in packaging/systemd/*.env.example; do
  sh -n "$env"
done

# The mdev fragment: every rule is an anchored pattern, owner:group and a four-digit mode.
echo "==> mdev fragment"
awk '
  /^[[:space:]]*(#|$)/ { next }
  NF != 3 || $1 !~ /^\^.*\$$/ || $2 !~ /^[a-z0-9]+:[a-z0-9]+$/ || $3 !~ /^[0-7]{4}$/ {
    printf "packaging/init/mdev.conf.fragment:%d: not a rule: %s\n", NR, $0; bad = 1
  }
  END { exit bad }
' packaging/init/mdev.conf.fragment

echo "    packaging: ok"
