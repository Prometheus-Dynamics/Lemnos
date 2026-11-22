# SHM-BELL-LOAD: ten `--only stream` runs at machine load

Rev: 0d37cc55 (origin/next when the runs started). The item's condition: the shm bell and poll checks held their
invariant on every run of the stream stanza while the machine was under load of 60 or more, next to the lander's
trains and other workers' builds. No synthetic load was added (the machine's own load was enough).

Command, each run: `testing/qemu/run.sh --only stream`, with `KDIR` and `KERNEL` at the lander's pinned 7.3.5 kernel
(the read-only tree `lemnos-targets/qk/modules-36008ba4fda2e9e0`, read under `flock -s` on its `.lock`, inside a
`slot-run.sh 6` slot), `JOBS=6`, `CC="zig cc -target x86_64-linux-musl"`, `taskset -c 6-31`. KVM accelerated.
Load: 1-minute value from `/proc/loadavg` at each run's start and end, and sampled every 30 s.

| Run | Start load (1/5/15 min) | End load (1/5/15 min) | Lowest 1-min sample | rc | Bell waits timed out | Bell skipped | Poll skipped | Bell frames missing | Poll frames missing |
|---|---|---|---|---|---|---|---|---|---|
| 01 | 110.32 / 88.40 / 82.24 | 99.02 / 106.32 / 96.68 | 58.11 | 0 | 0 | 0 | 0 | 0 | 0 |
| 02 | 99.02 / 106.32 / 96.68 | 78.44 / 77.13 / 83.80 | 58.25 | 0 | 0 | 0 | 0 | 0 | 0 |
| 03 | 78.44 / 77.13 / 83.80 | 128.16 / 95.56 / 88.80 | 76.75 | 0 | 0 | 0 | 0 | 0 | 0 |
| 04 | 128.16 / 95.56 / 88.80 | 106.75 / 97.31 / 90.09 | 108.56 | 0 | 0 | 0 | 0 | 0 | 0 |
| 05 | 106.75 / 97.31 / 90.09 | 112.85 / 121.36 / 107.18 | 81.32 | 0 | 0 | 0 | 0 | 0 | 0 |
| 06 | 112.85 / 121.36 / 107.18 | 98.34 / 114.15 / 106.14 | 97.47 | 0 | 0 | 0 | 0 | 0 | 0 |
| 07 | 98.34 / 114.15 / 106.14 | 91.54 / 100.00 / 102.02 | 82.05 | 0 | 0 | 0 | 0 | 0 | 0 |
| 08 | 91.54 / 100.00 / 102.02 | 122.79 / 113.24 / 106.46 | 83.18 | 0 | 0 | 0 | 0 | 0 | 0 |
| 09 | 123.85 / 113.61 / 106.62 | 93.80 / 92.14 / 97.77 | 72.84 | 0 | 0 | 0 | 0 | 0 | 0 |
| 10 | 93.80 / 92.14 / 97.77 | 72.55 / 84.80 / 92.30 | 70.21 | 0 | 0 | 0 | 0 | 0 | 0 |

In all ten runs the stanza reported `ok` on the bell and poll frame checks, the reader counted every lost frame
(0 reported, 0 missing), and no wait timed out. The guest's own verdict line was `qemu: passed on kvm, only stream`.

Load note: every run started at a 1-minute load of 78 or more. Runs 01 and 02 each had one 30-second sample dipping
to about 58 during the run. The invariant held in those runs too; the dip is recorded, not hidden.

Interrupted attempts: several runs were stopped by the coordinator or by a session restart before they finished
(pool and lock-order changes). None of those produced a result, and none is counted above.
