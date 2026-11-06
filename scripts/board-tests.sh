#!/usr/bin/env bash
# Runs the workspace's tests on a Linux board: cross-builds the test binaries, strips them, copies
# them over SSH and runs each one there, printing one result line per binary.
#
#   BOARD=root@host [SSH_OPTS="..."] [TARGET=aarch64-unknown-linux-musl] [STRIP=strip-tool] \
#   [LOCK=/path/to/lock] scripts/board-tests.sh
#
# Some tests read repository files (example looks, presets) or run `lemnos-ctl` through a path
# compiled in at build time. The board then needs those paths: when they are not writable there,
# a tmpfs is mounted on the first missing directory of the build path for the run and unmounted
# after it (the script reports both). trybuild's compile-fail test needs cargo, so it is skipped.
# LOCK, when set, is taken with `flock -w 900` on the board for the run.
set -euo pipefail

: "${BOARD:?set BOARD=user@host}"
SSH_OPTS=${SSH_OPTS:-}
TARGET=${TARGET:-aarch64-unknown-linux-musl}
STRIP=${STRIP:-llvm-strip}
root_dir=$(cd "$(dirname "$0")/.." && pwd)
build_dir=${BOARD_TESTS_TARGET_DIR:-$root_dir/target/board-tests}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
# shellcheck disable=SC2086
ssh_board() { ssh $SSH_OPTS "$BOARD" "$@"; }

echo "==> building the test binaries for $TARGET"
CARGO_TARGET_DIR=$build_dir cargo test --workspace --no-run --target "$TARGET" \
  --message-format=json 2>/dev/null |
  python3 -c '
import json, sys
for line in sys.stdin:
    try:
        m = json.loads(line)
    except ValueError:
        continue
    if m.get("reason") == "compiler-artifact" and m.get("executable") and m["profile"]["test"]:
        print(m["executable"])
' >"$work/list"
mkdir "$work/bin"
while read -r exe; do
  case "$exe" in */"$TARGET"/*) "$STRIP" -o "$work/bin/$(basename "$exe")" "$exe" ;; esac
done <"$work/list"
rm -f "$work"/bin/compile_fail-*
ctl="$build_dir/$TARGET/debug/lemnos-ctl"
"$STRIP" -o "$work/lemnos-ctl" "$ctl"
git -C "$root_dir" ls-files | grep -v '\.rs$' | tar -C "$root_dir" -T - -cf "$work/src.tar"
echo "==> $(ls "$work/bin" | wc -l) binaries, $(du -sh "$work/bin" | cut -f1) stripped"

remote=/tmp/lemnos-board-tests
ssh_board "rm -rf $remote && mkdir -p $remote"
tar -C "$work/bin" -cf - . | ssh_board "tar -C $remote -xf -"

# The first directory of the build paths that is missing on the board gets a tmpfs.
mount_point=$(ssh_board "p=/; for c in \$(echo '$root_dir' | tr / ' '); do p=\${p%/}/\$c; [ -d \$p ] || { echo \$p; exit; }; done")
mounted=
if [ -n "$mount_point" ]; then
  parent=$(dirname "$mount_point")
  if ! ssh_board "mkdir -p '$mount_point'" 2>/dev/null; then
    mount_point=$parent
    if ssh_board "grep -q ' $mount_point ' /proc/mounts || [ -n \"\$(ls -A '$mount_point')\" ]"; then
      echo "error: $mount_point is in use on the board; cannot mount the sources there" >&2
      exit 1
    fi
    ssh_board "mount -t tmpfs -o size=128m lemnos-board-tests '$mount_point'"
    mounted=$mount_point
    echo "==> mounted a tmpfs on $mount_point"
  fi
fi
cleanup_board() {
  ssh_board "rm -rf $remote" || true
  if [ -n "$mounted" ]; then
    ssh_board "umount '$mounted'" && echo "==> unmounted $mounted"
  fi
}
trap 'cleanup_board; rm -rf "$work"' EXIT
ssh_board "mkdir -p '$root_dir' '$(dirname "$ctl")'"
ssh_board "tar -C '$root_dir' -xf -" <"$work/src.tar"
ssh_board "cat > '$ctl' && chmod +x '$ctl'" <"$work/lemnos-ctl"

lock=${LOCK:+flock -w 900 $LOCK}
echo "==> running"
ssh_board "cd $remote && $lock sh -c 'fail=0; for t in *; do r=\$(./\$t --test-threads=4 2>&1); s=\$(echo \"\$r\" | grep \"^test result\" | tail -1); echo \"\$t: \${s:-no result}\"; case \"\$s\" in *\" 0 failed\"*) ;; *) fail=1; echo \"\$r\" | grep -E \"^test .* FAILED|panicked at\" | head -6 | sed \"s/^/    /\";; esac; done; exit \$fail'"
