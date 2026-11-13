# Identity facts

A board names itself to the stack with a set of facts. Each one is published as a topic
`hello.<fact>` by the Orion bridge: UTF-8, no trailing newline, at most 64 facts of at most 256
bytes each. The daemon serves them through `system.identity/1` and the bridge publishes them. The
library `engine/identity` (crate `lemnos-identity`) holds the serial sources and the fact formats.

## The facts

| Fact | Format | Source |
|---|---|---|
| `family` | the board's name: lowercase `[a-z0-9-]`, at most 64 bytes | `[board] id` in the board file |
| `serial` | lowercase hex, no `0x`, no separators, leading zeros kept, 8 to 32 digits | the `system-identity` device's serial source (see [settings](#serial-source-settings)) |
| `model` | display name with the revision, free text, at most 64 bytes, never parsed | `[board] name`, then ` ` and `[board] revision` when given |
| `version.slot` | the running slot's id, as `system.slots/1 list` names it | `board/slots/booted` on the data partition, read again on each `facts` call |
| `version.lemnos` | lemnosd's package version | the daemon: `Config::host_version()`, given to each driver as it binds |
| `version.product` | product name: lowercase `[a-z0-9-]` | `product` in the running slot's `slots/<id>.json` |
| `version.os` | the running slot's version | `version` in the running slot's `slots/<id>.json` |
| `offers` | space-separated contract ids the node serves, as Lemnos names them, e.g. `system.slots/1 system.reboot/1` | the devices lemnosd lists that are running now, computed by the bridge |

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
every slot. A recovery or flasher board must use the same source settings as the normal one.

## Serial source settings

The driver `system-identity` (served as `system.identity/1`) reads the serial from its device's
`config` in the board file. The `family` fact is the board's `[board] id`, which the host hands the
driver as its board id, so nothing else is needed for it.

```toml
[[devices]]
id = "identity"
driver = "system-identity"
[devices.config]
serial = "device-tree"   # or "cpuinfo", "nvmem", "cmdline"
name = "Alpha"           # the model fact, with the revision after it
revision = "r2"
```

| Key | Meaning |
|---|---|
| `serial` | The source kind. Required for a serial |
| `serial_path` | The file. Defaults: `/proc/device-tree/serial-number`, `/proc/cpuinfo`, `/proc/cmdline`. Required for `nvmem` |
| `serial_offset`, `serial_length` | For `nvmem`: byte offset (default 0) and length, 4 to 16 bytes |
| `serial_byte_order` | For `nvmem`: `"big"` (default) or `"little"` |
| `serial_key` | For `cmdline`: the command-line key |
| `name`, `revision` | The `model` fact |
| `data` | The data partition for the slot facts. Default `/data` |

