#!/bin/bash
# Lander wrapper: runs l-train.sh as a systemd user unit. Machine paths, CPU sets and job counts come from
# the env file named by LEMNOS_LAND_ENV (template: scripts/land/env.example).
# Usage: land.sh <unit-suffix> <id>... ; runs l-train.sh as unit <LAND_UNIT_PREFIX><unit-suffix>.
HERE=$(cd "$(dirname "$0")" && pwd)
. "$HERE/env-load.sh" || exit 2
u=$1; shift
mkdir -p "$LAND_STATE_DIR"
nice_arg=(); [ -n "${LAND_UNIT_NICE:-}" ] && nice_arg=(-p "Nice=$LAND_UNIT_NICE")
systemd-run --user --unit="$LAND_UNIT_PREFIX$u" -p LimitNOFILE=1048576 -p "WorkingDirectory=$LAND_UNIT_DIR" "${nice_arg[@]}" \
  -E LEMNOS_LAND_ENV="$LEMNOS_LAND_ENV" -E HOME="$HOME" -E PATH="$PATH" \
  bash -c "$HERE/l-train.sh $* > $LAND_STATE_DIR/run-$u.log 2>&1"
