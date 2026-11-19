//! The K230's device controller as the shared `Controller`. The shared control code and the bulk
//! pair's framing are in `lemnos-flash-stage`; this adapter maps each controller command onto the
//! stage's [`UsbHw`] (the dwc2 device block in the `-sys` crate) and keeps the events the shared
//! code asks for in the order they come.
//!
//! Waits: every method that waits takes the phase's deadline and returns `UsbError::Timeout` when
//! the clock passes it. A `UsbHw` call that cannot finish is the hardware's own bounded wait.
//!
//! Bench checks, not proven in simulation: the speed (read from the controller when it can say, see
//! [`UsbHw::enum_speed`]; dwc2 reads `DSTS.ENUMSPD`), the order of SET_ADDRESS against its status stage
//! (`address_before_status`), and that the dwc2 device path arms the bulk OUT after each frame, as
//! `UsbHw` implies. The K230 B2 check build records each of them (`fl1::check::record`).
//!
//! A bulk OUT transfer longer than the frame the shared code armed for is reported with its whole
//! length, so the shared serve loop refuses it by length (status 5) and the session goes on; it is
//! never cut down to a frame of the expected length.

use crate::usb_dev::{self as hw, BULK_IN, BULK_OUT, EP0, Event as HwEvent, HwError, UsbHw};
use lemnos_flash_stage::usb::{Controller, Event, Speed, UsbError};
use lemnos_flash_stage::wait::Clock;

/// The largest bulk frame the default device buffer takes ([`UsbHw::rx_capacity`] is the controller's
/// own). An arm for more is refused before the hardware is asked.
pub const BULK_MAX: usize = hw::RX_MAX;

/// The controller over the device hardware, with the clock its waits are read on.
pub struct Device<H: UsbHw, K: Clock> {
    hw: H,
    clock: K,
    /// A completed bulk OUT frame's length, until the shared code takes it.
    bulk_ready: Option<usize>,
    /// An endpoint 0 OUT (the host's status stage) seen while nothing waited for it.
    ep0_out: bool,
    /// One event the status wait took that belongs to the next poll.
    stash: Option<Event>,
}

impl<H: UsbHw, K: Clock> Device<H, K> {
    pub fn new(hw: H, clock: K) -> Self {
        Self {
            hw,
            clock,
            bulk_ready: None,
            ep0_out: false,
            stash: None,
        }
    }

    fn passed(&mut self, deadline_us: u64) -> bool {
        self.clock.now_us() > deadline_us
    }

    /// Maps one hardware event to the shared event. Bulk and endpoint 0 OUT completions are kept
    /// here and reported to the caller that waits for them.
    fn map(&mut self, event: HwEvent) -> Event {
        match event {
            HwEvent::Idle => Event::None,
            HwEvent::BusReset => {
                // A frame from before the reset is gone with it.
                self.bulk_ready = None;
                Event::BusReset
            }
            HwEvent::Setup(setup) => Event::Setup(setup),
            HwEvent::Out { ep: BULK_OUT, len } => {
                self.bulk_ready = Some(len);
                Event::None
            }
            HwEvent::Out { ep: EP0, .. } => {
                self.ep0_out = true;
                Event::None
            }
            HwEvent::Out { .. } => Event::None,
        }
    }
}

fn map_error(error: HwError) -> UsbError {
    match error {
        HwError::Stalled | HwError::Overrun => UsbError::Failed,
        HwError::Disconnected => UsbError::Reset,
        HwError::Timeout => UsbError::Timeout,
    }
}

impl<H: UsbHw, K: Clock> Controller for Device<H, K> {
    fn poll(&mut self, _deadline_us: u64) -> Event {
        if let Some(event) = self.stash.take() {
            return event;
        }
        let event = self.hw.poll();
        self.map(event)
    }

    fn set_address(&mut self, address: u8, deadline_us: u64) -> Result<(), UsbError> {
        if self.passed(deadline_us) {
            return Err(UsbError::Timeout);
        }
        self.hw.set_address(address);
        Ok(())
    }

    fn set_configured(&mut self, on: bool, deadline_us: u64) -> Result<(), UsbError> {
        if self.passed(deadline_us) {
            return Err(UsbError::Timeout);
        }
        // The bulk pair is activated here, not at core reset: dwc2 deactivates its non-control
        // endpoints on every bus reset, and a host resets the bus before it configures.
        self.hw.configure(on);
        if !on {
            self.bulk_ready = None;
        }
        Ok(())
    }

    fn ep0_in(&mut self, data: &[u8], deadline_us: u64) -> Result<(), UsbError> {
        if self.passed(deadline_us) {
            return Err(UsbError::Timeout);
        }
        self.hw.write_in(EP0, data).map_err(map_error)
    }

    fn ep0_in_status(&mut self, deadline_us: u64) -> Result<(), UsbError> {
        self.ep0_in(&[], deadline_us)
    }

    fn ep0_out_status(&mut self, deadline_us: u64) -> Result<(), UsbError> {
        if self.ep0_out {
            self.ep0_out = false;
            return Ok(());
        }
        loop {
            if self.passed(deadline_us) {
                return Err(UsbError::Timeout);
            }
            match self.hw.poll() {
                HwEvent::Idle => {}
                HwEvent::Out { ep: EP0, .. } => return Ok(()),
                other => {
                    // A SETUP or a bus reset ends the status stage: the request is gone. The event
                    // is kept for the next poll, and the shared code sees the request abort.
                    let event = self.map(other);
                    if event != Event::None {
                        self.stash = Some(event);
                        return Err(UsbError::Reset);
                    }
                }
            }
        }
    }

    fn ep0_stall(&mut self, _deadline_us: u64) -> Result<(), UsbError> {
        self.hw.stall(EP0);
        Ok(())
    }

    fn speed(&mut self) -> Speed {
        match self.hw.enum_speed() {
            // A controller that cannot say is taken at high speed, as the device descriptor states.
            None | Some(0) => Speed::High,
            Some(1 | 3) => Speed::Full,
            Some(2) => Speed::Low,
            Some(_) => Speed::Unknown,
        }
    }

    fn address_before_status(&self) -> bool {
        true
    }

    fn bulk_arm(&mut self, len: usize, deadline_us: u64) -> Result<(), UsbError> {
        if len > self.hw.rx_capacity() {
            return Err(UsbError::Failed);
        }
        if self.passed(deadline_us) {
            return Err(UsbError::Timeout);
        }
        // A frame that completed before this arm is kept: the hardware holds it (its endpoint is not
        // armed again until the frame is taken), so dropping it here would lose it for good.
        Ok(())
    }

    fn bulk_poll(&mut self, out: &mut [u8]) -> Result<Option<usize>, UsbError> {
        let Some(len) = self.bulk_ready.take() else {
            return Ok(None);
        };
        // A transfer longer than the frame is taken whole by the hardware and reported with its own
        // length: the caller refuses it by length. What fits is copied; the rest is dropped.
        let take = len.min(out.len());
        let n = self
            .hw
            .read_out_part(BULK_OUT, &mut out[..take])
            .map_err(map_error)?;
        // The transfer's length is the one the hardware reported for it, whatever was copied.
        Ok(Some(n.max(len)))
    }

    fn bulk_send(&mut self, data: &[u8], budget_us: u64) -> Result<(), UsbError> {
        let deadline = self.clock.now_us().saturating_add(budget_us);
        while !self.hw.in_ready(BULK_IN) {
            if self.passed(deadline) {
                return Err(UsbError::Timeout);
            }
        }
        self.hw.write_in(BULK_IN, data).map_err(map_error)
    }
}
