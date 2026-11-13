//! A USB device that runs the DFU 1.1 state machine, for the tests. It enforces the protocol: a
//! request in the wrong state stalls and puts the device in `dfuERROR`, as a real one would. It
//! also records every transfer it sees and when.

use core::time::Duration;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use lemnos_core::ErrorKind;
use lemnos_hal::UsbDevice;
use lemnos_hal::blocks::{ControlSetup, IsoPacket};

use crate::dfu;

/// `bStatus` values the mock uses.
pub const ERR_WRITE: u8 = 0x03;
pub const ERR_VERIFY: u8 = 0x07;
pub const ERR_ADDRESS: u8 = 0x08;
pub const ERR_STALLED: u8 = 0x0f;

/// One control transfer, as the device saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transfer {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub data: Vec<u8>,
}

impl Transfer {
    pub fn dnload(block: u16, data: &[u8]) -> Self {
        Self::out(dfu::DNLOAD, block, data)
    }

    pub fn out(request: u8, value: u16, data: &[u8]) -> Self {
        Self {
            request_type: dfu::REQUEST_OUT,
            request,
            value,
            index: 0,
            data: data.to_vec(),
        }
    }

    pub fn get_status() -> Self {
        Self {
            request_type: dfu::REQUEST_IN,
            request: dfu::GETSTATUS,
            value: 0,
            index: 0,
            data: Vec::new(),
        }
    }
}

/// What the device does, and what has happened to it. Tests reach in through [`MockUsb::state`].
pub struct DeviceState {
    pub descriptors: Vec<u8>,
    pub transfer_size: usize,
    pub tolerant: bool,
    pub state: u8,
    pub status: u8,
    /// The simulated time, set by the test before each tick.
    pub now_ns: u64,
    /// Reported in `bwPollTimeout` on each busy reply.
    pub poll_ms: u32,
    /// Busy replies for each block.
    pub busy_polls: u32,
    /// The busy replies left for this block.
    busy_left: u32,
    /// Busy replies while manifesting.
    pub manifest_polls: u32,
    manifest_left: u32,
    /// No GETSTATUS should arrive before this.
    next_poll_ns: u64,
    /// Time of every GETSTATUS that arrived too soon.
    pub early_polls: Vec<u64>,
    expected_block: u16,

    /// Every control transfer.
    pub transfers: Vec<Transfer>,
    /// The image as the device received it.
    pub received: Vec<u8>,
    pub claimed: Vec<u8>,
    pub alternates: Vec<(u8, u8)>,
    pub descriptor_reads: u32,

    /// Fail this block with `ERR_WRITE`, once.
    pub reject_block_once: Option<u16>,
    /// Fail manifestation with this status, once.
    pub reject_manifest_once: Option<u8>,
    /// Never leave the busy state.
    pub busy_forever: bool,
    /// The next GETSTATUS replies with this many bytes, once.
    pub short_status_once: Option<usize>,
    /// The next DNLOAD with data reports one byte fewer, once.
    pub short_dnload_once: bool,
    /// Every transfer after this many fails with this error.
    pub silent_after: Option<(usize, ErrorKind)>,
    /// GETSTATUS fails with this error while manifesting.
    pub manifest_status_error: Option<ErrorKind>,
    /// CLRSTATUS and ABORT do nothing, so the device stays where it is.
    pub ignore_recovery: bool,
    /// The next `set_alternate` fails with this, once.
    pub alternate_error_once: Option<ErrorKind>,
    /// The next claim fails with this, once.
    pub claim_error_once: Option<ErrorKind>,

    /// Every reset asked for: how many transfers had been seen, and the DFU state, at that
    /// moment. One that failed is here too.
    pub resets: Vec<(usize, u8)>,
    /// The next resets fail with these, in order, and are not carried out.
    pub reset_errors: VecDeque<ErrorKind>,
    /// A reset leaves the device as it is, where it would otherwise come back as another one.
    pub reset_keeps_identity: bool,
    /// The device has left the bus under this identity: every call fails with `Unavailable`.
    pub gone: bool,
    /// Every call made after the device left, by name.
    pub calls_after_gone: Vec<&'static str>,
}

/// The device, cloned between the driver and the test.
#[derive(Clone)]
pub struct MockUsb(Arc<Mutex<DeviceState>>);

impl MockUsb {
    /// A device with these descriptors and `wTransferSize`, in `dfuIDLE`.
    pub fn new(descriptors: Vec<u8>, transfer_size: usize, tolerant: bool) -> Self {
        Self(Arc::new(Mutex::new(DeviceState {
            descriptors,
            transfer_size,
            tolerant,
            state: dfu::IDLE,
            status: dfu::STATUS_OK,
            now_ns: 0,
            poll_ms: 0,
            busy_polls: 0,
            busy_left: 0,
            manifest_polls: 0,
            manifest_left: 0,
            next_poll_ns: 0,
            early_polls: Vec::new(),
            expected_block: 0,
            transfers: Vec::new(),
            received: Vec::new(),
            claimed: Vec::new(),
            alternates: Vec::new(),
            descriptor_reads: 0,
            reject_block_once: None,
            reject_manifest_once: None,
            busy_forever: false,
            short_status_once: None,
            short_dnload_once: false,
            silent_after: None,
            manifest_status_error: None,
            ignore_recovery: false,
            alternate_error_once: None,
            claim_error_once: None,
            resets: Vec::new(),
            reset_errors: VecDeque::new(),
            reset_keeps_identity: false,
            gone: false,
            calls_after_gone: Vec::new(),
        })))
    }

