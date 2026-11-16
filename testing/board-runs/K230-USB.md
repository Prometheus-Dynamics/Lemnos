# K230-USB board run: the USB-boot path of a K230D AI-CAM (board B)

Status: untested on board. This is the plan for review. Nothing in this change has loaded, written or reset
any board. Board A is powered off and out of everything; its earlier results are ignored.

Board B is a K230D AI-CAM on the user's word; both boards are the same model. This is not confirmed by a
board read (the user is asleep). Stage 3's kernel boot waits for the reviewer's facts (below).

## Preconditions and accepted risks

- **P0 (permission).** Opening board B's download device from this PC needs the user's word. The udev rule
  `/etc/udev/rules.d/60-atlas-usbboot.rules` for `29f1:0230` exists; nothing to add.
- **P1 (review).** The reviewer has read this plan. Each numbered stage below has its own condition.
- **P2 (keep-alive, held by the user).** The user holds board B alive with a 2 s `GET_CPU_INFO` loop. It is
  stopped by the user immediately before the first load, so two processes never talk to the device at once.
- **P3 (accepted: first-load risk).** If the stage does not come up and a watchdog reset boots the card, the
  cost is one KEY press in the morning. The card has no value; it may be wiped.
- **P4 (accepted: stage 3 risk).** Stage 3 is the step whose failure may not return unattended: no SDK
  watchdog is enabled in U-Boot's device tree, and none was found armed by SPL or U-Boot (UNVERIFIED), so a
  hang in SPL or U-Boot stays hung until hands. The reviewer accepts one KEY press in the morning for it.
- **P5 (the image gate).** The probe image is an existing, never-booted `k230d-ai-cam` `sdcard.img` from the
  image builder. The host refuses to write any image whose SHA-256 is not the one named on its command line.
  **Placeholder: the full SHA-256 is pending from the coordinator (it begins `d112bd47`).** Until it is
  filled in here, nothing is written.

## Board B: measured

- Boot device at USB port path **`3-4.2.1.4.1`** (bus 3). The port path is the only binding: the reply carries
  no chip identity. Nothing is selected by VID:PID alone. The host tool refuses a device at the path that is
  not `29f1:0230`, refuses a run with no path, and finds the same path after each re-enumeration.
- `GET_CPU_INFO` answers **exactly 4 bytes, `K230`**. The reply is short; the host accepts a short reply, and
  the sim's ROM answers the same four bytes.
- Bulk endpoints OUT `0x01` and IN `0x81`, 512-byte packets, one vendor interface (class `ff`), protocol
  `0x50` (measured on board A; board B's descriptors are read by the host before the first load, and the
  plan requires them to match).
