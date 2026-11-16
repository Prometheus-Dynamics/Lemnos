//! A scripted device controller for the device side's tests. The script is the host: it queues events (SETUP
//! packets, bus resets) and bulk frames, and the model records what the device side did to the controller.
//! The same code the chip runs (`usb::enumerate`, `usb::BulkLink`) drives it.

use std::collections::VecDeque;
use std::vec::Vec;

use crate::usb::{Controller, Event, UsbError};
use lemnos_flash_wire::usb::EP0_MAX_PACKET;

/// What the device side did on the control endpoint or the configuration, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Data(Vec<u8>),
    StatusIn,
    StatusOut,
    Stall,
    Address(u8),
    Configured(bool),
}

/// The scripted controller.
#[derive(Default)]
pub struct Model {
    /// Events the host has queued, in order.
    pub events: VecDeque<Event>,
    /// Bulk OUT frames the host has sent, in order.
    pub bulk_out: VecDeque<Vec<u8>>,
    /// What the device side did, in order.
    pub log: Vec<Action>,
    /// Bulk IN replies the device sent, in order.
    pub bulk_in: Vec<Vec<u8>>,
    armed: Option<usize>,
}

impl Model {
    /// A model whose host sends `events` in order.
    pub fn with_events(events: impl IntoIterator<Item = Event>) -> Self {
        Self {
            events: events.into_iter().collect(),
            ..Self::default()
        }
    }

    /// The control request a host sends: its eight bytes.
    pub fn setup(request_type: u8, request: u8, value: u16, index: u16, length: u16) -> Event {
        let v = value.to_le_bytes();
        let i = index.to_le_bytes();
        let l = length.to_le_bytes();
        Event::Setup([request_type, request, v[0], v[1], i[0], i[1], l[0], l[1]])
    }

    /// The device side's data packets for one control IN request, as the host reads them.
    pub fn data_of(&self) -> Vec<Vec<u8>> {
        self.log
            .iter()
            .filter_map(|a| match a {
                Action::Data(d) => Some(d.clone()),
                _ => None,
            })
            .collect()
    }
}

impl Controller for Model {
    fn poll(&mut self) -> Event {
        self.events.pop_front().unwrap_or(Event::None)
    }

    fn set_address(&mut self, address: u8) -> Result<(), UsbError> {
        self.log.push(Action::Address(address));
        Ok(())
    }

    fn set_configured(&mut self, on: bool) -> Result<(), UsbError> {
        self.log.push(Action::Configured(on));
        Ok(())
    }

    fn ep0_in(&mut self, data: &[u8]) -> Result<(), UsbError> {
        assert!(
            data.len() <= usize::from(EP0_MAX_PACKET),
            "a control packet is at most one max packet"
        );
        self.log.push(Action::Data(data.to_vec()));
        Ok(())
    }

    fn ep0_in_status(&mut self) -> Result<(), UsbError> {
        self.log.push(Action::StatusIn);
        Ok(())
    }

    fn ep0_out_status(&mut self) -> Result<(), UsbError> {
        self.log.push(Action::StatusOut);
        Ok(())
    }

    fn ep0_stall(&mut self) {
        self.log.push(Action::Stall);
    }

    fn bulk_arm(&mut self, len: usize) -> Result<(), UsbError> {
        self.armed = Some(len);
        Ok(())
    }

