#!/usr/bin/env bash
# The gate's check that the mdev.conf `lemnos-ctl tree --mdev` writes is what BusyBox's mdev does:
# runs the BusyBox 1.37.0 and 1.38.0 releases (built by testing/mdev/build-busybox.sh) against each
# board's generated file, and checks the owner, group and mode of each node against the udev rule the
# same board gives for it (`lemnos-ctl tree --udev`). The two renderings come from one list of facts,
# so an equal result is a check of the renderers, and the rules being right is the check of mdev.
#
# A fake sysfs (each device as the kernel describes it: dev, uevent with DEVNAME, subsystem link)
# and a tmpfs /dev are made in a user namespace (testing/mdev/nsrun.sh), or with sudo where the
# runner has no subordinate ids (a root mount namespace, same checks). Each device has a placeholder
# at the path mdev should name (see inner.sh). A decoy device, named like a daemon node but in another
# subsystem, must be left alone: it shows the SUBSYSTEM condition is matched.
#
# Negative: the same reference board with the heap's uevent missing DEVNAME. mdev then names the
# device by its basename, the path rule does not match, and the heap's node stays root:root 600. The
# check must report that, so a rule that stops matching fails the gate.
#
#   check.sh                    (BUSYBOX_DIR: where the builds go; default target/busybox)
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
target="${CARGO_TARGET_DIR:-$repo/target}"
busybox_dir="${BUSYBOX_DIR:-$target/busybox}"
versions=(1.37.0 1.38.0)
mkdir -p "$target"
work="$(mktemp -d "$target/mdev-check.XXXXXX")"
trap '[[ -n "${MDEV_KEEP:-}" ]] || rm -rf "$work"' EXIT

cargo build -q -p lemnos-ctl
ctl="$target/debug/lemnos-ctl"

# How the namespace is made: a user namespace with the caller's subordinate ids when that works here,
# else root in a private mount namespace through sudo (the same checks; mknod is then allowed too,
# but the placeholders are used all the same).
user="$(id -un)"
if unshare -U -m true 2>/dev/null && command -v newuidmap >/dev/null && command -v newgidmap >/dev/null \
   && grep -q "^$user:" /etc/subuid && grep -q "^$user:" /etc/subgid; then
  runner=user
else
  runner=sudo
fi
echo "namespace: $runner"

run_inner() {
  if [[ "$runner" == user ]]; then
    "$here/nsrun.sh" /bin/sh "$here/inner.sh" "$1" "$2"
  else
    sudo -n unshare -m --propagation private /bin/sh "$here/inner.sh" "$1" "$2"
  fi
}

