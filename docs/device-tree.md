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
- **the module configuration** (`--modprobe`): a modprobe.d fragment. `options lemnos` gives the
  kernel module the nodes the board gives to it (`owned=`, and the role and confinement options:
  `pisp_be=`, `unconfined=`, `confined=`, `gsdma=`, `a733_ve=`, `k230_isp=`, `vip9000=`), and a
  `blacklist` line for each stock module that binds one of those nodes. Each node has its reason
  beside it. Which driver binds an enabled node is not a fact of the tree, so the module
  configuration is what keeps the stock driver off it. A blacklist stops the module loading; a
  driver built into the kernel is not stopped, and the start check reports it.

Every device in the board file must be traceable to one of those outputs. A device whose needs
produce nothing (no node enabled, no line freed, no module option) is refused, unless its driver is
listed in the map's `needs_nothing`.

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

Example: `bus = "pwm:/axi/pcie@1000120000/rp1/pwm@9c000:1"` for channel 1 of the fan's PWM, and
`uses = { line = "gpio:pinctrl-rp1:34" }` for line 34 of the RP1 GPIO chip labelled `pinctrl-rp1`.
| PIO bus | `pio-i2c:...`, `pio-out:gpio=N` | A state machine of the PIO block | The PIO block is in the board's hardware |
| USB gadget | `udc:NAME` | This board as a USB device | The map's `usb-peripheral` controller |

The numbered forms `gpiochipN:LINE` and `pwmchipN:CHANNEL` still work on a board that has no tree
generated. The kernel numbers a chip as it probes, so they can point at another chip after a
reboot: the generator refuses them, and `lemnos-ctl validate` warns about them.

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
- A device that makes nothing is refused, unless its driver is in the map's `needs_nothing`.
- A `module` role must be `owned`, or a role the map gives the controller (`pisp_be` for the PiSP
  back end), and the controller must have a `compatible`. `unconfined` and `confined` need `module`, and not both.
- A `boot` state is `"low"` or `"high"`, and needs a `reason`.
- The board's `soc` must be the map's `soc`.

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
| `usb-peripheral` | path | `dr_mode = "peripheral"` | The controller in peripheral mode, only when the board has a `udc:` entry |
| `watchdog` | path | nothing else | Enables the hardware watchdog on a board that lists it |
| `block` | path | nothing else | Enables a block the kernel module drives (the PiSP back end). Its board entry gives the module option |

A base-tree holder of a line the board lists is disabled:
`status = "disabled"` on that node, with the map's reason.

### Fan, safe from boot

Lemnos has no `pwm-fan` node. The generator enables the PWM block and the channel's pin, and the
fan is Lemnos's own driver's, so no kernel thermal binding is made for it. The SoC's own thermal
throttling and critical shutdown stay, since they are the kernel's.

The fan's line must be safe from power-on until the kernel runs and Lemnos's driver takes it.
That is the `[[lines]]` entry with `boot`: the boot fragment holds the line at that level from
power-on. The same mechanism covers a line that must be on at boot, such as a USB port's power.

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
GPIO chip, `pinctrl-rp1`, a constant in `drivers/pinctrl/pinctrl-rp1.c`). `needs_nothing` lists
the drivers whose devices are composed of other devices and need no hardware.

The parser refuses a map whose paths are not absolute, whose alias is not `i2cN` or `spiN` for an
I2C or SPI controller, or whose pin sets share a GPIO.

## Reference

The reference board's three outputs and the names they use are in
[`testing/board-runs/Q2c.md`](../testing/board-runs/Q2c.md). The overlay compiled and applied to its
base tree is in [`testing/board-runs/Q2.md`](../testing/board-runs/Q2.md); the names there are the
numbered ones that the reference board no longer uses.
