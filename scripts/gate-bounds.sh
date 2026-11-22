#!/usr/bin/env bash
# Process control for scripts/gate.sh: runs each gate step in its own process group under a CPU budget and a
# progress watch. Sourced by gate.sh and by testing/gate-bounds/check.sh; it defines functions only.
#
# No step has a wall-clock bound. A step is killed in two cases, and both are named:
#   budget   the CPU its process tree has used (user plus system, in CPU-seconds) passes the step's budget.
#            The tree is every process in the step's process group and every process that carries the
#            step's token in its environment, so a child that left the group is still counted.
#   stalled  the tree made no progress for GATE_STALL_SAMPLES samples in a row. A sample is one look at the
#            tree, taken about once a second. The tree made progress in a sample when it used CPU since the
#            last sample, or when one of its processes was runnable (state R: waiting for a CPU). A step that
#            is slow under load is runnable or using CPU, so it never stalls; a step blocked on something
#            that never comes does neither, and is stopped.
# The budget is what the step costs in CPU, which load changes little. The stall count is in samples, not in
# seconds of a deadline: nothing is cut for taking long while it progresses.
# The gate's own state (gate_out, the step directories, the ccache check) lives in gate.sh and is used here.

GATE_STALL_SAMPLES="${GATE_STALL_SAMPLES:-180}"
# A gap of more than this many seconds between two looks at a step means the step was frozen or stopped by the
# machine (a lander train freezes the workers' units), not that it was slow. Such a step is starved.
GATE_FREEZE_GAP="${GATE_FREEZE_GAP:-10}"
# The exit status of a gate whose failing step was starved (a label, not a code red). Distinct from 1.
GATE_STARVED_EXIT="${GATE_STARVED_EXIT:-3}"
GATE_CLK_TCK="$(getconf CLK_TCK)"

# gate_sweep <token> <signal>: sends the signal to each process whose environment carries the token. A step's
# processes all inherit its token, so this also finds those that left its process group (a new session, a
# daemon the step's child reparented). Only /proc is read.
gate_sweep() {
  local tok="$1" sig="$2" hits f x e
  # One grep finds the candidates; each is then matched on its whole entry, so a longer token never matches.
  hits="$(grep -alF -- "GATE_STEP_TOKEN=$tok" /proc/[0-9]*/environ 2>/dev/null || true)"
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    mapfile -d '' -t e 2>/dev/null <"$f" || continue
    for x in "${e[@]}"; do
      if [ "$x" = "GATE_STEP_TOKEN=$tok" ]; then
        f="${f#/proc/}"
        kill -"$sig" "${f%%/*}" 2>/dev/null || true
        break
      fi
    done
  done <<<"$hits"
}

# is_group ID: true only for a plain number above 1 that is not this shell's own process group: the only ids a
# group kill here may take (never -1, 0, or this shell's group, whatever a pid file held). Checked by
# scripts/check-signals.py, which refuses a group kill whose line does not call it.
is_group() {
  case "$1" in ''|*[!0-9]*) return 1 ;; esac
  [ "$1" -gt 1 ] || return 1
  [ "$1" != "$(sed 's/^.*) //' /proc/$$/stat | cut -d' ' -f3)" ]
}

# stop_step <pgid> <token>: ends a step's processes. TERM to the group and to the token's processes, then,
# after the grace, KILL to both. The step's output is a file, so nothing left running can hold the gate open.
stop_step() {
  local pgid="$1" token="$2"
  is_group "$pgid" && kill -TERM -- "-$pgid" 2>/dev/null || true
  gate_sweep "$token" TERM
  sleep "$GATE_KILL_GRACE"
  is_group "$pgid" && kill -KILL -- "-$pgid" 2>/dev/null || true
  gate_sweep "$token" KILL
}

