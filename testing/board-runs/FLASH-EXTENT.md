# FLASH-EXTENT: first board use (run 1, 2026-11-21): STOPPED at R2

Status: **not passed.** R0 (the SoC-reset return) passed. R1 was not run. R2 (the report and its reset) stopped: the
image printed nothing on either console and the boot ROM stopped answering after the load. The figures the first
board use exists to record (`pmpcfg0`, `pmpaddr0`, the `stack:` figure, the paint-and-scan result) were **not read**
and are UNKNOWN. Nothing was written to the card, the ROM's settings or the environment. The board is back at the ROM.

Read-only, RAM-only: no card I/O, no SD controller (`--sd-controller` not given), no `saveenv`, no reboot-to-upgrade,
no DFU. Consoles were read only. Lock: `/run/user/1000/lemnos-k230d.lock`, held for each run and released at exit.

## Rehearsal (PC, no board)

- Rev `57b45592`, worktree on NVMe (`/mnt/nvme-wd/lemnos-wFLASHEXT`), target `/mnt/nvme-wd/lemnos-targets/FLASH-EXTENT`.
- `drivers/k230-usbboot-image/check.sh` at this rev: entry check, link checks and stack layout hold. Image 70,196 bytes,
  sha256 `4f95bf9b…a1112` before the host's mode patch.
- `cargo test -p lemnos-k230-usbboot-stage -p lemnos-k230-usbboot`: exit 0 (the crates' stand-in tests, including
  `bss_zeroed`).
- `drivers/k230-usbboot-image/pmp-rehearsal/run.sh all` on qemu: `pmp-true` and `pmp-false` PASS; `fail-naive` and
  `fail-broken` CAUGHT.
- Limit: the host tools have no process-level stand-in. The host run was rehearsed through the crates' test stand-ins only.

## Timeline

| Time (local, 2026-11-21) | Step |
|---|---|
| 03:57:40 | start; lock held; port check `3-4.2.1.4.1` = `29f1:0230`, product `K230 USB Boot Device`, devnum 57 |
| 03:57:40 to 03:57:41 | R0 `soc` (`k230-return-probe --mode soc`): identify `K230`; load 70,196 bytes at `0x80360000`; start |
| 03:57:41 | R0 verdict PASS; back at ROM, same port, devnum 66 |
| 03:58:36 | start R2; port check again: devnum 66 |
| 03:58:36 to 03:58:47 | R2 `report` (`k230-stage-report`, no SD, both consoles read only): identify `K230`; load at `0x80360000`; start at +0.325 s |
| 03:58:47 | R2 record written; back at ROM, same port, devnum 67 |

## Figures

| Item | Result |
|---|---|
| R0 `soc` | PASS. Device gone 0.204 s after the start; back as `K230` at 0.536 s; same port. Record: `R0-soc.txt` |
| R1 `report` (watchdog return) | Not run at the time of the R2 STOP. Re-ruled by Helios (yes), floor re-derived in `K230-USB.md` (the R2-era floor was 100 s, from 1 MHz). See the R1 section below for the result |
| R2 `report` (report and reset) | **STOP.** No end line, no console line on either port in 10.47 s, no record packet. The boot ROM stopped answering identify at 1.055 s after the load (EPROTO). The device left the bus at 1.255 s, about 0.93 s after the start. Record: `R2-report.txt` |
| `pmpcfg0`, `pmpaddr0` | **UNKNOWN.** The `pmp:` line was not printed. Whether the PMP read ran, trapped or never ran is not established |
| `stack: <used> of 16384 bytes` | **UNKNOWN.** The end line was not printed. The 8192-byte rule was not tested |
| Paint and scan below the guard | **UNKNOWN.** `below-guard:` line not printed |
| Image size | 70,196 bytes at `57b45592` on this PC (`check.sh`, rustc 1.99.0). `docs/k230-flash-extent.md` said 67,996. The 2,200-byte difference is **not explained**. The image end is `0x80371270` (doc: `0x803709d0`); spare below `0x80380000` is 60,816 bytes (doc: 63,024) |

## What the R2 silence does and does not show

- It does not show what the image did. The console got no byte in the window. Either the image stopped before its first
  print, or the console path did not carry output. Not established.
- The first thing the report mode does after arming is the PMP read in its trap window (`drivers/k230-usbboot-sys/src/pmp.rs`).
  That path ran only in qemu. The board's trap behaviour is untested, and R2 is the first board run that reaches it. This is a
  candidate, not a finding.
- The departure at about 0.93 s after the start does not match the 1.342 s that TOP 11 gives at 100 MHz. The earlier R1
  runs in the K230-USB notes also left at about 0.93 s, and their spread was left open there. Not resolved here.
- `k230-stage-report` exited 0 on this STOP. Its exit code does not carry the verdict; the record does. A host fix is
  needed before a run is judged on exit code.

## Board state left

- 3-4.2.1.4.1, `29f1:0230`, product `K230 USB Boot Device`, devnum 67 (sysfs, read after R2). The identity was not
  re-read after R2: that would be a further contact the run did not plan.
- Card: never touched (no SD controller, no card write). `saveenv`, reboot-to-upgrade and DFU: not used.
- Lock: released.

## Next, for the coordinator

1. Re-rule R1's bound for the 100 MHz watchdog, or hold R1.
2. Decide on a second R2 only after the silence is explained. The next step is a host-side check of the report path in
   qemu with the same entry, not another board contact.
3. The `k230-stage-report` exit code on a STOP.
4. The 2,200-byte size difference, and the stack figure, which still needs a run that prints its end line.

Records (NVMe, not the HDD): `/mnt/nvme-wd/lemnos-targets/FLASH-EXTENT/run1/R0-soc.txt`,
`/mnt/nvme-wd/lemnos-targets/FLASH-EXTENT/run1/R2-report.txt`. Status timeline and hypothesis (HDD, notes):
`K230B2/FLASH-EXTENT-run1.status`, `K230B2/FLASH-EXTENT-run1.md`.
