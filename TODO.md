# What is still open

The engine, the daemon, the tools, the kernel module and the drivers are written and pass the gate.
This is what is not done. It is kept short and honest: an entry leaves this file when the work is
done and shown.

## Hardware runs

Two runs on the reference board (2026-11-11) covered the kernel module, the backend's buses through
it (the PIO-made I2C bus among them, on its module route since the second run), the camera path and
the thermal zone; [`docs/hardware-status.md`](docs/hardware-status.md#the-first-board-run)
lists what they showed. Every other check under `testing/board-runs/` has no result yet, and
[`docs/testing.md`](docs/testing.md#board-runs) says how a run is done.

- [ ] Every check in `testing/board-runs/` that has no result.
- [ ] What the runs on the reference board left out: SPI and driven outputs through the module
      (`C3.md`, `C4.md`); the reset pulse (`C6.md`); the receiver's pool written back after a stop,
      and a second camera (`D5.md` 5, 8); a statistics range of the wrong size (`D7.md` 5); the
      back end's counters, which user space can't read, and its wedge test (`D6.md` 7, 8), a full
      ring and output 1 (`D17.md` 7, 8); the sensor's modes 2 to 5 (`D8.md`); its exposure
      linearity and single sixteenths, and when a posted exposure lands, which need the scene
      lit (`D8.md` 7, 8, `D10.md`); the chip and fan drivers' own checks (`B8.md` to `B12.md`, `B14.md`, `B15.md`);
      a host-owned chip (`A35.md`).
- [ ] The bench's reference set on the reference board, beside the earlier version's figures
      (`testing/bench/bench.sh --real`). The harness has no step for the camera, light and fan
      rows: their figures come only from the board runs `testing/bench/reference.tsv` names.
- [ ] The PiSP camera chain, a chain of links at `3324fec4` (F2c3): no board run has checked it.
      The earlier board records need correcting against it, and the PiSP sections of the runbook
      follow.
- [ ] The reference board's board file, two read-only re-checks, then a small correction to
      `testing/boards/reference.toml` from what the board says: the magnetometer's bus (the file
      says the PIO bus; an earlier run saw `i2c-1` at `0x10`), and the power monitor's chip and
      shunt values.
- [ ] The whole-system comparison on the board (F2), after the runs above. The hand-off to other
      repositories (F3) waits on it.

## Blocks that cannot be made safe from user space

Each of these is refused or cut down today because the vendor kernel's interface lets a job reach
memory that is not the caller's. They need a path through `lemnos.ko`, or cannot be closed at all.

- [ ] **K230 KPU** (`drivers/k230-kpu`): refuses to load a program. No IOMMU and no bus
      protection, so no kernel path of ours closes it without reading the program. Open until the
      chip offers something.
- [ ] **Rockchip NPU** (`drivers/rknpu`): confined only while the daemon is the node's sole
      opener. If a board run shows a program can reach the IOMMU's own registers
      (`testing/board-runs/E32.md`), it needs a kernel path too. The NPU address wrapper is parked
      for the owner's decision.
- [ ] **Rockchip AI-ISP** (`drivers/rkaiisp`): the layouts and checks exist; running the block is
      left out until its kernel driver's unchecked fields are bounded on a board.
- [ ] **A733 video engine encoder** (`drivers/a733-video`, K1c): the header side is written
      (K1d, PR #224: the QP map, the CTU info word and the codec, rate-control, pixel, profile and
      slice values, in `drivers/a733-video/src/encoder/`). What is missing is in the vendor
      library's binary, which is not read: the encoder's register offsets and encodings, the job
      structure, the register each record's address is written to, the QP map's stride and
      macroblock order, the encodings of the H.264 parameters, the bitstream's header and trailer,
      and how the engine reports a picture's length. No builder and no encoder job until those
      are known. The register facts need either the vendor's, or a register trace on a real board,
      which is the owner's decision. The licence question is settled (owner's ruling 2026-11-13: the
      facts may be transcribed, and are noted in `docs/drivers.md`).

## Unfinished in the code

