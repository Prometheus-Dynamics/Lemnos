# Drivers and contracts

Every driver and every contract in the repository, with the state each is in. Three states:

- **Serves its contract**: built into the daemon, and its contract's methods do real work.
- **Refuses to run**: the code is there and checked, but it refuses its main job, and the row says
  why.
- **Library only**: code that other crates build on, or a driver that is not built into the daemon.
  A board file cannot name it.

A state says what the code does. It does not say the driver has run on hardware. **No driver here
has been run on hardware in this form.** [Hardware status](hardware-status.md) lists what that
leaves open, driver by driver.

Each driver's own settings are documented at the top of its crate, `drivers/<crate>/src/lib.rs`.
The name in the first column is what a board file writes after `driver =`. The commands below show
that the names are the ones the daemon knows:

```sh
grep -c 'entry::<' service/lemnosd/src/builtin.rs
python3 scripts/generate-builtins.py --check
```

## Chips and small parts

| Driver | Provides | Needs | State |
|---|---|---|---|
| `bmi088` | `accelerometer/1`, `gyroscope/1` | `bus`: I2C | Serves its contract. Batches only with `fifo = true` |
| `bmm150` | `magnetometer/1`, `self-test/1` | `bus`: I2C | Serves its contract. Reads only on a host-owned device |
| `generic-magnetometer` | `magnetometer/1` | `bus`: I2C; `description`: a chip file | Serves its contract. The [description form](writing-a-driver.md#the-description-form), as are the four below |
| `generic-accelerometer` | `accelerometer/1` | `bus`: I2C; `description`: a chip file | Serves its contract. `read` only: no batches, a description has no FIFO |
| `generic-gyroscope` | `gyroscope/1` | `bus`: I2C; `description`: a chip file | Serves its contract. `read` only: no batches, a description has no FIFO |
| `generic-temperature` | `temperature/1` | `bus`: I2C; `description`: a chip file | Serves its contract |
| `generic-power-monitor` | `power-monitor/1` | `bus`: I2C; `description`: a chip file | Serves its contract. The chip's own registers give all four values |
| `modbus-magnetometer` | `magnetometer/1` | `uart`: a UART; `description`: a chip file starting `modbus` | Serves its contract. The description form for a [Modbus RTU device](writing-a-driver.md#a-chip-on-modbus), as are the four below: each read is a request and its checked reply |
| `modbus-accelerometer` | `accelerometer/1` | `uart`: a UART; `description`: a chip file starting `modbus` | Serves its contract. `read` only |
| `modbus-gyroscope` | `gyroscope/1` | `uart`: a UART; `description`: a chip file starting `modbus` | Serves its contract. `read` only |
| `modbus-temperature` | `temperature/1` | `uart`: a UART; `description`: a chip file starting `modbus` | Serves its contract |
| `modbus-power-monitor` | `power-monitor/1` | `uart`: a UART; `description`: a chip file starting `modbus` | Serves its contract |
| `ina2xx` | `power-monitor/1`, `self-test/1` | `bus`: I2C; `chip`: `ina226`, `ina238` or `ina260` | Serves its contract |
| `vcm` | `lens/1` | `bus`: I2C; `chip`; optional `direction`, `settle_us` | Serves its contract. The position is unknown until the first move. Direction and settle time are unknown unless the board gives them |
| `ws2812` | `pixels/1` | `bus`: SPI device, or a `pio-out` name (PIO state machine running the strip's 800 kHz program) | Serves its contract. The PIO program's timing is untested on board (`Q1.md`) |
| `gpio-power-switch` | `power-switch/1` | `line`: GPIO; optional `fault`: GPIO | Serves its contract. On and off only |
| `raw-bus` | `raw-bus/1` | any of `i2c`, `spi`, `line`, `pwm`, `uart` | Serves its contract. Closed by default: writes and each bus are allowed only by `config`. A UART's frame is set at bind; reading it needs no `writes` |
| `ov9782` | `camera.sensor/1` | `bus`: I2C; optional `values` stream | Serves its contract. Modes 0 and 1 measured on the reference board with both clock-lane settings, the exposure limit and refusal, and a bus trace with no OTP access (`D8.md`). When a posted exposure takes effect is an assumption until a run with the scene lit (`D10.md`) |

## Devices with no bus of their own

| Driver | Provides | Needs | State |
|---|---|---|---|
| `orientation` | `orientation/1` | `accel`, `gyro`, optional `mag`: devices | Serves its contract. No calibration: that is the caller's |
| `fan-control` | `fan-control/1` | `fan`, `t0` and optional `t1` to `t3`: devices | Serves its contract |
| `status-light` | `light/1` | `pixels`: a device | Serves its contract |
| `indicator` | `indicator/1`, `self-test/1` | `pixels`: a device (the LED strip, or a GPIO LED driver that provides `pixels/1`); `brightness` and `state.<name>` patterns in `config` | Serves its contract: named states (`idle`, `receiving`, `writing`, `verifying`, `done`, `failed`), each drawn with the pattern the board gives it (`solid`, `blink`, `chase`, `bar`, `off`), as a frame on the device's tick. An unknown state is refused. Allocates nothing once started |
| `pwm-channel-fan` | `fan/1` | `pwm`: a PWM channel, and optional `tach`: a GPIO line | Serves its contract. Drives the PWM directly, with no kernel fan driver between. Full duty at start, on release, and when stopped |
| `hwmon-fan` | `fan/1`, `self-test/1` | a hwmon node, by `name` or `hwmon` | Serves its contract, for a fan the kernel exposes through hwmon. Takes the fan over at the first `set_duty`. `release`, and dropping the driver, put the fan at full duty; nothing is handed to the kernel's thermal governor. A killed daemon runs no code, so the fan keeps the last duty |
| `thermal-zone` | `temperature/1` | `path`: a thermal zone | Serves its contract. Run on the reference board (`B13.md`) |
| `gpu-lease` | `gpu-lease/1` | nothing | Serves its contract. The lease is advisory: nothing is enforced on the GPU |
| `host-clock` | no contract | nothing | Runs. Shows the kernel's clock source and whether an RTC set the clock, as values |
| `hw-watchdog` | `self-test/1` | `uses = { watchdog = "watchdog:NODE" }` (the watchdog by its device-tree node), or `device`: a numbered watchdog node such as `watchdog0`; and `keepalive = true` on the device | Sets the timeout (`timeout_s`, default 15 s, clamped to the device's range) and reads it back. Feeds the node at half the timeout, only while the daemon is healthy (see [If the daemon dies](daemon.md#if-the-daemon-dies)). Never writes the magic close |

## The board's own system

| Driver | Provides | Needs | State |
|---|---|---|---|
| `system-slots` | `system.slots/1` | nothing; `data` and `boot` partitions, `record`, `keys`, `boot_args` in `config` (`docs/slots.md`) | Serves its contract. Installs a slot bundle as a long job, streamed or from a file in `.incoming`, checked against its schema 3 manifest and refused when built for another board (the board file's `[board] id`); a signature is always checked and shown, and `require_signed` refuses bundles no trusted key signed (off by default). Sets the default, boots a slot on trial, confirms, removes and keeps the last few. Keeps the boot history (`boots`, one line per boot, `docs/slots.md`). With an `indicator` need (`uses = { indicator = "<id>" }`) it shows `receiving` (with its progress), `verifying`, `done` or `failed` as an install runs. Never over the running or the default slot. On raw NAND (`ubi`, `record = "ubi"`) the images and the record are UBI volumes, replaced by atomic rename. Untested on board (`I5.md`, `I5c.md`) |
| `system-flash` | `system.flash/1` | `path` (the one target), `allow_first_flash`, `kind` (`block`, `file`, `mtd`), `image` and `mark_bad` (mtd), `max_dict_mb` (1 to 8), `keys`, `require_signed`, `mountinfo` in `config`; an optional `indicator` need (`docs/slots.md`, "First flash") | Serves its contract. Writes a whole first-flash image, streamed through `data` (or from a bridge's stream), decoded on the board (xz with a pure-Rust streaming decoder: one image stream and an optional signature-record stream; or raw), checked against its manifest's SHA-256 and size, its record checked against the one given, and re-read as a partition table. A signed image is verified against the board's keys with the signed message the board rebuilds; the policy (`require_signed`) is in `docs/slots.md`, "First flash". Refused unless `allow_first_flash` is set, on the running system's device, and for an image larger than the target. Raw NAND (`kind = "mtd"`): one driver per partition, bad blocks and erase through `drivers/system-flash-sys`, every block read back, UBI erase counts carried forward (`docs/slots.md`, "Raw NAND"). Untested on board (`W1.md`, `W1b.md`) |
| `system-identity` | `system.identity/1` | nothing; `serial` source, `name`, `revision`, `data` in `config`; the family is the board's `[board] id` | Serves the board's identity facts: `serial`, `family`, `model` and the running slot's `version.slot`, `version.product`, `version.os`. Binds fail when a serial source cannot be read, so the host retries. Untested on board (`P1.md`) |
| `system-reboot` | `system.reboot/1` | `data` (the data partition, `/data` by default) | Serves its contract. Leaves the caller's request for the boot history, then runs `systemctl reboot` |
| `named-files` | `lemnos.files/1` | `data`, `boot`, `sys_block`, `module`, and the image's `tree_overlay`, `tree_boot`, `tree_modprobe` paths (all optional, with the defaults `docs/daemon.md` gives) | Serves the `lemnos/` names (`docs/daemon.md`, "Named files"): each is a snapshot taken when a reader opens it, read in pieces, and closed. Lists what it serves in `lemnos/index` |

## USB

| Driver | Provides | Needs | State |
|---|---|---|---|
| `uvc` | `camera.frames/1` | `usb`: a USB device; `frames` stream | Serves its contract. Keeps 4 isochronous transfers queued while streaming. A frame larger than `frame_bytes` is dropped and counted. An `[[each]]` entry matching `usb:class:0e` binds it to every camera plugged in |
| `coral-loader` | no contract | `usb`: a USB device; `firmware`: a file | Runs. Loads the firmware file the board file names over DFU, then resets the device. It never reads the firmware |
| `usb-gadget` | `usb.function/1` | `function`: `udc:NAME`; optional `data` stream | Serves its contract. A caller serves the endpoints, isochronous ones in alternate settings among them. An NCM function may name its interface (`ifname=`); the kernel's name is described as `ifname` once bound |

## Camera receivers and ISPs

| Driver | Provides | Needs | State |
|---|---|---|---|
| `rp1-csi2` | `camera.receiver/1` | the kernel module; `frames` stream of dma-bufs | Serves its contract. Run on the reference board at 57 to 145 fps, with 200 stops each by `STOP`, client kill, daemon kill and unbind (`D5.md`) |
| `pisp-fe` | `camera.isp/1`, `isp.pisp/1` | the kernel module; `config` stream, optional `statistics`, `output0`, `output1` | Serves its contract. Temporal denoise and line events are not supported. Outputs 0 and 1 and statistics run on the reference board, with 200 stops each way (`D7.md`) |
| `pisp-be` | `camera.isp/1`, `isp.pisp/1` | the kernel module; `config`, `input`, `output0` streams, optional `output1` | Serves its contract. No statistics stream, no line events. Run on the reference board: the same output and round trip as the stock driver (`D6.md`) |
| `rkcif` | `camera.receiver/1` | the vendor kernel's media graph; optional `frames` stream | Serves its contract. Routes a MIPI channel and says which ISP it feeds. Frames only on the raw path, when `frames` and `capture` are set |
| `rkisp` | `camera.isp/1` | the vendor kernel's ISP nodes; `config`, `statistics`, `output0` streams | Serves its contract. No input stream: frames come from the receiver |
| `sunxi-vin` | `camera.receiver/1` | the vendor kernel's capture node; `frames` stream | Serves its contract |
| `isp600` | `camera.isp/1`, `isp.isp600/1` | the vendor kernel's nodes; `config`, `statistics`, `output0` streams | Serves its contract. No input stream. Tuning is the caller's: only the blocks in [ISP600 tuning](#isp600-tuning-what-is-public) are encoded, and the rest are unsupported |
| `k230-capture` | `camera.receiver/1` | `lemnos.ko` (`k230_isp`), or the vendor node with `dma`: `dma:BLOCK` from the kernel module; `frames` stream | Serves its contract. On the module's way it holds the ISP's frames role, so `k230isp` can configure the same ISP and read its statistics at once. The register facts it relies on are unconfirmed |
| `k230isp` | `camera.isp/1` | the vendor node, or `lemnos.ko` (`k230_isp`); `config`, `statistics` streams | Configuration and statistics only: inputs and outputs are refused, and no register that takes an address can be written. On the module's way it holds the ISP's configuration role, beside `k230-capture`'s frames. Frames through one driver go through `k230isp-frames` |
| `k230isp-frames` | `camera.isp/1` | `lemnos.ko` (`k230_isp`); `config`, `statistics`, `output0` streams | Frames from the sensor into the caller's dma-bufs, NV12 or NV16 at the input's size, with the configuration and statistics of `k230isp`. The module writes every plane's address and the size the block wraps within, and switches buffers only at a frame's end. A frame comes out when the next job takes the path. No input stream. The register facts it relies on are unconfirmed |

### ISP600 tuning: what is public

The ISP600 is `isp600` (`camera.isp/1`, `isp.isp600/1`), with its library in `lemnos-driver-isp600`.
Its load image is the caller's: the library encodes only the blocks below whose register encodings a
public source defines (`drivers/isp600/src/tuning.rs`), and refuses the words that name memory or set
how far the block reads or writes. Nothing here has run on a board (`testing/board-runs/U3.md`,
`testing/board-runs/U4.md`).

| Block | Encoding | Where it is defined |
|---|---|---|
| Channel black level and bit depths (`CH0` to `CH2` expand) | Encoded (`tuning::expand`); the setter keeps the word's other bits. Black level's value is not | `isp600_reg.h` in `radxa/allwinner-bsp` at `2045a3ca`, GPL-2.0. The library's default word is 12 bits in and out (`library_tests`) |
| Module enables | Encoded (`tuning::Image::set_modules`); reserved bits refused | `isp600_reg_cfg.h`, `isp600_reg.h`, same source, GPL-2.0 |
| Temporal denoise record enable | Encoded (`tuning::Image::set_temporal_record`), keeping the word's other bits, which the library's default sets (`library_tests`). Its buffers and the lossless group are kernel-owned | `isp600_reg.h`, same source, GPL-2.0 |
| Temporal denoise lossless group (`D3D_LBC_CFG0` to `CFG5`) | Not settable: refused as an extent word | `isp600_reg_cfg.c`, `sunxi_isp.c`, same source: derived from frame width |
| White balance gains, colour correction matrix, gamma, tone, lens shading, DRC, CEM, spatial denoise, sharpen | No encoding. The table places and sizes are public; the register words are not | Places and sizes: both sources. The register writers are only in `libisp_algo.so` of the library (below), which is not read |
| Statistics windows (AE, AF, AWB, histogram, flicker) and every statistics decode | No decode. The part sizes and offsets are public; the decode is not | Sizes and offsets: both sources. The decode (`isp_handle_stats`) is declared in the library but defined only in `libisp_algo.so`. The kernel copies the statistics as bytes (`vin_h3a.c`) |

Sources checked:

- `radxa/allwinner-bsp` at `2045a3ca2a01f088c0314dc924bda59d154e363e` (GPL-2.0 kernel). The ISP600 headers
  and `isp600_reg_cfg.c`, `sunxi_isp.c`, `vin_h3a.c`, `isp_tuning_priv.h`, `isp_comm.h`. Every usable fact above
  comes from here, and the rest of the table from the library. Its `drivers/vfe/lib/libisp_32` and `libisp_64` are
  binaries and are not read.
- `radxa/allwinner-isp6xx` at `516fd7443810e0807b289829dbae1ae5a62e0cb5` (the `main` branch, 2026-11-05), the
  vendor user-space ISP library. Used for these facts, for ISP 602, the version `scripts/mksetup.sh` gives the
  A733: `isp6xx/libisp/include/isp_comm.h` (every load, table and statistics size and offset), and
  `isp6xx/libisp/isp_manage/isp_manage.c` (`__isp_alloc_reg_tbl`, the default register words; the vectors are
  in `drivers/isp600/src/library_tests.rs`). Its register writers, its statistics decode and `isp_map_addr`
  are declared in its headers but defined only in prebuilt `out/isp602/.../libisp_algo.so`, which is not read.
  Its `out/` directories hold other prebuilt binaries too, also not read.
- `radxa/allwinner-isp6xx` `isp520/`, `isp522/` (other chips): not read for the ISP600.
- `hutchx86/freewinner` (AGPL-3.0) and other clean-room projects: not vendor sources, so not used as facts.

Licence: the library carries no open licence (its only `LICENSE` is the MIT licence of the bundled
iniparser, under `isp6xx/libisp/iniparser/`, which does not cover the library). On 2026-11-13 the owner ruled
that its facts may be transcribed here, as for every other layout library in the tree, with nothing copied
and nothing linked or vendored. The owner will revisit this before anything is distributed.


## Image blocks

| Driver | Provides | Needs | State |
|---|---|---|---|
| `rga` | `image.scaler/1`, `image.convert/1`, `image.blend/1` | optional `scale`, `convert`, `blend` streams; `soc` | Serves its contract. Nothing is computed in software: an operation the hardware route lacks is `NOT_SUPPORTED` |
| `g2d` | `image.convert/1` | `jobs` stream of dma-bufs | Serves its contract |
| `rkvpss` | `image.scaler/1` | `jobs` stream of dma-bufs | Serves its contract |
| `nonai2d` | `image.convert/1` | `jobs` stream of dma-bufs; optional `device` | Serves its contract. One size, one plane: no scaling, and the 4:2:0 formats only through the library with a dma-buf per plane |
| `vglite` | `image.scaler/1`, `image.convert/1` | optional `scale`, `convert` streams of dma-bufs, at least one | Serves its contract. Every image is copied through the GPU's own staging memory, since its kernel can't be trusted with a dma-buf in pieces. The area filter only halves. Not `image.blend/1`: its blend is straight alpha only |

## Codec blocks

| Driver | Provides | Needs | State |
|---|---|---|---|
| `rk-mpp` (crate `rk-mpp-codec`) | `codec.block/1` | `jobs` stream of dma-bufs | Serves its contract. The caller writes the register set; the driver checks every descriptor, offset and length. JPEG blocks included |
| `rk-iep` | `codec.block/1` | `jobs` stream of dma-bufs | Serves its contract |
| `v4l2-stateless` (crate `m2m-decode`) | `codec.block/1` | `jobs` stream of dma-bufs | Serves its contract. One media request per job, with the caller's controls as bytes. H.264 (`S264`) and HEVC (`S265`) take the same job shape. Slots: 0 is the bitstream, 1 and up the picture's planes; a reference is named by its stamp in the controls, not by a slot, and a stamp is a whole number of microseconds (see the crate's `stateless` module, "References") |
| `a733-di` | `codec.block/1` | `jobs` stream of dma-bufs | Serves its contract |
| `k230-vpu` | `codec.block/1` | `jobs` stream of dma-bufs | Serves its contract. Some of the node's private requests are refused |
| `a733-video` | `codec.block/1` | `jobs` stream | With `route = "module"`, encode jobs as register writes and dma-bufs, which `lemnos.ko` checks and runs: it writes every address register itself and refuses a range that doesn't cover what the job's own geometry makes the engine reach. On the vendor route (the default) register reads only, because an address written into a register cannot be bounded from user space. A decode job is refused: the module allows the H.264 encoder's registers only |

The A733 encoder's records and values (`drivers/a733-video/src/encoder`, `lemnos-
driver-a733-video`'s `encoder` module) are the per-macroblock QP map, the per-CTU info word and the
codec, rate-control, pixel, H.264 profile and level, buffer flag and slice values, from the header
files of `mashiqi/A733-Cedarc` at `2e07a43cff96903e962e6ff2f83da6def6cb3d9c` (`vendor/headers/`).
The headers define no register offset or bit position for the encoder, so the encoder builds no job
from them. The H.264 register facts the kernel module uses come from the open sources
`kernel/README.md` names. Those files carry no open licence (that repository's `vendor/README.md`
states it has no written redistribution grant), and the user will revisit that before anything is
distributed. This note is the only place it is recorded, and it is not a gate.

## Neural accelerators

| Driver | Provides | Needs | State |
|---|---|---|---|
| `rknpu` | `npu/1` | the vendor kernel's NPU render node (`node`, or found by its driver); `jobs` stream | Serves its contract. Each program runs in an IOMMU domain of its own that holds only its objects, checked before every run. A program belongs to the caller that loaded it and is unloaded when that caller goes. Confinement holds only while the daemon is the node's sole opener, and whether a program can reach the IOMMU's own registers is not known |
| `vip9000` | `npu/1` | the block driven by `lemnos.ko` (`route = "lemnos"`, its `device` name and a dma-`heap`); `jobs` stream | Serves its contract through the module. While a job runs, the block's MMU maps the module's copy of its program (read-only) and its slots, and nothing else; every program is checked once in the kernel; a fault or a run out of time resets the block and fails the run. Buffers are copied to and from the slots. `route = "vipcore"`, the vendor's node, binds only to refuse every load. Untested on board |

## Library crates

A board file cannot name these.

| Crate | What it is | Why it is not a driver |
|---|---|---|
| `k230-kpu` | `npu/1`'s methods over a device trait, with the checks | No Linux device is built, and `load` answers `NOT_SUPPORTED`. The block has no IOMMU and no bus protection, so a job cannot be confined at all without reading the program |
| `k230-ai2d` | The AI2D block's kernel interface, and checked job logic over a device trait | No hardware path: nothing implements the device trait on Linux, because the block's register map is not in the public sources. It serves no contract |
| `k230-gsdma` | Job checks, descriptors, and a Linux device on the engine's node in `lemnos.ko` | No contract driver wraps it. Untested on board |
| `rkaiisp` | The AI-ISP's layouts, parameter checks and ring | Never run on the block. No contract |
| `rk-jpeg` | The JPEG block's register and address model, the decoder's and the encoder's (the encoder's job is built by `lemnos-driver-rkjpege`, re-exported as `encoder`) | The decoder builds no job: no public source says how far its block reaches from each address. JPEG decodes go through `rk-mpp` with the caller's registers. Its tests run the encoder's job on the played kernel |
| `rk-mpp` | The MPP service's checked jobs: a job writes up to 8 runs, each inside one register window of the block. The first-generation encoder has two windows (780 words translated, and 320 raw at word 0x4000). The second-generation encoder (`rk-mpp:rkvenc2`) has its register classes, with only the picture class translated and its OSD class not a window (a job may not name it) | Used by `rk-mpp-codec` and `rk-iep` |
| `codec-jobs` | The job server every `codec.block/1` driver shares | A building block |
| `v4l2-capture` | A capture node that fills a stream's own buffers | Used by `rkcif` and `sunxi-vin` |
| `soc` | Chip description tables | Data |

## Which drivers take a block's own resources

A driver that programs a block itself (writes its registers, takes its interrupt, turns its
clocks on) could take those from the host by the board file's names `regs:`, `irq:`, `clocks:` and
`resets:` (see [Board files](boards.md#resource-names)). On Linux they are the
[kernel module](kernel-module.md)'s, with the block bound to it. No built-in driver takes them
now.

The K230 ISP's drivers reach the ISP through the module's register jobs instead, when the board
file gives them the ISP's platform device as `block` in `config`: the module checks every write
in the kernel and writes every address itself. One ISP serves two roles at once there.
`k230-capture` holds its frames, and `k230isp` its configuration and statistics; the module
merges the bits of the register both write (`isp_ctrl`) and hands each its own interrupt causes.
`k230isp-frames` holds both roles itself. Without `block` each takes its vendor node, and giving
`block` with a vendor setting is refused when the device binds. None falls back from one way to
the other.

The other block drivers keep the kernel driver's own interface, because there the kernel driver
programs the block and Lemnos carries the caller's work to it: the V4L2 and media-graph drivers,
the codec and NPU command services, and `a733-video`, which only reads registers through its
vendor node and writes none. `rp1-csi2`, `pisp-fe`, `pisp-be` and the graphics DMA engine under
`k230-gsdma` are driven inside the module, because their DMA must be checked there, so their
registers never reach user space.

Crates named `drivers/<name>-sys` hold a driver's kernel interface: structures, request numbers
and the `unsafe` that issues them. They are not drivers.

## Used as libraries

Other programs take these crates as plain libraries, with no `lemnosd`, no host and no driver
runtime. Each builds and tests alone with its default features off, and then its dependency tree
holds no part of the engine: not `lemnos-core`, its macros, `lemnos-hal`, a contract, the client
or the host. `scripts/check-libraries.sh`, a step of the gate, builds, lints and tests each one
that way and checks its tree with `cargo tree -e normal`.

| Crate | What it is | With no default features |
|---|---|---|
| `lemnos-v4l2` (`backends/v4l2`) | V4L2 video nodes, subdevices, the media controller and events | Everything; it has no driver half |
| `lemnos-driver-pisp` | The PiSP's uAPI layouts, front and back end builders, tiling and statistics decoding | No driver: the front and back end drivers are behind `device` |
| `lemnos-driver-pisp-sys`, `lemnos-driver-pisp-fe-sys` | The PiSP back and front end kernel interfaces | Everything |
| `lemnos-driver-rkisp` | The rkisp parameter and statistics layouts (as `uapi`), the generation table and the ISP path in the media graph (`graph`) | No driver and no job checks: `device` adds them |
| `lemnos-driver-rkisp-sys` | The ISP21, ISP30 and ISP35 layouts | Everything |
| `lemnos-driver-k230isp` | The configuration layout (register writes) and its check, the statistics layout and its decoder, the register map | No driver and no `hw` layer: `device` adds them |
| `lemnos-driver-k230isp-sys` | The vvcam-isp device's register requests, bounded to the registers that carry no address | Everything |
| `lemnos-driver-isp600` | The load image's and statistics buffer's layouts, and the load image's check | No driver and no job checks: `device` adds them |
| `lemnos-driver-isp600-sys` | The ISP and statistics subdevice requests | Everything |
| `lemnos-driver-rk-mpp` (`drivers/rk-mpp`) | The `/dev/mpp_service` uAPI, the message and job layouts and their checks: `Job`, `Address`, `BufferId`, the request numbers and client types | The uAPI and the job checks. `device` (on by default) adds the claim on the service and the `codec.block/1` block |
| `lemnos-driver-rkvenc` (`drivers/rkvenc`) | The Rockchip H.264 and H.265 encoder register layouts: the VEPU541 H.264 and H.265 write sides, read-backs and second windows, and the VEPU580 H.264 register classes (`vepu580_h264`, nine classes; the OSD class is left out, see its module doc), and a builder that makes rk-mpp's `Job` from the caller's values, with each address register checked for the header's alignment. The second window (L2, word 0x4000, 320 words) is set with `set_l2` and goes out as its own write run | Everything; the builder takes rk-mpp's job types, and nothing else is linked |
| `lemnos-driver-rkvenc` (`drivers/rkvenc`) | The Rockchip H.264 and H.265 encoder register layouts: the VEPU541 H.264 write side, read-back and second window, and the VEPU580 H.264 register classes (`vepu580_h264`, nine classes; the OSD class is left out, see its module doc), and a builder that makes rk-mpp's `Job` from the caller's values, with each address register checked for the header's alignment. The second window (L2, word 0x4000, 320 words) is set with `set_l2` and goes out as its own write run | Everything; the builder takes rk-mpp's job types, and nothing else is linked |
| `lemnos-driver-rkjpege` (`drivers/rkjpege`) | The Rockchip JPEG encoder register file (the VPU720 class, `rk-mpp:jpeg-encoder`, 79 words) pinned to the vendor header by gcc and clang, and a builder that makes rk-mpp's `Job`: the picture and its planes checked against the buffers the caller holds, the bitstream pointers checked for order, and each address register checked for the header's alignment. The ECS length buffer is not placed | Everything; the builder takes rk-mpp's job types, and nothing else is linked |
| `lemnos-linux-hotplug` (`backends/linux-hotplug`) | Devices appearing and going away: netlink uevents, inotify or rescans, in the kernel's uevent shape | Everything. `lemnos-linux` re-exports it as `hotplug` and adds the host's board hints |
| `lemnos-linux-stream` (`backends/linux-stream`) | A frame stream's descriptors received over `SCM_RIGHTS`, and `DMA_BUF_IOCTL_SYNC` around each access to a dma-buf | The `StreamPlatform` `lemnos-client` takes. The rings and buffers are mapped by the shm crates, which need no platform |
| `lemnos-wire` (`engine/wire`) | The frame stream's shared ring | Only `ring`. `protocol` (on by default) adds the socket protocol |

Every driver half is on by default, so the workspace, `lemnosd` and a plain dependency get the
drivers as before. A program that wants the library alone names the crate with
`default-features = false`.

## Contracts

One crate each under `contracts/`. The numbers are the method numbers on the wire.

| Tag | Methods | Served by |
|---|---|---|
| `accelerometer/1` | 1 read, 2 read_batch | `bmi088`, `generic-accelerometer`, `modbus-accelerometer` |
| `gyroscope/1` | 1 read, 2 read_batch | `bmi088`, `generic-gyroscope`, `modbus-gyroscope` |
| `magnetometer/1` | 1 read | `bmm150`, `generic-magnetometer`, `modbus-magnetometer` |
| `orientation/1` | 1 read | `orientation` |
| `temperature/1` | 1 read | `thermal-zone`, `generic-temperature`, `modbus-temperature` |
| `power-monitor/1` | 1 read | `ina2xx`, `generic-power-monitor`, `modbus-power-monitor` |
| `power-switch/1` | 1 set, 2 is_on, 3 fault | `gpio-power-switch` |
| `fan/1` | 1 set_duty, 2 duty, 3 rpm, 4 release | `hwmon-fan` |
| `fan-control/1` | 1 state, 2 settings, 3 set_settings, 4 curve, 5 set_curve, 6 hold, 7 release_hold, 8 set_enabled | `fan-control` |
| `pixels/1` | 1 pixel_count, 2 set, 3 show | `ws2812` |
| `light/1` | 1 layout, 2 show, 3 clear, 4 clear_all, 5 set_brightness | `status-light` |
| `indicator/1` | 1 set, 2 states | `indicator` |
| `self-test/1` | 1 tests, 2 run (job), 3 last | `hwmon-fan`, `bmm150`, `ina2xx`, `indicator`, `hw-watchdog`, `board-self-test` (add-on: see [Self-tests](#self-tests)) |
| `lens/1` | 1 range, 2 set_position, 3 position, 4 direction, 5 settle_us | `vcm` |
| `raw-bus/1` | 1 i2c_transfer, 2 spi_transfer, 3 line_get, 4 line_set, 5 pwm_set, 6 uart_write, 7 uart_read | `raw-bus` |
| `gpu-lease/1` | 1 acquire, 2 poll, 3 release, 4 hang, 5 state, 6 last_hang | `gpu-lease` |
| `camera.sensor/1` | 1 info, 2 mode, 3 select_mode, 4 selected, 5 start, 6 stop, 7 set_controls, 8 post_values, 9 applied, 10 set_flips, 11 set_test_pattern, 12 identity, 13 post_values_at, 14 applied_landing, 15 in_force, 16 set_exposure, 17 set_analogue_gain, 18 set_frame_length | `ov9782` |
| `camera.receiver/1` | 1 limits, 2 configure, 3 link, 4 frame_stream, 5 start, 6 stop, 7 counters, 8 feeds, 9 embedded_stream, 10 source, 11 layout, 12 frame_starts | `rp1-csi2`, `rkcif`, `sunxi-vin`, `k230-capture` |
| `camera.isp/1` | 1 info, 2 config_layout, 3 set_input, 4 set_output, 5 clear_output, 6 config_stream, 7 statistics_stream, 8 input_stream, 9 output_stream, 10 start, 11 stop, 12 counters, 13 input_layout, 14 output_layout | `pisp-be`, `pisp-fe`, `rkisp`, `isp600`, `k230isp`, `k230isp-frames` |
| `isp.pisp/1` | 1 info, 2 set_front_end_compression, 3 enable_temporal_denoise, 4 reset_temporal_denoise, 5 set_line_event | `pisp-be`, `pisp-fe` |
| `isp.isp600/1` | 1 info, 2 load_region, 3 statistics_region | `isp600` |
| `camera.frames/1` | 1 format_count, 2 format, 3 select_format, 4 selected, 5 frame_stream, 6 start, 7 stop, 8 counters | `uvc` |
| `image.scaler/1` | 1 info, 2 format_count, 3 format, 4 job_stream, 5 counters | `rga`, `rkvpss`, `vglite` |
| `image.convert/1` | 1 info, 2 conversion_count, 3 conversion, 4 job_stream, 5 counters | `rga`, `g2d`, `nonai2d`, `vglite` |
| `image.blend/1` | 1 info, 2 format_count, 3 format, 4 job_stream, 5 counters | `rga` |
| `codec.block/1` | 1 info, 2 block, 3 job_stream, 4 counters | `rk-mpp`, `rk-iep`, `v4l2-stateless`, `a733-di`, `k230-vpu`, `a733-video` |
| `npu/1` | 1 info, 2 job_stream, 3 load, 4 unload, 5 attach, 6 detach, 7 counters | `rknpu` |
| `usb.function/1` | 1 info, 2 endpoint, 3 data_stream, 4 state, 5 control_in, 6 control_out, 7 control_stall, 8 set_halt, 9 cancel, 10 counters, 11 ifname | `usb-gadget` |
| `system.flash/1` | 1 info, 2 manifest, 3 write (a long job), 4 data, 5 verify (a long job) | `system-flash` |
| `system.slots/1` | 1 list, 2 install (a long job), 3 write, 4 set_default, 5 trial, 6 confirm, 7 remove, 8 pin, 9 set_keep, 10 accept, 11 install_file (a long job), 12 boots (the boot history) | `system-slots` |
| `system.reboot/1` | 1 reboot | `system-reboot` |
| `lemnos.files/1` | 1 index, 2 open, 3 size, 4 read, 5 close | `named-files` |

A camera is not one contract. A consumer finds a `camera.sensor/1` device and follows the board
file's `link` to its receiver and from there to its ISP. A USB camera is one device with
`camera.frames/1`.

The sensor contract's timing, gain law and read-back rules are in
[`contracts/camera-sensor`](../contracts/camera-sensor/src/lib.rs). In short: a post's controls
show on the frame `F + delay` for each control, where `F` is the frame in progress when its group
launches. Frames are named by sequence (receivers publish identity 0). `ov9782` has no embedded
data, so a caller checks its posts from the timing rule. The simulated sensor in the camera proof
has configurable delays, masked frames and embedded data, and its schedule test checks a 1,000-step
run with unequal delays.

Confirming a link. `camera.receiver/1` `source` says which sensor its input is wired to, as the
kernel names it; `camera.sensor/1` `identity` says what the kernel calls that sensor. The sensor
driver asks the receiver its `source` at bind (its board `link` is the receiver) and compares the
answer with its `identity`: a receiver with nothing wired to its input fails the sensor (`Failed`),
and an answer the sensor can't match, or none, leaves it `Degraded` with the reason. Only a match is
a pass. Receivers answer `source` from the media graph (`sunxi-vin`, `rkcif`) and answer unknown
where no kernel describes the input (`rp1-csi2`, `k230-capture`). The `ov9782` sensor has no
kernel name, so its link is always `Degraded` on today's boards.

Exposure, white balance, tuning and the rest of a camera pipeline are the caller's. So is every
codec's bitstream work and every model: the contracts carry the caller's bytes to the block.

## Ranges, steps and limits of the built-in items

Every item a built-in driver describes, with what it knows of its unit, range, step and warning
limits, and where each figure comes from. A figure is one of:

- **chip**: a constant of the chip or of the driver's scale (a register's full scale, a per-mille
  duty, a code table), in the driver;
- **board**: a key of the device's `config` in the board file (named in the row), which the board
  states once and the description carries;
- **bind**: read from the device or its sysfs when the driver binds (named in the row);
- **unknown**: no source. The reason is in the row. An unknown figure is left out, never guessed.

A value that the driver reads as a float (`f32`) has no integer raw scale, so its range is given
only where the bounds are exact in the item's unit (a temperature in degrees, a duty in per mille);
a float value whose bounds need a finer unit than the read returns is left unknown.

| Driver | Item | Unit | Range | Step | Warning limits | Source |
|---|---|---|---|---|---|---|
| `ina2xx` | `bus_voltage` | V (nano) | 0 to the chip's bus full scale | the bus LSB | `bus_warn_below_v`, `bus_warn_above_v` (board) | chip for the range and step (`chip.rs`); board for the limits |
| `ina2xx` | `shunt_voltage` | V (nano) | ±the shunt full scale; absent for `ina260` and for an attached chip | the shunt LSB | none | chip, from board `shunt_ohms`/`shunt_micro_ohms` and `max_current_a` |
| `ina2xx` | `current` | A (nano) | ±the current full scale | the current LSB | `current_warn_above_a` (board) | chip, from board `max_current_a` (`ina260`: fixed 1.25 mA) |
| `ina2xx` | `power` | W (nano) | 0 to the power full scale | the power LSB | none | chip, from the current LSB (`ina260`: fixed 10 mW) |
| `bmi088` | `acceleration.x/y/z` | m/s² | ±the full scale of `accel_range` (default 6 g) | the scale per count, rounded | none | chip (`chip.rs`), range from board `accel_range` |
| `bmi088` | `angular_rate.x/y/z` | rad/s | ±the full scale of `gyro_range` (default 2000 dps) | the scale per count, rounded | none | chip (`chip.rs`), range from board `gyro_range` |
| `bmm150` | `magnetic_field.x/y/z` | T | unknown: the chip's ±1300 µT (x, y) and ±2500 µT (z) cannot be an integer bound in tesla, and the read is a float | none | none | unknown: the read is in tesla as `f32`; a range needs a finer unit than the read gives |
| `generic-sensor` (every kind, I2C and Modbus) | the kind's items (`acceleration`, `angular_rate`, `magnetic_field`, `temperature`, `bus_voltage`, `shunt_voltage`, `current`, `power`) | the SI unit | unknown | none | none | unknown: the reads are `f32` in the SI unit and no board key gives a range |
| `orientation` | `roll`, `pitch`, `yaw` | rad | unknown | none | none | unknown: the fused attitude is a float in radians; no integer bound fits its unit |
| `fan-control` | `duty`, `demand` | per mille | 0 to 1000 | 1 | none | chip: the per-mille scale of the fan law (`law::FULL`) |
| `hwmon-fan` | `rpm` | rpm | 0 to `max_rpm` (board) | 1 | `warn_below_rpm` (board, a stall) | board for the range and the limit; the tachometer counts whole rpm |
| `hwmon-fan` | `duty` | per mille | 0 to 1000 | 1 | none | chip: the per-mille scale (`MAX_DUTY`) |
| `pwm-channel-fan` | `rpm` (with a tachometer) | rpm | 0 to `max_rpm` (board) | 1 | `warn_below_rpm` (board, a stall) | board, as for `hwmon-fan` |
| `pwm-channel-fan` | `duty` | per mille | 0 to 1000 | 1 | none | chip: the per-mille scale (`FULL`) |
| `thermal-zone` | `temperature` | °C | `min_c` to `max_c` (board) | none | `warn_below_c`, `warn_above_c` (board) | board: the zone's trusted range and the limits; the zone's own trip points are not read |
| `hw-watchdog` | `timeout` | s | the device's `min_timeout` to `max_timeout` | 1 | none | bind: read from sysfs when the driver binds |
| `hw-watchdog` | `timeleft` | s | 0 to the device's `max_timeout` | 1 | none | bind: read from sysfs when the driver binds |
| `ov9782` | `exposure` (value and control) | line | 1 line to the selected mode's frame length less 25 lines; unknown before a mode is selected | 1 | none | chip: the mode table (`MODES`) and `EXPOSURE_MARGIN` |
| `ov9782` | `frame_length` (value and control) | line | the selected mode's frame length min to max; unknown before a mode is selected | 1 | none | chip: the mode table |
| `ov9782` | `analogue_gain` (value and control) | (none) | `0x10` to `0xff` (16 to 255), the codes in every mode | 1 | none | chip: `ANALOGUE_GAIN_RANGE` |
| `vcm` | `position` | (none) | `min` (board, default 0) to `max` (board, default 2^bits − 1 of the chip) | 1 | none | board for `min` and `max`; chip for the default (1023 or 4095) |
| `gpio-power-switch` | `power.on`, `power.fault` | switch | (none) | none | none | a switch: no figure |
| `host-clock` | `clock_source`, `rtc`, `rtc_hctosys` | switch or text | (none) | none | none | bind: kernel attributes (no figure) |
| `system-identity` | `serial`, `family`, `model`, `facts` | text | (none) | none | none | bind and board: the serial and the board's name (no figure) |
| `ws2812` | `pixels` | (none) | (none) | none | none | a count of pixels (board `count`); the item has no range and no step, and its reading is the number of pixels |
| `status-light` | `layout` | (none) | (none) | none | none | a structure of `pixels` (board `count`) and a constant of four layers; no figure of its own |
| `uvc` | `frames`, `dropped`, `errors` | (none) | unknown: the counters have no bound | 1 | none | unknown: counted by the driver since the stream started |
| `gpu-lease` | `state` | (none) | (none) | none | none | the driver's own state (no figure) |

The drivers whose items are only `start` and `stop` (actions) carry no figure: `isp600`,
`k230-capture`, `k230isp` (both items), `ov9782` (its actions), `pisp` (both blocks), `rkcif`,
`rkisp`, `rp1-csi2`, `sunxi-vin`. The drivers that describe no item at all are `indicator`, the
image and codec blocks (`a733-di`, `a733-video`, `g2d`, `m2m-decode`, `rga`, `rk-iep`, `rk-mpp-codec`,
`rknpu`, `rkvpss`, `nonai2d`, `vglite`, `vip9000`, `k230-vpu`), `coral-loader`, `raw-bus`,
`system-slots` (its two drivers) and `usb-gadget`. Their figures are the caller's, or the devices'
configuration, not an item.

Limits without a board key, by design: no built-in driver gives a warning limit unless the board
says so. The board keys are `max_rpm` and `warn_below_rpm` (`hwmon-fan`, `pwm-channel-fan`),
`min_c`, `max_c`, `warn_below_c` and `warn_above_c` (`thermal-zone`), and `bus_warn_below_v`,
`bus_warn_above_v` and `current_warn_above_a` (`ina2xx`). Each is optional; an absent key is no
figure.
## Self-tests

`self-test/1` (`contracts/self-test`) names checks a device runs on itself. A driver serves it beside
its own contract, and the built-in `board-self-test` serves it for a whole board. A UI lists the
tests, starts one, and reads what each last found, without knowing the driver. Nothing persists
across a restart. The contract carries no `kind`: it is an add-on, and the device keeps its own kind.

| # | Method | Arguments | Result |
|---|---|---|---|
| 1 | `tests` | an out buffer | a count, then one test record each |
| 2 | `run` (long job) | `name: &str`, `confirmed: bool` | a job; its progress is milliseconds of the test's `expect_ms`; its result is one result record |
| 3 | `last` | an out buffer | a count, then each test that has a result: its name and that result |

`run` answers `INVALID_ARGUMENT` for a name the device does not list, and `BUSY` while another test
runs, or for an intrusive test while another caller uses the device (refused before anything runs).
`confirmed` is the operator's word for a test that needs one. A cancelled test ends `CANCELLED`, and
`last` keeps the result it had. A job the driver loses ends `INTERRUPTED`.

**Names.** A test name is `[a-z0-9_]`, 1 to 32 bytes, and never `all` (the board's). The set and the
names are stable for a driver version, so a manufacturing flow may store them.

**Layouts.** Little-endian, fixed width, no padding. Text is a length byte then its field, zero padded.

| Record | Bytes | Fields |
|---|---|---|
| test (in `tests`) | 249 | name (80), description (96), intrusive (u8: 1 changes what the device does while it runs), operator (u8: 1 needs an operator to confirm), fixture (64), `expect_ms` (u32, a bound for a UI, not a promise) |
| result (`run`'s result, and the body of each `last` entry) | 373 | outcome (u8: 0 pass, 1 fail, 2 skipped, 3 not-supported), reason (u16), detail (96), measure count (u8, 0 to 4), four measure slots of 68 bytes |
| measure | 68 | name (32), value (i64), exponent (i8: the value is `value * 10^exponent` in the unit), unit (8), low and high limits (i64 each), flags (u8: bit 0 low is set, bit 1 high is set) |
| `last` entry | 454 | name (80), then a result |

A list is a count (u8), then its entries. A device lists at most 8 tests (`MAX_TESTS`). A board's
list names up to 16 members' tests and `all`, so at most 129 entries (`MAX_LISTED`). The sizes are
pinned by the contract's tests. The largest reply of each method is in the bridge's table in [`daemon.md`](daemon.md).

**Reasons.** The codes are numbered and never reused.

| Code | Name | Outcome | Meaning |
|---|---|---|---|
| 0 | `OK` | pass | within limits, or answered as expected |
| 1 | `OPERATOR_CONFIRMED` | pass | an operator confirmed what the test shows |
| 2 | `NEEDS_OPERATOR` | skipped | run it again with `confirmed = true` |
| 3 | `NOT_CONFIGURED` | skipped | the board file gives no setting the test needs |
| 4 | `ABSENT` | skipped | the hardware the test needs is not there, or the member is not running |
| 5 | `IN_USE` | skipped | (a board only) a member refused its start because another caller uses it |
| 6 | `NO_RESPONSE` | fail | a read or write of the device failed |
| 7 | `IDENTITY` | fail | the chip answered with another identity |
| 8 | `OUT_OF_RANGE` | fail | a measured value is outside its limits |
| 9 | `RESTORE_FAILED` | fail | the device could not be put back as found; retried |
| 10 | `UNKNOWN_TEST` | not-supported | the device does not offer that test |
| 11 | `READ_ONLY` | skipped | the device is the operating system's, and the test would write |

**The rules.** `lemnos-contract-self-test` keeps them in one place (`Runner`), and each driver's tests
show them on the simulator.

- A test leaves the device as it found it. It reads what it will change first, and puts it back on
  every end: pass, fail, skip and cancel. A put-back that fails is retried on the ticks with backoff
  capped at 8 s, the device reports `Degraded` meanwhile, and the answer is a fail with
  `RESTORE_FAILED` (a test's own failure keeps its reason).
- Cancelling stops the test and starts the put-back at once; the job answers `CANCELLED` from then.
- An intrusive test is refused with `BUSY` while another caller uses the device. A caller counts as
  using it once it has called the device and until it goes; a driver it drives (a fan under a
  controller) counts too. A read-only test is never refused for use.
- While an intrusive test runs, the driver refuses its own normal changes with `BUSY` (a fan's duty,
  an indicator's state), so no write is overwritten by a put-back.
- A missing fixture, setting, sensor or operator is `skipped`, with its reason. A `fail` means the
  device was reached and was wrong, or a read failed.
- A tick before a step is due takes no step. With no test running a tick costs nothing and allocates
  nothing.

**Driver-to-driver jobs.** A driver may start a long job on a device it uses: `LocalCaller` routes
`start_job`, `poll` and `cancel` through the host's router. The job belongs to the starter (its
device-owned caller number, `CallerId::device`), and only it polls or cancels it. Its progress and
result go to the starter, never to a client. When the starter's slot ends, its jobs are cancelled on
the device they run on. A package driver is given `NOT_SUPPORTED` for these calls.

**Served by.** Each test's cost is its `expect_ms` bound on the simulator; the fan's settle is fixed
at 3 s.

| Driver | Tests | Board-file keys |
|---|---|---|
| `bmm150` | `identity`: the chip ID register reads `0x32` (non-intrusive) | none |
| `ina2xx` | `bus_voltage`: the bus voltage is in the range (non-intrusive) | `self_test_bus_min_mv`, `self_test_bus_max_mv` (both, or the test is skipped `NOT_CONFIGURED`) |
| `hwmon-fan` | `spin` (intrusive): sets the test duty, waits 3 s, reads the tachometer, puts back the duty and the `pwm1_enable` mode it found. A fan with no tachometer is skipped `ABSENT` | `self_test_duty` (per mille, 1 to 1000, default 500), `self_test_rpm_min`, `self_test_rpm_max` (both, or skipped `NOT_CONFIGURED`) |
| `indicator` | `lamp` (intrusive, operator): shows `receiving` for 2 s, then puts back the state it had | none; `confirmed = true` runs it, else skipped `NEEDS_OPERATOR` |
| `hw-watchdog` | `answers` (non-intrusive): reads the device's timeout; feeds and changes nothing | none |
| `board-self-test` | the members' tests as `member:test`, and `all` | `members`, `include_intrusive` (see [Boards](boards.md#the-self-test-of-a-board)) |

The camera sensor (`ov9782`) serves no self-test yet: its identity and test-pattern checks need its
streaming state, which this change does not touch.

