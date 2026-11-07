# TODO

Tracks Lemnos work driven by the apps that integrate it and by the camera stack that consumes its
foundation ([docs/foundation.md](docs/foundation.md)). See [CHANGELOG.md](CHANGELOG.md) for the
user-facing summary of finished work. The version stays on the 2.0 line; releases are the user's
call.

Boards: two BCM2712 boards, #1 and #2, each with a USB webcam and a Coral USB accelerator.

## Decided, starts when the user says

Everything planned, in order. Execution is paused until the user starts it.

- [x] **1. `lemnos-abi`.** `Status` (`i32`: 0 ok, a negated errno passes through, own codes from
      -4096, positive counts), generation-counted `Handle` and `Slab` (released slots reused with a
      new generation, a slot retired before its generation wraps), `BufferDesc` (4 planes, fourcc,
      modifier, `OWNED` flag, `struct_size`) and `FenceDesc` (none, sync file, syncobj + point),
      `MessageSink`, `AbiVersion` (major, minor, struct size, layout hash from sizes and offsets),
      and `guard`/`guard_or` (a function, not a macro: a closure needs no macro). `no_std`, no
      dependencies, no unsafe. Host tests only. Decided: negative `i32` status, not the graph
      runtime's positive `u32`: a kernel errno passes through without a mapping table, and the
      MCU template already returns negative `i32`. Its stable ABI adopting this is its owner's
      change.
