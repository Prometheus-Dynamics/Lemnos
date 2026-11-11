# What is still open

The engine, the daemon, the tools, the kernel module and the drivers are written and pass the gate.
This is what is not done. It is kept short and honest: an entry leaves this file when the work is
done and shown.

## Hardware runs

Nothing has been run on hardware in its present form. Every piece of hardware-facing code has a
written check under `testing/board-runs/`, and none records a result.
[`docs/hardware-status.md`](docs/hardware-status.md) lists them by area, and
[`docs/testing.md`](docs/testing.md#board-runs) says how a run is done.

- [ ] Every check in `testing/board-runs/`.
- [ ] The bench's reference set on the reference board, beside the earlier version's figures
      (`testing/bench/bench.sh --real`). The harness has no step for the camera, light and fan
      rows: their figures come only from the board runs `testing/bench/reference.tsv` names.
- [ ] The whole system on the kernel-module route against the stock route, on a real bus.
- [ ] `lemnos-orion` against a running node.

## Blocks that cannot be made safe from user space

Each of these is refused or cut down today because the vendor kernel's interface lets a job reach
memory that is not the caller's. They need a path through `lemnos.ko`, or cannot be closed at all.

- [ ] **VIP9000 NPU** (`drivers/vip9000`): refuses to load a program. The block has one page table
      for every opener. Needs a kernel path that gives each job its own.
- [ ] **A733 video engine** (`drivers/a733-video`): register reads only. An address written into a
      register cannot be bounded from user space. Needs a kernel path that checks or writes them.
- [ ] **K230 DMA engine** (`drivers/k230-gsdma`): the kernel gives user space no way to the block.
      Needs a kernel node.
- [ ] **K230 KPU** (`drivers/k230-kpu`): refuses to load a program. No IOMMU and no bus
      protection, so no kernel path of ours closes it without reading the program. Open until the
      chip offers something.
- [ ] **Rockchip NPU** (`drivers/rknpu`): confined only while the daemon is the node's sole
      opener. If a board run shows a program can reach the IOMMU's own registers
      (`testing/board-runs/E32.md`), it needs a kernel path too.
- [ ] **Rockchip AI-ISP** (`drivers/rkaiisp`): the layouts and checks exist; running the block is
      left out until its kernel driver's unchecked fields are bounded on a board.
- [ ] **K230 ISP frames** (`drivers/k230isp`): configuration and statistics only. No register that
      takes an address may be written.

## Unfinished in the code

- [ ] **A PIO-made I2C bus does not go through the kernel module.**
- [ ] **Libraries with no contract driver**: `drivers/nonai2d`, `drivers/vglite`,
      `drivers/k230-ai2d` (its register map is not public), `drivers/rk-jpeg` (builds no job: the
      field layout is not public; JPEG runs through `rk-mpp` with the caller's registers).
- [ ] **The description form covers one contract**, `magnetometer/1`.
- [ ] **No built-in driver takes a UART.**
- [ ] **No built-in driver takes a block's registers, interrupt, clocks or resets** from the module
      (`regs:`, `irq:`, `clocks:`, `resets:`). The block drivers built in open their own kernel
      nodes.
- [ ] **The C API has no frame streams and no package list.**
- [ ] **The camera proof tool runs in one process.** It has no way to reach devices through the
      daemon's socket.
- [ ] **A USB camera needs a board entry.** There is no binding by device class.
- [ ] **USB**: the gadget has no isochronous endpoints, and `uvc` reads one isochronous transfer
      at a time.
- [ ] **Capture**: `rkcif` has no 16-bit unpacked layout; multi-plane frames are copied; nothing is
      captured straight into a stream while no consumer is attached.
- [ ] **`lemnosd` has no settings for the host**: the call deadline, the retry times and the
      opt-in auto-off policy are fixed at their defaults.
- [ ] **Two tests have been reported to fail now and then** under load:
      `service/orion/tests/orion_bridge.rs` and
      `call_reaches_any_method_by_name_or_with_its_tag` in `service/ctl`. Not yet traced.
