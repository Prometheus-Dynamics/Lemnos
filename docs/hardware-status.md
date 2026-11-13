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

On 2026-11-11, on the reference board (a Raspberry Pi Compute Module 5, kernel `7.2.9-v8-16k`),
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

Each file says which of its checks ran and which did not, and why. The fixes the run made are in
the kernel module (`lemnos_pio.c`, `lemnos_rp1_csi2.c`, `lemnos_pisp_fe.c`), the `rp1-csi2` and
`pisp` drivers and the Linux backend, each with a test that fails without it. The second run (PR
F2b) also gave the PIO I2C bus a module route.

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
- **What happens after a block is wedged.** The module keeps its memory pinned until reboot by
  design. The path has run only against a test block.
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
| [`A35.md`](../testing/board-runs/A35.md) | Reading a chip the kernel's own driver holds, with its configuration left unchanged |

## Chips and small parts

| Check | What a board has to show |
|---|---|
| [`B8.md`](../testing/board-runs/B8.md) | `bmi088`: bring-up, 200 Hz batches, stamp gaps, read times, an outage |
| [`B9.md`](../testing/board-runs/B9.md) | `bmm150`: field magnitude, rotation, an outage |
| [`B10.md`](../testing/board-runs/B10.md) | `orientation`: angles against a reference, rate dependence |
| [`B11.md`](../testing/board-runs/B11.md) | `ina2xx`: volts, amps and watts against a meter |
| [`B12.md`](../testing/board-runs/B12.md) | `hwmon-fan`: take-over, release, hand-back on stop, the gap after a kill |
| [`S1b.md`](../testing/board-runs/S1b.md) | `hw-watchdog` as the daemon's keep-alive: the timeout, the feed while healthy, the stop when not, the restart, a panicking driver, clean shutdown |
| [`S1a.md`](../testing/board-runs/S1a.md) | `pwm-channel-fan`: bind at full, duty mapping, polarity, minimum and kick, tachometer, failure, release and stop |
| [`S1c.md`](../testing/board-runs/S1c.md) | `hwmon-fan`: take-over, release to full duty with the mode left manual, drop, a fan never taken |
| [`V1b3.md`](../testing/board-runs/V1b3.md) | The ranges, steps and warning limits of the fan, power monitor, thermal zone and watchdog items, from the board file and sysfs |
| [`V1b4.md`](../testing/board-runs/V1b4.md) | The `location` line of the thermal zone and the hwmon fan, and the three unit states in `lemnos-ctl show` |
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
| [`I5.md`](../testing/board-runs/I5.md) | `system-slots` and `system-reboot`: a streamed install checked against its signed manifest, refused bundles, cancel, kill and restart mid-install, a confirmed trial, an unconfirmed trial falling back on a single-slot board, housekeeping, an install from a file in `.incoming`, the U-Boot record and script, and the board's own slot tool reading the same state |
| [`I5f.md`](../testing/board-runs/I5f.md) | How a trial resolves on a board: a healthy trial confirms by itself, a trial survives power cuts until its attempts run out, and the cost of the extra tryboot pass (power-on to the trial's daemon, with and without it); a state that can't be written at the deadline; a socket whose backlog is full; a USB device unplugged in a trial. Untested on board |
| [`I5d.md`](../testing/board-runs/I5d.md) | `lemnos-slot-init` as a slot's `/init`: a plain boot through dm-verity, a trial, a damaged image, a wrong root hash and a cut-short image each falling back to the previous slot on trial, three fallbacks in a row stopping, and the default never written |
| [`I5c.md`](../testing/board-runs/I5c.md) | Slots on raw NAND over UBI: an install into a static volume renamed into place, a refused and a cancelled install leaving no work volume, the record volume replaced by rename, `boot-ubi.cmd` run by U-Boot (trial, disarm, default, root copy), and `lemnos-slot-init` booting through `ubiblock` and falling back through the record volume |
| [`W2.md`](../testing/board-runs/W2.md) | `indicator`: its states on a real strip, each pattern kind over time (blink, bar, solid), the state set and refusals, and a job driving the states (`W1.md` or `I5.md`) |
| [`W1.md`](../testing/board-runs/W1.md) | `system-flash`: a first-flash image streamed through the bridge and written to a spare card, the refusals (`allow_first_flash`, the running system), a corrupt and a cancelled write, verify, the partition re-read, and the bridge's memory over a long stream |

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
| [`H4b.md`](../testing/board-runs/H4b.md) | Isochronous streams both ways: `uvc` with several transfers queued, and a gadget's isochronous endpoints in an alternate setting |

## Cameras

| Check | What a board has to show |
|---|---|
| [`D8.md`](../testing/board-runs/D8.md) | `ov9782`: bring-up, mode timing, exposure limits, flips, recovery |
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
| [`B22.md`](../testing/board-runs/B22.md) | The bench's reference set, measured for real |
| [`A17.md`](../testing/board-runs/A17.md) | The hardware performance counters on the reference board's cores |
| [`P1.md`](../testing/board-runs/P1.md) | The identity facts on the reference board: the serial from the device tree equals cpuinfo's `Serial`, and the daemon serves the same serial |
| [`P1c.md`](../testing/board-runs/P1c.md) | The bridge publishes the identity facts as `hello.*` on the reference board: the facts match the board file and the daemon, `offers` follows a device off and on, and a moved boot marker is followed without a rebind. Also records the RK3566 serial source on a mainline kernel (read on a board by another team) |
| [`F2d.md`](../testing/board-runs/F2d.md) | The PiSP camera path at 3324fec4: raw path 114.802 fps as root, front end through the links 114.803 fps, queued back end clean after the proof fix, back-end unbind 10 of 10; receiver unbind FAILED 5 of 5; full chain not run |
