# lemnos-drivers-ws2812

`#![no_std]` frame model for WS2812 and SK6812 addressable LED strips: `StripConfig` (LED
count, `Wire::Rgb` or `Wire::Rgbw`, the index offset of logical LED 0, brightness) and
encoders for two transports:

- `encode_rp1`: the Raspberry Pi RP1 `ws2812-pio` character device (`/dev/ledsN`), 4 bytes
  per LED (`r | g << 8 | b << 16 | w << 24`, little-endian); the kernel driver applies gamma
  and the GRB/GRBW wire order.
- `encode_spi`: the strip's bit stream for a 2.4 MHz SPI bus (three SPI bits per data bit),
  for microcontrollers.

`lemnos-drivers-linux::Ws2812Pio` is the device-model LED strip (`Pixels` + `brightness` and
`color` controls) over the RP1 device; `lemnosd` arbitrates LED intents on top of it.
