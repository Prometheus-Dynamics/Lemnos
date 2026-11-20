# G9 board run: raw claims, line configuration and edges on a real GPIO line and PWM channel

Status: untested on board.

Claims (`raw-bus/1` methods 10 `claim`, 11 `release`, 12 `line_configure`, 13 `read_edges`) are checked
on the simulated board by `drivers/raw-bus/tests/claims.rs`. The checks through the real service, with a
client killed with `SIGKILL`, are in `service/lemnosd/tests/raw_claim.rs`. Nothing here has run on
hardware. The checks below need a spare GPIO line that a jumper or a signal generator can toggle, and a
PWM channel whose output a logic analyser can see.

1. Bind a `raw-bus` device with `uses = { line = "<spare line>", pwm = "<channel>" }` and
   `config = { writes = true, line_safe = "low" }`.
2. Client A: `claim`, then `line_configure(output, initial true)` and `pwm_set(40000, 20000, true)`.
   Expect the pin high and a 25 kHz, 50 % signal on the analyser. Client B: `claim` and `pwm_set` answer
   `BUSY`, and `line_get` answers `true`.
3. Kill client A with `SIGKILL`. Expect the pin low and the PWM output stopped. Measure the time from the
   kill to both on the analyser; the bound is 1 s. Client B's `claim` then answers `OK`.
4. Client B: `line_configure(input, bias pull-up, edge both, debounce 1000)`. Toggle the line 10 times
   by hand or from a generator at 10 Hz. `read_edges` returns 10 edges, alternating, with consecutive
   sequence numbers and kernel timestamps 100 ms apart, give or take the debounce.
5. Toggle the line 50 times with no read in between. `read_edges` returns 32 edges, with the oldest
   sequence number showing the lost ones (or a gap where the kernel's queue overflowed).

Record the analyser captures or the call outputs under this heading, with the board, the kernel and the
date. CM5 timings only (see AGENTS.md, "Board runs").
