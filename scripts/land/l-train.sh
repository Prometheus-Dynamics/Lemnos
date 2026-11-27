#!/bin/bash
# Local lander (user rule, 2026-11-17: GitHub CI never gates a merge on next).
# Usage: l-train.sh <id>...   Stacks <remote>/pr/<id> onto <remote>/next in order (a PR that does not apply
# is left out and named), runs the full gate and both QEMU harnesses on this machine, side by side, and
# on green pushes the stack to next (fast-forward only) and closes each PR with the landing rev.
# Machine settings come from the env file (LEMNOS_LAND_ENV, see env-load.sh and env.example).
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
. "$HERE/env-load.sh" || exit 2
T=$LAND_STATE_DIR; REPO=$LAND_REPO; W=$LAND_WORKTREE; R=$LAND_PR_REPO
# The remote (a name or a URL) is fetched into $RN/: its branches never share names with a local remote's.
RMT=$LAND_REMOTE; RN=refs/land/remote
# LTRAIN_BASE: stack on another train's head instead of next (a train behind a train); it pushes
# only once next has reached that base. LTRAIN_AFTER: the unit of the train ahead, waited for before the push.
BASE_REF=${LTRAIN_BASE:-$RN/next}
cred=(-c credential.helper= -c 'credential.helper=!gh auth git-credential')
# The gate's backstop (seconds). It is 4 x the sum of the gate's step budgets (scripts/gate.sh, CPU-seconds, with
# loop bodies counted per run): 27291 x 4 = 109164. A progressing gate never reaches it; a step that stops
# progressing is killed by its own watch long before. Its only other job is to stop a gate the watch cannot see.
GATE_BACKSTOP_S=109164
. "$HERE/gate-attempts.sh"
export QUEUE_STOP_FILE="$LAND_STATE_DIR/QUEUE-STOPPED"
exec 8>>"$LAND_LOCK_FILE"; flock 8
if [ -s "$QUEUE_STOP_FILE" ]; then echo "queue stopped: $(cat "$QUEUE_STOP_FILE"); clear it by hand before landing"; exit 2; fi
n=$(( $(cat "$T/counter" 2>/dev/null || echo 0) + 1 )); echo $n > "$T/counter"; L=$T/$n; mkdir -p "$L"
echo "== ltrain $n $(date -u +%FT%TZ): $*"
git -C "$REPO" fetch -q "$RMT" "+refs/heads/*:$RN/*" || { echo "fetch failed"; exit 2; }
asis=""; [ "${1:-}" = --as-is ] && { asis=1; shift; }
in=(); out=()
if [ -n "$asis" ]; then
  # --as-is <id>...: the stack already in the worktree (an earlier run's), unchanged; it must sit on next.
  cd "$W" || exit 2
  base=$(git rev-parse $RN/next)
  git merge-base --is-ancestor "$base" HEAD || { echo "--as-is: the stack is not on $RMT next $base"; exit 2; }
  [ -z "$(git status --porcelain --untracked-files=no)" ] || { echo "--as-is: the stack worktree has changes"; exit 2; }
  in=("$@"); echo "as-is: $(git rev-parse --short HEAD) on $base for ${in[*]}"
  set --
