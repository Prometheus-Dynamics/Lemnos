# lemnos-soc

`no_std` chip descriptors, as data. For each SoC in `CHIPS` (K230D, RK3588S, RK3576, RK3566,
RK3562, RV1106): an `Entry` per IO block and capability, marked `Used`, `Unused` or
`Impossible`, with the software path it would replace and a note on what decides the status.
`docs/io-blocks.md` has the same tables in prose.

- `Chip::find`, `Chip::count` (controllers of a kind, from the base addresses).
- `rockchip_pwm`, `rockchip_timer`, `clint`: the register layouts of the blocks a user-space path
  could reach, as typed constants, taken from the mainline drivers.

Every row is from the public device trees and drivers named in the chip's `sources`. Nothing
here has run on a board, and the TRM rows that the mainline description does not cover are
marked "not checked here". No allocation, no dependencies, no unsafe code.
