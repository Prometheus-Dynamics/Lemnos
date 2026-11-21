#!/usr/bin/env bash
# Checks the files in packaging/ without installing anything: the udev rules with udevadm, the
# sysusers file with systemd-sysusers (a dry run in an empty root), and the units with
# systemd-analyze. Each check is skipped, with a line saying so, when its tool is not installed.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

# The units' security exposure, from `systemd-analyze security --offline=true`. A unit stays at or
# below max_exposure (the baseline, as Orion's node unit does), in tenths of a point. The one
# exception is a board's optional fragment (the files in packaging/systemd/lemnosd.service.d/), each
# checked with the base unit it extends: it may add exactly its items in `additions`, each at the
# score systemd-analyze gives it. An addition the unit no longer has fails the check, so the list
# cannot outlive what it names, and any other exposure fails it.
max_exposure=2.5
max_tenths=25
# the fragment | the base unit with it, as checked
fragments=(
  "packaging/systemd/lemnosd.service.d/gadget.conf|lemnosd-gadget.service"
  "packaging/systemd/lemnosd.service.d/health-tcp.conf|lemnosd-health-tcp.service"
  "packaging/systemd/lemnosd.service.d/slots.conf|lemnosd-slots.service"
  "packaging/systemd/lemnosd.service.d/security.conf|lemnosd-security.service"
)
# the unit it is in ; name as systemd-analyze prints it ; score in tenths ; reason
# (a semicolon between the fields: a name may hold a bar)
additions=(
  "lemnosd-gadget.service;CapabilityBoundingSet=~CAP_SYS_ADMIN;3;the root phase's FunctionFS mount: mount(2) of functionfs needs CAP_SYS_ADMIN. The daemon drops it with the rest after the root phase"
  "lemnosd-gadget.service;SystemCallFilter=~@mount;2;the root phase's FunctionFS mount and its unmount. Without CAP_SYS_ADMIN they fail with EPERM"
  "lemnosd-health-tcp.service;RestrictAddressFamilies=~AF_(INET|INET6);3;a tcp: health source is a TCP connect the daemon makes to a port on the board. It listens on nothing"
)

work="$(mktemp -d "${TMPDIR:-/tmp}/lemnos-packaging.XXXXXX")"
trap 'rm -rf "$work"' EXIT

# The generated fragment (`lemnos-ctl tree --udev`) for the reference board is checked the same way,
# and its group must be the one packaging/systemd/lemnos.conf creates: the daemon's user and group.
generated=service/devtree/tests/golden/reference-udev.rules
groups=$(grep -o 'GROUP="[^"]*"' "$generated" | sort -u)
if [ "$(printf '%s\n' "$groups" | grep -c .)" != 1 ] || ! grep -q '^u lemnos ' packaging/systemd/lemnos.conf; then
  echo "==> generated udev rules: the group in $generated is not the daemon's group" >&2
  exit 1
fi
if [ "$groups" != 'GROUP="lemnos"' ]; then
  echo "==> generated udev rules: $generated gives $groups, the daemon's group is lemnos" >&2
  exit 1
fi

