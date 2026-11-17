//! The boot ROM's USB download mode: EP0 vendor requests read the CPU info and set the load
//! address, a RAM image goes over bulk out in pages, then a request starts it. The board resets as
//! the image starts, so the device comes back at the same port as whatever image was started.
//!
//! Sources (k230_flash 33a75f0 unless stated; `k230_flash` is the vendor's burner, `kendryte/k230_sdk`
//! the SDK):
//! - Vendor IDs: `kburn.h:205` (VID 0x29f1 default), `kburn.cpp:403-405` (PID 0x0230 selects the K230).
//! - Requests: `kburn_k230.h:19-25` (GET_CPU_INFO 0, SET_DATA_ADDRESS 1, SET_DATA_LENGTH 2 unused,
//!   PROG_START 4). Bulk endpoints: `kburn_k230.h:13-14` (OUT 0x01, IN 0x81).
//! - Page size 1000: `burner_brom.cpp:119`. Load-address range 0x80300000..=0x80400000:
//!   `k230_flash.cpp:109`; the default 0x80360000: `k230_flash.cpp:403`, `kburn_k230.h:31`.
//! - CPU info: `probe_device.cpp:9-61`, 32 bytes at request 0 with wValue 0.
//! - Start: `burner_brom.cpp:16-37`; the image is then started at the address it was loaded to.
//!
//! UNVERIFIED: the boot ROM's own address limit, what it answers on error, and whether the start
//! request's status stage completes or the device resets first (see `docs/flashing.md`).

use std::ops::RangeInclusive;

use crate::transport::{UsbError, UsbIo};

/// `EP0_GET_CPU_INFO` (`kburn_k230.h:20`).
pub const GET_CPU_INFO: u8 = 0;
/// `EP0_SET_DATA_ADDRESS` (`kburn_k230.h:21`).
pub const SET_DATA_ADDRESS: u8 = 1;
/// `EP0_PROG_START` (`kburn_k230.h:24`).
pub const PROG_START: u8 = 4;
/// The CPU info's length (`probe_device.cpp:19`).
pub const CPU_INFO_LEN: u16 = 32;
/// The boot ROM's bulk page (`burner_brom.cpp:119`, `K230_SRAM_PAGE_SIZE`).
pub const PAGE: usize = 1000;
/// The range k230_flash accepts for a load address (`k230_flash.cpp:109`). The boot ROM's own limit
/// is UNVERIFIED. The caller always passes the address; nothing here picks one.
pub const LOAD_ADDRESS_RANGE: RangeInclusive<u32> = 0x8030_0000..=0x8040_0000;

/// The text the vendor's boot ROM answers (`probe_device.cpp:55`).
pub const TEXT_BOOT_ROM: &str = "K230";
/// The text the vendor's burner answers (`probe_device.cpp:49`).
pub const TEXT_VENDOR_BURNER: &str = "Uboot Stage for K230";

/// Which stage answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stage {
    /// The boot ROM ("K230").
    BootRom,
    /// The vendor's burner ("Uboot Stage for K230").
    Burner,
    /// Our stage (`lemnos_k230_usbboot_stage::format::STAGE_IDENT`).
    Lemnos,
    /// Something else answered, with its text.
    Unknown(String),
}

/// Asks the device which stage answers (`probe_device.cpp:9-61`): request 0, wValue 0, 32 bytes.
pub fn stage(io: &mut dyn UsbIo) -> Result<Stage, UsbError> {
    let reply = io.control_in(GET_CPU_INFO, 0, 0, CPU_INFO_LEN)?;
    let text = String::from_utf8_lossy(&reply)
        .trim_matches(|c: char| c == '\0' || c.is_whitespace())
        .to_string();
    Ok(match text.as_str() {
        TEXT_VENDOR_BURNER => Stage::Burner,
        TEXT_BOOT_ROM => Stage::BootRom,
        other if other == lemnos_k230_usbboot_stage::format::STAGE_IDENT => Stage::Lemnos,
        _ => Stage::Unknown(text),
    })
}

