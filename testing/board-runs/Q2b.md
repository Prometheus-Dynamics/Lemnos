# Q2b: device-tree maps for the RK3566, RK3588, K230 and A733 families

Status: untested on board. Nothing was compiled against a real base tree, and nothing was loaded.

## What was checked, against what

- Each map cites its source in its header (`socs/rk3566.toml`, `socs/rk3588.toml`, `socs/k230.toml`,
  `socs/sun60i-a733.toml`). The RK3566 and RK3588 maps are the mainline v6.12 device trees; the RK3566
  map also notes where the vendor kernel names two blocks differently. The K230 map is the Kendryte
  SDK's SoC device tree at a pinned commit. The A733 map is a vendor BSP's kernel patch series.
- The generated overlays (RK3566, RK3588) compile with `dtc -@ -I dts -O dtb` (DTC 1.8.1).
- The generated fragments (K230, A733) reference nodes by path (`&{/soc/...}`). Against a stand-in base
  that has the same nodes and labels, they compile with `dtc -@`. The stand-in is generated from the
  paths in the fragment, not read from a real base tree, so this checks syntax only.
- The paths and pin numbers in each map were read from the source files above and checked by hand
  against the groups that name them. Pin numbers are bank*32 plus the pin index.

## The checks to run on the board

1. RK3566 (first board): `lemnos-ctl tree testing/boards/rk3566.toml --socs socs -o board.dts --boot boot.txt`,
   then `dtc -@ -I dts -O dtb -o board.dtbo board.dts` and apply it with the vendor U-Boot `fdtoverlays`.
   Read back the status of `/i2c@fe5a0000`, `/pwm@fdd70010` and `/spi@fe610000`.
2. RK3588: the same, with `testing/boards/rk3588.toml`.
3. K230: include the fragment in a board `.dts` that includes the SDK's `k230.dtsi`, build the dtb,
   and read `/soc/i2c@91405000` and `/soc/spi@91584000` back.
4. A733: the same, against the BSP's base tree.
5. Every family: `lemnosd --check` on the family's board file, then the start check against the live tree.

Until these run, every map and every generated output is untested on board.

## Board run, 2026-11-21: read-only on the ZERO 3E (RK3566)

Status: untested on board. Part read-only, part not run. This run read the live device tree of the ZERO 3E
(board serial `b3d1ffecafae5cec`, Radxa ZERO 3E, kernel `7.3.3`, a gadget device tree
`rk3566-radxa-zero-3e-gadget.dtb`). Nothing was applied, compiled against the board, or written.

Hypothesis (written before the run): every path in `socs/rk3566.toml` is present in the live tree with the
compatible its reason names; the nodes the boards' I2C, PWM and SPI use may be `disabled` on this image.

| Map path | Live status | Live compatible | Result |
|---|---|---|---|
| `/i2c@fe5a0000` (i2c1) | disabled | `rockchip,rk3568-i2c`, `rockchip,rk3399-i2c` | present; the overlay would enable it |
| `/i2c@fdd40000` (i2c0) | okay | same | present, in use by the PMIC |
| `/i2c@fe5b0000`, `fe5c0000`, `fe5d0000`, `fe5e0000` | disabled | same | present |
| `/pwm@fdd70010` (pwm1) | disabled | `rockchip,rk3568-pwm` | present |
| `/pwm@fdd70030` (pwm3) | disabled | same | present |
| `/spi@fe610000` (spi0), `fe620000`, `fe630000`, `fe640000` | disabled | `rockchip,rk3568-spi` | present |
| `/serial@fe660000` (uart2) | okay | `rockchip,rk3568-uart` | present |
| `/usb@fcc00000` (usb-otg0) | okay | `rockchip,rk3568-dwc3`, `snps,dwc3` | present (see Q2e) |
| `/usb2phy@fe8a0000` and `/otg-port` | okay | `rockchip,rk3568-usb2phy` | present (see Q2e) |
| `/watchdog@fe600000` | no status (none set) | `rockchip,rk3568-wdt`, `snps,dw-wdt` | present |
| `/video-codec@fdea0400` (vpu) | no status | `rockchip,rk3568-vpu` | present |
| `/video-codec@fdee0000` (vepu) | no status | `rockchip,rk3568-vepu` | present |
| `/rga@fdeb0000` (rga) | no status | `rockchip,rk3568-rga`, `rockchip,rk3288-rga` | present |
| `/pinctrl/gpio@fdd60000`, `fe740000`, `fe750000`, `fe760000`, `fe770000` | no status | `rockchip,gpio-bank` | present (the five GPIO banks) |

The compatibles match the names the map's reasons give. The map's header says the vendor kernel names the RGA
and the VEPU differently; this image uses the mainline names, which the map uses.

Result: the map's paths and compatibles agree with the live tree on this board for the RK3566. The three nodes
the check names (`/i2c@fe5a0000`, `/pwm@fdd70010`, `/spi@fe610000`) are `disabled` here, as the base tree has
them: they are the nodes an overlay enables, and no overlay was applied, so the enable step is not shown.

Not run: `lemnos-ctl tree`, `dtc -@`, `fdtoverlays` (the vendor loader), `lemnosd --check` and the start check.
None is on the board, and applying an overlay is a boot-time change. The row stays **untested on board**.

Evidence: `/run/user/1000/zero3e-sweep/q2b.out` on the PC (the output above, in full).
