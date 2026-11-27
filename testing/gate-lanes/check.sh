#!/usr/bin/env bash
# Plants one failure in each gate and lander lane that once could pass on a failure, and checks that the lane
# now fails on it, with its own message. A lane that passes, or fails for another reason, is NOT CAUGHT.
#
#   testing/gate-lanes/check.sh [TREE]
#
# TREE is the source tree whose scripts are tested (default: this one). Run against a tree from before the
# fixes, every case is NOT CAUGHT; against this one, every case is caught. Nothing in TREE is changed: the
# cases that plant files work on a copy of it, and stand-in tools go first on PATH. The real git history the
# consumer-compile cases read is this checkout's. CARGO_TARGET_DIR and BUSYBOX_DIR are used as the gate uses
# them (caches only).
#
# What each case plants, and the lane's old way through it:
#   cc-trigger     git fails inside the trigger check         old: "skipped", exit 0
#   cc-newerrs     comparing the error keys fails             old: "PASS", exit 0
#   cc-cache       a cached result whose coverage is empty    old: "PASS", exit 0
#   shipped        cargo tree fails for the licence list      old: the list was empty, the step held
#   libraries      cargo tree fails for an shm crate          old: no dependency seen, the step held
#   packaging      systemd-analyze verify: a unit not found   old: "skipped", "packaging: ok", exit 0
#   mdev-status    mdev fails and leaves the nodes as placed  old: inner.sh exited 0
#   boundary-rule  a fixture caught by a different rule       old: any failure counted as caught
#   notes-command  a runnable command block in notes/         old: --list ran nothing and held
#   notes-key      a board-run record left out of the key     old: testing/board-runs/ was a note
#   dtc-guard      dtc's version is not the pinned one        old: exit 1 with no message (the guard was a comment)
#   gate-once      a gate that fails with the starved status  old: run again, and green on the second run
#   kconfig-cmp    a required symbol behind `A && B=n`        old: the expression was skipped, A not required
# check-features takes its sets into variables the same way (not planted here: it builds every crate), and
# the gate's stall and starved verdicts have their own check, testing/gate-bounds/check.sh.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
tree="$(cd "${1:-$repo}" && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/gate-lanes.XXXXXX")"
trap 'rm -rf -- "$work"' EXIT
fails=0 case_start=$SECONDS

# verdict <case> <status> <output file> <wanted status: nonzero|N> <needle>
verdict() {
  local name="$1" rc="$2" out="$3" want="$4" needle="$5" ok=1
  if [ "$want" = nonzero ]; then [ "$rc" != 0 ] || ok=0; else [ "$rc" = "$want" ] || ok=0; fi
  grep -qF -- "$needle" "$out" || ok=0
  local took=$((SECONDS - case_start)); case_start=$SECONDS
  if [ "$ok" = 1 ]; then
    echo "caught      $name (exit $rc: $needle; ${took} s)"
  else
    echo "NOT CAUGHT  $name (exit $rc, wanted exit $want with: $needle)"
    sed 's/^/    | /' "$out" | tail -n 8
    fails=$((fails + 1))
  fi
}

# A copy of TREE without its build output, for the cases that plant files.
copy="$work/copy"
mkdir -p "$copy"
tar -C "$tree" --exclude=./target --exclude=./.git -cf - . | tar -C "$copy" -xf -

# Stand-in tools. Each passes every call through to the real tool but the one it plants.
bin="$work/bin"
mkdir -p "$bin"
real_git="$(command -v git)" real_cargo="$(command -v cargo)" real_sa="$(command -v systemd-analyze)"
cat >"$bin/git" <<EOF
#!/bin/bash
for a in "\$@"; do [ "\$a" = --name-only ] && [ -n "\${PLANT_GIT_DIFF_FAILS:-}" ] && { echo "fatal: planted" >&2; exit 128; }; done
exec "$real_git" "\$@"
EOF
cat >"$bin/cargo" <<EOF
#!/bin/bash
case " \$* " in
  *" tree "*"lemnos-firmware-check"*"{l}"*) [ -n "\${PLANT_CARGO_LICENCES_FAIL:-}" ] && { echo "error: planted" >&2; exit 101; } ;;
  *" tree "*"--depth 1"*) [ -n "\${PLANT_CARGO_DEPTH_FAILS:-}" ] && { echo "error: planted" >&2; exit 101; } ;;
  *" clippy "*|*" test "*) [ -n "\${PLANT_CARGO_DEPTH_FAILS:-}" ] && exit 0 ;;
