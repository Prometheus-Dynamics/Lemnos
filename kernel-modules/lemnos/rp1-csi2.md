# lemnos-rp1-csi2 (a personality of `lemnos.ko`)

The Raspberry Pi RP1 CSI-2 receiver as a plain character device, `/dev/lemnos-rp1-csi2`. It
covers the CFE block's D-PHY, its CSI-2 host and one CSI-2 DMA channel; the PiSP front end is not
used. It is a fast path beside the stock `rp1-cfe` V4L2 driver, never a requirement. The module
has no `of_match_table`, so it binds only where the device's `driver_override` names it, after
`rp1-cfe` is unbound.

The module only receives. User space sets up the sensor over i2c-dev. The user-space side is
`lemnos-drivers-pisp` with the `csi2-module` feature:
- `device::csi2_module::Csi2Module` and the `Receiver` trait it shares with `MockReceiver`;
- `device::cci` for the sensor's registers;
- all of it over `lemnos_linux_sys::lemnos_rp1_csi2`.

GPL-2.0. The D-PHY bring-up, the DMA channel's registers and the frame start and end accounting
are ported from the stock driver (`rp1_cfe/dphy.c`, `csi2.c` and `cfe.c`, copyright Raspberry Pi
Ltd, notice kept in the source). The uAPI header `lemnos_rp1_csi2.h` is
`GPL-2.0-only WITH Linux-syscall-note`. The reasoning is in `docs/kernel-module.md`, section 4.

## What it does

- **`LEMNOS_RP1_CSI2_IOC_START`** takes the link the sensor was set up for and the stream to
  keep:
  - the link: 1 to the device tree's lane count, 80 to 1500 Mbit/s per lane;
  - the stream: virtual channel 0-3, data type 1-63, width, height, stride and size, with
    stride and size multiples of 16.

  `LEMNOS_RP1_CSI2_UNPACK16` makes the DMA channel unpack each RAW10/12/14 pixel to 16 bits,
  most significant bit aligned. This is `rp1-cfe`'s 16-bit remap, and it is what the PiSP back
  end reads, so a frame can go straight from the receiver's buffer into a back end job.

  It then brings things up in `rp1-cfe`'s order: the DMA channel, the first buffer armed,
  MIPICFG, the D-PHY, and last the CSI-2 host. Other data types on the link, such as a sensor's
  embedded line, are not written.
- **A buffer is a dma-buf in one contiguous segment** (a CMA heap buffer): RP1 has no IOMMU in
  front of it. `QUEUE` takes the dma-buf and a 16-byte-aligned offset, maps it on the receiver
  `DMA_FROM_DEVICE` and checks that it holds `size` bytes. Mappings are kept between frames, 16
  buffers per open file, and dropped at close.
- **Up to 8 buffers** may be queued and not yet read. The channel holds the one being written
  and the next. A frame start with no buffer armed is counted as dropped and nothing is written.
- **Each finished buffer is a `struct lemnos_rp1_csi2_done`**, read from the descriptor
  (`poll` for `POLLIN`). It carries the frame start's and frame end's `ktime_get_ns()` and the
  frame's sequence number since `START`.
  - A frame start before the previous frame's end returns that buffer with `-EIO`.
  - `STOP` and close return the queued buffers with `-ECANCELED`. `STOP` writes FORCE to the
    channel, then clears its address twice, as `rp1-cfe` does.
- One open file at a time.

## Build

Against the configured and built kernel tree of the image, with its toolchain:

The receiver is a personality of `lemnos.ko`; build it as [README.md](README.md) says:

```sh
make KDIR=<the image's linux build tree> ARCH=arm64 CROSS_COMPILE=<its toolchain prefix>
```

`W=1` builds without warnings.

## Load and bind

As root, with no user of the `rp1-cfe` nodes running. The sensor's kernel driver must also be
unbound, so that i2c-dev can claim its address. If the sensor's supply is a fixed regulator on a
GPIO, unbind the regulator too, so that user space can drive the line.

```sh
D=1f00110000.csi
echo $D > /sys/bus/platform/drivers/rp1-cfe/unbind
echo <bus-addr> > /sys/bus/i2c/drivers/<sensor driver>/unbind
echo <regulator> > /sys/bus/platform/drivers/reg-fixed-voltage/unbind
insmod lemnos.ko
echo lemnos-rp1-csi2 > /sys/bus/platform/devices/$D/driver_override
echo $D > /sys/bus/platform/drivers/lemnos-rp1-csi2/bind
# and back:
echo $D > /sys/bus/platform/drivers/lemnos-rp1-csi2/unbind
echo > /sys/bus/platform/devices/$D/driver_override
echo <regulator> > /sys/bus/platform/drivers/reg-fixed-voltage/bind
echo $D > /sys/bus/platform/drivers/rp1-cfe/bind
```

Binding the regulator again let the sensor driver probe again on its own (deferred probe).

## Measured on a board

Board #1 (BCM2712, kernel 7.2.9-v8-16k), 2026-11-06, with `lemnos-drivers-pisp`'s
`csi2_module_check`:
- Sensor: an OV9782 on i2c-10 (it reports the OV9281 id, 0x9281).
- Sensor setup: a register file kept outside this repository. It sets 1280x800 RAW10 over 2
  lanes at 800 Mbit/s, VTS 1822 and the sensor's colour-bar test pattern.
- Buffers: 8 CMA buffers of 1,280,000 bytes at stride 1600.

Results:
- **3600 frames at 60.28 fps.** 0 failed and 0 frame starts without a buffer. 60.28 fps is the
  rate the sensor's timing gives: 1823 lines of 9.1 µs.
- **Frame start to frame end:** median 7465 µs (800 lines).
- **Content:** the bars exactly. Rows of the same parity were identical byte for byte, as a
  Bayer sensor's bars alternate between two rows. A frame dumped and decoded showed the
  expected 0 and 1023 runs.
- **Restore:** `rp1-cfe` bound again afterwards and registered its nodes, and the camera
  service started again. This held on each of three runs.

**Receiver to back end**, the same day: `csi2_module_check --be`, with this module and
the back end personality bound, `UNPACK16` at stride 2560, and each frame's buffer
submitted as the input of a demosaic job (BGGR to RGB888, identity matrix):
- No copy: the back end reads the receiver's dma-buf.
- 600 frames at 60.28 fps; none failed in either module, and no frame start went without a
  buffer.
- Frame end to the back end's interrupt: p50 757 µs, p99 1324 µs, max 1847 µs. To the
  user-space wake-up: p50 759 µs, p99 1341 µs, max 1864 µs.
- The RGB output's sampled rows were identical byte for byte, with 18 runs of one colour across
  a row (the bars and the demosaic's transitions between them).
- `pispbe` and `rp1-cfe` were bound again afterwards.

**At 120 fps**, the same day: the sensor's VTS set to 910 lines (120.63 fps), 9000 frames a run,
beside a `lemnosd` probe:
- Alone: none failed or dropped. Frame end to the back end's interrupt p50 756 µs, p99 1007 µs,
  max 3589 µs; to the wake-up p50 759 µs, p99 1014 µs.
- With 4 busy loops on the 4 cores: none failed or dropped. Interrupt p50 757 µs, p99 1082 µs,
  max 5168 µs; wake-up p50 760 µs, p99 3729 µs, max 6693 µs. The checking program runs at normal
  priority, so the busy loops show in its wake-up tail.

Untested: more than one virtual channel or data type, 1 lane, `PACK_BYTES`, link rates other
than 800 Mbit/s, a missing frame end, and the 6.12 kernels.
