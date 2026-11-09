# TODO

Every planned PR into `next`. `python3 scripts/pr-status.py` shows the live state; a merged PR is one whose `pr/<id>` branch merged. Briefs are in [`docs/prs/`](docs/prs/). PRs don't edit this file.

## A. Foundation

The engine with no hardware. A1 to A6 go one at a time because everyone builds on them. After that the group fans out.

- [x] [A1](docs/prs/A1.md) Clean slate and skeleton (wave 1; needs nothing)
- [x] [A2](docs/prs/A2.md) Boundary checks in the gate (wave 2; needs A1)
- [x] [A3](docs/prs/A3.md) lemnos-abi (wave 2; needs A1)
- [x] [A4](docs/prs/A4.md) lemnos-hal: what every driver is written against (wave 3; needs A3)
- [ ] [A5](docs/prs/A5.md) Driver model (wave 4; needs A4)
- [ ] [A6](docs/prs/A6.md) #[driver] and #[contract] macros (wave 5; needs A5)
- [ ] [A7](docs/prs/A7.md) Simulated backend (wave 4; needs A4)
- [ ] [A8](docs/prs/A8.md) Board file (wave 5; needs A5)
- [ ] [A9](docs/prs/A9.md) Host: bind and dispatch (wave 6; needs A6, A7, A8)
- [ ] [A10](docs/prs/A10.md) Host: reads and scheduling (wave 7; needs A9)
- [ ] [A11](docs/prs/A11.md) Containment and the failure rule (wave 7; needs A9)
- [ ] [A12](docs/prs/A12.md) Wire format and spec (wave 5; needs A5)
- [ ] [A13](docs/prs/A13.md) Client (wave 6; needs A12)
- [ ] [A14](docs/prs/A14.md) C surface (wave 7; needs A13, A3)
- [ ] [A15](docs/prs/A15.md) lemnosd (wave 8; needs A10, A11, A13)
- [ ] [A16](docs/prs/A16.md) lemnos-ctl, generic (wave 9; needs A15)
- [ ] [A17](docs/prs/A17.md) Perf counters and allocation counts (wave 5; needs A5; board run)

## B. Linux and the chips we have

Real drivers on stock Linux. Everything compiles and tests on simulated buses in the cloud; a local session then runs the board check.

- [ ] [B1](docs/prs/B1.md) linux-sys, the general part (wave 2; needs A1)
- [ ] [B2](docs/prs/B2.md) Linux I2C and SPI (wave 4; needs B1, A4)
- [ ] [B3](docs/prs/B3.md) Linux GPIO, PWM, UART (wave 4; needs B1, A4)
- [ ] [B4](docs/prs/B4.md) Hotplug (wave 3; needs B1)
- [ ] [B5](docs/prs/B5.md) RP1 PIO I2C (wave 5; needs B2; board run)
- [ ] [B6](docs/prs/B6.md) Motion contracts (wave 6; needs A6)
- [ ] [B7](docs/prs/B7.md) Power, thermal, fan, light, switch contracts (wave 6; needs A6)
- [ ] [B8](docs/prs/B8.md) bmi088 (wave 7; needs A9, B6; board run)
- [ ] [B9](docs/prs/B9.md) bmm150 (wave 7; needs A9, B6)
- [ ] [B10](docs/prs/B10.md) orientation and calibration (wave 7; needs A9, B6; board run)
- [ ] [B11](docs/prs/B11.md) ina2xx (wave 7; needs A9, B7)
- [ ] [B12](docs/prs/B12.md) hwmon-fan (wave 7; needs A9, B7; board run)
- [ ] [B13](docs/prs/B13.md) thermal-zone (wave 7; needs A9, B7)
- [ ] [B14](docs/prs/B14.md) fan-control (wave 8; needs B12, B13; board run)
- [ ] [B15](docs/prs/B15.md) gpio-power-switch (wave 7; needs A9, B7, B3; board run)
- [ ] [B16](docs/prs/B16.md) ws2812 (wave 7; needs A9, B7)
- [ ] [B17](docs/prs/B17.md) status-light (wave 8; needs B16; board run)
- [ ] [B18](docs/prs/B18.md) raw-bus (wave 7; needs A9, B2, B3)
- [ ] [B19](docs/prs/B19.md) gpu-lease (wave 7; needs A9, B1)
- [ ] [B20](docs/prs/B20.md) lemnos-orion (wave 10; needs A13, A16; board run)
- [ ] [B21](docs/prs/B21.md) vcm (wave 7; needs A9, B7)
- [ ] [B22](docs/prs/B22.md) Bench board file and comparison harness (wave 10; needs A16, B8, B14, B17; board run)

## C. The module becomes the way in

C1 builds a QEMU test harness, so the cloud can load the module and test it for real. Only PIO, power and clocks need a board.

- [ ] [C1](docs/prs/C1.md) Module core and QEMU harness (wave 2; needs A1)
- [ ] [C2](docs/prs/C2.md) Claims (wave 3; needs C1)
- [ ] [C3](docs/prs/C3.md) I2C and SPI transfer lists (wave 4; needs C2; board run)
- [ ] [C4](docs/prs/C4.md) GPIO and PWM (wave 4; needs C2; board run)
- [ ] [C5](docs/prs/C5.md) PIO (wave 4; needs C2; board run)
- [ ] [C6](docs/prs/C6.md) Power, clocks, resets, thermal, hotplug, faults (wave 4; needs C2; board run)
- [ ] [C7](docs/prs/C7.md) Linux backend through the module (wave 5; needs C3, C4, B2, B3)
- [ ] [C8](docs/prs/C8.md) Whole-system A/B (wave 11; needs C5, C6, C7, B22; board run)

