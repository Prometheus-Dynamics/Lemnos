# The microcontroller host

The same driver crates that the daemon runs also run on a microcontroller, with no allocator, no
threads and no operating system. Two crates make that possible:

- `lemnos-host-static` (`engine/host-static`): the host. Its devices are a table compiled into the
  firmware.
- `lemnos-mcu` (`backends/mcu`): the backend. It turns the peripherals the firmware already has
  into the resources drivers ask for.

Neither has any `unsafe`. Both build for bare-metal targets:

```sh
cargo check -q -p lemnos-host-static -p lemnos-mcu --no-default-features --target thumbv7em-none-eabihf
cargo check -q -p lemnos-host-static -p lemnos-mcu --no-default-features --target riscv32imac-unknown-none-elf
```

## The table

There is no board file. The firmware declares one `Slot` per device and a `Table` over them, all
`static`. A slot's `Spec` holds what a board file entry holds: the device's name, its bus, what it
uses, its link and its config.

```rust
static SENSOR: Slot<Bmi088, Locked<Device<Bmi088>>> =
    Slot::new(Spec::new("imu").bus("i2c0"), Locked::new(Device::new()));
static FUSED: Slot<Orientation, Locked<Device<Orientation>>> = Slot::new(
    Spec::new("orientation")
        .uses(&[("accel", "imu"), ("gyro", "imu")])
        .config(Config::new(&[("sample_rate_hz", ConfigValue::Int(100))])),
    Locked::new(Device::new()),
);
static TABLE: Table = Table::new(&[&SENSOR, &FUSED], &CLOCK, Options::DEFAULT);

TABLE.start(&mut buses);                         // binds, the devices others use first
loop {
    let next = TABLE.poll(&mut buses);           // runs the retries and ticks that are due
    TABLE.call(1, method, &args, &mut reply);    // calls a device by its place in the table
}
```

`Locked` is the firmware's own lock, such as a critical section around a `RefCell`. The crate has
no `unsafe`, so it cannot make a shared cell itself. `CLOCK` is the firmware's monotonic clock.

A driver is built in its slot, never on the stack, so a large driver costs no stack. A device that
uses another calls it directly, in the caller's stack frame.

The table also answers what the daemon answers: `find` a device by tag, `describe` it, its
`state`, `health` and `link`, and `off` and `on`.

## The backend

`lemnos-mcu` wraps what the firmware's HAL provides:

| Type | Wraps |
|---|---|
| `I2cBus`, `SpiBus` | Any embedded-hal 1.0 bus |
| `OutputLine`, `InputLine` | An embedded-hal pin |
| `PwmChannel` | An embedded-hal duty-cycle channel |
| `usb` | USB in both roles, over a small port trait the firmware implements on its USB peripheral |
| `Stall` | The place a bus lives while no driver holds it, and where it returns when its driver is dropped |
| `Buses` | The firmware's stalls by name: the provider a table binds from |

## The failure rule here

It is the same rule as on the daemon. A device that fails to bind is retried forever, after 1 s
doubling to at most 8 s. A driver that says its hardware is gone, or whose calls fail the way a
missing device fails several times in a row, is dropped and bound again. Dropping a driver gives
back every bus it held. Nothing is switched off unless the firmware calls `Table::off`.

One thing differs: there is no panic containment. A panic on a microcontroller ends the firmware.

## What is not here

There is no socket, no wire protocol and no client: the firmware calls the table. There are no
driver packages. The backend hands out I2C, SPI, GPIO lines, PWM, UARTs and USB in both roles. It
has no frame streams, register windows or DMA memory, so the block drivers do not bind here. Chip
drivers are written `no_std` so that they do; block drivers that open Linux nodes are not.

These crates are checked without `std` on every gate run: `lemnos-abi`, `lemnos-hal`,
`lemnos-core`, `lemnos-host-static`, `lemnos-mcu`, the accelerometer, gyroscope, magnetometer and
orientation contracts, and the `orientation` and `usb-gadget` drivers.

## Tests

The tests bind two real drivers in a static table against mock peripherals, count allocations from
start to steady state (none), and drop and rebind a driver.

```sh slow
cargo test -q -p lemnos-host-static -p lemnos-mcu
```

Nothing here has run on a microcontroller. The check is written down in
`testing/board-runs/E28.md`.
