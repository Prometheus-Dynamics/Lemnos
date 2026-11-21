# RAZE2-SWEEP: read-only and RAM-only sweep on Raze #2 (2026-11-21)

Board: Raze #2 (CM5 Rev 1.0), serial a317bcbee5226d57, boot b34fee0e-2dc9-4da1-a77f-791d01632baf, kernel 7.2.9-v8-16k,
reached over usb0 (fe80::7d:c4ff:fe9e:fe9a%usb0). Host key checked against the pin before any login: SHA256 RZsQKK…g6w
(private known_hosts, `~/.ssh/known_hosts` not touched). Both board locks taken with flock over ssh for every step, each
released when its command exited. Rev: origin/next 0d37cc55, branch pr/RAZE2-SWEEP.

Nothing was written on the board. No module loaded or unbound, no reboot, no `mem=`, no change to the default slot or
config.txt, no `.reboot-ok`, the watchdog never opened, no service stopped or restarted, Raze #1 never contacted.

Hypotheses were written to `$H/notes/RAZE2-sweep.status` before each step. The board clock has no RTC and reads
2026-11-08, so board timestamps are not wall-clock times.

## Items run

| Item | Record | Result |
|---|---|---|
| CMA reading (after boot) | [CMA-READ.md](CMA-READ.md) | Run. CmaFree 48308 kB of 65536 kB; 12 CMA blocks of order 9 or more. "After a day" not possible yet (uptime 4 h). |
| Seccomp reads | [DAEMON-SECCOMP.md](DAEMON-SECCOMP.md) | Run. **FAIL**: `Seccomp: 0` (expected 2); `SystemCallFilter=` empty (expected non-empty). Reported. |
| S-CAM | [S-CAM.md](S-CAM.md) | Run. **A sensor is present**: ov9782 at 10-0060, bound to driver ov9282, rp1-cfe video nodes registered. Bus not touched (driver owns the address). Camera rows held. |
| DTB-NAME-RUN-A | [DTB-NAME-RUN-A.md](DTB-NAME-RUN-A.md) | Partly run. `vclog` absent; the loaded DTB name is not readable on this image. Revision `c04180`. |
| SLOT-MACHINE-ID | [SLOT-MACHINE-ID.md](SLOT-MACHINE-ID.md) | Partly run: steps 1, 2 (findmnt), 3, 5. Machine-id `13fe474f…`, not the derived id. Steps 4 and 6 not run. |
| WDT-CHAIN | [WDT-CHAIN.md](WDT-CHAIN.md) | Partly run: journal read. Hand-over gap not in the log. Not measured. |
| BENCHREF | [BENCHREF.md](BENCHREF.md) | **Not run.** `bench.sh --real` stops lemnos* units (lemnosd) and writes to /var/tmp; reported first per ruling. |
| B8 checks 9 to 12 (phase 2) | [B8.md](B8.md) | **Held.** Needs the image's lemnosd stopped behind a failsafe; reported first. Checks 5, 6, 7 (dev half), 8 not run. |

## Untested-on-board rows in docs/hardware-status.md (origin/next 0d37cc55)

The list is the 47 rows with "untested on board" in that doc. Each is either run above or listed here with its reason.
"Held" means the row needs a camera stream or a stop of the image's services, and is reported.

| Record | Run? | Reason |
|---|---|---|
| Q2i | no | Needs the lemnos module's device nodes; the module is not loaded on the image. |
| F2g1 | held | Camera chain: needs a stream on the sensor (now present). |
| F2h3 | held | Camera frame period: needs a stream. |
| FLAKE-CSI2 | held | Camera STOP window: needs a stream. |
| G9-RAW-CLAIM | no | Claims a real GPIO line and PWM channel; needs a driven loop or scope (hands). |
| G11-BUS-LISTS | no | Raw-bus transfer lists need a device on a chip-selected bus; the image's bus use is the daemon's. |
| MAG-GATE | no | Needs a magnet moved near the magnetometer (hands). |
| Q2b | no | Device-tree maps for RK3566, RK3588, K230, A733: not this family. |
| Q2e | no | USB device-port maps for RK356x, RK3588, RK3576: not this family. |
| Q2f | no | VBUS change on a device port; boot-time overlay. Writes boot config. |
| Q2g | no | Boot holds and overlay; reboot. |
| S2 | no | Security switch: writes. |
| S2b | no | Security switch authorization: writes. |
| I5i3 | no | Boot-count fallback: reboots, brown-out, firmware counts. |
| I5i4 | no | Every-boot boot-file check: needs the slot's boot check code and a boot. |
| I5i5 | no | Writes boot pointers by raw sector; reboots; point 4 is another family. |
| BB2 | no | U-Boot restart count: no U-Boot on this image; reboots. |
| I5i8 | no | Payload bays on the boot partition: writes and boots. |
| I5i | no | Power cuts on the boot partition: hands and writes. |
| I5f | no | Trial resolution: reboots. |
| HEALTH-TCP | no | Trial boot with a tcp health source: reboot. |
| I5h | no | slot-init on a kernel lacking a piece: a trial boot. |
| W1b | no | system-flash on raw NAND: the board has no NAND; writes. |
| R1f | no | Lent gadget pair on a real controller: changes gadget binding. |
| U4 | held | isp600 tuning on the camera; vendor library; needs a stream. |
| K1d | no | A733 encoder: another family. |
| RKJPEGE | no | JPEG encoder on the RK356x: another family. |
| M36-deadline-grow | no | Expiry restarts the board; grows the data partition: reboot and writes. |
| M41b | no | Once-per-bind figure: needs device bind and bring-up timing through a daemon restart. |
| R1h | no | Gadget OS descriptors: gadget bind; host-side test only. |
| M32 | held | Camera link reads: needs a stream. |
| M36 | no | Watchdog gap at hand-over: needs a boot. |
| WDT-CHAIN | partly | See WDT-CHAIN.md. |
| SLOT-MACHINE-ID | partly | See SLOT-MACHINE-ID.md. |
| M41 | no | Second check: device bring-up through a daemon restart. |
| K230-USB | no | K230 board. |
| MR1 | no | RK3566 board. |
| FL1 | no | Flash wire on the RK3566 host path; another board. |
| FEL | no | Allwinner FEL: another board. |
| R2-CFG | no | Kit outside this repository; not a board step. |
| DAEMON-SECCOMP | run | See DAEMON-SECCOMP.md (FAIL). |
| R2-PREV | no | Pi previous slot at counts 4 to 7: sim-only by its own record; needs a boot path. |
| BOARD-USB0 | no | ZERO 3E board: not Raze #2. |
| RKJPEG-DEC | no | Decoder on the RK356x: another family. |
| E32 | no | NPU on the RK356x: another family. |
| K1b | no | VPU720 probe: another family. |

## Coordinator rows

| Row | Result |
|---|---|
| DTB-NAME-RUN-A | partly run; see its record. |
| S-CAM | run; sensor present; see its record. |
| BENCHREF | not run (stops lemnosd); reported. |

## Items reported, not run

- Camera rows (PISP-RUN, R-PASS, M34 lent chain) now that a sensor is present: need a streaming session on the image's
  capture path, which the image's helios-engine owns. Reported first, as ruled.
- B8 checks 9 to 12: need the image's lemnosd stopped behind a failsafe (reported).
- CMA "after a day": needs the board's uptime to reach 24 h.
