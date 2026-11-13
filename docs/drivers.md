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
| `bmm150` | `magnetometer/1` | `bus`: I2C | Serves its contract. Reads only on a host-owned device |
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
| `ina2xx` | `power-monitor/1` | `bus`: I2C; `chip`: `ina226`, `ina238` or `ina260` | Serves its contract |
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
| `pwm-channel-fan` | `fan/1` | `pwm`: a PWM channel, and optional `tach`: a GPIO line | Serves its contract. Drives the PWM directly, with no kernel fan driver between. Full duty at start, on release, and when stopped |
| `hwmon-fan` | `fan/1` | a hwmon node, by `name` or `hwmon` | Serves its contract, for a fan the kernel exposes through hwmon. Takes the fan over at the first `set_duty` and gives it back on release. A killed daemon leaves the last duty. Not a safety net: the controller keeps the fan safe |
| `thermal-zone` | `temperature/1` | `path`: a thermal zone | Serves its contract. Run on the reference board (`B13.md`) |
| `gpu-lease` | `gpu-lease/1` | nothing | Serves its contract. The lease is advisory: nothing is enforced on the GPU |
| `host-clock` | no contract | nothing | Runs. Shows the kernel's clock source and whether an RTC set the clock, as values |
| `hw-watchdog` | no contract | `device`: a watchdog node, and `keepalive = true` on the device | Sets the timeout (`timeout_s`, default 15 s, clamped to the device's range) and reads it back. Feeds the node at half the timeout, only while the daemon is healthy (see [If the daemon dies](daemon.md#if-the-daemon-dies)). Never writes the magic close |

## The board's own system

| Driver | Provides | Needs | State |
|---|---|---|---|
| `system-slots` | `system.slots/1` | nothing; `data` and `boot` partitions, `record`, `keys`, `boot_args` in `config` (`docs/slots.md`) | Serves its contract. Installs a slot bundle as a long job, streamed or from a file in `.incoming`, checked against its schema 3 manifest and refused when built for another board (the board file's `[board] id`); a signature is always checked and shown, and `require_signed` refuses bundles no trusted key signed (off by default). Sets the default, boots a slot on trial, confirms, removes and keeps the last few. Never over the running or the default slot. Untested on board (`I5.md`) |
| `system-identity` | `system.identity/1` | nothing; `serial` source, `name`, `revision`, `data` in `config`; the family is the board's `[board] id` | Serves the board's identity facts: `serial`, `family`, `model` and the running slot's `version.slot`, `version.product`, `version.os`. Binds fail when a serial source cannot be read, so the host retries. Untested on board (`P1.md`) |
| `system-reboot` | `system.reboot/1` | nothing | Serves its contract. Runs `systemctl reboot` |

## USB

| Driver | Provides | Needs | State |
|---|---|---|---|
| `uvc` | `camera.frames/1` | `usb`: a USB device; `frames` stream | Serves its contract. Keeps 4 isochronous transfers queued while streaming. A frame larger than `frame_bytes` is dropped and counted. An `[[each]]` entry matching `usb:class:0e` binds it to every camera plugged in |
| `coral-loader` | no contract | `usb`: a USB device; `firmware`: a file | Runs. Loads the firmware file the board file names over DFU, then resets the device. It never reads the firmware |
| `usb-gadget` | `usb.function/1` | `function`: `udc:NAME`; optional `data` stream | Serves its contract. A caller serves the endpoints, isochronous ones in alternate settings among them |

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
licensed public source defines (`drivers/isp600/src/tuning.rs`), and refuses the words that name
memory or set how far the block reads or writes. Nothing here has run on a board (`testing/board-runs/U3.md`).

| Block | Encoding | Where it is defined, and licence |
|---|---|---|
| Channel black level and bit depths (`CH0` to `CH2` expand) | Encoded (`tuning::expand`). Black level's value is not | `isp600_reg.h` in `radxa/allwinner-bsp` at `2045a3ca`, GPL-2.0 |
| Module enables | Encoded (`tuning::Image::set_modules`); reserved bits refused | `isp600_reg_cfg.h`, `isp600_reg.h`, same source, GPL-2.0 |
| Temporal denoise record enable | Encoded (`tuning::temporal`). Its buffers and the lossless group are kernel-owned | `isp600_reg.h`, same source, GPL-2.0 |
| Temporal denoise lossless group (`D3D_LBC_CFG0` to `CFG5`) | Not settable: refused as an extent word | `isp600_reg_cfg.c`, `sunxi_isp.c`, same source: derived from frame width |
| White balance gains, colour correction matrix, gamma, tone, lens shading, DRC, CEM, spatial denoise, sharpen | No encoding. Only table regions and their update bits are public | The kernel's `isp_tuning_priv.h` gives the caller's tuning structure's field types, not the registers. Register writers are in the vendor's user-space library (below), which has no licence |
| Statistics windows (AE, AF, AWB, histogram, flicker) and every statistics decode | No encoding. The kernel copies the statistics as bytes (`vin_h3a.c`) | Same kernel source. Decodes are only in the vendor's user-space library |

Sources checked for the rest, and why none of them is used:

- `radxa/allwinner-bsp` at `2045a3ca2a01f088c0314dc924bda59d154e363e` (GPL-2.0 kernel). The ISP600 headers
  and `isp600_reg_cfg.c`, `sunxi_isp.c`, `vin_h3a.c`, `isp_tuning_priv.h`, `isp_comm.h`. Every usable fact above
  comes from here. Its `drivers/vfe/lib/libisp_32` and `libisp_64` are binaries and are not read.
- `radxa/allwinner-isp6xx` at `516fd7443810e0807b289829dbae1ae5a62e0cb5` (the `main` branch,
  2026-11-05). It is the vendor user-space ISP library, with `isp6xx/libisp/isp_tuning/isp_tuning.c`,
  `isp_manage/isp_manage.c`, `include/isp_module_cfg.h` and `include/isp_comm.h`, which hold the tuning
  register writers and statistics decodes. The repository has no licence file at its root. Its only
  `LICENSE` is the MIT licence of the bundled iniparser, under `isp6xx/libisp/iniparser/`, which does
  not cover the library. Its files carry Allwinner copyright headers without a licence grant. So it is
  not used. Its `out/` directories hold prebuilt `libAWIspApi` binaries, which are not read either.
- `radxa/allwinner-isp6xx` `isp520/`, `isp522/` (other chips): the same repository's user-space for
  other ISPs, not read for the ISP600's encodings.
- `hutchx86/freewinner` (AGPL-3.0) and other clean-room projects: not vendor sources, so not used as facts.


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
| `rk-jpeg` | The JPEG block's register and address model | Builds no job: no public source says how far the block reaches from each address. JPEG jobs go through `rk-mpp` with the caller's registers |
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
| `lemnos-driver-rkvenc` (`drivers/rkvenc`) | The Rockchip H.264 and H.265 encoder register layouts: the VEPU541 H.264 write side, read-back and second window, and the VEPU580 H.264 register classes (`vepu580_h264`, nine classes; the OSD class is left out, see its module doc), and a builder that makes rk-mpp's `Job` from the caller's values, with each address register checked for the header's alignment. The second window (L2, word 0x4000, 320 words) is set with `set_l2` and goes out as its own write run | Everything; the builder takes rk-mpp's job types, and nothing else is linked |
| `lemnos-linux-hotplug` (`backends/linux-hotplug`) | Devices appearing and going away: netlink uevents, inotify or rescans, in the kernel's uevent shape | Everything. `lemnos-linux` re-exports it as `hotplug` and adds the host's board hints |
| `lemnos-linux-stream` (`backends/linux-stream`) | A frame stream's ring and buffers mapped (shared memory or dma-bufs, with `DMA_BUF_IOCTL_SYNC` around each access), lengths and seals checked first | The mappings alone. `client` (on by default) adds the `StreamPlatform` `lemnos-client` takes |
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
| `lens/1` | 1 range, 2 set_position, 3 position, 4 direction, 5 settle_us | `vcm` |
| `raw-bus/1` | 1 i2c_transfer, 2 spi_transfer, 3 line_get, 4 line_set, 5 pwm_set, 6 uart_write, 7 uart_read | `raw-bus` |
| `gpu-lease/1` | 1 acquire, 2 poll, 3 release, 4 hang, 5 state, 6 last_hang | `gpu-lease` |
| `camera.sensor/1` | 1 info, 2 mode, 3 select_mode, 4 selected, 5 start, 6 stop, 7 set_controls, 8 post_values, 9 applied, 10 set_flips, 11 set_test_pattern, 12 identity | `ov9782` |
| `camera.receiver/1` | 1 limits, 2 configure, 3 link, 4 frame_stream, 5 start, 6 stop, 7 counters, 8 feeds, 9 embedded_stream, 10 source, 11 layout | `rp1-csi2`, `rkcif`, `sunxi-vin`, `k230-capture` |
| `camera.isp/1` | 1 info, 2 config_layout, 3 set_input, 4 set_output, 5 clear_output, 6 config_stream, 7 statistics_stream, 8 input_stream, 9 output_stream, 10 start, 11 stop, 12 counters | `pisp-be`, `pisp-fe`, `rkisp`, `isp600`, `k230isp`, `k230isp-frames` |
| `isp.pisp/1` | 1 info, 2 set_front_end_compression, 3 enable_temporal_denoise, 4 reset_temporal_denoise, 5 set_line_event | `pisp-be`, `pisp-fe` |
| `isp.isp600/1` | 1 info, 2 load_region, 3 statistics_region | `isp600` |
| `camera.frames/1` | 1 format_count, 2 format, 3 select_format, 4 selected, 5 frame_stream, 6 start, 7 stop, 8 counters | `uvc` |
| `image.scaler/1` | 1 info, 2 format_count, 3 format, 4 job_stream, 5 counters | `rga`, `rkvpss`, `vglite` |
| `image.convert/1` | 1 info, 2 conversion_count, 3 conversion, 4 job_stream, 5 counters | `rga`, `g2d`, `nonai2d`, `vglite` |
| `image.blend/1` | 1 info, 2 format_count, 3 format, 4 job_stream, 5 counters | `rga` |
| `codec.block/1` | 1 info, 2 block, 3 job_stream, 4 counters | `rk-mpp`, `rk-iep`, `v4l2-stateless`, `a733-di`, `k230-vpu`, `a733-video` |
| `npu/1` | 1 info, 2 job_stream, 3 load, 4 unload, 5 attach, 6 detach, 7 counters | `rknpu` |
| `usb.function/1` | 1 info, 2 endpoint, 3 data_stream, 4 state, 5 control_in, 6 control_out, 7 control_stall, 8 set_halt, 9 cancel, 10 counters | `usb-gadget` |
| `system.slots/1` | 1 list, 2 install (a long job), 3 write, 4 set_default, 5 trial, 6 confirm, 7 remove, 8 pin, 9 set_keep, 10 accept, 11 install_file (a long job) | `system-slots` |
| `system.reboot/1` | 1 reboot | `system-reboot` |

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
