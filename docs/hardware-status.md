# Hardware status

**Most of this repository has not been run on hardware in its present form.** The drivers, the
backends and the kernel module were written and tested against simulations, mocks and models of
the kernel's behaviour, and each has a written check waiting under `testing/board-runs/`.
Of those files, the ones listed under [The first board run](#the-first-board-run) record what
two runs on the reference board showed; everything else is still *untested on board*.

This page lists what a board still has to show. [Testing](testing.md#board-runs) says how to do a
run and where its result goes.

```sh
ls testing/board-runs/*.md | wc -l
grep -Li 'untested on board' testing/board-runs/*.md | wc -l
```

The first number is how many checks are written. The second is how many no longer say
`untested on board`.

## The first board run

On 2026-11-11, on the reference board (a Raspberry Pi Compute Module 5, kernel `7.2.9-v8-16k` then;
its kernel is now `rpi-7.3.y` `b005af66` with 4 KiB pages, and these checks have not been run again on it),
with the board's own software stopped only for each test and put back afterwards:

| Check | Result |
|---|---|
| [`C1.md`](../testing/board-runs/C1.md) | The module core on the PiSP back end, behind its IOMMU: the check tools pass, interrupt to handler 0.5 µs, to user space 1.5 µs (p99) |
| [`C3.md`](../testing/board-runs/C3.md), [`C4.md`](../testing/board-runs/C4.md), [`C8.md`](../testing/board-runs/C8.md) | Register reads, a free input and a PWM channel (output off) on both routes: same data, 0 allocations; I2C the same time on both, PWM 6 times faster through the module. No SPI device, nothing driven |
| [`C5.md`](../testing/board-runs/C5.md) | PIO in board mode, including DMA; a killed holder took a minute to die, now at once. The IMU on PIO I2C both ways: same data, same bus time, 0 allocations; two bus faults on the module route fixed |
| [`C6.md`](../testing/board-runs/C6.md) | A block's clock, thermal reads and watches, bind and unbind events |
| [`C7.md`](../testing/board-runs/C7.md) | The daemon on the module route with real chips: claims, values, a kill and a restart, back to stock |
| [`D5.md`](../testing/board-runs/D5.md), [`D16.md`](../testing/board-runs/D16.md), [`D9.md`](../testing/board-runs/D9.md) | Frames from the sensor into a client at 57 to 145 fps with no drops; the DMA writes only the stream's buffers; stop, kill and unbind while streaming. Three receiver faults found and fixed |
| [`D7.md`](../testing/board-runs/D7.md), [`D18.md`](../testing/board-runs/D18.md) | The front end at capture rate: both outputs and statistics with every frame, frames with no job counted, refused configurations, 200 stops each way. Two module faults found and fixed: frames with no job uncounted, and a stop mid-frame that wedged the block until a reboot |
| [`D6.md`](../testing/board-runs/D6.md), [`D15.md`](../testing/board-runs/D15.md), [`D17.md`](../testing/board-runs/D17.md) | The back end on a captured frame: output identical to the stock driver's, round trip 739 µs against its 738, 120 jobs/s for 60 s, refused jobs, close and unbind with jobs on the block, no IOMMU fault. The first job on each buffer was dropped; fixed |
| [`U1a.md`](../testing/board-runs/U1a.md) | Not run yet: the back end's YCbCr, output colour conversion, debin, demosaic, sharpening, false colour, saturation, tone mapping, CAC and input decompression with the libpisp defaults, on the colour bars. HOG and stitch are refused before a job |
| [`U1b.md`](../testing/board-runs/U1b.md) | Not run yet: the camera ISP's layout methods, the back end's refusal of a configuration that does not match its input frame, and the configuration buffer rule |
| [`D8.md`](../testing/board-runs/D8.md), [`D10.md`](../testing/board-runs/D10.md) | The sensor's modes 0 and 1 with both clock-lane settings: line time within 0.02 %, no receiver errors at the shortest frame; the exposure limit; no OTP access on the bus. Exposure linearity and when a post lands need the scene lit: not judged |
| [`J4b2.md`](../testing/board-runs/J4b2.md) | Not run yet: the receiver's frame-start count against the sensor's frame rate, its sequence numbers, and whether every frame start shows (a channel with no buffer address raises none, as measured earlier) |
| [`B13.md`](../testing/board-runs/B13.md) | `thermal-zone` on the board's zone, a missing zone and a file of its own: values, `NOT_FOUND`, `IO`, recovery with no restart, the zone followed under load |
| [`GROW.md`](../testing/board-runs/GROW.md) | Not run yet: the data partition's first-boot grow on a card, MBR or GPT: `info` states, `e2fsck` after a clean power-off, a second boot that does nothing, the watchdog through the resize |
| [`WDT-ORDER.md`](../testing/board-runs/WDT-ORDER.md) | Not run yet: lemnosd's unit ordering on a board's cold boot, the unfed gap at its real systemd start, the watchdog descriptor store across a kill and a deliberate restart |

Each file says which of its checks ran and which did not, and why. The fixes the run made are in
the kernel module (`rs/pio.rs`, `rs/rp1_csi2.rs`, `rs/pisp_fe.rs`), the `rp1-csi2` and
`pisp` drivers and the Linux backend, each with a test that fails without it. The second run (PR
F2b) also gave the PIO I2C bus a module route.

## The second board run, the raw camera path (2026-11-13)

| Check | Result |
|---|---|
| [`F2e.md`](../testing/board-runs/F2e.md) | The 9000-frame raw run at 114.802 fps with no gaps or drops, as root and as the `lemnos` user with the three device nodes it needs; the sensor's six modes select; the LED ring's pixel order not decided; a client that stops with an error leaves the sensor started (found, not fixed) |
| [`F2f1.md`](../testing/board-runs/F2f1.md) | Not run on the board. The receiver and the front end claim their parts, observers read counters while another file drives; the chain run that needs it is the next board session |
| [`Q2i.md`](../testing/board-runs/Q2i.md) | Untested on board. The daemon's device permissions from the generated udev fragment (`lemnos-ctl tree --udev`): the module's nodes and the board's dma-heaps given to group `lemnos`, the start check that names a node it cannot open, and the steps to run on the board |
| [`F2g1.md`](../testing/board-runs/F2g1.md) | The chain proof's checks (not checked is a failure; timestamps; rate; the latch beside the chain) and the isp proof's 640x400 luma readout: not run, untested on board |
| [`CZ-MOD-CSI2.md`](../testing/board-runs/CZ-MOD-CSI2.md) | Not run on the board. The `rp1-csi2` receiver of the kernel module in Rust: the bind line and lane count, the raw run's counts and sequences, the frame-end stop, fifty stops and starts with no wedge, a consumer holding every buffer, an unbind while streaming |
| [`M34.md`](../testing/board-runs/M34.md) | Not run on the board. A front end's output0 lent to the back end's job with no copy: the chain proof end to end, the pool page's layout word and the lend's release on the board |
| [`READ-FLOOR.md`](../testing/board-runs/READ-FLOOR.md) | Not run on the board. The daemon's idle cost with the read floor: per-thread CPU and wakes a second, and what the I2C bus sees, against the measurement that found readings being read in a loop |
| [`CZ-MOD-FE-ACK.md`](../testing/board-runs/CZ-MOD-FE-ACK.md) | Partly run on the board (passes 2 and 3, the earlier module): a sensor stopped first wedges the receiver; a receiver stopped first ends clean; no frame end for an idle FORCE. Not yet run: the stop confirmed by the frame end or by idle, the `dma` attribute, release on a late frame end |
| [`CZ-MOD-PISP.md`](../testing/board-runs/CZ-MOD-PISP.md) | Not run on the board. The PiSP front end and back end of the kernel module in Rust: the chain runs, the front end's register write log against the C module's, fifty stops and starts with no wedge, a consumer keeping frames, the back end's queued and paced jobs, fifty closes with a job on the block, an unbind of each while streaming |
| [`F2e1.md`](../testing/board-runs/F2e1.md) | Not run yet: a receiver unbound mid-stream stops the OV9782 (host tests only: the watch and the check in `select_mode`, two tests fail with the check off); five unbind and rebind cycles on the board are the check |
| [`F2f2.md`](../testing/board-runs/F2f2.md) | Not run on the board. A client that goes away while its sensor streams stops the sensor (host check only) |
| [`L1-chain.md`](../testing/board-runs/L1-chain.md) | Not run. The steps of the camera chain run (sensor, receiver, front end, back end, 600+ frames, recovery), for the next board session |
| [`F2h.md`](../testing/board-runs/F2h.md) | The sensor's frame period: the mean interval is 0.04 % to 0.11 % above the nominal frame (lines × line time); a fixed per-frame line of 1456 sysclk fits all six runs; decided by the board run in `F2h3.md` |
| [`F2h3.md`](../testing/board-runs/F2h3.md) | The sensor's frame period `(N - 1) x HTS + G` (G = 0x382C/D, 1456), measured on the reference board at seven gated and five continuous frame lengths: every mean 7.5 to 8.1 ppm over it (the crystal); a write of G by 80 clocks moved the period by 832 ns (833.3 predicted); bit 2 of 0x382E reads 0 and the term is used anyway. 10-bit modes and the 720/400 windows: untested on board |
| [`F2h-judge.py`](../testing/board-runs/F2h-judge.py) | Judges the F2h board step: the intercepts of the frame-period candidates and the 0x382C/D write, from the rate-proof logs; checked on synthetic B1 and A logs, not run on a board |
| [`lemnos-check.sh`](../testing/board-runs/scripts/lemnos-check.sh) | Runs the checks that stop lemnosd or the comm core (B20, E25, E36, H4a, V1g) detached on the board, each stop behind a timed restore that outlives the check; rehearsed on the PC against the stand-in in `scripts/rehearse/`, not run on a board |
| [`FLAKE-CSI2.md`](../testing/board-runs/FLAKE-CSI2.md) | Untested on board. The receiver's line counter during STOP at the reference board's mode (10 us lines), against the stop window that rests on it; the QEMU checks run the window, not the hardware |

## What has been shown, and how

| What | Shown by | Not shown |
|---|---|---|
| The engine: binding, calls, values, containment, retries, the protocol, the client, the C API, packages | Tests on a PC, in the gate | Nothing in the engine depends on a board. Timing on a board is unmeasured |
| Chip drivers | Tests on a simulated bus whose registers the test writes | That the real chip answers as its datasheet says |
| Block drivers | Tests against a model of the kernel node, written from the kernel's source | That the kernel on a board behaves as its source reads; that the block does |
| The kernel module | QEMU, in a real kernel with lock and memory checking, on blocks with nothing behind them; on the reference board, the PiSP blocks, the CSI-2 receiver, I2C, a GPIO input, PWM and PIO (above) | Every other board and block; SPI; driven outputs |
| Frame streams | Tests with test memory, and a real dma-heap when one is named; on the reference board, frames from a sensor and buffers filled by both PiSP blocks | Every other board |
| The microcontroller host | Builds for two bare-metal targets; tests on a PC with mock peripherals | Any microcontroller |

Many of these drivers were ported from an earlier version of Lemnos that did run on boards. That
is where their register sequences come from. It is not evidence for this code: the driver model,
the host and the frame path under them are new.

## Known unknowns

These are stated in the code and can only be settled on a board:

- **When a posted exposure takes effect** on the `ov9782`. The driver assumes a frame; the check is
  in `D10.md`.
- **Whether the vendor capture drivers accept dma-bufs** from outside (`E39.md`). If not, frames
  on those boards are copied once.
- **Whether the vendor `rkcif` takes its memory-mode request before each format**, so that
  16-bit words come out as the uAPI's formats say (`H5a.md`). If not, only the compact layout
  is offered there.
- **Whether a program on the Rockchip NPU can reach its IOMMU's own registers** (`E32.md`). If it
  can, no driver in user space can confine it.
- **Register facts the code marks `Unconfirmed`**: the `k230-capture` frame path
  (`drivers/k230-capture-sys/src/regs.rs`) and parts of the `k230isp` register map
  (`drivers/k230isp-sys/src/regs.rs`).
- **What happens after a block is wedged.** The module keeps its memory pinned until it sees the
  DMA stop, and refuses `START` meanwhile ([Stopping a receiver](kernel-module.md#stopping-a-receiver)).
  On the board a sensor stopped before its receiver wedges the receiver, and the block raises no
  frame end for an idle FORCE (`CZ-MOD-FE-ACK.md`, passes 2 and 3). Release on a late frame end,
  and the idle-FORCE confirmation, are confirmed in QEMU and untested on board.
- **USB reset and kernel-driver detach** on usbfs (`A22.md`), and the gadget's asynchronous
  transfers and their cancel path (`E36.md`), have run only against test files and mocks.

## What cannot be made safe from user space

Some blocks are refused or cut down, not for want of a board, but because user space cannot keep
a job to the caller's own memory. They need a path through the kernel module first. They are
listed in [Drivers and contracts](drivers.md#library-crates) and in [`TODO.md`](../TODO.md).

## The Linux bus backends

| Check | What a board has to show |
|---|---|
| [`B2.md`](../testing/board-runs/B2.md) | I2C and SPI on the stock nodes: reads against `i2cget`, a kernel-owned address refused, a node removed |
| [`B3.md`](../testing/board-runs/B3.md) | GPIO, PWM and UART: levels, edge order, the PWM waveform, loopback at speed, unplug |
| [`B5.md`](../testing/board-runs/B5.md) | The PIO-made I2C bus: chip ID, a missing address, stuck-bus recovery, both speeds |
| [`B4.md`](../testing/board-runs/B4.md) | Hotplug events: the order the kernel sends them in, and an overflow |
| [`A25.md`](../testing/board-runs/A25.md) | Hints: a device bound within a second of the kernel announcing it, for each kind of name; a flapping port |
| [`B18.md`](../testing/board-runs/B18.md) | `raw-bus`: its closed-by-default policy on a real I2C, SPI, GPIO and PWM |
| [`G9-RAW-CLAIM.md`](../testing/board-runs/G9-RAW-CLAIM.md) | `raw-bus` claims on a real GPIO line and PWM channel: the safe state after a killed client, and kernel line edges read and subscribed to. Untested on board |
| [`SILENCE.md`](../testing/board-runs/SILENCE.md) | A sensor that stops sending behind its driver is seen silent and restarted: the sim half runs in `chain_silence`; the board step (a write to the sensor's mode select behind the driver) is not built; untested on board |
| [`G11-BUS-LISTS.md`](../testing/board-runs/G11-BUS-LISTS.md) | `raw-bus` transfer lists on a real I2C bus and SPI device: a repeated start, per-transfer settings and chip select. Untested on board |
| [`A35.md`](../testing/board-runs/A35.md) | Reading a chip the kernel's own driver holds, with its configuration left unchanged |

## Chips and small parts

| Check | What a board has to show |
|---|---|
| [`BENCHREF.md`](../testing/board-runs/BENCHREF.md) | `bench.sh --real` reference set on the reference board against dev's figures: IMU 200 Hz stamps and gaps, CPU at 50 and 200 Hz, USB cycle |
| [`B8.md`](../testing/board-runs/B8.md) | `bmi088`: bring-up, 200 Hz batches, stamp gaps, read times, an outage |
| [`B9.md`](../testing/board-runs/B9.md) | `bmm150`: field magnitude, rotation, an outage |
| [`B10.md`](../testing/board-runs/B10.md) | `orientation`: angles against a reference, rate dependence |
| [`MAG-GATE.md`](../testing/board-runs/MAG-GATE.md) | `orientation`: a magnetometer field outside 15 to 80 µT is ignored and health says so, and it clears when the source moves: untested on board |
| [`B11.md`](../testing/board-runs/B11.md) | `ina2xx`: volts, amps and watts against a meter |
| [`B12.md`](../testing/board-runs/B12.md) | `hwmon-fan`: take-over, release, hand-back on stop, the gap after a kill |
| [`S1b.md`](../testing/board-runs/S1b.md) | `hw-watchdog` as the daemon's keep-alive: the timeout, the feed while healthy, the stop when not, the restart, a panicking driver, clean shutdown |
| [`S1a.md`](../testing/board-runs/S1a.md) | `pwm-channel-fan`: bind at full, duty mapping, polarity, minimum and kick, tachometer, failure, release and stop |
| [`S1c.md`](../testing/board-runs/S1c.md) | `hwmon-fan`: take-over, release to full duty with the mode left manual, drop, a fan never taken |
| [`V1b3.md`](../testing/board-runs/V1b3.md) | The ranges, steps and warning limits of the fan, power monitor, thermal zone and watchdog items, from the board file and sysfs |
| [`V1b4.md`](../testing/board-runs/V1b4.md) | The `location` line of the thermal zone and the hwmon fan, and the three unit states in `lemnos-ctl show` |
| [`V1c.md`](../testing/board-runs/V1c.md) | The unit state through the package table, the real-valued ranges of the chip, bmm150 and orientation, the `location` the host fills from the board's uses, and `lemnos-ctl show` over the simulated walk |
| [`B13.md`](../testing/board-runs/B13.md) | `thermal-zone`: reads against the kernel's value, a missing zone |
| [`H3a.md`](../testing/board-runs/H3a.md) | The description form: a power monitor described by a file beside `ina2xx` on the same chip, identity refusal, an outage; a temperature or inertial chip from a file |
| [`H3b.md`](../testing/board-runs/H3b.md) | Drivers on a UART: `raw-bus` loopback and its frame settings, a Modbus sensor from a description, an outage, a wrong unit id |
| [`B14.md`](../testing/board-runs/B14.md) | `fan-control`: curve, slew, hold, a failed sensor, restart |
| [`B15.md`](../testing/board-runs/B15.md) | `gpio-power-switch`: two USB port power lines |
| [`B16.md`](../testing/board-runs/B16.md) | `ws2812`: bit timing, colour order, reset between frames |
| [`Q2.md`](../testing/board-runs/Q2.md) | the device-tree overlay from the board file: compiled and applied to the base tree; the boot fragment |
| [`Q1.md`](../testing/board-runs/Q1.md) | `ws2812` on the RP1 PIO block: the bit stream, reset gap and retry, on a 16-LED ring |
| [`Q2c.md`](../testing/board-runs/Q2c.md) | the reference board's overlay, boot fragment and module configuration; stable GPIO and PWM names |
| [`Q2b.md`](../testing/board-runs/Q2b.md) | the device-tree maps for the RK3566, RK3588, K230 and A733 families; untested on board |
| [`BB1.md`](../testing/board-runs/BB1.md) | serving data for the bench SoCs (the RK3566, RK3588, A733 and RV1126B maps and boards, `--check` and the offline serve run); the A733 USB OTG port by its platform device, the sources and what is unconfirmed; not run on a board |
| [`Q2e.md`](../testing/board-runs/Q2e.md) | USB device ports in the RK3566, RK3588 and RK3576 maps: peripheral mode, the PHYs, the UDC names; untested on board |
| [`Q2f.md`](../testing/board-runs/Q2f.md) | VBUS of a USB device port: the PHY's host supply replaced by a stand-in, the board's line or undriven statement, and the start check; untested on board |
| [`Q2g.md`](../testing/board-runs/Q2g.md) | boot holds: the overlay's `gpio-hog`, the boot fragment and the U-Boot fragment; the VBUS regulator disabled under a held line; untested on board |
| [`B17.md`](../testing/board-runs/B17.md) | `status-light`: looks, layers, frame interval, idle cost |
| [`B21.md`](../testing/board-runs/B21.md) | `vcm`: move, clamp, settle time |
| [`E24.md`](../testing/board-runs/E24.md) | `hw-watchdog`: arming, feed cadence, the reset when feeding stops |
| [`E29.md`](../testing/board-runs/E29.md) | A guest board on the operating system's own drivers: reads work, writes are refused, nothing changes on stop |

## The kernel module

| Check | What a board has to show |
|---|---|
| [`C1.md`](../testing/board-runs/C1.md) | The core on a real block: the check tool, interrupt latency, an IOMMU-confined mapping, unbind with a held buffer |
| [`C3.md`](../testing/board-runs/C3.md) | I2C and SPI through the module against the stock nodes: same data, time per transfer |
| [`C4.md`](../testing/board-runs/C4.md) | GPIO and PWM through the module: levels, edge stamps, the waveform |
| [`C5.md`](../testing/board-runs/C5.md) | PIO through the module: same reads, read time, clean-up after a killed holder |
| [`C6.md`](../testing/board-runs/C6.md) | Clocks, supplies, resets, thermal and events on a real block |
| [`C7.md`](../testing/board-runs/C7.md) | The daemon on the module route: its claims, the fall-back to stock |
| [`C8.md`](../testing/board-runs/C8.md) | The whole system on the module route against the stock route, timed |
| [`B23.md`](../testing/board-runs/B23.md) | DMA memory for a block bound to the module |
| [`G4.md`](../testing/board-runs/G4.md) | A block's registers, interrupts, clocks and resets through the backend, on a real block |
| [`I3.md`](../testing/board-runs/I3.md) | Frame timestamps from the module, a V4L2 node and a tick-stamped completion, all on `CLOCK_MONOTONIC` |

## The board's system

| Check | What a board has to show |
|---|---|
| [`S2.md`](../testing/board-runs/S2.md) | The board security switch: a change refused by name until its preconditions hold, applied at once with no restart, the view replaced by rename, a power cut between the record and the render, and a corrupt record reading on. Untested on board |
| [`S2b.md`](../testing/board-runs/S2b.md) | Turning the security switch off needs an authorization under the board's trust root: the request, a refusal by name for none, a wrong board, a bad signature or a replay, and off needing none on a board with no keys. Untested on board |
| [`I5.md`](../testing/board-runs/I5.md) | `system-slots` and `system-reboot`: a streamed install checked against its signed manifest, refused bundles, cancel, kill and restart mid-install, a confirmed trial, an unconfirmed trial falling back on a single-slot board, housekeeping, an install from a file in `.incoming`, the U-Boot record and script, and the board's own slot tool reading the same state |
| [`I5i3.md`](../testing/board-runs/I5i3.md) | The count-based fallback on the Pi firmware family: `[boot_count>3]` selecting the previous pointer (the threshold; see the correction in I5i3.md), the kernel watchdog keys, the panicking-kernel case, brown-out and the `rsts` bits. Untested on board |
| [`I5i4.md`](../testing/board-runs/I5i4.md) | The every-boot boot-file check (raw FAT read, no mount), the bundle's command line (byte for byte), the data partition by its label, and lemnosd's check before a restart and on a slow period. Untested on board |
| [`I5i5.md`](../testing/board-runs/I5i5.md) | The boot partition is never mounted at run time: the Pi firmware pointers and the trial record changed by one raw sector write (checksummed pointers), lemnosd's repoint and slot-init's default-mismatch repoint, and the U-Boot `extlinux.conf` check (`append` byte for byte against the hashed command line). Untested on board |
| [`I5i6.md`](../testing/board-runs/I5i6.md) | The Pi firmware's boot count: the property mailbox (get and clear) on `/dev/vcio`, the count and `rsts` read from the device tree, the firmware files `tree --boot-dir` writes, the boot-good and before-restart clears, and the arrival rules through lemnosd. **Not run.** |
| [`BB2.md`](../testing/board-runs/BB2.md) | The U-Boot restart count and episodes: `lemnos_restarts` raised by the script and cleared by lemnosd, the rung at 4 and the rescue at 8, the episodes demoting the default, and the power-on reset (blocked: no SoC has a cited reset word). Host side tested; **untested on board** |
| [`I5i7.md`](../testing/board-runs/I5i7.md) | The U-Boot form's trial and default in U-Boot's redundant environment, written raw: the environment found by its GPT type GUID and read by U-Boot, `saveenv` and the cut at each step, the first start's save, the `--uboot-config` fragment in a built U-Boot, and `boot.cmd`'s download mode (fastboot, LED, watchdog, no cable). **Not run.** |
| [`UBOOT-ENV-GRANT.md`](../testing/board-runs/UBOOT-ENV-GRANT.md) | The daemon's `uboot` record writes its U-Boot environment through the partition its root phase hands over, not the disk: a QEMU guest with a GPT environment shows the unit's user refused by name on the disk and the partition, the daemon's `set_default` through the handed partition, and the environment read back as root. Untested on board |
| [`I5i8.md`](../testing/board-runs/I5i8.md) | Payload bays on the boot partition: the two-sector pointer read by the firmware (torn sectors, which line wins), the bay boot, the U-Boot environment and per-set `extlinux.conf`. Untested on board |
| [`I5i.md`](../testing/board-runs/I5i.md) | The boot partition's two-copy records under real power cuts: a trial's attempts and the security switch's generation pulled out of power many times, every boot after each checked against its old-or-new rule, and no sector outside the record files changed. Untested on board |
| [`I5f.md`](../testing/board-runs/I5f.md) | How a trial resolves on a board: a healthy trial confirms by itself, a trial survives power cuts until its attempts run out, and the cost of the extra tryboot pass (power-on to the trial's daemon, with and without it); a state that can't be written at the deadline; a socket whose backlog is full; a USB device unplugged in a trial. Untested on board |
| [`HEALTH-TCP.md`](../testing/board-runs/HEALTH-TCP.md) | A trial that waits on a `tcp:` health source: IPv4 and IPv6 link-local with a zone, a port that does not answer, an address that is not up yet, and the systemd drop-in that lets the daemon open the socket. Untested on board |
| [`I5d.md`](../testing/board-runs/I5d.md) | `lemnos-slot-init` as a slot's `/init`: a plain boot through dm-verity, a trial, a damaged image, a wrong root hash and a cut-short image each falling back to the previous slot on trial, three fallbacks in a row stopping, and the default never written |
| [`I5h.md`](../testing/board-runs/I5h.md) | `lemnos-slot-init` on a kernel that lacks a piece of the chain: the missing option is named, no attempt is taken and no slot failed, and the trial is held (`untested on board`) |
| [`I5c.md`](../testing/board-runs/I5c.md) | Slots on raw NAND over UBI: an install into a static volume renamed into place, a refused and a cancelled install leaving no work volume, the record volume replaced by rename, `boot-ubi.cmd` run by U-Boot (trial, disarm, default, root copy), and `lemnos-slot-init` booting through `ubiblock` and falling back through the record volume |
| [`W2.md`](../testing/board-runs/W2.md) | `indicator`: its states on a real strip, each pattern kind over time (blink, bar, solid), the state set and refusals, and a job driving the states (`W1.md` or `I5.md`) |
| [`W1.md`](../testing/board-runs/W1.md) | `system-flash`: a first-flash image streamed through the bridge and written to a spare card, the refusals (`allow_first_flash`, the running system), a corrupt and a cancelled write, verify, the partition re-read, and the bridge's memory over a long stream |
| [`W1b.md`](../testing/board-runs/W1b.md) | `system-flash` on raw NAND: a UBI or raw image written to a spare MTD partition, its bad blocks skipped, the erase counts carried forward, the read-back and verify, the refusals (the layout, the size after bad blocks, the running root), and the same image attached by the kernel's UBI and read back, in QEMU against the kernel's NAND simulator (real chip: untested on board) |

## USB

| Check | What a board has to show |
|---|---|
| [`D11.md`](../testing/board-runs/D11.md) | The usbfs host: enumeration, descriptors, every transfer type, unplug, timeouts |
| [`A22.md`](../testing/board-runs/A22.md) | Reset and kernel-driver detach on usbfs |
| [`D12.md`](../testing/board-runs/D12.md) | `uvc`: descriptors, frames, drops, unplug and replug |
| [`H4a.md`](../testing/board-runs/H4a.md) | `[[each]]` by USB class: two cameras alike, ids by port, replug, a pinned and a refused one |
| [`D13.md`](../testing/board-runs/D13.md) | `coral-loader`: block size, end state, retries |
| [`E25.md`](../testing/board-runs/E25.md) | The gadget backend: identity, control stalls, endpoint bytes, unplug |
| [`E36.md`](../testing/board-runs/E36.md) | `usb-gadget` served by a caller: control replies, 64 MiB each way, cancel, unbind |
| [`R1.md`](../testing/board-runs/R1.md) | The gadget's NCM and ACM functions, the root phase's configfs tree and FunctionFS mount, and the UDC attribute the daemon writes after the drop |
| [`R1d.md`](../testing/board-runs/R1d.md) | The gadget's unit capability in its drop-in, and three hardening candidates to try on a board |
| [`PWM-SYSFS-UNIT.md`](../testing/board-runs/PWM-SYSFS-UNIT.md) | The root-phase PWM grant: exported channels chowned root:lemnos 0660 before the drop, the daemon writes them as `lemnos` under the unmodified unit. Guest: a real systemd 259.10 unit shows `/sys` rw; the PWM chip itself untested on board |
| [`UNIT-GRANTS.md`](../testing/board-runs/UNIT-GRANTS.md) | The root-phase grant of a hwmon fan's `pwm1` and `pwm1_enable` to root:lemnos 0660 before the drop, and the unit's grants table; untested on board |
| [`HOTPLUG-GRANT.md`](../testing/board-runs/HOTPLUG-GRANT.md) | The late grants `lemnos-ctl tree --udev` and `--mdev` write for a board's own fan and PWM channel: a fan's `pwm1` and `pwm1_enable` on its add, a channel's attributes on the chip's `EXPORT` change; the guest's real `systemd-udevd` and a real kernel PWM chip showed the event shape; untested on board |
| [`R1e.md`](../testing/board-runs/R1e.md) | The NCM interface's name (`ifname=`): the kernel takes it from 5.12 on, the setup refuses older kernels by name, and the read-back is reported as `ifname` |
| [`H4b.md`](../testing/board-runs/H4b.md) | Isochronous streams both ways: `uvc` with several transfers queued, and a gadget's isochronous endpoints in an alternate setting |
| [`R1f.md`](../testing/board-runs/R1f.md) | What a holder of the lent gadget pair sees on a real controller: the errno of a read in flight at each host or daemon event, and whether the same descriptors work again. QEMU (dummy_hcd, through lemnosd, R1g): cases 1 to 6 and the control request run with END asserted; untested on board |
| [`V1g.md`](../testing/board-runs/V1g.md) | The USB gadget set up by the privileged helper: a controller that appears late, host role then device role, configfs not mounted at start, the two processes and their capabilities, the helper dying and being forked again, a serial that is not readable yet, and stop taking the gadget down |

### NCM after the controller came back

A kernel fault, not a Lemnos one, found in the QEMU guest on 7.3.5 (the same `f_ncm.c` and
`net/core/dev.c` as the board kernel's 7.3.y tree), with no upstream fix found. When the USB
controller's device goes away and comes back (`echo dummy_udc.0 > .../drivers/dummy_udc/unbind`,
then `bind`) after a gadget's NCM network device was registered under it, every NCM registration
for the rest of the boot fails: the UDC write returns `EBUSY` and the log has
`kobject_add_internal failed for <ifname> (error: -2 parent: net)`. A failed registration leaves the
function's one `net_device` in `NETREG_UNREGISTERED` (`dev.c`, the error path of
`register_netdevice`), and binding that same function again hits the `BUG_ON` on `reg_state` at the
top of `register_netdevice`: a panic. Stopping and starting the gadget, or `lemnosd`, without the
controller's device going away does not meet it.

Reproduce: a gadget with `ncm.usb0` bound to `dummy_udc.0`; unbind and bind the `dummy_udc`
platform device. The kernel's own bind on the controller's return fails (`failed to start ...: -2`)
and the gadget stays pending on the controller, so a `UDC` write answers `EBUSY`; clear `UDC`, then
write it again with a new `ncm.usb0` (fails, the kobject line); a write with a function whose
registration failed is the panic.

What Lemnos does (the privileged helper's bind): when a bind fails and the tree has an NCM
function, that function is removed and a fresh one is made and linked in the board file's order,
and the bind is tried on it at once. When that fails too and a bind without the network function
works, the gadget runs without it, degraded, its reason saying the kernel refuses the network device
for the rest of the boot and that the USB network is gone until the board restarts; every later
bind on that controller takes the network function out before the write. No function a bind
failed on is ever bound again, and the FunctionFS function and the session a holder has are not
touched. In that state the host still has the link's interface and the ACM console; Ethernet
reaches the board once a peer listens there. Lemnos never restarts the board. QEMU: the
`ncm-after-controller` stanza, the last one, on the link check's daemon: the kernel's refusal is
in the log, the gadget is bound with the FunctionFS and ACM functions, the host has the device
configured with three interfaces, the reason names the state, and the kernel stays up. Untested on
board: no production path on the board is known to take the controller's device away.

## Cameras

| Check | What a board has to show |
|---|---|
| [`D8.md`](../testing/board-runs/D8.md) | `ov9782`: bring-up, mode timing, exposure limits, flips, recovery |
| [`F2g2.md`](../testing/board-runs/F2g2.md) | `ov9782`: the first chip-ID read waits 10 ms after the start (the datasheet's power-up time); the first start succeeds on the board or the board says why not |
| [`D10.md`](../testing/board-runs/D10.md) | `ov9782`: which frame a posted exposure lands on |
| [`J4b.md`](../testing/board-runs/J4b.md) | `ov9782`: which frame a posted exposure lands on, through the receiver's frame-start count (`post_values_at`); every real receiver answers `NOT_SUPPORTED` in this PR |
| [`V1b2.md`](../testing/board-runs/V1b2.md) | `ov9782`: the exposure, analogue gain and frame length items (ranges, steps, unit `line` for the first and last); a set through an item lands on the frame `post_values_at` gives, on the reference board |
| [`J4b3.md`](../testing/board-runs/J4b3.md) | Not run yet: the embedded data channel of the rp1-csi2 receiver (data type 0x12, a line a frame) on a sensor that sends it; no sensor in the tree does |
| [`J4.md`](../testing/board-runs/J4.md) | `ov9782` gain codes are linear; `rp1-csi2`, `k230-capture` and `sunxi-vin` publish identity 0 on every frame |
| [`D5.md`](../testing/board-runs/D5.md) | `rp1-csi2`: 60 and 120 frames a second with no drops, stop waits for DMA, a second receiver |
| [`D7.md`](../testing/board-runs/D7.md) | `pisp-fe`: statistics and output, queue pressure, 200 stop cycles, bad configurations |
| [`D6.md`](../testing/board-runs/D6.md) | `pisp-be`: a 1000-job round trip against the stock driver, equal output, refusal, the wedge test |
| [`D15.md`](../testing/board-runs/D15.md) | `pisp-be` output into the stream's own dma-bufs |
| [`D16.md`](../testing/board-runs/D16.md) | Receiver and front-end frames reaching a client of the daemon |
| [`D17.md`](../testing/board-runs/D17.md) | `pisp-be` taking its configuration and input from a client |
| [`D18.md`](../testing/board-runs/D18.md) | `pisp-fe` taking its configuration from a client and returning statistics |
| [`D9.md`](../testing/board-runs/D9.md) | The camera proof tool: a whole chain at 120 frames a second |
| [`D14.md`](../testing/board-runs/D14.md) | The V4L2 backend: formats, queues, export and import, topology, controls, events |
| [`E8.md`](../testing/board-runs/E8.md) | `rkcif`: graph names, routing, frames, recovery |
| [`E9.md`](../testing/board-runs/E9.md) | `rkisp`: graph, sizes, parameters taken, statistics, both paths |
| [`E12.md`](../testing/board-runs/E12.md) | `sunxi-vin`: layout, 3000 frames, a stall, recovery |
| [`U2c.md`](../testing/board-runs/U2c.md) | Not run yet: `sunxi-vin` selects its input before the format, and the input's name and kind via `VIDIOC_ENUMINPUT` |
| [`U2d.md`](../testing/board-runs/U2d.md) | Not run yet: `camera.receiver` `inputs` and `input` on `sunxi-vin`'s V4L2 node, listing the board's sensor inputs |
| [`E13.md`](../testing/board-runs/E13.md) | `isp600`: request numbers, statistics size, regions, an address refused |
| [`U3.md`](../testing/board-runs/U3.md) | `isp600` tuning encodings: channel black level and bit depths, module enables, temporal denoise record, the extent words refused |
| [`U4.md`](../testing/board-runs/U4.md) | `isp600` tuning: the default word of the record enable's register kept by its setter, the channel expand words, the table and statistics places from the vendor library; untested on board |
| [`E39.md`](../testing/board-runs/E39.md) | Capture straight into a stream's dma-bufs, and whether the vendor capture drivers take them at all |
| [`H5a.md`](../testing/board-runs/H5a.md) | `rkcif`'s 16-bit word layout through the vendor memory-mode request, a frame in planes straight into a stream, capture on standby |
| [`E11.md`](../testing/board-runs/E11.md) | `k230isp`: the register window, the request numbers, refusals, statistics |
| [`U2b.md`](../testing/board-runs/U2b.md) | Not run yet: `k230isp` lens shading: the 634-write load, the mapping of samples to sectors, the last sample of each row, the bank readback |
| [`H8b.md`](../testing/board-runs/H8b.md) | The K230 ISP through `lemnos.ko`: the main path's register map, frames into dma-bufs, the buffer switch at a frame's end, refusals, a stall reset; `k230-capture` and `k230isp` on one ISP at once |
| [`E30.md`](../testing/board-runs/E30.md) | `k230-capture`: nodes, the link, guard pages, formats, stop and restart |
| [`H1.md`](../testing/board-runs/H1.md) | The kernel module's `unconfined=`: the K230 ISP's registers mapped though no IOMMU confines it |
| [`J1.md`](../testing/board-runs/J1.md) | The sensor's link confirmed from the receiver's `source`, the receiver's `layout` with its stride, and the buffer length a stream reports: each receiver's answer on the reference board |

## Frame streams on dma-bufs

| Check | What a board has to show |
|---|---|
| [`A23.md`](../testing/board-runs/A23.md) | A block taking a dma-buf from the board's heap, and the consumer reading what it wrote |
| [`A24.md`](../testing/board-runs/A24.md) | Heaps found by name, descriptor counts, refusals, hardware-written bytes at the consumer |
| [`A31.md`](../testing/board-runs/A31.md) | A million jobs with a buffer given back and named again in the same round |
| [`I1.md`](../testing/board-runs/I1.md) | The shared-memory ring (`shm/`) built here: publish to a held frame against the 10.5 µs measured before the move, and its tests on the board's weaker memory order |
| [`I2.md`](../testing/board-runs/I2.md) | Frame streams on the shm rings: the camera path through `lemnosd`, consumer-filled streams for the ISP, and the kernel module's own ring still on its old layout (I2b). Not yet run on the board |
| [`I2d.md`](../testing/board-runs/I2d.md) | Outside readers on a stream's own buffers by ticket: the read-only dma-buf import on the reference board's GPU (measured by another team), and tickets on the camera chain's streams. Not yet run on the board |

## Image blocks

| Check | What a board has to show |
|---|---|
| [`E2.md`](../testing/board-runs/E2.md) | `rga`: conformance, the multi-core mapping, contiguity, the job streams |
| [`E4.md`](../testing/board-runs/E4.md) | `g2d`: each operation, chroma order, recovery |
| [`E7.md`](../testing/board-runs/E7.md) | `rkvpss`: scaling, the stride rule, flips, limits, buffers released |
| [`E5.md`](../testing/board-runs/E5.md) | `nonai2d`: conversion and colour |
| [`E6.md`](../testing/board-runs/E6.md) | `nonai2d`: overlay and border, guard bytes untouched |
| [`E3.md`](../testing/board-runs/E3.md) | `vglite`: crop, flip, rotate, fill and blend against a reference |
| [`H3c.md`](../testing/board-runs/H3c.md) | `nonai2d` and `vglite` bound from a board file: their job streams, a job past its buffer, recovery |
| [`E35.md`](../testing/board-runs/E35.md) | `k230-ai2d`: the import requests and the address they answer |
| [`E22.md`](../testing/board-runs/E22.md) | `k230-gsdma`: confirms there is no way in from user space |
| [`H7b.md`](../testing/board-runs/H7b.md) | `k230-gsdma` through `lemnos.ko`: turned copies byte-equal, refusals, a stopped job, unbind |

## Codec blocks

| Check | What a board has to show |
|---|---|
| [`E16.md`](../testing/board-runs/E16.md) | The MPP service: hardware ID, layouts, a claim, IOMMU confinement |
| [`E37.md`](../testing/board-runs/E37.md) | `codec.block/1` on each block that serves it: job checks, refusals, clocks off after a kill |
| [`N1.md`](../testing/board-runs/N1.md) | `v4l2-stateless` on an H.264 decoder: formats, a frame with its controls, a reference by stamp, a stamp refused, multi-slice pictures and the hold flag |
| [`E17.md`](../testing/board-runs/E17.md) | The IEP deinterlacer: strides, scratch size, results, a dropped job |
| [`E40.md`](../testing/board-runs/E40.md) | `rk-iep` through its job stream |
| [`E18.md`](../testing/board-runs/E18.md) | The JPEG block: hardware ID, the register file, offsets |
| [`RKJPEG-DEC.md`](../testing/board-runs/RKJPEG-DEC.md) | The JPEG decoder: the table-length units (joint sweep), the output format and chroma plane, the reach |
| [`E38.md`](../testing/board-runs/E38.md) | `a733-di` output and field order; JPEG through `rk-mpp` |
| [`E19.md`](../testing/board-runs/E19.md) | `a733-video`: the register window, the claim, a second claimant |
| [`H8a.md`](../testing/board-runs/H8a.md) | `a733-video` through `lemnos.ko`: the encoder's register map, an encode, tight extents, refusals, a timeout reset |
| [`K1a.md`](../testing/board-runs/K1a.md) | The VEPU541 H.264 encoder through `rk-mpp`: the identity register, the read-back window, the stream length, the address alignments |
| [`K1d.md`](../testing/board-runs/K1d.md) | The A733 encoder's QP map and CTU info records and header values: nothing runs on an engine; untested on board |
| [`K1a2.md`](../testing/board-runs/K1a2.md) | The VEPU541 H.264 encoder's second window (L2) and multi-run jobs through `rk-mpp`: the L2 words read back, the write runs, the refusals |
| [`K1a2b.md`](../testing/board-runs/K1a2b.md) | The VEPU580 H.264 encoder through `rk-mpp:rkvenc2`: the classes, the picture-class translation with its offset in the descriptor word, the status read-back |
| [`K1a3.md`](../testing/board-runs/K1a3.md) | The VEPU541 H.265 encoder through `rk-mpp:rkvenc`: the identity word, the read-back, one encode and the L2 words |
| [`K1a4.md`](../testing/board-runs/K1a4.md) | The VEPU580 H.265 encoder through `rk-mpp:rkvenc2`: the identity word, the status read-back, one encode |
| [`K1b.md`](../testing/board-runs/K1b.md) | The JPEG encoder (the VPU720 class) through `rk-mpp`: the version word, one encode, the stream's length and end marker |
| [`RKJPEGE.md`](../testing/board-runs/RKJPEGE.md) | The JPEG encoder's source-plane check is on the PC (the planes must lie in their buffers for the encoded picture). The encoder's hardware reach past the picture and the ECS length buffer's extent: parked, the K1b probe that decides them; untested on board |
| [`E20.md`](../testing/board-runs/E20.md) | `a733-di`: every mode, reads past the rows, recovery |
| [`E21.md`](../testing/board-runs/E21.md) | `k230-vpu`: layouts, JPEG encode and decode, guards, sessions |
| [`E41.md`](../testing/board-runs/E41.md) | `k230-vpu` through its job stream: out-of-order completions, a source change |

## Neural accelerators

| Check | What a board has to show |
|---|---|
| [`E32.md`](../testing/board-runs/E32.md) | `rknpu`: a domain per program, fault handling, whether a program can reach the IOMMU's own registers, a real program |
| [`G2.md`](../testing/board-runs/G2.md) | `rknpu` in the daemon: bound from a board file, a run through `lemnos-ctl`, a caller that goes, the node unbound and bound again |
| [`H7a.md`](../testing/board-runs/H7a.md) | `vip9000` through `lemnos.ko`: the chip and its MMU set-up, the program check against real compiled programs, a fault that writes nowhere and recovers, a run in the daemon |
| [`E33.md`](../testing/board-runs/E33.md) | `vip9000` on the vendor's node: confirms the facts behind its refusal to run there |
| [`E34.md`](../testing/board-runs/E34.md) | `k230-kpu`: confirms the facts behind its refusal to run |
| [`E10.md`](../testing/board-runs/E10.md) | `rkaiisp`: layouts, node order, what the kernel refuses |

## Everything else

| Check | What a board has to show |
|---|---|
| [`E27.md`](../testing/board-runs/E27.md) | Driver packages on a board: load, call, crash, signature, every kind of resource |
| [`E28.md`](../testing/board-runs/E28.md) | The microcontroller host on a microcontroller: boot, a bus outage, USB, flash and RAM |
| [`L3.md`](../testing/board-runs/L3.md) | The root phase on a board image: lemnosd and `lemnos-orion` start as root, drop to `lemnos`, keep `CAP_SYS_RAWIO`, and their device groups, socket and restart time |
| [`B20.md`](../testing/board-runs/B20.md) | `lemnos-orion` against a node on a board: every device a resource, with its topics and actions |
| [`I6b.md`](../testing/board-runs/I6b.md) | `lemnos-orion` on Orion `6dfb990`: sample times on the monotonic clock, facts and devices coming and going with no gap on unrelated topics, caller identity and `caller_gone`, idle sessions, a comm core restart |
| [`I6c.md`](../testing/board-runs/I6c.md) | `lemnos-orion` on Orion `455a956e`: a comm core restart with a run asked at once, facts and a device switched off and on with an unrelated topic watched, and a cancelled long action with its driver's holds unchanged |
| [`B22.md`](../testing/board-runs/B22.md) | The bench's reference set, measured for real |
| [`A17.md`](../testing/board-runs/A17.md) | The hardware performance counters on the reference board's cores |
| [`P1.md`](../testing/board-runs/P1.md) | The identity facts on the reference board: the serial from the device tree equals cpuinfo's `Serial`, and the daemon serves the same serial |
| [`P1c.md`](../testing/board-runs/P1c.md) | The bridge publishes the identity facts as `hello.*` on the reference board: the facts match the board file and the daemon, `offers` follows a device off and on, and a moved boot marker is followed without a rebind. Also records the RK3566 serial source on a mainline kernel (read on a board by another team) |
| [`F2d.md`](../testing/board-runs/F2d.md) | The PiSP camera path at 3324fec4: raw path 114.802 fps as root, front end through the links 114.803 fps, queued back end clean after the proof fix, back-end unbind 10 of 10; receiver unbind FAILED 5 of 5; full chain not run |
| [`M36-deadline-grow.md`](../testing/board-runs/M36-deadline-grow.md) | The good-boot deadline (`lemnos-good-boot`, `FailureAction=reboot-force`): expiry restarts the board and raises the firmware count to the previous slot at count 4; the data-partition grow to the disk's end, with a cut mid-pass finished by the next boot. Host side tested (every cut point on a file, `systemd-analyze verify`); untested on board |
| [`M41b.md`](../testing/board-runs/M41b.md) | The once-per-bind `running ok` figure taken from the host's stamp, not the 250 ms look: three sensors that logged 286 to 287 ms before must now agree with an independent bring-up time within 250 ms. Host side tested on the simulated board; untested on board |
| [`X1.md`](../testing/board-runs/X1.md) | The self-test contract on the reference board's own devices: the magnetometer's identity, the power monitor's bus voltage, the watchdog's answer, the fan's `spin` with its registers put back, the indicator's `lamp` with an operator, and a board run of `all` |
| [`R1h.md`](../testing/board-runs/R1h.md) | The link's Microsoft OS 1.0 descriptors (WinUSB and the GUID for the link, WINNCM, the vendor request and string 0xEE): Windows-style descriptor check run locally in QEMU (positive, OS descriptors off, one GUID byte in the driver, one in the contract); Windows 10, Windows 11 and macOS untested on board |
| [`M32.md`](../testing/board-runs/M32.md) | A stated camera link (the board names the receiver) reads `ok` when the receiver cannot say what is wired to its input, as rp1-csi2 does on c1: the sensor was `degraded` with a healthy chain. Checked on the simulated chain through the service and `lemnos-ctl` list, chain and describe. Untested on board: the read on the reference board is the check in the record |
| [`M36.md`](../testing/board-runs/M36.md) | The watchdog's gap at the boot's hand-over: slot-init arms the watchdog at 15 s, and lemnosd takes over feeding. QEMU showed the arming, a 45 s feed and the stall reset; the board's gap and lemnosd's feeding are **untested on board** |
| [`WDT-CHAIN.md`](../testing/board-runs/WDT-CHAIN.md) | The watchdog's chain from the kernel's probe to steady feeding: slot-init's open, the period the device applies, the hand-over gap from slot-init's exec to lemnosd's root-phase feed, and any refusal. Host side tested (`dog.rs`, `hw-watchdog`, the QEMU hand-over gap and the real lemnosd's start trace in the `daemon*` modes); **untested on board** |
| [`SLOT-MACHINE-ID.md`](../testing/board-runs/SLOT-MACHINE-ID.md) | The slot's machine id: read from `/etc/machine-id` after a boot, read-only mounted, the same after a reboot, and the journal directory under `/var/log/journal/<id>` unchanged. Host side tested (the derivation, the refusals, the driver's serial check); the QEMU guest cannot reach a slot hand-over; **untested on board** |
| [`M37.md`](../testing/board-runs/M37.md) | A refused bind says why: on the reference board the I2C chips' identity reads at bind, the sensor's power-up order (the supply binds before the sensor's identity read) and present devices reading `running ok`. The board's services are stopped with systemctl behind a failsafe timer for the window and started again. Run 2 (rev e0f4f74b): identity and cpu-thermal `running ok`; imu, mag and power did not reach `running ok` (their bring-up waited for a client's read). Fixed in M41; the M37 record is in `M37.md` |
| [`M41.md`](../testing/board-runs/M41.md) | The IMU, magnetometer and power monitor reach `running ok` from bind with no client: the daemon ticks their bring-up, and logs each device's time from bind. The second board check (same procedure as M37) must show all four devices `running ok` at +15 s with `list` only, the GPIO levels of the image daemon's lines, and the USB power switches' decision. Host side tested on simulated chips with their waits; **untested on board** |
| [`K230-USB.md`](../testing/board-runs/K230-USB.md) | The K230 USB-boot stage: the watchdog armed first, every run ending in the SoC reset back to the boot ROM. Run on the board: the return paths, the SD host reading and writing the slot's card, a bounded write test, and a whole image written through one boot-ROM load per piece and verified by a SHA-256 computed on the board. The stage's own session over the dwc2 device path (the FL1 flash wire) is simulated only: **untested on board**; its first run (B2: enumeration, endpoint 0, bulk echo and CRC over a RAM window, no card code) is planned and rehearsed over a register model of the core |

## Written, untested on board

| Check | Result |
|---|---|
| [`RESTART-UNIT.md`](../testing/board-runs/RESTART-UNIT.md) | The daemon's restart through the restart helper under the packaged units, in QEMU under systemd: the helper's check and its refusals of a peer in group lemnos and of a request that is not verb and nonce; the StartUnit failure disarms a trial with its reason; an accepted reboot keeps the trial armed, and a slow shutdown keeps it armed while the daemon answers. The BusyBox init restart runs in a QEMU guest. Not run on the board; the helper's 20 s bound (a reboot accepted but never begun) is covered only by its unit tests |
| [`REBOOT-TO-RPIBOOT.md`](../testing/board-runs/REBOOT-TO-RPIBOOT.md) | `reboot_to("rpiboot")` on `system.reboot/1`, offered only where the board file declares `[boot] loop_return`: in QEMU, the real daemon under the packaged unit, dropped to its user, sets the RPIBOOT one-shot through the mailbox its root phase holds (a root:root 0600 stand-in) and restarts through the helper; another target and a refusing mailbox ask and restart nothing. Untested on board |
| [`WAY-BACK-TOOL.md`](../testing/board-runs/WAY-BACK-TOOL.md) | `lemnos-way-back rpiboot` from an initramfs with no daemon: the firmware's one-shot RPIBOOT through `/dev/vcio`, a refusal and a missing node each exit 1 with nothing taken; QEMU on the mailbox stand-in only |
| [`BOOTFILES-OWNER.md`](../testing/board-runs/BOOTFILES-OWNER.md) | slot-init gives the boot-file store (`slots/bootfiles`) with the state and image dirs at every owned boot; a live install whose boot files differ commits on a `data-partition` tree; QEMU under the packaged unit only |
| [`TICK-BOUND.md`](../testing/board-runs/TICK-BOUND.md) | every install tick bounded by a quarter of the call deadline, checked per 64 KiB piece; the slow-period boot-file look off the tick; QEMU with throttled disk writes and unit tests with a slow clock only |
| [`TRIAL-COUNT.md`](../testing/board-runs/TRIAL-COUNT.md) | slot-init clears the firmware's boot count (and the restart baseline) before its restart into a trial, so a trial whose kernel never boots comes back to the default and counts nothing against it; modelled boot by boot in a test only |
| [`FLASH-BCM2712.md`](../testing/board-runs/FLASH-BCM2712.md) | The first flash of the reference board's eMMC from a RAM flasher: `system-flash` names its target by the eMMC's controller; in an arm64 QEMU guest with an eMMC and two boot areas, the real daemon and the bridge's stream write and verify an xz image, the answer carries `raw_bytes` and `outcome` as fields, the boot areas and the bytes after the image are unchanged. Untested on board |
| [`FLASH-TARGET-GRANT.md`](../testing/board-runs/FLASH-TARGET-GRANT.md) | `system-flash` writes its first-flash target through the node its root phase hands over: in an arm64 QEMU guest the real daemon, dropped to its own user, takes the bridge's stream onto a `root:root 0600` eMMC node, commits and verifies the image, and names the partition re-read its user can't do; before the change the stream's begin was refused with -19, as on the board. Untested on board |
| [`FLASH-BRIDGE-PUMP.md`](../testing/board-runs/FLASH-BRIDGE-PUMP.md) | The bridge's stream work runs off the thread that pumps its Orion session: a held device call leaves the device's other files answered, a stream and its job outlive a broken session (the bridge's or the sender's), and the push begun again resumes at the bytes taken, never at 0; a committed image pushed again is answered with its outcome and not sent again. PC tests through a real node and lemnosd with socket proxies. Untested on board |
| [`FLASH-DATA-OFF-CALL.md`](../testing/board-runs/FLASH-DATA-OFF-CALL.md) | `system-flash` decodes and writes the first-flash image on its own writer thread, never inside a call: `data` queues its piece or answers `BUSY` at once, and the bridge offers it again. In an arm64 QEMU guest at the default 2 s call deadline, the daemon as its own user, an eMMC throttled to 1 MiB/s is flashed and verified with the longest `data` call 8.7 ms, where the tree before failed with `-110`. A write that fails part-way ends the job naming the image byte and the partial image, and nothing reports success. Untested on board |
| [`MR1.md`](../testing/board-runs/MR1.md) | The maskrom reflash of the RK3566 board: the on-chip stage and the shared wire pass their tests on the host (the PC-side host is in another project); the reboot-mode entry, the watchdog's survival through the boot ROM, the 472 load address and the first SD write are untested on board. Nothing has been run on the board |
| [`ZERO3E-MR1.md`](../testing/board-runs/ZERO3E-MR1.md) | The ZERO 3E maskrom run sheet (RK3566): the watchdog-return line (run, passed), the 471 stub that clears OS_REG0 (built, qemu-rehearsed), and steps E, 1a, 1b, U1 (not run: no PC-side host, no go). Untested on board. |
| [`BOARD-USB0.md`](../testing/board-runs/BOARD-USB0.md) | The RK3566 bench board on its mainline image, asked for its identity and blocks: none of the open probe records can run there (no vendor service node, no NPU node); what an image or a serial console would unlock. Read-only session. **untested on board** |
| [`FL1.md`](../testing/board-runs/FL1.md) | The flash wire and the on-chip stage's device side: every timing (init budget, first window from send-end, reply bound, the phase deadlines, the storage budget) is checked in simulation only; the controller command times, address-before-status, the speed encoding, the stop procedure and the 64 KiB storage budget on a slow card are untested on board. The surplus bytes of an overlong transfer are open. Nothing has been run on the board; **untested on board** |
| [`FEL.md`](../testing/board-runs/FEL.md) | The Allwinner FEL thunk (armv7 and AArch64): the pinned bytes run correctly under user-mode emulation, and the build reproduces them; the return into the boot ROM's FEL loop, the instruction set the A733's FEL loop runs, the SoC's watchdog values and the bad-table reset are untested on board. Nothing has been run on the board; **untested on board** |
| [`R2-CFG.md`](../testing/board-runs/R2-CFG.md) | The board kit's kernel-config step accepts the one toolchain-detected line (`CONFIG_RUST_IS_AVAILABLE=y`) and nothing else; PC tests against the real CONFIG_RUST configurations only. Kit outside this repository. **untested on board** |
| [`DAEMON-SECCOMP.md`](../testing/board-runs/DAEMON-SECCOMP.md) | The daemon under its unit's syscall filter on the board image: the kernel's seccomp mode of the running daemon (expected 2) and the unit's `SystemCallFilter` as loaded (expected non-empty). Two read-only reads, written, not run. **untested on board** |
| [`R2-PREV.md`](../testing/board-runs/R2-PREV.md) | The Pi form's previous slot at counts 4 to 7 (before flashing mode): the count's pick is the firmware's and is sim-only, since a trial slot does not go through `[boot_count&4]`; slot-init's side is tested in simulation. The check is written, not run. **untested on board** |
