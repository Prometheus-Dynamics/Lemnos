use super::descriptor::{
    MockUsbInterfaceDescriptor, MockUsbInterfaceIdentity, build_interface_descriptor, usb_bus,
    usb_ports,
};
use lemnos_core::{
    CapabilityAccess, CapabilityDescriptor, DeviceAddress, DeviceDescriptor, DeviceKind,
    UsbControlTransfer, UsbDirection,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

const USB_PROPERTY_VENDOR_ID: &str = "vendor_id";
const USB_PROPERTY_PRODUCT_ID: &str = "product_id";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct MockUsbControlKey {
    pub direction: UsbDirection,
    pub request_type: lemnos_core::UsbRequestType,
    pub recipient: lemnos_core::UsbRecipient,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub data: Vec<u8>,
}

impl From<&UsbControlTransfer> for MockUsbControlKey {
    fn from(value: &UsbControlTransfer) -> Self {
        Self {
            direction: value.setup.direction,
            request_type: value.setup.request_type,
            recipient: value.setup.recipient,
            request: value.setup.request,
            value: value.setup.value,
            index: value.setup.index,
            data: value.data.clone(),
        }
    }
}

#[derive(Clone)]
struct MockUsbInterfaceConfig {
    interface_number: u8,
    alternate_setting: Option<u8>,
    class_code: Option<u8>,
    subclass_code: Option<u8>,
    protocol_code: Option<u8>,
}

#[derive(Clone)]
pub struct MockUsbDevice {
    device_descriptor: DeviceDescriptor,
    interfaces: Vec<MockUsbInterfaceConfig>,
    control_responses: BTreeMap<MockUsbControlKey, Vec<u8>>,
    bulk_in_responses: BTreeMap<u8, VecDeque<Vec<u8>>>,
    interrupt_in_responses: BTreeMap<u8, VecDeque<Vec<u8>>>,
    claimed_interfaces: BTreeMap<u8, Option<u8>>,
    last_control_out: Option<UsbControlTransfer>,
    last_bulk_writes: BTreeMap<u8, Vec<u8>>,
    last_interrupt_writes: BTreeMap<u8, Vec<u8>>,
}

impl MockUsbDevice {
    pub fn new(bus: u16, ports: impl Into<Vec<u8>>) -> Self {
        let ports = ports.into();
        let ports_label = ports
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join("_");
        let descriptor = DeviceDescriptor::builder_for_kind(
            format!("mock.usb.bus{bus}.ports{ports_label}.dev"),
            DeviceKind::UsbDevice,
        )
        .expect("mock usb device builder")
        .display_name(format!("usb-{bus}-{ports_label}"))
        .summary("Mock USB device")
        .address(DeviceAddress::UsbDevice {
            bus,
            ports: ports.clone(),
            vendor_id: None,
            product_id: None,
        })
        .driver_hint("lemnos.usb.generic")
        .label("bus", bus.to_string())
        .property("bus", u64::from(bus))
        .property(
            "ports",
            ports
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join("."),
        )
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
        ))
        .build()
        .expect("mock usb device descriptor");

        Self {
            device_descriptor: descriptor,
            interfaces: Vec::new(),
            control_responses: BTreeMap::new(),
            bulk_in_responses: BTreeMap::new(),
            interrupt_in_responses: BTreeMap::new(),
            claimed_interfaces: BTreeMap::new(),
            last_control_out: None,
            last_bulk_writes: BTreeMap::new(),
            last_interrupt_writes: BTreeMap::new(),
        }
    }

    pub fn with_display_name(mut self, display_name: impl Into<String>) -> Self {
        self.device_descriptor.display_name = Some(display_name.into());
        self
    }

    pub fn with_vendor_product(mut self, vendor_id: u16, product_id: u16) -> Self {
        self.device_descriptor.address = Some(DeviceAddress::UsbDevice {
            bus: usb_bus(&self.device_descriptor),
            ports: usb_ports(&self.device_descriptor),
            vendor_id: Some(vendor_id),
            product_id: Some(product_id),
        });
        self.device_descriptor.match_hints.vendor = Some(format!("{vendor_id:04x}"));
        self.device_descriptor.match_hints.model = Some(format!("{product_id:04x}"));
        self.device_descriptor
            .properties
            .insert(USB_PROPERTY_VENDOR_ID.into(), u64::from(vendor_id).into());
        self.device_descriptor
            .properties
            .insert(USB_PROPERTY_PRODUCT_ID.into(), u64::from(product_id).into());
        self
    }

    pub fn with_serial_number(mut self, serial_number: impl Into<String>) -> Self {
        self.device_descriptor.match_hints.serial_number = Some(serial_number.into());
        self
    }

    pub fn with_interface(self, interface_number: u8) -> Self {
        self.with_interface_details(interface_number, None, None, None, None)
    }

    pub fn with_interface_details(
        mut self,
        interface_number: u8,
        alternate_setting: Option<u8>,
        class_code: Option<u8>,
        subclass_code: Option<u8>,
        protocol_code: Option<u8>,
    ) -> Self {
        self.interfaces
            .retain(|candidate| candidate.interface_number != interface_number);
        self.interfaces.push(MockUsbInterfaceConfig {
            interface_number,
            alternate_setting,
            class_code,
            subclass_code,
            protocol_code,
        });
        self.interfaces
            .sort_by_key(|interface| interface.interface_number);
        self
    }

    pub fn with_control_response(
        mut self,
        transfer: UsbControlTransfer,
        response: impl Into<Vec<u8>>,
    ) -> Self {
        self.control_responses
            .insert(MockUsbControlKey::from(&transfer), response.into());
        self
    }

    pub fn with_bulk_in_response(mut self, endpoint: u8, bytes: impl Into<Vec<u8>>) -> Self {
        self.bulk_in_responses
            .entry(endpoint)
            .or_default()
            .push_back(bytes.into());
        self
    }

    pub fn with_interrupt_in_response(mut self, endpoint: u8, bytes: impl Into<Vec<u8>>) -> Self {
        self.interrupt_in_responses
            .entry(endpoint)
            .or_default()
            .push_back(bytes.into());
        self
    }

    pub fn descriptor(&self) -> &DeviceDescriptor {
        &self.device_descriptor
    }
}

