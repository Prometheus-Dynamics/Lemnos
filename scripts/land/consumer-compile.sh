#!/bin/bash
# consumer-compile.sh <base> <cand> <logdir>
# Compiles each consumer that locks a Lemnos crate this change can break, against <base> and against <cand>,
# and fails only on errors the candidate adds. A commit in base..cand with the trailer
# "Consumer-break-announced: <text>" lets the new errors pass (they are still logged).
# Writes <logdir>/consumer-compile.log (trigger, per-consumer counts, the new-error list, verdict).
# Exit: 0 passed or skipped; 1 new errors with no announcement; 2 the step could not run.
# Reads only: consumer repos (git archive / show), the Lemnos repo (git archive, rev-list). Writes under
# $WORK and $CACHE (both from the env file). Lander tooling; see scripts/land/README.md.
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
. "$HERE/env-load.sh" || exit 2
REPO=$LAND_REPO
CC=$LAND_CC_DATA
CACHE=$LAND_CC_CACHE
WORK=$LAND_CC_WORK
export PATH="$HOME/.cargo/bin:$PATH"
[ $# = 3 ] || { echo "usage: consumer-compile.sh <base> <cand> <logdir>" >&2; exit 2; }
base_in=$1; cand_in=$2; L=$3
mkdir -p "$L" "$CACHE/locks" "$CACHE/baseline" "$WORK"/{pristine,trees,stubs,work,target,tmp,out}
LOG=$L/consumer-compile.log; : > "$LOG"
say() { echo "$*" | tee -a "$LOG"; }
PY=(python3 -I "$HERE/cc.py")
# What a compile's result depends on besides the two commits and the consumer's pinned rev (which carries its
# lock): these scripts, the consumer data (shims, fields, scratch fixes) and the toolchain. A cached result is
# keyed on all of it.
TOOLH=$( { cat "$HERE/cc.py" "$CC/shims.tsv" "$CC/consumers.tsv" "$CC/fields.tsv" "$(readlink -f "$0")";
  cat "$CC"/fixes/*.json 2>/dev/null; "$HOME/.cargo/bin/rustc" -vV; "$HOME/.cargo/bin/cargo" -V; } | sha256sum | cut -c1-10)
t_start=$(date +%s.%N)
# One compile at a time: the targets and archives are shared by every train.
exec 9>"$WORK/lock"; flock 9

base=$(git -C "$REPO" rev-parse --verify -q "$base_in^{commit}") || { say "consumer-compile: bad base $base_in"; exit 2; }
cand=$(git -C "$REPO" rev-parse --verify -q "$cand_in^{commit}") || { say "consumer-compile: bad candidate $cand_in"; exit 2; }
say "consumer-compile: base $(git -C "$REPO" rev-parse --short "$base") cand $(git -C "$REPO" rev-parse --short "$cand") tool $TOOLH"

# The consumers, with their pinned revs and the Lemnos packages each locks (cached per pinned rev).
declare -A REPO_OF FULL_OF R8_OF MDIR_OF LOCKF_OF NF_OF
order=()
excluded=()
while IFS=$'\t' read -r -u 3 name repo rev mdir lockf status; do
  case "$name" in ''|\#*) continue;; esac
  full=$(git -C "$repo" rev-parse --verify -q "$rev^{commit}") || { say "consumer-compile: $name: pinned rev $rev not found"; exit 2; }
  r8=${full:0:8}; nf=$CACHE/locks/$name-$r8.names
  if [ ! -s "$nf" ]; then
    git -C "$repo" show "$full:$lockf" > "$CACHE/locks/$name-$r8.lock" \
      && "${PY[@]}" locknames "$CACHE/locks/$name-$r8.lock" > "$nf" || { say "consumer-compile: $name: lock unreadable"; exit 2; }
  fi
  if [ -n "${status:-}" ]; then excluded+=("$name"); continue; fi
  order+=("$name"); REPO_OF[$name]=$repo; FULL_OF[$name]=$full; R8_OF[$name]=$r8; MDIR_OF[$name]=$mdir
  LOCKF_OF[$name]=$lockf; NF_OF[$name]=$nf
done 3< "$CC/consumers.tsv"

# Commits whose trailer announces a break.
announced=()
for c in $(git -C "$REPO" rev-list --reverse "$base..$cand"); do
  t=$(git -C "$REPO" show -s --format=%B "$c" | git interpret-trailers --parse | sed -n 's/^Consumer-break-announced: *//p')
  [ -n "$t" ] && announced+=("$(git -C "$REPO" rev-parse --short "$c"): $t")
done

tr=$("${PY[@]}" trigger "$REPO" "$base" "$cand" $(for n in "${order[@]}"; do echo "${NF_OF[$n]}"; done))
t_trig=$(date +%s.%N)
grep -E '^(TRIGGER|CHECKED)' <<<"$tr" >> "$LOG"
if ! grep -q '^RESULT yes' <<<"$tr"; then
  say "consumer-compile: skipped (no public change in consumer crates)"
  say "consumer-compile: trigger check $(echo "$t_trig - $t_start" | bc | cut -c1-5) s"
  exit 0
fi
affected=()
for n in "${excluded[@]}"; do
  if grep -q "^AFFECT $n " <<<"$tr"; then
    say "$n: not checked (pre-port pin $(awk -F'\t' -v n="$n" '$1==n{print $3}' "$CC/consumers.tsv"); the step checks it once its pin moves to a next-based rev)"
  fi
done
for n in "${order[@]}"; do grep -q "^AFFECT $n " <<<"$tr" && affected+=("$n"); done
if [ ${#affected[@]} -eq 0 ]; then say "consumer-compile: skipped (the affected consumers are not checked, see above)"; exit 0; fi
# CC_FORCE=Name,Name: compile these consumers whatever the trigger says (inspection and measurement runs only).
if [ -n "${CC_FORCE:-}" ]; then affected=(); for n in "${order[@]}"; do case ",$CC_FORCE," in *",$n,"*) affected+=("$n");; esac; done; say "consumer-compile: CC_FORCE: ${affected[*]}"; fi
say "consumer-compile: affected consumers: ${affected[*]}"

# The Lemnos tree at a sha, always at one path ($WORK/trees/live): archived fresh, its shims applied, then
# synced into the live tree so only the files that differ are rewritten. The path and the unchanged files'
# mtimes stay, so cargo rebuilds only the crates that changed since the last compile (base to candidate, and
# one train's candidate to the next train's).
tree_for() { # <sha>
  local fresh=$WORK/trees/fresh live=$WORK/trees/live
  rm -rf "$fresh"; mkdir -p "$fresh"
  git -C "$REPO" archive "$1" | tar -x -C "$fresh" || return 1
  "${PY[@]}" shim "$fresh" "$CC/shims.tsv" >> "$LOG" || return 1
  "${PY[@]}" sync "$fresh" "$live" || return 1
  rm -rf "$fresh"
  echo "$live"
}

# Compiles one consumer on one side (base or cand); each side's error keys are cached per commit.
compile_side() { # <consumer> <side:base|cand> <sha>
  local name=$1 side=$2 sha=$3 full=${FULL_OF[$1]} r8=${R8_OF[$1]} repo=${REPO_OF[$1]}
  local mdir=${MDIR_OF[$1]} nf=${NF_OF[$1]}
  # A side's result is kept under its own sha: in a serial lander the candidate that lands is the next
  # train's base, so the base is found here and not compiled again.
  # A rerun of the same candidate finds it the same way.
  local skeys=$CACHE/baseline/$name-$r8-${sha:0:8}-$TOOLH.keys
  local keys=$L/cc-$name-$side.keys
  if [ -f "$skeys" ]; then
    say "consumer $name $side: result cached ($(wc -l < "$skeys") errors at ${sha:0:8})"
    cp "$skeys" "$keys"; cp "$skeys.cov" "$L/cc-$name-$side.cov" 2>/dev/null || { say "consumer-compile: $name: cached result has no coverage: rerun it"; return 2; }; return 0
  fi
  local pristine=$WORK/pristine/$name-$r8
  if [ ! -f "$pristine/.ok" ]; then
    rm -rf "$pristine"; mkdir -p "$pristine"
    git -C "$repo" archive "$full" | tar -x -C "$pristine" && touch "$pristine/.ok" \
      || { say "consumer-compile: $name: archive of $full failed"; return 2; }
  fi
  local tree; tree=$(tree_for "$sha") || { say "consumer-compile: $name: Lemnos tree ${sha:0:8} failed"; return 2; }
  # One work dir per consumer, synced from the pristine copy for each side (its mtimes stay warm).
  local work=$WORK/work/$name-$r8
  "${PY[@]}" prepare "$pristine" "$work" "$mdir" "$tree" "$nf" "$WORK/stubs" >> "$LOG" \
    || { say "consumer-compile: $name $side: prepare failed"; return 2; }
  # Resolution loop: a Lemnos package a consumer asks a feature of that the tree lacks gets the feature (scratch).
  local i resolved=0
  for i in 1 2 3 4 5 6 7 8; do
    if ( cd "$work/$mdir" && "$HOME/.cargo/bin/cargo" metadata --format-version 1 > /dev/null 2> "$WORK/out/$name-$side.resolve.err" ); then resolved=1; break; fi
    grep -q 'does not have that feature' "$WORK/out/$name-$side.resolve.err" || { say "consumer-compile: $name $side: resolution failed (not a missing feature):"; tail -8 "$WORK/out/$name-$side.resolve.err" | cut -c1-200 >> "$LOG"; return 2; }
    "${PY[@]}" addfeat "$tree" "$WORK/out/$name-$side.resolve.err" "$WORK/stubs" >> "$LOG" || { say "consumer-compile: $name $side: missing feature not on a tree package: did not run"; return 2; }
  done
  [ $resolved = 1 ] || { say "consumer-compile: $name $side: resolution still failing after 8 feature passes"; return 2; }
  # Scratch edits for the pinned rev: keyed to the rev, so a moved pin has no file and the step stops.
  local fixf=$CC/fixes/$name-$r8.json
  [ -f "$fixf" ] || { say "consumer-compile: $name $r8: no scratch-fix file ($fixf): the fixes need redoing for this pin; the step did not run"; return 2; }
  "${PY[@]}" fix "$work" "$fixf" >> "$LOG" || { say "consumer-compile: $name $r8: scratch fixes no longer match this rev: they need redoing; the step did not run"; return 2; }
  local json=$WORK/out/$name-$side.json err=$WORK/out/$name-$side.err tim=$WORK/out/$name-$side.time
  # Compile; insert the scratch defaults for E0063 "missing field" errors that have one (fields.tsv); recompile.
  # Four passes at most; if inserts are still needed after the last, the keys would be stale, so the step stops.
  local pass rc wall user sys cpu cpu_total=0 nedit=0 converged=0
  for pass in 1 2 3 4; do
    "$HERE/target-sv.sh" "$WORK/target/$name" >> "$LOG" || { say "consumer-compile: $name: its target dir could not be made a subvolume"; return 2; }
    ( cd "$work/$mdir" && env CARGO_TARGET_DIR="$WORK/target/$name" CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=$LAND_JOBS_CC TMPDIR="$WORK/tmp" \
        taskset -c $LAND_CPUS_CC timeout -k 30 3600 /usr/bin/time -o "$tim" -f "%e %U %S" \
        cargo check --workspace --all-targets --all-features --keep-going --message-format=json > "$json" 2> "$err" )
    rc=$?
    read -r wall user sys < <(tail -1 "$tim" 2>/dev/null); : "${wall:=?}" "${user:=0}" "${sys:=0}"
    cpu=$(echo "$user + $sys" | bc); cpu_total=$(echo "$cpu_total + $cpu" | bc)
    say "consumer $name $side (${sha:0:8}) pass $pass: cargo rc $rc, wall ${wall} s, CPU ${cpu} s (user $user, sys $sys)"
    nedit=$("${PY[@]}" autofields "$json" "$CC/fields.tsv" "$WORK/out/$name-$side.auto.json") || nedit=0
    if [ "$nedit" = 0 ]; then converged=1; break; fi
    say "consumer $name $side pass $pass: $nedit E0063 scratch default(s) inserted, compiling again"
    "${PY[@]}" fix "$work" "$WORK/out/$name-$side.auto.json" >> "$LOG" || { say "consumer-compile: $name $side: inserting defaults failed"; return 2; }
  done
  [ $converged = 1 ] || { say "consumer-compile: $name $side: scratch defaults did not converge in 4 passes: the step did not run"; return 2; }
  say "consumer $name $side (${sha:0:8}): total CPU ${cpu_total} s over $pass pass(es)"
  if grep -q 'Prometheus-Dynamics/Lemnos' "$work/Cargo.lock" && grep -qE '^source = "git\+https://github.com/Prometheus-Dynamics/Lemnos' "$work/Cargo.lock"; then
    say "consumer-compile: $name $side: a Lemnos package still resolves to git (not the tree): the step did not run"; return 2
  fi
  "${PY[@]}" keys "$json" > "$keys.tmp"
  local n; n=$(wc -l < "$keys.tmp")
  if [ "$rc" != 0 ] && [ "$n" = 0 ]; then
    say "consumer-compile: $name $side: cargo failed (rc $rc) with no compiler error: the step did not run"
    tail -15 "$err" | cut -c1-200 >> "$LOG"; return 2
  fi
  mv "$keys.tmp" "$keys"
  ( cd "$work/$mdir" && "$HOME/.cargo/bin/cargo" metadata --no-deps --format-version 1 > "$WORK/out/$name-$side.meta" 2>/dev/null )
  local cov; cov=$("${PY[@]}" coverage "$json" "$WORK/out/$name-$side.meta")
  sed "s/^/consumer $name $side: /" <<<"$cov" >> "$LOG"
  # .cov: line 1 "reached total"; then "masked: <names>" when members are not reached (logged, never a failure by itself)
  awk '/^REACHED/{print $2" "$3} /^NOT-REACHED/{sub(/^NOT-REACHED /,""); print "masked: " $0}' <<<"$cov" > "$L/cc-$name-$side.cov"
  say "consumer $name $side: $n error(s)"
  cp "$keys" "$skeys.tmp" && cp "$L/cc-$name-$side.cov" "$skeys.cov" && mv "$skeys.tmp" "$skeys"
  return 0
}

verdict=0; newtotal=0; covfail=0; covhide=0
for n in "${affected[@]}"; do
  compile_side "$n" base "$base" || { verdict=2; break; }
  compile_side "$n" cand "$cand" || { verdict=2; break; }
  "${PY[@]}" newerrs "$L/cc-$n-base.keys" "$L/cc-$n-cand.keys" > "$L/cc-$n.diff"
  nnew=$(grep -c '^NEW' "$L/cc-$n.diff"); nfix=$(grep -c '^FIXED' "$L/cc-$n.diff")
  say "consumer $n: $nnew new error(s), $nfix fixed by the candidate (keys: $L/cc-$n.diff)"
  grep '^NEW' "$L/cc-$n.diff" | cut -f2- | sed 's/^/  NEW  /' | tee -a "$LOG" | head -60
  newtotal=$((newtotal + nnew))
  read -r br bt < "$L/cc-$n-base.cov"; read -r cr ct < "$L/cc-$n-cand.cov"
  if [ "$br" != "$bt" ]; then say "!! BASE SHORTFALL (logged, not a failure): consumer $n base reaches $br of $bt members; masked: $(sed -n 's/^masked: //p' "$L/cc-$n-base.cov") !!"; fi
  if [ "$cr" -lt "$br" ]; then say "consumer $n: candidate reaches $cr members, base $br: the candidate hides $((br - cr)) member(s) from the compile"; covhide=1; fi
done

t_end=$(date +%s.%N)
say "consumer-compile: wall $(echo "$t_end - $t_start" | bc | cut -c1-7) s; CPU see the cargo lines above"
if [ $verdict = 2 ]; then say "consumer-compile: DID NOT RUN (see above)"; exit 2; fi
if [ $covfail = 1 ]; then say "consumer-compile: FAIL: base coverage incomplete (see above)"; exit 1; fi
if [ $covhide = 1 ] && [ ${#announced[@]} -eq 0 ]; then say "consumer-compile: FAIL: the candidate hides members from the compile and no Consumer-break-announced trailer covers it"; exit 1; fi
if [ "$newtotal" = 0 ] && [ $covhide = 0 ]; then say "consumer-compile: PASS (no new errors in ${affected[*]})"; exit 0; fi
if [ ${#announced[@]} -gt 0 ]; then
  say "consumer-compile: $newtotal new error(s) and $covhide hidden-member shortfall(s), announced by trailer, passing:"
  printf '  %s\n' "${announced[@]}" | tee -a "$LOG"; exit 0
fi
say "consumer-compile: FAIL: $newtotal new error(s) and no Consumer-break-announced trailer in base..cand"
exit 1