- [x] **2. C surface and client wrapper.** `lemnos-capi`: the `lemnosd` device client behind ten
      C functions (`include/lemnos.h`, surface 1.0), `staticlib` and `cdylib`, and `Client`, a Rust
      wrapper calling the same exports. A C program (`tests/c/client.c`) is built against each
      library and run on mock hardware: handshake, list, read, three streamed readings, close,
      stale handle. Release `.so` 585 KB unstripped, exporting only the ten `lemnos_*` symbols.
      Host, release, load 93: a read is 122-131 us direct or through the surface (the IPC round
      trip; the surface's share is in the noise); the cheapest surface call (device count) 34-39
      ns. Decided: unsafe is allowed in a second crate, `lemnos-capi`, under the same rules as
      `lemnos-linux-sys` (own lint table, one operation per block, `SAFETY` comments); the Rust
      wrapper calls the exports directly, so static mode needs no loader (a loader for the
      shared form is item 7). The layout hash leaves out pointer-sized types, so it is one
      constant on every target (a compile-time assert; checked on armv7).
- [x] **3. `lemnos-ipc` written spec.** [docs/ipc.md](docs/ipc.md): the byte-level protocol as the
      code has it (frames, encodings, enum codes, handshake, every request and message, replies,
      lifecycle, compatibility, limits), with 11 test vectors that
      `crates/lemnos-ipc/src/spec_vectors.rs` encodes, decodes and compares byte for byte. Writing
      it found 17 gaps (its section 14). Open from them:
  - [x] Defects, fixed 2026-11-05 (docs/ipc.md section 14 marks each): the version is never checked by either side; replies, frames and the
        other non-reading messages queue without a bound; the client keeps a bad frame in its
        buffer and fails every later receive; a connection that never sent `Hello` keeps its
        claims, control writes and LED intents after it closes; a refused LED request gets no
        reply; an inline look with no layers closes the connection; edge events ignore the
        `events` flag; a request inside the byte limits can exceed the frame limit and is closed
        without a refusal.
  - [x] Smaller ones, all settled 2026-11-06 (docs/ipc.md section 14): strings cut at a UTF-8
        boundary (gap 9), `opt` sentinels saturated (gap 10), lenient raw tags refused (gap 11),
        `SubscribeChannels` keeps its id (gap 12, kept), reads matched by an appended id
        (gap 14), trailing bytes left unchecked (gap 8, kept as the rule).
- [x] **4. Single owner of hardware blocks.** Done 2026-11-06 (dev 1ab6b5e): V4L2, subdev and
      media, RGA, VGLite, PiSP, the K230 and rkisp device layers and the sensor bridge are Lemnos's.
      `crates/rga` and `crates/vglite` came over from the graph runtime, and the generic parts of
      the camera stack's `crates/kernel` moved here. Every device path is untested on board; the
      consumers switch on the coordinator's go.
  - [x] V4L2, subdev and media: `lemnos-v4l2` (5,274 lines, no `unsafe`, 35 tests) over the sys
        uAPI layer. All of the camera stack's generic `crates/kernel` code moved except `bus`,
        `dma_heap` (already `lemnos-linux::dma`) and `v4l2::m2m`.
  - [x] Merge the two V4L2 layers. `lemnos-linux::v4l2::m2m` and its sys module predate
        `v4l2::device`, so `BufType`, `Memory`, `Rect` and `Fraction` existed twice. Done on
        `lemnos-r8-m2m`: the sys wrappers sit on `v4l2::device` (its copy of the uAPI is gone);
        `BufType`, `Memory` and `Rect` are each defined once, re-exported by `lemnos-v4l2`;
        `Fraction` had one. m2m's format types are `lemnos-v4l2`'s (`lemnos-linux` takes it
        under `v4l2-m2m`), converted at the sys calls in `v4l2/sys_format.rs`.
  - [x] PiSP: `lemnos-drivers-pisp` (the camera stack's `crates/pisp`, bb34279c): uAPI layouts, front and back end builders, tiling, statistics, and the `device` layer on `lemnos-v4l2` and `lemnos-linux` (no `unsafe`; the line timer's volatile accesses are in `lemnos-linux-sys::mmap`). Untested on board: the `device` layer since the move (the board results in the source are the earlier implementation's). `layout-check/run.sh` matches the Raspberry Pi 7.2 kernel tree (53679a578), the boards' kernel.
  - [x] Step 6, the sensor bridge (block-move step 6): `kernel-modules/lemnos-sensor-bridge` (GPL-2.0, out of tree) and the `lemnos-sensor-bridge` crate, from the camera stack's module and bridge client (d17de2ed, see `docs/provenance.md`). Renamed in one step: the module, its platform driver, the `lemnos,*` device-tree properties, the `LEMNOS_CID_*` controls (same ids), and `lemnos-bridge.service`. The module is syntax-checked only; the crate builds, clippy is clean and its host tests pass. Untested on board. The camera stack has not switched to these yet.
  - [x] RGA and VGLite from the graph runtime, on V4L2 M2M and direct kernel routes. Untested
        on board.
    - [x] RGA: `lemnos-drivers-rga` on the V4L2 M2M route, with `caps::route_for` naming the route
          each operation takes on each SoC. No `unsafe`, no dlopen, no consumer dev-dependencies;
          its device tests skip without `vim2m` or `rockchip-rga`, and this host has neither.
          Untested on board.
    - [x] RGA `/dev/rga` (the vendor multi-RGA node): `lemnos-drivers-rga`'s `MultiRga` covers
          fill, mask ROP, blend, palette, colour key, gray to RGBA, flips and turns. The request is
          from librga's Apache-2.0 `rga.h` (commit 331d5750), in `lemnos-linux-sys::rga`, with its
          layout pinned to the header. Untested on board; the field mappings the header does not
          state are marked unverified in the crate. Y-only output is not served.
    - [x] VGLite, phase 1: `lemnos-drivers-vglite` on `/dev/vg_lite`, with no vendor library and
          no dlopen. `lemnos-linux-sys::vg_lite` has the safe calls (open, initialize, allocate,
          map and unmap, a checked command stream run with its wait, and the identification
          registers). The clear, blit (resize, crop, flips and turns, blends), palette and context
          streams are ported from the vendor's `vg_lite.c` and checked word for word against its
          output. Untested on board.
    - [x] VGLite stroke (phase 2): `src/stroke.rs` ports `vg_lite_update_stroke` for open and
          closed polylines with BUTT and SQUARE caps and MITER and BEVEL joins, and the stroke draw
          inlines the outline; `line_bytes` and `line_dmabuf` draw width-1 square-capped lines,
          golden-tested word for word against the vendor. Round caps and joins, curves, dashes and
          the swing handling are not ported (refused).
    - [x] K230 and rkisp device layers: `lemnos-drivers-k230isp` and `lemnos-drivers-rkisp`, from the camera stack's `crates/k230isp` and `crates/rkisp` (d17de2ed). Moved: the uAPI layouts and bindgen headers, the K230 register map, `RegisterAccess` and the statistics register reads, `Isp`, `ParamBuffer`, `isp32::{Variant, nodes}`, and both `device` layers (on `lemnos-v4l2` and `lemnos-linux-sys::vvcam_isp`). Stayed in the camera stack: the builders and statistics math (`params.rs`, `stats.rs`, `isp32/params.rs`, `isp32/stats.rs`). Untested on board: both `device` layers (no K230D or Rockchip SoC on this PC), the ISP32 fixed-point scales, and the K230 register map's unconfirmed constants. The uAPI layouts are in `lemnos-rkisp-sys`, whose ISP32 bitfield accessors (bindgen output) and bitfield-unit `Pod` impls are the only unsafe code of the move. The camera stack has not switched to these crates yet.
- [x] **5. Kernel module and Rust user-space drivers.** Done in the leap, 2026-11-06: one module,
      `kernel-modules/lemnos` (see "The leap" below). One small module of our own, its sources in
      `kernel-modules/` (GPL-2.0, see its README): claim a block, DMA buffers and device addresses, dma-buf export, interrupt forwarding,
      register mapping. The drivers are Rust in user space. It is a fast path, never a requirement:
      stock V4L2 stays as the fallback, and other boards, the simulator and other kernels stay on
      stock interfaces. No patches on other people's code; our own modules are fine.
  - [x] The PiSP back end is the first block. First task: answer IOMMU coverage,
        interrupt-to-user-space latency, clocks, power and reset, and the GPL position of porting
        from stock drivers. Answered from the kernel sources in
        [docs/kernel-module.md](docs/kernel-module.md):
        - The IOMMU translates only its 40-44 GiB aperture and passes lower addresses (all of
          DRAM) straight through, so the module must write every address the block uses. No
          register mapping to user space for this block.
        - The clock is the firmware's ISP clock and is marked critical. There is no power domain
          and no reset reachable from Linux, so a stuck block still needs a reboot.
        - The module is GPL-2.0 and may port from the stock driver. User space uses only the
          uAPI headers (syscall note) and libpisp (BSD-2-Clause).
        - Latency is unmeasured.
  - [x] PiSP back end on the board (2026-11-06, board #1): `kernel-modules/lemnos-pisp-be` and
        `lemnos-drivers-pisp`'s `be-module` feature. Numbers in the module's README and
        [docs/kernel-module.md](docs/kernel-module.md) section 5:
        - interrupt to user-space wake-up p50 2 µs, p99 20 µs, max 76 µs under `SCHED_FIFO`
          (10000 jobs); the round trip matches `pispbe` (p50 511 against 505 µs) once mappings
          are kept between jobs.
        - Neither soft recovery (AXI rewrite, clock rate change) revives a stopped block: a
          reboot stays the recovery.
        - An IOMMU violation never shows: the block reads and writes the IOMMU's default page and
          finishes with status 0. The module never unmaps a buffer a job holds.
  - [x] The receiver, the sensor over i2c-dev, and a mock backend for the simulator
        (2026-11-06):
        - `kernel-modules/lemnos-rp1-csi2` and `lemnos-drivers-pisp`'s `csi2-module` feature:
          `Csi2Module`, `cci::{I2cCci, RegisterFile}`, and the `Receiver` trait with
          `MockReceiver` and `MockCci` for a simulator.
        - Board #1, OV9782 at 1280x800 RAW10 over 2 lanes at 800 Mbit/s: 3600 frames at
          60.28 fps, 0 failed, 0 dropped; frame start to frame end median 7465 µs. The colour
          bars were exact. `rp1-cfe` and the camera service were restored after each run.
        Decided:
        - The module only receives. It never touches the sensor, its supply or its clock; user
          space owns them (the register file, a GPIO line).
        - Lemnos ships no register tables: the sensor's register file is read at run time from
          a path the caller gives.
        - The mock covers the receiver and the sensor bus. A mock of the back end module waits
          for a simulator that needs one: the stock `pispbe` path has no mock either.
        Untested on board: other lanes, rates, data types and virtual channels, and `PACK_BYTES`.
  - [x] Reworded "Lemnos is user-space hardware access" in [docs/dma.md](docs/dma.md) and "no
        kernel modules" in [docs/system-service.md](docs/system-service.md) for the module.
- [x] **6. GPU** (closed 2026-11-06: the lease is done, the rest closed or dropped below), per the agreed seam note (kept outside this repository, because it names the
      products). Mesa stays, and `lemnosd` coordinates rather than enforces: an advisory lease
      first, `lemnos-gpu` only when a consumer needs it.
  - [x] The advisory lease (2026-11-06): `lemnosd` keeps one per render node (named by driver,
        `v3d`): exclusive or shared, waiters by priority then arrival, a yield request to a
        lower-priority holder, an overdue notice past the holder's `max_hold_ms`, hang reports in
        the state, leases ending with their connection. `lemnos_ipc::GpuClient`,
        `lemnos-ctl gpu [status]` and `lemnos-ctl gpu run -- <command>`.
        [docs/ipc.md](docs/ipc.md) section 6.6, [docs/system-service.md](docs/system-service.md).
        Board #1: the v3d node found, runs under the lease ordered as expected.
        Decided:
        - Additive to protocol 1: request kinds 18, 19, 31 and 32 and messages 113 and 114, sent
          only after the welcome's new `features` field (appended; absent is 0) says the service
          has them, and `GpuLease` only to a client that asked. Existing clients' types are
          unchanged (`Update`, `Event`, `Refusal`, `ClientEvent` gain nothing).
        - A separate `GpuClient` connection that never reconnects: a lost connection has lost
          its leases.
        - Leases end with the connection whatever `keep` says: a lease nobody can release would
          hold the GPU until the service restarts.
        - Not overtaking: the first waiter is granted first, so a stream of shared leases cannot
          starve an exclusive waiter of the same priority.
  - [x] Closed 2026-11-06: switching the board runs from the `flock` lock file to
        `lemnos-ctl gpu run` is the consumers' call, not Lemnos work; the lease is shipped.
  - [x] Dropped 2026-11-06: `lemnos-gpu`. No consumer without wgpu has asked for it; it comes
        back as a new item if one does.
- [ ] **7. Shared and static builds.** Both modes, with core, surface and wrapper for each
      dependency. The workspace's LTO test informs how shared gets built.
  - [x] Decided 2026-11-06, for the image's surface checks: the feature is `shared` on each
        surface crate (first `lemnos-capi`). It makes the Rust wrapper link to the installed
        library (`liblemnos_capi.so.1`, by the normal dynamic linker, never dlopen) instead of
        the crate's own exports. Core: `lemnos-abi` and `lemnos-ipc`, no process-wide state.
        Surface: the exports and their handle table, the only process-wide state. Wrapper:
        `Client`. A process holds each surface once: statically or loaded, never both.
  - [x] `lemnos-capi/lemnos.exports` (the surface list, checked against the header and the
        exports) and the SONAME `liblemnos_capi.so.<major>`.
  - [x] lemnos-capi's `shared` feature (2026-11-06): `Client` links `liblemnos_capi.so.1` from
        `LEMNOS_CAPI_LIB_DIR` (or one the build script builds, for development), and the crate's
        own exports are left out. The tests, the C program's included, run both ways.
  - [ ] Leap, started 2026-11-06: the static and shared forms finished for `lemnos-capi`, with
        `lemnos-abi` as the shared vocabulary (see "The leap" below). Other surfaces follow the
        same pattern as each gets one.
- [x] **Consumer compatibility of items 5-7 and the IPC gaps** (decided 2026-11-06, before the
      consumers switch). Each stays additive to what the consumers switch to now:
  - Item 5: the module path is a second, opt-in backend of lemnos-drivers-pisp's device layer.
    Stock V4L2 stays the default and its constructors, types and behaviour do not change.
  - Item 6: the lease is advisory. RGA, VGLite and V4L2 users make no new calls, and an
    unleased user keeps working; lemnos-gpu stays a new crate.
  - Item 7: no crate or feature is renamed or split. lemnos-capi keeps `rlib`, `staticlib` and
    `cdylib`, and `Client` keeps calling the exports directly, so static linking is unchanged.
    The shared-form loader is a new opt-in feature. Any per-dependency core or surface is a new
    crate beside the existing ones.
  - IPC: protocol 1 stays byte for byte. Done the same day: gaps 11, 15 and 16 are refused or
    ignored only for frames the reference client never sends, gap 9 cuts at a UTF-8 boundary with
    `encode`'s signature kept, and gap 17 sets the socket to 0660. Gaps 8 and 12 are kept as
    intended (appended fields rely on unchecked tails). Gaps 10 (the opt sentinels) and 14 (no read
    id) were set aside for a version 2.
  - Protocol 2 design (2026-11-06, `docs/ipc-v2.md`): neither gap needs a version. Both are fixed
    within protocol 1's own rules. The encoder saturates `Some(sentinel)` one below it, since every
    optional field is thousandths or milliseconds. `Read` and its answering `Reading` carry an
    appended id, and `FEATURE_READ_ID` (welcome bit 1) tells the client to match by it. No byte
    changes for an older peer. The file keeps the rule for when a version is due, the negotiation
    (`Hello` frozen; the service answers the client's version per connection), and what waits.
- [x] **RVV for the K230's CPU fallbacks** (2026-11-06). Lemnos had no CPU code for what a route
      leaves to the CPU. `lemnos-drivers-rga::cpu` now has four operations: channel to gray, gray to
      RGBA, colour key and mask ROP. Each is a scalar loop, with RVV 1.0 rows from
      `lemnos_linux_sys::rvv` when the kernel offers V. They match byte for byte under qemu at VLEN
      128/256/512 (`scripts/rvv-qemu.sh`, in the riscv64 push gate). Untested on board; no K230
      profile exists, so the four were picked as the per-pixel operations VGLite does not serve.
  - [x] Conversions and resizes on the CPU route (2026-11-06): `cpu::convert` with the three
        matrices the units offer and `cpu::resize` (area, bilinear with each unit's documented
        phase), scalar and RVV, byte-identical under qemu at three VLENs. Untested on board.
  - [x] Parked 2026-11-06, written, untested on board: measuring each unit against the CPU
        route's definitions (to fill `caps`' `Exactness`) needs the RGA and VGLite boards, and
        the RVV rows need a K230 (`LEMNOS_EXPECT_VECTOR=1`, then time against the scalar
        loops). Both are board runs, picked up when such a board is on the bench.
- [x] **8. `a_polled_400_hz_imu_delivers_each_sample_once` was intermittent.** Fixed: its rate floor
      is now 90 % of 400 for the time the service's threads had a CPU (their run-queue delay in
      `/proc/self/task/*/schedstat`), not a share set by a 10 ms wakeup probe's p99. Measured on two
      cores with 0-16 busy loops: a plain thread sleeping on the same 2.5 ms grid hit 99.8-100 % of
      it, but the service's loop thread waited 60-74 % of the time for a CPU (89 preemptions in
      2.5 s, about 54 us of work per wakeup in a debug build) and delivered 156-335 a second. 12 of
      12 runs pass at 0-16 busy loops; a 5 ms schedule (200 a second) still fails with no load
      (floor 358).
  - [x] A second failure under load: one delivered reading repeated the last sample. The repeat
        filter measured a period from the start of the last delivered read; a read preempted between
        its timestamp and its transaction took a later sample, which the next read found again a
        period after that start. Now measured from the end of the read.
  - [x] Reopened 2026-11-06, fixed the same day: one failure in the workspace gate at cb5a790, with the host's load
        average at 39-46 on 32 cores (other builds): 294 readings a second against a floor of 308,
        with the service's threads waiting only 14 % of the time for a CPU. So the rate floor's
        wait accounting misses part of the loss under that load. The test passed 5 of 5 alone
        straight after. Find where the remaining loss goes (wakeup latency not seen as run-queue
        delay, or a thread the floor does not count) before calling the test stable.
        Found: the polled period was the output period itself (2.5 ms), not the 90 % its doc
        promised, so the read grid kept one phase to the sample grid. Near the step, timer
        jitter made a read early (a miss) and the next a sample late (a skip) for the whole
        run, which no CPU wait shows. A/B under 24 busy loops, runs interleaved: at 2.5 ms,
        half the runs fell to 334-338 a second at 15-21 % waited (about 200 misses, 190
        skips); at 2.25 ms none did (380-387 at 13-16 % waited, skips tracking the wait). The
        cost is about 11 % more reads that find no new sample. The test's failure message now
        counts skipped samples and misses.
        On a board (2026-11-06, board #1, BMI088 on the RP1 PIO I2C bus, accelerometer at
        200 Hz, the image's other services running; the release service build before and at
        the fix, two interleaved rounds each, 45 s a window): no difference. The board entry's
        `poll_ms = 5` caps the read period at 5 ms, above the 4.5 ms floor, so even a 1 ms
        subscriber is read 200 times a second by both builds. lemnosd's CPU: 0.7-0.8 % idle,
        3.1-3.2 % at 200 Hz (5 ms and 1 ms subscribers), 1.4-1.5 % at 50 Hz (20 ms, the
        recorder's rate), both builds. A read takes 555-576 µs on the bus. At 200 Hz, 9000 or
        9001 of 9000 readings delivered, no misses, largest gap 5.1-5.2 ms; at 50 Hz, 2250 or
        2251 of 2250, at most one miss (one 40 ms gap), in both builds. Repeats (the same
        accelerometer triple delivered twice in a row; polled reads carry no sensor time):
        0-5 of 9000 at 200 Hz in both, none at 50 Hz. So no cheaper early read is needed here.
  - [x] The 100 Hz tests in the same file failed the same way (one 100 ms gap against a fixed
        25 ms bound, at load 93). Every rate floor there is now 95 % of nominal for the time the
        service's threads had a CPU, and the largest gap may pass 25 ms only by the time they
        waited for one. The p99 spacing bound is unchanged (it is what catches a slow read
        delaying the IMU). The whole binary passes 8 of 8 at 0-16 busy loops on two cores.
  - [x] Under CPU contention lemnosd's loop was preempted and lost samples. Measured on board #2
        with the image's release build: 0 missed with only the 30 fps pipeline running, 20 to 43
        of 12 000 with four busy loops, 33 with `Nice=-10`, and 0 with `SCHED_FIFO` priority 10.
        The packaged unit now sets `CPUSchedulingPolicy=fifo`. Rerun beside a 120 fps pipeline
        (board #1, 2026-11-06): 12 000 of 12 000 delivered, one 9.9 ms gap with 0 ms of CPU wait
        (a read that found no new sample), and none with 4 busy loops added
        (`docs/system-service.md`, "Beside a 120 fps pipeline").
  - [x] The p99 spacing bound was the one left fixed. It failed once in a gate run at load 120
        (p99 11.5 ms against 8.4 ms, with the service waiting 0.86 s of 3 s for a CPU), and it
        passed 6 of 6 on rerun. It now adds a hundredth of the service's CPU wait, which is nothing
        on an idle host.

- [x] **9. `serves_the_simulated_readings_over_the_socket` failed once** (2026-11-05, gate run at
      load 70): `acceleration.z: no value` on a one-shot `read("imu")` right after start. Cause, as
      the lead suspected: a one-shot reader that arrived while a subscription's channel-masked read
      was in flight was answered from that read's partial row. Fixed (2026-11-06): waiters are
      answered only from a read of every channel. Otherwise they stay queued, and the next dispatch
      gives them a full read first. `a_one_shot_read_during_a_masked_subscription_read_gets_every_channel`
      (tests/subscribe.rs) fails at read 16 without the fix and passes with it.

## The leap (go from the user, 2026-11-06)

Lemnos goes first, since the consumers build on it. The joint-round lock (80d5d1e) stays stable
for consumers: everything below is additive until they switch.

- [x] **A. One generic kernel module** (done 2026-11-06, board #1). The generic core passed
      20 of 20 on the PiSP back end's node. The personalities matched the old modules in four
      interleaved 120 fps chain runs (record in `kernel-modules/lemnos/README.md`), so the old
      modules are removed. Decided:
      - The generic driver claims only the compatibles in its `owned` parameter.
      - Interrupts are not shared, and start masked.
      - `lemnos-linux-sys::block` is already a safe API, so no second wrapper in `lemnos-linux`.
      - Size: `lemnos.ko` is 58 264 B unstripped, against 50 432 B for the two modules it
        replaces (the generic core).

      Both paths left untested were then proven on board #1 (2026-11-06) with test-only load
      parameters:
      - registers mapped read-only (the back end's version, 0x02252701);
      - interrupts injected at the GIC through the whole forwarding path: 2000 of 2000, user
        space woken p99 3.3-3.4 µs after the injection.

      A level interrupt that fires while masked is not resent on enable. That is genirq's rule,
      since the device's line is still held. The installed module is stripped by default:
      59 416 B shipped (the tree has no debug information).
      The plan as written: Item 5's
      per-block modules become one:
      - A core that claims any platform device bound through `driver_override` and gives it
        `/dev/lemnos-<name>`. It reports the device's register regions and interrupts, allocates
        DMA buffers and exports them as dma-bufs, imports dma-bufs to device addresses held per
        open file, and forwards interrupts as timestamped reads with `poll`.
      - Register mapping to user space only for a block whose DMA cannot reach memory the module
        did not map (behind a confining IOMMU, or no DMA). The PiSP back end and the CSI-2
        receiver are neither (docs/kernel-module.md), so their jobs keep being written by the
        module.
      - Those two become personalities inside the same module, with their ioctl uAPIs kept byte
        for byte, so `be-module` and `csi2-module` work unchanged. The two old modules go once
        board #1 passes the same runs.
      - User side: `lemnos-linux-sys::block` (the uAPI) and a safe `lemnos-linux::block`.
- [x] **B. Item 7, the static and shared forms of `lemnos-capi`** (2026-11-06). Done: pkg-config
      files for both forms, `scripts/install-capi.sh`, the second-copy refusal (`-EBUSY` at
      `lemnos_client_open`), and both link modes in the gate (the all-features run covers
      `shared`). Decided: no symbol version node. rustc's anonymous version script for a
      `cdylib` overrides a named one, so the SONAME major and the handshake carry the version.
      The original list:
      - a symbol version node (`LEMNOS_1.0`);
      - pkg-config files for both forms;
      - an install layout;
      - a check that refuses a second copy of the surface in one process;
      - the C test linked statically and dynamically in the gate.
- [x] **C. `lemnos-abi` finalized as the shared vocabulary** (2026-11-06; abi 1.0, layout hash
      `0xa02cfb4a8dd4545b` asserted at compile time, rules in the crate's README):
      - handles, status, slabs, buffers and fences in a header of their own
        (`include/lemnos/abi.h`) that every surface includes, generated from the crate and
        checked against its layout hash;
      - the stability rules written down;
      - abi 1.0 frozen.

## Round 4 (the user's list, 2026-11-03)

- [x] **Orion protocol 1** (351b7eb, 00aa534): the bridge pinned to Orion a89ebd5. Live on #1
      (2026-11-04, image dev.912): 9 devices published as `lemnos.device` resources, all healthy;
      `read` matches `lemnos-ctl`, `calibration.status`, `set` and `restore` succeed, a bad control
      is rejected.
- [x] **Absent look on a light** (2026-11-07, asked through the OS image's coordinator: the
      process that held a status look at priority 20 while its API was down is gone). Board
      keys `absent_look` and `absent_priority` (default 20). lemnosd holds the look as its own
      application intent at that priority for as long as it runs. Decisions:
      - A held intent at a priority, not "while no client of the status layer is
        connected": the OS image's writers send named looks, which are application intents,
        so a priority is what they already arbitrate by, and it reproduces the removed
        process exactly.
      - Default 20: under a client's default of 50.
      Tested in the mock (`tests/absent_look.rs`); untested on board.
- [x] **Polled `DeviceClient`** (2026-11-07, asked by the coordinator, for board-agent to drop its
      keeper thread): `poll`, `send_read`/`send_set`/`send_get`, `fd`, feature `tokio`
      (`AsyncDeviceClient`). 1000 polled read round trips allocate nothing; tested against a
      fake service, not yet in board-agent. Decisions:
      - The first `poll` returns `Connected { reconnects: 0 }`, as `next_event` does.
      - Reconnection happens inside `poll` (its handshake blocks up to the client's timeout).
        The socket is new, so `Connected` means: register `fd()` again.
      - Size: lemnos-ctl's text grew 3.7 KB (x86_64 service profile), lemnosd's 272 bytes.
        About 1.9 KB of that is inlined into lemnos-ctl's `main` and `led`; the mode switch
        is out of line. Not trimmed further: boxing the polled state (two buffers, two flags)
        would shrink `Connection`'s drop and construction code, but by an unmeasured amount.
- [ ] **Gyroscope aligned to the accelerometer** (2026-11-07, asked by the coordinator: no
      duplicates, misalignment well under 0.1 ms, measured before and after):
      - **Done:** the gyroscope is resampled at the accelerometer's sample times, with each
        die's clock taken from its timed FIFO counts (`align.rs`). On board #1, against an
        offline fit: before, p50 1.8 ms and p99 7.4 ms, with 1.6 % of gyroscope values
        repeated. After, p50 38-72 µs and p99 231-436 µs, with no repeats.
      - **Not met: under 0.1 ms.** Count timing alone gives no better. A read bounds a sample
        only when it lands just after one, and the counts sometimes show a sample late (so only
        upper bounds hold). The offline fit itself moves by about 0.1 ms at p99 between window
        lengths.
      - **Ruled by the coordinator (2026-11-07):** p99 of about 0.1-0.2 ms is the accepted floor
        on this hardware, and the gyroscope stays hull-resampled as is.
      - **Done (2026-11-07):** the accelerometer's side from its sensor time (`TickClock` in
        `align.rs`, a 3-byte read per batch). On board #1, loaded and idle: an accelerometer
        sample's time is off by 31-38 µs at p50 and 73-84 µs at p99 (from counts: 310-372 µs
        at p99). The gyroscope's time at it is off by 34-39 µs at p50 and 90-107 µs at p99.
        The p50 is almost all a constant 31-38 µs offset against the offline fit, cause not
        traced. The gyroscope's side stays on the count hull, as ruled.
- [x] **The extra allocation in the light test** (2026-11-07, asked by the coordinator to
      find, not leave):
      - **Found** with `where_look_requests_allocate`, which records backtraces: 4001 service
        allocations over 4000 looks. 4000 are the look names' decoding. The other is
        `status_event` from `fan_publish`, the fan controller's first change from degraded (no
        input sample yet) to available. It copied the device id into an owned event, and on a
        loaded host it landed in the measured window.
      - **Fixed:** status events are encoded from the slot into reused frames. A degraded fan's
        reason is replaced only when it changes (it allocated every tick while degraded). From
        start, the only fan allocation left is the reason's first growth.
      - **Still open:** `alloc_board`'s 21 allocations at load average about 114 were not
        reproduced again. Starvation can make the fan's input stale and flip it, which is the
        same path, but that is not verified.
- [x] **Batched IMU: no lost samples, stamps on the device's clock** (2026-11-07, asked by the
      coordinator after the board check of the stamp fix):
      - **Cause.** The 0.4 % lost samples were single samples, with or without load: 79 of 20 078
        unloaded, 294 of 72 293 loaded. A per-read trace on #1 found no skip frames, no partial
        frames and no FIFO overflow. The two dies' FIFOs filled at 403.9 and 399.6 Hz, and the
        driver paired them one to one, returning only as many samples as the scarcer FIFO held.
      - **Driver.** Every accelerometer sample is returned. The gyroscope is paired from the
        newest end, and its oldest sample is repeated when it holds fewer.
      - **Stamps.** lemnosd measures the period per batched device: the slope through the latest
        read of each second, against the sample count, over 4 s. On #1 under 4-core load:
        403.8 Hz, 0 lost, 0 back, 71 964 of 71 999 steps within 1 µs, after 10 s none beyond
        81 µs.
      - **Decisions.**
        - The latest read, not the earliest: a preempted read only looks early, and the
          earliest read was 0.02 % off under load.
        - A 4 s window: the accelerometer's period moved 650 ppm over 90 s under load. In a
          replay of that run's reads, 16 s gave 95 nudged stamps and 4 s gave 2.
        - No phase band around the fitted line: it is biased by the time to the length read,
          and the replay got worse (about 40 nudges).
- [x] **Timed fan hold for Orion callers** (2026-11-07, asked by the OS image's API for its
      fan override): `set` takes `for_ms` (1 to 600000); at expiry the bridge releases a fan to its
      governor (restores any other control). A renewing `set` restarts the hold; the caller's
      `restore` or `release` ends it. Decision: an Orion action has no connection that outlives it,
      so "release when the caller goes" is a short `for_ms` the caller renews (docs/orion.md,
      *Timed holds*). Tested against an in-process node and the mock fan; untested on board.
- [x] **15. Composites** (6d66b53, 7aac6a3): the `Composite` trait in `lemnos-device`; the fan
      controller is a device slot listed by `lemnos-ctl list` with status, reason, channels and
      controls; client tuning persists in `<state>/fan/<id>.tuning`; the watchdog is pinged only
      while every enabled controller has stepped lately. #1 fan: listing, live tuning and
      persistence tested, then restored exactly.
- [x] **16. The BMI088 in the validator** (e28ae66): the bus targets take a `pio-i2c:sda=N,scl=N`
      selector (facade feature `rp1`). Passes on #1.
- [x] **17. String-keyed maps and probe sharing** (37065b5, 8c1cec0, 1e71ab7, ba46fb7, 153a405,
      ccafd32, 4b2d0e5, c7174aa): `SmallMap<MapKey, V>` (sorted vector, `Cow<'static, str>` keys)
      replaces the `BTreeMap`s in descriptors and runtime bookkeeping (an API break, serde shape
      unchanged); the probes share their descriptor builders. linux-lite 560 928 -> 511 664 B
      (-8.8 %), linux-lite-builtin 720 584 -> 662 032 B; lemnosd +17 KB from the composite work.
- [x] **21. Pending fence** (0735212, #2): 1024 TFU copy jobs (4 MiB), the out-syncobj's sync
      file reads pending straight after the submits and signals 4.9-5.0 ms later; all bytes match
      in five runs. The TFU `icfg` (32) and one-row-per-job shape came from a sweep.
- [x] **21. HEVC decode** (9f0c0d8, 0ce8289, 78222b0, #1 rpi-hevc-dec): `lemnos-hevc`, a no_std
      parser (IDR path only), and the stateless HEVC controls (`v4l2-hevc`). One 320x240 IDR
      decodes and matches the host VA-API decode byte for byte (3 runs); request run 181-192 us.
- [x] **22. Raw RP1 DMA dropped** (the user, 2026-11-03): the `rp1-dma` engine, its register
      layout and the planned PCIe DMA engine are removed. The ruling covers this memory-copy engine
      only.

## Round 3 (the user's ruling, 2026-11-03)

Every item below was ruled on by the user. Shas are on `dev`; "host" means this workstation.

- [x] **3. USB layer on a real device, both boards, against the webcam.** Listing, descriptors,
      claims, control (UVC probe, GET_LEN), interrupt, isochronous UVC streams in both reaping
      modes, usbdevfs mappings (`CAP_MMAP` on both), `next_block_until`, `driver()`/`speed()`,
      reattach. #1: 29.4 fps of 30, 0 packet errors; 60 s run 1800 frames, 473 664 packets, 0
      errors, ~1 % CPU, ~485 xHCI IRQ/s. #2: 24.5 fps of 25, 0 errors, monitored (throttled 0x0).
      Fixes: claim check before the kernel, release through `USBDEVFS_IOCTL`, usbfs memory cap with
      heap fallback, the reaper fails a hung-up descriptor instead of spinning (7a96740..a4512c9,
      f41ac9c..92e21a7). Reattach: a release does not rebind the UVC streaming interface; rebinding
      uvcvideo on the control interface does. `docs/usb.md`.
- [x] **4. lemnosd under real load** (#1): FIFO at 400 Hz, a full-rate subscriber, camera stack
      running: 23 872 of 24 000 readings, wakeup p50 55 / p99 79 / max 446 us, 1.72 % of a core;
      receive age p50 11.0 / p99 20.8 ms (batched by design) (399b41e).
- [x] **8. IO blocks as chip support** (untested on board): `lemnos-soc` (no_std data crate) with
      the K230D, RK3588S, RK3576, RK3566, RK3562 and RV1106 blocks marked used / unused / impossible,
      and register layouts for the Rockchip PWM v3, timer and the CLINT; per-chip tables in
      `docs/io-blocks.md` (6d839e4, 700348b). Source: mainline device trees and drivers.
- [x] **10. IMU FIFO vs polled on the PIO bus, matched rates** (#1), lemnosd CPU and timing:
      200 Hz polled 2.55 %, 0 gaps, age p50 0.56 ms; 200 Hz FIFO 1.40 %, 66 gaps, age p50 7.2 ms;
      400 Hz FIFO 1.72 %, 128 gaps, age p50 11.0 ms; 400 Hz polled: see the clamp below.
      FIFO trades latency and gaps for CPU (a 48-frame drain takes ~16 ms on the PIO bus).
- [x] **11. Scheduler with real buses** (#1): per-slot read cost over IPC (`Request::Stats`,
      `lemnos-ctl stats`, 191c646). IMU 100 Hz beside the INA238 and BMM150: 6001 readings, grid
      deviation mean 16.9 / p99 118 / max 614 us, 1.62 % of a core; read means IMU 563 us,
      magnetometer 1082 us, power monitor 2543 us.
- [x] **12. A mock FIFO that drains** (bd9543d): `lemnos-hal` mock `with_fifo` /
      `with_fifo_stream`; the batched-IMU test asserts strict stamp order and a 25 ms gap bound.
- [x] **13. IIO buffered reads** (2e79517, untested on board: neither board has IIO):
      `IioBuffer` (`KernelDevice::buffered`), scan elements, trigger, the timestamp channel on
      CLOCK_BOOTTIME, poll-based reads; host-tested on a fake sysfs tree.
- [x] **14. Phase 5 rich-model slimming:** `Arc` in inventory events (cacdb6c), retained-event
      byte accounting behind `retention-bytes` (9e22a4f), a shared absent-root probe helper
      (05d85e1). With item 17: linux-lite 574 480 -> 562 704 B, linux-lite-builtin -11 744 B,
      facade -1 792 B.
- [x] **15. Composite devices: the fan controller** (148bcd1, 3c76a2c, 50533a8, slimmed in 77089e3):
      `lemnos_device::fan_control` (no_std), a `[[composites]]` board entry, hosted by lemnosd.
      #1 real fan: two curves settled at pwm1 111 and 189; restored exactly (pwm1 179,
      pwm1_enable 1, cooling state 0). Cost: lemnosd ~+16 KB, lemnos-ctl ~+9.8 KB.
      `docs/composite-devices.md`.
- [x] **16. Board validator on the sensor crates** (0e00029, 1e04b49, 1ad7a6e): #2 BMM150 and
      INA238 pass (4.253 V, 2.953 W, 38.6 °C). The BMI088 is skipped: the validator opens `i2c-N`
      only and the IMU is on the PIO bus.
- [x] **17. Slimmer small-Linux runtime:** device-keyed runtime state in a sorted vector
      (baa0076); the nightly `-Z build-std` recipe documented (f30d2fd, `docs/compact-model.md`,
      *Constrained images*): empty main 278 856 -> 12 232 B, sensors 319 056 -> 43 712 B,
      linux-lite 562 704 -> 232 608 B.
- [x] **18. `SetDutyCycle` on sysfs PWM** (5341463): `lemnos_linux::hal::SysfsPwm`, feature `pwm`.
      Untested on board.
- [x] **19. Memory of a refresh with eager binds** (#1, `bringup_memory`, cd15641): PSS start
      616 kB, facade 1212 kB, refresh without binds ~1892 kB, with eager binds 2136 kB (+248 kB;
      36 tried, 20 bound). Refresh 2 ms without binds, 110 ms with. The camera's I2C bus
      (`linux.i2c.bus.10`) is excluded; the USB power lines are not discovered without the board
      file.
- [x] **20. Accelerator transport, USB only:** a generic DFU loader (`UsbDevice::dfu_download`,
      `wait_for_device`), blocks of the device's `wTransferSize`. Coral 1a6e:089a -> 18d1:9302 on
      both boards: 43 blocks of 256 B, download and reset 1.22 / 1.23 s, re-enumerated after
      1.52 / 1.53 s. The first attempt on #1 used 4096-byte blocks and wedged the device until a
      port power cycle.
- [x] **21. USB and DMA on hardware.** Coral bulk endpoints after the load (both boards):
      `UsbBulkEngine` claim, zero-length OUT 56-147 us, IN timeouts cancel cleanly. DMA heaps on #2
      (1 MiB: system 60 us, CMA 162-165 us; CMA pool 64 MiB), now capped before the kernel
      (019d090). Fences: a signalled v3d syncobj exports and imports. I2C engine: BMM150 chip id in
      435 us (#2). SPI: zero-length only. M2M: stateless request API (bb4eb7e, e24e50a), host
      decode through vicodec; on #1 rpi-hevc-dec takes a request, a control and an OUTPUT buffer.
- [x] **24. Isochronous packet tables reused:** the pool was already in c546b44; a host test now
      proves steady-state reuse.
- [x] **26. Product names out of Lemnos** (569ad29, 47b3b07): look writers are a board-file
      `look_writers` list (default `lemnos-ctl`, `orion:*`); the lemnosd fixture is a generic
      `sample-board.toml`; the unit's ordering is a drop-in example. Their allowlist entries are
      gone.
- [x] **Scope overlaps decided** (user, 2026-11-03; `docs/scope-overlaps.md`):
      1B, the named looks move out (c87819a, a1414a7, 97d9d03, ef4ef15, 7fdfb1d): built-ins are
      `status.*` only, the rest are example look files and presets under `examples/looks/`, boards
      get `default_look`, and the 21 retired ws2812 look keys are refused with an error naming
      where each now lives. 2A: `lemnos-fusion` stays. 3A: the client queue stays in lemnosd; its
      drop classes are documented in `lemnos-ipc`.
- [x] **Polled IMU never faster than the device, never a repeated sample** (found in item 10:
      a 2 ms poll read a 400 Hz device at 500 Hz; e9ab879): lemnosd clamps a polled
      sensor to its output period (from the board config) and skips a reading equal to the last
      one within that period. #1 at 400 Hz: 399.90 Hz, 6 gaps, grid p99 64 us, 4.41 % of a core,
      receive age p50 559 us. Firmware images unchanged.

### Decisions (made during the round)

- 2026-11-06, the camera stack's request: `FrontEndDevice::add_image_buffers` creates raw
  buffers while streaming and leaves them unqueued for the caller. They are only freed by closing
  the capture: rp1-cfe implements `VIDIOC_CREATE_BUFS` but not `VIDIOC_REMOVE_BUFS`, and
  `VIDIOC_REQBUFS(0)` needs the queue stopped. Untested on board.
- 2026-11-06, the camera stack's request: `FrontEndSetup::image_compressed: bool` rather than a format enum, since rp1-cfe offers one compressed mode on fe_image0 (`PISP_COMP1_*`, depth 8, the sink's 16-bit media bus code). Lemnos sets the node format and `image_format()`; the compression offset and mode stay with the caller, who sets the front and back end configs. Untested on board.
- The validator drops the BMI055 (no crate support, no board carries one).
- INA238 range: 16.38 A at 10 mOhm (16.384 A is exactly the 163.84 mV limit and is refused).
- Failure records stay on (they feed `DeviceStatus::Faulted`); `failures()` stays a `BTreeMap`.
- `Arc` in `InventoryEvent`, `InventoryDiff` and `ChangedDevice` is an API break; the serde shape
  is unchanged.
- `retention-bytes` is on by default; with it off only the event-count limit applies.
- The fan controller is a concrete type in a `[[composites]]` entry that reuses the device table
  shape (roles in `config`); it is not a device and is not listed by `lemnos-ctl`.
- lemnosd hands a fan back after a failed write and waits 10 s.
- Stats are counted per read on the slot and read on request (three integer updates per read).
- The mock FIFO fills on the device clock, not from a feeder thread.
- IIO: boottime clock (monotonic converted per record, others refused); a device without a
  timestamp element is refused; a running buffer is `Busy`.
- DMA heaps: CMA requests are capped by `CmaFree`, system by a quarter of `MemAvailable`;
  `DmaHeap::with_capacity` sets a lower cap.
- USB: transfers on an unclaimed interface are refused before the kernel; driver rebind after a
  release goes through sysfs, for UVC through the control interface.
- USB port power (the user, 2026-11-03): a wedged USB device is recovered by cycling its port's
  `gpio-power-switch` through lemnosd. Both boards: `usb-c-power` (RP1 GPIO16) feeds the Coral
  (bus 1 port 1), `usb-a-power` (RP1 GPIO20) the webcam (bus 3 port 1). Cycles this round: #1
  usb-c once (Coral recovery) and usb-a once (mapping); #2 each once (mapping).
- Polled sensors: repeats are filtered by value within one output period (a still device is
  still delivered once per period). A sensor-clock index (BMI088 SENSORTIME) for polled reads
  was tried and dropped: its rate did not match the samples. (It runs 1.1 % fast with the
  accelerometer's die; FIFO batches now use it to place the accelerometer's samples.)
- Looks: the retired ws2812 look keys are refused, not ignored (the user: nothing unreleased gets
  compat). A light with no owner, or a look name nothing defines, shows `default_look`.
- `finish_refresh` stays at about 15 KB: split into its steps it showed no duplicated code.
- HEVC: frame-based decode mode (the only one the driver takes); the driver's NV12 is in 128-byte
  columns; `bit_size` covers the slice data from `data_byte_offset`, not the whole buffer.

## Done earlier

The compact model (phases 1-4: `lemnos-device`, `lemnos-lite`, the generic L3 adapter, board
definitions, `lemnosd`, `lemnos-ipc`, `lemnos-light`, `lemnos-drivers-ws2812`); the foundation
(`lemnos-hal`, `lemnos-linux-sys`, no_std sensor drivers, regulators and clocks, GPIO character
devices); lemnosd on the BCM2712 board; the IMU on the RP1 PIO I2C bus (100 Hz 1.9 %, 200 Hz 2.9 %,
400 Hz 6.0 % of a core on #1); riscv64 and armv7 builds; DMA buffers, the transfer interface and
V4L2 M2M sessions; `AsyncGpioLine` (host-tested, untested on board: no line gives real edges
without rewiring); one USB layer (`lemnos-usb`) and one hotplug watcher (`lemnos-uevent`); the
`tokio` facade feature kept (user, 2026-10-30). Details are in [CHANGELOG.md](CHANGELOG.md).

Consumer-side work (apps and the camera stack) is tracked in those repositories.
