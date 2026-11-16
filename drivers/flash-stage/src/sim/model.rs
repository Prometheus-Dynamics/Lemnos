//! A scripted device controller, for the device side's tests and for the simulated device. The script is the
//! host: an ordered list of control events (SETUP packets, bus resets) and bulk frames. The controller hands the
//! steps out in order, so a reset can land between two requests. The model starts at reset: address 0, no
//! configuration. The same code the chip runs (`usb::enumerate`, `usb::BulkLink`, `session::run`) drives it.

use std::cell::Cell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::vec::Vec;

use crate::usb::{Controller, Event, Speed, UsbError};
use crate::wait::Clock;
use lemnos_flash_wire::usb::EP0_MAX_PACKET;

/// The simulated controller's clock. It moves only when the controller moves it: each controller pass takes one tick,
/// and a command takes its own time (`Model::with_command_us`). So a sim run has exact instants: the deadline tests
/// assert the instant they expect, not a tolerance.
#[derive(Clone, Default)]
pub struct SimClock(Rc<Cell<u64>>);

impl SimClock {
    /// A clock reading `us`.
    pub fn at(us: u64) -> Self {
        Self(Rc::new(Cell::new(us)))
    }

    /// The clock's reading.
    pub fn now(&self) -> u64 {
        self.0.get()
    }

    /// Moves the clock forward by `us`.
    pub fn advance(&self, us: u64) {
        self.0.set(self.0.get() + us);
    }
}

impl Clock for SimClock {
    fn now_us(&mut self) -> u64 {
        self.now()
    }
}

/// One controller pass, in microseconds, when a clock is attached.
const PASS_US: u64 = 1_000;

/// One step of the host's script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// A control event: a SETUP packet, a bus reset, a disconnect.
    Event(Event),
    /// A bulk OUT frame, delivered to the next armed receive.
    Frame(Vec<u8>),
    /// A pause: the next `n` receive polls find nothing. Each poll is a turn of the wait, so the clock moves on.
    Gap(u64),
    /// Changes what the controller operations cost, from this point of the script on.
    Costs(Costs),
}

/// What the device side did to the controller, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Data(Vec<u8>),
    StatusIn,
    StatusOut,
    Stall,
    Address(u8),
    Configured(bool),
}

/// What each controller operation costs on the simulated clock, in microseconds. Every operation charges its cost against
/// the deadline it is given: a cost that runs past the deadline advances the clock to the deadline and is a timeout, and
/// the operation does not complete. The default is free, so a test sets the costs it means to exercise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Costs {
    /// One control data packet or status stage on EP0.
    pub control_us: u64,
    /// Enabling or disabling the bulk pair (the endpoint configuration commands).
    pub configure_us: u64,
    /// Stalling the control endpoint.
    pub stall_us: u64,
    /// Setting the device address.
    pub address_us: u64,
}

/// The scripted controller.
#[derive(Default)]
pub struct Model {
    /// The host's script, in order.
    pub script: VecDeque<Step>,
    /// What the device side did, in order.
    pub log: Vec<Action>,
    /// The bulk IN replies the device sent, in order.
    pub bulk_in: Vec<Vec<u8>>,
    /// The speed the bus negotiated. High by default.
    pub speed: Speed,
    /// The simulated clock, when the test wants exact instants. `None` leaves time to the test's own clock.
    clock: Option<SimClock>,
    /// How long a bulk arm takes. A command that would not finish by its deadline times out at the deadline.
    command_us: u64,
    /// What the other controller operations cost.
    costs: Costs,
    armed: Option<usize>,
}

impl Model {
    /// A controller at reset with an empty script.
    pub fn new() -> Self {
        Self::default()
    }

    /// Attaches the simulated clock: every controller pass moves it by one tick.
    pub fn on(mut self, clock: &SimClock) -> Self {
        self.clock = Some(clock.clone());
        self
    }

    /// Makes a bulk arm take `us` (the controller's command time). An arm whose deadline comes sooner times out there.
    pub fn with_command_us(mut self, us: u64) -> Self {
        self.command_us = us;
        self
    }

    /// Sets what the control operations cost (see [`Costs`]).
    pub fn with_costs(mut self, costs: Costs) -> Self {
        self.costs = costs;
        self
    }

