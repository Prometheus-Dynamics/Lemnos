# R1h: the link's Windows and macOS descriptors (untested on board)

Untested on board. Nothing in this file has been run on Windows or macOS. The Linux side is checked
by the QEMU harness (`testing/crates/gadget-check`), which reads the descriptors the way Windows
requests them. This file is the host test to run on real machines, from the research in
`W4-usb-hosts.md` (worker notes, not in the repository). Sources for the descriptor facts are
Microsoft's "Microsoft OS 1.0 Descriptors Specification" and "WinUSB installation" pages, and the
Linux 7.3 `composite.c`, `f_fs.c` and `configfs.c`, all read 2026-11-13.

## What the gadget presents

- Device: VID:PID `1d6b:0104` (the placeholder in `drivers/usb-gadget`, see `docs/boards.md`, "The
  link's identity"), class 0xEF/0x02/0x01 (composite with interface association descriptors), and
  `bcdDevice` `0x0200` (the release of the current link; see `revision.rs`).
- Interface 0 (class 0xFF, the link): FunctionFS, interface string `orion-link` in the product board
  file. Microsoft OS 1.0 descriptors: compatible ID `WINUSB`, and the property `DeviceInterfaceGUID`
  (REG_SZ) `{CF13388F-BE34-4A1E-94C2-9AAFEF9A04E9}`. The kernel cannot send the plural
  `DeviceInterfaceGUIDs` as REG_MULTI_SZ (the composite wire writer has no case for it), so only the
  singular name is presented.
- Interfaces 1 and 2: CDC NCM. Compatible ID `WINNCM` (community reports, not in Microsoft's list).
- Interfaces 3 and 4: CDC ACM, no OS descriptor.
- String 0xEE: `MSFT100`, vendor code `0x51`.

## Host test, per machine

Plug the device in with no driver installed. Record every line of output. Pass or fail is written per
function on the last line of each block.

### Windows 10 22H2 and Windows 11 (current), PowerShell as administrator

0. The first check, before anything else: does WinUSB register a device interface from the singular
   property alone? The host program's library (nusb 0.2.7) finds the link's child by its hardware id
   ending `&MI_00`, requires its service to be `WinUSB`, and reads `DeviceInterfaceGUID`; it does not
   need the plural name. Run, as administrator, after plugging in:
   - `pnputil /enum-devices /connected /properties | Select-String -Pattern 'MI_00' -Context 0,12`
     (the child's properties: `Service` must read `WinUSB`, and `Hardware IDs` must end `&MI_00`).
   - `Get-PnpDevice -PresentOnly | Where-Object InstanceId -like 'USB\VID_1D6B&PID_0104&MI_00*' | Format-List InstanceId,Class,Status,Problem`
     (Device Manager shows a "WinUSB device" node; its `Class` reads `WinUsbDevice` or `USBDevice`,
     and `Problem` is 0.)
   - `Get-PnpDeviceProperty -InstanceId '<the MI_00 id>' -KeyName DEVPKEY_Device_Service,DEVPKEY_Device_HardwareIds,DEVPKEY_Device_DriverDesc | Format-List`
   - the GUID under the child's Device Parameters:
     `reg query "HKLM\SYSTEM\CurrentControlSet\Enum\USB\VID_1D6B&PID_0104&MI_00\<instance>\Device Parameters" /v DeviceInterfaceGUID`
     It must print `{CF13388F-BE34-4A1E-94C2-9AAFEF9A04E9}`.
   - the interface itself: `Get-PnpDevice -InstanceId 'USB\VID_1D6B&PID_0104&MI_00*' -PresentOnly` plus
     `reg query "HKLM\SYSTEM\CurrentControlSet\Control\DeviceClasses\{CF13388F-BE34-4A1E-94C2-9AAFEF9A04E9}" /s`
     which lists the device interface the WinUSB driver registered.
   PASS if the service is WinUSB, the GUID value is there and the device interface is listed. FAIL
   otherwise, with each output. This check is the one that decides the link; the rest follow it.

1. Nodes:
   `Get-PnpDevice -PresentOnly | Where-Object InstanceId -like 'USB\VID_1D6B&PID_0104*' | Format-List InstanceId,Class,Status,FriendlyName`
   Expect a composite parent and children `MI_00` (link), `MI_01`/`MI_02` (NCM), `MI_03`/`MI_04` (ACM).
2. Driver service and compatible IDs of each child:
   `Get-PnpDeviceProperty -InstanceId '<child id>' -KeyName DEVPKEY_Device_Service,DEVPKEY_Device_DriverDesc,DEVPKEY_Device_CompatibleIds | Format-List`
   Link (`MI_00`): service `WinUSB`, compatible IDs contain `WINUSB`. NCM: record the service and the
   compatible IDs (`WINNCM` may or may not be listed). ACM: service `usbser`.
3. The interface GUID registered for the link:
   `Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Enum\USB\VID_1D6B&PID_0104&MI_00\<instance>\Device Parameters' | Format-List DeviceInterfaceGUID*`
   Record the value. A `DeviceInterfaceGUIDs` value of REG_MULTI_SZ is not expected (see above).
4. The cached OS-descriptor state for this release:
   `Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\usbflags\1d6b01040200' | Format-List osvc`
   Record it before and after the first enumeration.
5. Clearing the cache between tries. Microsoft's documentation names no method (`usb-device-specific-registry-settings`).
   Tried in order, recording which one changed the result: (a) a new release (the policy in
   `revision.rs` already gives one per change); (b) delete the `usbflags\1d6b01040200` key as
   administrator, unplug, replug; (c) reboot.
6. The network and the serial port:
   `Get-NetAdapter | Format-Table Name,InterfaceDescription,Status,DriverInformation`
   `Get-CimInstance Win32_SerialPort | Format-Table DeviceID,Description,PNPDeviceID`
   The host program then opens the link by VID:PID and the interface string `orion-link` (never by GUID
   or tty number), with `nusb` 0.2.7 on Windows, and reads the interface string back.

Pass and fail, per function:

- Link: PASS if the service is `WinUSB`, the GUID value is present and `orion-link` reads back. FAIL with the Device Manager problem code otherwise.
- NCM: PASS if a network adapter appears with no driver prompt. FAIL otherwise, with the service and compatible IDs of the child.
- ACM: PASS if a COM port appears with service `usbser`. FAIL otherwise.

### macOS, current and one version older

1. `system_profiler SPUSBDataType` (record the vendor, product, the interfaces and any `Driver` line).
2. `ioreg -p IOUSB -l -w0 | grep -i -A3 'VID_1d6b\|idProduct" = 260'` (the product 0x0104 is 260).
3. `networksetup -listallhardwareports` and `ifconfig` (the NCM interface, its name, its address).
4. `ls /dev/cu.*` (the ACM port, usually `usbmodem`).
5. The host program opens the link with IOKit through `nusb`, with no kernel driver to detach. Record
   whether `nusb` needed any entitlement or detach.

Pass and fail, per function:

- Link: PASS if the device opens and `orion-link` reads back with no driver installed. FAIL otherwise.
- NCM: PASS if a network service appears and gets an address with no driver install. FAIL otherwise.
- ACM: PASS if `/dev/cu.usbmodem*` appears. FAIL otherwise.

## Linux side, checked in QEMU

The harness's host reads string 0xEE, the extended compatible IDs (`wIndex` 4, to the device) and the
extended properties (`wIndex` 5, to interface 0, with `bmRequestType` 0xC1) byte for byte. Their
expected bytes are the ones Windows should read. Any mismatch fails the check.

Untested on board. The results below are the next board run's to fill in.
