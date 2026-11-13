# I5h board run: `lemnos-slot-init` on a kernel missing a piece of the chain

Status: untested on board. The self-check and the removal of the module loader have run only in
the crate's tests, over a temporary directory with fakes for every system call. Nothing has been
booted with a kernel that lacks a piece.

Not run on the shared board (CM5). This check changes the boot path of a slot and needs a second
kernel built without one piece; a board run takes `/run/lock/board-gpu.lock` and restarts the
board, which the shared board's other users do not allow for this check.

## What the run would do

Setup as in `I5d.md` (the slot initramfs holding `lemnos-slot-init` as `/init`, no modules), with
two kernels built from the same tree:

1. **Complete kernel.** Plain boot. Expect `starting slot A` and no `the kernel lacks` line.
2. **Kernel without `CONFIG_DM_VERITY`** (built with it off, or as a module). Restart. Expect on the
   console and in `slot-init.log`: `the kernel lacks CONFIG_DM_VERITY: the verity target over the
   slot's image`, then `no slot can start: ...`, a 30 s wait and a restart. Expect `A.meta` to
   have no `FAILED` and `FAILURES` unchanged, and `boot/trial-attempts` (if a trial was pending)
   unchanged.
3. **Trial on that kernel.** Start `B` on trial with the complete kernel, then boot the kernel
   without the option. Expect `the kernel lacks CONFIG_DM_VERITY` in `trial-reason` on the boot
   partition, the attempt given back (`trial-attempts` one higher), `trial.txt` kept, and the
   default `A` booting after a plain restart (the daemon's `list` then shows the trial's reason as
   the kernel option when it resolves the trial).
4. **Missing storage driver.** Not run here: needs a kernel without `MMC_BLOCK` and a board with
   no other storage. Expect `the kernel lacks CONFIG_MMC_BLOCK` in place of a bare "no data
   partition".

## Results

Not run yet.
