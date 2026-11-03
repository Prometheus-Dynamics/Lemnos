//! USB DFU 1.1 firmware download, and waiting for the device to come back.
//!
//! A boot ROM that takes its firmware over DFU needs, in order: one DNLOAD
//! request per block (the block number in `wValue`, the data after it), a
//! GETSTATUS after each block until the device is idle again, a zero-length
//! DNLOAD that ends the download, a GETSTATUS that moves it to manifestation,
//! and then a bus reset. The device re-enumerates with its new identity, which
//! [`wait_for_device`] waits for. Nothing here names a product: the caller
//! brings the image and the IDs.

use crate::device::UsbDevice;
use crate::error::{Error, ErrorKind, Result};
use crate::info::UsbDeviceInfo;
use crate::listing::list_devices;
use crate::types::{ControlKind, ControlRecipient, ControlSetup, Direction};
use std::time::{Duration, Instant};

/// `bRequest` values of the DFU class (DFU 1.1, table 3.2).
const DFU_DNLOAD: u8 = 0x01;
const DFU_GETSTATUS: u8 = 0x03;

/// `bState` values (DFU 1.1, table 4.1).
const STATE_DNLOAD_IDLE: u8 = 5;
const STATE_MANIFEST_SYNC: u8 = 6;
const STATE_MANIFEST: u8 = 7;
const STATE_MANIFEST_WAIT_RESET: u8 = 8;

/// The GETSTATUS polls allowed for one block before the download gives up.
const MAX_POLLS: u32 = 1000;

/// The control requests a DFU download makes. [`UsbDevice`] implements it; a
/// test implements it with a simulated device.
pub(crate) trait DfuControl {
    /// A class request to `interface` with data to the device (DNLOAD).
    fn dfu_out(&self, interface: u8, request: u8, value: u16, data: &[u8]) -> Result<()>;
    /// GETSTATUS: the status, the poll timeout and the state.
    fn status(&self, interface: u8) -> Result<DfuStatus>;
}

/// A DFU GETSTATUS reply (`bStatus`, `bwPollTimeout`, `bState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DfuStatus {
    /// `bStatus`: zero when the last request succeeded.
    pub status: u8,
    /// `bwPollTimeout`: how long the host waits before the next GETSTATUS.
    pub poll_timeout: Duration,
    /// `bState`: the DFU state the device is in.
    pub state: u8,
}

impl DfuStatus {
    /// Parses the six-byte GETSTATUS reply.
    fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 6 {
            return Err(Error::new(
                ErrorKind::Other,
                format!("DFU status is {} bytes, not 6", bytes.len()),
            ));
        }
        let millis = u64::from(bytes[1]) | (u64::from(bytes[2]) << 8) | (u64::from(bytes[3]) << 16);
        Ok(Self {
            status: bytes[0],
            poll_timeout: Duration::from_millis(millis),
            state: bytes[4],
        })
    }
}

fn dfu_setup(direction: Direction, request: u8, value: u16, interface: u8) -> ControlSetup {
    ControlSetup {
        direction,
        kind: ControlKind::Class,
        recipient: ControlRecipient::Interface,
        request,
        value,
        index: u16::from(interface),
    }
}

impl DfuControl for UsbDevice {
    fn dfu_out(&self, interface: u8, request: u8, value: u16, data: &[u8]) -> Result<()> {
        let setup = dfu_setup(Direction::Out, request, value, interface);
        self.control_out(&setup, data, Duration::from_secs(5))
            .map(|_| ())
    }

    fn status(&self, interface: u8) -> Result<DfuStatus> {
        self.dfu_status(interface)
    }
}