- Console: CH342 dual serial, serial `5B08068863`: `/dev/serial/by-id/usb-1a86_USB_Dual_Serial_5B08068863-if00`
  and `-if02`. The host opens the console read-only for capture, and captures both ports. The first port
  (`if00`) is the U-Boot and Linux console (the SDK's quick-start). Stage 3 alone writes to `if00`, to interrupt
  autoboot (below).
- The ROM is silent on the console while idle in USB boot.
- It has an SD card. It was entered into USB boot by KEY held through RESET.
- Its ROM stopped answering after a while in the earlier session (EPROTO, still enumerated, between 96 and
  112 s of polling every 20 to 30 s). The working model is an idle timeout that host traffic resets; UNVERIFIED.
  The plan therefore identifies immediately before each step, and holds the device with traffic at least every
  2 s while the ROM is waiting for a load. EPROTO on identify is the named error `Deaf` (`UsbError::Deaf`), not
  "absent" and not a generic USB error. A deaf ROM is not retried blindly.

## What the vendor documents, and what is not observed

The vendor's burning-tool user guide (kendryte.com, the USB burning section) states the ROM's order: storage boot
is tried first and fails; then USB burning mode is entered when a USB connection is seen; the ROM function only
downloads to RAM and runs. **Documented by the vendor, not observed by us.** No public document gives a ROM
timeout or a ROM watchdog. Which pins or straps select the boot source is not in any source (the boot mode in
`soc_boot_ctl` bits 1:0, `sysctl_boot.c:9-23`, is latched at reset).

## Where the stage runs, and what the ROM leaves us

- The ROM downloads the stage to `0x80360000`, inside `0x80300000..=0x80400000` (`k230_flash.cpp:109`, the
  vendor tool's range). The SDK's SRAM starts at `0x80000000` (`src/little/uboot/arch/riscv/cpu/k230/platform.h:54`).
  That the range is SRAM and not DDR, so the stage runs without DDR init, is UNVERIFIED: no SDK source names the
  memory the ROM download targets on the K230D. The U-Boot DDR config is `CONFIG_DDR_SIZE 0x8000000` (128 MiB),
  LPDDR4-2667, initialised by U-Boot's own DRAM code (`dram.c`), not by the ROM.
- The stage is linked at `0x80360000` (`drivers/k230-usbboot-image/image.ld`), not position-independent.
- The console the stage prints to before USB comes up is the first UART, `serial0 = serial@91400000`
  (`k230.dtsi:99`, 115200 8N1; the vendor's console, UNVERIFIED on this board). Whether the ROM leaves it
  configured is UNVERIFIED; the stage configures it minimally if it does not (step L).

## The SD card's boot area (what the ROM and SPL read), sourced

From the SDK's `board/common/gen_image_cfg/genimage-sdcard.cfg` (7e302f7):
- SPL copy 1 at **offset 1 MiB, sector `0x800`** (`0x400` sectors), the `uboot_spl_1` partition;
- SPL copy 2 at **offset `0x180000`, sector `0xc00`** (`0x800` sectors), the `uboot_spl_2` partition;
- the U-Boot environment at `0x1e0000` (size `0x20000`); U-Boot proper at **2 MiB, sector `0x1000`**; the boot
  partition at 30 MiB.

The SPL's boot list on SD is set by the boot mode (`k230_spl.c:65-68`): for `SYSCTL_BOOT_SDIO1` it tries
`BOOT_DEVICE_MMC2` then `BOOT_DEVICE_MMC1`. Which controller is `MMC1` and `MMC2` is UNVERIFIED here.
The firmware header that the ROM checks is made by host tools with no source in the SDK (`k230_priv_gzip`,
`firmware-gen`); its magic and layout are **UNVERIFIED** until read from the probe image. The sectors the ROM
reads on SD are not in any SDK source (the ROM is mask ROM): **UNVERIFIED**; the plan takes the SPL copies'
sectors as the candidates, and blanks more than the header would need (below).

## Stage L: the first load (the stage banner, the card report; no write)

Accepted risk: P3.

The first image is as small and as observable as the stage can make it. Before it touches USB or SD:
1. the watchdogs its mode names are armed and read back (wdt0 TOP 11, as the entry does, `image.rs`);
2. the stage prints a banner and its progress on the UART (`0x91400000`, 115200 8N1), one line per step: boot
   mode read, USB device up, card check, report sent. A stage that hangs before USB comes up still shows
   where it hangs on the UART.

Then the stage, with nothing written:
- reads the boot mode (`soc_boot_ctl`, `0x91102040`) and reports it;
- reads the **card report**: CID, CSD (capacity), the sectors `0x800` and `0xc00` (first 512 bytes of each SPL
  copy), a CRC-32 of each, and a **verdict with its reason**. The verdict says whether the first 512 bytes of
  a copy carry the firmware header's magic (the image's magic is UNVERIFIED until the probe image is read, so
  the verdict reports the bytes, the CRCs and the magic as found, and does not guess a boot decision beyond
  "non-erased" / "all 0xff");
- sends the report on the bulk IN endpoint (a card record, `telemetry::card`), and prints it on the UART.

The card report is the only thing reviewed before the next step.

Gate after L (reviewer):
- **Blank (all `0xff`) in both copies:** the card holds no SPL header. The proving runs go ahead (Stage R).
- **Not blank (any non-`0xff` byte in either copy):** the card may be bootable. The reviewer ruled (approved in
  advance): the **blank boot area** step runs next (Stage B), then the proving runs.

## Stage B: blank the boot area the ROM reads (approved in advance, no further word)

Runs only after a non-blank report. Writes only these sectors, and nothing else on the card:
- **sectors `0x800` through `0x807` (4 KiB) and `0xc00` through `0xc07` (4 KiB)**: the first 4 KiB of each SPL copy.
  This covers the candidate header and any header up to 4 KiB, in both copies the SDK's layout places. Which
  sectors the ROM reads is UNVERIFIED; the set is a superset of the header block, and nothing outside it is
  written. The U-Boot environment, U-Boot proper and the boot partition are not touched.
- Each sector is written with `0xff`, then read back, and every byte of the read-back is checked to be `0xff`.
  A mismatch, or a card error, is a failure reported as a **card fault candidate** with the record, and the
  step stops (no retry into a card that refuses).
- The stage reports the read-back result on the bulk IN and on the UART, then the report is re-read to confirm
  the verdict is now blank.

## Stage R: the proving runs (no storage writes)

Only after Stage L's report is blank, or Stage B's read-back is blank. The probe image is the one stage image
(`lemnos-k230-usbboot-image`, `check.sh`, linked at `0x80360000`), with the mode word patched by the host. None
of the runs writes the card. The runs go from all paths armed to one path, so the first is the one most likely
to return the board.

Each run is one host process, attached across the reset: before the reset it identifies, then polls the port
path every 100 to 250 ms, and loads the next image as soon as the ROM answers. Each run ends with the board
back in USB boot and listening (`K230` confirmed after the return).

| Run | Mode word | Armed | Expected path | Predicted time before the run | Telemetry the image sends | Console (second witness) | If it does not return |
|---|---|---|---|---|---|---|---|
| R1 | `all` (1 + 2) | wdt0 TOP 11 + wdt1 TOP 10 + deadline | wdt1 (first to run out) | `2^26/f_wdt`: 0.67 s at 100 MHz to 67 s at 1 MHz | boot record, samples (CR and CCVR of both WDTs, time CSR), "taking deadline" if the deadline fires | the reset banner, if printed | the board is enumerated, deaf, or off the bus, or boots the card. One power cycle by hand (a KEY press); nothing on the card changes. The night's work ends. |
| R2 | `wdt0` (1) | wdt0 TOP 11 + deadline | wdt0 | `2^27/f_wdt`, narrowed by R1 | as R1 | as R1 | as R1 |
| R3 | `wdt1` (2) | wdt1 TOP 10 + deadline | wdt1 | `2^26/f_wdt`, narrowed by R1 | as R1 | as R1 | as R1 |
| R4 | `spin` (0) | deadline only | deadline | `2^28/f_time` plus R5's return | as R1; "taking deadline" before the SoC reset | as R1 | as R1 |
| R5 | `soc` (4) | none; SoC reset at once | soc | the ROM's return time, measured here | boot record; "taking soc" | as R1 | as R1 |

- R2 to R5 run only if the run before returned the board and it listened.
- Verdicts (`lemnos_k230_usbboot::probe::report`): expected path, predicted time, last telemetry, console
  banner time when printed, actual return, verdict. `MATCHED` within 5% or 0.25 s of the predicted expiry;
  `UNEXPECTED`; `EARLY`/`LATE` with the path and by how much; `ambiguous ... (telemetry missing)` only when no
  telemetry came back and more than one window fits; `DEAF` when the ROM answered EPROTO after the return;
  `FAILED TO RETURN` when no return came within the wait.
- **The return is unknown.** After a watchdog or SoC reset with a card that is blank (Stage L or B), the
  vendor's order (documented, not observed) says USB burning mode. Whether it does is what R1 to R5 show.

## Stage 3a: the probe image on the card, to the U-Boot prompt (separate condition)

**Not tonight unless every condition below holds.** Stage 3 is the first write of an image of ours to the card,
and needs: the card report (Stage L) reviewed; P4 and P5 satisfied; the image's bootdelay read from the image
file (below) and found usable; the reviewer's word for the write.

**The bootdelay fact (before the write is even proposed).** The U-Boot environment and config are read from
the image file on disk, by the image builder's own files, and the value of `bootdelay` (the boot delay in
seconds) is written here before any write. Decision rule: `bootdelay` is greater than 0 and the console is
live, so autoboot can be interrupted: the write may go ahead. If `bootdelay` is 0 or negative, or the console
input is dead, **do not write the image tonight**. The coordinator reads the value from the image.
**Value: pending (the coordinator reads it from the image file).**

Sub-steps:
1. Write the image (`k230d-ai-cam` `sdcard.img`, sha256 gated, P5) to the card through the stage's write path.
   Read back the whole image and check its SHA-256 against the file's. A mismatch stops the step.
2. Reset. The host watches the console (`if00` and `if02`) from the first byte after reset.
3. **Interrupt autoboot.** The host opens `if00` READ-WRITE, for this stage only, and sends the interrupt
   character U-Boot expects (a space, or the configured stop string, from the image's config) repeatedly from
   the first console byte after reset, until the prompt `K230# ` appears or a bound expires. This is the only
   write to the console in this plan.
4. At the prompt, read-only commands only, each followed by a wait for the prompt, all captured:
   `bdinfo`, `version`, `mmc list`, `mmc dev 0` then `mmc info` (the AI-CAM has no eMMC, so whether `mmc0`
   answers is the identifying fact), `mmc dev 1` then `mmc info`, `printenv`, `fdt addr`, `fdt print / model`,
   `fdt print / compatible`, and anything else that identifies the board. Never `saveenv`.
5. If the prompt never appears and the kernel starts, the host records everything and does nothing more.

**The residual, stated plainly:** an autoboot that wins the race boots the vendor kernel once, unattended.
That kernel's device tree may be for the wrong board; the plan does not try to stop it beyond the interrupt.

**Stage 3a is done at the prompt.** `fdt print / model` shows U-Boot's own device tree (`k230d_canmv_bpi` on any
image of this boot chain), so it does not tell board models apart; `mmc0` is the identifying fact.

## Stage 3b: `boot` at the prompt (separate condition, the morning)

Runs only if **both** hold, which the reviewer checks:
1. the image builder states which device tree the image's kernel was built from, and that it is an AI-CAM device tree;
2. what U-Boot reports at the prompt is consistent with that (no eMMC on `mmc0`).

If either is unclear, stay at the prompt. If it boots: capture the whole console, never `saveenv`, never log in
and change nothing, and leave it running.

Failure to reach the prompt, or a hang in SPL or U-Boot, stays hung until hands (P4). If the board is lost to a
card boot at any point, the host keeps capturing the console (that shows what is on the card), writes the run
record, and does nothing that needs hands. No retries.

## The run record

One record per process: the commands sent, their timestamps, the USB results (including `Deaf` and the card
report), and the console bytes captured on both ports. Written to NVMe, not the Github HDD.

## UNVERIFIED items, and what settles each

| Item | Settled by |
|---|---|
| Whether a reset with a blank card returns to USB boot (vendor order, documented, not observed) | Stage L's blank report; R1 to R5 |
| Whether a reset with a bootable card boots the card (no return) | Stage L's report (bootable branch: Stage B first) |
| The sectors the ROM reads on SD, and the header's magic and layout | Stage L's report of the sectors; the probe image's header read |
| Which controller is `MMC1` and `MMC2` in the SPL, and the boot controller | the boot record, the lead's parameter, and `mmc dev` at the prompt (Stage 3a) |
| Whether 0x80300000..0x80400000 is SRAM (no DDR init needed) | the stage's banner and telemetry arriving with no DDR init |
| The ROM's deaf window; whether host traffic every 2 s prevents it | the 2 s keep-alive (P2) and the identify before each step; measured, not assumed |
| The WDT clock | R1 telemetry |
| Whether a WDT reset reaches the whole SoC or one core | R2 and R3 |
| The time CSR's rate (1 MHz assumed) | R1 and R4 telemetry |
| Whether `soc_glb_rst` resets the SoC and the ROM returns to USB boot | R5 |
| The ROM's return time (the soc window of 0 to 5 s is a bound) | R5 |
| The UART `serial0 = 0x91400000` at 115200 and whether the ROM leaves it configured | the stage's banner on the bench (Stage L) |
| The SD clock and response layout (the stage's SD host) | Stage L's card report (CID and CSD read) |
| The dwc2 device sequences and bulk transfers with interface 0 | the telemetry and the card report arriving over USB |
| The ROM's bulk convention (1000-byte pages versus 512-byte packets; short and zero-length packets) | matched to the vendor tool's source before the first load (see the code step); then the report arriving |
| The bootdelay (0 or negative stops stage 3) | the image file, read before any write |
| Whether autoboot can be interrupted at the first console byte | Stage 3a, step 3 |
| Whether an unattended kernel boot takes a wrong device tree | the image builder's statement and `mmc0` (Stage 3b) |
| Whether any SDK watchdog is armed by SPL or U-Boot (P4) | none found (UNVERIFIED); Stage 3a's hang, if any |
| The card's health (suspect after running hot) | Stage L's report: a CRC or read failure is a card-fault candidate |
