# Hardware status

**Nothing in this repository has been run on hardware in its present form.** Every driver, every
backend and the kernel module were written and tested against simulations, mocks and models of
the kernel's behaviour. Each is marked *untested on board*, and each has a written check waiting
under `testing/board-runs/`. None of those 83 files records a result yet.

This page lists what a board still has to show. [Testing](testing.md#board-runs) says how to do a
run and where its result goes.

```sh
ls testing/board-runs/*.md | wc -l
grep -Li 'untested on board' testing/board-runs/*.md | wc -l
```

The first number is how many checks are written. The second is how many no longer say
`untested on board`.

## What has been shown, and how

| What | Shown by | Not shown |
|---|---|---|
| The engine: binding, calls, values, containment, retries, the protocol, the client, the C API, packages | Tests on a PC, in the gate | Nothing in the engine depends on a board. Timing on a board is unmeasured |
| Chip drivers | Tests on a simulated bus whose registers the test writes | That the real chip answers as its datasheet says |
| Block drivers | Tests against a model of the kernel node, written from the kernel's source | That the kernel on a board behaves as its source reads; that the block does |
| The kernel module | QEMU, in a real kernel with lock and memory checking, on blocks with nothing behind them | Any real block, bus, interrupt or DMA |
| Frame streams | Tests with test memory, and a real dma-heap when one is named | A frame from a sensor; a buffer filled by a block |
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
| [`B13.md`](../testing/board-runs/B13.md) | `thermal-zone`: reads against the kernel's value, a missing zone |
| [`B14.md`](../testing/board-runs/B14.md) | `fan-control`: curve, slew, hold, a failed sensor, restart |
| [`B15.md`](../testing/board-runs/B15.md) | `gpio-power-switch`: two USB port power lines |
| [`B16.md`](../testing/board-runs/B16.md) | `ws2812`: bit timing, colour order, reset between frames |
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

## USB

| Check | What a board has to show |
|---|---|
| [`D11.md`](../testing/board-runs/D11.md) | The usbfs host: enumeration, descriptors, every transfer type, unplug, timeouts |
| [`A22.md`](../testing/board-runs/A22.md) | Reset and kernel-driver detach on usbfs |
| [`D12.md`](../testing/board-runs/D12.md) | `uvc`: descriptors, frames, drops, unplug and replug |
| [`D13.md`](../testing/board-runs/D13.md) | `coral-loader`: block size, end state, retries |
| [`E25.md`](../testing/board-runs/E25.md) | The gadget backend: identity, control stalls, endpoint bytes, unplug |
| [`E36.md`](../testing/board-runs/E36.md) | `usb-gadget` served by a caller: control replies, 64 MiB each way, cancel, unbind |

## Cameras

| Check | What a board has to show |
|---|---|
| [`D8.md`](../testing/board-runs/D8.md) | `ov9782`: bring-up, mode timing, exposure limits, flips, recovery |
| [`D10.md`](../testing/board-runs/D10.md) | `ov9782`: which frame a posted exposure lands on |
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
| [`E13.md`](../testing/board-runs/E13.md) | `isp600`: request numbers, statistics size, regions, an address refused |
| [`E39.md`](../testing/board-runs/E39.md) | Capture straight into a stream's dma-bufs, and whether the vendor capture drivers take them at all |
| [`E11.md`](../testing/board-runs/E11.md) | `k230isp`: the register window, the request numbers, refusals, statistics |
| [`E30.md`](../testing/board-runs/E30.md) | `k230-capture`: nodes, the link, guard pages, formats, stop and restart |

## Frame streams on dma-bufs

| Check | What a board has to show |
|---|---|
| [`A23.md`](../testing/board-runs/A23.md) | A block taking a dma-buf from the board's heap, and the consumer reading what it wrote |
| [`A24.md`](../testing/board-runs/A24.md) | Heaps found by name, descriptor counts, refusals, hardware-written bytes at the consumer |
| [`A31.md`](../testing/board-runs/A31.md) | A million jobs with a buffer given back and named again in the same round |

## Image blocks

| Check | What a board has to show |
|---|---|
| [`E2.md`](../testing/board-runs/E2.md) | `rga`: conformance, the multi-core mapping, contiguity, the job streams |
| [`E4.md`](../testing/board-runs/E4.md) | `g2d`: each operation, chroma order, recovery |
| [`E7.md`](../testing/board-runs/E7.md) | `rkvpss`: scaling, the stride rule, flips, limits, buffers released |
| [`E5.md`](../testing/board-runs/E5.md) | `nonai2d`: conversion and colour |
| [`E6.md`](../testing/board-runs/E6.md) | `nonai2d`: overlay and border, guard bytes untouched |
| [`E3.md`](../testing/board-runs/E3.md) | `vglite`: crop, flip, rotate, fill and blend against a reference |
| [`E35.md`](../testing/board-runs/E35.md) | `k230-ai2d`: the import requests and the address they answer |
| [`E22.md`](../testing/board-runs/E22.md) | `k230-gsdma`: confirms there is no way in from user space |

## Codec blocks

| Check | What a board has to show |
|---|---|
| [`E15.md`](../testing/board-runs/E15.md) | Stateless decode: an intra picture byte-equal to a reference, timings |
| [`E16.md`](../testing/board-runs/E16.md) | The MPP service: hardware ID, layouts, a claim, IOMMU confinement |
| [`E37.md`](../testing/board-runs/E37.md) | `codec.block/1` on each block that serves it: job checks, refusals, clocks off after a kill |
| [`E17.md`](../testing/board-runs/E17.md) | The IEP deinterlacer: strides, scratch size, results, a dropped job |
| [`E40.md`](../testing/board-runs/E40.md) | `rk-iep` through its job stream |
| [`E18.md`](../testing/board-runs/E18.md) | The JPEG block: hardware ID, the register file, offsets |
| [`E38.md`](../testing/board-runs/E38.md) | `a733-di` output and field order; JPEG through `rk-mpp` |
| [`E19.md`](../testing/board-runs/E19.md) | `a733-video`: the register window, the claim, a second claimant |
| [`E20.md`](../testing/board-runs/E20.md) | `a733-di`: every mode, reads past the rows, recovery |
| [`E21.md`](../testing/board-runs/E21.md) | `k230-vpu`: layouts, JPEG encode and decode, guards, sessions |
| [`E41.md`](../testing/board-runs/E41.md) | `k230-vpu` through its job stream: out-of-order completions, a source change |

## Neural accelerators

| Check | What a board has to show |
|---|---|
| [`E32.md`](../testing/board-runs/E32.md) | `rknpu`: a domain per program, fault handling, whether a program can reach the IOMMU's own registers, a real program |
| [`G2.md`](../testing/board-runs/G2.md) | `rknpu` in the daemon: bound from a board file, a run through `lemnos-ctl`, a caller that goes, the node unbound and bound again |
| [`E33.md`](../testing/board-runs/E33.md) | `vip9000`: confirms the facts behind its refusal to run |
| [`E34.md`](../testing/board-runs/E34.md) | `k230-kpu`: confirms the facts behind its refusal to run |
| [`E10.md`](../testing/board-runs/E10.md) | `rkaiisp`: layouts, node order, what the kernel refuses |

## Everything else

| Check | What a board has to show |
|---|---|
| [`E27.md`](../testing/board-runs/E27.md) | Driver packages on a board: load, call, crash, signature, every kind of resource |
| [`E28.md`](../testing/board-runs/E28.md) | The microcontroller host on a microcontroller: boot, a bus outage, USB, flash and RAM |
| [`B20.md`](../testing/board-runs/B20.md) | `lemnos-orion` against a node: every device appears with its values |
| [`B22.md`](../testing/board-runs/B22.md) | The bench's reference set, measured for real |
| [`A17.md`](../testing/board-runs/A17.md) | The hardware performance counters on the reference board's cores |
