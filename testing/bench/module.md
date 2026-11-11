# The module route against the stock route

> **Read this first.** Every number below comes from a virtual machine with simulated adapters: an
> in-memory I2C target, the kernel's SMBus stub, a loopback SPI controller, a simulated GPIO chip
> and a test PWM chip that only stores what it is given. They measure the software path only: the
> system call, the claim check, the copies in and out, and the kernel's own bus core. They say
> nothing about real bus time (clock stretching, a 100 kHz or 1 MHz wire, chip-select setup, a
> PWM controller's registers) and nothing about real hardware. On a real bus the wire time is
> usually far larger than either route's software cost. The real comparison is the board run in
> `testing/board-runs/C8.md`.

## What was compared

`lemnos-module-check` (`testing/crates/module-check`) drives the real Linux backend
(`backends/linux`) in the QEMU harness (`testing/qemu/run.sh`). It runs three times in one boot:

1. the module loaded and the SPI device bound to it: every bus through `/dev/lemnos`;
2. the module loaded and the SPI device bound to `spidev`: SPI on its stock node, the rest through
   the module;
3. no module: every bus on its stock node (`/dev/i2c-N`, `/dev/spidevB.C`, the GPIO character
   device, PWM sysfs).

After each bus's checks pass, the same handle runs one operation 2100 times: 100 untimed, then
2000 each timed with the monotonic clock (`std::time::Instant`) around the single call. The line
printed is the median, the 99th percentile and the heap allocations counted over all 2100 runs by
the counting allocator. Nothing fails on time; the numbers are only printed.

| Operation | What one run does |
| --- | --- |
| i2c write+read | test adapter, one transaction: write 1 byte, repeated start, read 4 |
| i2c smbus write+read | SMBus stub, write 1 byte then read 1 (a byte-data read on an SMBus-only adapter) |
| spi duplex 4 bytes | loopback controller, one full-duplex transfer of 4 bytes, checked |
| gpio set | output line, alternating high and low |
| gpio get | the same line read back |
| pwm configure | enabled channel, duty alternating between two values at a fixed period |

Setup: x86-64 host, KVM, 2 virtual CPUs, 768 MB, the host's 7.2 kernel, the harness's debug build
of the backend (`cargo build` without `--release`), one run.

## Numbers

Nanoseconds per operation, median / 99th percentile. Run 1 and run 2 both measure the module route
for I2C, GPIO and PWM; run 2 and run 3 both measure the stock route for SPI.

| Operation | Module (run 1) | Module (run 2) | Stock (run 3) | Stock SPI (run 2) | Allocations |
| --- | --- | --- | --- | --- | --- |
| i2c write+read | 3476 / 3777 | 2926 / 4198 | 3456 / 3616 | | 0 |
| i2c smbus write+read | 1803 / 2013 | 1703 / 3617 | 1122 / 1173 | | 0 |
| spi duplex 4 bytes | 9197 / 17222 | | 8927 / 21540 | 8425 / 20519 | 0 |
| gpio set | 841 / 921 | 871 / 1463 | 892 / 993 | | 0 |
| gpio get | 891 / 972 | 891 / 1493 | 881 / 1002 | | 0 |
| pwm configure | 832 / 1403 | 932 / 982 | 78057 / 134112 | | 0 |

What the run shows, within the limits above:

- Neither route allocates on any of these operations.
- I2C, SPI and GPIO cost about the same on both routes: the system call and the kernel's bus core
  dominate, and the module's claim check and copy are within run-to-run noise. The SMBus stub is
  the exception, slower through the module by roughly 0.6 µs. In steady state both routes make one
  SMBus call per operation (the module route learns the adapter is SMBus-only from its first
  refused plain transfer, the stock route from the adapter's functionality bits at open), so the
  difference is the module's own path: its claim check, the copy, and its SMBus dispatch. This run
  does not break it down further.
- PWM differs by two orders of magnitude: the stock route writes sysfs files (open, format, write,
  close, per attribute), the module route is one ioctl. On real hardware the PWM controller's own
  register writes add to both.
- The 99th percentiles are host scheduling noise as much as anything; a single run on a virtual
  machine does not say which route has the steadier tail.

## Board figures

On the reference board, 2026-11-11 (`testing/board-runs/C8.md`): three rounds of
`lemnos-module-check board`, alternating the routes, 0 allocations everywhere.

| Operation | Stock, median / p99 | Module, median / p99 |
| --- | --- | --- |
| I2C register read, 1 byte out and 2 in, on a real chip | about 505.4 µs / 524 µs | about 505.6 µs / 525 µs |
| GPIO get, a free input | 1278 ns / 1408 ns | 1241 ns |
| PWM configure, output off | about 25.1 µs / 33 to 40 µs | 3945 ns / about 4.5 µs |

The I2C time is the wire's on both routes. SPI was not measured: the board has no SPI device that
may be touched.

## Behaviour that differs between the routes

These are in the merged code (`kernel/`, `backends/linux/src/lib.rs`, `backends/linux/src/module/`),
whatever the timings say.

- **Claims are the kernel's.** On the module route every I2C target, SPI device, GPIO line and PWM
  channel is held by a claim file from `/dev/lemnos`. A second claim on the same name gets `EBUSY`
  from the kernel, across processes, and the claim ends when its file closes, including when the
  holder dies. On the stock route an I2C adapter has no claim at all (the kernel serialises
  transfers but any process can talk to any address), SPI and PWM are claimed only inside one
  process, and a GPIO line is exclusive only because the kernel's line request is.
- **I2C targets are claimed one by one.** The module claims `i2c-N:0xAA` on the first transfer to
  an address, keeps it while the adapter handle lives, and lets go of an address that did not
  answer. An address a kernel driver holds is refused with `EBUSY` on the module route.
- **Transfer limits.** On the module route a transfer has at most 16 segments and at most
  `bus_buffer` bytes each way (a module parameter, 4096 by default, at most 1 MiB); beyond that the
  kernel answers `EMSGSIZE`. An I2C segment is at most 65535 bytes. The stock route has the stock
  nodes' own limits instead.
- **PIO-driven I2C is not on the module route.** A bus named `pio-i2c:sda=N,scl=N` always runs on
  the backend's own PIO path, module or not.
- **Which buses fall back to stock, and when.** With no module opened, everything is stock. With
  it opened:
  - I2C: every `i2c-N` adapter that exists goes through the module; there is no fallback per
    adapter.
  - SPI: through the module only if the device is bound to the module (`driver_override` and
    `owned=`); a device bound to `spidev` stays on its stock node.
  - GPIO and PWM: through the module unless opening there fails with `ENODEV`, `ENOENT`, `ENOTTY`
    or `EINVAL` (the module does not serve it), and then stock. `EBUSY`, `EPERM` and other failures
    are returned as they are, never retried on the stock route, so the stock node is never a way
    around a claim.
- **The control node has one opener.** Only one process can open `/dev/lemnos`, so only the host
  of a machine's hardware uses the module route; anything else on the machine is on stock nodes.
  Inside that process the backend and every block driver share one open file
  (`lemnos_linux_sys::module::open_shared`).
