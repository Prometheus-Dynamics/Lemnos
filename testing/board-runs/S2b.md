# S2b: turning the security switch off needs an authorization, on the reference board

Untested on board. Run this on the reference board (CM5, Raze #2) in a local session, with the
board lock, after S2b merges. Nothing here touches the board's disks: the record and the view are in a
scratch directory the run makes itself, and the keys are a throwaway pair made for the run.

## What to show

1. Board file: `[security]` with `record` (scratch) and `keys` (a file holding the throwaway public
   key), a `system-security` device with `uses = { slots, flash, identity }`. Pass: `lemnos-ctl
   security security state` shows `serial <the board's serial>`, `trust root present` and the key id.
2. Turn the switch on (the board needs a verified default slot), then `lemnos-ctl security security
   request-off`. Pass: it prints target, generation, serial and the message in hex.
3. `lemnos-ctl security security off` with no authorization. Pass: refused, `no authorization given`.
4. Sign the message with the throwaway key (any Ed25519 tool), write the 108-byte record, then
   `off --authorization FILE`. Pass: the switch is off and the generation moved by one.
5. Replay the same file after turning on again. Pass: refused, `made for generation G, the switch is at M`.
6. A file signed for another serial, one with a flipped signature byte, one by a key not on the
   list. Pass: each refused with its own reason text, nothing changed.
7. Empty the keys file and restart the daemon. Pass: `state` says `trust root absent`, and `off`
   needs no authorization.

## Recording the run

Write the results under this file's heading, with the output of each `lemnos-ctl` call and the
board's kernel version. CM5 timings only. A failed step is written down with its output.
