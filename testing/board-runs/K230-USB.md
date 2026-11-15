# K230-USB board run: the USB-boot path of a K230

Status: untested on board.

No board has been touched for this change. This file is the whole plan for the bench: the return
paths proved one at a time (step R), then the steps that use them. For each step: what it does, what
it expects, what stops it, and how the board comes back without hands. The lead reviews the plan
before any step that writes or depends on a return path. The facts it relies on, with their sources
and UNVERIFIED marks, are in [`docs/flashing.md`](../../docs/flashing.md).

## What is in this PR, and what the run can use

- **Built and tested in simulation and unit tests:** the host protocol and the flasher; the stage's
  logic; the SD card protocol and the medium; the stage's USB device path (`lemnos_k230_usbboot_stage::usb_dev`,
  driven by a scripted host that fails the ways hardware does); the watchdogs' arming, readback and
  model; the SoC software reset; the start's observation rule; the libusb transport (no device opened).
- **Built, not run on any board:** the entry images in `drivers/k230-usbboot-sys`. The production entry
  (`lemnos_k230_usbboot_entry`) arms both watchdogs, reads them back and spins with the deadline. The
  five probe images (`lemnos_probe_all`, `lemnos_probe_wdt0`, `lemnos_probe_wdt1`, `lemnos_probe_spin`,
  `lemnos_probe_soc`) are the runs of step R. All build for `riscv64imac-unknown-none-elf`.
- **The dwc2 device driver** (`drivers/k230-usbboot-sys/src/dwc2.rs`) is built and compiles; its register
  sequences are from the Linux driver in the SDK, cited in the file. It has not talked to a host.
- **Not built:** the stage's image with its main loop (the medium, the device path and the stage are
  tested together in the simulator, not linked into a board image). Steps 3 and 4 need that image.

The board lock: the session takes the lock named in `AGENTS.md` before any contact with the board.
This is a K230, not the reference board, so no timing from it is compared with the reference board's.
The run uses the board's download mode over its USB port only, plus the read-only boot-mode check
below on the board's own system, if the board boots one.

## The return paths, and what each one depends on

The rule: no board test may leave a state that only a person's hands can undo. The board can come
back without hands by four paths. Each is independent of the others, and each depends on different
facts. The plan proves them one at a time (step R) before any step that relies on one.

| Path | Armed by | Returns the board when | Depends on (UNVERIFIED unless noted) |
|---|---|---|---|
| **wdt1** | the entry: TOP 10 (`0xaa`), kick, enable, read back | `wdt1` runs out (`2^26` cycles of the WDT clock) | the WDT clock; that a WDT reset reaches the SoC (not in any source) |
| **wdt0** | the entry: TOP 11 (`0xbb`), kick, enable, read back | `wdt0` runs out (`2^27` cycles) | the same two facts, for `wdt0` |
| **deadline** | the spin in the entry: no feed, the core's `time` CSR against `2^28` ticks | `2^28` ticks of the time CSR pass, then the SoC reset | the time CSR's rate (1 MHz assumed); the SoC reset (next row) |
| **soc** | nothing: the entry writes `soc_glb_rst` (`0x91102060` = `0x10001`) | at once; the ROM returns after its boot | that `soc_glb_rst` resets the SoC and returns the ROM to download mode with the medium's header invalid (`sysctl_boot.c:43-49`; `cpu.c` `do_reset`) |

What **none of them** can rely on: a reset-cause register. The SDK has none. `soc_wakeup_src`
(`sysctl_boot.h:53`) is the wake-up source, and `reserved0` (`sysctl_boot.h:46`) is undocumented; a
scratch word that survives reset is not shown anywhere. So the only evidence a host can read after a
return is the **elapsed time** from the start request to the re-enumeration, compared against each
path's window. The probes report nothing else.

