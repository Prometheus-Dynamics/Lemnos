# Lemnos

Lemnos gives a program the hardware of the board it runs on. It is an engine: it runs drivers,
keeps them running, and carries calls, values and buffers between a driver and whoever uses it. It
does not know what any device is.

What is in it:

- **Drivers** for chips and hardware blocks, and the **contracts** they serve (`accelerometer/1`,
  `camera.sensor/1`, `image.scaler/1` and so on).
- **A host** that reads a board file, binds each driver to its bus, contains its panics and hangs,
  and reconnects it to hardware that went away.
- **A small daemon**, `lemnosd`, that owns the hardware and serves every program over one socket,
  and a **wire protocol** any language can speak. `lemnos-ctl` is the command-line client.
- **A client library** in Rust, and the same client as a C API.
- **An optional kernel module**, `lemnos.ko`, for what user space cannot do safely by itself:
  claims the kernel enforces, DMA memory, and frame paths with every address checked.

What it is not:

- **No vendor libraries.** A driver talks to its block through the kernel's interface or its
  registers. Nothing closed is linked or loaded.
- **No software stand-in for hardware.** A job the hardware cannot do fails. Lemnos does not do
  the work on the CPU instead.
- **What runs on a block is the caller's business.** Lemnos carries a compiled model, an ISP
  configuration or an encoder's settings as bytes. It does not make them, tune them or read them.

None of this code has run on hardware in its present form. It is tested against simulations and
models of the kernel. [`docs/hardware-status.md`](docs/hardware-status.md) says exactly what a
board still has to show.

## Layout

```text
engine/      the driver model, the host, the wire protocol, the client, the C API, packages
backends/    how the engine reaches hardware: Linux, USB, V4L2, a microcontroller, a simulation
service/     lemnosd, lemnos-ctl, lemnos-orion
contracts/   one small crate per contract
drivers/     one crate per driver; drivers/<name>-sys holds a driver's unsafe kernel interface
kernel/      lemnos.ko
include/     the C headers
testing/     test tools, test boards, the QEMU harness, the bench, board-run records
scripts/     the gate and its checks
docs/        the documents below
```

## Build and run

Rust comes from `rust-toolchain.toml`. Build the daemon and the tool. The `fake-devices` feature
adds drivers with no hardware behind them, for trying things out:

```sh
cargo build -p lemnosd -p lemnos-ctl --features lemnosd/fake-devices
export PATH="$PWD/target/debug:$PATH"
```

Start the daemon on the simulated board, and talk to it. Both programs read the socket's path from
`LEMNOSD_SOCKET`:

```sh
export LEMNOSD_SOCKET="${TMPDIR:-/tmp}/lemnosd.sock"
lemnosd --sim --board docs/examples/sim-board.toml &
```

```sh
lemnos-ctl list
lemnos-ctl describe counter
lemnos-ctl read accel
lemnos-ctl set counter target 7
lemnos-ctl read counter count
```

`list` prints one device a row: its name, driver, state, health and tags, and the reason when it
is not running.

```text
accel        fake-accelerometer  running  ok  accelerometer/1
gyro         fake-gyroscope  running  ok  gyroscope/1
mag          fake-magnetometer  running  ok  magnetometer/1
orientation  orientation  running  ok  orientation/1
counter      fake-counter  running  ok  example.counter/1  -> accel
absent       bmm150  retrying  failed  magnetometer/1  (bind failed: Status(-2))
```

Stop the daemon with a signal. Stopping is what releases the hardware:

```sh
kill %1
wait
```

## The gate

One script decides whether a change may land: formatting, lints, every test, the `no_std` builds,
the layering rules and the commands in these documents.

```sh needs:the-gate-itself
scripts/gate.sh
```

## Read next

| Document | What it covers |
|---|---|
| [Concepts](docs/concepts.md) | Devices, contracts, drivers, resources, health and the failure rule |
| [Board files](docs/boards.md) | Every key, every kind of name, a complete example |
| [The daemon and lemnos-ctl](docs/daemon.md) | Every flag and every subcommand |
| [The client library and the C API](docs/client.md) | Using devices from a program |
| [Frame streams](docs/streams.md) | Shared memory and dma-bufs, jobs, faults |
| [The kernel module](docs/kernel-module.md) | What it gives, building, loading, claims |
| [Writing a driver](docs/writing-a-driver.md) | The three forms: built in, description file, package |
| [The microcontroller host](docs/mcu.md) | The same drivers with no allocator and no threads |
| [Drivers and contracts](docs/drivers.md) | Each one, and the state it is in |
| [Testing](docs/testing.md) | The gate, the QEMU harness, board runs |
| [Hardware status](docs/hardware-status.md) | What is not proven on hardware |
| [The design](docs/design.md) | Why it is built this way |
| [The socket protocol](docs/wire.md) | The specification, with test vectors |

[`AGENTS.md`](AGENTS.md) has the working rules for changing this repository, and
[`TODO.md`](TODO.md) lists what is still open.

Licensed under MIT or Apache-2.0, at your option. The kernel module is GPL-2.0-only.