    /// Charges `us` against `deadline_us` on the attached clock. An operation that cannot finish by the deadline advances
    /// the clock to it and is a timeout; one that can finish advances the clock by its cost. Without a clock nothing is
    /// charged and the operation completes.
    fn charge(&mut self, us: u64, deadline_us: u64) -> Result<(), UsbError> {
        let Some(clock) = self.clock.clone() else {
            return Ok(());
        };
        let now = clock.now();
        if now >= deadline_us {
            return Err(UsbError::Timeout);
        }
        let remaining = deadline_us - now;
        if us > remaining {
            clock.advance(remaining);
            return Err(UsbError::Timeout);
        }
        clock.advance(us);
        Ok(())
    }

    /// Queues a control event.
    pub fn event(&mut self, event: Event) -> &mut Self {
        self.script.push_back(Step::Event(event));
        self
    }

    /// Queues a bulk OUT frame.
    pub fn frame(&mut self, frame: Vec<u8>) -> &mut Self {
        self.script.push_back(Step::Frame(frame));
        self
    }

    /// Queues a change of the operation costs, taking effect when the script reaches it.
    pub fn costs(&mut self, costs: Costs) -> &mut Self {
        self.script.push_back(Step::Costs(costs));
        self
    }

    /// Applies any cost changes at the front of the script.
    fn apply_costs(&mut self) {
        while let Some(Step::Costs(costs)) = self.script.front() {
            self.costs = *costs;
            self.script.pop_front();
        }
    }

    /// Queues a pause of `polls` receive turns.
    pub fn gap(&mut self, polls: u64) -> &mut Self {
        self.script.push_back(Step::Gap(polls));
        self
    }

    /// Queues the standard enumeration a host runs after a reset: the device and configuration descriptors, the
    /// strings, SET_ADDRESS, and SET_CONFIGURATION 1.
    pub fn enumeration(&mut self, address: u16) -> &mut Self {
        self.event(Self::setup(GET, 6, 0x0100, 0, 18))
            .event(Self::setup(GET, 6, 0x0200, 0, 255))
            .event(Self::setup(GET, 6, 0x0300, 0, 255))
            .event(Self::setup(GET, 6, 0x0302, 0x0409, 255))
            .event(Self::setup(SET, 5, address, 0, 0))
            .event(Self::setup(SET, 9, 1, 0, 0))
    }

    /// The control request a host sends: its eight bytes, as an event.
    pub fn setup(request_type: u8, request: u8, value: u16, index: u16, length: u16) -> Event {
        let v = value.to_le_bytes();
        let i = index.to_le_bytes();
        let l = length.to_le_bytes();
        Event::Setup([request_type, request, v[0], v[1], i[0], i[1], l[0], l[1]])
    }

    /// The data packets the device side sent on control IN, in order.
    pub fn data_of(&self) -> Vec<Vec<u8>> {
        self.log
            .iter()
            .filter_map(|a| match a {
                Action::Data(d) => Some(d.clone()),
                _ => None,
            })
            .collect()
    }

    /// Whether the next script step is a frame: the link has something to deliver.
    pub fn frame_pending(&self) -> bool {
        matches!(self.script.front(), Some(Step::Frame(_)))
    }
}

/// The request types the enumeration uses.
const GET: u8 = 0x80;
const SET: u8 = 0x00;

impl Controller for Model {
    fn poll(&mut self, _deadline_us: u64) -> Event {
        self.apply_costs();
        if let Some(clock) = &self.clock {
            clock.advance(PASS_US);
        }
        match self.script.front() {
            Some(Step::Event(_)) => match self.script.pop_front() {
                Some(Step::Event(event)) => event,
                _ => Event::None,
            },
            _ => Event::None,
        }
    }

    fn set_address(&mut self, address: u8, deadline_us: u64) -> Result<(), UsbError> {
        self.charge(self.costs.address_us, deadline_us)?;
        self.log.push(Action::Address(address));
        Ok(())
    }

    fn speed(&mut self) -> Speed {
        self.speed
    }

    fn address_before_status(&self) -> bool {
        // The model stands in for a controller that takes the address before the status stage.
        true
    }

