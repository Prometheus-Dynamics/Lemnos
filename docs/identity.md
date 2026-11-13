# Identity facts

A board names itself to the stack with a set of facts. Each one is published as a topic
`hello.<fact>` by the Orion bridge: UTF-8, no trailing newline, at most 64 facts of at most 256
bytes each. `lemnos-identity` prints the same facts as `name=value` lines. Both come from the
library `engine/identity` (crate `lemnos-identity`).

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
guessed, and `lemnos-identity` says why on standard error.

The library gathers `family`, `serial`, `model` and the `version.*` slot facts. `version.lemnos`
and `offers` are set by the caller, from the daemon.

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

The table names the generic source kind each SoC family uses. Rows are by family and source
kind, never by product. "Verified" means read on a board. Everything else is untested on board.

| SoC family | Source kind | Path, offset, length | Provenance | Status |
|---|---|---|---|---|
| Raspberry Pi firmware serial | `device-tree` | `/proc/device-tree/serial-number`; the kernel reads it into `Serial` (`init_machine_late`, `of_property_read_string(root, "serial-number", ...)`, `arch/arm/kernel/setup.c`, raspberrypi/linux `rpi-6.6.y`) | torvalds/linux and raspberrypi/linux `arch/arm/kernel/setup.c` | verified on the reference board, read-only, by the requester: `Serial` and `serial-number` are the same 16 lowercase hex digits. Re-check in `testing/board-runs/P1.md` |
| Rockchip OTP / eFuse CPU id | `nvmem` | OTP node `otp: efuse@fecc0000` (RK3588, `rk3588-base.dtsi`); the CPU-id cell is not named in this source | torvalds/linux `arch/arm64/boot/dts/rockchip/rk3588-base.dtsi` | untested on board. The source read names only `cpu-code` (`reg = <0x02 0x2>`), which is a product code, not a unique serial, so no offset or length is given here |
| Allwinner SID | `nvmem` | SID register block at `efuse@3006000` (H6); the chip-ID offset is not in the sources read | torvalds/linux `arch/arm64/boot/dts/allwinner/sun50i-h6.dtsi`, `drivers/nvmem/sunxi_sid.c` (`sun50i_h6_cfg`: `value_offset = 0x200`, `size = 0x200`) | untested on board. The SID block starts at 0x200 in the driver, but no chip-ID cell length is named in the sources read |
| K230 | none found | not read | not read | no source has been read for this family; left out |

When a row is added, its source file and the offset and length must come from a source read,
with its URL and commit, not from memory.

## Who checks what

`lemnos-identity` reads the serial through the same library the bridge uses.
A recovery or flasher image runs `lemnos-identity` with its own board file, and it prints the same
`serial=` as the running system. That is the check the board run in `testing/board-runs/P1.md`
makes on the reference board.
