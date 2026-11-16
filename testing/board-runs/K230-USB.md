# K230-USB board run: the USB-boot path of a K230D AI-CAM (board B)

Status: untested on board. This is the plan for review. Nothing in this change has loaded, written or reset
any board. Board A is powered off and out of everything; its earlier results are ignored.

Board B is a K230D AI-CAM on the user's word; both boards are the same model. This is not confirmed by a board
read (the user is asleep). Stage 3's kernel boot waits for the reviewer's word.

## Preconditions and accepted risks

- **P0 (permission).** Opening board B's download device from this PC needs the user's word. The udev rule
  `/etc/udev/rules.d/60-atlas-usbboot.rules` for `29f1:0230` exists; nothing to add.
- **P1 (review).** The reviewer has read this plan. Each numbered stage below has its own condition.
- **P2 (keep-alive, held by the user).** The user holds board B alive with a 2 s `GET_CPU_INFO` loop. The user
  stops it immediately before the first load, so two processes never talk to the device at once.
- **P3 (accepted: first-load risk).** If the stage does not come up and a watchdog reset boots the card, the cost
  is one KEY press in the morning. The card has no value; it may be wiped.
- **P4 (accepted: stage 3 risk).** Stage 3 is the step whose failure may not return unattended. No SDK watchdog is
  enabled in U-Boot's device tree, and none was found armed by SPL or U-Boot (UNVERIFIED), so a hang in SPL or
  U-Boot, or in the kernel, stays hung until hands. The reviewer accepts one KEY press in the morning for it.
- **P5 (the image gate).** The probe image is the image builder's never-booted `k230d-ai-cam` `sdcard.img`:
  `/mnt/ssd-samsung/gaia/work/.gaia/examples/k230d-ai-cam/out/images/sdcard.img`, 1,276,116,992 bytes, SHA-256
  **`d112bd47bdfe58bed03d30e18a2c8f421b9c20952a03bd272293bb0cacc66e60`**. The host refuses to write an image
  whose SHA-256 is not this value, and the value is named on the command line.

## Board B: measured

- Boot device at USB port path **`3-4.2.1.4.1`** (bus 3). The port path is the only binding: the reply carries no
  chip identity. Nothing is selected by VID:PID alone. The host tool refuses a run with no path, refuses a device
  at the path that is not `29f1:0230`, and finds the same path again after each re-enumeration.
- `GET_CPU_INFO` answers **exactly 4 bytes, `K230`** (`4b 32 33 30`). The host accepts the short reply; the sim's
  ROM answers the same four bytes.
- Bulk endpoints OUT `0x01` and IN `0x81`, 512-byte packets, one vendor interface (class `ff`, protocol `0x50`),
  measured on board A; board B's descriptors are read by the host before the first load, and must match.
- Console: CH342 dual serial, serial `5B08068863`, at `/dev/serial/by-id/usb-1a86_USB_Dual_Serial_5B08068863-if00`
  and `-if02`. The host captures both ports read-only. The first port (`if00`) is the U-Boot and Linux console.
  Only stage 3a writes to `if00`, to interrupt autoboot.
- The ROM is silent on the console while idle in USB boot.
- The board has an SD card. It was entered into USB boot by KEY held through RESET.
- An earlier session's ROM went deaf (EPROTO, still enumerated) between 96 and 112 s of polling every 20 to 30 s.
  The working model is an idle timeout that host traffic resets; UNVERIFIED. The plan identifies immediately before
  each step and holds the device with traffic at least every 2 s while the ROM is waiting for a load. EPROTO on
  identify is the named error `Deaf` (`UsbError::Deaf`), not "absent" and not a generic USB error. A deaf ROM is
  not retried blindly.

## What the vendor documents, and what is not observed

The vendor's burning-tool user guide (kendryte.com, the USB burning section) states the ROM's order: storage boot is
tried first and fails; then USB burning mode is entered when a USB connection is seen; the ROM function only downloads
to RAM and runs. **Documented by the vendor, not observed by us.** No public document gives a ROM timeout or a ROM
watchdog. Which pins or straps select the boot source is not in any source (the boot mode in `soc_boot_ctl` bits 1:0,
`sysctl_boot.c:9-23`, is latched at reset).

## Where the stage runs, and what the ROM leaves us

- The ROM downloads the stage to `0x80360000`, inside `0x80300000..=0x80400000` (`k230_flash.cpp:109`). The SDK's SRAM
  starts at `0x80000000` (`src/little/uboot/arch/riscv/cpu/k230/platform.h:54`). That the range is SRAM, so the stage
  runs without DDR init, is UNVERIFIED: no SDK source names the memory the ROM download targets on the K230D.
