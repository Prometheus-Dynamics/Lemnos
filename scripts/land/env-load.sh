# Sourced by land.sh, l-train.sh and consumer-compile.sh. Reads the machine's env file (named by
# LEMNOS_LAND_ENV, template scripts/land/env.example) and checks that every variable is set.
# Returns 2 when the lander cannot run; the caller exits with that status.

if [ -z "${LEMNOS_LAND_ENV:-}" ]; then
  echo "LEMNOS_LAND_ENV is not set: point it at your land.env (the template is scripts/land/env.example)" >&2
  return 2
fi
if [ ! -r "$LEMNOS_LAND_ENV" ]; then
  echo "LEMNOS_LAND_ENV=$LEMNOS_LAND_ENV: no such file (scripts/land/setup writes it; or copy scripts/land/env.example there and fill it in)" >&2
  return 2
fi
. "$LEMNOS_LAND_ENV" || return 2

__land_missing=""
for __land_v in LAND_REPO LAND_WORKTREE LAND_STATE_DIR LAND_UNIT_DIR LAND_LOCK_FILE LAND_SCRATCH LAND_TMP \
  LAND_GUARD_BIN LAND_QK_DIR LAND_KERNEL_TARBALLS LAND_BUSYBOX_DIR LAND_CARGO_HOME LAND_CC_WORK LAND_CC_CACHE LAND_CC_DATA \
  LAND_REMOTE LAND_UNIT_PREFIX LAND_CPUS_GATE LAND_CPUS_CC LAND_CPUS_QEMU LAND_CPUS_OFF \
  LAND_JOBS_GATE LAND_JOBS_CC LAND_JOBS_KERNEL LAND_JOBS_QEMU LAND_JOBS_QEMU_CARGO LAND_JOBS_OFF; do
  [ -n "${!__land_v:-}" ] || __land_missing="$__land_missing $__land_v"
done
# LAND_PR_REPO may be empty (no PR is closed); it must still be set.
[ -n "${LAND_PR_REPO+x}" ] || __land_missing="$__land_missing LAND_PR_REPO"
if [ -n "$__land_missing" ]; then
  echo "$LEMNOS_LAND_ENV: not set:$__land_missing" >&2
  return 2
fi
unset __land_v __land_missing
