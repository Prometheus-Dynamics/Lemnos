# Flashing the K230 over USB

A K230 (and a K230D, which has the same boot ROM) is flashed through its boot ROM's USB download
mode. The boot ROM loads a RAM image over USB and starts it. Lemnos's path does this once, by hand,
and then the board is flashed over its own USB link. This page says what the code does, what it
proves, and what the vendor sources do and do not say. The board run that comes after is
[`testing/board-runs/K230-USB.md`](../testing/board-runs/K230-USB.md).

**Status:** the host protocol, the flasher, the stage's logic, the SD card medium, the stage's USB
device path, the watchdog design and the reset paths run in simulation and in unit tests. Nothing
here has run on a board. The stage image that links them is not built (see "What is not built").
Every fact the board depends on is listed below with its source, or marked UNVERIFIED.

## What is in the repository

| Crate | What it is |
|---|---|
| `testing/crates/k230-usbboot` (`lemnos-k230-usbboot`) | The host: the boot ROM's requests, the burner protocol, the flasher, the libusb transport (`usb`, opens a device only when asked), and a simulated board. No `unsafe` |
| `testing/crates/k230-usbboot-stage` (`lemnos-k230-usbboot-stage`) | The stage's logic, `no_std`: the wire format, the two watchdogs' registers, readback and model, the SoC reset constants, the medium trait, the SD card protocol (`sd`), and the stage's state machine. No `unsafe` |
| `drivers/k230-usbboot-sys` (`lemnos-driver-k230-usbboot-sys`) | The stage's entries (`naked_asm!`: the production entry and five probe images), the watchdogs' register access, the SDHCI host registers and the dwc2 device registers. The only `unsafe` code of the stage |

The stage's logic runs in the simulator as the board would run it. The entry image in
`drivers/k230-usbboot-sys` arms and reads back the watchdogs and spins with a deadline; five probe
images cover the return paths one at a time (see "The return paths" below). The stage image that
links the device path, the medium and the state machine is not built (see the board run).

## The flash, in order

The layout: the boot header is the first 4 KiB block of the medium; the payload starts after it.
The header is the only thing the ROM reads to decide whether to boot.

1. **Invalidate.** Erase the header block and read it back. No valid header may remain.
2. **Payload.** Each piece is erased, written, and read back equal. A piece is tried up to 3 times.
3. **Check the whole payload.** The stage hashes the payload on the medium (SHA-256), and the host
   compares it with the image's. This is before the header on purpose: a mismatch leaves no header.
4. **Header, last.** It names the payload (offset, size, digest). The host sends the 4 KiB block in
   bulk pages; the stage writes and reads back each page, and the header is complete at byte 512 of
   the first page. The header is read back and verified too.

A cut or reset at any point leaves one of two states: no valid header, so the ROM stays in download
mode and the board is recovered over USB; or a valid header over a complete, checked payload. The
simulator proves this at every medium operation and at 6 byte offsets inside each (see
`testing/crates/k230-usbboot/tests/flash.rs`). The header's own write is cut at **every operation of
the write and at every byte of each page** (`tests/header.rs`): no cut before byte 512 of the first
page leaves a header the ROM would boot, and a header it boots always names the verified payload.
The stage refuses an order the host gets wrong too: no write under a valid header, and no header
that does not name the last verified payload.

## The watchdogs