/// Refuses an address outside [`LOAD_ADDRESS_RANGE`] before any transfer.
pub fn check_address(address: u32) -> Result<(), UsbError> {
    if LOAD_ADDRESS_RANGE.contains(&address) {
        Ok(())
    } else {
        Err(UsbError::Other(format!(
            "load address {address:#010x} is outside {:#010x}..={:#010x}",
            LOAD_ADDRESS_RANGE.start(),
            LOAD_ADDRESS_RANGE.end()
        )))
    }
}

/// `burner_brom.cpp:73-74`: wValue the high 16 bits, wIndex the low 16.
fn split(address: u32) -> (u16, u16) {
    ((address >> 16) as u16, (address & 0xffff) as u16)
}

/// The IDs the boot ROM's download mode presents (`kburn.h:205`, `kburn.cpp:403-405`).
pub const DOWNLOAD_VID: u16 = 0x29f1;
pub const DOWNLOAD_PID: u16 = 0x0230;
/// How long the start waits for the image to re-enumerate, in milliseconds. The watchdog's reset of
/// a stage that does not come back takes at most a few seconds at the fastest clock, and the wait
/// covers the boot ROM's own return (`sim::BOOT_US`).
pub const START_WAIT_MS: u32 = 10_000;

/// Loads `image` at `address`, starts it, and settles the outcome by observation.
///
/// The start request's own result is not trusted either way. A disconnect, a normal return and a
/// stall all end the same way: the device is waited for, for at most [`START_WAIT_MS`], and it must
/// come back with the download IDs and present the stage's identity. Success is that. Every other
/// outcome is an error that names its case: no device before the deadline (a timeout), a device with
/// other IDs, the boot ROM answering again (the image did not run), the vendor burner, or an identity
/// text that is not the stage's. A device that leaves the bus again after it re-enumerates is also an
/// error.
///
/// The call blocks for the wait. The image is the caller's; starting is the same request for any image.
pub fn load_and_start(io: &mut dyn UsbIo, image: &[u8], address: u32) -> Result<(), UsbError> {
    check_address(address)?;
    if image.is_empty() {
        return Err(UsbError::Other("there is no image to load".into()));
    }
    let (high, low) = split(address);
    io.control_out(SET_DATA_ADDRESS, high, low, &[])?;
    // burner_brom.cpp:119-156: whole pages, the last one short.
    for page in image.chunks(PAGE) {
        io.bulk_out(page)?;
    }
    // The start's own result is not used: the observation below decides (see the doc above).
    let _start = io.control_out(PROG_START, high, low, &[]);
    settle(io)
}

/// Waits for the device to re-enumerate after a start, and checks it is the stage.
fn settle(io: &mut dyn UsbIo) -> Result<(), UsbError> {
    let (vid, pid) = io.wait_for_device(START_WAIT_MS)?;
    if (vid, pid) != (DOWNLOAD_VID, DOWNLOAD_PID) {
        return Err(UsbError::Protocol(format!(
            "after the start the device is {vid:04x}:{pid:04x}, not {DOWNLOAD_VID:04x}:{DOWNLOAD_PID:04x}"
        )));
    }
    match stage(io) {
        Ok(Stage::Lemnos) => Ok(()),
        Ok(Stage::BootRom) => Err(UsbError::Protocol(
            "the boot ROM answered again: the image did not run".into(),
        )),
        Ok(Stage::Burner) => Err(UsbError::Protocol(
            "the vendor burner answered: the image is not the stage".into(),
        )),
        Ok(Stage::Unknown(text)) => Err(UsbError::Protocol(format!(
            "after the start the device answered {text:?}, not the stage's identity"
        ))),
        Err(UsbError::Disconnected) => Err(UsbError::Protocol(
            "the device left the bus again after it re-enumerated".into(),
        )),
        Err(error) => Err(error),
    }
}