# The devices a board's udev rules name, as the kernel would describe them. Writes sys/ and
# placeholders, and expect (path owner group mode) for each device, from the rule that matches it.
# A decoy in the tty subsystem, named like a glob, matches no rule: mdev then gives it its default,
# 0:0 660 (docs/mdev.txt: "If no line is matched, then the default of 0:0 660 is used").
prepare() {
  local rules="$1" dir="$2" conf="$3" rule subsystem kernel group mode owner name prefix major minor n=100
  mkdir -p "$dir/sys/class" "$dir/sys/dev/char" "$dir/etc"
  : >"$dir/placeholders"
  : >"$dir/expect"
  cp "$conf" "$dir/etc/mdev.conf"
  printf 'root:x:0:0:root:/root:/bin/sh\n' >"$dir/etc/passwd"
  printf 'root:x:0:\nlemnos:x:999:\n' >"$dir/etc/group"
  local have_glob=0
  while IFS= read -r rule; do
    [[ "$rule" == SUBSYSTEM=* ]] || continue
    subsystem="$(sed -n 's/.*SUBSYSTEM=="\([^"]*\)".*/\1/p' <<<"$rule")"
    kernel="$(sed -n 's/.*KERNEL=="\([^"]*\)".*/\1/p' <<<"$rule")"
    group="$(sed -n 's/.*GROUP="\([^"]*\)".*/\1/p' <<<"$rule")"
    mode="$(sed -n 's/.*MODE="\([^"]*\)".*/\1/p' <<<"$rule")"
    owner="$(sed -n 's/.*OWNER="\([^"]*\)".*/\1/p' <<<"$rule")"
    owner="${owner:-root}"
    case "$subsystem" in
      misc) major=10; prefix="" ;;
      dma_heap) major=250; prefix="dma_heap/" ;;
      *) echo "check.sh: no test major for SUBSYSTEM $subsystem" >&2; return 1 ;;
    esac
    name="${kernel//\*/x0}"
    [[ "$kernel" == *\** ]] && have_glob=1
    add_device "$dir" "$subsystem" "$major" "$n" "$prefix$name" 1
    printf '/dev/%s%s %s %s %s\n' "$prefix" "$name" "$owner" "$group" "${mode#0}" >>"$dir/expect"
    n=$((n + 1))
  done <"$rules"
  if ((have_glob)); then
    add_device "$dir" tty 4 "$n" "lemnos-decoy" 1
    printf '/dev/lemnos-decoy root root 660\n' >>"$dir/expect"
  fi
}

# One device: its sysfs entries, and its placeholder unless it is a negative (no DEVNAME).
add_device() {
  local dir="$1" subsystem="$2" major="$3" minor="$4" devname="$5" with_devname="$6" base
  base="$dir/sys/dev/char/$major:$minor"
  mkdir -p "$base" "$dir/sys/class/$subsystem"
  printf '%s:%s\n' "$major" "$minor" >"$base/dev"
  if ((with_devname)); then
    printf 'MAJOR=%s\nMINOR=%s\nDEVNAME=%s\n' "$major" "$minor" "$devname" >"$base/uevent"
  else
    printf 'MAJOR=%s\nMINOR=%s\n' "$major" "$minor" >"$base/uevent"
  fi
  ln -sfn "../../../class/$subsystem" "$base/subsystem"
  printf '%s\n' "$devname" >>"$dir/placeholders"
}

# Runs one BusyBox against one prepared directory and compares its result with the expectation.
check_one() {
  local label="$1" bb="$2" dir="$3" actual
  actual="$(run_inner "$bb" "$dir" 2>/dev/null)" || true
  if diff -u "$dir/expect" <(printf '%s\n' "$actual") >"$dir/diff"; then
    echo "ok   $label: $(wc -l <"$dir/expect") nodes as the udev rules give them"
    return 0
  fi
  echo "FAIL $label"
  cat "$dir/diff"
  return 1
}

status=0
for version in "${versions[@]}"; do
  bb="$("$here/build-busybox.sh" "$busybox_dir/$version" "$version")"
  echo "mdev $version: $bb"
  for board in "$repo"/testing/boards/*.toml; do
    grep -q '^soc' "$board" || continue
    name="$(basename "$board" .toml)"
    dir="$work/$version-$name"
    mkdir -p "$dir"
    "$ctl" tree --socs "$repo/socs" "$board" -o "$dir/overlay.dts" \
      --mdev "$dir/mdev.conf" --udev "$dir/rules"
    prepare "$dir/rules" "$dir" "$dir/mdev.conf"
    check_one "mdev $version $name" "$bb" "$dir" || status=1
  done
  # The negative on the reference board: the heap's uevent without DEVNAME must be reported.
  dir="$work/$version-negative"
  mkdir -p "$dir"
  "$ctl" tree --socs "$repo/socs" "$repo/testing/boards/reference.toml" -o "$dir/overlay.dts" \
    --mdev "$dir/mdev.conf" --udev "$dir/rules"
  prepare "$dir/rules" "$dir" "$dir/mdev.conf"
  # The heap's uevent without its DEVNAME line, as a uevent without it would be.
  sed -i '/^DEVNAME=/d' "$dir/sys/dev/char/250:"*/uevent
  check_one "negative $version (heap uevent without DEVNAME)" "$bb" "$dir" >"$dir/negative.log" || true
  # The symptom must be exactly the heap node left root:root 600; any other difference is not the case.
  if grep -qx '+/dev/dma_heap/linux,cma root root 600' "$dir/negative.log" \
     && grep -qx -- '-/dev/dma_heap/linux,cma root lemnos 660' "$dir/negative.log"; then
    echo "ok   negative $version: the heap node without DEVNAME is root root 600 and is reported"
  else
    echo "FAIL negative $version: the check did not report the heap node without DEVNAME"
    cat "$dir/negative.log"
    status=1
  fi
done
exit "$status"
