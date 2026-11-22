# BOOTFILES-OWNER board run

Status: untested on board. Nothing here has touched a board. The defect was found on a board pass (a first-flashed
data image: `slots/bootfiles` root:root 0755, and the first live install whose boot files differed from the stored
ones failed at its finish with `the slot's boot files: Permission denied (os error 13)`, after all bytes were taken
and the signature checked).

The fix: `Layout::daemon_dirs` (the state dir, the images dir and the boot-file store) is the one list of the dirs the
daemon writes on the data partition. slot-init gives each of them to the daemon's user at every owned boot
(`lemnos_slot_store::owner::give_daemon_dirs`), so a board flashed before the fix gets the store at its next boot. The
daemon's write probe (`state-unwritable`) checks the same list, so a store it still can't write refuses an install
before any byte.

Proven in QEMU: `testing/qemu/run-slot-install.sh` (kvm, next 612aea88 plus this change), with its data partition now
made by `lemnos-ctl data-partition` (signed base-1, root's as on a board) and the guest giving the dirs by the same
function. It installs install-1, whose boot files differ from base-1's, under the packaged unit.

- after: the job ends committed (rc 0), and the boot partition verifies with install-1 in a bay;
- before (`daemon_dirs` cut back to the state and images dirs, the rule before the fix): job status -5, `info`
  `last_attempt` failed with `the slot's boot files: Permission denied (os error 13)`, received 9629696 of 9629696.

Unit tests: slot-init gives `data/slots/bootfiles` (fails before); the first-flash tree makes no dir outside the list
except the root and `board/` (`service/ctl/tests/data_partition.rs`); each given dir is under a writable `/data` grant
of the unit (`drivers/slot-store/src/owner.rs`); a boot-file store the daemon can't write refuses an install before
any byte (`drivers/system-slots/src/tests.rs`, fails before).

## Checks to run on the board (a board flashed with `lemnos-ctl data-partition`)

1. Before updating it: `ls -ld /data/slots/bootfiles` is `root root`. Boot the fixed build once: the kernel log
   has slot-init's `/data/slots/bootfiles is given to lemnos (U:G)`, and `ls -ld` shows lemnos.
2. Install a slot whose boot files differ from the stored one (a different cmdline or kernel): the job ends
   committed, and `info` shows `last_attempt` installed.
3. Boot again: no `is given` line (the dir is already the user's), and nothing else under `/data` changed owner.
