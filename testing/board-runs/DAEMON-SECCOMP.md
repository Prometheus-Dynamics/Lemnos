# DAEMON-SECCOMP: the daemon runs under its unit's syscall filter on the board image (untested on board)

Not run on a board. This sheet holds the two reads a board run takes on the M2 image once it boots
with the daemon's unit, to show that the unit's `SystemCallFilter` reached the running process.
Board: the CM5 (Raze #2). Both reads are read-only; no lock beyond the run's own is needed, nothing
is changed, and nothing is restarted.

## Hypothesis

On the image as built, `lemnosd` runs as the unit starts it, with the unit's syscall filter in force:
the kernel reports the process in seccomp filter mode, and systemd reports a non-empty filter for the
unit. A process in mode 0 (no seccomp) or 1 (strict), or an empty property, means the image's unit
does not carry the filter `docs/daemon.md` describes, and the run stops there and reports.

## The reads

| Read | Expected | What it shows |
|---|---|---|
| `grep Seccomp: /proc/$(pidof lemnosd)/status` | `Seccomp:	2` | the running daemon is under a seccomp filter (mode 2, filter) |
| `systemctl show lemnosd -p SystemCallFilter` | `SystemCallFilter=` followed by a non-empty list | the unit as loaded carries a syscall filter |

`pidof lemnosd` must name exactly one process; more than one, or none, is reported as found, and
neither read is taken.

## What the PC covers

The daemon's own allowlists and the helper's (`docs/daemon.md`, **Seccomp**) are tested on the PC.
What only the board shows is that the image's unit file applies the unit filter to the process the
image starts.

## Result

Not run. **untested on board**