The windows are formulas, with the clocks taken from the first run where they can be measured:
- wdt1: `2^26 / f_wdt` seconds; wdt0: `2^27 / f_wdt`; deadline: `2^28 / f_time`, plus the SoC reset's
  return time; soc: the ROM's return time, which is UNVERIFIED (the window used is 0 to 5 s, a bound
  chosen, not measured).
- `f_wdt` is taken from 1 MHz to 100 MHz until run 1 measures it. Over that range the windows overlap
  (for example at 100 MHz the wdt1 window is about 0.6 to 0.7 s, and the soc window starts at 0), so
  the host does not decide between paths it did not arm. Each run classifies only among the paths it
  armed (`probe::RUNS[*].candidates`), and reports `ambiguous` when two of them fit.
- A failed return (no re-enumeration within the wait) is the failure named for each run below.

## Step R: prove the return paths, one at a time (nothing written to storage)

Storage writes need a separate go-ahead, after this step. None of the five runs writes the medium:
each sends a probe image to RAM with the download requests, starts it, and waits for the device. A
probe image is not the stage, so the blocking `load_and_start` is not used: its observation would
report the ROM's answer as a failure. The host calls `probe::run_probe` for each run, and prints the
row with `probe::report`.

The runs go from all paths armed to none armed, one path removed per run. The first run therefore
has the most paths armed and is the one most likely to return the board.

| Run | Image | Armed | Expected path (named before the run) | Expected window (s) | Actual (elapsed, from the host) | If it does not return |
|---|---|---|---|---|---|---|
| R1 | `lemnos_probe_all` | wdt0 + wdt1 + deadline | wdt1 (the first to run out) | `2^26/f_wdt` ±10%; formula above | `probe::report` prints it | Every path failed: wdt0, wdt1 and the deadline all did not return the board. One power cycle. Nothing changed on storage. |
| R2 | `lemnos_probe_wdt0` | wdt0 + deadline | wdt0 | `2^27/f_wdt` ±10% | `probe::report` | wdt0 and the deadline both did not return the board. One power cycle. Nothing changed on storage. |
| R3 | `lemnos_probe_wdt1` | wdt1 + deadline | wdt1 | `2^26/f_wdt` ±10% | `probe::report` | wdt1 and the deadline both did not return the board. One power cycle. Nothing changed on storage. |
| R4 | `lemnos_probe_spin` | deadline only | deadline | `2^28/f_time` plus the SoC return, ±10% | `probe::report` | The deadline did not reach the SoC reset, or the time CSR does not advance. One power cycle. Nothing changed on storage. |
| R5 | `lemnos_probe_soc` | soc reset only | soc | 0 to 5 s (the bound; UNVERIFIED) | `probe::report` | `soc_glb_rst` did not reset the board. One power cycle. Nothing changed on storage. |

Each row's "actual" field is written by the host from the measured time: the expected path, the window
and the actual time are on one line of the report. A return classified as an unexpected path is a
finding, not a pass: the lead decides before the next run.

**The cost, stated so it can be decided.** Every run that does not return costs one power cycle of
the board, by a person. Nothing else changes: no run writes storage, and no run leaves a state that
a second run needs. The reviewer decides whether one power cycle per failing run is accepted for this
board. If it is not, R5 is not run, and R4 does not run either (it depends on the same SoC reset).

**Why this order.** R1 has the most paths armed and is the run most likely to return the board
without a person. Each later run removes one path, so a return in R2 or R3 proves the path that
remains, and R4 and R5 prove the deadline and the SoC reset with no watchdog to hide them.

**The exact host check for the run's clock.** After R1 returns, `f_wdt` is `2^26 / t_R1` if the return
is the wdt1 window (`probe::classify` decides; `t_R1` is the elapsed time). The timebase is measured by
R4 (`2^28 / t_R4`, less the SoC return, which R5 measures).

## Read-only boot-mode check (before step R, on the board's own system if it boots one)

The SD controller the board boots from is not in any source the stage can read, and the stage refuses
to choose one (`lemnos_k230_usbboot_stage::sd::resolve_controller`). The host passes it as a parameter,
after this check. The check reads `soc_boot_ctl` (`0x91102040`) on the board's own system, and reads
nothing else:

    busybox devmem 0x91102040 32