- [ ] **ISP600 tuning encodings** (`drivers/isp600`, U4, PR #227): the channel black level and bit
      depths, module enables, and temporal denoise's record enable are encoded, and their sizes,
      places and default words are cross-checked against the vendor library at `516fd744`. They wait
      for a board run (`testing/board-runs/U3.md`, `U4.md`). Still missing, because they are defined
      only in the library's binary `libisp_algo.so` (not read): the register encodings of white
      balance, colour matrix, gamma, tone, lens shading, DRC, CEM, spatial denoise and sharpen, and
      the statistics decode. Also open in the layouts: the element layouts of the gamma and lens
      shading regions, no place for CEM's second table, and an LDCI buffer the layout does not
      hold. Until these are known the driver's tuning for those blocks is unsupported. The sources
      checked are in `docs/drivers.md` ("ISP600 tuning").
- [ ] **Two libraries with no contract driver**, rechecked against the public sources on
      2026-11-11:
  - `drivers/k230-ai2d`: the block's job registers are in no public source. The SDK kernel's
    `drivers/misc/canaan/ai2d/k230_ai2d.c` only imports dma-bufs; the RT-Smart driver
    (`kendryte/k230_sdk` `src/big/rt-smart/kernel/bsp/maix3/board/interdrv/gnne/ai2d_dev.c`)
    names one interrupt register (`0xca0`) and takes no job; `kendryte/nncase` at `master` has
    no AI2D source (the K230 runtime ships prebuilt); `canmv_k230` `port/kpu/ai2d.c`, the SDK's
    `ai2d_kpu` and `python-k230` packages and the K230 AI2D runtime manuals only call the
    runtime's builder. Unknown: every register offset and field of a job (addresses, sizes,
    strides, formats, the start bit).
  - `drivers/rk-jpeg`, the decoder: its field layouts are public in Rockchip MPP (`rockchip-linux/mpp`
    at `9b56348`, Apache-2.0: `mpp/hal/vpu/jpegd/hal_jpegd_rkv_reg.h`, `hal_jpegd_vpu730_reg.h`, and
    how `hal_jpegd_rkv.c` fills them). What no source states is how far the decoder reaches from
    each address, which a job's check needs: where it puts its chroma plane after
    `reg13_dec_out_base` and how large it is in each `yuv_out_format`, tiled and scaled-down mode;
    and the unit of `qtbl_len`, `htbl_mincode_len` and `htbl_value_len` (the table sizes are `RKD_*`
    constants, not hardware facts). Until those are known the decoder builds no job, and JPEG
    decodes go through `rk-mpp` with the caller's registers. The encoder (`drivers/rkjpege`, the
    VPU720 class) has its file pinned and its job built; what stays open is how far it reads past
    the encoded picture (the frame's padding rows, which `hal_jpege_vpu720.c` derives from
    `ver_stride`) and the extent of the ECS length buffer (`reg021_adr_ecs_len`): the builder checks
    the encoded picture only and does not place the ECS buffer, which the vendor HAL leaves unset on
    the frame-by-frame path.

## Frame streams and the shared ring

- [ ] **Frame streams on the shared ring** (I2). `lemnosd`'s streams, the client and the C API still
      use their own ring and doorbell (`docs/streams.md`), and move onto `shm-ring`,
      `shm-ring-buffers`, `shm-ring-holds` and `shm-ring-doorbell` (`docs/ring.md`), with
      descriptors passed by `shm-fdpass`. Open:
  - I2a (PR #221, in CI): the streams on the shared ring. Until it merges, the entry above stands.
  - I2b: the kernel module's stream ring onto the shared layout.
  - I2c: the job ring.
  - I2d: outside readers by ticket. A third process gets a reader slot from the service, and the
    service asks the daemon for a ticket on its own connection. Read and import rights are kept
    apart: a read-only mapping is enforced by the kernel, but an importer of a dma-buf can write
    through its device, so an importing reader is trusted with the frame's content. Additive
    wire frames only.

## Contracts and the wire

- [ ] **Per-contract hashes, bind checks and feature parts** (T1d to T1f). T1, T1b and T1c have
      merged: the wire layout hash, the compatibility rules and the core hash, struct blocks and
      wire parts. Still open: the hash per contract, the checks at bind against it, and the feature
      parts, including `ENDPOINTS` and `SHARED_RINGS`, in one form.

## Orion bridge and the daemon

- [ ] **Bridge to the Orion lock** (I6b): the bridge builds against the Orion lock at `455a956e`,
      keeps one copy of the shm crates, and withdraws per fact. Not merged.
- [ ] **Bridge replies and the paged slots list** (M8a, PR #223, open: the out-buffer size in the
      text path, and the bridge refusing oversize replies by name; M8b: the paged slots list and a
      live bridge walk, not started).
- [ ] **Flaky tests, unexplained** (M9): the bridge restart's `NO_DEVICE`, the `orion_bridge.rs`
      restart test, `closing_ends_what_the_connection_held`
      (`service/lemnosd/tests/service.rs`), and `health_quiet`. No cause is found yet.
- [ ] **Shm family in its own repository**: owner's decision pending. Nothing moves until it is made.

## USB gadget

- [ ] **NCM interface name** (R1e): open; R1a to R1d have merged (the NCM and ACM functions, the
      per-board addresses, the gadget end to end in QEMU).
- [ ] **What an endpoint holder sees on teardown** (R1f): open; the gadget's bulk endpoints are
      lent to one client (R1b), and the teardown case is not yet settled.
- [ ] **VBUS supply on device-mode ports** (Q2f): open; the USB device ports are in the maps
      (Q2e), but the VBUS supply is not.

## Camera and sensors

- [ ] **Embedded data lines from the receivers** (J4b3, PR #225, open): embedded data lines, the
      metadata mode, and the sunxi-vin answer.
- [ ] **Descriptions through the package ABI** (V1b): open. V1a has merged (label, kind, bus,
      address, reading, range and limits, and `lemnos-ctl show`); the package ABI side and the
      sensor exposure and gain items are not.

## Firmware, slots and the first flash

- [ ] **First-flash write** (W1): the raw first-flash write, with the bridge's streaming sink. W2
      (indicator states) has merged.
- [ ] **Raw NAND first flash** (W1b).
- [ ] **Modules-off kernel boot in CI** (W3b): the kernel built with modules off is not yet booted
      in CI. W3 has merged (the configuration, and the built-in module booted in QEMU).
- [ ] **Trial health and hotplug origin** (I5f3, PR #226, open): trial health without blocking, the
      reason kept when the state fails, the hotplug origin taken from the bus, and the static host
      refusing board watchers.

## Follow-ups

- [ ] A mutation test for the watchdog driver (`drivers/hw-watchdog`).
- [ ] The deprecated `libc::time_t` in `backends/linux-sys/src/aio.rs` (lines 101 and 757).
