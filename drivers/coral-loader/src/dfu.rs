//! The parts of USB DFU 1.1 this driver speaks, and the parsing of what a device says. Every byte
//! parsed here comes from the device, so nothing in it is trusted.

/// `bRequest` values (DFU 1.1, table 3.2).
pub const DNLOAD: u8 = 1;
pub const GETSTATUS: u8 = 3;
pub const CLRSTATUS: u8 = 4;
pub const ABORT: u8 = 6;

/// `bmRequestType` of a class request to an interface, host to device.
pub const REQUEST_OUT: u8 = 0x21;
/// `bmRequestType` of a class request to an interface, device to host.
pub const REQUEST_IN: u8 = 0xa1;

/// `bState` values (DFU 1.1, table 6.2).
pub const APP_IDLE: u8 = 0;
pub const APP_DETACH: u8 = 1;
pub const IDLE: u8 = 2;
pub const DNLOAD_SYNC: u8 = 3;
pub const DNBUSY: u8 = 4;
pub const DNLOAD_IDLE: u8 = 5;
pub const MANIFEST_SYNC: u8 = 6;
pub const MANIFEST: u8 = 7;
pub const MANIFEST_WAIT_RESET: u8 = 8;
pub const UPLOAD_IDLE: u8 = 9;
pub const ERROR: u8 = 10;

/// `bStatus` when the last request succeeded.
pub const STATUS_OK: u8 = 0;

/// Bytes in a GETSTATUS reply.
pub const STATUS_LEN: usize = 6;

const INTERFACE_DESCRIPTOR: u8 = 0x04;
const FUNCTIONAL_DESCRIPTOR: u8 = 0x21;
const INTERFACE_LEN: usize = 9;
/// DFU 1.0 descriptors stop after `wTransferSize`; 1.1 adds `bcdDFUVersion`.
const FUNCTIONAL_LEN: usize = 7;

const CLASS_APPLICATION: u8 = 0xfe;
const SUBCLASS_DFU: u8 = 0x01;
/// `bInterfaceProtocol` of an interface in DFU mode. Protocol 1 is the runtime interface, which
/// takes no download.
const PROTOCOL_DFU_MODE: u8 = 0x02;

const ATTR_CAN_DOWNLOAD: u8 = 1 << 0;
const ATTR_MANIFESTATION_TOLERANT: u8 = 1 << 2;

pub const MALFORMED: &str = "the device's descriptors are malformed: retrying";
pub const NO_DFU_INTERFACE: &str = "the device has no interface in DFU mode: retrying";
pub const ZERO_TRANSFER: &str = "the device reports a wTransferSize of 0: retrying";
pub const CANNOT_DOWNLOAD: &str = "the device says it cannot take a download: retrying";
pub const SHORT_STATUS: &str = "the device sent a short DFU status: retrying";

/// What the device's own descriptors say about its DFU interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DfuInterface {
    pub interface: u8,
    pub alternate: u8,
    /// `wTransferSize`: the most the device takes in one DNLOAD. Never zero.
    pub transfer_size: u16,
    /// Whether the device still answers after manifestation.
    pub manifestation_tolerant: bool,
}

/// Finds the first interface in DFU mode in a raw configuration descriptor, and its functional
/// descriptor.
///
/// A functional descriptor counts only inside a DFU interface: type 0x21 is also the HID
/// descriptor. A descriptor that claims to be shorter than its header or longer than what is left
/// ends the walk with an error, so a hostile length cannot move it anywhere.
pub fn find_interface(descriptors: &[u8]) -> Result<DfuInterface, &'static str> {
    let mut rest = descriptors;
    let mut current = None;
    while !rest.is_empty() {
        let length = usize::from(rest[0]);
        if length < 2 || length > rest.len() {
            return Err(MALFORMED);
        }
        let descriptor = &rest[..length];
        match descriptor[1] {
            INTERFACE_DESCRIPTOR => {
                if length < INTERFACE_LEN {
                    return Err(MALFORMED);
                }
                current = (descriptor[5] == CLASS_APPLICATION
                    && descriptor[6] == SUBCLASS_DFU
                    && descriptor[7] == PROTOCOL_DFU_MODE)
                    .then_some((descriptor[2], descriptor[3]));
            }
            FUNCTIONAL_DESCRIPTOR => {
                if let Some((interface, alternate)) = current {
                    if length < FUNCTIONAL_LEN {
                        return Err(MALFORMED);
                    }
                    return functional(interface, alternate, descriptor);
                }
            }
            _ => {}
        }
        rest = &rest[length..];
    }
    Err(NO_DFU_INTERFACE)
}

fn functional(
    interface: u8,
    alternate: u8,
    descriptor: &[u8],
) -> Result<DfuInterface, &'static str> {
    let attributes = descriptor[2];
    let transfer_size = u16::from_le_bytes([descriptor[5], descriptor[6]]);
    if transfer_size == 0 {
        return Err(ZERO_TRANSFER);
    }
    if attributes & ATTR_CAN_DOWNLOAD == 0 {
        return Err(CANNOT_DOWNLOAD);
    }
    Ok(DfuInterface {
        interface,
        alternate,
        transfer_size,
        manifestation_tolerant: attributes & ATTR_MANIFESTATION_TOLERANT != 0,
    })
}

/// A GETSTATUS reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DfuStatus {
    pub status: u8,
    /// `bwPollTimeout`: as much as 24 bits of milliseconds, about four and a half hours.
    pub poll_ms: u32,
    pub state: u8,
}

impl DfuStatus {
    /// Parses a reply. The last two bytes are not read: only `iString` is left there.
    pub fn parse(reply: &[u8]) -> Result<Self, &'static str> {
        if reply.len() < STATUS_LEN {
            return Err(SHORT_STATUS);
        }
        Ok(Self {
            status: reply[0],
            poll_ms: u32::from_le_bytes([reply[1], reply[2], reply[3], 0]),
            state: reply[4],
        })
    }
}
