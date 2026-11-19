# G11 board run: transfer lists on a real I2C bus and SPI device

Status: untested on board.

The lists (`raw-bus/1` methods 8 `i2c_transactions` and 9 `spi_transfers`) are checked on the
simulated board by `drivers/raw-bus/tests/lists.rs`. Nothing here has run on hardware. The
checks below need a board whose `raw-bus` device names both `spi` and `raw_spi` for the same
spidev node, and an I2C target that answers a register pointer and read.

1. Bind a `raw-bus` device with `uses = { i2c = "i2c-1", spi = "spidev0.0", raw_spi = "spidev0.0" }`
   and `config = { writes = true, allow_i2c = "0x50" }` (use the device's own address).
2. I2C list, one transaction: `i2c_transactions(0x50, [write 0x10, read 2])`. Expect the two bytes
   the device holds at register 0x10, with no stop between the pointer and the read (confirm on a
   logic analyser or with `i2cdetect`-style probing that the bus shows a repeated start).
3. I2C list with a write after a read, with `writes = false`: expect `NOT_PERMITTED` and no bus
   traffic (the bus is quiet on the analyser).
4. SPI list: `spi_transfers([mode 3, 1 MHz, 8 bits, cs_change, delay 5 us, write [1, 2], read 2],
   [mode 0, 500 kHz, 0 bits, no cs_change, read 1])`. Expect three bytes back, the chip select to
   drop between the two transfers and a 5 us gap after the first (on the analyser).
5. Refusals on the board: nine ops in one list, a read longer than the reply buffer, and mode 4 each
   answer `INVALID_ARGUMENT` before any bus traffic.

Record the analyser captures or the call outputs under this heading, with the board, the kernel
and the date. CM5 timings only (see AGENTS.md, "Board runs").
