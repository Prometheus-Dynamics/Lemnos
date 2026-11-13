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

## Blocks that cannot be made safe from user space

Each of these is refused or cut down today because the vendor kernel's interface lets a job reach
memory that is not the caller's. They need a path through `lemnos.ko`, or cannot be closed at all.

- [ ] **K230 KPU** (`drivers/k230-kpu`): refuses to load a program. No IOMMU and no bus
      protection, so no kernel path of ours closes it without reading the program. Open until the
      chip offers something.
- [ ] **Rockchip NPU** (`drivers/rknpu`): confined only while the daemon is the node's sole
      opener. If a board run shows a program can reach the IOMMU's own registers
      (`testing/board-runs/E32.md`), it needs a kernel path too.
- [ ] **Rockchip AI-ISP** (`drivers/rkaiisp`): the layouts and checks exist; running the block is
      left out until its kernel driver's unchecked fields are bounded on a board.

## Unfinished in the code

- [ ] **ISP600 tuning encodings** (`drivers/isp600`): the channel black level and bit depths, module
      enables and temporal denoise's record enable are encoded and wait for a board run
      (`testing/board-runs/U3.md`). No public, licensed source defines the register encodings of white
      balance, colour matrix, gamma, tone, lens shading, spatial denoise, sharpen, or any statistics
      window or decode. The sources checked, and why each is not used, are in `docs/drivers.md`
      ("ISP600 tuning"). Until one is found the driver's tuning for those blocks is unsupported.
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
- [ ] **Frame streams are not on the shared ring** (I2). `lemnosd`'s streams, the client and the
      C API still use their own ring and doorbell (`docs/streams.md`). Move them onto
      `shm-ring`, `shm-ring-buffers`, `shm-ring-holds` and `shm-ring-doorbell` (`docs/ring.md`),
      and pass descriptors with `shm-fdpass`.