## D. Camera

Lemnos gets a camera working on its own, with a proof tool. The highest risk is D7. Styx isn't involved.

- [ ] [D1](docs/prs/D1.md) Frame stream, engine side (wave 7; needs A12, A13)
- [ ] [D2](docs/prs/D2.md) Frame stream, module side (wave 8; needs C2, D1)
- [ ] [D3](docs/prs/D3.md) Camera contracts (wave 6; needs A6)
- [ ] [D4](docs/prs/D4.md) PiSP config library (wave 2; needs A1)
- [ ] [D5](docs/prs/D5.md) rp1-csi2 (wave 9; needs D2, D3, D4; board run)
- [ ] [D6](docs/prs/D6.md) pisp back end (wave 9; needs D2, D3, D4; board run)
- [ ] [D7](docs/prs/D7.md) pisp front end (wave 10; needs D5; board run)
- [ ] [D8](docs/prs/D8.md) ov9782 (wave 7; needs A9, D3; board run)
- [ ] [D9](docs/prs/D9.md) Camera proof tool (wave 10; needs D5, D6, D8; board run)
- [ ] [D10](docs/prs/D10.md) Exposure at frame cadence (wave 11; needs D8, D9; board run)
- [ ] [D11](docs/prs/D11.md) USB backend (wave 4; needs B1, A4)
- [ ] [D12](docs/prs/D12.md) uvc (wave 8; needs D11, D3, D1, B4; board run)
- [ ] [D13](docs/prs/D13.md) Coral firmware load (wave 5; needs D11; board run)
- [ ] [D14](docs/prs/D14.md) V4L2 backend (wave 3; needs B1)

## E. Everything else

Runs alongside the rest as its needs land. Written from sources. Marked untested on board until those boards arrive.

- [ ] [E1](docs/prs/E1.md) Image contracts and job ring (wave 8; needs D1, A6)
- [ ] [E2](docs/prs/E2.md) rga (wave 9; needs E1, D14)
- [ ] [E3](docs/prs/E3.md) vglite (wave 10; needs E1, E2)
- [ ] [E4](docs/prs/E4.md) g2d (wave 9; needs E1)
- [ ] [E5](docs/prs/E5.md) nonai2d, conversion (wave 9; needs E1, D14)
- [ ] [E6](docs/prs/E6.md) nonai2d, OSD and border (wave 10; needs E5)
- [ ] [E7](docs/prs/E7.md) rkvpss (wave 9; needs E1)
- [ ] [E8](docs/prs/E8.md) rkcif (wave 7; needs D3, D14)
- [ ] [E9](docs/prs/E9.md) rkisp (wave 7; needs D3, D14)
- [ ] [E10](docs/prs/E10.md) rkaiisp (wave 8; needs E9)
- [ ] [E11](docs/prs/E11.md) k230isp (wave 7; needs D3)
- [ ] [E30](docs/prs/E30.md) K230 capture, direct (wave 8; needs E11, D1)
- [ ] [E12](docs/prs/E12.md) sunxi-vin (wave 7; needs D3, D14)
- [ ] [E13](docs/prs/E13.md) A733 ISP600 (wave 8; needs E12)
- [ ] [E14](docs/prs/E14.md) Chip tables (wave 2; needs A1)
- [ ] [E15](docs/prs/E15.md) Stateless video decode (wave 4; needs D14; board run)
- [ ] [E16](docs/prs/E16.md) Rockchip VPU (wave 9; needs E1)
- [ ] [E17](docs/prs/E17.md) Rockchip IEP (wave 9; needs E1)
- [ ] [E18](docs/prs/E18.md) Rockchip JPEG (wave 9; needs E1)
- [ ] [E19](docs/prs/E19.md) A733 video engine (wave 9; needs E1)
- [ ] [E20](docs/prs/E20.md) A733 deinterlacer (wave 9; needs E1)
- [ ] [E21](docs/prs/E21.md) K230 VPU (wave 9; needs E1)
- [ ] [E22](docs/prs/E22.md) K230 GSDMA (wave 9; needs E1)
- [ ] [E31](docs/prs/E31.md) NPU contract (wave 8; needs D1, A6)
- [ ] [E32](docs/prs/E32.md) RKNPU (wave 9; needs E31)
- [ ] [E33](docs/prs/E33.md) VIP9000 (wave 9; needs E31)
- [ ] [E34](docs/prs/E34.md) K230 KPU (wave 9; needs E31)
- [ ] [E35](docs/prs/E35.md) K230 AI2D (wave 9; needs E1)
- [ ] [E23](docs/prs/E23.md) Clock and RTC facts (wave 7; needs A9)
- [ ] [E24](docs/prs/E24.md) Hardware watchdog (wave 7; needs A9)
- [ ] [E25](docs/prs/E25.md) USB gadget setup (wave 7; needs A9, A4)
- [ ] [E26](docs/prs/E26.md) Description-only drivers (wave 8; needs D8)
- [ ] [E27](docs/prs/E27.md) Package drivers (wave 9; needs A11, A15)
- [ ] [E28](docs/prs/E28.md) MCU host and backend (wave 8; needs A9, B8, B10)
- [ ] [E29](docs/prs/E29.md) SystemCore guest (wave 9; needs A15, B2; board run)

## F. Finish

- [ ] [F1](docs/prs/F1.md) Docs (wave 11; needs A15, C7, D9)
- [ ] [F2](docs/prs/F2.md) Final comparison (wave 12; needs C8, D10, F1; board run)
- [ ] [F3](docs/prs/F3.md) Ready for other repositories (wave 13; needs F2)
