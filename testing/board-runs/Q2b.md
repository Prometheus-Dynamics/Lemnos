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