#[derive(Clone)]
pub(crate) struct MockUsbDeviceState {
    pub device_descriptor: DeviceDescriptor,
    pub interface_descriptors: Vec<DeviceDescriptor>,
    pub interface_numbers: BTreeSet<u8>,
    pub control_responses: BTreeMap<MockUsbControlKey, Vec<u8>>,
    pub bulk_in_responses: BTreeMap<u8, VecDeque<Vec<u8>>>,
    pub interrupt_in_responses: BTreeMap<u8, VecDeque<Vec<u8>>>,
    pub claimed_interfaces: BTreeMap<u8, Option<u8>>,
    pub last_control_out: Option<UsbControlTransfer>,
    pub last_bulk_writes: BTreeMap<u8, Vec<u8>>,
    pub last_interrupt_writes: BTreeMap<u8, Vec<u8>>,
}

impl From<MockUsbDevice> for MockUsbDeviceState {
    fn from(value: MockUsbDevice) -> Self {
        let identity = match &value.device_descriptor.address {
            Some(DeviceAddress::UsbDevice {
                vendor_id,
                product_id,
                ..
            }) => MockUsbInterfaceIdentity {
                vendor_id: *vendor_id,
                product_id: *product_id,
            },
            _ => MockUsbInterfaceIdentity::default(),
        };
        let interface_descriptors = value
            .interfaces
            .iter()
            .map(|interface| {
                build_interface_descriptor(
                    &value.device_descriptor,
                    MockUsbInterfaceDescriptor {
                        identity,
                        interface_number: interface.interface_number,
                        alternate_setting: interface.alternate_setting,
                        class_code: interface.class_code,
                        subclass_code: interface.subclass_code,
                        protocol_code: interface.protocol_code,
                    },
                )
            })
            .collect::<Vec<_>>();

        Self {
            interface_numbers: value
                .interfaces
                .iter()
                .map(|interface| interface.interface_number)
                .collect(),
            device_descriptor: value.device_descriptor,
            interface_descriptors,
            control_responses: value.control_responses,
            bulk_in_responses: value.bulk_in_responses,
            interrupt_in_responses: value.interrupt_in_responses,
            claimed_interfaces: value.claimed_interfaces,
            last_control_out: value.last_control_out,
            last_bulk_writes: value.last_bulk_writes,
            last_interrupt_writes: value.last_interrupt_writes,
        }
    }
}

fn usb_capability(id: &str, access: CapabilityAccess) -> CapabilityDescriptor {
    CapabilityDescriptor::new(id, access).expect("static USB capability identifiers are valid")
}
