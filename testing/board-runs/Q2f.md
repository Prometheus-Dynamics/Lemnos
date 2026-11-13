# Q2f: VBUS of a USB device port

Status: untested on board. Nothing was loaded on a board, and no device or meter was attached to this PC.

## What changed

- The overlay never leaves a device port's USB 2 PHY with a VBUS supply. Its `phy-supply` is
  replaced by a stand-in, `regulator-lemnos-no-vbus`: a fixed regulator with no GPIO, no enable pin
  and no voltage, which supplies nothing. `/delete-property/` is not used: `dtc` 1.8.1 drops it from a
  plugin overlay, so `fdtoverlay` keeps the base's supply (checked: the blob has no trace of it).
- Each map may give a base tree's VBUS facts (`[bases.NAME.vbus.CONTROLLER]`: the regulator, its
  `regulator-name`, `always_on`, or the `switch` line that enables it). A device port's board entry
  states how its VBUS is handled: `vbus` with `vbus_off` (the line and its off level), or
  `vbus_undriven = true`, each with `vbus_reason`. The generator refuses a port whose base supply can
  be on and whose board says nothing, and a port the base does not describe, naming what to add.
- The reference module's device port has a VBUS fact in its base (`module-io`): no supply. Its PHY is a
  plain USB PHY (`usb-nop-xceiv`) with no supply, and no board tree of rpi-7.2.y sets one on the port
  (`bcm2712-ds.dtsi` `usb@480000`; `&usb` in `bcm2712-rpi-5-b.dts` and `bcm2712-rpi-cm5.dtsi` sets only its
  power domain). Read from the sources, not from a schematic: a meter on the port confirms it.
- `every_fixture_overlay_applies_to_a_base_built_from_its_map` (`service/devtree/tests/apply.rs`): each
  board fixture's overlay is compiled with `dtc -@`, applied with `fdtoverlay` to a base built from its map
  (the map's paths and labels, `-@`), and read back with `fdtget`. A fragment output is merged by `dtc`.
- The start check reads the live PHY's `phy-supply`, the line's level (from `/sys/kernel/debug/gpio`),
  and an always-on regulator's state (from `/sys/class/regulator`).
- Boot holds are written with the SoC's GPIO number (the chip's `number_base` plus the line). Before
  this change the Rockchip fixture's `[[lines]]` hold was written with the chip line alone
  (`gpio=4` for GPIO1 line 4; it is now `gpio=36`).

## Compatibility

- Additive: the board keys `vbus`, `vbus_off`, `vbus_undriven`, `vbus_reason`; the map's `bases.*.vbus`
  and `chips.*.number_base`; the `Live` fields `gpio_debug` and `regulators`; the start check's
  `Findings` (problems and notes).
- Breaking, on purpose: a device port with no statement and a base that either has a supply that can
  be on, or does not describe the port, is now refused. Any board that named a `usb-peripheral` port
  needs a base fact (in its map) or a statement in its board file. The testing boards for the three
  Rockchip SoCs were given theirs.
- Breaking in the boot fragment: the Rockchip `[[lines]]` hold numbers change (see above).

## The stand-in on mainline base trees

Each base tree is the kernel's own board tree at a pinned tag, with its `#include` closure read as raw
files and preprocessed with `cpp -undef`, then compiled with `dtc -@`. Each overlay came from a
minimal board that names the port (the same form the RK3566 and RK3588 fixtures use), compiled with
`dtc -@`, and applied with `fdtoverlay`. `fdtget` read the PHY before and after.

| Board tree (tag) | OTG port `phy-supply` in the base | After `fdtoverlay` |
|---|---|---|
| RK3566 Lubancat-1 (v6.12) | none | `usb2phy@fe8a0000/otg-port` names the stand-in (phandle 596) |
| RK3588S ROCK 5A (v6.12) | none on `u2phy0_otg` (its host PHY has one) | `syscon@fd5d0000/usb2phy@0/otg-port` names the stand-in (phandle 750) |
| RK3576 EVB1 v10 (v6.14), port `usb-drd0` | `vbus5v0_typec` (phandle 23), a GPIO-switched regulator on `gpio0` line 25 | names the stand-in (phandle 952); `vbus5v0_typec` is still in the tree |
| RK3576 EVB1 v10 (v6.14), port `usb-drd1` | `vcc5v0_host` (phandle 24), `regulator-always-on` | names the stand-in (952); `vcc5v0_host` is still in the tree |

Commands (scratch files only, nothing committed): `dtc -@ -I dts -O dtb` on the base and on the overlay;
`fdtoverlay -i base.dtb -o applied.dtb overlay.dtbo`; `fdtget -t u applied.dtb <otg-port> phy-supply`
against `fdtget -t u applied.dtb /regulator-lemnos-no-vbus phandle`.