- The stage is linked at `0x80360000` (`drivers/k230-usbboot-image/image.ld`), not position-independent.
- The console the stage prints to before USB comes up is `serial0 = serial@91400000` (`k230.dtsi:99`, 115200 8N1).
  Whether the ROM leaves it configured is UNVERIFIED; the stage configures it minimally if it does not.

## The SD card's boot area (sourced; the header magic verified on the probe image)

From the SDK's `board/common/gen_image_cfg/genimage-sdcard.cfg` (7e302f7): the first-stage SPL copy 1 at offset
1 MiB (sector 2048), copy 2 at `0x180000` (sector 3072), the stored environment at `0x1e0000`, U-Boot proper at 2 MiB
(sector 4096), the boot partition at 30 MiB. The SPL's SD boot list for `SYSCTL_BOOT_SDIO1` is `BOOT_DEVICE_MMC2`
then `BOOT_DEVICE_MMC1` (`k230_spl.c:65-68`); which controller those are is UNVERIFIED.

The probe image's own first-stage header is checked on the file: sector 2048 and sector 3072 both begin with the ASCII
magic `K230T1` (`4b 32 33 30 54 31`, then `03 00 00 00 00 00`, then `49 99 d7 e5 …`). Which copy the ROM reads first,
or whether it reads both, is UNVERIFIED. The sectors the ROM reads on SD are not in any SDK source (the ROM is mask
ROM). The firmware header's layout beyond the magic is made by host tools with no source in the SDK (UNVERIFIED).

## Stage L: the first load (the stage banner and the card report; no write)

Accepted risk: P3.

The first image is as small and as observable as the stage can make it. Before it touches USB or SD:
1. the watchdogs its mode names are armed and read back (wdt0 TOP 11, as the entry does, `image.rs`);
2. the stage prints a banner and its progress on the UART (`0x91400000`, 115200 8N1), one line per step: boot mode
   read, USB device up, card check, report sent. A stage that hangs before USB comes up still shows where.

Then the stage, with nothing written:
- reads the boot mode (`soc_boot_ctl`, `0x91102040`) and reports it;
- reads the **card report**: CID, CSD (capacity), the first 512 bytes of sectors 2048 and 3072 (CRC-32 of each), and a
  **verdict with its reason**:
  - **bootable**: `K230T1` at sector 2048 or at sector 3072;
  - **blank**: neither sector holds `K230T1`, and both are all `0xff` or all zero throughout (the 4 KiB header blocks);
  - **unknown**: anything else, which stops the run (reported with the CRCs);
- sends the report on the bulk IN endpoint (a card record, `telemetry::card`), and prints it on the UART.

The card report is the only thing reviewed before the next step.

Gate after L (reviewer):
- **Blank:** the proving runs go ahead (Stage R).
- **Bootable:** Stage B runs next (approved in advance; no further word), then the proving runs.
- **Unknown:** the run stops; the reviewer reads the CRCs.

## Stage B: blank the boot area the ROM reads (approved in advance, no further word)

Runs only after a bootable report. Writes only these, and nothing else on the card:
- **the 4 KiB header block at sector 2048 and the 4 KiB header block at sector 3072 (8 sectors each), zeroed.** That is
  the block that begins `K230T1` in each copy. Which sectors the ROM reads is UNVERIFIED; the zeroed set is a superset
  of the header block. The stored environment (byte `0x1e0000`, sector 3840), U-Boot proper (sector 4096) and the GPT
  partitions are not touched.
- Each sector is zeroed, read back, and every byte of the read-back is checked to be zero. A mismatch, or a card
  error, is reported as a **card fault candidate** with the record, and the step stops (no retry into a card that
  refuses).
- The stage reports the read-back on the bulk IN and on the UART; the report is read again to confirm **blank**.

## Stage R: the proving runs (no storage writes)

Only after a blank report (Stage L or B). The probe image is the one stage image (`lemnos-k230-usbboot-image`,
`check.sh`, linked at `0x80360000`), with the mode word patched by the host. None of the runs writes the card. The runs go
from all paths armed to one path, so the first is the one most likely to return the board.

Each run is one host process, attached across the reset: before the reset it identifies, then polls the port path every
100 to 250 ms, and loads the next image as soon as the ROM answers. Each run ends with the board back in USB boot and
listening (`K230` confirmed after the return).

