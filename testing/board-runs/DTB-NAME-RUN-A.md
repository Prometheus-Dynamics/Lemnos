# DTB-NAME-RUN-A: which device-tree file name the firmware loaded on Raze #2 (read-only, 2026-11-21)

Board: Raze #2 (CM5 Rev 1.0), serial a317bcbee5226d57, boot b34fee0e-2dc9-4da1-a77f-791d01632baf. Rev: origin/next
0d37cc55. Both board locks held by flock over ssh for each read. Read-only; nothing changed.

## Hypothesis (written before the read)

The firmware picks the device-tree file by board revision. The name it loaded is one of the slot's
`device_tree_names`, and the firmware log names it. Official docs say only that selection is by board revision; no
official text names the CM5 file, so the read is the test.

## Reads and results

| Read | Result |
|---|---|
| Firmware log: `vclog -m` | **Not available.** `vclog` is not on the image (`sh: vclog: not found`). |
| Firmware tool: `vcgencmd` | Present. `vcgencmd commands` lists `get_config`, `bootloader_config`, `bootloader_version`, `get_rsts`, and no log command. No subcommand of it prints the loaded DTB name. |
| Kernel log | `Machine model: Raspberry Pi Compute Module 5 Rev 1.0`. No line names the DTB file. |
| Board revision, from the device tree and cpuinfo | Model `Raspberry Pi Compute Module 5 Rev 1.0`; compatible `raspberrypi,5-compute-modulebrcm,bcm2712`; `Revision : c04180`. |
| Boot partition at `/boot` | Not mounted at `/boot`; no file listed. |

## Result

**Partly run. The file name the firmware loaded was NOT read.** The image has no firmware-log reader, and nothing
on it prints the DTB name, so the hypothesis (picked by revision, name in `device_tree_names`) is neither confirmed
nor refuted. The revision the firmware reports is `c04180` (CM5 Rev 1.0).

What would settle it: a firmware log reader on the image (`vclog`), or the boot partition read (`config.txt`
and the loaded `.dtb` name) with the boot partition read-only. Neither was done here.

Board state: unchanged.
