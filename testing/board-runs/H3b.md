# H3b board run: drivers on a UART

Status: untested on board. `raw-bus`'s UART calls and the description drivers over Modbus RTU have
run only against the simulated port; the frame settings (parity, stop bits) only against a
pseudo-terminal.

Setup: a board with a free UART, wired either in loopback (TX to RX) or through an RS-485
transceiver to a Modbus RTU sensor, and `lemnosd` built with `generic-sensor`'s `std` feature. Take
the board lock named in `AGENTS.md`.

1. Loopback through `raw-bus`: a device with `uses = { uart = "<the port>" }` and
   `config = { writes = true, uart_baud = 115200 }`. `uart_write` 64 bytes, then `uart_read` with
   a 100 ms wait until all are back.
   Pass: the same 64 bytes, in order; `stty -F <port>` shows `115200`, `-parenb`, `-cstopb`.
2. The frame: bind again with `uart_parity = "even"` and `uart_stop_bits = 2`.
   Pass: `stty` shows `parenb -parodd cstopb`; loopback as in 1 still holds.
3. Refusal: drop `writes`. Pass: `uart_write` answers `NOT_PERMITTED` and nothing appears on the
   wire (scope or the far end); `uart_read` still answers.
4. A Modbus sensor: a description of the sensor's input or holding registers from its datasheet,
   starting `modbus input` or `modbus holding`, with `generic-temperature` (or the contract the
   sensor fits) and `config = { address = <its unit id>, baud = <its rate>, parity = "<its
   parity>" }`.
   Pass: the value agrees with the sensor's own display or tool to its stated accuracy, read once a
   second for a minute with no failed read.
5. Unplug the sensor (or its bus) for 10 s while reading, then plug it back.
   Pass: reads fail with health `failed` (`bus error`) while it is gone, and answer again without a
   restart, the frame and the start-up writes done again.
6. A wrong unit id in the board file.
   Pass: every read fails (`bus error`, a timeout), nothing else on the bus answers, and a correct
   id afterwards reads.