The stage arms **both** watchdogs, `wdt0` at `0x91106000` and `wdt1` at `0x91106800`, before anything
else runs (`lemnos_k230_usbboot_stage::wdt::arm`, and the entry's first instructions). Each is armed
with TORR `0xbb`, kicked, and enabled. Then each is read back: CR must read enabled, and CCVR must
move within 16 reads. A failed readback is not a spin: the entry takes the second reset path at once.

- Period `2^(16+TOP)` cycles, TOP 11, so `2^27` cycles. The WDT clock is UNVERIFIED. The plausible
  range is taken as 1 MHz to 100 MHz: the period is then between 1.34 s (100 MHz) and 134.2 s
  (1 MHz); 5.59 s at 24 MHz.
- **Which core a WDT reset reaches is UNVERIFIED.** The sources do not say whether a WDT system
  reset resets the whole SoC or one core. Both are armed for that reason. The big core's WDT is
  `wdt1`, which the SDK's RT-Thread driver uses (`board.h:171-175`).
- The stage feeds both after each verified chunk: a write piece that reads back equal, or a VERIFY
  chunk that was read and hashed. Nothing else feeds. A VERIFY that stalls past one period is reset
  by the watchdogs (a test: `tests/reset.rs`).
- The reboot command asks for a fast reset: TOP 0, `2^16` cycles on both, which is a reset within
  one short period.

### The deadline and the second reset path

- **The deadline.** The entry's spin does not feed. It is bounded by the core's `time` CSR against
  `SPIN_DEADLINE_TICKS`, which is `2 × 2^27` cycles of the longest period, i.e. 268 s if the timebase
  is 1 MHz (the timebase is UNVERIFIED; the bench step 2 measures it). In the stage the deadline is
  counted from the last kick (`DEADLINE_US`): a reset that never reaches the board is caught by it.
  The deadline is past every armed period, so a watchdog that resets the board resets it first.
- **The second path is the SoC software reset.** `soc_glb_rst` at `0x91102060` (the boot system
  controller at `0x91102000`) with `0x10001`: bit 0 is the reset, bit 16 its write enable.
  Sources: `sysctl_boot.c:43-49` (`sysctl_boot_reset_soc`), `sysctl_boot.h:34,47` (the base and the
  register), and the U-Boot reboot command, which writes `0x10001` to the same register at
  `SYSCTL_BOOT_BASE_ADDR+0x60` (`src/little/uboot/arch/riscv/cpu/k230/cpu.c`, `do_reset`). It does
  not use a watchdog. Whether it returns the ROM to download mode with the medium's header invalid is
  UNVERIFIED on the board.
- The SDK has no reset that is independent of both the watchdogs and this register. The
  `sysctl_reset(SYSCTL_RESET_WDT1_APB)` the RT-Thread driver uses to disable a watchdog
  (`drv_wdt.c:103-109`) resets the watchdog's own block: it is a watchdog-scoped reset, not the
  second path, and the entry never calls it.

The simulator models both watchdogs, their readback, the deadline and the SoC reset. Each reset cause
has a test that ends in the ROM back in download mode, with no hands: a CR that does not take (on
either watchdog), a readback that lies about a frozen counter, a reset that never reaches the board
(the deadline catches it, and a test shows that without the deadline the board would stay up), a
healthy stage that a silent host resets by its period, and a stalled VERIFY.

The stage does no waiting of its own: it answers one message at a time, and every loop in it is
bounded by the size it was given. The medium and the transport bound their own waits; the watchdogs
are the last deadline behind them.

## The SD card medium

The stage's medium for the K230 SD host (`lemnos_k230_usbboot_stage::sd`): the SD protocol's
initialisation (CMD0, CMD8 with its echo, ACMD41 bounded at 1000 tries, CMD2, CMD3, CMD9 for the
capacity, CMD7, CMD16), single-block reads (CMD17) and writes (CMD24) with a bounded busy poll
(CMD13), and erase by writing 0xff. Only SDHC/SDXC (block addressing) is accepted.

- The controller's register map is the SDHCI standard, as the SDK's `drv_sdhci.h` names it. The
  device tree gives two controllers: `sdhci0` at `0x91580000` and `sdhci1` at `0x91581000`, both
  `kendryte,k230-dw-mshc` (`k230.dtsi:154-176`). The code picks neither. The boot controller is chosen
  by `sd::resolve_controller` from the boot mode (`soc_boot_ctl` bits 1:0, `sysctl_boot.c:9-23`: 3 SD
  card, 2 eMMC, 0 NOR, 1 NAND) and a parameter the host passes (0 for `sdhci0`, 1 for `sdhci1`). An SD
  or eMMC boot with no parameter, a NOR or NAND boot, and an unknown parameter are each refused by name.
  The read-only check that reads the boot mode is in the board run.
- The clock is UNVERIFIED: the device tree gives it from a `dummy_sd` clock, so the host enables the
  internal clock and does not set a frequency. The bench step measures what the card sees.
- The long-response (R2) layout and the data timing follow the SDHCI standard and are UNVERIFIED on
  the board. The card protocol is tested with a fake card (`sd::tests`): a healthy card, a card that
  stays busy, a v1 card, a write the card never finishes, and the CSD's bit positions.
- Each write is read back by the stage before the stage feeds the watchdogs (`store`).

## The host transport

`testing/crates/k230-usbboot/src/usb.rs`: `UsbIo` over libusb through `rusb` 0.9 (libusb 1.0 is on
the build host; `rusb` is synchronous, which fits the blocking `UsbIo`). Its facts, from the vendor
tool at 33a75f0:

- The interface is claimed as interface 0 (`kburn.cpp:335`).
- The bulk endpoints are OUT `0x01` and IN `0x81` (`kburn_k230.h:13-14`).
- The vendor request types are IN `0xc0` and OUT `0x40` (vendor, device recipient;
  `burner_uboot.cpp:144-152`, `burner_brom.cpp:21`).
- The ROM's timeout is 1 s (`kburn_k230.h:11`), and a VERIFY's is raised to 300 s
  (`burner_uboot.cpp:392-396`). The VERIFY call sets the transport's timeout for its one call
  (`UsbIo::set_timeout_ms`), and restores it.
- `wait_for_device` waits for a device at the port the device was on before the start, opens it and
  claims its interface. It returns the IDs the device came back with.

The transport's error mapping and the port selection are unit-tested. `RusbIo::open_download` opens a
device and is never called by a test. **No device has been opened in this work.** Whether the
download-mode device's bulk transfers work with interface 0 claimed and no kernel-driver detach is
UNVERIFIED on the board.

## The start: the outcome is settled by observation

The start request's own result is not trusted, either way. After the image is sent and the start
request is made (whatever it returns: a disconnect, a normal return, or a stall), the host waits for
the device, for at most `START_WAIT_MS` (10 s), and settles it:

- **Success:** the device re-enumerates with the download IDs `29f1:0230`, and presents the stage's
  identity, `Lemnos stage for K230`.
- **Failure**, each named in its error: no device before the deadline (a timeout); a device with
  other IDs; the boot ROM answering again (the image did not run); the vendor burner answering (the
  wrong image); an identity text that is not the stage's; and a device that leaves the bus again
  after it re-enumerated.

The sim runs each branch with a test (`tests/bootrom.rs`), including a start that stalls (the device
settles it, not the stall). The earlier crate's behaviour (a disconnect is success) and the vendor
tool's (any failure at the start is fatal) are both replaced by this rule. The bench step 2 records
the outcome of each start.

## The stage's USB device path

`lemnos_k230_usbboot_stage::usb_dev`: the device logic, safe code, over a `UsbHw` trait. It presents
the download IDs (`29f1:0230`, the device descriptor), one configuration with a bulk IN `0x81` and a
bulk OUT `0x01` at 512 bytes (`kburn_k230.h:13-14`), and the stage's two vendor requests: GET_CPU_INFO
(`0xc0`, request 0, value 0: the identity, NUL padded to 32, `probe_device.cpp`) and the loader version
(request 0, value 1). The standard requests the host needs are answered: GET_DESCRIPTOR (device,
configuration, string 0), SET_ADDRESS and SET_CONFIGURATION. Anything else is stalled.

- The device layer never feeds the watchdogs. Bytes move between the hardware and the stage's
  `BulkEndpoints`; the stage feeds after a verified chunk, as before. A test checks that enumeration
  and the identity feed nothing, and that a write feeds once per verified piece.
- A bus reset abandons the stage's session (`Stage::abandon`) and flushes the IN endpoint; a write
  in progress is not finished by the data after the reset.
- The device-side hardware is `drivers/k230-usbboot-sys/src/dwc2.rs`: the dwc2 register sequences from
  the Linux driver in the SDK (`src/little/linux/drivers/usb/dwc2/hw.h`, offsets cited in the file),
  in slave mode, polled, with the OUT packets reassembled into a transfer. Its FIFO layout, the TX
  space width and the enable order are UNVERIFIED on hardware; none of it has run.
- The model (`tests/usb_dev.rs`) fails like hardware: no host (nothing is sent, nothing is fed), a
  stalled endpoint (the host's read is refused), an unknown request (EP0 is stalled), a bus reset in
  the middle of a write (the data after it is refused as a short command, and nothing is stored), a
  short packet (a refused command, no feed), and a core that does not come up.

## What is not built

**The stage image.** There is no `main` or linker script that links the device path, the medium and
the state machine into a board image. The entries arm, read back and spin; the stage's pieces are
tested together in the simulator. Steps 3 and 4 of the board run need that image.

## The return paths

Five probe images (`lemnos_probe_all`, `lemnos_probe_wdt0`, `lemnos_probe_wdt1`, `lemnos_probe_spin`,
`lemnos_probe_soc`) arm and spin in different combinations, so each return path is proved on its own
(the board run's step R). The windows and the classifier are `lemnos_k230_usbboot::probe`: each run
names its expected path, its window is a formula over the clocks, and the host classifies the elapsed
time only among the paths the run armed. No reset-cause register exists in the SDK (`soc_wakeup_src`
is the wake-up source; `reserved0` is undocumented), so the elapsed time is the only evidence.

## Facts, with their sources

The sources are the kendryte/k230_flash release `33a75f0` (v0.0.15, 2026-11-06; the burner) and
kendryte/k230_sdk `7e302f7` (the SDK: device tree, U-Boot, the RT-Thread drivers, the board header).
The burner's line numbers are its own; "C:" lines are `burner_uboot.cpp` at `33a75f0`.

| Fact | Value | Source | Status |
|---|---|---|---|
| USB VID / PID in download mode | `29f1` / `0230` | `kburn.h:205`; `kburn.cpp:403-405`; the SDK's burntool U-Boot config `src/little/uboot/configs/k230_canmv_burntool_defconfig:100-101` | Source-verified. The ROM's own descriptor is not in any source; the burner's IDs are the same |
| Requests | GET_CPU_INFO 0, SET_DATA_ADDRESS 1, SET_DATA_LENGTH 2 (not used), PROG_START 4 | `kburn_k230.h:19-25` | Source-verified |
| Bulk endpoints | OUT `0x01`, IN `0x81` | `kburn_k230.h:13-14` | Source-verified |
| Interface | 0, claimed with retries for 10 s | `kburn.cpp:335-348` | Source-verified |
| Control request types | IN `0xc0`, OUT `0x40` | `burner_uboot.cpp:144-152`, `burner_brom.cpp:21` | Source-verified |
| ROM timeout / VERIFY timeout | 1 s / 300 s | `kburn_k230.h:11`; `burner_uboot.cpp:392-396` | Source-verified. Whether the ROM's own timing fits is UNVERIFIED |
| CPU info | 32 bytes at request 0, wValue 0, wIndex 0; `K230` (ROM) or `Uboot Stage for K230` (burner) | `probe_device.cpp:9-61` | Source-verified |
| Load address | Any value in `0x80300000..=0x80400000`; the tool's default `0x80360000` | `k230_flash.cpp:109`, `:403`; `kburn_k230.h:31` | The range is the tool's check. The ROM's own limit is UNVERIFIED |
| Load size | Pages of 1000 bytes on bulk OUT; the last page is short | `burner_brom.cpp:119-156` | Page size source-verified. The ROM's memory limit is UNVERIFIED |
| Address split | wValue is the high 16 bits, wIndex the low 16 | `kburn_k230.h:16-17`, `burner_brom.cpp:77-82` | Source-verified |
| Execution handoff | The image is written to the load address, then started with PROG_START at the same address | `k230_flash.cpp:599-606`, `burner_brom.cpp:16-37` | The tool implies it. The ROM's jump convention is UNVERIFIED on hardware |
| Start outcome | Settled by observation (see "The start"); no source decides it | this file | Settled on the bench by step 2 |
| Re-enumerates | As the burner, at the same port | `k230_flash.cpp:615-630` (`poll_and_open_device`) | The tool implies it; UNVERIFIED on hardware |
| Packet layout | 60 bytes: `cmd`, `result`, `data_size` (u16 each, little-endian), then 54 bytes of data; a reply's `cmd` has bit 15 set | `burner_uboot.cpp:46-59`, `:13` | Source-verified |
| Commands | NOP 0, REBOOT 1, PROBE `0x10`, GET_INFO `0x11`, ERASE `0x20`, WRITE `0x21`, READ `0x23`, READ_CHUNK `0x24`, VERIFY `0x25` | `burner_uboot.cpp:16-33` | Source-verified |
| Results | OK 1, ERROR 2, MESSAGE `0xff` | `burner_uboot.cpp:35-41` | Source-verified |
| VERIFY reply | 48 bytes: SHA-256, bytes verified (u64), milliseconds (u64) | `burner_uboot.cpp:369-410` | Source-verified |
| Image header | Magic `0x27cb8f93`, CRC-32 over the 512-byte header with its CRC zeroed, 4 KiB part alignment | `kdimage.h:13-35`, `kdimage.cpp:168-180` | The host-side image format. Whether the ROM checks it is UNVERIFIED |
| WDT bases | `wdt0` `0x91106000`, `wdt1` `0x91106800` | `k230.dtsi:513-518`; `board.h:171-175` | Source-verified |
| WDT registers | CR `0x00`, TORR `0x04`, CCVR `0x08`, CRR `0x0c` (kick `0x76`), COMP_PARAM_1 `0xf4` | `drv_wdt.h:37-72`; `drv_wdt.c:86-135`, `:179-199`; `dw_wdt.c:32-64` | Source-verified |
| WDT clock | Not given. The RT-Thread driver reads `SYSCTL_CLK_WDT_1_CLK` (`drv_wdt.c:62`) | `drv_wdt.c:62`; `k230.dtsi:513-518` | UNVERIFIED. The range is an assumption; the bench measures it |
| WDT reset scope | Whether a WDT reset reaches the whole SoC or one core | not in the sources | UNVERIFIED. Both WDTs are armed |
| WDT disable | `sysctl_reset(SYSCTL_RESET_WDT1_APB)` (the block's own reset) | `drv_wdt.c:103-109` | Source-verified. The stage never calls it |
| SoC software reset | `soc_glb_rst` at `0x91102060`, value `0x10001` | `sysctl_boot.c:43-49`; `sysctl_boot.h:34,47`; `cpu.c` (`do_reset`) | Source-verified. Whether it returns the ROM to download mode with the header invalid is UNVERIFIED |
| SD controller | `sdhci0` at `0x91580000`; `sdhci1` at `0x91581000` (`kendryte,k230-dw-mshc`) | `k230.dtsi:154-176`; `board.h:243` | Source-verified for the addresses. The boot controller and the clock are UNVERIFIED |
| SDHCI registers | ARGUMENT `0x08`, COMMAND `0x0e`, RESPONSE `0x10`, BUFFER `0x20`, PRESENT_STATE `0x24`, POWER `0x29`, CLOCK `0x2c`, SOFTWARE_RESET `0x2f`, INT_STATUS `0x30` | `drv_sdhci.h` (the SDK's `interdrv/sdio`) | Source-verified names; the R2 layout is UNVERIFIED |
| USB2 OTG (device) | `0x91500000`, window `0x80000` | `board.h:240-241`; dwc2 register offsets `hw.h` (cited in `drivers/k230-usbboot-sys/src/dwc2.rs`) | Source-verified addresses and offsets. The device-mode sequences are UNVERIFIED on hardware |

### Disagreements with the earlier crate

The earlier K230 flash crate (commit `4304d728` of the other repository, retired by this work) was
written against the same vendor tool. Where the sources disagree with it:

1. **A disconnect at the start request.** Settled by the observation rule above, not by either
   reading. The vendor tool treats any failure at PROG_START as fatal (`burner_brom.cpp:30-35`); the
   earlier crate treated a disconnect as success. Both are replaced.
2. **The CPU-info text.** The vendor tool compares the 32-byte reply with `memcmp(info, "...", size)`
   (`probe_device.cpp:49`, `:55`), where `size` is the reply's length (32). The literal is shorter
   than 32 bytes, so the compare reads past it. The earlier crate trims the NUL padding and compares
   the text exactly. This port does the same.
3. **The WDT base.** The brief and `socs/k230.toml` gave `0x91106000` (`wdt0`). The SDK's RT-Thread
   driver uses `WDT1` at `0x91106800` for the big core. This port arms both.
4. **The stage's identity.** The stage answers CPU info with `Lemnos stage for K230`, which the
   earlier crate's `Stage` does not know. The host reads it as `Stage::Lemnos`.
5. **Sessions.** The earlier crate writes a whole image in one WRITE session. The flasher writes
   each piece in its own session, with an erase before it, so each piece is checked alone. This is
   the flasher's design, not a source fact.
6. **The SD controller.** The brief named `0x91581000` and `snps,dwcmshc-sdhci`. The device tree
   gives `sdhci0` at `0x91580000` and `sdhci1` at `0x91581000` as `kendryte,k230-dw-mshc`.

The stale-reply rule, the probe's chunk checks, the load-address range and the packet layout agree
with the sources.

## The API the earlier crate's driver calls

The earlier driver (in the other repository) calls three functions and one type. They exist here
with the same names and meaning:

- `bootrom::stage(io) -> Result<Stage, UsbError>`, with `Stage::{BootRom, Burner, Lemnos, Unknown}`.
- `bootrom::check_address(address) -> Result<(), UsbError>`.
- `bootrom::load_and_start(io, image, address) -> Result<(), UsbError>`. **It blocks** for the start's
  bounded wait (up to `START_WAIT_MS`, 10 s, plus the re-enumeration). It returns `Ok` only when the
  device came back as the stage. A driver calls it from a blocking context, or runs it on a thread it
  owns; it must not hold a lock the device's other calls need while it waits.

The API is synchronous, where the earlier crate was `async`: the calls take `&mut dyn UsbIo` (see
`transport.rs`). The earlier crate's burner and write paths (`Burner::probe`, `info`, `read`,
`erase`, `write`, `sha256`, `reboot`) are here too, with the same behaviour, and so is the flasher
(`flash::flash`).

## Tests

Run with `cargo test -p lemnos-k230-usbboot -p lemnos-k230-usbboot-stage -p lemnos-driver-k230-usbboot-sys`.
The `-sys` crate builds for `riscv64imac-unknown-none-elf`, which CI builds too.

- Host (`lemnos-k230-usbboot`, 67 tests): the boot ROM's requests and the start's observation (14),
  the burner (8), the write path (9), the flasher (11, including the cut at every operation and byte
  offsets, the return path at each clock, the feed rule, and the medium that drops writes), the
  header write cut at every operation and every byte (2), the reset paths (9: both watchdogs, the
  readback, the deadline, the stalled VERIFY, and the healthy stage), the vectors (1), and the libusb
  transport's pure parts (4), and the packet and sim unit tests (2).
- Stage (`lemnos-k230-usbboot-stage`, 46 tests): the wire format, the feed rule and each refusal of
  the stage (13 in `tests/stage.rs`), the watchdogs' model and readback (11), and the SD protocol (8),
  with a real-failing case for each. The SD protocol runs against a fake card that also fails each way
  a card can.
- Stage device path (`tests/usb_dev.rs`, 10 tests; in the 46): the enumeration, the identity, the failures listed above.
- Probe classifier (`probe::tests`, 6 tests): the windows, the attribution among armed paths, the ambiguity, and the no-return cost.
- `-sys`: the entries' sequences are checked against the stage's constants by a test that reads their
  source. They build for the riscv target.