    fn set_configured(&mut self, on: bool, deadline_us: u64) -> Result<(), UsbError> {
        self.charge(self.costs.configure_us, deadline_us)?;
        self.log.push(Action::Configured(on));
        Ok(())
    }

    fn ep0_in(&mut self, data: &[u8], deadline_us: u64) -> Result<(), UsbError> {
        assert!(
            data.len() <= usize::from(EP0_MAX_PACKET),
            "a control packet is at most one max packet"
        );
        self.charge(self.costs.control_us, deadline_us)?;
        self.log.push(Action::Data(data.to_vec()));
        Ok(())
    }

    fn ep0_in_status(&mut self, deadline_us: u64) -> Result<(), UsbError> {
        self.charge(self.costs.control_us, deadline_us)?;
        self.log.push(Action::StatusIn);
        Ok(())
    }

    fn ep0_out_status(&mut self, deadline_us: u64) -> Result<(), UsbError> {
        self.charge(self.costs.control_us, deadline_us)?;
        self.log.push(Action::StatusOut);
        Ok(())
    }

    fn ep0_stall(&mut self, deadline_us: u64) -> Result<(), UsbError> {
        self.charge(self.costs.stall_us, deadline_us)?;
        self.log.push(Action::Stall);
        Ok(())
    }

    fn bulk_arm(&mut self, len: usize, deadline_us: u64) -> Result<(), UsbError> {
        self.apply_costs();
        self.charge(self.command_us, deadline_us)?;
        self.armed = Some(len);
        Ok(())
    }

    fn bulk_poll(&mut self, out: &mut [u8]) -> Result<Option<usize>, UsbError> {
        self.apply_costs();
        let Some(len) = self.armed else {
            return Err(UsbError::Failed);
        };
        if let Some(Step::Gap(polls)) = self.script.front_mut() {
            if *polls > 0 {
                *polls -= 1;
                return Ok(None);
            }
            self.script.pop_front();
        }
        if !self.frame_pending() {
            return Ok(None);
        }
        let Some(Step::Frame(frame)) = self.script.pop_front() else {
            return Ok(None);
        };
        self.armed = None;
        let n = frame.len().min(len).min(out.len());
        out[..n].copy_from_slice(&frame[..n]);
        Ok(Some(frame.len()))
    }