else
[ -d "$W" ] || git -C "$REPO" worktree add -q --detach "$W" "$BASE_REF" || exit 2
cd "$W" && git checkout -q --detach "$BASE_REF" && git reset -q --hard "$BASE_REF" && git clean -qfdx -e target -e target-qemu -e target-off || exit 2
base=$(git rev-parse HEAD); echo "base $base $(git log -1 --format=%s | cut -c1-70)"
fi
# Cargo.lock is the landing's (AGENTS.md); a PR that committed one conflicts only there. Such a
# conflict takes the stack's lock (it is written again below); any other conflict leaves the PR out.
pick_range() {
  # Only the commits whose patch is not on the stack already: a PR stacked on one that landed (or that
  # is in this train) carries that one's commits again, and applying them twice conflicts.
  local commits; commits=$(git rev-list --reverse --no-merges --cherry-pick --right-only "HEAD...${1#*..}")
  [ -n "$commits" ] || { echo "every commit is on the stack already"; return 0; }
  # shellcheck disable=SC2086
  git cherry-pick --allow-empty --empty=drop $commits && return 0
  while :; do
    local u; u=$(git diff --name-only --diff-filter=U)
    if [ -z "$u" ] && ! git rev-parse -q --verify CHERRY_PICK_HEAD >/dev/null; then return 0; fi
    # A commit already on the stack in another form (a landed PR it was stacked on) comes out empty: skip it.
    if [ -z "$u" ] && git diff --cached --quiet && git diff --quiet; then
      echo "already applied: skipping $(git rev-parse --short CHERRY_PICK_HEAD)"
      git cherry-pick --skip && ! git rev-parse -q --verify CHERRY_PICK_HEAD >/dev/null && return 0
      continue
    fi
    [ "$u" = "Cargo.lock" ] || return 1
    echo "lock conflict: taking the stack's Cargo.lock"
    git checkout --ours -- Cargo.lock && git add Cargo.lock || return 1
    local rc
    if git diff --cached --quiet; then git cherry-pick --skip; rc=$?; else GIT_EDITOR=true git cherry-pick --continue; rc=$?; fi
    if [ $rc = 0 ] && ! git rev-parse -q --verify CHERRY_PICK_HEAD >/dev/null; then return 0; fi
  done
}
for id; do
  b=$RN/pr/$id
  git rev-parse -q --verify "$b" >/dev/null || { echo "out $id: no branch $b"; out+=("$id"); continue; }
  mb=$(git merge-base "$base" "$b")
  if pick_range "$mb..$b" > "$L/pick-$id.log" 2>&1; then in+=("$id"); echo "in  $id $(git rev-parse --short HEAD)"
  else git cherry-pick --abort 2>/dev/null; echo "out $id: does not apply on the stack (see $L/pick-$id.log)"; out+=("$id"); fi
