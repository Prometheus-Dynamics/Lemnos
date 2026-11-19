# H4a: a device for each USB camera, by class

Status: untested on board. The board format's `[[each]]` entry, the host's looks (at start, on a
hint, every retry cap), the USB class and vendor:product patterns, pinning by a device entry and
`except` are covered by tests on the simulated board, on a fake sysfs tree for the Linux backend,
and with the `uvc` driver on mock cameras. No test has listed a real `/sys/bus/usb/devices`, and
no real camera has been bound this way.

Run with the board lock named in `AGENTS.md`. Two UVC cameras of the same make, and one USB
device that is not a camera (a disk or a serial adapter), plugged into a hub on the board.

```toml
[[each]]
id = "camera"
driver = "uvc"
match = "usb:class:0e"
need = "usb"
config = { frame_bytes = 4147200 }

[each.streams.frames]
buffers = 4
buffer_bytes = 4147200
```

Checks, in order:

1. Start lemnosd with both cameras plugged in. `lemnos-ctl list` shows `camera@usb:B-P` for each,
   named by the port `ls /sys/bus/usb/devices` shows it on, both running and `state: idle`. The
   other device has no device and is untouched (its kernel driver still bound).
2. Stream from each camera at once (`select_format`, `start`, attach a consumer to each
   `frames` stream): frames from both, at the rate of their formats.
3. Pull one camera. Within the hint gap its device goes retrying with the reason that it is
   gone; the other keeps streaming. Plug it back into the same port: the same id and the same
   index come back, running, with no restart of lemnosd.
4. Plug it into another port: a new device `camera@usb:B-P2` is made for it, and the old id stays
   listed, retrying.
5. Stop lemnosd and set `LEMNOSD_BOARD` to a file without the hotplug watcher's events (or block
   uevents): a camera plugged in is bound within 8 s by the look the retry cap brings.
6. Add a `[[devices]]` entry naming one camera by `usb:VVVV:PPPP` of a third camera of another
   make, and one with `disabled = true` naming a port: the first is bound only under its own id,
   the second is not bound at all and its kernel driver keeps it. `except = ["usb:B-P"]` on the
   entry does the same for that port.
7. `usb:class:0e:01` and `usb:class:ef:02:01` bind the same cameras as `usb:class:0e`.

Record: the `lemnos-ctl list` output of each step, and how long from plug to running in steps 3
and 5.

## Detached form (image where lemnosd owns the USB link)

Two things in this record restart lemnosd on that image, and each restart cuts the link for its
length:

- The `[[each]]` entry has to be in lemnosd's board file for every check, and the image's
  `/etc/lemnos/board.toml` is on a read-only root. Check 5's "set `LEMNOSD_BOARD`" also has no
  effect under the unit: its `ExecStart` passes `--board`, which wins
  (`packaging/systemd/lemnosd.env.example`). So lemnosd runs on `ALT_BOARD` (the image's file plus
  the entry above) through a runtime drop-in (`ExecStart=` override, gone at the next boot).
- Check 5 needs the hotplug hints off. The same drop-in resets `RestrictAddressFamilies` to
  `AF_UNIX`, so lemnosd's uevent socket (AF_NETLINK) is refused; the check requires lemnosd's own line
  `not watching for hardware coming and going` before it goes on, and records NOT-RUN otherwise.

`ALT_BOARD` keeps the image's `usb-gadget` device and the `hw-watchdog` device (the check refuses a
file without either), so the link comes back after each restart and lemnosd feeds throughout.
`scripts/checks/H4a.sh`:

- `H4A_MODE=step5` (check 5): restart onto `ALT_BOARD` with hints off (one lemnosd down window,
  5 s restore), then `PLUG NOW` in the log: a person plugs one camera (hands) within `PLUG_WAIT_S`.
  The check sees its video-class interface appear in `/sys/bus/usb/devices` (0.1 s samples) and
  times `camera@usb:B-P` to `running` in `lemnos-ctl list` (0.2 s samples). Pass: at most 8 s plus
  one look. Then the drop-in out and one more restart onto the image's settings (5 s restore).
