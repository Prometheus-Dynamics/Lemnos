#!/usr/bin/env bash
# Rehearses the card image's PMP read on qemu-system-riscv64 (virt machine, rv64, machine mode from reset).
#
#   run.sh pmp-true     pmp=true:  the read succeeds, and the line is `pmp: cfg0=0x.. addr0=0x..`, then identify
#   run.sh pmp-false    pmp=false: the read traps into the probe's vector, and the line is
#                       `pmp: trapped mcause=0x00000002 (no PMP)`, then identify
#   run.sh fail-naive   pmp=false, the reads with no vector of ours (feature pmp-naive): must NOT reach identify
#   run.sh fail-broken  pmp=false, the vector without the mepc step (feature pmp-broken): must NOT reach identify
#   run.sh all          the two green cases, then the two failing ones (each must fail to reach identify)
#
# A case passes when its expectation holds. The two fail-* cases pass when the rehearsal does NOT print identify
# (the trap loops, or the run times out): that shows the test can catch a bad vector.
set -u
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export PATH="$HOME/.cargo/bin:$PATH"
target="${REHEARSAL_TARGET:-/dev/shm/lemnos-FLASH-EXTENT/reh}"
seconds=15

build() { # $1 = feature list (may be empty)
    (cd "$here" && CARGO_TARGET_DIR="$target" cargo build --release --quiet ${1:+--features "$1"}) || return 1
    echo "$target/riscv64imac-unknown-none-elf/release/k230-pmp-rehearsal"
}

run_case() { # $1 name, $2 pmp, $3 features, $4 expect: pass|nopass
    local name=$1 pmp=$2 features=$3 expect=$4 elf out status
    elf=$(build "$features") || { echo "$name: BUILD FAILED"; return 1; }
    out="$target/$name.out"
    timeout "$seconds" qemu-system-riscv64 -machine virt -cpu "rv64,pmp=$pmp" -m 128M -nographic \
        -bios none -kernel "$elf" -no-reboot > "$out" 2>&1
    status=$?
    echo "== $name (pmp=$pmp${features:+, features $features}, qemu exit $status)"
    cat "$out"
    if [ "$expect" = pass ]; then
        grep -q "^identify: K230" "$out" || { echo "$name: FAIL (identify not reached)"; return 1; }
        # The vector is put back: mtvec reads the same before the window and after it.
        local before after
        before=$(sed -n 's/^mtvec: before=\([0-9a-f]*\) after=.*/\1/p' "$out")
        after=$(sed -n 's/^mtvec: before=[0-9a-f]* after=\([0-9a-f]*\)$/\1/p' "$out")
        [ -n "$before" ] && [ "$before" = "$after" ] || { echo "$name: FAIL (mtvec not restored: before=$before after=$after)"; return 1; }
        case "$pmp:$name" in
            true:*) grep -q "^pmp: cfg0=0x" "$out" || { echo "$name: FAIL (no cfg0 line)"; return 1; } ;;
            false:*) grep -q "^pmp: trapped mcause=0x00000002 (no PMP)" "$out" \
                || { echo "$name: FAIL (no trap line)"; return 1; } ;;
        esac
        echo "$name: PASS"
    else
        if grep -q "^identify: K230" "$out"; then
            echo "$name: FAIL (identify was reached; the test did not catch the bad vector)"
            return 1
        fi
        echo "$name: CAUGHT (identify not reached, qemu exit $status)"
    fi
}

case "${1:-all}" in
    pmp-true) run_case pmp-true true "" pass ;;
    pmp-false) run_case pmp-false false "" pass ;;
    fail-naive) run_case fail-naive false pmp-naive nopass ;;
    fail-broken) run_case fail-broken false pmp-broken nopass ;;
    all)
        rc=0
        run_case pmp-true true "" pass || rc=1
        run_case pmp-false false "" pass || rc=1
        run_case fail-naive false pmp-naive nopass || rc=1
        run_case fail-broken false pmp-broken nopass || rc=1
        exit $rc ;;
    *) echo "usage: $0 pmp-true|pmp-false|fail-naive|fail-broken|all"; exit 2 ;;
esac
