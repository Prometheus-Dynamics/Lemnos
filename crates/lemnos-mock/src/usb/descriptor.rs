use lemnos_core::{
    CapabilityAccess, CapabilityDescriptor, DeviceAddress, DeviceDescriptor, DeviceKind,
    DeviceLink, DeviceRelation,
};

#[derive(Clone, Copy, Default)]
pub(crate) struct MockUsbInterfaceIdentity {
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
}

#[derive(Clone, Copy)]
pub(crate) struct MockUsbInterfaceDescriptor {
    pub identity: MockUsbInterfaceIdentity,
    pub interface_number: u8,
    pub alternate_setting: Option<u8>,
    pub class_code: Option<u8>,
    pub subclass_code: Option<u8>,
    pub protocol_code: Option<u8>,
}

pub(crate) fn build_interface_descriptor(
    device_descriptor: &DeviceDescriptor,
    interface: MockUsbInterfaceDescriptor,
) -> DeviceDescriptor {
    let (bus, ports) = (usb_bus(device_descriptor), usb_ports(device_descriptor));
    let ports_label = ports_label(&ports);
    let mut builder = DeviceDescriptor::builder_for_kind(
        format!(
            "mock.usb.bus{bus}.ports{ports_label}.if{}",
            interface.interface_number
        ),
        DeviceKind::UsbInterface,
    )
    .expect("mock usb interface builder")
    .display_name(format!(
        "usb-{bus}-{ports_label}:if{}",
        interface.interface_number
    ))
    .summary("Mock USB interface")
    .address(DeviceAddress::UsbInterface {
        bus,
        ports: ports.clone(),
        interface_number: interface.interface_number,
        vendor_id: interface.identity.vendor_id,
        product_id: interface.identity.product_id,
    })
    .driver_hint("lemnos.usb.generic")
    .label("bus", bus.to_string())
    .property("bus", u64::from(bus))
    .property("ports", ports_display(&ports))
    .property("interface_number", u64::from(interface.interface_number))
    .link(DeviceLink::new(
        device_descriptor.id.clone(),
        DeviceRelation::Parent,
    ))
    .capability(usb_capability(
        "usb.control_transfer",
        CapabilityAccess::READ_WRITE,
    ))
    .capability(usb_capability("usb.bulk_read", CapabilityAccess::READ))
    .capability(usb_capability("usb.bulk_write", CapabilityAccess::WRITE))
    .capability(usb_capability("usb.interrupt_read", CapabilityAccess::READ))
    .capability(usb_capability(
        "usb.interrupt_write",
        CapabilityAccess::WRITE,
    ))
    .capability(usb_capability(
        "usb.claim_interface",
        CapabilityAccess::CONFIGURE,
    ))
    .capability(usb_capability(
        "usb.release_interface",
        CapabilityAccess::CONFIGURE,
    ));

    if let Some(alternate_setting) = interface.alternate_setting {
        builder = builder.property("alternate_setting", u64::from(alternate_setting));
    }
    if let Some(class_code) = interface.class_code {
        builder = builder.property("class_code", u64::from(class_code));
    }
    if let Some(subclass_code) = interface.subclass_code {
        builder = builder.property("subclass_code", u64::from(subclass_code));
    }
    if let Some(protocol_code) = interface.protocol_code {
        builder = builder.property("protocol_code", u64::from(protocol_code));
    }

    builder.build().expect("mock usb interface descriptor")
}

fn usb_capability(id: &str, access: CapabilityAccess) -> CapabilityDescriptor {
    CapabilityDescriptor::new(id, access).expect("static USB capability identifiers are valid")
}

pub(crate) fn usb_bus(descriptor: &DeviceDescriptor) -> u16 {
    match descriptor.address.as_ref() {
        Some(DeviceAddress::UsbDevice { bus, .. }) => *bus,
        Some(DeviceAddress::UsbInterface { bus, .. }) => *bus,
        other => panic!("unexpected mock USB descriptor address: {other:?}"),
    }
}

pub(crate) fn usb_ports(descriptor: &DeviceDescriptor) -> Vec<u8> {
    match descriptor.address.as_ref() {
        Some(DeviceAddress::UsbDevice { ports, .. }) => ports.clone(),
        Some(DeviceAddress::UsbInterface { ports, .. }) => ports.clone(),
        other => panic!("unexpected mock USB descriptor address: {other:?}"),
    }
}

fn ports_label(ports: &[u8]) -> String {
    ports
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join("_")
}

fn ports_display(ports: &[u8]) -> String {
    ports
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(".")
}