- `H4A_MODE=hold` (checks 1 to 4): restart onto `ALT_BOARD` with hints on, then hold. The link is
  up; run checks 1 to 4 over the session, then `lemnos-check.sh release H4a` (or `HOLD_S` passes)
  and the check restarts lemnosd onto the image's settings.
- A `session` restore armed first (`PLUG_WAIT_S` or `HOLD_S` + 120 s) puts the image's settings
  back if the check dies.

Running it (shared by every detached form; `scripts/lemnos-check.sh`'s header has the details):

1. Upload `scripts/lemnos-check.sh`, `scripts/detached-lib.sh` and `scripts/checks/` to
   `/data/board/checks/bin/` over the session (not `/tmp`: the session's `/tmp` may be private to the
   comm core's unit, and the transient units would not see it).
2. Hold no board lock in the session: the check takes `/run/lock/raze-gpu.lock` and
   `/run/lock/board-gpu.lock` itself (10 s wait) and refuses, stopping nothing, if either is held.
3. As root: `bash /data/board/checks/bin/lemnos-check.sh start H4a ALT_BOARD=<file> [H4A_MODE=step5|hold] [PLUG_WAIT_S=600] [HOLD_S=1800]`. It copies the
   scripts into `/data/board/checks/H4a/<UTC stamp>/bin/`, starts the check in the transient unit
   `lemnos-check-H4a` and returns. The link then goes; nothing on the board depends on the session.
4. The check logs its hypothesis, a baseline and every step (CLOCK_REALTIME stamp and seconds since
   boot) to `log` in the run directory, verdicts to `result`. Before each stop it arms a timed
   restore in its own transient unit (`lemnos-restore-H4a-NAME`) and sees it running; at the bound,
   uncancelled, the restore kills the check's unit, undoes what the check changed, starts lemnosd
   and the comm core with backoff 1, 2, 4, 8, 8 ... s, never giving up, and checks the end state.
   The end state, checked by the check or the restore: lemnosd active, the `hw-watchdog` device
   `running ok`, the comm core and the bridge as before, the gadget bound to the same controller, no
   drop-in of ours, the link's addresses back (one still missing after 30 s is added back as it was,
   and recorded as a finding), the same boot id. Nothing touches config.txt, the default slot,
   `mem=`, `.reboot-ok` or the watchdog.
5. Reconnect after the restarts (the link is back within seconds; the session may reconnect during the plug wait) and run `bash /data/board/checks/bin/lemnos-check.sh collect H4a`: the
   log, the restore logs, the results, whether each restore was cancelled or fired, and whether the
   board reset since the baseline (`THE BOARD RESET`, from the boot id). Paste it here.

The watchdog: once slot-init has exec'd away, lemnosd's `hw-watchdog` driver is the only feeder of
`/dev/watchdog`. It feeds every half period and never writes the magic close. When lemnosd stops or
dies the kernel pings once on the close and logs `watchdog did not stop!`
(`drivers/watchdog/watchdog_dev.c`, `watchdog_release`); the board resets one period (15 s) after
that unless a new lemnosd feeds. A stop that hangs before it drops the driver leaves only what was
left of the last feed: half a period (7.5 s) at worst. Every lemnosd down window is therefore bounded
by its timed restore at `DOWN_BOUND_S` = 5 s, plus 2 s for lemnosd's start, under that 7.5 s; the
check refuses to start if the device's own `timeout` makes this false. The log says for each window
whether the close ping was seen and how long lemnosd was down. A lemnosd that cannot be started
inside the period (the backoff running past it) resets the board: expected, recorded by `collect`.

Status of the detached form: untested on board. Rehearsed on the PC against the stand-in board
(`scripts/rehearse/rehearse.sh ID CASE`: fake systemd with `Restart=always` and starts that fail
like a failed start job, the kernel's close ping, a 15 s watchdog that resets the stand-in): the
normal run; the check killed right after it stops or restarts lemnosd; the check frozen there; two
failed starts (the restore retries 1 and 2 s apart and the board stays up); five failed starts (the
backoff passes the period and the stand-in resets, which `collect` reports). Three failed starts are
the most a restore survives inside one period.
