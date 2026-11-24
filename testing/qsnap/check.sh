#!/usr/bin/env bash
# qsnap's own check: one x86_64 guest (testing/crates/qsnap-guest, as its first process) is booted
# once and kept, then restored for each case:
#   - the run's line reaches the restored guest;
#   - a payload disk given at the restore is read by the guest (not the one at the save);
#   - a save the guest asks for mid-run is kept, the run that asked goes on, and a restore of it
#     gets its own line and its own payload;
#   - the same spec again is the same key: the state is reused, not booted again;
#   - a disk the guest writes: each run gets its own overlay, a save keeps it, the base stays as it was.
#
#   check.sh                 KERNEL: an x86_64 bzImage with virtio-pci, virtio-console and virtio-blk
#                            (the QEMU kernel testing/qemu/build-kernel.sh makes has them)
# QSNAP_CACHE: where states are kept (default: a temporary directory, removed at the end).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
: "${KERNEL:?KERNEL must name an x86_64 bzImage}"
work="$(mktemp -d "${TMPDIR:-/tmp}/qsnap-check.XXXXXX")"
trap 'rm -rf -- "$work"' EXIT
cache="${QSNAP_CACHE:-$work/cache}"
accel=tcg
[[ -w /dev/kvm ]] && accel=kvm
fails=0
ok() { echo "ok   $*"; }
fail() { echo "FAIL $*"; fails=$((fails + 1)); }

target=x86_64-unknown-linux-musl
(cd "$repo" && cargo build -q -p lemnos-qsnap-guest --bin qsnap-guest-check --target "$target")
bin="${CARGO_TARGET_DIR:-$repo/target}/$target/debug/qsnap-guest-check"
mkdir -p "$work/root/dev" "$work/root/proc" "$work/root/sys" "$work/root/payload"
cp "$bin" "$work/root/init"
(cd "$work/root" && find . -print0 | cpio --quiet --null -o -H newc) >"$work/initramfs"

cat >"$work/spec.json" <<EOF
{"guests": [{"name": "g", "argv": ["qemu-system-x86_64", "-accel", "$accel", "-cpu", "max", "-smp", "1",
   "-m", "256", "-nographic", "-no-reboot", "-kernel", "$KERNEL", "-initrd", "$work/initramfs",
   "-append", "console=ttyS0 panic=-1 loglevel=1"],
   "inputs": ["$KERNEL", "$work/initramfs"]}],
 "ready": "guest", "payload_mb": 16, "timeout_s": 120}
EOF
payload() { # <dir> <hello text>: a newc archive holding hello
  mkdir -p "$1" && echo "$2" >"$1/hello" && (cd "$1" && find . -print0 | cpio --quiet --null -o -H newc) >"$1.cpio"
}
payload "$work/p1" "from-payload-one"
payload "$work/p2" "from-payload-two"
q() { python3 "$here/qsnap.py" "$@" --cache "$cache"; }
ms() { echo $(( ($(date +%s%N) - $1) / 1000000 )); }

t=$(date +%s%N); key=$(q save "$work/spec.json" 2>"$work/save.err"); echo "     save (boot to ready, keep): $(ms "$t") ms, key $key"
[[ -d "$cache/$key" ]] && ok "the state is kept under its key" || fail "no state under $cache/$key"

t=$(date +%s%N); out=$(q run "$work/spec.json" --send "hello there" 2>"$work/run1.err"); echo "     restore and run: $(ms "$t") ms ($(grep -o 'restored .* in [0-9.]* s' "$work/run1.err"))"
grep -q 'qsnap-guest: line "hello there"' <<<"$out" && ok "the run's line reaches the restored guest" || { fail "the line: $out"; }

out=$(q run "$work/spec.json" --send payload --payload "$work/p1.cpio" 2>/dev/null)
grep -q 'hello "from-payload-one"' <<<"$out" && ok "the restored guest reads the payload given at the restore" || fail "payload one: $out"
out=$(q run "$work/spec.json" --send payload --payload "$work/p2.cpio" 2>/dev/null)
grep -q 'hello "from-payload-two"' <<<"$out" && ok "another payload, the same state: the guest reads the new one" || fail "payload two: $out"

out=$(q run "$work/spec.json" --send "save mid" 2>"$work/save-mid.err")
dir=$(sed -n 's/^QSNAP SAVED //p' "$work/save-mid.err" | head -1)
grep -q "qsnap-guest: kept as $dir" <<<"$out" && [[ -n "$dir" && -d "$cache/$dir" ]] && ok "a save asked for mid-run is kept ($dir) and the run that asked goes on" || fail "mid-run save: $out $(cat "$work/save-mid.err")"
t=$(date +%s%N); out=$(q run "$work/spec.json" --from "$dir" --send payload --payload "$work/p2.cpio" 2>/dev/null); echo "     restore of the mid-run state and run: $(ms "$t") ms"
grep -q 'restored with "payload"' <<<"$out" && grep -q 'hello "from-payload-two"' <<<"$out" && ok "a restore of the mid-run state gets its own line and payload" || fail "mid-run restore: $out"

t=$(date +%s%N); again=$(q save "$work/spec.json" 2>/dev/null); echo "     save again: $(ms "$t") ms"
[[ "$again" == "$key" ]] && ok "the same spec is the same key, and its state is reused" || fail "key $again, was $key"

# A disk the guest writes: each run writes an overlay of its own, a save keeps the overlay as it was,
# and the base is never written.
truncate -s 1M "$work/base.img"
base_sum=$(sha256sum <"$work/base.img")
cat >"$work/disk.json" <<EOF
{"guests": [{"name": "g", "argv": ["qemu-system-x86_64", "-accel", "$accel", "-cpu", "max", "-smp", "1",
   "-m", "256", "-nographic", "-no-reboot", "-kernel", "$KERNEL", "-initrd", "$work/initramfs",
   "-append", "console=ttyS0 panic=-1 loglevel=1",
   "-drive", "file={disk:d0},if=none,id=d0,format=qcow2", "-device", "virtio-blk-pci,drive=d0"],
   "inputs": ["$KERNEL", "$work/initramfs", "$work/base.img"]}],
 "disks": {"g": {"d0": "$work/base.img"}},
 "ready": "guest", "payload_mb": 16, "timeout_s": 120}
EOF
out=$(q run "$work/disk.json" --send "write alpha read" 2>/dev/null)
grep -q 'disk holds "alpha"' <<<"$out" && ok "a run writes its disk and reads it back" || fail "write: $out"
out=$(q run "$work/disk.json" --send "read" 2>/dev/null)
grep -q 'disk holds ""' <<<"$out" && ok "the next run does not see the last run's writes" || fail "leak: $out"
out=$(q run "$work/disk.json" --send "write beta save d" 2>"$work/disk-save.err")
dir=$(sed -n 's/^QSNAP SAVED //p' "$work/disk-save.err" | head -1)
out=$(q run "$work/disk.json" --from "$dir" --send "read" 2>/dev/null)
grep -q 'disk holds "beta"' <<<"$out" && ok "a restore of a mid-run state finds the disk as it was at the save" || fail "kept disk: $out"
out=$(q run "$work/disk.json" --from "$dir" --send "read" 2>/dev/null)
grep -q 'disk holds "beta"' <<<"$out" && ok "and so does every restore of it" || fail "kept disk again: $out"
[[ "$(sha256sum <"$work/base.img")" == "$base_sum" ]] && ok "the base image is never written" || fail "the base image changed"

[[ $fails -eq 0 ]] && echo "qsnap: passed on $accel" || { echo "qsnap: FAILED ($fails failed)"; exit 1; }
