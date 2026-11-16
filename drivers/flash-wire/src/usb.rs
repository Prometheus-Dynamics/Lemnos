//! The USB identity a stage presents: the device, its one interface and its two bulk endpoints. A host
//! claims the interface by the vendor and product IDs and by the class; it then uses the bulk pair.
//!
//! The VID:PID is the pid.codes test pair (1209:0001). It is for tests only and is to be replaced before a
//! release. A host must not depend on it beyond finding the device on the bench.

/// The USB vendor ID (pid.codes test pair).
pub const VENDOR_ID: u16 = 0x1209;
/// The USB product ID (pid.codes test pair, to be replaced before a release).
pub const PRODUCT_ID: u16 = 0x0001;
/// The interface class: vendor specific.
pub const INTERFACE_CLASS: u8 = 0xFF;
/// The interface subclass and protocol: none.
pub const INTERFACE_SUBCLASS: u8 = 0x00;
/// The interface protocol: none.
pub const INTERFACE_PROTOCOL: u8 = 0x00;
/// The one configuration's value, set by SET_CONFIGURATION.
pub const CONFIGURATION_VALUE: u8 = 1;
/// The bulk OUT endpoint address: the host's frames come in here.
pub const BULK_OUT: u8 = 0x01;
/// The bulk IN endpoint address: the stage's replies go out here.
pub const BULK_IN: u8 = 0x81;
/// The bulk endpoints' maximum packet size (high speed bulk).
pub const BULK_MAX_PACKET: u16 = 512;
/// The control endpoint's maximum packet size.
pub const EP0_MAX_PACKET: u8 = 64;
/// The product string, string index 2. It names the stage.
pub const PRODUCT: &str = "Lemnos flash stage";
/// The language the strings are in (US English).
pub const LANGUAGE: u16 = 0x0409;
