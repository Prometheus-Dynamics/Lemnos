# BB1: serving data for the bench SoCs, and the A733 USB OTG port

Not run on a board. Every entry below is from a source; the marks say what is not confirmed.

## The A733 USB OTG port (`socs/sun60i-a733.toml`, `[controllers.usb-otg0]`)

Source: the DockSeed series, `patches/a733-usb0-gadget/arm64-dts-allwinner-a733-add-the-usb0-otg-controller.patch`
(commit fe0a8ed, `sun60i-a733.dtsi`), read through the Atlas research notes (bb1b-facts).

- Node: `usb_otg: usb@4100000`; compatible `allwinner,sun60i-a733-musb`, `allwinner,sun8i-a33-musb`; reg `0x04100000`
  size `0x400`; interrupt GIC_SPI 156 "mc"; clock CLK_BUS_OTG; reset RST_BUS_OTG; `phys = <&usbphy 0>`; `status = "disabled"`
  in the SoC file. The board patch sets `dr_mode = "peripheral"` and `status = "okay"`.
- Path in the map: `/soc@0/usb@4100000`. The series renames the bus node `soc@0` in a later patch. **To confirm in a built
  dtb.** Same caveat as the r_wdt row.
- The port's UDC name: **UNVERIFIED, to confirm on board.** The kernel names the controller `musb-hdrc.N.auto`, under the
  platform device `4100000.usb`. The map names the port `4100000.usb`, and a gadget names it `udc:4100000.usb`. The backend
  (`backends/linux/src/gadget.rs`, `resolve_udc`) resolves that name to the one class entry whose sysfs path runs through
  a directory of that name. `N` is not fixed, so no board file names it.
- PHY: **not in the map.** The series names `phys = <&usbphy 0>`, and the PHY node's path is in no source read. A gadget
  on this port works only once its PHY is named, so this is a known gap. **To confirm on board.**
- VBUS: the map has no base VBUS fact for this port. The series' SoC tree has no VBUS supply for the USB-C PHY (the Atlas
  notes), so a board file that gives the port must state `vbus_undriven` with the reason, as the other device ports do.
  **To confirm on a meter.**
- Boards: the port is "peripheral only" on the board with the Type-C connector. The other boards on the same controller
  (the A733 variant on port 1) are not read yet.

## The runtime resolution (`lemnos-linux`, `resolve_udc`)

- A name that is an entry of `/sys/class/udc` is itself. Otherwise the one entry whose path runs through a directory of
  that name. None when there is none or several (ambiguous, so the gadget waits).
- Used by `FunctionFsGadget::new` and by `prepare`, the privileged setup, so both see the same controller.
- The hotplug hints (`hotplug/hints.rs`) still watch the class entry by the name in the board file. A parent name does not
  match the class event, so a gadget on this port waits on its retries, not on the event. Known, not changed here.

## Tests

- `backends/linux/src/gadget/tests.rs`: `a_platform_device_name_resolves_to_its_controller_entry` (passes).
- `service/devtree/tests/usb_port.rs`: a gadget on `udc:4100000.usb` passes the generator; a port the map lacks is refused
  (both pass).
