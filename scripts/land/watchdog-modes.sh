# watchdog_modes <tree>: the watchdog modes the off part runs, one per line: exactly the ones
# <tree>/testing/qemu/run-watchdog.sh lists (--modes), so the two never drift. A script that fails, or
# names no mode, fails here: the off part never passes on a list of nothing.
watchdog_modes() {
  local out
  out=$(bash "$1/testing/qemu/run-watchdog.sh" --modes) || { echo "off: run-watchdog.sh --modes failed" >&2; return 1; }
  out=$(printf '%s\n' $out)
  [ -n "$out" ] || { echo "off: run-watchdog.sh named no watchdog mode" >&2; return 1; }
  printf '%s\n' "$out"
}
