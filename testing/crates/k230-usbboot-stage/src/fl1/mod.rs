//! The K230 stage on the FL1 flash wire, built the way the Rockchip maskrom stage is: the shared
//! rules (`lemnos-flash-stage`) run the session, and this module supplies the K230's four pieces:
//!
//! - [`guard`]: the two watchdogs as the shared entry contract's `Guard` (arm, read back, lower
//!   the period, feed);
//! - [`usb`]: the device controller as the shared `Controller`, over the stage's `UsbHw` (the
//!   dwc2 device block in the `-sys` crate);
//! - [`storage`]: the SD card as the shared `Storage`, with the budget checked per sector;
//! - [`report`]: the cause, printed on the UART before the SoC reset.
//!
//! [`run`] holds the order: arm first, then bring the controllers up, then the session. A chip
//! that does not arm stops before any USB or SD call.
//!
//! The regions and the identity record are the K230's: see [`REGIONS`]. The build id is
//! [`BUILD_ID`]. Both are checked by the tests and stated in `testing/board-runs/K230-USB.md`.

pub mod guard;
pub mod report;
pub mod run;
pub mod storage;
pub mod usb;

use lemnos_flash_wire::identity::{IDENTITY_LEN, identity_record};
use lemnos_flash_wire::region::{Class, Region};

/// The stage's build id, the identity record's. Stated in the board plan.
pub const BUILD_ID: u64 = 0x4b32_3330_0000_0001;

/// The first sector of the boot partition (30 MiB). Everything below it is the boot area: the SPL
/// copies, the stored environment and U-Boot. The header blocks are the two places the ROM reads
/// its first stage from (sectors 2048 and 3072, each an eight-sector header block).
pub const BOOT_AREA_END: u32 = 61_440;

/// The regions the stage polices. The strictest class of any region a write overlaps wins, so the
/// header blocks are inside the boot area and the header class.
pub const REGIONS: [Region; 3] = [
    Region {
        first: 0,
        end: BOOT_AREA_END,
        class: Class::BootArea,
    },
    Region {
        first: 2048,
        end: 2056,
        class: Class::Header,
    },
    Region {
        first: 3072,
        end: 3080,
        class: Class::Header,
    },
];

/// The identity record a host reads first.
pub const IDENTITY: [u8; IDENTITY_LEN] = identity_record(BUILD_ID, &REGIONS);
