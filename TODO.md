# What is still open

The engine, the daemon, the tools, the kernel module and the drivers are written and pass the gate.
This is what is not done. An entry leaves this file when its work is merged and shown. Items that
were dropped on purpose are not listed here, and the items parked for later are in
[`PARKED.md`](PARKED.md). The IDs in backticks are the planning IDs.

## Hardware runs

Two runs on the reference board (2026-11-11) covered the kernel module, the backend's buses through
it (the PIO-made I2C bus among them, on its module route since the second run), the camera path and
the thermal zone; [`docs/hardware-status.md`](docs/hardware-status.md#the-first-board-run)
lists what they showed. [`docs/testing.md`](docs/testing.md#board-runs) says how a run is done.

- [ ] `BR-OPEN`: every check in `testing/board-runs/` that has no result (`C3.md`, `C4.md`, `C6.md`,
      `D5.md`, `D6.md`, `D7.md`, `D8.md`, `D10.md`, `D17.md`, `B8.md` to `B15.md`, `A35.md`).
      They need the reference board, a lit scene for the exposure checks, or a second camera.
- [ ] `BENCHREF`: the bench's reference set on the reference board, beside the earlier version's
      figures (`testing/bench/bench.sh --real`). The camera, light and fan rows come only from the
      board runs that `testing/bench/reference.tsv` names.
- [ ] `PISP-RUN`: the first board run of the PiSP camera chain, a chain of links at `3324fec4`
      (F2c3). The earlier board records need correcting against it, and the PiSP sections of the
      runbook follow.
- [ ] `REFTOML`: the reference board's board file, two read-only re-checks, then a small correction
      to `testing/boards/reference.toml` from what the board says: the magnetometer's bus (the file
      says the PIO bus; an earlier run saw `i2c-1` at `0x10`), and the power monitor's chip and
      shunt values.
- [ ] `LIT`: the exposure checks with a lit scene (`D8.md` 7 and 8, `D10.md`): exposure linearity,
      single sixteenths, and when a posted exposure lands.
- [ ] `R-PASS`: the first frame after a reattach, and the span from the front end's done to the
      back end's done to the client, from the per-frame stamps.
- [ ] `V1G-BOARD`: the V1g checks on a board, and the Pi boot-mode codes other than 1.
- [ ] `F2`: the whole-system comparison on the board, after the runs above.

## Reference board bring-up (milestone 2)

- [ ] `GROW`: the data-partition grow. Part 1 is ready and not landed. The threaded resize
      (`GROW2`) waits for the 2 TiB liveness and hard-reset runs to pass, and for the 256 GiB
      refusal; the GPT rewrite, stale data and the image builder's contract come after (`GROW3`).
- [ ] `M37b-bridge`: the Orion bridge passes the refusal kind through: a typed `refusal` key on the
      status topic, the describe record and the snapshot. Ready; not landed.
- [ ] `M41`: a second reference-board check of bring-up on `c8528314`. It closes on its result.
- [ ] `M41b`: the 250 ms limit on the bring-up line's stamp, checked on the next board run that
      happens anyway.
- [ ] `M40`: a stall bound for each long job method, declared in the driver contract and timed by
      lemnosd. A progress report renews it; a silent job is cancelled and ends `TIMED_OUT` (110).
      The lease-ended fact becomes a typed field on the reply.
- [ ] `FEAT1`: the product binaries (lemnosd, lemnos-ctl, lemnos-orion, lemnos-slot-init) built in
      one cargo run are byte-identical to solo builds. First check whether a test feature can reach
      a product binary through feature unification.
- [ ] `ORION-R3-WIRE`: the board's security switch asks the Orion bridge for the security
      precondition (`system.security-precondition/1`), with a bounded deadline and a named refusal
      when it does not answer. Waits for the Orion pin that includes it.
- [ ] `SLOT-MACHINE-ID`: slot-init derives the machine id from the serial and binds it over
      `/etc/machine-id`. Work in progress on a local commit; not landed.
- [ ] `R2-CFG`: the board-kit check accepts the `RUST_IS_AVAILABLE` line. Not started.
- [ ] `R2-IMG`: the next image, first flash on the reference board, boot, measure, and the layout
      declaration. Blocked on the items above.
- [ ] `R2-PREV`: the Pi "previous before flashing" mode, through a trial slot, or marked sim-only.
      Needs the first image.

## Bench boards

- [ ] `MR1`: the ZERO 3E maskrom flasher, with its own watchdog-first stage. In progress; held.
- [ ] `FL1`: the flasher split. The PC side belongs to the flasher tool in another repository. The
      board side moves to `drivers/` as blobs: the stage crate (watchdog first, bounded waits,
      verify before the header), and the FEL thunks as a `no_std` blob crate with a test that the
      built bytes equal the uploaded bytes. The wire crate and the Rockchip maskrom stage are merged.
- [ ] `K230-MAP2`: the K230 map in `socs/k230.toml` uses the mainline kernel names (`usb@91500000`,
      `mmc@91581000`, `mmc@91580000`). Pushed and ready; the UDC test changes with it.
- [ ] `K230-USB`: the K230 USB-boot reflash, with its own watchdog-first stage, bounded waits, and a
      return that is proven before any write. Queued after `K230-MAP2`. Review before the first write.
- [ ] `UB-SIM`: run the real `packaging/uboot/boot.cmd` under U-Boot's sandbox or QEMU, against
      the environment and FAT image the writers produce. Not started.
- [ ] `BB-FACTS`: board facts for the Aura, K230D, A7A and A7Z, and the A733 watchdog path.
      Blocked on the other repository's answers.
- [ ] `BB-GAIA`: the K230D boot-source key and its env revision. Blocked on the same.
- [ ] `BB-IMG`, `BB-FLASH`, `UB-RUNS`, `BB-OTA`: a next image per bench board, a hands-free first
      flash per board, the U-Boot board runs (`I5i5`, `I5i7`), and one remote update with a return
      per board. Each comes after the items above.
- [ ] `I5i8-NAND`: NAND and UBI bays, only if a bench board boots from NAND. Otherwise dropped.

## Blocks that cannot be made safe from user space

Each of these is refused or cut down today, because the vendor kernel's interface lets a job reach
memory that is not the caller's. They need a kernel path through `lemnos.ko`, or a register fact
from the vendor side or a board trace.

- [ ] **Rockchip NPU** (`drivers/rknpu`): confined only while the daemon is the node's only opener.
      `RKNPU-E32`: if a board run shows a program can reach the IOMMU's registers
      (`testing/board-runs/E32.md`), it needs a kernel path too.
- [ ] **Rockchip AI-ISP** (`drivers/rkaiisp`): running the block waits until its kernel driver's
      unchecked fields are bounded on a board (`RKAIISP`, a canary probe in a vendor trial slot).
- [ ] **A733 video engine encoder** (`drivers/a733-video`): the header side is written. The register
      facts are not: the encoder's offsets and encodings, the job structure, the QP map's stride,
      the bitstream header and trailer, and how the engine reports a picture's length. They come
      from a register trace on a vendor trial slot (`A733ENC`). Until then there is no encoder job.
- [ ] **K230 AI2D** (`drivers/k230-ai2d`): the job registers are in no public source. They need a
      trace on a vendor trial slot (`AI2D`), with the unattended return proven first.
- [ ] **Rockchip JPEG** (`drivers/rk-jpeg`, `drivers/rkjpege`): the decoder builds no job until the
      reach past the chroma plane and the table units are known (`RKJPEG-DEC`, a probe on the ROCK
      5C). The encoder's reach past the picture and the ECS buffer's extent are open (`RKJPEGE`).

## Frame streams and the shared ring

- [ ] `I2b2-6`: the sys crates move to the shared ring's form (one ring form, no 1 ms polls left).
      The kernel module's own stream ring (`LEMNOS_RING_*`, in `kernel/lemnos.h`) is still the old
      form that `drivers/pisp-fe-sys`, `drivers/pisp-sys`, `drivers/rp1-csi2-sys` and
      `drivers/k230-gsdma-sys` read.
- [ ] `I2d`: outside readers by ticket. A third process gets a reader slot from the service, and
      the service asks the daemon for a ticket on its own connection. Read and import rights are
      kept apart. Changes the shared-rings hash, so it needs its own announcement and a re-pin.
- [ ] `RESUME-LOST`: a driver's opaque resume record survives a host-level rebind, so a rebind
      reports `Resumed{Kept}` instead of `Lost`. Changes the driver interface; needs its own review.
- [ ] `SIM1`: the simulated camera chain with front and back ends, and frames that carry pixels.
- [ ] `REATTACH`: a detached bridge run is re-attached through a new session (a test; fix if it
      fails). `I6d`: a detached run's job survives the bridge's own session loss, and is re-offered
      after the session restores. Both wait on the Orion side's own work.

## Camera and capture drivers

- [ ] `OV-RANGES`: the sensor's exposure and frame-length ranges per mode, not empty until a mode is
      selected.
- [ ] `BE-HWVER`: the back end reads its hardware version at bind, not 0 before its first job.
- [ ] `FSTOP`: the capture drivers that do not notice frames stopped (`k230-capture`, `rkcif`,
      `sunxi-vin`, `rp1-csi2`) get the frozen frame-start watch.
- [ ] `SILENCE`: a software way to make a streaming sensor go silent on the reference board, for the
      silence watch and the restart test (`F2i`). Also fix the pre-cut health wait of the `s1` step.
- [ ] `RAWBUS-RO`: a raw-bus device that is read-only still reads its registers. Needs a policy
      that splits "may write the address pointer" from "may write".
- [ ] `HASH-F2H`: the `camera.sensor/1` base hash is checked by a bind on the board.

## Checks in the daemon and the tools

- [ ] `VALIDATE-RES`: a `uses` naming a bus, controller or line the backend does not have is caught
      by `lemnos-ctl validate`, not only at bind. Needs the SoC description (`socs/`) to list them.
- [ ] `HEALTH-FIG`: the health reason carries the measured figures, in a fixed buffer, not static
      text. Needs the core's `Health` type to hold a formatted reason.
- [ ] `STANDIN`: the stand-in audit. Walk the client, host, C surface and shm tests for test doubles
      that pass where the real object fails (a memfd for a dma-buf, a socketpair for FunctionFS, a
      regular file for a block device). Add one failing-like-the-real-thing case each, or say why not.

## Kernel module and the C code

- [ ] `DOC-MODPROBE`: `docs/device-tree.md` says a blacklist stops automatic loading only, that the
      generated file needs a modprobe that honours `blacklist` and `options`, and that nothing loads
      a blacklisted stock module before `lemnos.ko`. The image check names a modprobe that ignores
      blacklists. Lands with the next PR that touches the docs.
- [ ] `CZ-MOD`: the kernel module is ported to Rust by subsystem, each stage proven against the C
      module's tests in the QEMU harness (KASAN on, clang) before that C is deleted. One C helpers
      file of one-line shims remains, with a size cap and a CI check. `rust-src` joins the toolchain.
- [ ] `CZ-GUEST`: the QEMU guest tests, the test kernel modules' userspace, and the guest init and
      watchdog programs move to Rust. KASAN stays in the harness.
- [ ] `PINNED`: the pinned kernel set used by the QEMU harness is stale against
      `testing/qemu/kernel.config` and `kernel-modules-off.config`: `CONFIG_EROFS_FS_ZIP` differs.
      Rebuild it when no harness run is queued.

## Tests and flakes

- [ ] `M41b` flake: `the_call_action_carries_an_out_buffer_reply_whole_or_names_why_not`
      (`service/orion/tests/orion_bridge.rs`) once refused `TIMED_OUT` ("the device can't be described
      now"): the identity lane was held by other work for the whole call deadline. Seen once in about
      90 loops under load; the holding work is not yet identified.
- [ ] `M9`: the bridge restart's `NO_DEVICE`, `closing_ends_what_the_connection_held`
      (`service/lemnosd/tests/service.rs`), the `orion_bridge.rs` restart test, and `health_quiet`:
      none reproduced in 100 loops each. Open until 200 loops pass or one fails with its output.
- [ ] `FLAKE-CETE`: `chain_end_to_end` (`testing/crates/camera-proof/tests/chain_end_to_end.rs`)
      not reproduced in the loops run so far. Open.
- [ ] `FLAKE-CSI2` (formerly `FLAKE-RP1`): the QEMU check `lemnos_rp1_csi2_check` "STOP waits for DMA
      on its own buffers" returned early under host load. Cause: the harness fake's progress is a
      delayed work the host can starve. Owned by another worker; the fix is a design choice (an MMIO
      model in the harness, or a board fact about the receiver's DMA status).
- [ ] `FLAKE-ORION`: a bridge test (`a_comm_core_with_a_short_idle_time`) failed once under load,
      with "Restoring after restart". Its fix is on a branch not yet merged.
- [ ] `ASKBUF-MOCK`: mock tests for the `ask_buffer` of `k230isp` and `rkisp`.

## Follow-ups

- [ ] `WDT-MUT`: a mutation test for the watchdog driver (`drivers/hw-watchdog`).
- [ ] `TIME_T`: the deprecated `libc::time_t` in `backends/linux-sys/src/aio.rs` (lines 101 and 757).
- [ ] `X2A2`: the self-test entry of `#299`, and a green CI. An open pull request.
- [ ] `F3`: the hand-off. The other repositories move to this branch, and the old `dev` branch is
      retired. After `F2`, last.