Bits 1:0 give the boot mode (`sysctl_boot.c:9-23`): `0` NOR, `1` NAND, `2` eMMC, `3` SD card. For SD
card or eMMC, the controller is not decided by this check: it is the lead's parameter (`0` for
`sdhci0` at `0x91580000`, `1` for `sdhci1` at `0x91581000`, from `k230.dtsi:154-176`). The stage refuses
an SD or eMMC boot with no parameter, and a NOR or NAND boot, by name. If the board does not boot a
system that runs `busybox`, the check is not made, and the parameter is not passed.

## Step 1: download mode, no stage (untested on board)

Connect the board in its download mode. Open the device with `RusbIo::open_download` and claim
interface 0.
- Expect: VID `29f1`, PID `0230`; the CPU info is `K230` (the boot ROM).
- Stop if the device is not there, or it answers anything else. No write.

## Step 2: the production entry, all paths armed (untested on board; after step R)

Send `lemnos_k230_usbboot_entry` at `0x80360000` with the raw requests, and time the wait. Expect a
return by wdt0 or wdt1 (whichever is first, the same window as R1), with the device back as `K230`.
Record the time and the path. Stop if the device does not come back within 269 s. Repeat three times.

## Step 3: arm and return with a silent host (untested on board; blocked on the stage image)

Start the full stage with a host that sends nothing after the start. Expect a reset within one period,
and the device back in download mode. The device path is built and tested with a scripted host; the
stage image is not linked yet, so this cannot run from this PR.

## Before any write: keep the medium

The first step that writes the medium is step 4. The flash erases the boot header first, so the
board's own image is lost unless it is copied. Before step 4, read the whole medium to a file on the
bench machine's NVMe or SSD (never the Github HDD), using the vendor's burner stage (not this stage),
read only, and record its SHA-256. Nothing in steps 1 to 3 writes the medium, so this copy is not
needed before them.

## Step 4: first write (untested on board; blocked; after steps R, 2 and 3, the medium copy, and review)

Write a test image to a spare region, never the board's own image (the medium copy is in place
first). Expect: the header is invalid after the erase; each piece reads back equal; the stage's
SHA-256 matches the image's; the header reads back as written; a reboot boots the test image. Then a
cut at each third of the write, with the board recovered each time by the return path. Stop at the
first mismatch. Not runnable from this PR (the stage image is not linked).

## Results

Read-only boot-mode check: not run.
Step R1-R5: not run. Each row's actual field is filled by `probe::report` on the bench.
Step 1: not run.
Step 2: not run (after step R).
Step 3: not runnable from this PR (the stage image is not linked).
Step 4 (first write): not run (after steps R, 2 and 3, and the medium copy below).

## Unverified on the board

- The WDT clock (step R1 measures it, with the window formulas above).
- Whether a WDT reset reaches the whole SoC or one core (R2 and R3 show it by the return time).
- The time CSR's rate (R4 shows it, given the SoC return from R5).
- Whether `soc_glb_rst` returns the ROM to download mode with the medium's header invalid (R5).
- The ROM's return time, which sets the soc window (R5).
- The boot SD controller and the SD clock (from the parameter; the clock is set by the device tree's
  `dummy_sd`, so no frequency is set).
- The SDHCI R2 response layout and the data timing (step 4).
- The dwc2 device driver's register sequences (`drivers/k230-usbboot-sys/src/dwc2.rs`): none has run.
  Its FIFO layout, the DTXFSTS width, and the first enable order are the first things a bench run shows.
- Whether the download device's bulk transfers work with interface 0 claimed and no kernel-driver detach.
- The ROM's answers on error and its load and memory limits; whether the ROM checks the image header.
- The start's outcome, settled by observation (the first full start).
- The stage's image is not linked: steps 3 and 4 cannot run from this PR.