# tree_sample <pgid> <token>: one look at a step's process tree. Sets tree_ticks (the tree's CPU so far, in clock
# ticks) and sample_progress (1 when the tree used CPU since the last look, or when one of its processes is runnable,
# state R). The tree is every process in the step's process group and every process that carries the token. A
# process's CPU is its own time plus the time of the children it has waited for (cutime, cstime), so a short child
# that exited between two looks is still counted, once: the child's time moves into its parent's cutime when the
# parent reaps it. The sum is taken fresh at each look, never as deltas, so a reaped child is not counted twice.
# Only /proc is read: one grep for the token's processes, one cat and one awk over every process's stat.
tree_sample() {
  local pgid="$1" tok="$2" pids before line
  pids="$(grep -alP -- "GATE_STEP_TOKEN=\Q$tok\E\x00" /proc/[0-9]*/environ 2>/dev/null | sed -n 's#^/proc/\([0-9]*\)/environ$#\1#p' | tr '\n' ' ' || true)"
  before="$tree_ticks"
  sample_running=0
  read -r tree_ticks sample_running < <(cat /proc/[0-9]*/stat 2>/dev/null | awk -v g="$pgid" -v want=" $pids " '
    { pid = $1; line = $0; sub(/^.*\) /, "", line); n = split(line, a, " ")
      if (a[3] == g || index(want, " " pid " ") > 0) {
        total += a[12] + a[13] + a[14] + a[15]
        if (a[1] == "R") running = 1
      } }
    END { printf "%d %d\n", total + 0, running + 0 }')
  sample_progress=$sample_running
  sample_starved=0
  if [ "$tree_ticks" -gt "$before" ]; then sample_progress=1
  elif [ "$sample_running" = 1 ]; then sample_starved=1
  fi
}

# step_tmp_clear <label> <dir>: a step that was killed can leave what it had open in its TMPDIR (rustdoc's
# rustdoctest<N> directory, while its test binary runs). The kill is the step's failure and is already named, so
# these entries are listed as the kill's leftovers and removed; they are not a second red.
step_tmp_clear() {
  local label="$1" dir="$2" e n=0
  for e in "$dir"/* "$dir"/.[!.]*; do [ -e "$e" ] || [ -L "$e" ] || continue; n=$((n + 1)); echo "    killed step left ${e##*/}"; done
  rm -rf -- "$dir"
  [ "$n" -gt 0 ] && echo "gate: $label was killed; removed the $n entries it left in its TMPDIR (the kill is the failure)"
  return 0
}

# bounded <key> <budget> <command...>: runs the command as step <key>, in its own process group, with stdin from
# /dev/null and its output in $gate_out/<key>.out. The output is a file, never a pipe. Sets step_rc (the status,
# or 124 when the step was killed), step_killed (0 not killed, 1 over its CPU budget, 2 stalled) and step_live
# (the CPU-seconds its tree had used when it was killed). <budget> is in CPU-seconds.
# Keys are unique within a run, so the .done file is not there before the step writes it.
bounded() {
  local key="$1" budget="$2" token pid steptmp before loops=0 stalled=0 budget_ticks
  shift 2
  token="gate.$$.$key.$RANDOM$RANDOM"
  step_rc=0 step_killed=0 step_live=0
  steptmp="$(step_dir "g$key")"
  rm -rf -- "$steptmp"; mkdir -p -- "$steptmp"
  before="$(ccache_names || true)"
  set -m
  (
    export GATE_STEP_TOKEN="$token" TMPDIR="$steptmp"
    rc=0
    "$@" || rc=$?
    # The step's CPU: bash's times, whose second line is the children this subshell waited for (user, then
    # system). Straight to a file: in a pipeline, times would run in a new subshell that has no children.
    times >"$gate_out/$key.cpu"
    echo "$rc" >"$gate_out/$key.done"
  ) </dev/null >"$gate_out/$key.out" 2>&1 &
  pid=$!
  set +m
  echo "$pid $token" >"$gate_out/$key.pid"
  budget_ticks=$((budget * GATE_CLK_TCK))
  tree_ticks=0
  starved_gap=0; last_look=""; samples=0; starved_samples=0; max_gap=0
  while [ ! -e "$gate_out/$key.done" ]; do
    if [ $((loops % 5)) = 0 ]; then
      now_look="${EPOCHREALTIME%%.*}"
      if [ -n "$last_look" ]; then
        gap=$((now_look - last_look)); [ "$gap" -gt "$max_gap" ] && max_gap=$gap
        [ "$gap" -gt "$GATE_FREEZE_GAP" ] && starved_gap=1
      fi
      last_look="$now_look"
      tree_sample "$pid" "$token"
      samples=$((samples + 1))
      if [ "$sample_starved" = 1 ]; then starved_samples=$((starved_samples + 1)); fi
      step_live=$((tree_ticks / GATE_CLK_TCK))
      if [ "$tree_ticks" -gt "$budget_ticks" ]; then
        step_killed=1
        break
      fi
      if [ "$sample_progress" = 1 ]; then stalled=0; else stalled=$((stalled + 1)); fi
      if [ "$stalled" -ge "$GATE_STALL_SAMPLES" ]; then
        step_killed=2
        break
      fi
    fi
    loops=$((loops + 1))
    sleep 0.2
  done
  if [ "$step_killed" != 0 ]; then
    stop_step "$pid" "$token"
    step_rc=124
  else
    step_rc="$(cat "$gate_out/$key.done")"
  fi
  wait "$pid" 2>/dev/null || true
  # Starved: a freeze or stop of the machine showed as a gap between looks, or at least half the looks found a
  # runnable process that got no CPU (it waited for a CPU under load). Starved is a label, not a pass: the step's
  # result stands, and the gate reports it as STARVED[<step>] with its evidence.
  starved=$starved_gap
  if [ "$samples" -ge 5 ] && [ $((starved_samples * 2)) -ge "$samples" ]; then starved=1; fi
  starved_pct=0; [ "$samples" -gt 0 ] && starved_pct=$((starved_samples * 100 / samples))
  echo "longest gap ${max_gap} s between looks; runnable-without-CPU in ${starved_pct}% of ${samples} looks; load $(cut -d' ' -f1-3 /proc/loadavg)" >"$gate_out/$key.evidence"
  echo "$starved" >"$gate_out/$key.starved"
  : >"$gate_out/$key.pid"
  if [ "$step_killed" != 0 ]; then
    step_tmp_clear "$(cat "$gate_out/$key.label")" "$steptmp" >>"$gate_out/$key.out" 2>&1
  elif ! step_tmp_check "$(cat "$gate_out/$key.label")" "$steptmp" >>"$gate_out/$key.out" 2>&1; then
    [ "$step_rc" != 0 ] || step_rc=1
  fi
  ccache_growth "$(cat "$gate_out/$key.label")" "$before" >>"$gate_out/$key.out"
  return 0
}

