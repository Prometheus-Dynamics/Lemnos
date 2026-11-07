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
- `block`: the uAPI of the generic Lemnos block device (`/dev/lemnos-<device>`, `lemnos.ko`'s
  `lemnos` driver): info, register regions and their mapping, DMA buffers as dma-bufs,
  dma-buf imports and interrupts.
- `lemnos_pisp_be`: the uAPI of Lemnos's own PiSP back end module (`/dev/lemnos-pisp-be`):
  info, submit, recover and the finished-job records.

Every `unsafe` block carries a `// SAFETY:` comment (`clippy::undocumented_unsafe_blocks` is
denied). Applications use `lemnos-linux`, not this crate. Ported from the earlier kernel layer (see docs/provenance.md).