    fn bulk_poll(&mut self, out: &mut [u8]) -> Result<Option<usize>, UsbError> {
        let Some(len) = self.armed else {
            return Err(UsbError::Failed);
        };
        // The host's control requests come before its next frame: a frame is not delivered while events are queued.
        if !self.events.is_empty() {
            return Ok(None);
        }
        let Some(frame) = self.bulk_out.pop_front() else {
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
    use crate::usb::{BulkLink, EnumError, Ep0, descriptors, enumerate};
    use crate::wait::IterClock;
    use lemnos_flash_wire::usb::{PRODUCT, PRODUCT_ID, VENDOR_ID};

    const GET: u8 = 0x80;
    const SET: u8 = 0x00;

    fn host_enumerates_fully() -> Vec<Event> {
        vec![
            Model::setup(GET, 6, 0x0100, 0, 18),       // device descriptor
            Model::setup(GET, 6, 0x0200, 0, 255),      // configuration, wLength past its size
            Model::setup(GET, 6, 0x0300, 0, 255),      // string 0
            Model::setup(GET, 6, 0x0302, 0x0409, 255), // product string
            Model::setup(SET, 5, 7, 0, 0),             // SET_ADDRESS 7
            Model::setup(SET, 9, 1, 0, 0),             // SET_CONFIGURATION 1
        ]
    }

    #[test]
    fn a_host_that_enumerates_fully_gets_the_device_and_the_configuration() {
        let mut ctrl = Model::with_events(host_enumerates_fully());
        let mut clock = IterClock::new();
        let mut ep0 = Ep0::new();
        enumerate(&mut ctrl, &mut clock, &mut ep0).unwrap();
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
        assert_eq!(
            ctrl.log.last(),
            Some(&Action::StatusIn),
            "SET_CONFIGURATION ends with its status stage"
        );
        assert!(ctrl.log.contains(&Action::Configured(true)));
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
        let mut events = vec![Model::setup(0xC0, 1, 0, 0, 16)]; // a vendor request
        events.extend(host_enumerates_fully());
        let mut ctrl = Model::with_events(events);
        let mut clock = IterClock::new();
        let mut ep0 = Ep0::new();
        enumerate(&mut ctrl, &mut clock, &mut ep0).unwrap();
        assert_eq!(ctrl.log[0], Action::Stall);
        assert!(ep0.configured());
    }

    #[test]
    fn a_host_that_never_sets_a_configuration_ends_the_enumeration_at_its_limit() {
        let mut ctrl = Model::with_events([Model::setup(GET, 6, 0x0100, 0, 18)]);
        let mut clock = IterClock::new();
        let mut ep0 = Ep0::new();
        assert_eq!(
            enumerate(&mut ctrl, &mut clock, &mut ep0),
            Err(EnumError::NotConfigured)
        );
        assert!(!ep0.configured());
    }

    #[test]
    fn a_bus_reset_during_enumeration_starts_it_again() {
        let mut events = vec![Model::setup(SET, 5, 9, 0, 0), Event::BusReset];
        events.extend(host_enumerates_fully());
        let mut ctrl = Model::with_events(events);
        let mut clock = IterClock::new();
        let mut ep0 = Ep0::new();
        enumerate(&mut ctrl, &mut clock, &mut ep0).unwrap();
        assert_eq!(
            ep0.address(),
            7,
            "the reset cleared the address 9 the host had set"
        );
        assert!(ctrl.log.contains(&Action::Address(9)));
    }

    #[test]
    fn a_disconnect_ends_the_enumeration() {
        let mut ctrl = Model::with_events([Event::Disconnect]);
        let mut clock = IterClock::new();
        let mut ep0 = Ep0::new();
        assert_eq!(
            enumerate(&mut ctrl, &mut clock, &mut ep0),
            Err(EnumError::Disconnected)
        );
    }

    fn configured_device() -> (Model, Ep0) {
        let mut ctrl = Model::with_events(host_enumerates_fully());
        let mut ep0 = Ep0::new();
        enumerate(&mut ctrl, &mut IterClock::new(), &mut ep0).unwrap();
        ctrl.log.clear();
        (ctrl, ep0)
    }

    #[test]
    fn a_bulk_frame_is_received_whole_and_a_reply_is_sent() {
        let (mut ctrl, mut ep0) = configured_device();
        ctrl.bulk_out.push_back(vec![7u8; 12]);
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
        let (mut ctrl, mut ep0) = configured_device();
        let mut clock = IterClock::new();
        let mut link = BulkLink {
            ctrl: &mut ctrl,
            clock: &mut clock,
            ep0: &mut ep0,
        };
        let mut buf = [0u8; 12];
        assert_eq!(link.recv(&mut buf, 1_000), Err(IoError::Timeout));
    }

    #[test]
    fn a_bus_reset_while_waiting_for_a_frame_is_a_reset() {
        let (mut ctrl, mut ep0) = configured_device();
        ctrl.events.push_back(Event::BusReset);
        let mut clock = IterClock::new();
        let mut link = BulkLink {
            ctrl: &mut ctrl,
            clock: &mut clock,
            ep0: &mut ep0,
        };
        let mut buf = [0u8; 12];
        assert_eq!(link.recv(&mut buf, 1_000_000), Err(IoError::Reset));
    }

    #[test]
    fn an_unconfiguration_while_waiting_for_a_frame_is_a_reset() {
        let (mut ctrl, mut ep0) = configured_device();
        ctrl.events.push_back(Model::setup(SET, 9, 0, 0, 0));
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
        let (mut ctrl, mut ep0) = configured_device();
        ctrl.events.push_back(Model::setup(GET, 0, 0, 0, 2)); // GET_STATUS
        ctrl.bulk_out.push_back(vec![3u8; 12]);
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
        let (mut ctrl, mut ep0) = configured_device();
        ctrl.events.push_back(Model::setup(0x21, 0x20, 0, 0, 7)); // a class request
        ctrl.bulk_out.push_back(vec![3u8; 12]);
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
