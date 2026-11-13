# S1a: pwm-channel-fan, the fan on a PWM channel

Status: untested on board. The driver's arithmetic, its failure cases, its kick, its tachometer and
its steady-state allocations are covered by unit and host tests on the simulated board
(`drivers/pwm-channel-fan/src/tests.rs`, `tests/host.rs`, `tests/alloc.rs`). No real fan has been
driven through it.

Run on the reference board (the CM5, Raze #2), with a fan wired to a PWM channel of its chip. Take
the board lock named in `AGENTS.md` (`/run/lock/board-gpu.lock`) before starting, and build on NVMe
or SSD, not the HDD. Name the channel as `uses = { pwm = "pwmchipN:CHANNEL" }`, and add
`tach = "gpiochipN:LINE"` when the fan has a tachometer. The `config` is the board's own; the run
records what it used.

Before any change, record the channel's `period` and `duty_cycle` from
`/sys/class/pwm/pwmchipN/pwmM/`, with the channel exported, and the fan's speed by tachometer or by
ear.

1. Bind. Start the daemon with the device and no `set_duty` call.
   Pass: the device is running, and the channel's `duty_cycle` equals its `period`: the fan starts
   at full duty before anything asks for less. The duty read back is 1000.
2. Duty mapping. `set_duty(500)`, then `set_duty(1000)`, then `set_duty(0)`.
   Pass: the channel's `duty_cycle` is half the period, then the period, then 0. The fan slows, then
   runs at full speed, then stops.
3. Polarity. With `inverted = true`, repeat step 2.
   Pass: the channel's polarity reads `inversed`, and the `duty_cycle` values are the same as in
   step 2. The fan's speed follows the duty asked for.
4. Minimum and kick. With `min_duty = 200` and `kick_ms = 500`, `set_duty(0)` then `set_duty(50)`.
   Pass: the duty is raised to 200 (a fifth of the period), and for the first 500 ms the channel is
   at full duty. The fan starts at once and keeps turning at 200.
5. Tachometer, with `tach` wired and `pulses_per_rev` set to the fan's count.
   Pass: `rpm` matches the tachometer reading at the duty asked for. At `set_duty(0)`, after a
   second, `rpm` reads 0.
6. Failure with a real channel. Unexport the channel while the fan runs (a board change: record it),
   then `set_duty(600)`.
   Pass: the call returns no error, `health` reads degraded, and the retries follow 1, 2, 4, then 8
   seconds. Re-export the channel: the next retry writes 600 and `health` reads ok.
7. Controller on a real sensor. `fan-control` with `t0` on `thermal-zone`, and this fan as `fan`.
   Pass: the fan follows the curve as the board heats, and runs at full duty while the thermal zone
   is unreadable (make it unreadable by removing its read permission, and record that).
8. Release. `release()` on the client.
   Pass: the channel reads full duty and the fan speeds up to it.
9. Stop. Stop the device with the client (`off`), then stop the daemon with `SIGTERM`.
   Pass: after each stop the channel reads full duty and the fan runs at full speed, and nothing
   writes the channel after the process exits.

The results go in the board-run notes, and the first run's figures are the expected values for
every later run.
