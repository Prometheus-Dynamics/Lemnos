# lemnos-linux-sys

The one place in Lemnos with `unsafe` code. It defines the Linux uAPI structures that
`lemnos-linux` needs (`#[repr(C)]`, with layout tests against the kernel headers) and wraps
each system call in a safe function:

- `i2c`: `I2C_FUNCS`, `I2C_SLAVE` (never `I2C_SLAVE_FORCE`), combined `I2C_RDWR` transfers,
  SMBus byte/word/block transfers.
- `spi`: spidev mode, word size, speed and bit order; `SPI_IOC_MESSAGE` transfers.
- `gpio`: GPIO character device uAPI v2: chip and line info, line requests, values,
  reconfiguration.
- `netlink`: the kernel's `NETLINK_KOBJECT_UEVENT` socket.
- `inotify`: init, add and remove watches.
- `poll`: waiting on one descriptor.

Every `unsafe` block carries a `// SAFETY:` comment (`clippy::undocumented_unsafe_blocks` is
denied). Applications use `lemnos-linux`, not this crate. Ported from Styx's `styx-kernel`.