done
[ ${#in[@]} -gt 0 ] || { echo "nothing to land"; exit 1; }
[ -n "$asis" ] || {
# Each PR leaves Cargo.lock to the landing (AGENTS.md); the stack's lock is written once, here, and committed.
"$HOME/.cargo/bin/cargo" metadata --format-version 1 >/dev/null 2>"$L/lock.log" || { echo "lock: cargo metadata failed"; cat "$L/lock.log"; exit 2; }
if ! git diff --quiet -- Cargo.lock; then git add Cargo.lock && git commit -q -m "Cargo.lock for the crates landed in ltrain $n" && echo "lock committed $(git rev-parse --short HEAD)"; fi
}
head=$(git rev-parse HEAD)
# TMPDIR never under /dev or /sys: tests refuse fake device trees there (backends/linux). Each train gets train-<n> under LAND_TMP.
export TMPDIR=$LAND_TMP; case "$TMPDIR" in /dev/*|/sys/*) echo "TMPDIR $TMPDIR is under /dev or /sys"; exit 2;; esac; TMPDIR="$TMPDIR/train-$n"; mkdir -p "$TMPDIR"; export TMPDIR PATH="$LAND_GUARD_BIN:$HOME/.cargo/bin:$PATH" CARGO_INCREMENTAL=0
# The gate's own CARGO_HOME (setup: offline, no automatic cleaning). The crates of every lock file the gate builds
# are fetched into it here, online, before the gate starts; the gate then runs offline in it, so it never
# downloads and never waits on another program's lock on ~/.cargo. fetch.log records each lock file's hash,
# the sources, and a hash over the fetched crate files; a failed fetch stops the train before any part runs.
fetch_home() {
  local lock rc=0 locks
  locks=$(git ls-files '*Cargo.lock')
  echo "fetch: CARGO_HOME=$LAND_CARGO_HOME"
  for lock in $locks; do
    echo "fetch: $lock sha256 $(sha256sum "$lock" | cut -d' ' -f1)"
    CARGO_HOME=$LAND_CARGO_HOME "$HOME/.cargo/bin/cargo" fetch --locked --config net.offline=false \
      --manifest-path "$(dirname "$lock")/Cargo.toml" || { echo "fetch: $lock: cargo fetch failed"; rc=1; }
  done
  # shellcheck disable=SC2086 # the lock file paths have no spaces
  echo "fetch: sources: $(sed -n 's/^source = "\([a-z]*+[^?#"]*\)[^#"]*\(#[0-9a-f]*\)\{0,1\}".*/\1\2/p' $locks | sort -u | tr '\n' ' ')"
  local crates; crates=$(cd "$LAND_CARGO_HOME/registry/cache" 2>/dev/null && find . -name '*.crate' -print0 | LC_ALL=C sort -z | xargs -0r sha256sum)
  echo "fetch: $(printf '%s' "$crates" | grep -c . ) registry crate files, sha256 over their sums $(printf '%s\n' "$crates" | sha256sum | cut -c1-64)"
  echo "fetch: $(find "$LAND_CARGO_HOME/git/checkouts" -mindepth 2 -maxdepth 2 -type d 2>/dev/null | wc -l) git checkouts"
  return $rc
}
fetch_home > "$L/fetch.log" 2>&1 || { echo "fetch: the gate's CARGO_HOME could not be filled (see $L/fetch.log)"; tail -5 "$L/fetch.log"; exit 2; }
grep -E '^fetch: ([0-9]+ registry|sources)' "$L/fetch.log" | cut -c1-200
# The QEMU kernels, cached by the same inputs as CI's cache keys; built here when missing.
kernel() { # <dir-name> <config> [--modules-off]
  # The modules-off kernel builds kernel/ in, so its key also holds kernel/'s tree hash.
  local key; key=$( { cat testing/qemu/build-kernel.sh "testing/qemu/$2"; [ "$1" = off ] && git rev-parse HEAD:kernel; } | sha256sum | cut -c1-16)
  local qk=$LAND_QK_DIR; local d=$qk/$1-$key; [ -L "$qk" ] && mkdir -p "$(readlink "$qk")"; mkdir -p "$d"
  # The kernel version is the one build-kernel.sh pins; its tarball comes from the local cache (hash checked by build-kernel.sh).
  local ver; ver=$(sed -n 's/^version=\([0-9.]*\).*/\1/p' testing/qemu/build-kernel.sh | head -1)
  [ -n "$ver" ] || { echo "kernel $1: no version= line in build-kernel.sh" > "$L/kernel-$1.log"; return 1; }
  [ -f "$d/linux-$ver.tar.xz" ] || cp "$LAND_KERNEL_TARBALLS/linux-$ver.tar.xz" "$d/" 2>/dev/null || true
  # A tree whose extraction never finished (a stopped run) is removed: build-kernel.sh only extracts when the tree is missing.
  [ -f "$d/.extracted" ] || rm -rf "$d/linux-$ver" "$d/build"
  # Each kernel depends only on its key's inputs (and the compilers, checked below): once built, it is reused as is.
  # One builder per tree: a stopped or parallel build never leaves a half tree another run takes as built.
  exec 7>>"$d.lock"; flock 7
  # Compiler check: a Rust kernel and its modules need the same rustc, bindgen and C compiler. The tree's
  # .config records the ones that built it; a tree built by others is built again (the key does not cover them).
  if [ -f "$d/.built" ] && ! compilers_match "$d/build/.config"; then
    echo "kernel $1: built by other compilers ($(compilers_differ "$d/build/.config")): building again" > "$L/kernel-$1.log"
    rm -f "$d/.built"
  fi
  if [ -f "$d/.built" ] && [ -f "$d/build/arch/x86/boot/bzImage" ]; then echo "kernel $1: cached" >> "$L/kernel-$1.log"; exec 7>&-; echo "$d"; return 0; fi
  JOBS=$LAND_JOBS_KERNEL testing/qemu/build-kernel.sh "$d" ${3:-} > "$L/kernel-$1.log" 2>&1 || { echo "kernel $1 build failed" >> "$L/kernel-$1.log"; return 1; }
  touch "$d/.extracted" "$d/.built"; exec 7>&-
  # Modules-off trees for older kernel/ trees: the newest three are kept, the rest removed (each is a whole kernel build).
  if [ "$1" = off ]; then ls -1dt "$qk"/off-*/ 2>/dev/null | tail -n +4 | while read -r old; do old=${old%/}
    [ "$old" = "$d" ] && continue; exec 8>>"$old.lock"; flock -n 8 && rm -rf --one-file-system "${old:?}" "${old:?}.lock"; exec 8>&-; done; fi
  python3 scripts/check-kernel-config.py --tree "$d/linux-$ver" --config "$d/build/.config" --arch x86 >> "$L/kernel-$1.log" 2>&1 || return 1
  echo "$d"
}
compilers_now() { printf 'CONFIG_RUSTC_VERSION_TEXT="%s"\nCONFIG_BINDGEN_VERSION_TEXT="%s"\nCONFIG_CC_VERSION_TEXT="%s"\n' \
  "$(rustc --version)" "$(bindgen --version 2>/dev/null)" "$(clang --version | head -1)"; }
compilers_differ() { compilers_now | grep -vxFf <(grep -E '^CONFIG_(RUSTC|BINDGEN|CC)_VERSION_TEXT=' "$1" 2>/dev/null) | cut -d= -f1 | tr '\n' ' '; }
compilers_match() { [ -z "$(compilers_differ "$1")" ]; }
# A cargo target dir belongs to one checkout. Workspace artifacts carry the checkout's absolute paths
# (CARGO_MANIFEST_DIR, build-script OUT_DIR) and cargo still calls them fresh from another checkout at
# the same relative paths, so a target built from another worktree is removed here, never reused
# (2026-11-18: the QEMU targets in the scratch dir had been built from the HDD stack worktree and were
# then used from the NVMe one).
own_target() { # <target dir>
  local t=$1 mark="$1/.lemnos-owner"
  if [ -d "$t" ] && [ "$(cat "$mark" 2>/dev/null)" != "$W" ]; then
    echo "target $t was built from $(cat "$mark" 2>/dev/null || echo 'an unrecorded checkout'): removed"
    rm -rf --one-file-system "$t"
  fi
  mkdir -p "$t" && echo "$W" > "$mark"
}
own_target "$W/target"
own_target "$LAND_SCRATCH/target-qemu"
own_target "$LAND_SCRATCH/target-off"
# No ceremony runs (user rule, 2026-11-23): a part runs only when something it reads changed. Each
# part is keyed by the tree of the paths it reads, and a part already green for its key is not run
# again. The notes (TODO.md, PARKED.md, notes/) are read only by the notes checks: check-docs refuses
# a runnable command block in them, and its --list mode with check-boundaries is everything the gate
# reads them for. testing/board-runs/ is not a note here: documents' commands read it. docs/ is read by the gate's tests (include_str! of docs/wire.md, docs/daemon.md and an
# example board file, all in test code) and by its kernel-config and documents checks, and by
# nothing the QEMU, modules-off or consumer lanes run.
notes_re='^(TODO\.md|PARKED\.md|notes/)'
key_of() { git ls-tree -r HEAD | RE="$1" awk -F'\t' '$2 !~ ENVIRON["RE"]' | sha256sum | cut -c1-40; }
gate_key=$(key_of "$notes_re")
code_key=$(key_of "^(TODO\\.md|PARKED\\.md|notes/|testing/board-runs/|docs/)")
mkdir -p "$T/green"
echo "keys: gate $gate_key, code $code_key"
part() { # <name> <key> <command...>: runs it unless already green for its key; marks it green on success
  local p=$1 key=$2; shift 2
  if [ -f "$T/green/$key.$p" ]; then echo 0 > "$L/$p.rc"; echo "$p: green already for its inputs ($key)" > "$L/$p.log"; return; fi
  "$@"; local rc=$?; echo $rc > "$L/$p.rc"; [ $rc = 0 ] && touch "$T/green/$key.$p"
}
# Hard bounds (2026-11-18, after the mdev hang held T74 for 1 h 40 min): about 3x the slowest green
# time over trains 115-133 (qemu 318 s, off 181 s per part; the whole gate, 3600 s, is a backstop over its own
# per-step bounds). timeout signals its whole process group; a part past its bound is killed and named.
# The three parts get their own cores. The gate's test step runs on every core it is given, and
# the QEMU guests (2 vCPUs each) ran on the same cores: since the gate went parallel (2026-11-18),
# guest checks with waits in guest time failed here and there with no kernel change in the stack
# (irq, pisp_fe, pisp_be, gadget). Gate 6-23, modules harness 24-27, modules-off 28-31.
# The notes checks: what reads the notes, run every train (seconds).
( { scripts/check-boundaries.sh && python3 scripts/check-docs.py --list >/dev/null; } > "$L/notes.log" 2>&1; echo $? > "$L/notes.rc" ) &
( part gate "$gate_key" gate_attempts "$L" bash -c 'CARGO_HOME=$6 CARGO_TARGET_DIR=$0/target CARGO_BUILD_JOBS=$3 BUSYBOX_DIR=$1 taskset -c $4 /usr/bin/time -o "$2/gate.time" -f "%e %U %S" timeout -k 30 $5 ./scripts/gate.sh; r=$?; [ $r = 124 -o $r = 137 ] && echo "lander: gate exceeded its bound of $5 s (killed)"; exit $r' "$W" "$LAND_BUSYBOX_DIR" "$L" "$LAND_JOBS_GATE" "$LAND_CPUS_GATE" "$GATE_BACKSTOP_S" "$LAND_CARGO_HOME" ) &
qemu_part() { local d; d=$(kernel modules kernel.config) || return 1
  # LAND_SCRATCH: where the harness writes (its work dir and cargo target), when the stack's own disk is unwell.
  local sc=$LAND_SCRATCH; mkdir -p "$sc/qemu-tmp" "$sc/qemu-modules"
  # Every stanza runs each train. Most trains deal them over three guests side by side (the two
  # longest, orion-link with the poison stanza that starts from it, and gadget with route-a, each
  # get their own); every LAND_WHOLE_EVERY-th train
  # (default 10) runs them all in one boot, so the one-boot order is still checked. The modules are
  # kept by the hash of all they are built from (run.sh), so an unchanged kernel/ builds none.
  local how=(--split orion-link,ncm-after-controller gadget,route-a)
  [ $(( n % ${LAND_WHOLE_EVERY:-10} )) = 0 ] && how=() && echo "qemu: train $n runs every stanza in one boot" > "$L/qemu-mode.log"
  PATH="$d/lld:$PATH" KDIR=$d/build KERNEL=$d/build/arch/x86/boot/bzImage JOBS=$LAND_JOBS_QEMU CC="zig cc -target x86_64-linux-musl" TMPDIR="$sc/qemu-tmp" \
    LEMNOS_QEMU_MODULES=$sc/qemu-modules CARGO_TARGET_DIR=$sc/target-qemu CARGO_BUILD_JOBS=$LAND_JOBS_QEMU_CARGO taskset -c $LAND_CPUS_QEMU timeout -k 20 1000 testing/qemu/run.sh "${how[@]}" > "$L/qemu.log" 2>&1
  local r=$?; [ $r = 124 -o $r = 137 ] && echo "lander: qemu exceeded its bound of 1000 s (killed)" >> "$L/qemu.log"; return $r; }
off_part() { local d; d=$(kernel off kernel-modules-off.config --modules-off) || return 1
  export KERNEL=$d/build/arch/x86/boot/bzImage CARGO_TARGET_DIR=$LAND_SCRATCH/target-off CARGO_BUILD_JOBS=$LAND_JOBS_OFF CC="zig cc -target x86_64-linux-musl"
  # The built-in boots and the watchdog modes run side by side on the lane's cores (OFF-SIDE,
  # notes/OFF-SIDE.md: same margins as one after another). Each part logs to off-<part>.log; off.log gets
  # every part's log under its rc, then one verdict line naming each part that failed (the red summary
  # greps off.log and shows its tail). Each script has its own mktemp work dir; the watchdog's LOGS names
  # its files per mode, and the guests' cargo builds share the target under cargo's own lock.
  local wdt=(slot-init normal stall daemon daemon-late daemon-root daemon-hang daemon-stop daemon-loop) names=(builtin) pids=() i r bad=()
  names+=("${wdt[@]}")
  taskset -c $LAND_CPUS_OFF timeout -k 20 600 testing/qemu/run-builtin.sh > "$L/off-builtin.log" 2>&1 & pids+=($!)
  for i in "${wdt[@]}"; do LOGS="$L/qemu-watchdog-$i" MODES=$i taskset -c $LAND_CPUS_OFF timeout -k 20 600 testing/qemu/run-watchdog.sh > "$L/off-$i.log" 2>&1 & pids+=($!); done
  : > "$L/off.log"
  for i in "${!pids[@]}"; do wait "${pids[$i]}"; r=$?
    { echo "=== off ${names[$i]}: rc $r ($L/off-${names[$i]}.log)"; cat "$L/off-${names[$i]}.log"; } >> "$L/off.log"
    [ $r = 124 -o $r = 137 ] && echo "lander: off ${names[$i]} exceeded its bound of 600 s (killed)" >> "$L/off.log"
    [ $r = 0 ] || bad+=("${names[$i]}"); done
  [ ${#bad[@]} = 0 ] && { echo "off: passed (${names[*]} side by side)" >> "$L/off.log"; return 0; }
  for i in "${bad[@]}"; do echo "off: FAILED: $i: $(tail -1 "$L/off-$i.log" | cut -c1-160)"; done >> "$L/off.log"; return 1; }
( part qemu "$code_key" qemu_part ) &
( part off "$code_key" off_part ) &
# The consumer compile (scripts/land/README.md): compiles the consumers this change can break, against the stack's base
# and head. Its own lane, at the train's nice level (LAND_UNIT_NICE); ccomp.rc is 0 (passed, or not checked), 1 (new breaks or hidden members, not
# announced), or 2 (the step could not run). Its log is the train's consumer-compile.log, linked as ccomp.log.
ccomp_part() { ln -sf consumer-compile.log "$L/ccomp.log"; "$CCS" "$base" "$head" "$L" > "$L/ccomp.out" 2>&1; }
CCS=$(cd "$(dirname "$0")" && pwd)/consumer-compile.sh
# Keyed by the code key and the base: what it compares is base against head.
( part ccomp "$code_key-$base" ccomp_part ) &
wait
# Each train has its own TMPDIR (2026-11-18: leaked test dirs filled the shared one to 9.8 GB and an
# install test failed for space). Record what the tests left behind, then remove it.
echo "tmp left: $(find "$TMPDIR" -mindepth 1 -maxdepth 1 | wc -l) entries, $(du -sh "$TMPDIR" 2>/dev/null | cut -f1)"; find "$TMPDIR" -mindepth 1 -maxdepth 1 -printf '  left: %f %s\n' 2>/dev/null | head -10; rm -rf -- "$TMPDIR"
# NDF-CACHE record (a consumer maintainer's request, 2026-11-18): per train, the features summary, the step's time, each lane's end and
# the gate's wall time, from the gate's own lines. One TSV row per train; empty fields when a line is absent.
{ f() { grep -m1 -oE "$1" "$L/gate.log" 2>/dev/null | head -1; }
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$n" "$*" \
    "$(f 'features: [0-9]+ crates, [0-9]+ cached, [0-9]+ rebuilt \((hit|partial|full)\)')" \
    "$(grep -E '^ +features: [0-9]+ s' "$L/gate.log" 2>/dev/null | tail -1 | sed -E 's/^ +features: ([0-9]+) s.*/\1/')" \
    "$(f 'lane features ended at [0-9]+' | tr -dc 0-9)" "$(f 'lane tail ended at [0-9]+' | tr -dc 0-9)" \
    "$(f 'lane cargo ended at [0-9]+' | tr -dc 0-9)" "$(f 'gate: wall [0-9]+' | tr -dc 0-9)" \
    "$(grep -E '^ +features: [0-9]+ s, [0-9]+ CPU-s' "$L/gate.log" 2>/dev/null | tail -1 | sed -E 's/.* ([0-9]+) CPU-s.*/\1/')" \
    "$(awk '{ printf "%d", $2 + $3 + 0.5 }' "$L/gate.time" 2>/dev/null)"
} >> "$T/ndf-record.tsv"

red=0
for p in notes gate qemu off ccomp; do rc=$(cat "$L/$p.rc" 2>/dev/null || echo missing); echo "$p=$rc"; [ "$rc" = 0 ] || red=1; done
if [ $red = 1 ]; then
  for p in notes gate qemu off ccomp; do [ "$(cat "$L/$p.rc" 2>/dev/null)" = 0 ] && continue
    echo "--- $p"; grep -hE 'FAIL|FAILED|panicked|^error|failed' "$L/$p.log" "$L"/kernel-*.log 2>/dev/null | cut -c1-200 | head -25; tail -5 "$L/$p.log" 2>/dev/null | cut -c1-200; done
  echo "RED: nothing pushed. Stack: ${in[*]}"; exit 1
fi
if [ -n "${LTRAIN_AFTER:-}" ]; then while systemctl --user is-active --quiet "$LTRAIN_AFTER"; do sleep 20; done; fi
git fetch -q "$RMT" "+refs/heads/next:$RN/next"
[ "$(git rev-parse $RN/next)" = "$base" ] || { echo "next is $(git rev-parse --short $RN/next), not this stack's base $(git rev-parse --short "$base"): rerun on next"; exit 3; }
GIT_TERMINAL_PROMPT=0 git "${cred[@]}" push -q "$RMT" "$head:refs/heads/next" || { echo "push failed"; exit 4; }
echo "LANDED next=$head"
for id in "${in[@]}"; do
  # LAND_PR_REPO empty: no PR is closed (a rehearsal against a local remote).
  [ -n "$R" ] || { echo "landed $id (LAND_PR_REPO empty: no PR closed)"; continue; }
  num=$(gh pr list -R $R --head "pr/$id" --state open --json number -q '.[0].number')
  sha=$(git log --format=%h -1 --grep="^$id" "$base..$head" 2>/dev/null)
  [ -n "$num" ] && gh pr close "$num" -R $R -c "Landed on next at ${sha:-$head} (local gate, ltrain $n: gate, QEMU modules, QEMU modules-off all green)." >/dev/null 2>&1 && echo "closed $id #$num" || echo "landed $id (no open PR closed)"
done
echo "out: ${out[*]:-none}"