esac
exec "$real_cargo" "\$@"
EOF
cat >"$bin/systemd-analyze" <<EOF
#!/bin/bash
[ "\${1:-}" = verify ] && { echo "Unit planted.target not found."; exit 1; }
exec "$real_sa" "\$@"
EOF
chmod +x "$bin"/*

# ---- consumer-compile --------------------------------------------------------------------------------------
# One consumer: this checkout at its HEAD, with its lock's Lemnos names cached (so no lock is read from git).
cc_env() { # <case dir>
  local d="$1" head
  head="$(git -C "$repo" rev-parse HEAD)"
  mkdir -p "$d"/{data/fixes,cache/locks,cache/baseline,work,logs}
  printf 'self\t%s\t%s\t.\tCargo.lock\t\n' "$repo" "$head" >"$d/data/consumers.tsv"
  : >"$d/data/shims.tsv"; : >"$d/data/fields.tsv"
  printf 'lemnos-core\t0.0.0\n' >"$d/cache/locks/self-${head:0:8}.names"
  {
    for v in LAND_WORKTREE LAND_STATE_DIR LAND_UNIT_DIR LAND_LOCK_FILE LAND_SCRATCH LAND_TMP LAND_GUARD_BIN \
      LAND_QK_DIR LAND_KERNEL_TARBALLS LAND_BUSYBOX_DIR LAND_REMOTE LAND_UNIT_PREFIX; do echo "$v=$d/unused"; done
    for v in LAND_CPUS_GATE LAND_CPUS_CC LAND_CPUS_QEMU LAND_CPUS_OFF; do echo "$v=0"; done
    for v in LAND_JOBS_GATE LAND_JOBS_CC LAND_JOBS_KERNEL LAND_JOBS_QEMU LAND_JOBS_QEMU_CARGO LAND_JOBS_OFF; do echo "$v=1"; done
    echo "LAND_PR_REPO="
    echo "LAND_REPO=$repo"; echo "LAND_CC_DATA=$d/data"; echo "LAND_CC_CACHE=$d/cache"; echo "LAND_CC_WORK=$d/work"
  } >"$d/land.env"
}
# Base and candidate are both HEAD: a CI checkout is one commit deep, and no case needs two.
base="$(git -C "$repo" rev-parse HEAD)" cand="$base"

d="$work/cc-trigger"; cc_env "$d"
rc=0; PATH="$bin:$PATH" PLANT_GIT_DIFF_FAILS=1 LEMNOS_LAND_ENV="$d/land.env" \
  bash "$tree/scripts/land/consumer-compile.sh" "$base" "$cand" "$d/logs" >"$d/out" 2>&1 || rc=$?
verdict cc-trigger "$rc" "$d/out" 2 "the trigger check failed"

# The other two run the copy's consumer-compile with a stand-in cc.py: its trigger names the consumer, and
# each side's result is already cached, so nothing is compiled.
cc_standin() { # <case dir> <newerrs fails: 0|1>
  local d="$1" land="$copy/scripts/land"
  [ -f "$land/cc_real.py" ] || mv "$land/cc.py" "$land/cc_real.py"
  cat >"$land/cc.py" <<EOF
import os, sys
cmd = sys.argv[1] if len(sys.argv) > 1 else ""
if cmd == "trigger":
    print("TRIGGER lemnos-core engine/core/src/lib.rs: planted"); print("AFFECT self lemnos-core")
    print("CHECKED 1 changed files of locked crates"); print("RESULT yes"); sys.exit(0)
if cmd == "newerrs" and os.environ.get("PLANT_NEWERRS_FAILS"):
    sys.exit("planted")
os.execv(sys.executable, [sys.executable, "-I", os.path.join(os.path.dirname(__file__), "cc_real.py")] + sys.argv[1:])
EOF
}
cc_cached() { # <case dir> <coverage text>
  local d="$1" cov="$2" r8 toolh sha
  r8="$(git -C "$repo" rev-parse HEAD | cut -c1-8)"
  # The tool hash the script computes: these files, its own path's contents and the toolchain.
  toolh=$( { cat "$copy/scripts/land/cc.py" "$d/data/shims.tsv" "$d/data/consumers.tsv" "$d/data/fields.tsv" \
    "$copy/scripts/land/consumer-compile.sh"; cat "$d"/data/fixes/*.json 2>/dev/null; "$HOME/.cargo/bin/rustc" -vV;
    "$HOME/.cargo/bin/cargo" -V; } | sha256sum | cut -c1-10)
  for sha in "$base" "$cand"; do
    : >"$d/cache/baseline/self-$r8-${sha:0:8}-$toolh.keys"
    printf '%s' "$cov" >"$d/cache/baseline/self-$r8-${sha:0:8}-$toolh.keys.cov"
  done
}
d="$work/cc-newerrs"; cc_env "$d"; cc_standin "$d"; cc_cached "$d" $'5 5\n'
rc=0; PLANT_NEWERRS_FAILS=1 LEMNOS_LAND_ENV="$d/land.env" \
  bash "$copy/scripts/land/consumer-compile.sh" "$base" "$cand" "$d/logs" >"$d/out" 2>&1 || rc=$?
verdict cc-newerrs "$rc" "$d/out" 2 "comparing the errors failed"

d="$work/cc-cache"; cc_env "$d"; cc_standin "$d"; cc_cached "$d" ""
rc=0; LEMNOS_LAND_ENV="$d/land.env" \
  bash "$copy/scripts/land/consumer-compile.sh" "$base" "$cand" "$d/logs" >"$d/out" 2>&1 || rc=$?
verdict cc-cache "$rc" "$d/out" 2 "cached result is not whole"

# ---- cargo-tree lanes --------------------------------------------------------------------------------------
rc=0; (cd "$copy" && PATH="$bin:$PATH" PLANT_CARGO_LICENCES_FAIL=1 bash scripts/check-shipped.sh) >"$work/shipped.out" 2>&1 || rc=$?
verdict shipped "$rc" "$work/shipped.out" nonzero "error: planted"
rc=0; (cd "$copy" && PATH="$bin:$PATH" PLANT_CARGO_DEPTH_FAILS=1 bash scripts/check-libraries.sh) >"$work/libraries.out" 2>&1 || rc=$?
verdict libraries "$rc" "$work/libraries.out" nonzero "error: planted"

# ---- packaging ---------------------------------------------------------------------------------------------
rc=0; (cd "$copy" && PATH="$bin:$PATH" bash scripts/check-packaging.sh) >"$work/packaging.out" 2>&1 || rc=$?
verdict packaging "$rc" "$work/packaging.out" 1 "Unit planted.target not found."

# ---- mdev --------------------------------------------------------------------------------------------------
m="$work/mdev"; mkdir -p "$m/sys" "$m/etc"
printf 'root:x:0:0::/:/bin/sh\n' >"$m/etc/passwd"; printf 'root:x:0:\n' >"$m/etc/group"; : >"$m/etc/mdev.conf"
printf 'planted\n' >"$m/placeholders"
printf '#!/bin/sh\necho "mdev: planted failure" >&2\nexit 1\n' >"$m/busybox"; chmod +x "$m/busybox"
# The namespace is made as testing/mdev/check.sh makes it: a user namespace where that works, else sudo.
user="$(id -un)"
if unshare -U -m true 2>/dev/null && command -v newuidmap >/dev/null && command -v newgidmap >/dev/null \
   && grep -q "^$user:" /etc/subuid && grep -q "^$user:" /etc/subgid; then
  ns=("$tree/testing/mdev/nsrun.sh")
elif [ "${GITHUB_ACTIONS:-}" = true ]; then
  ns=(sudo -n unshare -m --propagation private)
else
  # sudo only on CI. Not run is counted as not caught, never as a pass.
  ns=()
fi
rc=0
if [ "${#ns[@]}" -gt 0 ]; then
  "${ns[@]}" /bin/sh "$tree/testing/mdev/inner.sh" "$m/busybox" "$m" >"$m/out" 2>&1 || rc=$?
else
  echo "not run: no user namespace with subordinate ids for $user, and sudo is used only on CI" >"$m/out"; rc=125
fi
# inner.sh prints each node as placed: the planted one is left as it was made, root root 600 in the namespace.
verdict mdev-status "$rc" "$m/out" 1 "/dev/planted"

# ---- boundaries --------------------------------------------------------------------------------------------
# The product-name fixture carries a different violation: a file named after hardware, and no product name.
f="$copy/testing/boundary/product-name"
rm -rf -- "${f:?}"/*; mkdir -p "$f/engine/x/src"; printf 'pub fn f() {}\n' >"$f/engine/x/src/imu.rs"
# --fixtures runs the self-test alone; a tree from before it has no such mode and runs the whole check.
rc=0; (cd "$copy" && bash scripts/check-boundaries.sh --fixtures) >"$work/boundary.out" 2>&1 || rc=$?
verdict boundary-rule "$rc" "$work/boundary.out" 1 "planted violation product-name was not caught by its rule"
rm -rf -- "${f:?}"; cp -a "$tree/testing/boundary/product-name" "$f"

# ---- notes -------------------------------------------------------------------------------------------------
mkdir -p "$copy/notes"
printf '# Planted\n\n```sh\ntrue\n```\n' >"$copy/notes/planted.md"
rc=0; (cd "$copy" && python3 scripts/check-docs.py --list) >"$work/notes.out" 2>&1 || rc=$?
verdict notes-command "$rc" "$work/notes.out" 1 "a command block in the notes"
rm -f -- "$copy/notes/planted.md"

line="$(grep -m1 '^notes_re=' "$tree/scripts/land/l-train.sh")"
notes_re=""; eval "$line"
if printf 'testing/board-runs/x.md\n' | grep -qE -- "$notes_re"; then rc=0; echo "board-runs/x.md is a note: $notes_re" >"$work/key.out"
else rc=1; echo "testing/board-runs/ is in the gate key: $notes_re" >"$work/key.out"; fi
verdict notes-key "$rc" "$work/key.out" 1 "testing/board-runs/ is in the gate key"

# ---- dtc guard ---------------------------------------------------------------------------------------------
guard="$(grep -m1 -F 'Version: DTC $version' "$tree/testing/devtree/build-dtc.sh")"
rc=0; bash -c "set -euo pipefail; probe='Version: DTC 0.0.1'; version=1.7.2; $guard; echo reached-end" >"$work/dtc.out" 2>&1 || rc=$?
verdict dtc-guard "$rc" "$work/dtc.out" 1 "built Version: DTC 0.0.1, not 1.7.2"

# ---- the lander's gate run ---------------------------------------------------------------------------------
g="$work/gate"; mkdir -p "$g/slots"
rc=0
(
  export SLOT_DIR="$g/slots" SLOT_TOTAL=2 LAND_RESERVE_SLOTS=1 QUEUE_STOP_FILE="$g/stop"
  . "$tree/scripts/land/gate-attempts.sh"
  # A gate that fails with the old starved status (3) on its first run and passes on any later one.
  printf '#!/bin/bash\nn=$(( $(cat "%s/calls" 2>/dev/null || echo 0) + 1 )); echo "$n" >"%s/calls"\n[ "$n" -ge 2 ] || exit 3\n' "$g" "$g" >"$g/standin"
  chmod +x "$g/standin"
  gate_attempts "$g" "$g/standin"
) >"$g/out" 2>&1 || rc=$?
calls="$(cat "$g/calls" 2>/dev/null || echo 0)"
echo "the gate ran $calls time(s)" >>"$g/out"
[ "$calls" = 1 ] || rc=0
verdict gate-once "$rc" "$g/out" 3 "the gate ran 1 time(s)"

# ---- kernel config ------------------------------------------------------------------------------------------
rc=0; PYTHONDONTWRITEBYTECODE=1 python3 -I - "$tree/scripts" >"$work/kconfig.out" 2>&1 <<'PY' || rc=$?
import importlib.util, sys
spec = importlib.util.spec_from_file_location("ckc", sys.argv[1] + "/check-kernel-config.py")
ckc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ckc)
sym = lambda deps: {"deps": deps, "prompt": True}
got = sorted(ckc.closure({"ROOT": sym(["A && B=n"]), "A": sym([]), "B": sym([])}, {"ROOT": "y"}, "ROOT"))
if got == ["A"]:
    print("A is required of ROOT through `A && B=n`")
    sys.exit(1)
print(f"required of ROOT: {got}")
PY
verdict kconfig-cmp "$rc" "$work/kconfig.out" 1 "A is required of ROOT through \`A && B=n\`"

if [ "$fails" = 0 ]; then echo "gate-lanes: every planted failure caught"; exit 0; fi
echo "gate-lanes: $fails planted failure(s) not caught"
exit 1
