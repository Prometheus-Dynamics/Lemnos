# Q2e: USB device ports in the RK3566, RK3588 and RK3576 maps

Status: untested on board. Nothing was loaded on a board, and no device was attached to this PC.

## What the maps now say

| SoC | Port the map gives | `udc:` name | PHYs the port is linked to | Board option |
|---|---|---|---|---|
| RK3566 (and the RK3568 on the same nodes) | `usb-otg0`, the DRD controller at `/usb@fcc00000`, port 0 | `udc:fcc00000.usb` | the USB 2 PHY's OTG port only (`usb2phy0_otg`) | none: this controller has no SuperSpeed PHY |
| RK3588 (and RK3588S, which adds no USB node) | `usb-otg0`, the DRD controller at `/usb@fc000000` | `udc:fc000000.usb` | USB 2 PHY 0's OTG port; with `usb3`, also USBDP combo PHY 0 | `usb3` |
| RK3576 | `usb-drd0` at `/soc/usb@23000000`, and `usb-drd1` at `/soc/usb@23400000` | `udc:23000000.usb`, `udc:23400000.usb` | USB 2 PHY 0 or 1's OTG port; with `usb3`, USBDP (drd0) or combo PHY 1 (drd1) | `usb3` |

Sources, read as raw files at a fixed tag: RK3566 `rk3566.dtsi` and `rk356x.dtsi` at v6.12; RK3588
`rk3588-base.dtsi` at v6.12; RK3576 `rk3576.dtsi`, `rk3576-pinctrl.dtsi` and `pinctrl-rockchip.c` at
v6.14 (the first tag with the RK3576's USB nodes; v6.13 has none).

The base trees link each controller to its PHYs (`phys`, `phy-names`). On RK3588 and RK3576 that link
includes the SuperSpeed PHY, which must be enabled for the controller to probe. So a port without the
`usb3` option is linked to its USB 2 PHY alone, and the overlay writes that `phys`. The generator writes
no `phy-supply`, no `extcon` and no `maximum-speed`.

## The board file entry

A device port is named by a `[[hardware]]` entry and a gadget that names the same `udc:` name:

```toml
# RK3566: the USB 2 port
[[hardware]]
name = "udc:fcc00000.usb"
controller = "usb-otg0"
role = "peripheral"

[[devices]]
id = "gadget"
driver = "usb-gadget"
uses = { function = "udc:fcc00000.usb" }
```

`options = ["usb3"]` goes in the entry on RK3588 and RK3576 for a port whose SuperSpeed lanes are wired.

## What was checked, and how

- `lemnos-ctl tree` on the three board files (`testing/boards/rk3566.toml`, `rk3588.toml`, `rk3576.toml`)
  writes the overlays. Each compiles with `dtc -@ -I dts -O dtb`. The RK3566 overlay's USB fragments:
  `/usb@fcc00000` status okay, `dr_mode = "peripheral"`; `/usb2phy@fe8a0000` and its `otg-port` okay.
- Applied to real base trees with `fdtoverlay`. The base was built with `cpp` and `dtc -@` from the
  kernel's own board tree, with the includes fetched at the same tag:
  - RK3566: the mainline board tree at v6.12 that has an OTG port (the one with a host VBUS
    supply on that port). `fdtoverlay` applied the overlay cleanly. The result has
    `dr_mode = "peripheral"`, `status = "okay"` on `usb@fcc00000`, and `status = "okay"` on
    `usb2phy@fe8a0000` and its `otg-port`. The board's own `phy-supply` on the OTG port stays: the
    overlay cannot remove a board property, and a host VBUS supply on a device port is for the board to
    drop.
  - RK3588: the mainline evaluation board tree at v6.12, with the `usb3` option. Applied cleanly; the
    controller has `dr_mode = "peripheral"`, `status = "okay"`, and `phys` for the USB 2 PHY and USBDP PHY 0
    with `phy-names = "usb2-phy", "usb3-phy"`.
  - RK3576: the mainline evaluation board tree at v6.14, with `usb3` on `usb-drd0`. Applied cleanly; the
    controller has `dr_mode = "peripheral"` and `phys` for USB 2 PHY 0 and USBDP. The board tree already
    sets its second port to host mode; the overlay does not touch that port.
- Generator tests (`service/devtree/tests/generate.rs`, 54 tests pass): a gadget makes its port a
  peripheral with its PHYs; the RK3588 port has its USBDP PHY only with `usb3`; the RK3576 drd1 port
  with its own PHYs; no gadget makes no port; a port no gadget names is refused; a controller the map
  lacks is refused with the list; a UDC name that is not the controller's is refused; an option the map
  does not give is refused; a role other than `peripheral` is refused; a usb-peripheral map entry without
  `sysfs` is refused; a needed node with a relative path is refused.
- Start-check tests on fixture live trees: a peripheral port with its PHY on and its UDC present is
  clean; host mode, a missing `dr_mode`, a disabled PHY port, an absent PHY node, a PHY the controller is
  not linked to (`PHY link missing`), and a missing `/sys/class/udc` entry are each named.
- Each rule was removed in turn and the tests rerun (`mutation` results below).

## The check to run on the RK3566 board

1. `lemnos-ctl tree testing/boards/rk3566.toml --socs socs -o board.dts --boot boot.txt --modprobe lemnos.conf`,
   then `dtc -@ -I dts -O dtb -o board.dtbo board.dts`.
2. Apply the overlay with the vendor loader's `fdtoverlays`, and reboot. Read the live tree:
   `/proc/device-tree/usb@fcc00000/dr_mode` reads `peripheral` (with its trailing NUL), and the node's
   `status` is `okay`. The `otg-port` under `usb2phy@fe8a0000` is `okay`.
3. `ls /sys/class/udc` lists `fcc00000.usb`.
4. Plug the port into a host. The host's `lsusb` shows the gadget. This board file gives the gadget no
   `config`, so the host sees the driver's default descriptors; a board that needs a particular identity
   adds its `config` as the board-file docs show.
5. `lemnos-ctl validate testing/boards/rk3566.toml` (with `--socs socs`, live): no `USB port` problem is named.

## Mutation results

Each rule was removed (one at a time, the source restored after each run), and the devtree tests
rerun. The removed rule and the tests that failed:

| Rule removed | Tests that failed |
|---|---|
| a port no device names is refused | `a_port_no_gadget_names_is_refused` |
| the overlay's `needs` nodes are not written | `a_gadget_makes_its_usb_port_a_peripheral_with_the_phy_it_needs`, `every_family_makes_its_three_outputs_as_its_goldens`, `the_rk3576_ports_name_their_own_udc_and_phys`, `the_rk3588_port_enables_its_usb3_phy_only_when_the_board_names_usb3` |
| an option the map does not give is refused | `a_port_option_the_map_does_not_give_is_refused` |
| the UDC name must be the controller's | `a_udc_name_must_be_the_controllers_udc` |
| the role must be peripheral | `a_port_role_other_than_peripheral_is_refused` |
| `dr_mode = "peripheral"` is written | `a_gadget_makes_its_usb_port_a_peripheral_with_the_phy_it_needs`, `every_family_makes_its_three_outputs_as_its_goldens`, `every_resource_kind_is_generated`, `the_rk3576_ports_name_their_own_udc_and_phys`, `the_rk3588_port_enables_its_usb3_phy_only_when_the_board_names_usb3` |
| `phys` and `phy-names` are written | `every_family_makes_its_three_outputs_as_its_goldens` |
| the start check's host-mode check | `a_port_in_host_mode_is_named` |
| the start check's PHY link check | `a_port_its_phy_is_not_linked_to_is_named` |
| the start check's UDC presence check | `a_port_with_no_udc_is_named` |
| a usb-peripheral map entry needs `sysfs` | `a_usb_peripheral_map_entry_needs_its_udc_name` |
| a needed node's path must be absolute | `a_needed_node_must_have_an_absolute_path` |

Each removal was a one-line change, the source restored from a copy afterwards, and the devtree tests rerun. Every removed rule failed at least one test named for it.

## Not checked

- Nothing was loaded on a board, and the gadget was not attached to a host. Whether the UDC name
  `fcc00000.usb` (and `23000000.usb`, `fc000000.usb`) is what the kernel registers is the platform
  device's name from the node's address; that is the usual naming, and step 3 above checks it.
- The `udc:` name of the Raspberry reference SoC's port (`1000480000.usb` in `socs/bcm2712-rp1.toml`)
  is taken from the bus address, not read from a board.
- `dr_mode = "peripheral"` with no `extcon` on the RK3566 or RK3588: the fixed peripheral role does not
  use `extcon`. Whether a board needs VBUS detection on the port is unproven on hardware.