| Run | Mode word | Armed | Expected path | Predicted time before the run | Telemetry the image sends | Console (second witness) | If it does not return |
|---|---|---|---|---|---|---|---|
| R1 | `all` (1 + 2) | wdt0 TOP 11 + wdt1 TOP 10 + deadline | wdt1 (first to run out) | `2^26/f_wdt`: 0.67 s at 100 MHz to 67 s at 1 MHz | boot record, samples (CR and CCVR of both WDTs, time CSR), "taking deadline" if the deadline fires | the reset banner, if printed | the board is enumerated and deaf, or off the bus, or boots the card. One power cycle by hand (a KEY press). Nothing on the card changes. The night's work ends. |
| R2 | `wdt0` (1) | wdt0 TOP 11 + deadline | wdt0 | `2^27/f_wdt`, narrowed by R1 | as R1 | as R1 | as R1 |
| R3 | `wdt1` (2) | wdt1 TOP 10 + deadline | wdt1 | `2^26/f_wdt`, narrowed by R1 | as R1 | as R1 | as R1 |
| R4 | `spin` (0) | deadline only | deadline | `2^28/f_time` plus R5's return | as R1; "taking deadline" before the SoC reset | as R1 | as R1 |
| R5 | `soc` (4) | none; SoC reset at once | soc | the ROM's return time, measured here | boot record; "taking soc" | as R1 | as R1 |

- R2 to R5 run only if the run before returned the board and it listened.
- Verdicts (`lemnos_k230_usbboot::probe::report`): expected path, predicted time, last telemetry, console banner time when
  printed, actual return, verdict. `MATCHED` within 5% or 0.25 s of the predicted expiry; `UNEXPECTED`; `EARLY`/`LATE`
  with the path and by how much; `ambiguous ... (telemetry missing)` only when no telemetry came back and more than one
  window fits; `DEAF` when the ROM answered EPROTO after the return; `FAILED TO RETURN` when no return came in the wait.
- **The return is unknown.** After a watchdog or SoC reset with a blank card, the vendor's order (documented, not observed)
  says USB burning mode. Whether it does is what R1 to R5 show.

## Stage 3a: the probe image to the U-Boot prompt, then a read-only capture (separate word)

**Not tonight unless the reviewer gives the word for 3a.** The image is the one named in P5.

Facts from the file (checked by the coordinator):
- the first stage (SPL with the ROM header) is at sector 2048 (1 MiB) and a second copy at sector 3072; both begin with
  `K230T1`;
- the stored environment is at byte `0x1e0000` (sector 3840), 64 KiB: `bootdelay=3`, `bootcmd=run blinux;`,
  `baudrate=115200`, no stop string. **Any key stops autoboot**;
- U-Boot proper at 2 MiB (sector 4096): SDK 2022.10 with the BPI board configuration (there is no AI-CAM U-Boot
  configuration). It expects the SD card as `mmc 1` (`ext4load mmc 1:1 …`). Console `uart0 = serial@91400000`, 115200 8N1;
- GPT: `boot` ext4 at sectors 61440–163839, `rootfs-a` 262144–1310719, `rootfs-b` 1310720–2359295, `data` 2359296–2490367;
- the kernel is mainline 7.3.3 with the image builder's own AI-CAM device tree, never run and not verified on any board,
  without pinctrl. No watchdog driver is enabled in the defconfig, and none was found armed (UNVERIFIED against the binary).

**The write rules.** The write is the whole image. This plan does not skip zero runs (a skipped run would need the card
region verified zero first, which this plan does not do). The two header sectors (2048 and 3072, `K230T1`) are written
**last**, after the whole image's read-back SHA-256 equals P5's value, so a cut or failure before them leaves a card the
ROM cannot boot from those sectors. A mismatch stops the step.

**The bootdelay fact** is read from the image file before the write (`bootdelay=3`). Decision rule: if bootdelay is 0 or
negative, or the console is dead, the image is not written.

Sub-steps:
1. Write the image; read back the whole image; check its SHA-256 against P5; then write and read back the two header
   sectors. Any mismatch stops the step.
2. Reset. The host watches the console (`if00` and `if02`) from the first byte after reset, and captures the whole SPL and
   U-Boot banner.
3. **Interrupt autoboot.** The host opens `if00` READ-WRITE, for this stage only, and sends a space (any key) repeatedly
   from the first console byte after reset until the prompt `K230# ` appears, or a bound expires. This is the only write
   to the console in this plan.
4. At the prompt, read-only commands only, each followed by a wait for the prompt, all captured, in this order: `version`,
   `bdinfo`, `mmc list`, `mmc dev 0` then `mmc info` (the AI-CAM has no eMMC: whether `mmc 0` answers is the identifying
   fact), `mmc dev 1` then `mmc info`, `printenv`, `fdt addr ${fdtcontroladdr}`, `fdt print / model`,
   `fdt print / compatible`, `gpio status -a` (an unknown-command reply is recorded). No `saveenv`, no `mmc write`, no `md`
   of device registers unless the reviewer adds it.
5. If the prompt never appears and the kernel starts, the host records everything and does nothing more.

**The residual, stated plainly:** an autoboot that wins the race boots the vendor kernel once, unattended. Its device tree
may be for the wrong board. `fdt print / model` shows U-Boot's own device tree, so it does not tell board models apart;
`mmc 0` is the identifying fact.

## Stage 3b: `boot` at the prompt (separate word, after 3a)