    fn bulk_send(&mut self, data: &[u8], _budget_us: u64) -> Result<(), UsbError> {
        self.bulk_in.push(data.to_vec());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::{IoError, Link};
    use crate::usb::{BulkLink, EnumError, Ep0, descriptors, enumerate_from_now};
    use crate::wait::{Clock, IterClock};
    use lemnos_flash_wire::usb::{PRODUCT, PRODUCT_ID, VENDOR_ID};

    /// A clock that moves a millisecond per read, so a limit of thirty seconds is thirty thousand reads.
    struct Quick(u64);

    impl Clock for Quick {
        fn now_us(&mut self) -> u64 {
            self.0 += 1_000;
            self.0
        }
    }

    fn enumerated() -> (Model, Ep0) {
        let mut ctrl = Model::new();
        ctrl.enumeration(7);
        let mut ep0 = Ep0::new();
        enumerate_from_now(&mut ctrl, &mut IterClock::new(), &mut ep0).unwrap();
        ctrl.log.clear();
        (ctrl, ep0)
    }

    #[test]
    fn a_host_that_enumerates_fully_from_reset_gets_the_device_and_the_configuration() {
        let mut ctrl = Model::new();
        ctrl.enumeration(7);
        let mut ep0 = Ep0::new();
        assert!(
            !ep0.configured(),
            "the device starts at reset: unconfigured, address 0"
        );
        enumerate_from_now(&mut ctrl, &mut IterClock::new(), &mut ep0).unwrap();
        assert!(ep0.configured());
        assert_eq!(ep0.address(), 7);
        let data = ctrl.data_of();
        assert_eq!(data[0], descriptors::DEVICE.to_vec(), "device descriptor");
        assert_eq!(
            data[1],
            descriptors::CONFIGURATION.to_vec(),
            "configuration, truncated to its size"
        );
        assert_eq!(data[2], descriptors::LANGUAGES.to_vec());
        assert_eq!(data[3], descriptors::PRODUCT_STRING.to_vec());
        assert!(ctrl.log.contains(&Action::Address(7)));
        assert!(ctrl.log.contains(&Action::Configured(true)));
        assert_eq!(
            ctrl.log.last(),
            Some(&Action::StatusIn),
            "SET_CONFIGURATION ends with its status stage"
        );
    }

    #[test]
    fn the_device_descriptor_carries_the_wire_identity() {
        let d = descriptors::DEVICE;
        assert_eq!(d[0], 18);
        assert_eq!(u16::from_le_bytes([d[8], d[9]]), VENDOR_ID);
        assert_eq!(u16::from_le_bytes([d[10], d[11]]), PRODUCT_ID);
        assert_eq!(d[7], 64, "EP0 packet size");
    }

    #[test]
    fn the_configuration_names_one_vendor_interface_and_two_bulk_endpoints() {
        let c = descriptors::CONFIGURATION;
        assert_eq!(
            u16::from_le_bytes([c[2], c[3]]) as usize,
            c.len(),
            "wTotalLength covers it all"
        );
        assert_eq!(c[9 + 5], 0xFF, "interface class: vendor specific");
        assert_eq!(c[9 + 4], 2, "two endpoints");
        assert_eq!(c[18 + 2], 0x01, "bulk OUT");
        assert_eq!(c[25 + 2], 0x81, "bulk IN");
        assert_eq!(u16::from_le_bytes([c[18 + 4], c[18 + 5]]), 512);
    }

    #[test]
    fn the_product_string_is_utf16_and_names_the_stage() {
        let s = descriptors::PRODUCT_STRING;
        assert_eq!(usize::from(s[0]), 2 + 2 * PRODUCT.len());
        let text: Vec<u8> = s[2..].iter().step_by(2).copied().collect();
        assert_eq!(text, PRODUCT.as_bytes());
    }

    #[test]
    fn a_request_the_device_does_not_answer_is_stalled_and_enumeration_goes_on() {
        let mut ctrl = Model::new();
        ctrl.event(Model::setup(0xC0, 1, 0, 0, 16));
        ctrl.enumeration(7);
        let mut ep0 = Ep0::new();
        enumerate_from_now(&mut ctrl, &mut IterClock::new(), &mut ep0).unwrap();
        assert_eq!(ctrl.log[0], Action::Stall);
        assert!(ep0.configured());
    }

    #[test]
    fn a_host_that_never_sets_a_configuration_ends_the_enumeration_at_its_limit() {
        let mut ctrl = Model::new();
        ctrl.event(Model::setup(GET, 6, 0x0100, 0, 18));
        let mut ep0 = Ep0::new();
        assert_eq!(
            enumerate_from_now(&mut ctrl, &mut Quick(0), &mut ep0),
            Err(EnumError::NotConfigured)
        );
        assert!(!ep0.configured());
    }

    #[test]
    fn a_bus_reset_during_enumeration_starts_it_again() {
        let mut ctrl = Model::new();
        ctrl.event(Model::setup(SET, 5, 9, 0, 0))
            .event(Event::BusReset)
            .enumeration(7);
        let mut ep0 = Ep0::new();
        enumerate_from_now(&mut ctrl, &mut IterClock::new(), &mut ep0).unwrap();
        assert_eq!(
            ep0.address(),
            7,
            "the reset cleared the address 9 the host had set"
        );
        assert!(ctrl.log.contains(&Action::Address(9)));
    }

    #[test]
    fn a_host_that_configures_a_full_speed_link_fails_enumeration_and_the_device_is_unconfigured() {
        let mut ctrl = Model::new();
        ctrl.speed = Speed::Full;
        ctrl.enumeration(7);
        let mut ep0 = Ep0::new();
        assert_eq!(
            enumerate_from_now(&mut ctrl, &mut IterClock::new(), &mut ep0),
            Err(EnumError::NotHighSpeed)
        );
        assert!(!ep0.configured(), "the configuration was undone");
        assert_eq!(
            ctrl.log.last(),
            Some(&Action::Configured(false)),
            "and the bulk pair was disabled"
        );
    }

    #[test]
    fn a_disconnect_ends_the_enumeration() {
        let mut ctrl = Model::new();
        ctrl.event(Event::Disconnect);
        let mut ep0 = Ep0::new();
        assert_eq!(
            enumerate_from_now(&mut ctrl, &mut IterClock::new(), &mut ep0),
            Err(EnumError::Disconnected)
        );
    }

    #[test]
    fn a_bulk_frame_is_received_whole_and_a_reply_is_sent() {
        let (mut ctrl, mut ep0) = enumerated();
        ctrl.frame(vec![7u8; 12]);
        let mut clock = IterClock::new();
        let mut link = BulkLink {
            ctrl: &mut ctrl,
            clock: &mut clock,
            ep0: &mut ep0,
        };
        let mut buf = [0u8; 12];
        assert_eq!(link.recv(&mut buf, 1_000_000), Ok(12));
        assert_eq!(buf, [7u8; 12]);
        link.send(&[0], 1_000_000).unwrap();
        assert_eq!(ctrl.bulk_in, vec![vec![0u8]]);
    }

    #[test]
    fn a_frame_that_never_comes_times_out_at_the_budget() {
        let (mut ctrl, mut ep0) = enumerated();
        let mut clock = Quick(0);
        let mut link = BulkLink {
            ctrl: &mut ctrl,
            clock: &mut clock,
            ep0: &mut ep0,
        };
        let mut buf = [0u8; 12];
        assert_eq!(link.recv(&mut buf, 1_000_000), Err(IoError::Timeout));
    }

    #[test]
    fn a_bus_reset_while_waiting_for_a_frame_is_a_reset_and_forgets_the_configuration() {
        let (mut ctrl, mut ep0) = enumerated();
        ctrl.event(Event::BusReset);
        let mut clock = IterClock::new();
        let mut link = BulkLink {
            ctrl: &mut ctrl,
            clock: &mut clock,
            ep0: &mut ep0,
        };
        let mut buf = [0u8; 12];
        assert_eq!(link.recv(&mut buf, 1_000_000), Err(IoError::Reset));
        assert!(!ep0.configured());
    }

    #[test]
    fn an_unconfiguration_while_waiting_for_a_frame_is_a_reset() {
        let (mut ctrl, mut ep0) = enumerated();
        ctrl.event(Model::setup(SET, 9, 0, 0, 0));
        let mut clock = IterClock::new();
        let mut link = BulkLink {
            ctrl: &mut ctrl,
            clock: &mut clock,
            ep0: &mut ep0,
        };
        let mut buf = [0u8; 12];
        assert_eq!(link.recv(&mut buf, 1_000_000), Err(IoError::Reset));
        assert!(!ep0.configured());
    }

    #[test]
    fn a_control_request_during_a_session_is_answered_and_the_frame_still_comes() {
        let (mut ctrl, mut ep0) = enumerated();
        ctrl.event(Model::setup(GET, 0, 0, 0, 2))
            .frame(vec![3u8; 12]); // GET_STATUS, then the frame
        let mut clock = IterClock::new();
        let mut link = BulkLink {
            ctrl: &mut ctrl,
            clock: &mut clock,
            ep0: &mut ep0,
        };
        let mut buf = [0u8; 12];
        assert_eq!(link.recv(&mut buf, 1_000_000), Ok(12));
        assert!(ctrl.log.contains(&Action::Data(vec![0, 0])));
    }

    #[test]
    fn a_stalled_unsupported_request_in_a_session_does_not_end_it() {
        let (mut ctrl, mut ep0) = enumerated();
        ctrl.event(Model::setup(0x21, 0x20, 0, 0, 7))
            .frame(vec![3u8; 12]); // a class request, then the frame
        let mut clock = IterClock::new();
        let mut link = BulkLink {
            ctrl: &mut ctrl,
            clock: &mut clock,
            ep0: &mut ep0,
        };
        let mut buf = [0u8; 12];
        assert_eq!(link.recv(&mut buf, 1_000_000), Ok(12));
        assert_eq!(ctrl.log, vec![Action::Stall]);
    }
}
