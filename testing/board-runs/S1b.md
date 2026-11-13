# S1b: the hardware watchdog as the daemon's keep-alive

Status: untested on board. The watchdog's timeout request, its feed cadence, its retries and its
never writing the magic close are covered by unit tests over a fake device
(`drivers/hw-watchdog/src/tests.rs`) and the timeout request's numbers by `drivers/hw-watchdog-sys`.
The gating is covered by host tests (`engine/host/tests/keepalive.rs`), by the watchdog on a
host (`drivers/hw-watchdog/tests/host.rs`), and by the daemon on its socket
(`service/lemnosd/tests/keepalive.rs`). Nothing here has armed a real watchdog.

This check extends `E24.md`, which covers the reset of the reference board. Run it on the CM5
(Raze #2), under `/run/lock/board-gpu.lock`. Build on NVMe or SSD, never on the Github HDD. Record
CM5 timings only. Arming and resetting the board's watchdog is the test: nothing else may be on the
board.

Board file: the `watchdog` device of `docs/daemon.md` ("If the daemon dies"), with `device` set to
the watchdog node (`watchdog0` or the one the board has) and `timeout_s = 15`.

1. Timeout. Start the daemon. Read `/sys/class/watchdog/watchdog0/timeout` and the device's
   `min_timeout` and `max_timeout`.
   Pass: the timeout reads 15, or the nearest value the device allows, and the daemon's health for
   `watchdog` reads `ok`. Record the value the device gave for `timeout` before the daemon ran.
2. Feed. Watch `timeleft` with the daemon running for a minute.
   Pass: `timeleft` never falls to zero, and it is refilled about every 7.5 s.
3. Daemon healthy, then not. Stop the daemon's socket answering while it runs, by blocking its
   socket path (a board change: record it), and wait.
   Pass: within about 5 s the feed stops (`timeleft` falls), and the board resets about the timeout
   after the last feed. Record the time from the last feed to the reset.
4. Restart at once. Stop the daemon and let systemd start it again (a restart, not a stop).
   Pass: the new daemon's first feed comes within about 1 s of its socket answering, and the board
   does not reset. Record the time from the stop to the socket answering.
5. Driver panic. With a second device on the board that panics on every tick (a test driver), keep
   the daemon up for a minute.
   Pass: the watchdog is still refilled, and the other device's health reads degraded, not the
   daemon.
6. Clean shutdown. Shut the board down (`systemctl poweroff`) with the daemon running.
   Pass: the daemon stops feeding and the watchdog is not disarmed. If the shutdown hangs (for
   instance by holding a unit that does not stop), the board resets at the timeout.

Untested on board. The results go in this file, under each check's number.
