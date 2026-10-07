# lemnos-drivers-vcm

`#![no_std]`, allocation-free drivers for the voice-coil motors (VCMs) that move camera focus
lenses, over any embedded-hal 1.0 I2C bus, blocking (`Vcm`) and async (`asynch::Vcm`):

| Chip | Command | Position | Power |
|---|---|---|---|
| Dongwoon DW9714 | two bytes, no register: `position << 4` | 10 bits | bit 15 powers down |
| Dongwoon DW9807 | register 0x03-0x04 | 10 bits | control register 0x02: 0 on, 1 off |
| Dongwoon DW9817 (Raspberry Pi Camera Module 3) | as DW9807 | 10 bits | as DW9807 |
| Asahi Kasei AK7375 | register 0x00-0x01: `position << 4` | 12 bits | control register 0x02: 0 active, 0x40 standby |

Other chips (`VcmChip::Custom`) are described with a `VcmFormat` (register, width, shift,
constant bits, power sequences). Features: `alloc` adds `OwnedVcmFormat`, a format that owns
its power sequences (loaded at run time, lent as a `VcmFormat` without allocating); `serde`
(de)serializes `VcmChip` by name (`"dw9817"`, `"custom"`) and, with `alloc`, `OwnedVcmFormat`,
so a sensor description can name its lens chip or spell out its format. The command formats come from Styx (`styx-sensor`'s lens descriptions), with its
tests. VCMs report no position; frame-exact lens scheduling stays in Styx.

```rust
use lemnos_drivers_vcm::Vcm;

fn focus<I: embedded_hal::i2c::I2c, D: embedded_hal::delay::DelayNs>(i2c: I, delay: &mut D) {
    let mut lens = Vcm::dw9817(i2c);
    lens.power_up(delay).ok();
    lens.move_to(512).ok();
}
```