# unit <key> <label> <budget> <command...>: runs one step bounded, and records it as $gate_out/<key>.*: its output,
# seconds, CPU budget, how it was killed (0, 1 or 2), its CPU-seconds when killed, and its status last (a step is
# finished when .rc exists).
unit() {
  local key="$1" label="$2" budget="$3" start=$SECONDS
  shift 3
  echo "$label" >"$gate_out/$key.label"
  echo "$budget" >"$gate_out/$key.bound"
  unit_out="$gate_out/$key.out"
  bounded "$key" "$budget" "$@"
  echo "$((SECONDS - start))" >"$gate_out/$key.secs"
  echo "$step_killed" >"$gate_out/$key.killed"
  echo "$step_live" >"$gate_out/$key.live"
  echo "$step_rc" >"$gate_out/$key.rc"
  return "$step_rc"
}

# finish <key>: prints a finished step as the gate shows it. A failing step names itself and stops the gate.
finish() {
  local key="$1" label rc killed
  label="$(cat "$gate_out/$key.label")"
  rc="$(cat "$gate_out/$key.rc")"
  killed="$(cat "$gate_out/$key.killed")"
  echo "==> $label"
  cat "$gate_out/$key.out"
  if [ "$rc" != 0 ]; then
    case "$killed" in
      1) echo "gate: $label exceeded its CPU budget of $(cat "$gate_out/$key.bound") CPU-s (killed at $(cat "$gate_out/$key.live") CPU-s)" >&2 ;;
      2) echo "gate: $label stalled: no CPU progress and nothing runnable for $GATE_STALL_SAMPLES samples (killed at $(cat "$gate_out/$key.live") CPU-s)" >&2 ;;
    esac
    if [ "$(cat "$gate_out/$key.starved")" = 1 ]; then
      echo "STARVED[$label]: $label failed while starved; $(cat "$gate_out/$key.evidence")" >&2
      echo "gate: $label failed (exit $rc, starved: not a pass; not rerun)" >&2
      stop_lanes
      exit "$GATE_STARVED_EXIT"
    fi
    echo "gate: $label failed (exit $rc)" >&2
    stop_lanes
    exit 1
  fi
  echo "    $label: $(cat "$gate_out/$key.secs") s, $(step_cpu "$key") CPU-s"
}

# step_cpu <key>: the step's user plus system CPU in whole seconds, from bash's times ("0m1.234s 0m0.567s").
step_cpu() {
  local line m s total=0 t
  line="$(sed -n 2p "$gate_out/$1.cpu" 2>/dev/null)"
  [ -n "$line" ] || { echo "?"; return; }
  for t in $line; do
    m="${t%%m*}"; s="${t#*m}"; s="${s%s}"
    total=$(awk -v t="$total" -v m="$m" -v s="$s" 'BEGIN { printf "%.3f", t + m * 60 + s }')
  done
  awk -v t="$total" 'BEGIN { printf "%d", t + 0.5 }'
}

# step <label> <budget> <command...>: a step run before the lanes, bounded and shown like a lane step.
step() {
  unit top "$1" "$2" "${@:3}" || true
  finish top
}
