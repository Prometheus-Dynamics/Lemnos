# HANG-UNDER-LOAD: open faults in the orion-link stanza under a starved guest

For the user. Product behaviour under starvation, not fixed here. The TODO row is `HANG-UNDER-LOAD`.

## Train 15 (ltrain 15, 2026-11-27): stuck call 1, open, not reproduced

The log is the lander's state/15/qemu.log, guest 1, the orion-link stanza at a host load of 74-80.
- Before the stanza began, the daemon's own `usb.function.state` call ran past its 2000 ms deadline, and identity's
  `system.identity.facts` (caller orion) did too.
- The stanza froze the lane threads, the abandoned call returned at guest time 28.137 s, and the rebind began.
- `bind gadget failed` came at 30.141 s, past its deadline. The instance kept across the rebind was let go, the
  host session was Lost at round 24, and UDC was clear for 4085 ms.
- FAIL lines: the paced session; "UDC stayed set"; "the kept instance was never let go early" (stuck call 1); stuck
  call 2's kept instance and offer.
- A later rebind at 34.157 s bound fine. In train 14 the same rebind took 11 ms.

The stanza alone under a CPU quota did not give this signature (below). From this train on, the stanza prints its
timeline, deadline lines, binds and lane-thread states after each stuck call (testing/crates/orion-link-check),
so a recurrence names where it stood.

## Reproduced under a CPU quota (orion-link alone)

`run.sh --only orion-link` as the lander's qemu lane runs it: 6 CPUs, JOBS 8, cargo 4, nice 10. The quota is
CPUQuota on the unit, in % of one CPU (600% is the lane's six cores). No load was added. The logs are
`/mnt/nvme-wd/lemnos/cache/stuckcall/q<quota>.log` on the lander machine.

| quota | result | signature |
|---|---|---|
| 600% | pass | |
| 300% | FAIL (q300.log) | the restart sequence: host session Lost after round 67 (from restart 5), then "the Orion session's round trips went on (0 since the stop)" for restarts 6-10 and kill at Exec, RootPhase and HostStart |
| 300% again | FAIL (q300b.log) | "kill at RootPhase: the start never reached RootPhase" |
| 150% | pass | |
| 75% | pass | |
| 37% | FAIL (q37.log) | stuck calls 1 and 2: bind gadget ok, UDC stayed set, the kept instance was taken up; the host session was Lost after round 77 (call 1) and at round 10 (call 2); FAIL "paced host session: every round trip came back" |
| 18% | FAIL (q18.log) | as at 37%: Lost after round 193 and after round 15 |

## A question about the test's premise

Even in passing runs (600%, with the new diagnostics), the deadline line the stanza counts as "the gadget's call
runs past its deadline" is the gadget's own tick (`its tick ran past its 2000 ms deadline`), not the stanza's
`pair_lent` call. The rebind then starts on new lane threads about 3 s after the freeze, while the frozen ones are
still stopped (state t). In train 15 the counted line was the daemon's own `usb.function.state` call. Whether the
stanza counts the right line is a question about the test as well as about the product. The check is unchanged.