/// Polls GETSTATUS until the device leaves its busy states, honouring each
/// poll timeout. `done` says which state ends the wait.
fn poll_until(
    control: &impl DfuControl,
    interface: u8,
    done: impl Fn(u8) -> bool,
    sleep: &mut impl FnMut(Duration),
) -> Result<DfuStatus> {
    for _ in 0..MAX_POLLS {
        let status = control.status(interface)?;
        if status.status != 0 {
            return Err(Error::new(
                ErrorKind::Other,
                format!(
                    "DFU error {} in state {} (the device refused the block)",
                    status.status, status.state
                ),
            ));
        }
        if done(status.state) {
            return Ok(status);
        }
        sleep(status.poll_timeout);
    }
    Err(Error::new(
        ErrorKind::Timeout,
        "the DFU device stayed busy through every status poll",
    ))
}

/// The DFU download sequence over `control`: each block of `block_size` bytes
/// (DNLOAD, then GETSTATUS until idle), a zero-length DNLOAD, and one
/// GETSTATUS to start the manifestation. The caller resets the device.
pub(crate) fn download(
    control: &impl DfuControl,
    interface: u8,
    image: &[u8],
    block_size: usize,
    sleep: &mut impl FnMut(Duration),
) -> Result<()> {
    if block_size == 0 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "the DFU block size is zero",
        ));
    }
    let mut blocks = 0u16;
    for block in image.chunks(block_size) {
        control.dfu_out(interface, DFU_DNLOAD, blocks, block)?;
        poll_until(
            control,
            interface,
            |state| state == STATE_DNLOAD_IDLE,
            sleep,
        )?;
        blocks = blocks.wrapping_add(1);
    }
    // The zero-length DNLOAD ends the download; the device then manifests.
    control.dfu_out(interface, DFU_DNLOAD, blocks, &[])?;
    let manifest = poll_until(
        control,
        interface,
        |state| {
            matches!(
                state,
                STATE_MANIFEST_SYNC | STATE_MANIFEST | STATE_MANIFEST_WAIT_RESET
            )
        },
        sleep,
    );
    // Some devices reset themselves while manifesting, so the status poll can
    // fail with the device gone. That is the end the download wants.
    match manifest {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == ErrorKind::Disconnected => Ok(()),
        Err(error) => Err(error),
    }
}

impl UsbDevice {
    /// GETSTATUS on DFU interface `interface`: a read, no state change. The
    /// interface must be claimed.
    pub fn dfu_status(&self, interface: u8) -> Result<DfuStatus> {
        let setup = dfu_setup(Direction::In, DFU_GETSTATUS, 0, interface);
        let bytes = self.control_in(&setup, 6, Duration::from_secs(5))?;
        DfuStatus::from_bytes(&bytes)
    }

    /// Downloads `image` to the DFU interface `interface` in blocks of
    /// `block_size` bytes, then resets the device so it re-enumerates. The
    /// interface must be claimed (DFU requests address it). The device's claims
    /// are gone after the reset: drop the handle, then [`wait_for_device`] for
    /// its new identity. A reset that fails because the device already
    /// disconnected is not an error.
    pub fn dfu_download(&self, interface: u8, image: &[u8], block_size: usize) -> Result<()> {
        download(self, interface, image, block_size, &mut std::thread::sleep)?;
        match self.reset() {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::Disconnected => Ok(()),
            Err(error) => Err(error),
        }
    }
}

