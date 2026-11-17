# Parked

Work that is open but can wait, with the reason it can wait. Each line says what, and why it can
wait. The open items themselves are in [`TODO.md`](TODO.md). Reviewed at each milestone.

- `SIM1`: the simulated camera chain with front and back ends and frames that carry pixels. The
  simulator has neither block, and the reference board's chain does not depend on it.
- `ASKBUF-MOCK`: mock tests for `ask_buffer` in `k230isp` and `rkisp`. Coverage only; both run on
  their boards.
- `I2b2-6`: the sys crates move to the shared ring's form. The kernel path reads through the daemon's
  ring until then, which is what the reference board uses.
- `I2d`: readers by ticket, for a third process on a stream's own buffers. The current consumer
  keeps its readers inside its service, so it does not need this. It moves the shared-rings hash,
  so it needs its own announcement and a re-pin of the consumers.
- `I6d`: a detached run's job survives the bridge's own session loss. It needs the Orion side's
  persisted runs first; half of it on our side alone is worse than none.
- `REATTACH`: a new session re-attaches to a detached run. Same wait as `I6d`. Until then the app
  shows the outcome from the device's own last-install fact.
- `RESUME-LOST`: a driver's resume record survives a host-level rebind. It changes the driver
  interface, so it needs its own review. Until then a rebind reports `Lost`, which is safe.
- `HEALTH-FIG`: measured figures in a driver's health reason. It needs the core's `Health` type to
  hold a formatted reason. Health still names the step reached, which is enough for now.
- `VALIDATE-RES`: a `uses` naming a missing bus, controller or line is caught at bind, not by
  `lemnos-ctl validate`. Bind refuses it, so nothing runs wrong; the check needs the SoC description.
- `OV-RANGES`: the ov9782's exposure and frame-length ranges are empty until a mode is selected.
  Only `describe` is affected.
- `BE-HWVER`: the PiSP back end reports hardware version 0 before its first job. Only `describe` is
  affected; the fix is to read it at bind, which is not done yet.
- `FSTOP`: the capture drivers `k230-capture`, `rkcif`, `sunxi-vin` and `rp1-csi2` do not notice when
  frames stop. The fix is named: a frozen frame-start watch, as the ov9782 has. `rp1-csi2` is the
  reference board's receiver, so this is a known gap on that path until the watch lands.
- `SILENCE`: a software way to make a streaming sensor go silent on the reference board. The enable
  line does not do it. It waits for the `s1` step's health wait to be fixed before its next use.
- `RAWBUS-RO`: a raw-bus device cannot be read-only and still read registers, because register reads
  need writes. It needs a policy decision: "may write the address pointer for a read" against "may
  write".
- `STANDIN`: test doubles that pass where the real object fails. One failing-like-the-real-thing
  case each is wanted. It can wait until the camera path is board-proven.
- `R2-PREV`: the Pi "previous before flashing" mode is untested on board. It is tried only on the
  first reference-board image, as a trial slot, or marked sim-only. The first image comes first.
- `I5i8-NAND`: NAND and UBI bays. Not needed for a board that boots from eMMC. Needed before a NAND
  board takes the slot code.
- `UB-RUNS`: U-Boot board runs for the `fdt` alias read, `fdt set` and the download-mode loop. They
  come after the first board runs of the next image.
- `UB-SIM`: the real `boot.cmd` under U-Boot's sandbox or QEMU. No U-Boot board is in the
  milestone, so the check can wait; every U-Boot board note says it is untested until then.
- `V1G-BOARD`: the V1g residual checks and the Pi boot-mode codes other than 1. Board time, after
  the reference board's runs.
- `HASH-F2H`: `camera.sensor/1`'s base hash moved at F2h without an `#[added]` mark. The next bind on
  the board shows whether it is refused. The break list says rebuild.
- `PINNED`: the pinned kernel set is stale against the harness configs (`CONFIG_EROFS_FS_ZIP`).
  CI does not use it. Rebuild when no harness run is queued.
- Shm ring, revoke acknowledgement: a cut reader may still be reading a frame when its buffer is
  reused, the same residual as our own cut. The producer-side view of holders is merged (`R1a`).
  Needs a reader-side acknowledgement, which re-opens the ring crate; after the camera path.