Runs only after 3a is captured, and only with the reviewer's word for 3b. Then `boot` at the prompt, with no environment
change and no `saveenv`. The worst realistic outcome is a hang at `rootwait` or silence after OpenSBI. The console capture
keeps running.

Whatever happens after `boot`, the board is left as it is and the capture keeps running. If it reaches a shell, the host
sends nothing at all (not even a newline) unless the coordinator adds a command. If it hangs, the host does **not** reset
it. A hang at SPL, U-Boot or the kernel stays hung until hands (P4).

A failure to reach the prompt, or a hang before it, stays hung until hands (P4). If the board is lost to a card boot at
any point, the host keeps capturing the console (that shows what is on the card), writes the run record, and does nothing
that needs hands. No retries.

## Observed ROM facts (for whoever takes the PC-side logic over)

Each fact: what was observed, how it was measured, and its status.

| Fact | Observed | How measured | Status |
|---|---|---|---|
| Identify reply (`GET_CPU_INFO`, request 0, value 0, 32 asked) | exactly 4 bytes, `K230` (`4b 32 33 30`) | read-only control transfer from the host on the download device | observed on board A and board B |
| Device descriptors | VID `29f1`, PID `0230`, manufacturer "Kendryte", product "K230 USB Boot Device", no serial, high speed, one vendor interface (class `ff`, protocol `0x50`), bulk OUT `0x01` and IN `0x81`, 512-byte max packet, no kernel driver bound | host descriptor read | observed on board A; board B's descriptors are read before the first load and must match |
| Idle behaviour | alive under 2 s identify polling for over 5 minutes (board B). An earlier session went deaf with EPROTO (`-71`, not a stall) while still enumerated: between 96 and 112 s when polled every 20 to 30 s, and at 65 s on another board | the host's identify loop, recording each reply and error | observed; the mechanism (an idle timeout that host traffic resets) is UNVERIFIED |
| Re-enumeration | the device number changes after a reset; the port path (`3-4.2.1.4.1` on board B, `3-4.2.1.2` on board A) does not | host enumeration before and after a reset | observed |
| Console while idle in USB boot | silent | the console capture | observed; the reset-time print is not yet captured |
| The ROM's order (storage, then USB burning mode when a USB connection is seen) | documented by the vendor (kendryte.com, burning-tool user guide, USB burning section) | not observed | documented, unobserved |

## The run record

One record per process: the commands sent, their timestamps, the USB results (including `Deaf` and the card report), and
the console bytes captured on both ports. Written to NVMe, not the Github HDD.

## UNVERIFIED items, and what settles each

| Item | Settled by |
|---|---|
| Whether a reset with a blank card returns to USB boot (vendor order, documented, not observed) | Stage L's blank report; R1 to R5 |
| Whether a reset with a bootable card boots the card (no return) | Stage L's report (bootable branch: Stage B first) |
| Which SD sectors the ROM reads, and which copy it tries first | Stage L's report and the probe image's boot behaviour |
| Which controller is `MMC1` and `MMC2` in the SPL, and the boot controller | the boot record, the lead's parameter, `mmc dev` at the prompt (3a) |
| Whether 0x80300000..0x80400000 is SRAM (no DDR init needed) | the banner and telemetry arriving with no DDR init |
| The ROM's deaf window; whether host traffic every 2 s prevents it | the 2 s keep-alive (P2) and the identify before each step |
| The WDT clock | R1 telemetry |
| Whether a WDT reset reaches the whole SoC or one core | R2 and R3 |
| The time CSR's rate (1 MHz assumed) | R1 and R4 telemetry |
| Whether `soc_glb_rst` resets the SoC and the ROM returns to USB boot | R5 |
| The ROM's return time (the soc window of 0 to 5 s is a bound) | R5 |
| The UART `serial0 = 0x91400000` at 115200, and whether the ROM leaves it configured | the stage's banner on the bench (Stage L) |
| The SD clock and response layout (the stage's SD host) | Stage L's card report |
| The dwc2 device sequences and bulk transfers with interface 0 | the telemetry and the card report arriving over USB |
| The ROM's bulk convention (1000-byte pages versus 512-byte packets; short and zero-length packets) | matched to the vendor tool's source before the first load (see the code step); then the report arriving |
| The bootdelay (0 or negative stops stage 3) | the image file, read before any write (`bootdelay=3`) |
| Whether autoboot can be interrupted at the first console byte | 3a, step 3 |
| Whether an unattended kernel boot takes a wrong device tree | the image builder's statement and `mmc 0` (3b) |
| Whether an SDK watchdog is armed by SPL or U-Boot (P4) | none found (UNVERIFIED); 3a's hang, if any |
| The card's health (suspect after running hot) | Stage L's report: a CRC or read failure is a card-fault candidate |