/// Waits until a device with `vendor_id:product_id` is in the listing, polling
/// every 100 ms, up to `timeout`. Returns it, or `None` at the timeout. This is
/// the re-enumeration after a firmware download: the device's identity changes,
/// its location does not.
pub fn wait_for_device(
    vendor_id: u16,
    product_id: u16,
    timeout: Duration,
) -> Result<Option<UsbDeviceInfo>> {
    let deadline = Instant::now() + timeout;
    loop {
        let found = list_devices()?
            .into_iter()
            .find(|info| info.vendor_id() == vendor_id && info.product_id() == product_id);
        if found.is_some() {
            return Ok(found);
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    const STATE_DNBUSY: u8 = 4;

    /// A DFU device that accepts every block, reports BUSY once after each
    /// block, and manifests after the zero-length DNLOAD.
    struct Simulated {
        blocks: RefCell<Vec<(u16, Vec<u8>)>>,
        busy_next: Cell<bool>,
        manifesting: Cell<bool>,
        /// The status to report for every GETSTATUS, when set.
        refuse_with: Option<u8>,
        polls: Cell<u32>,
    }

    impl Simulated {
        fn new() -> Self {
            Self {
                blocks: RefCell::new(Vec::new()),
                busy_next: Cell::new(false),
                manifesting: Cell::new(false),
                refuse_with: None,
                polls: Cell::new(0),
            }
        }
    }

    impl DfuControl for Simulated {
        fn dfu_out(&self, interface: u8, request: u8, value: u16, data: &[u8]) -> Result<()> {
            assert_eq!(interface, 0, "DFU requests address the interface");
            assert_eq!(request, DFU_DNLOAD);
            if data.is_empty() {
                self.manifesting.set(true);
            } else {
                self.blocks.borrow_mut().push((value, data.to_vec()));
                self.busy_next.set(true);
            }
            Ok(())
        }

        fn status(&self, interface: u8) -> Result<DfuStatus> {
            assert_eq!(interface, 0);
            self.polls.set(self.polls.get() + 1);
            if let Some(status) = self.refuse_with {
                return Ok(DfuStatus {
                    status,
                    poll_timeout: Duration::ZERO,
                    state: 10,
                });
            }
            if self.manifesting.get() {
                return Ok(DfuStatus {
                    status: 0,
                    poll_timeout: Duration::ZERO,
                    state: STATE_MANIFEST,
                });
            }
            if self.busy_next.get() {
                self.busy_next.set(false);
                return Ok(DfuStatus {
                    status: 0,
                    poll_timeout: Duration::from_millis(5),
                    state: STATE_DNBUSY,
                });
            }
            Ok(DfuStatus {
                status: 0,
                poll_timeout: Duration::ZERO,
                state: STATE_DNLOAD_IDLE,
            })
        }
    }

    #[test]
    fn the_image_goes_out_in_numbered_blocks_then_a_zero_length_download() {
        let device = Simulated::new();
        let image: Vec<u8> = (0..10u8).collect();
        let mut slept = Vec::new();
        download(&device, 0, &image, 4, &mut |d| slept.push(d)).expect("download");
        let blocks = device.blocks.borrow();
        assert_eq!(blocks.len(), 3, "10 bytes in blocks of 4");
        assert_eq!(blocks[0], (0, vec![0, 1, 2, 3]));
        assert_eq!(blocks[1], (1, vec![4, 5, 6, 7]));
        assert_eq!(blocks[2], (2, vec![8, 9]), "the last block is short");
        assert!(device.manifesting.get(), "the zero-length DNLOAD was sent");
        assert_eq!(
            slept,
            vec![Duration::from_millis(5); 3],
            "each block waited out its BUSY poll"
        );
    }

    #[test]
    fn a_device_that_refuses_a_block_fails_the_download() {
        let mut device = Simulated::new();
        device.refuse_with = Some(3);
        let error = download(&device, 0, &[1, 2, 3], 2, &mut |_| {}).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Other);
        assert!(error.message().contains("DFU error 3"));
    }

    #[test]
    fn an_empty_image_sends_only_the_zero_length_download() {
        let device = Simulated::new();
        download(&device, 0, &[], 64, &mut |_| {}).expect("download");
        assert!(device.blocks.borrow().is_empty());
        assert!(device.manifesting.get());
    }

    #[test]
    fn a_zero_block_size_is_refused_before_any_request() {
        let device = Simulated::new();
        let error = download(&device, 0, &[1], 0, &mut |_| {}).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert!(device.blocks.borrow().is_empty());
    }

    #[test]
    fn the_status_reply_carries_its_poll_timeout_in_milliseconds() {
        let status = DfuStatus::from_bytes(&[0, 0x34, 0x12, 0, 5, 0]).expect("status");
        assert_eq!(status.poll_timeout, Duration::from_millis(0x1234));
        assert_eq!(status.state, STATE_DNLOAD_IDLE);
        assert!(DfuStatus::from_bytes(&[0, 0, 0]).is_err());
    }
}
