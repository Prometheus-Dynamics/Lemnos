# The device tree

A Lemnos board runs the kernel with a device tree that enables exactly what the board file
uses, and nothing else. The board file is the one description. The tree is generated from it,
so the two cannot drift apart: a board never keeps a second, hand-kept list of overlays.

```sh slow
lemnos-ctl tree --socs socs testing/boards/reference.toml -o "${TMPDIR:-/tmp}/board-overlay.dts" --boot "${TMPDIR:-/tmp}/boot.txt" --modprobe "${TMPDIR:-/tmp}/lemnos.conf"
lemnos-ctl validate --no-live --socs socs testing/boards/reference.toml
```

`tree` needs no daemon and runs on a build host. It writes three outputs, each from the same board file:

- **the overlay** (`-o`, stdout by default): device-tree source with `/plugin/;`. Every fragment
  has the reason it is there in a comment beside it. Compile it with `dtc -@`, and load it where
  the loader applies overlays.
- **the boot fragment** (`--boot`): firmware configuration that must hold before the kernel
  runs, such as a line's level from power-on. It is written as the firmware reads it, and each
  line has its reason. Every `[[lines]]` entry with a `boot` state is in it. The syntax is not
  verified on every board: see the board run that names it.
- **the U-Boot fragment** (`--uboot`): `gpio set` and `gpio clear` for each held line, for a boot
  script to run before `booti` or `sysboot`. The same lines and levels as the boot fragment; written
  only for a SoC whose U-Boot line names the map knows (see [Holds from power-on](#holds-from-power-on)).
- **the module configuration** (`--modprobe`): a modprobe.d fragment. `options lemnos` gives the
  kernel module the nodes the board gives to it (`owned=`, and the role and confinement options:
  `pisp_be=`, `unconfined=`, `confined=`, `gsdma=`, `a733_ve=`, `k230_isp=`, `vip9000=`), and a
  `blacklist` line for each stock module that binds one of those nodes. Each node has its reason
  beside it. Which driver binds an enabled node is not a fact of the tree, so the module
  configuration is what keeps the stock driver off it. A blacklist stops the module loading; a
  driver built into the kernel is not stopped, and the start check reports it.

Every device in the board file must be traceable to one of those outputs, or be a device whose
driver needs no hardware. What a driver needs is in its own manifest: the tool reads the manifests of
the built-in drivers (the list `lemnosd` runs), so no list is kept by hand. A device that names the
hardware its driver needs (in `bus` or `uses`) makes a node enabled, a line freed or a module option.
A device whose driver declares no hardware need (the system identity, a driver built from other
devices) makes nothing, by definition, and is accepted with no name. A device whose driver does
need hardware, and names none, is refused, and the refusal names the need.

`validate` checks the board file as `lemnosd` does. With a `soc`, it also checks the live
device tree (`/proc/device-tree`) and the platform devices (`/sys/bus/platform/devices`) against
the same map, and names each mismatch. `lemnosd` makes the same check at start and only reports
it: a mismatch is never worked around, and the daemon starts anyway.

## The pieces

| Piece | Where | What it says |
|---|---|---|
| Board file | `testing/boards/...`, or the board's own | Which hardware the board has (`[[hardware]]`), which lines it gives to Lemnos (`[[lines]]`), and the devices that use them |
| SoC map | `socs/<soc>.toml` | For each controller: its node's path in the base tree, its alias, its pin sets, its compatible and module roles, and what a stock driver would do with it. The GPIO chips and their labels |
| Base | a `[bases.NAME]` table of the map | The base tree's own nodes that hold a line the board gives to Lemnos, which the overlay disables |

The generator knows no kind of hardware. Everything it says about a controller comes from the
map. A new SoC is a new map file; a new board on a known SoC is a board file.

## Writing a board file for the generator

```toml
[board]
id = "example"
soc = "bcm2712-rp1"      # the map's name for this SoC
base = "module-io"       # the map's base tree, when a base node holds a line the board uses

# Each piece of hardware the devices use, once. Its pins are written here and nowhere else.
[[hardware]]
name = "i2c-1"           # the runtime name the devices use, as the kernel gives it
controller = "i2c1"      # the map's controller
pins = "gpio2_3"         # the map's pin set for that controller

[[hardware]]
controller = "csi0"      # no name: no device names the receiver
module = "owned"         # the kernel module owns the node; `pisp_be` and the others are roles the map gives

# A GPIO line the board gives to Lemnos, by its stable name. `boot` holds it from power-on until
# the kernel takes it.
[[lines]]
line = "gpio:pinctrl-rp1:45"
boot = "high"
reason = "the fan header's line: at full speed from power-on, until Lemnos's fan driver takes it"
```

### Names

| Name | Form | What it is | How it is found |
|---|---|---|---|
| I2C bus | `i2c-N` | The kernel's adapter number | The controller's alias `i2cN` in the map |
| SPI bus | `spidevB.C` | The SPI device | The alias `spiB` |
| GPIO line | `gpio:LABEL:LINE` | A line of the GPIO chip whose label is `LABEL`. The map's `[chips]` table gives the labels | At bind: the chip with that label, found from its character device. Stable across boots |
| PWM channel | `pwm:NODE:CHANNEL` | A channel of the PWM chip whose device-tree node is `NODE`, the node's path as the live tree shows it | At bind: the chip whose device's `of_node` is that path. Stable across boots |
| Watchdog | `watchdog:NODE` | The hardware watchdog whose device-tree node is `NODE`. Named in a device's `uses` (as `watchdog = "watchdog:NODE"`), since the watchdog is the driver's optional need | At bind: the `/dev/watchdogN` whose class device's `of_node` is that path. Stable across boots |

Example: `bus = "pwm:/axi/pcie@1000120000/rp1/pwm@9c000:1"` for channel 1 of the fan's PWM, and
`uses = { line = "gpio:pinctrl-rp1:34" }` for line 34 of the RP1 GPIO chip labelled `pinctrl-rp1`.
| PIO bus | `pio-i2c:...`, `pio-out:gpio=N` | A state machine of the PIO block | The PIO block is in the board's hardware |
| USB gadget | `udc:NAME` | This board as a USB device, through the port the board names | The map's `usb-peripheral` controller, whose `sysfs` is `NAME` |

The numbered forms `gpiochipN:LINE`, `pwmchipN:CHANNEL` and `watchdogN` still work on a board that has
no tree generated. The kernel numbers a device as it probes, so they can point at another device after
a reboot: the generator refuses them, and `lemnos-ctl validate` warns about them. A hw-watchdog device
may also name its node by the `device` setting (`device = "watchdog0"`); the generator cannot see a
setting, so such a device is accepted and enables no node.

### Rules the generator enforces

Each rule has a refusal that names the entry:

- A `controller` must be in the map. The refusal lists the map's controllers.
- A `pins` set must be in that controller's map entry, and a kind that needs pins (PWM, UART)
  must name one. No GPIO may be in two pin sets on one board.
- An `i2c-N` name must come out of the controller's alias `i2cN`. An `spidevB.C` name must come
  out of alias `spiB`. The kernel numbers both that way.
- A device's `i2c-N`, `spidevB.C` or `udc:` name must be named by a `[[hardware]]` entry. A
  `pio-i2c:` or `pio-out:` name needs the PIO block in the board's hardware. A `pwm:` name needs
  the PWM block at that node in the board's hardware.
- A `gpio:` name, on a device or in `[[lines]]`, must name a label the map's `[chips]` table has. A
  device's GPIO line must have a `[[lines]]` entry, and that entry must free something: a base node
  holding the line, or a boot state.
- Every name a device uses must be one of the forms above. Any other name is refused.
- A device names the hardware its driver needs. A device whose driver declares a hardware need (a bus,
  a line, a PWM channel, a watchdog, the PIO block, a USB controller) and names none is refused, naming
  the need. A device whose driver declares none is accepted with no name.
- A `watchdog:` name must be a watchdog node of the map, and its controller a `[[hardware]]` entry.
- A `usb-peripheral` entry (a USB port in device mode) is refused unless a device names it with a
  `udc:` name. Its name is `udc:` and the map's `sysfs`, so a name that is not the controller's UDC
  is refused. Its `role` is `"peripheral"` when it is given, the only role the generator makes.
- An `options` name must be one the map's controller gives in a `needs` entry, and each is named once.
- A `module` role must be `owned`, or a role the map gives the controller (`pisp_be` for the PiSP
  back end), and the controller must have a `compatible`. `unconfined` and `confined` need `module`, and not both.
- A `boot` state is `"low"` or `"high"`, and needs a `reason`.
- The board's `soc` must be the map's `soc`.

## Holds from power-on

A line the board holds from power-on is a `[[lines]]` entry with `boot`, or a device port's VBUS
held at its off level (see [VBUS of a device port](#vbus-of-a-device-port)). Each such hold is written
to up to three outputs, from the same list, and a test checks that they agree line for line
(`service/devtree/tests/holds.rs`):

- **the boot fragment** (`--boot`): `gpio=N=op,dh` or `dl`. This is the Raspberry Pi firmware's file,
  applied before the kernel. Its format is unchanged.
- **the U-Boot fragment** (`--uboot`): `gpio set NAME` for a high level, `gpio clear NAME` for a low
  one, for a boot script to run before `booti` or `sysboot`. It is written only for a map that names
  how U-Boot names its lines (`uboot` in the map, see below). A map without one, with a hold to write,
  is refused by name: `--uboot` names no line it cannot apply.
- **the overlay's `gpio-hog`** (in the overlay, under the chip's node): the kernel asserts the level when
  the chip registers, with no loader step. A hold is hogged by the rule below.

**The hog rule.** A hold gets a `gpio-hog` when no run-time user needs the line. A hogged line is owned
by the kernel, so a driver that requests it would fail, which is why the rule is decided from the board
file and the map:

- A line a device names in `bus` or `uses` is used at run time: its driver requests it when it binds.
  Such a `[[lines]]` hold is not hogged. The boot path holds it until the driver takes it (the RK3566's
  power-switch line is this case). A device port's VBUS line that a device also uses is refused by name:
  VBUS must be held by the overlay, and a device cannot share the line with that hog.
- A line a chosen controller's pin set takes is the pad of that function (a PWM output, say). No hog is
  made: the boot path holds the pad until the driver's pinctrl takes it at probe. The reference board's
  fan line is this case, so the reference board has no hog. A device port's VBUS on such a pad is
  refused by name.
- Otherwise the hold is hogged. Its chip must have a `path` in the map (the chip's node); a chip without
  one is refused by name, since the overlay could not hold the line.

Each base-tree node that holds a line the overlay holds is disabled by the overlay (`status = "disabled"`,
with the reason). A base node that owns the line therefore owns nothing, and the hog takes it. The
VBUS regulator of a port held by its line is disabled the same way: its `phy-supply` is the stand-in, so
nothing uses it, and left enabled it would request the line itself.

Two refusals are made where a hold cannot be written in every output: a line with a boot state twice
(`[[lines]] ... has a boot state twice`), and a VBUS line that is also a `[[lines]]` boot entry (see the
VBUS rules).

**The kernel side, read from source (v6.12 unless noted).** `drivers/gpio/gpiolib-of.c`:
`of_gpiochip_add()` calls `of_gpiochip_scan_gpios()`, which calls `of_gpiochip_add_hog()` for each
available child with `gpio-hog`, so a hog in the base tree is applied as the chip registers. An overlay
applied after the chip registered is also handled: `of_gpio_notify()` takes the
`OF_RECONFIG_CHANGE_ADD` of a `gpio-hog` node and adds its hog to the registered chip (the chip is found
by the node's parent). The Raspberry Pi kernel (raspberrypi/linux `rpi-7.2.y`) differs in its code path
(`gpiochip_add_hog` from `of_gpio_notify`; its hog scan is at `drivers/gpio/gpiolib-of.c:1085`), and
its hog handling was not compared line by line. A hog's `line-name` is the consumer label the kernel
keeps for it (`of_gpiochip_add_hog`: the name is `line-name`, or the node's name when it is absent).
A requested line is listed in `/sys/kernel/debug/gpio` as `gpio-N (line-name) out hi` (`gpiolib.c`,
`gpiolib_dbg_show`), which is how the start check reads a hog.

**Why the VBUS regulator is disabled.** `drivers/regulator/fixed.c` (v6.12) requests a regulator's
`enable-gpios` with `gpiod_get_optional()` and a non-exclusive flag, and a line a hog owns is refused a
second request (`gpiod_request_commit` tests `FLAG_REQUESTED`). The base's regulator would therefore
fail its probe with the hog's line, so the overlay disables it. Not run on a board: the request's error
is read from source only.

**The pad's mux.** A GPIO request reaches the pinctrl core (`pinctrl_gpio_request`, `pinmux_request_gpio`),
which calls the pinctrl driver's gpio hook. Whether the Rockchip and RP1 drivers switch the pad to its GPIO
function there was not verified. A hog on a pad whose reset function is not GPIO is therefore not known to
move the pad: the board run checks the pad with a meter (`testing/board-runs/Q2g.md`).

### The window before the controller probes

From power-on until the GPIO controller probes, the line is at its reset state, and the map does not give
it. What the map knows about the reset state:

| SoC family | Reset direction and pull | Reset function (mux) | What holds the line before the kernel |
|---|---|---|---|
| BCM2712 with RP1 (`socs/bcm2712-rp1.toml`) | unknown: the map has none | unknown | the firmware, from `boot.txt`, before the kernel; the RP1 pad before the firmware writes it is unknown |
| RK3566, RK3576, RK3588 (`socs/rk35*.toml`, `socs/rk3588.toml`) | unknown: the map has none | unknown | the U-Boot fragment, if the boot script runs it before `booti`; before that, the reset state |
| K230, A733 | no GPIO chip in the map: no line can be held | n/a | n/a |

After the controller probes, a hogged line is held from the chip's registration (the hog is applied
inside it). A line held by the boot path only is held from the boot path's write until a driver requests
it, so a driver's own probe is not covered: that is the case the device-tree and board-run note name.

Which boards this matters for: a VBUS regulator that is on by default. The map does not know whether the
base's regulator is enabled at boot (`regulator-boot-on`, or the enable pin's reset level), so the window
of an RK3588 or RK3576 port's VBUS is the U-Boot fragment's, not the overlay's. The image team runs the
U-Boot fragment before `booti`; the window before that is the reset state above.

### U-Boot naming

`uboot` in the map names how U-Boot writes a line. Only `rockchip-bank` is known: a bank is its letter
(`A` for bank 0, `E` for bank 4), and the line is the line within the bank (`group * 8 + pin`), so
`GPIO4_B3` is `E11`. This is the form U-Boot's lookup takes by bank name (`dm_gpio_lookup_name` in
`drivers/gpio/gpio-uclass.c`, v2025.01: a name that starts with the bank's letter, then a decimal offset
within the bank). The global number form (`gpio set 139`) depends on the order the banks probe in, so it
is not written.

| Family | U-Boot form | Source | Verified |
|---|---|---|---|
| Rockchip RK3566 | `gpio set B4` (GPIO 36) | `rk_gpio.c` `rockchip_gpio_probe` (bank from `gpio-ranges`, letter `'A' + bank`); `gpio-uclass.c` `dm_gpio_lookup_name`; `cmd/gpio.c` (`set`, `clear`) | the naming in code, v2025.01; not run on a board |
| Rockchip RK3588 | `gpio clear E11` (GPIO 139) | the same driver; the RK3588 device tree and its banks were not read in U-Boot | not verified: the same driver is assumed |
| Rockchip RK3576 | none written: no hold in its board file | not read | not verified |
| Allwinner A733 | refused: no `sun60i` device tree or compatible in U-Boot v2025.01 (`drivers/gpio/sunxi_gpio.c` has none) | `arch/arm/dts` and `drivers/gpio/` listings | verified absent in v2025.01 |
| RP1 (BCM2712) | refused: U-Boot v2025.01 has no RP1 GPIO driver | `drivers/gpio/` listing | verified absent in v2025.01 |

The U-Boot `gpio` command sets the pad's data and direction registers (`rk_gpio.c` `set_value`,
`direction_output`). It does not switch the pad's mux to GPIO: the Rockchip pinctrl sets a mux from the
device tree's pin states only (`pinctrl-rockchip-core.c`, `rockchip_set_mux`). A pad whose reset function is
not GPIO therefore does not move; the board run reads it.

## What each kind of resource becomes

Every enabled node gets `status = "okay"`. What a kind adds:

| Kind | Map entry | Overlay adds | Why |
|---|---|---|---|
| `i2c` | path, `alias = "i2cN"`, pin sets | `pinctrl-names`, `pinctrl-0` for the pin set; the alias | The kernel numbers the adapter `i2c-N` by its alias |
| `spi` | path, `alias = "spiN"` | the alias | The kernel numbers the SPI bus `spidevN.C` by its alias |
| `uart` | path, alias, pin sets | pin set | A serial port with its pins. No built-in driver takes a UART yet |
| `pwm` | path, pin sets (one per channel's pin) | pin set | Enables the block and the channel's pin function. No `pwm-fan` node: see below |
| `pio` | path | nothing else | Enables the PIO block. Its pins are claimed at run time by the driver |
| `csi` | path, `sysfs`, `stock_driver` | nothing else | Enables the receiver with no sensor node. The module drives it |
| `usb-peripheral` | path, `sysfs`, `needs` | `dr_mode = "peripheral"`, `phys` and `phy-names` for the PHYs the entry takes, and the stand-in for the USB 2 PHY's `phy-supply` | The controller in peripheral mode, for a gadget that names it; the PHYs it is linked to; no VBUS from the port (see [VBUS of a device port](#vbus-of-a-device-port)) |
| `watchdog` | path | nothing else | Enables the hardware watchdog on a board that lists it |
| `block` | path | nothing else | Enables a block the kernel module drives (the PiSP back end). Its board entry gives the module option |

A base-tree holder of a line the board lists is disabled:
`status = "disabled"` on that node, with the map's reason.

### USB device ports

A `usb-peripheral` controller is a port the SoC can run as a device. The board file names the port
(`controller`, and `name = "udc:NAME"` with `NAME` the map's `sysfs`), and a `usb-gadget` device
names the same `udc:NAME`. Its nodes are written only for a port a gadget names: no gadget, no
peripheral node.

- `role = "peripheral"` is the one role the generator makes. It is optional.
- A `needs` entry in the map names a node the controller needs enabled. With no `option`, it is always
  enabled with the controller; with an `option`, only when the board's entry names it in `options`.
  An entry with `phy` (the `phy-names` entry) and `label` (the PHY's label in the base tree) is a PHY
  the controller is linked to. The overlay writes the controller's `phys` as the list of PHYs the
  entry takes, so a port without the SuperSpeed option is linked to its USB 2 PHY only.
- The SoC's base tree links the controller to its SuperSpeed PHY on the RK3588 and RK3576, and that
  PHY must be enabled for the controller to probe. A USB 2 only board therefore leaves that PHY out of
  the `phys`, and the generator writes the USB 2 PHY alone.
- The generator writes no `extcon` and no `maximum-speed`. It always replaces the USB 2 PHY's
  `phy-supply` with the stand-in, so the port drives no VBUS (see below).

The start check, for each port the board names: the node is enabled, its `dr_mode` is `peripheral`
(`host mode` and `no dr_mode` are named), each enabled `needs` node is present and enabled (`PHY or
node disabled`, `is absent`), each PHY the entry takes is in the live `phys` (`PHY link missing`), and,
for a named port, `/sys/class/udc/<sysfs>` exists (`no UDC`). Then its VBUS, as below.

### VBUS of a device port

A USB 2 PHY's OTG port can carry a `phy-supply`: the regulator that drives 5 V onto VBUS for host mode.
A device port that drives VBUS into the host it is plugged into is a hardware hazard, so a port in
device mode never keeps one.

**The supply is enabled in device mode too.** The PHY core enables a PHY's `phy-supply` whatever the
port's mode: `phy_power_on` in `drivers/phy/phy-core.c` (v6.12) calls `regulator_enable(phy->pwr)`, and
`phy->pwr` is `regulator_get_optional(&phy->dev, "phy")`, the `phy-supply` of the node the PHY was
created for. The Rockchip USB 2 PHY creates that PHY for the `otg-port` node
(`drivers/phy/rockchip/phy-rockchip-inno-usb2.c`, `devm_phy_create(dev, child_np, ...)`). The controller
powers its USB 2 PHYs in `dwc3_core_init`, through `dwc3_phy_power_on` (`drivers/usb/dwc3/core.c`), before
`dr_mode` makes it a gadget. Removing the supply is therefore required, not a precaution.

**The overlay replaces it; it does not delete it.** `dtc` 1.8.1 compiles `/delete-property/` inside a
`/plugin/;` overlay to nothing: the blob has no trace of it, and `fdtoverlay` keeps the base's
`phy-supply`. The v6.12 kernel's overlay code has no property-delete form either. What works is a
replacement: the overlay writes `phy-supply = <&lemnos_no_vbus>;` on the PHY's OTG port, and adds the
stand-in at the root, `regulator-lemnos-no-vbus`: a `regulator-fixed` with no GPIO, no enable pin and
no voltage, which no hardware uses, so it supplies nothing. With `form = "fragment"` the same two
nodes are written into the board's own `.dts`, which `dtc` merges with the base.

| Loader | What it does with the overlay | Checked |
|---|---|---|
| `fdtoverlay` (libfdt) | replaces the property; the base regulator stays in the tree | yes: `service/devtree/tests/apply.rs`, and mainline base trees (see the board-run note) |
| `dtc` of a board's `.dts` (`form = "fragment"`) | merged with the base, which then has the stand-in | yes: `apply.rs` |
| U-Boot `fdt apply` | the same libfdt overlay application | no: expected the same, since it is the same library call |
| Raspberry Pi firmware overlay loader | libfdt overlay application | no: expected the same, for the same reason |
| kernel configfs overlay (`drivers/of/overlay.c`, v6.12) | adds and replaces properties; no delete form | no: expected the same, since it has no delete for the supply to need |

**What the generator knows, and what the board says.** The generator reads the base tree's VBUS facts
from the map, under `[bases.NAME.vbus.CONTROLLER]`: `regulator` (the path of the regulator the base's
`phy-supply` names), `regulator_name` (its `regulator-name`, to find it in `/sys/class/regulator`),
`always_on` (`regulator-always-on`, or no enable control: detaching the PHY's supply does not stop
VBUS), `switch` (the `gpio:LABEL:LINE` that enables it), and `reason`. The base is the one the board
names with `[board] base`. A port the base does not describe is not known to have no supply, so the
board must say how its VBUS is handled.

The board says it in the port's `[[hardware]]` entry, with one of two statements:

- `vbus = "gpio:LABEL:LINE"` with `vbus_off = "low"` or `"high"` (the level that is off): VBUS is
  switched by that line. The generator holds the line at its off level in the boot fragment.
- `vbus_undriven = true`: this board does not drive VBUS on this port. An assertion from the schematic,
  checked by a meter (the generator cannot know it).

Both need `vbus_reason`. `vbus_off` or `vbus_reason` without a statement is refused.

| The base says | The board says | Result |
|---|---|---|
| no regulator on the port | nothing | generated |
| no regulator | a statement | generated, with the statement's hold |
| regulator switched by line L | `vbus = L` with `vbus_off` | generated; the boot fragment holds L at its off level |
| regulator switched by L | `vbus_undriven = true` | generated |
| regulator switched by L | nothing, or another line | refused, naming L |
| regulator always on | nothing | refused, naming the regulator and why |
| regulator always on | a statement | generated |
| no fact for the port (no base, or the base is silent) | nothing | refused: the supply cannot be seen |
| no fact for the port | a statement | generated |

Every refusal names the entry, the base, the regulator and the two statements the board may make.

**The boot hold.** The port's VBUS line is held at its off level from power-on in three outputs (see
[Holds from power-on](#holds-from-power-on)): `gpio=N=op,dl` in the boot fragment, where `N` is the SoC's
GPIO number (the chip's `number_base` plus the line; a Rockchip bank is 32 times its number, so `gpio4`
line 11 is GPIO 139); `gpio clear E11` in the U-Boot fragment; and a `gpio-hog` in the overlay, which the
kernel applies with no step of the boot script. The overlay also disables the base regulator that the
line switches, so that nothing else requests the line.

**The start check** (`lemnos-ctl validate`, live) repeats the rule against the running system. For each
device port, with its PHY's live `phy-supply`:

- absent, or the stand-in: no VBUS problem;
- any other supply: a problem, unless the board holds VBUS by a line that reads its off level;
- a `vbus` line is read from the kernel's GPIO listing (`/sys/kernel/debug/gpio`, which needs root and
  debugfs). A level other than the off level is named (`reads hi, but the board says its off level is
  low`). A line no driver has requested is not listed there: a note asks for a meter. A listing that
  cannot be read is named;
- an always-on regulator the base names is found by its name under `/sys/class/regulator`; an enabled
  one is a note, with what the board says about it;
- a statement the generator would refuse is a problem.

Each of these is a finding about one port: the check names it and does not stop the daemon.

### Fan, safe from boot

Lemnos has no `pwm-fan` node. The generator enables the PWM block and the channel's pin, and the
fan is Lemnos's own driver's, so no kernel thermal binding is made for it. The SoC's own thermal
throttling and critical shutdown stay, since they are the kernel's.

The fan's line must be safe from power-on until the kernel runs and Lemnos's driver takes it.
That is the `[[lines]]` entry with `boot`: the boot fragment holds the line at that level from
power-on. The reference board's fan line is the PWM output's pad, so no `gpio-hog` is made for it (see
the hog rule): the pad is held by the boot fragment until the PWM's pin set takes it. The same
mechanism covers a line that must be on at boot, such as a USB port's power.

## What the generator cannot make come out as written

- **Whether the stock driver binds.** An enabled node with a stock driver's `compatible` binds that
  driver when it loads, unless the module claims the node (`driver_override`) or the loader blocks
  the driver. That is a runtime choice, not a property of the tree, so the overlay cannot prevent
  it. The module configuration blacklists the stock module, and the start check reports a stock
  driver bound to a node the board gives to the module, naming both.
- **Stable names are resolved at bind.** A `gpio:` label is matched against the live chips when a
  device binds, and a `pwm:` node against the live PWM chips. The start check (and `validate`
  without `--no-live`) checks them against the machine: a GPIO label must be on a chip the machine
  has, and each line must be in the chip's range; a `pwm:` node must be in the live tree and
  enabled. Each problem names the device or `[[lines]]` entry it is for.
- **Hotplug hints.** A stable name is hinted by any event of its subsystem (`gpio` or `pwm`). The
  event does not carry the chip's label or node, so the hint is "look again" for every chip of the
  subsystem. That costs a retry, and nothing else.

## Writing an SoC map

A map is a TOML file in `socs/`. It is data: the generator reads it and no code names a SoC.

```toml
soc = "bcm2712-rp1"

[controllers.i2c1]
kind = "i2c"                                   # i2c, spi, uart, pwm, pio, csi, usb-peripheral, watchdog, block
path = "/axi/pcie@1000120000/rp1/i2c@74000"    # the node's path in the base tree
alias = "i2c1"                                 # the kernel's alias for it, when it has one
reason = "an I2C bus on RP1; the board's i2c-1"

[controllers.i2c1.pins.gpio2_3]
path = "/axi/pcie@1000120000/rp1/gpio@d0000/rp1_i2c1_2_3"
label = "rp1_i2c1_2_3"                         # the label an overlay refers to
gpios = [2, 3]

[bases.module-io.lines."gpio:pinctrl-rp1:34"]
holders = ["/cam0_reg"]                        # base-tree nodes that drive the line
reason = "the camera power regulator's enable pin; a Lemnos power switch drives it"
```

How to get the values: take the base tree the board boots (the compiled `.dtb`, with its
`__symbols__`). The symbol of each label gives its path. `dtc -I dtb -O dts` prints it; the labels
of pin sets are the ones an overlay can refer to. A path is written as it appears in the live tree.

For a controller with a `sysfs` name, the name is the platform device's name in
`/sys/bus/platform/devices` (for example `1f00110000.csi`). `stock_driver` is the driver the
kernel would bind to it. `stock_modules` are the modules that bind it, for the blacklist. A
controller the module drives needs `compatible`, `vendor,name`, from the node's `compatible` in the
base tree's source (for example `raspberrypi,rp1-cfe`). `module_roles` are the module roles the node
can take besides `owned`: `pisp_be`, `gsdma`, `a733_ve`, `k230_isp`, `vip9000`.

GPIO chips are in `[chips.KEY]`: `label` is the chip's label as the kernel reports it (for the RP1
GPIO chip, `pinctrl-rp1`, a constant in `drivers/pinctrl/pinctrl-rp1.c`). `number_base` is the SoC's GPIO
number of the chip's line 0, and `path` is the chip's controller node in the base tree: the node a hog is
a child of (see [Holds from power-on](#holds-from-power-on)). A chip without `path` cannot hog a line. A
map's top-level `uboot` names how U-Boot writes the chips' lines (`rockchip-bank`). The drivers' needs are not in the map: the tool reads them from the built-in drivers' manifests.

The parser refuses a map whose paths are not absolute, whose alias is not `i2cN` or `spiN` for an
I2C or SPI controller, or whose pin sets share a GPIO.

## SoC families

| Map | Source | Form | Not in the map |
|---|---|---|---|
| `socs/bcm2712-rp1.toml` | the base tree the reference board boots, rpi-7.2.y | overlay | its labels are not checked against a base tree on this host |
| `socs/rk3566.toml` | mainline v6.12, `rk3566.dtsi` / `rk356x.dtsi`, `rk3568-pinctrl.dtsi` | overlay (`fdtoverlays`) | camera receiver and ISP, NPU, video decoder (no mainline node). USB: port 0 in device mode, USB 2 only; its base fact: no VBUS supply on that port |
| `socs/rk3588.toml` | mainline v6.12, `rk3588-base.dtsi`, `rk3588-base-pinctrl.dtsi` | overlay | camera receiver and ISP, NPU, rkvdec (no mainline node). USB: port 0 in device mode, USB 2 or USB 3 by the board's `usb3` option; RK3588S is the same (`rk3588s.dtsi` adds no USB node) |
| `socs/rk3576.toml` | mainline v6.14 (USB, the first tag with the RK3576's USB nodes), `rk3576.dtsi`, `rk3576-pinctrl.dtsi`; GPIO bank names from v6.14 `pinctrl-rockchip.c` | overlay | PWM (no PWM controller node through v6.18), camera receiver and ISP, NPU, video decoder |
| `socs/k230.toml` | `kendryte/k230_sdk` at 7e302f73, `k230.dtsi` (kernel 5.10.4) | fragment (the SDK loads a plain blob) | camera, ISP, GSDMA, KPU (no node in the SDK's SoC description); GPIO chip labels (one node per pin) |
| `socs/sun60i-a733.toml` | a vendor BSP's A733 kernel patch series | fragment (extlinux `fdt` loads a full blob) | I2C, PWM and SPI pin groups (none in the series) |

The `form` key chooses the output. `overlay` (the default) is a `/plugin/;` overlay with
`target-path` fragments, for a loader that applies overlays. `fragment` is `&{/path} { ... };`
blocks, for a board `.dts` that includes the base tree. A fragment names nodes by path, so it needs
no labels except the pin sets it references.

The A733 map (`socs/sun60i-a733.toml`) has no device port, so no VBUS handling applies to it. Its
USB-C PHY has no VBUS supply in the mainline 7.3.4 tree the family runs, plus its vendor series.

### What a base tree must provide

The generator does not read the base tree. It writes paths and labels from the map, and the base must
supply them. Each board fixture is checked against a base built from its map
(`service/devtree/tests/apply.rs`, `every_fixture_overlay_applies_to_a_base_built_from_its_map`), and
the same checks are what a real base needs:

- **Built with symbols**: `dtc -@`, so each label is in `__symbols__`. An overlay's `&label` or
  `phys = <&label>` resolves against that.
- **A node at each map path**: every controller's `path`, each `needs` path and pin set path, and each
  line holder a base names. The overlay enables the node (`status = "okay"`) or disables a holder.
- **The labels the map gives**: each pin set's `label` (the `pinctrl-0` target), and each `needs` entry
  with a `label` (the PHY that the controller's `phys` refers to). `socs/*.toml` names them.
- **Nothing for the VBUS stand-in**: the overlay adds `regulator-lemnos-no-vbus` and its label, so the
  base needs no label for it. A device port's OTG port must exist at its `needs` path, since the
  overlay sets its `phy-supply`.
- **The VBUS supply** a base has on a port is declared in the map's `[bases.NAME.vbus.CONTROLLER]`, not
  read from the base. A base whose map has no such fact is refused for any device port that does not
  state its VBUS (see [VBUS of a device port](#vbus-of-a-device-port)).

A missing label is found when the overlay is applied, not when it is generated. The overlay compiles
with `dtc -@`, and `fdtoverlay` then fails with `Failed to apply '<overlay>': FDT_ERR_NOTFOUND`, which
does not name the label. To find which label the overlay wants, list the overlay's fixups:
`dtc -@ -I dtb -O dts overlay.dtbo` prints a `__fixups__` block with each label and its fragment.

The gate: `apply.rs` runs in CI (the gate job installs `device-tree-compiler`, and the test fails rather
than skips when `CI` is set and a tool is missing). For each board fixture it generates the output,
applies it, and reads the result back with `fdtget`: each controller the board names is `okay`, and each
device port's PHY names the stand-in. A fragment-form output is merged into its base by `dtc` instead.
What the gate does not check is a real vendor base: that is each family's board-run note.

## Reference

The reference board's three outputs and the names they use are in
[`testing/board-runs/Q2c.md`](../testing/board-runs/Q2c.md). The overlay compiled and applied to its
base tree is in [`testing/board-runs/Q2.md`](../testing/board-runs/Q2.md); the names there are the
numbered ones that the reference board no longer uses.
