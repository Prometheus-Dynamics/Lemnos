# Parked

Work that is open but can wait, with the reason it can wait. Each line says what, and why it can
wait. The open items themselves are in [`TODO.md`](TODO.md). Reviewed at each milestone. An item
leaves this file when its work is merged, or when it is dropped.

- `I2b2-6`: the sys crates are on the shared ring's form, but the kernel module's own stream ring
  (`LEMNOS_RING_*` in `kernel/lemnos.h`) is still the old form. The daemon reads that ring through
  its own path until the kernel side moves, which is what the reference board uses.
- `I6d`: a detached run's job survives the bridge's own session loss. It needs the Orion side's
  persisted runs first; half of it on our side alone is worse than none. Blocked on Orion's work.
- `STANDIN`: test doubles that pass where the real object fails. One failing-like-the-real-thing
  case each is wanted. It can wait until the camera path is board-proven.
- `R2-PREV`: the Pi "previous before flashing" mode. The simulated test and its record have landed
  (`testing/board-runs/R2-PREV.md`); the board check is not run, and waits for the first reference-board
  image. Until then the previous-file path is marked sim-only.
- `UB-RUNS`: U-Boot board runs for the `fdt` alias read, `fdt set` and the download-mode loop. They
  come after the first board runs of the next image.
- `V1G-BOARD`: the V1g residual checks and the Pi boot-mode codes other than 1. Board time, after
  the reference board's runs. Its checks take down the USB gadget, which is the only route to the
  board, so they need a second route first.
- `AFTER-IMAGE`: the health source `tcp:@<iface>:<port>` means this interface's own address, since
  `lemnos0` is per-board at boot. Parked by the coordinator on 2026-11-18; comes after the board image.
- Shm ring, revoke acknowledgement: a cut reader may still be reading a frame when its buffer is
  reused, the same residual as our own cut. The producer-side view of holders is merged (`R1a`).
  Needs a reader-side acknowledgement, which re-opens the ring crate; after the camera path.
