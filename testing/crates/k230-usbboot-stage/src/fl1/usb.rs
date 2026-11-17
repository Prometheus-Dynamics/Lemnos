//! The K230's device controller as the shared `Controller`. The shared control code and the bulk
//! pair's framing are in `lemnos-flash-stage`; this adapter maps each controller command onto the
//! stage's [`UsbHw`] (the dwc2 device block in the `-sys` crate) and keeps the events the shared
//! code asks for in the order they come.
//!
//! Waits: every method that waits takes the phase's deadline and returns `UsbError::Timeout` when
//! the clock passes it. A `UsbHw` call that cannot finish is the hardware's own bounded wait.
//!
//! Bench checks, not proven in simulation: the speed (the stage reports high speed; dwc2's enum
//! speed is not read), the order of SET_ADDRESS against its status stage (`address_before_status`),
//! and that the dwc2 device path arms the bulk OUT after each frame, as `UsbHw` implies.

use crate::usb_dev::{self as hw, BULK_IN, BULK_OUT, EP0, Event as HwEvent, HwError, UsbHw};
use lemnos_flash_stage::usb::{Controller, Event, Speed, UsbError};
use lemnos_flash_stage::wait::Clock;

/// The largest bulk frame the device takes. Larger is a failed transfer, not a truncated one.
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
            HwEvent::BusReset => Event::BusReset,
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

    fn set_configured(&mut self, _on: bool, deadline_us: u64) -> Result<(), UsbError> {
        // The endpoints are set up at core reset (bench check); configuring is only a deadline.
        if self.passed(deadline_us) {
            return Err(UsbError::Timeout);
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
        Speed::High
    }

    fn address_before_status(&self) -> bool {
        true
    }

    fn bulk_arm(&mut self, len: usize, deadline_us: u64) -> Result<(), UsbError> {
        if len > BULK_MAX {
            return Err(UsbError::Failed);
        }
        if self.passed(deadline_us) {
            return Err(UsbError::Timeout);
        }
        self.bulk_ready = None;
        Ok(())
    }

    fn bulk_poll(&mut self, out: &mut [u8]) -> Result<Option<usize>, UsbError> {
        let Some(len) = self.bulk_ready.take() else {
            return Ok(None);
        };
        if len > out.len() {
            return Err(UsbError::Failed);
        }
        let n = self
            .hw
            .read_out(BULK_OUT, &mut out[..len])
            .map_err(map_error)?;
        Ok(Some(n))
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
