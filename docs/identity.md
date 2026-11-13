# Identity facts

A board names itself to the stack with a set of facts. Each one is published as a topic
`hello.<fact>` by the Orion bridge: UTF-8, no trailing newline, at most 64 facts of at most 256
bytes each. The daemon serves them through `system.identity/1` and the bridge publishes them. The
library `engine/identity` (crate `lemnos-identity`) reads them, and nothing else uses it as a program.

## The facts

| Fact | Format | Source |
|---|---|---|
| `family` | the board's name: lowercase `[a-z0-9-]`, at most 64 bytes | `[board] id` in the board file |
| `serial` | lowercase hex, no `0x`, no separators, leading zeros kept, 8 to 32 digits | the board file's `[board.serial]` source |
| `model` | display name with the revision, free text, at most 64 bytes, never parsed | `[board] name`, then ` ` and `[board] revision` when given |
| `version.slot` | the running slot's id, as `system.slots/1 list` names it | `board/slots/booted` on the data partition |
| `version.lemnos` | lemnosd's version | the daemon |
| `version.product` | product name: lowercase `[a-z0-9-]` | `product` in the running slot's `slots/<id>.json` |
| `version.os` | the running slot's version | `version` in the running slot's `slots/<id>.json` |
| `offers` | space-separated contract ids the node serves, as Lemnos names them, e.g. `system.slots/1 system.reboot/1` | the daemon's devices |

`state`, `name` and `revision` are not facts. A fact that cannot be known is absent. It is never
guessed, and the library says why in its notes.

The library gathers `family`, `serial`, `model` and the `version.*` slot facts. `version.lemnos`
and `offers` are set by the daemon.

## Serial normalisation

Every source yields the same format. The library:

1. trims spaces, newlines and the NUL a device-tree string ends with;
2. drops a `0x` prefix and the separators `:`, `-`, `_`, `.` and space;
3. lowercases the digits;
4. refuses a value that is not 8 to 32 hex digits, or that is all zeros.

A binary nvmem cell is 4 to 16 bytes, read with `offset` and `length`. `byte_order = "big"` (the
default) reads the first byte as the most significant. `"little"` reverses the bytes first. An
all-zero or all-ones cell is refused: it is unprogrammed or erased.

The serial must be the same in every state (normal running, recovery, a flasher image) and in
every slot. A recovery or flasher board file must name the same source as the normal one.

## Board file syntax

```toml
[board]
id = "alpha-1"        # family
name = "Alpha"        # model, with revision
revision = "r2"

[board.serial]
kind = "device-tree"  # or "cpuinfo", "nvmem", "cmdline"
```

| `kind` | Keys | Reads |
|---|---|---|
| `device-tree` | `path` (default `/proc/device-tree/serial-number`) | the device tree's `serial-number` string |
| `cpuinfo` | `path` (default `/proc/cpuinfo`) | the `Serial` line |
| `nvmem` | `path` (required), `offset` (default 0), `length` (required, 4 to 16 bytes), `byte_order` (`big` or `little`) | bytes of an nvmem or sysfs file: fuses, OTP or a SID block |
| `cmdline` | `key` (required), `path` (default `/proc/cmdline`) | the value of the last `key=` word on the kernel command line |

Unknown keys are refused. The daemon's board parser checks this table too.

## Serial sources by SoC family

Each board file names one source. A fact that cannot be read is absent. There is no chain of fallbacks, and no MAC address is ever a serial.

The table names the generic source kind each SoC family uses. Rows are by family and source
kind, never by product. "Verified" means read on a board. Everything else is untested on board.

| SoC family | Source kind | Path, offset, length | Provenance | Status |
|---|---|---|---|---|
| Reference board (Raspberry Pi firmware serial) | `device-tree` | `/proc/device-tree/serial-number`; the kernel reads it into `Serial` (`init_machine_late`, `of_property_read_string(root, "serial-number", ...)`, `arch/arm/kernel/setup.c`, raspberrypi/linux `rpi-6.6.y`) | torvalds/linux and raspberrypi/linux `arch/arm/kernel/setup.c` | documented source is `/proc/device-tree/serial-number` with NULs stripped and lowercased. Verified read-only by the requester: it equals cpuinfo's `Serial`, both 16 lowercase hex digits. Re-check in `testing/board-runs/P1.md` |
| Rockchip (vendor kernel) | `device-tree` | candidate: the device tree's `serial-number`, which the vendor kernel fills from the OTP CPU id. The OTP cell itself, if a source names it, would be `nvmem` instead: candidate offset 0x07, length 16 from memory, not read | torvalds/linux `arch/arm64/boot/dts/rockchip/rk3588-base.dtsi` (`otp: efuse@fecc0000`, only the `cpu-code` cell read, not a serial); the `Serial` line copies `serial-number` per `arch/arm/kernel/setup.c` (read in this session) | untested on board. Read on a real board before the source is fixed |
| Allwinner SID | `nvmem` | candidate: the chip ID at offset 0x200, length 16 bytes, in the SID block `efuse@3006000` (H6) | the offset is `value_offset = 0x200` in `drivers/nvmem/sunxi_sid.c` (`sun50i_h6_cfg`), read in this session (torvalds/linux master). The 16-byte length is from memory and is not in the sources read. The H6 device tree (`arch/arm64/boot/dts/allwinner/sun50i-h6.dtsi`) names only `ths_calibration` (0x14, 8) and `cpu_speed_grade` (0x1c, 4) | untested on board. The length is unconfirmed |
| K230 | `nvmem` | candidate: an OTP node in the K230 device tree, reached through the nvmem binding, cell not known. Old software tried cpuinfo `Serial`, then `serial-number`, then a MAC address; the source is one named kind per board file and no fallback is kept | the K230 Linux SDK's `arch/riscv/boot/dts/canaan/k230.dtsi`, not yet read: the URL tried on `kendryte/k230_linux_sdk` returned 404. The K230 driver API reference says the OTP is on nvmem and holds security data (https://developer.canaan-creative.com/k230/en/v1.7/01_software/board/osdrv/K230_Little_Core_Linux_Driver_API_Reference.html) | untested on board |
| Small NAND SoC boards | `device-tree` | candidate: the device tree's `serial-number`. Old software tried cpuinfo `Serial` (skipping all-zero), then `serial-number`, then the first MAC address. No board is recorded as having taken which branch; the MAC step is not kept | the old code's order, as the requester described it. No source read in this session | untested on board. Nothing depends on this row |

When a row is added, its source file and the offset and length must come from a source read,
with its URL and commit, not from memory.

## Who checks what

The serial reaches the stack only through the daemon: lemnosd is the one process that serves
`system.identity/1`, and the bridge publishes what it serves. Recovery and flasher images run the
daemon too, so they report the same serial as a normal boot.