    pub fn state(&self) -> MutexGuard<'_, DeviceState> {
        self.0.lock().unwrap()
    }
}

impl DeviceState {
    /// Fails the call, and records it, if the device has left the bus.
    fn check_present(&mut self, call: &'static str) -> Result<(), ErrorKind> {
        if self.gone {
            self.calls_after_gone.push(call);
            return Err(ErrorKind::Unavailable);
        }
        Ok(())
    }

    /// A port reset: the device starts what it was given and comes back as another device,
    /// unless the test says it keeps its identity.
    fn reset(&mut self) -> Result<(), ErrorKind> {
        self.resets.push((self.transfers.len(), self.state));
        self.check_present("reset")?;
        if let Some(kind) = self.reset_errors.pop_front() {
            return Err(kind);
        }
        if self.reset_keeps_identity {
            return Ok(());
        }
        self.gone = true;
        Err(ErrorKind::Unavailable)
    }

    /// Fails the transfer if the device has gone silent.
    fn check_silent(&self) -> Result<(), ErrorKind> {
        match self.silent_after {
            Some((after, kind)) if self.transfers.len() > after => Err(kind),
            _ => Ok(()),
        }
    }

    /// A request the current state does not allow: the device stalls and reports an error.
    fn stall(&mut self, status: u8) -> ErrorKind {
        self.state = dfu::ERROR;
        self.status = status;
        ErrorKind::Failed
    }

    fn back_to_idle(&mut self) {
        self.state = dfu::IDLE;
        self.status = dfu::STATUS_OK;
        self.expected_block = 0;
        self.received.clear();
    }

    fn dnload(&mut self, block: u16, data: &[u8]) -> Result<usize, ErrorKind> {
        if !matches!(self.state, dfu::IDLE | dfu::DNLOAD_IDLE) {
            return Err(self.stall(ERR_STALLED));
        }
        if data.is_empty() {
            // Ending a download that never started is not allowed.
            if self.state == dfu::IDLE {
                return Err(self.stall(ERR_STALLED));
            }
            self.state = dfu::MANIFEST_SYNC;
            self.manifest_left = self.manifest_polls;
            return Ok(0);
        }
        if block != self.expected_block {
            return Err(self.stall(ERR_ADDRESS));
        }
        if data.len() > self.transfer_size {
            return Err(self.stall(ERR_STALLED));
        }
        self.expected_block = self.expected_block.wrapping_add(1);
        self.received.extend_from_slice(data);
        self.state = dfu::DNLOAD_SYNC;
        self.busy_left = self.busy_polls;
        if self.reject_block_once == Some(block) {
            self.reject_block_once = None;
            self.state = dfu::ERROR;
            self.status = ERR_WRITE;
        }
        if self.short_dnload_once {
            self.short_dnload_once = false;
            return Ok(data.len() - 1);
        }
        Ok(data.len())
    }

    fn get_status(&mut self) -> Result<[u8; dfu::STATUS_LEN], ErrorKind> {
        if self.now_ns < self.next_poll_ns {
            self.early_polls.push(self.now_ns);
        }
        let mut poll_ms = 0;
        let reported = match self.state {
            dfu::DNLOAD_SYNC | dfu::DNBUSY => {
                if self.busy_forever || self.busy_left > 0 {
                    self.busy_left = self.busy_left.saturating_sub(1);
                    self.state = dfu::DNBUSY;
                    poll_ms = self.poll_ms;
                    dfu::DNBUSY
                } else {
                    self.state = dfu::DNLOAD_IDLE;
                    dfu::DNLOAD_IDLE
                }
            }
            dfu::MANIFEST_SYNC | dfu::MANIFEST => {
                if let Some(kind) = self.manifest_status_error {
                    self.state = dfu::MANIFEST;
                    return Err(kind);
                }
                if self.manifest_left > 0 {
                    self.manifest_left -= 1;
                    self.state = dfu::MANIFEST;
                    poll_ms = self.poll_ms;
                    dfu::MANIFEST
                } else if let Some(status) = self.reject_manifest_once.take() {
                    self.state = dfu::ERROR;
                    self.status = status;
                    dfu::ERROR
                } else if self.tolerant {
                    self.state = dfu::IDLE;
                    self.status = dfu::STATUS_OK;
                    dfu::IDLE
                } else {
                    self.state = dfu::MANIFEST_WAIT_RESET;
                    dfu::MANIFEST_WAIT_RESET
                }
            }
            other => other,
        };
        self.next_poll_ns = self.now_ns + u64::from(poll_ms.min(8_000)) * 1_000_000;
        let [low, middle, high, _] = poll_ms.to_le_bytes();
        Ok([self.status, low, middle, high, reported, 0])
    }

