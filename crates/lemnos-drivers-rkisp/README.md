# lemnos-drivers-rkisp

The Rockchip ISP21 (RK3566, RK3568), ISP30 (RK3588, RK3588S) and ISP32 (RV1106, RV1103, and the
ISP32-lite of RK3562) image signal processors from Rust: which ISP a SoC has, the ISP's parameter
buffer trait, the ISP32 variants and their video node names, and, with the `device` feature, the
vendor `rkisp` driver's nodes on `lemnos-v4l2`. The uAPI layouts are re-exported as `uapi` from the
`-sys` crate `lemnos-rkisp-sys`. The crate forbids unsafe code.

Status: **written, untested on board.** None of these ISPs has run on its chip yet. The device
layer (`device`) is untested on board, and so are the ISP32 layouts and the fixed-point scales the
headers do not state (named as constants in the camera stack's builders, to be confirmed on a
device).

## Features

- `std` (default): devicetree detection (`Isp::detect`). Without it the crate is `no_std` +
  `alloc`, and the uAPI is `no_std` too.
- `device` (Linux only, implies `std`): the vendor driver's ISP, parameter, statistics and main
  path nodes, through `lemnos-v4l2`. Untested on board.

## Modules

- `uapi` (= `lemnos_rkisp_sys`): the parameter and statistics buffers' layouts for ISP21
  (`isp21`), ISP30 (`isp3x`) and ISP32 (`isp32`), with the module bits and the compile-time size and
  offset checks. See the `-sys` crate's README.
- `Isp`: the generation a SoC has: `from_soc`, `from_compatibles`, `detect`.
- `ParamBuffer`: a parameter buffer the device takes as it is. Public and open: the camera
  stack's builders implement it for their own types.
- `isp32`: `Variant` (ISP32 or ISP32-lite) and `nodes` (the video node names), with
  `Variant::output_nodes`.
- `device` (feature `device`): `IspDevice`, which links the ISP's graph, sets the formats, and
  streams the parameter, statistics and output queues. Untested on board.

## What moved, and what stayed (with the camera stack)

From the camera stack's `crates/rkisp` (commit d17de2ed) this crate and `lemnos-rkisp-sys` take the
chip's kernel interface; the camera stack keeps the tuning and statistics math.

| Moved | Where it is now | Stays in the camera stack's `rkisp` crate |
|---|---|---|
| uAPI layouts, the four headers, the bindgen script | `lemnos-rkisp-sys` (headers in its `headers/`, script `scripts/rkisp-bindgen.sh`) | `params.rs`, `stats.rs` (builder and statistics math, `Generation`) |
| `device.rs` | `lemnos-drivers-rkisp` (`device`, on `lemnos-v4l2`) | `isp32/params.rs`, `isp32/stats.rs` |
| `isp.rs`: `Isp` (`from_soc`, `from_compatibles`, `detect`), `ParamBuffer` | `lemnos-drivers-rkisp` | `isp.rs`: `AnyParams`, `Isp::params()`, `Isp::statistics()`, and the `ParamBuffer` impls for `Params`, `isp32::Params`, `AnyParams` |
| `isp32/mod.rs`: `Variant`, `nodes`, `Variant::output_nodes` | `lemnos-drivers-rkisp` | `isp32/mod.rs`: the `pub mod params` / `pub mod stats` lines and their re-exports |
| the layout asserts and the SoC test | `lemnos-rkisp-sys`, `lemnos-drivers-rkisp` | the builder and statistics tests |

The camera stack's params and stats import the uAPI through `lemnos_drivers_rkisp::uapi` and keep
their `no_std` + `alloc` build: nothing they use needs std.

## Renames

The camera stack's `rkisp` crate is `lemnos-drivers-rkisp` here. It switches by renaming its
dependency (`rkisp = { package = "lemnos-drivers-rkisp", ... }`) or by changing its paths. Items
in the moved code:

- `device.rs`: the camera stack's kernel dependency is `lemnos-v4l2`. The items it uses (`FourCc`,
  `media`, `subdev`, `v4l2`, `Error`) have the same names under `lemnos_v4l2`;
  `DeviceError::Kernel` now wraps `lemnos_v4l2::Error`.
- No public type, env var or capability string was product-named, so none renames.

## Licences

The uAPI headers are `(GPL-2.0+ WITH Linux-syscall-note) OR MIT` (see `lemnos-rkisp-sys`). The
rest of the crate is `MIT OR Apache-2.0`, the workspace licence.

## Tests

`cargo test -p lemnos-drivers-rkisp --all-features` runs the SoC-to-ISP table and the ISP32 node
test. The layouts are checked at compile time in `lemnos-rkisp-sys`.
