# Q2c: stable chip names, the module configuration, and the reference board's three outputs

Status: untested on board. Everything here was run on the build host. Nothing was loaded on the
board, and no start check ran against a live board tree.

## What was checked, against what

- The reference board's outputs are golden files in `service/devtree/tests/golden/`:
  `reference.dts` (overlay), `reference-boot.txt` (boot fragment) and `reference-modprobe.conf`
  (module configuration). `lemnos-ctl tree` writes them; the golden tests compare the generator to
  them.
- The overlay compiles with `dtc -@` (DTC 1.8.1, this machine). It was not applied to a base tree:
  the base tree (`bcm2712-rpi-cm5-cm5io.dtb`) is not on the build host, so the labels it refers to
  (`rp1_i2c1_2_3`, `rp1_pwm1_gpio45`) are unchecked against it.
- The RP1 GPIO chip's label, `pinctrl-rp1`, is the constant `MODULE_NAME` in
  `drivers/pinctrl/pinctrl-rp1.c` of `raspberrypi/linux` at `rpi-7.2.y` (read as a raw file). The
  kernel reports it through `GPIO_GET_CHIPINFO`, so the backend matches it there. Not read from the
  board's `/dev/gpiochipN`.
- The PWM node `raspberrypi,rp1-pwm` (`pwm@9c000`, `pwm-rp1.c`) has no chip name in the driver. Its
  chip is named by its device-tree node: the path under `/sys/firmware/devicetree/base`, which the
  chip's `device/of_node` link gives. The path form follows the kernel's OF sysfs layout; it is not
  read from a running board.
- The camera receiver's compatible, `raspberrypi,rp1-cfe` (the `csi@110000` node), is from
  `arch/arm64/boot/dts/broadcom/rp1.dtsi` of `rpi-7.2.y` (raw file). Its stock module is named
  `rp1-cfe` in the map; the module name spelling was not compared against the driver's Makefile.

## Names on the reference board

| Device | Name | Output it makes |
|---|---|---|
| `ring` | `pio-out:gpio=13` | the RP1 PIO block enabled |
| `imu` | `i2c-1` | RP1 I2C1 enabled, alias `i2c1`, pins on GPIO2 and GPIO3 |
| `sensor` | `i2c-10` | the SoC's I2C10 enabled, alias `i2c10` |
| `cam0-power` | `gpio:pinctrl-rp1:34` | the base node `/cam0_reg` disabled (the line is freed) |
| `fan` | `pwm:/axi/pcie@1000120000/rp1/pwm@9c000:1` | the PWM block enabled with GPIO45's pin set |

The fan's GPIO45 is also the boot line `gpio:pinctrl-rp1:45`, held high from power-on.
The receiver `csi0` is `module = "owned"`: the module owns `raspberrypi,rp1-cfe` and the stock
`rp1-cfe` module is blacklisted. The PiSP back end `pisp-be` (`/axi/pisp_be@880000`, from
`bcm2712-ds.dtsi` of rpi-7.2.y) is `module = "pisp_be"`: the module owns `raspberrypi,pispbe` in the
`pisp_be` role.

## The outputs

Module configuration (`--modprobe`):

```
options lemnos owned=raspberrypi,pispbe,raspberrypi,rp1-cfe pisp_be=raspberrypi,pispbe
blacklist pispbe
blacklist rp1-cfe
```

The PiSP back end's `pispbe` blacklist uses the platform driver's name (`.name = PISPBE_NAME` in
`pisp_be.c`); the module's file name was not read, so check it on the board.

Boot fragment (`--boot`):

```
gpio=45=op,dh
```

## Checks to run on the board

1. `lemnos-ctl tree testing/boards/reference.toml --socs socs -o board.dts --boot boot.txt --modprobe lemnos.conf`,
   then `dtc -@ -I dts -O dtb -o board.dtbo board.dts` and `fdtoverlay -i <base.dtb> -o applied.dtb board.dtbo`.
   Read back: `/sys/firmware/devicetree/base/axi/pcie@1000120000/rp1/pwm@9c000/status` is `okay`, and
   `cam0_reg` is `disabled`.
2. Install `lemnos.conf` in `/etc/modprobe.d/`, load `lemnos.ko`, and start `lemnosd --check` with
   the board file. With `rp1-cfe` bound to `1f00110000.csi` the start check reports it and names the
   module's node.
3. `/sys/class/pwm/pwmchipN/device/of_node` resolves to `.../rp1/pwm@9c000`, so `pwm:` binds the
   fan's channel 1 and not the other RP1 PWM. Likewise `/dev/gpiochipN` with label `pinctrl-rp1`
   answers `gpio:pinctrl-rp1:34`.
4. The start check reports no problem for `gpio:pinctrl-rp1:34` (a chip labelled `pinctrl-rp1` with
   more than 34 lines is on the machine).
5. The camera power switch drives the regulator line, and the fan runs at full duty when Lemnos releases it.

Until these run, the stable names and the module configuration are untested on board.