    fn clear_status(&mut self) -> Result<usize, ErrorKind> {
        if self.ignore_recovery {
            return Ok(0);
        }
        if self.state != dfu::ERROR {
            return Err(self.stall(ERR_STALLED));
        }
        self.back_to_idle();
        Ok(0)
    }

    fn abort(&mut self) -> Result<usize, ErrorKind> {
        if self.ignore_recovery {
            return Ok(0);
        }
        if !matches!(
            self.state,
            dfu::IDLE | dfu::DNLOAD_IDLE | dfu::UPLOAD_IDLE | dfu::MANIFEST_SYNC
        ) {
            return Err(self.stall(ERR_STALLED));
        }
        self.back_to_idle();
        Ok(0)
    }
}

impl UsbDevice for MockUsb {
    type Error = ErrorKind;

    fn config_descriptor(&mut self, index: u8, out: &mut [u8]) -> Result<usize, ErrorKind> {
        assert_eq!(index, 0, "a DFU device has one configuration");
        let mut device = self.state();
        device.check_present("config_descriptor")?;
        device.descriptor_reads += 1;
        let length = device.descriptors.len().min(out.len());
        out[..length].copy_from_slice(&device.descriptors[..length]);
        Ok(length)
    }

    fn reset(&mut self) -> Result<(), ErrorKind> {
        self.state().reset()
    }

    fn detach_kernel_driver(&mut self, _interface: u8) -> Result<(), ErrorKind> {
        panic!("a DFU interface has no kernel driver for this driver to detach");
    }

    fn claim_interface(&mut self, interface: u8) -> Result<(), ErrorKind> {
        let mut device = self.state();
        device.check_present("claim_interface")?;
        if let Some(kind) = device.claim_error_once.take() {
            return Err(kind);
        }
        device.claimed.push(interface);
        Ok(())
    }

    fn release_interface(&mut self, _interface: u8) -> Result<(), ErrorKind> {
        Ok(())
    }

    fn set_alternate(&mut self, interface: u8, alternate: u8) -> Result<(), ErrorKind> {
        let mut device = self.state();
        device.check_present("set_alternate")?;
        if let Some(kind) = device.alternate_error_once.take() {
            return Err(kind);
        }
        device.alternates.push((interface, alternate));
        Ok(())
    }

    fn control_in(
        &mut self,
        setup: ControlSetup,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        assert!(timeout < Duration::from_secs(2), "under the call deadline");
        let mut device = self.state();
        device.check_present("control_in")?;
        device.check_silent()?;
        device.transfers.push(Transfer {
            request_type: setup.request_type,
            request: setup.request,
            value: setup.value,
            index: setup.index,
            data: Vec::new(),
        });
        if setup.request != dfu::GETSTATUS || setup.request_type != dfu::REQUEST_IN {
            return Err(device.stall(ERR_STALLED));
        }
        let reply = device.get_status()?;
        let length = device
            .short_status_once
            .take()
            .unwrap_or(dfu::STATUS_LEN)
            .min(data.len());
        data[..length].copy_from_slice(&reply[..length]);
        Ok(length)
    }

    fn control_out(
        &mut self,
        setup: ControlSetup,
        data: &[u8],
        timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        assert!(timeout < Duration::from_secs(2), "under the call deadline");
        let mut device = self.state();
        device.check_present("control_out")?;
        device.check_silent()?;
        device.transfers.push(Transfer {
            request_type: setup.request_type,
            request: setup.request,
            value: setup.value,
            index: setup.index,
            data: data.to_vec(),
        });
        if setup.request_type != dfu::REQUEST_OUT {
            return Err(device.stall(ERR_STALLED));
        }
        match setup.request {
            dfu::DNLOAD => device.dnload(setup.value, data),
            dfu::CLRSTATUS => device.clear_status(),
            dfu::ABORT => device.abort(),
            _ => Err(device.stall(ERR_STALLED)),
        }
    }

    fn bulk_in(&mut self, _: u8, _: &mut [u8], _: Duration) -> Result<usize, ErrorKind> {
        Err(ErrorKind::Unsupported)
    }

    fn bulk_out(&mut self, _: u8, _: &[u8], _: Duration) -> Result<usize, ErrorKind> {
        Err(ErrorKind::Unsupported)
    }

    fn interrupt_in(&mut self, _: u8, _: &mut [u8], _: Duration) -> Result<usize, ErrorKind> {
        Err(ErrorKind::Unsupported)
    }

    fn iso_in_start(&mut self, _: u8, _: u8, _: u16, _: u32) -> Result<(), ErrorKind> {
        Err(ErrorKind::Unsupported)
    }

    fn iso_in_next(
        &mut self,
        _: &mut [u8],
        _: &mut [IsoPacket],
        _: Duration,
    ) -> Result<(), ErrorKind> {
        Err(ErrorKind::Unsupported)
    }

    fn iso_in_stop(&mut self) -> Result<(), ErrorKind> {
        Ok(())
    }
}