A USB gadget takes the board's serial from this device: its `identity` setting names the device
(see [the gadget section of `boards.md`](boards.md#identity-derived-addresses-and-the-usb-serial)).
The daemon reads the serial once, when it starts, through the same settings as this driver, and
gives the result to the gadget's setup and driver as `board_serial`. The serial is read by this
device's source only. A gadget does not carry a second copy of these settings.

One source per board file: the device names exactly one `serial` kind. A source that cannot be read
fails the bind and is retried. A value that does not fit the format leaves `serial` absent and the
device `Degraded`.

## Serial sources by SoC family

Each board names one source. A fact that cannot be read is absent. There is no chain of fallbacks, and no MAC address is ever a serial.

The table names the generic source kind each SoC family uses. Rows are by family and source
kind, never by product. "Verified" means read on a board. Everything else is untested on board.

| SoC family | Source kind | Path, offset, length | Provenance | Status |
|---|---|---|---|---|
| Reference board (Raspberry Pi firmware serial) | `device-tree` | `/proc/device-tree/serial-number`; the kernel reads it into `Serial` (`init_machine_late`, `of_property_read_string(root, "serial-number", ...)`, `arch/arm/kernel/setup.c`, raspberrypi/linux `rpi-6.6.y`) | torvalds/linux and raspberrypi/linux `arch/arm/kernel/setup.c` | documented source is `/proc/device-tree/serial-number` with NULs stripped and lowercased. Verified read-only by the requester: it equals cpuinfo's `Serial`, both 16 lowercase hex digits. Re-check in `testing/board-runs/P1.md` |
| Rockchip (vendor kernel) | `device-tree` | candidate: the device tree's `serial-number`, which the vendor kernel fills from the OTP CPU id. The OTP cell itself, if a source names it, would be `nvmem` instead: candidate offset 0x07, length 16 from memory, not read | torvalds/linux `arch/arm64/boot/dts/rockchip/rk3588-base.dtsi` (`otp: efuse@fecc0000`, only the `cpu-code` cell read, not a serial); the `Serial` line copies `serial-number` per `arch/arm/kernel/setup.c` (read in this session) | untested on board. Read on a real board before the source is fixed |
| Rockchip RK3566, mainline-based kernel (Linux 7.3.3) | `device-tree` | `/proc/device-tree/serial-number`, read as is: 16 lowercase hex digits, already in the normalised form. `/proc/cpuinfo` has no `Serial` line on this kernel, so `cpuinfo` is not a source here | Read on a board on 2026-11-13 by another team (read-only). Not checked whether the boot loader fills this property from the OTP CPU id or from elsewhere; the vendor kernel may differ | verified on board (read-only check by another team). Re-check in `testing/board-runs/P1c.md` |
| Allwinner SID | `nvmem` | candidate: the chip ID at offset 0x200, length 16 bytes, in the SID block `efuse@3006000` (H6) | the offset is `value_offset = 0x200` in `drivers/nvmem/sunxi_sid.c` (`sun50i_h6_cfg`), read in this session (torvalds/linux master). The 16-byte length is from memory and is not in the sources read. The H6 device tree (`arch/arm64/boot/dts/allwinner/sun50i-h6.dtsi`) names only `ths_calibration` (0x14, 8) and `cpu_speed_grade` (0x1c, 4) | untested on board. The length is unconfirmed |
| K230 | `nvmem` | candidate: an OTP node in the K230 device tree, reached through the nvmem binding, cell not known. Old software tried cpuinfo `Serial`, then `serial-number`, then a MAC address; the source is one named kind per board file and no fallback is kept | the K230 Linux SDK's `arch/riscv/boot/dts/canaan/k230.dtsi`, not yet read: the URL tried on `kendryte/k230_linux_sdk` returned 404. The K230 driver API reference says the OTP is on nvmem and holds security data (https://developer.canaan-creative.com/k230/en/v1.7/01_software/board/osdrv/K230_Little_Core_Linux_Driver_API_Reference.html) | untested on board |
| Small NAND SoC boards | `device-tree` | candidate: the device tree's `serial-number`. Old software tried cpuinfo `Serial` (skipping all-zero), then `serial-number`, then the first MAC address. No board is recorded as having taken which branch; the MAC step is not kept | the old code's order, as the requester described it. No source read in this session | untested on board. Nothing depends on this row |

When a row is added, its source file and the offset and length must come from a source read,
with its URL and commit, not from memory.

## How the bridge publishes them

`lemnos-orion` reads the identity device's `facts` (method 4) from lemnosd, and publishes one
topic per fact, `hello.<fact>`, on its Orion session. Orion's hello document carries the newest
value of each. The bridge is the one path: there is no mode without lemnosd, and with lemnosd
down nothing is published.

- **Values.** UTF-8 text, no trailing newline, one text field named `value`.
- **Limits.** At most 64 facts, each value at most 256 bytes, no control character, no empty
  value. A fact that does not fit is left out and the log says why. The rule is enforced in the
  bridge (`service/orion/src/hello.rs`), whatever the driver sent.
- **Absent.** A fact the device does not give is not published at all. When the device cannot be
  read (it is off, or its call fails), none of its facts is published; `offers` still is.
- **When each is republished.** `offers` when a device starts or stops running, since only running
  devices offer their contracts. The identity device's facts are read every 2 seconds, and a
  changed value is published as it changes. The running slot is read again on every `facts`
  call, so `version.slot`, `version.product` and `version.os` follow the boot marker without a
  rebind.
- **A new fact** (one that was absent and now is, or one that goes away) changes the set of
  topics. Orion has no way to take one topic back, so the bridge publishes its session again:
  every topic is said again, and the facts are then as they are.
- **Reconnect.** After Orion goes away and comes back, every fact's value is said again.

`version.slot` after a confirm: the confirm sets the default slot, not the running one. The
running slot moves only when the boot marker does, at boot, before lemnosd binds the device. So
the bridge's periodic read is there for a marker that moves while lemnosd runs, not for a confirm.

## Who checks what

The serial reaches the stack only through the daemon: lemnosd is the one process that serves
`system.identity/1`, and the bridge publishes what it serves. The USB gadget's derived addresses
come from the same serial, read by the daemon in its start (`resolve` in `service/lemnosd`). Recovery and flasher images run the
daemon too, so they report the same serial as a normal boot.