if command -v udevadm >/dev/null; then
  echo "==> udev rules"
  udevadm verify --resolve-names=never --no-summary packaging/udev/*.rules "$generated"
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
  # The generated security unit (`lemnos-ctl tree --security-unit`), as the reference board's golden.
  sed -e 's|^ExecStart=/usr/bin/[^ ]*.*|ExecStart=/bin/true|' service/ctl/tests/golden/reference-security.service \
    >"$work/lemnos-board-security.service"
  verified="$(cd "$work" && systemd-analyze verify --man=no ./*.service 2>&1)" || {
    if grep -q "not found" <<<"$verified"; then
      echo "    units: skipped, a unit the files refer to is not on this systemd: $(grep -m1 'not found' <<<"$verified")"
      echo "    packaging: ok"
      exit 0
    fi
    echo "$verified" >&2
    exit 1
  }
  # The base unit with one fragment inserted at the end of its [Service] section (before [Install]),
  # after the base's own settings: a drop-in's `ExecStart=` reset then clears the base's one, as it
  # does on a board. The unit a board with that fragment runs. Each is checked as one unit, against
  # its additions.
  for entry in "${fragments[@]}"; do
    fragment="${entry%%|*}"
    combined="${entry#*|}"
    [[ -f "$fragment" ]] || { echo "packaging: $fragment is missing"; exit 1; }
    awk -v frag="$fragment" '
      function emit(   line) {
        while ((getline line < frag) > 0) {
          if (line ~ /^\[Service\]/ || line ~ /^#/) continue
          sub(/^ExecStart=\/usr\/bin\/[^ ]*/, "ExecStart=/bin/true", line)   # the stand-in, as for the base
          print line
        }
        close(frag)
      }
      /^\[Install\]/ && !done { emit(); done = 1 }
      { print }
      END { if (!done) emit() }
    ' "$work/lemnosd.service" >"$work/$combined"
    sed -i '/^$/d' "$work/$combined"
  done
  verified="$(cd "$work" && systemd-analyze verify --man=no ./*.service 2>&1)" || {
    echo "$verified" >&2
    exit 1
  }
  # Every unit's rows and overall score, one unit at a time.
  bad=0
  # Scored on a copy without the line `DefaultDependencies=no`. systemd-analyze treats a unit with that line as
  # early-boot and stops crediting its namespacing (ProtectSystem=, PrivateTmp=, ProtectHome=, RootDirectory=)
  # as protection, so the shipped unit would score above its limit for a reason that is not a weaker sandbox.
  # systemd still applies the namespacing to such a unit: this is the analyzer's view, not the runtime's (the
  # namespace under DefaultDependencies=no is to be shown in a QEMU guest under systemd: board-runs/WDT-ORDER.md,
  # section 0, which is not yet proven). The limit
  # stays at 2.5; the shipped unit must keep the three directives below, which are asserted by name.
  for shipped in ProtectSystem=strict PrivateTmp=yes ProtectHome=yes; do
    grep -qx -- "$shipped" "$work/lemnosd.service" || {
      echo "packaging: lemnosd.service does not set $shipped; the exposure score is scored without DefaultDependencies=no, so the namespacing must be asserted by name" >&2
      exit 1
    }
  done
  for unit in "$work"/*.service; do
    name="$(basename "$unit")"
    mkdir -p "$work/score"
    scored="$work/score/$name"
    grep -vx -- 'DefaultDependencies=no' "$unit" >"$scored"
    out="$(systemd-analyze security --offline=true "$scored" 2>&1)"
    score="$(awk '/Overall exposure level/ { print $7 }' <<<"$out")"
    [[ -n "$score" ]] || { echo "packaging: no exposure level read for $name"; exit 1; }
    rows="$(awk '/^✗/ { print $2 ";" $NF }' <<<"$out")"
    limit=$max_tenths
    home=""
    for entry in "${fragments[@]}"; do
      if [[ "${entry#*|}" == "$name" ]]; then home="${entry%%|*}"; fi
    done
    # The additions that belong to another unit must not be in this one.
    while IFS=';' read -r item found; do
      for entry in "${additions[@]}"; do
        rest="${entry#*;}"
        if [[ "${rest%%;*}" == "$item" && "${entry%%;*}" != "$name" ]]; then
          echo "packaging: $item is an addition of ${entry%%;*}, so it belongs in that unit's fragment, not in $name"
          bad=1
        fi
      done
    done <<<"$rows"
    if [[ -n "$home" ]]; then
      for entry in "${additions[@]}"; do
        [[ "${entry%%;*}" == "$name" ]] || continue
        rest="${entry#*;}"
        item="${rest%%;*}"
        rest="${rest#*;}"
        listed="${rest%%;*}"
        found="$(grep -F "$item;" <<<"$rows" | head -1 | cut -d';' -f2 || true)"
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

# The hardware watchdog is opened by slot-init and then lemnosd only (docs/slots.md, "The watchdog chain"),
# and a restart delay is constant: no unit may set
# a systemd watchdog, or a restart delay that grows (packaging/README.md, "The hardware watchdog").
echo "==> watchdog and restart"
for unit in packaging/systemd/*.service; do
  if grep -Eq '^(RuntimeWatchdogSec|ShutdownWatchdogSec|WatchdogSec|RestartSteps|RestartMaxDelaySec)=' "$unit"; then
    echo "packaging: $unit sets a watchdog or a growing restart delay" >&2
    exit 1
  fi
done

# lemnosd's unit keeps its watchdog descriptor in the service manager's store, and orders itself minimally: the
# store is what a restart takes back (a store kept across a stop, or a runtime directory that marks a restart,
# would change that), and no ordering waits for local-fs.target, since a filesystem check must not run in the
# unfed gap (docs/slots.md, "The watchdog chain", window 4; packaging/README.md, "The hardware watchdog").
echo "==> watchdog store and ordering"
lemnosd_unit=packaging/systemd/lemnosd.service
grep -Eq '^FileDescriptorStoreMax=[1-9][0-9]*$' "$lemnosd_unit" || {
  echo "packaging: lemnosd.service keeps no file descriptor store (FileDescriptorStoreMax)" >&2
  exit 1
}
if grep -Eq '^(FileDescriptorStorePreserve|RuntimeDirectoryPreserve)=' "$lemnosd_unit"; then
  echo "packaging: lemnosd.service sets FileDescriptorStorePreserve or RuntimeDirectoryPreserve; a restart is told by the store" >&2
  exit 1
fi
grep -Eq '^DefaultDependencies=no$' "$lemnosd_unit" || {
  echo "packaging: lemnosd.service keeps the implicit dependencies (DefaultDependencies=no is missing)" >&2
  exit 1
}
if grep -Eq '^(After|Requires|Wants|RequiresMountsFor|BindsTo)=.*(local-fs|basic\.target|sysinit\.target)' "$lemnosd_unit"; then
  echo "packaging: lemnosd.service orders itself after local-fs, basic or sysinit: a filesystem check would run in the unfed gap" >&2
  exit 1
fi

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
