# lemnos-layout-check

Compares each vendor-header driver's layout assertions (`drivers/*/src/layout.rs`, `src/uapi.rs`,
`src/modules.rs`) with the layout its vendor header gives. It replaces the C programs that were
`drivers/*/layout-check/check.c` with their run scripts. The C is gone; the facts they recorded are
here as data (`src/specs.rs`), and the Rust assertions remain the gate's check.

## Use

- `cargo run -p lemnos-layout-check`: no header given, so nothing is compared. It prints one line per
  variant with the header it needs and where it comes from. The gate runs this.
- `lemnos-layout-check <driver>[/<variant>] -I <dir> [-I <dir>] [-D <macro>] [--target <triple>] --header-version <v>`:
  runs bindgen (0.73.2, with clang) on a wrapper that includes the header, and compares each recorded
  number with bindgen's reading. A header is compared only when `<v>` is the version the record was taken
  from. Otherwise the line says `not compared: host header is version <v>, the record is from <r>`,
  which is neither a pass nor a fail. An exit status of 1 is a difference; 0 is a pass or not compared.
- `--evidence` reports every value with the label `EVIDENCE ONLY, not a check` and exits 0. It is for
  looking at a header, not for a verdict.

A fact that the header does not have (a member the header lacks) is reported as `NOT IN THIS HEADER`,
with clang's error, and counts against the verdict.

## Records and their versions

| Variant | Record taken from | Header it needs | Run here |
|---|---|---|---|
| `isp600` | unknown (not pinned in the repo) | `isp600_reg.h`, `isp600_reg_cfg.h`, allwinner-bsp at the pinned rev | not compared |
| `isp600-sys` | unknown | `linux/sunxi_camera_v2.h`, `isp600_reg_cfg.h` | not compared |
| `rkaiisp` | rockchip-linux/kernel develop-6.1 | `rk-aiisp-config.h`, `rk-isp2-config.h` | not compared |
| `rknpu-sys` | rockchip-linux/kernel commit 77168c8d5ab82399f65a80e9f807b50ba37cf483 | `rknpu_ioctl.h` (and drm/drm.h) | not compared |
| `vip9000` | unknown | `vip_drv_os.h`, `vip_drv_share.h` (vip2 dir) | not compared |
| `rkisp-sys/v510` | rockchip-linux/kernel develop-5.10 | `linux/rk-isp2-config.h`, `rk-isp21`, `rk-isp3`, `rk-isp32` (camera-module include cut) | not compared |
| `rkisp-sys/v61` | rockchip-linux/kernel develop-6.1 | `linux/rk-isp2-config.h`, `rk-isp33`, `rk-isp35`, `rk-isp39` | not compared |
| `pisp/fe` | rpi-7.2.y | `pisp_fe_config.h`, `pisp_statistics.h` (rp1_cfe dir) | not compared |
| `pisp/be` | rpi-7.2.y | `pisp_be_config.h`, `pisp_common.h` | not compared |

"Not compared" means no header of the record's version was given on this PC. The host's kernel headers
are 7.2.9 and 7.3.4 (Nobara), which are not rpi-7.2.y. The record's revision is stated by the caller for
pisp it is the reference tree named in `docs/kernel-config.md` (rpi-7.2.y); most other records name a
branch or nothing, so no header can be matched to them and they are not compared.

## pisp/be evidence (not a check)

`lemnos-layout-check pisp/be -I /usr/src/kernels/7.3.4-201.nobara.fc44.x86_64/include/uapi/linux/media/raspberrypi --evidence`
on this PC gives 97 of 98 facts agreeing with the record, and one `NOT IN THIS HEADER`:
`offset_of!(BeConfig, pad1)`. The host `pisp_be_config.h` (identical in 7.2.9 and 7.3.4) has no direct
member `pad1`. It has `lsc_extra`, `cac_extra`, `downscale_extra`, `resample_extra`, `crop`,
`hog_format` and the dirty flags over the bytes 6392 to 6476 that the record calls `pad1` (84 bytes).
The total size agrees (6476). The record was taken from rpi-7.2.y, which this PC does not have, so
this is evidence about the version difference, not a verdict on the record.