No mainline tree at v6.12 that was searched has a GPIO-switched regulator on an RK3588 OTG port, so the
switched shape on the RK3588 is covered by the test board and the RK3576 tree, not by a mainline RK3588.

The loaders: `fdtoverlay` (libfdt) was run. U-Boot's `fdt apply`, the Raspberry Pi firmware's overlay
loader and the kernel's configfs overlays were not run here. Each is expected to behave the same: the
first two apply overlays with libfdt, and the kernel's `drivers/of/overlay.c` (v6.12) has no
property-delete form for the supply to need. The image team must confirm the first on the board.

## The boot holds for the three shapes

| Shape | Board statement | Base fact | Hold written to the boot fragment |
|---|---|---|---|
| GPIO-switched, RK3588 test board | `vbus = "gpio:gpio4:11"`, `vbus_off = "low"` | `switch = "gpio:gpio4:11"` | `gpio=139=op,dl` (bank 4 starts at GPIO 128) |
| Always-on on two ports, RK3576 test board | drd0: `vbus_undriven = true`; drd1 (in the test): `vbus_undriven = true` | both `always_on` | none (both are asserted undriven) |
| No supply, RK3566 test board | nothing | `usb-otg0` with no regulator | none |

On the RK3576 mainline tree the GPIO-switched port's line is `gpio0` line 25; the proof board holds
`gpio=25=op,dl` for it (bank 0 starts at 0).

## Mutations (each rule removed, the test run, the source restored and compared)

| Rule removed | Tests that failed |
|---|---|
| the stand-in written on the PHY's OTG port | `a_gadget_makes_its_usb_port_a_peripheral_with_the_phy_it_needs`, `every_device_port_phy_gets_the_stand_in_supply`, `every_family_makes_its_three_outputs_as_its_goldens`, `fdtoverlay_leaves_the_port_with_the_stand_in_and_no_vbus_supply`, `the_fragment_form_leaves_the_port_with_the_stand_in_when_compiled_with_the_base`, `the_stand_in_is_one_node_for_two_device_ports`, `two_always_on_ports_stated_on_their_own_generate_both_stand_ins` |
| the always-on refusal | `an_always_on_port_with_nothing_stated_is_refused_naming_its_regulator`, `a_port_whose_board_states_nothing_is_named_by_the_check_too` |
| the boot hold | `a_gpio_switched_port_holds_its_line_off_in_the_boot_fragment`, `a_hold_at_high_writes_dh`, `an_always_on_port_may_hold_its_vbus_by_a_line_switched_elsewhere`, `every_family_makes_its_three_outputs_as_its_goldens` |
| the start check's stand-in acceptance | `a_phy_with_the_stand_in_has_no_vbus_problem`, `an_enabled_always_on_supply_is_a_note_with_what_the_board_says` |

Other rules are covered by the refusal tests in `service/devtree/tests/vbus.rs` (the line must match the
base's switch; the off level; the reason; both statements; an unknown base; a map's VBUS fact on a
controller that is not a device port; `always_on` with `switch`; a regulator with no name).

## The check to run on a board

1. Write the board's overlay and hold: `lemnos-ctl tree <board> --socs socs -o board.dts --boot boot.txt
   --modprobe lemnos.conf`. Confirm `phy-supply = <&lemnos_no_vbus>;` on each device port's OTG port.
   Compile with `dtc -@ -I dts -O dtb -o board.dtbo board.dts` and apply with the vendor loader's
   `fdtoverlays`.
2. Read the live tree: `/proc/device-tree/<otg-port>/phy-supply` must name `regulator-lemnos-no-vbus`
   (or be absent). `lemnos-ctl validate <board>` (live) must name no VBUS problem; its notes say which
   always-on regulators are enabled.
3. Meter VBUS on the port in device mode, with the cable unplugged: 0 V. Plug the port into a host
   that supplies its own VBUS: the port must not add 5 V to the host's line, and the meter reads what the
   host supplies. Record both readings with the board and the board file's `vbus_reason`.
4. For a `vbus` line: read its level with `/sys/kernel/debug/gpio` once the kernel runs (`out lo` with
   `vbus_off = "low"`), and measure the regulator's enable pin during the firmware or U-Boot stage, where
   the boot hold is the only thing holding it.
5. For a `vbus_undriven` port: measure VBUS as in step 3. If it reads 5 V, the statement is wrong and the
   board must be made to say `vbus`.

Untested on board. The image team's note carries the boot-hold numbers for U-Boot, since no Rockchip loader in
this repository reads the boot fragment.

## Not checked

- The loaders other than `fdtoverlay`; the boot-hold numbers as U-Boot applies them.
- Whether a real base's supply on any board is always on (the facts above come from the mainline trees
  and the test boards, not from a schematic).
- The reference SoC's `usb-peripheral` port: its base tree was not read for a VBUS fact, so a board that
  names that port must state its VBUS.
