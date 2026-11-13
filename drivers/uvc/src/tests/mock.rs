//! A USB camera a test can script. It answers the probe control as a device would and hands back
//! queued isochronous transfers. Clones share state, so the test keeps one.

use core::time::Duration;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use lemnos_core::ErrorKind;
use lemnos_hal::UsbDevice;
use lemnos_hal::blocks::{ControlSetup, IsoPacket};

use crate::probe;

/// One packet of a queued transfer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Packet {
    pub data: Vec<u8>,
    /// The bus's own verdict.
    pub ok: bool,
    /// What the transfer claims arrived, when it lies.
    pub claimed: Option<u32>,
}

impl Packet {
    pub fn good(data: Vec<u8>) -> Self {
        Self {
            data,
            ok: true,
            claimed: None,
        }
    }

    pub fn failed() -> Self {
        Self {
            data: Vec::new(),
            ok: false,
            claimed: None,
        }
    }

    pub fn lying(data: Vec<u8>, claimed: u32) -> Self {
        Self {
            data,
            ok: true,
            claimed: Some(claimed),
        }
    }
}

/// A control transfer as the device saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Control {
    pub setup: ControlSetup,
    pub data: Vec<u8>,
}

#[derive(Default)]
pub struct DeviceState {
    pub descriptors: Vec<u8>,
    pub claim_errors: VecDeque<ErrorKind>,
    pub claimed: Vec<u8>,
    pub claims: u32,
    pub descriptor_reads: u32,
    pub controls: Vec<Control>,
    pub alternates: Vec<(u8, u8)>,
    /// What the device answers `dwMaxPayloadTransferSize` with.
    pub max_payload: u32,
    /// The device changes the block's `bFormatIndex` to this.
    pub answer_format: Option<u8>,
    /// The next `GET_CUR` is cut to this many bytes.
    pub short_probe: Option<usize>,
    /// The next probe `SET_CUR` reports this many bytes taken.
    pub short_probe_write: Option<usize>,
    pub control_errors: VecDeque<ErrorKind>,
    pub alternate_errors: VecDeque<ErrorKind>,
    pub iso: VecDeque<Vec<Packet>>,
    pub iso_errors: VecDeque<ErrorKind>,
    pub iso_calls: u32,
    /// The endpoint and packet capacity of the last stream started.
    pub last_iso: Option<(u8, u32)>,
    /// The stream started and not stopped: endpoint, transfers, packets and packet bytes.
    pub stream: Option<(u8, u8, u16, u32)>,
    /// Streams started, and streams stopped.
    pub starts: u32,
    pub stops: u32,
    /// Alternate settings chosen while a stream was started.
    pub alternates_mid_stream: u32,
    /// Interfaces the host's own driver holds: a claim finds them `Busy`.
    pub kernel_drivers: Vec<u8>,
    /// Every `detach_kernel_driver`, by interface, in order.
    pub detaches: Vec<u8>,
    pub detach_errors: VecDeque<ErrorKind>,
    /// The device has left: every call fails with `Unavailable`.
    pub gone: bool,
    /// Requests made of the handle after the device had gone.
    pub refused: u32,
    probe_block: Vec<u8>,
}

#[derive(Clone)]
pub struct MockUsb(Arc<Mutex<DeviceState>>);

impl MockUsb {
    pub fn new(descriptors: Vec<u8>) -> Self {
        Self(Arc::new(Mutex::new(DeviceState {
            descriptors,
            max_payload: 900,
            probe_block: vec![0; probe::CONTROL_MAX],
            ..DeviceState::default()
        })))
    }

    pub fn state(&self) -> MutexGuard<'_, DeviceState> {
        self.0.lock().unwrap()
    }

    /// Queues one isochronous transfer.
    pub fn queue(&self, packets: Vec<Packet>) {
        self.state().iso.push_back(packets);
    }

    /// Binds the host's own driver to `interface`, as the kernel does when a camera is plugged in.
    pub fn bind_kernel_driver(&self, interface: u8) {
        self.state().kernel_drivers.push(interface);
    }

    /// The device leaves the bus. The handle never reaches it again.
    pub fn unplug(&self) {
        self.state().gone = true;
    }

    fn present(&self) -> Result<MutexGuard<'_, DeviceState>, ErrorKind> {
        let mut device = self.state();
        if device.gone {
            device.refused += 1;
            return Err(ErrorKind::Unavailable);
        }
        Ok(device)
    }
}

impl UsbDevice for MockUsb {
    type Error = ErrorKind;

    fn config_descriptor(&mut self, index: u8, out: &mut [u8]) -> Result<usize, ErrorKind> {
        assert_eq!(index, 0, "a camera has one configuration here");
        let mut device = self.present()?;
        device.descriptor_reads += 1;
        let length = device.descriptors.len().min(out.len());
        out[..length].copy_from_slice(&device.descriptors[..length]);
        Ok(length)
    }

    /// Claims stand, and alternate settings go back to none.
    fn reset(&mut self) -> Result<(), ErrorKind> {
        self.present()?.alternates.clear();
        Ok(())
    }

    fn detach_kernel_driver(&mut self, interface: u8) -> Result<(), ErrorKind> {
        let mut device = self.present()?;
        device.detaches.push(interface);
        if let Some(error) = device.detach_errors.pop_front() {
            return Err(error);
        }
        device.kernel_drivers.retain(|bound| *bound != interface);
        Ok(())
    }

    fn claim_interface(&mut self, interface: u8) -> Result<(), ErrorKind> {
        let mut device = self.present()?;
        device.claims += 1;
        if let Some(error) = device.claim_errors.pop_front() {
            return Err(error);
        }
        if device.kernel_drivers.contains(&interface) || device.claimed.contains(&interface) {
            return Err(ErrorKind::Busy);
        }
        device.claimed.push(interface);
        Ok(())
    }

    fn release_interface(&mut self, interface: u8) -> Result<(), ErrorKind> {
        self.present()?
            .claimed
            .retain(|claimed| *claimed != interface);
        Ok(())
    }

    fn set_alternate(&mut self, interface: u8, alternate: u8) -> Result<(), ErrorKind> {
        let mut device = self.present()?;
        if let Some(error) = device.alternate_errors.pop_front() {
            return Err(error);
        }
        if device.stream.is_some() {
            device.alternates_mid_stream += 1;
        }
        device.alternates.push((interface, alternate));
        Ok(())
    }

    fn control_in(
        &mut self,
        setup: ControlSetup,
        data: &mut [u8],
        _timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        let mut device = self.present()?;
        device.controls.push(Control {
            setup,
            data: Vec::new(),
        });
        if let Some(error) = device.control_errors.pop_front() {
            return Err(error);
        }
        let mut block = device.probe_block.clone();
        block[22..26].copy_from_slice(&device.max_payload.to_le_bytes());
        block[18..22].copy_from_slice(&153_600u32.to_le_bytes());
        // A field the host left at 0 and a device fills in.
        block[12..14].copy_from_slice(&5000u16.to_le_bytes());
        if let Some(format) = device.answer_format {
            block[2] = format;
        }
        let length = device
            .short_probe
            .take()
            .unwrap_or(data.len())
            .min(data.len());
        data[..length].copy_from_slice(&block[..length]);
        Ok(length)
    }

    fn control_out(
        &mut self,
        setup: ControlSetup,
        data: &[u8],
        _timeout: Duration,
    ) -> Result<usize, ErrorKind> {
        let mut device = self.present()?;
        device.controls.push(Control {
            setup,
            data: data.to_vec(),
        });
        if let Some(error) = device.control_errors.pop_front() {
            return Err(error);
        }
        if setup.value == u16::from(probe::VS_PROBE_CONTROL) << 8 {
            let length = data.len().min(probe::CONTROL_MAX);
            device.probe_block = vec![0; probe::CONTROL_MAX];
            device.probe_block[..length].copy_from_slice(&data[..length]);
            if let Some(taken) = device.short_probe_write.take() {
                return Ok(taken);
            }
        }
        Ok(data.len())
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

    fn iso_in_start(
        &mut self,
        endpoint: u8,
        transfers: u8,
        packets: u16,
        packet_bytes: u32,
    ) -> Result<(), ErrorKind> {
        let mut device = self.present()?;
        if device.stream.is_some() {
            return Err(ErrorKind::Busy);
        }
        assert!(
            lemnos_hal::blocks::iso_shape_ok(transfers, packets, packet_bytes),
            "a stream no backend takes"
        );
        device.stream = Some((endpoint, transfers, packets, packet_bytes));
        device.starts += 1;
        device.last_iso = Some((endpoint, packet_bytes));
        Ok(())
    }

    fn iso_in_next(
        &mut self,
        data: &mut [u8],
        packets: &mut [IsoPacket],
        _timeout: Duration,
    ) -> Result<(), ErrorKind> {
        let mut device = self.present()?;
        let Some((_, _, count, bytes)) = device.stream else {
            return Err(ErrorKind::InvalidInput);
        };
        let capacity = bytes as usize;
        if packets.len() != usize::from(count) || data.len() < capacity * packets.len() {
            return Err(ErrorKind::InvalidInput);
        }
        device.iso_calls += 1;
        if let Some(error) = device.iso_errors.pop_front() {
            return Err(error);
        }
        let transfer = device.iso.pop_front().unwrap_or_default();
        assert!(transfer.len() <= packets.len(), "a test queued too many");
        for (index, packet) in packets.iter_mut().enumerate() {
            packet.capacity = bytes;
            match transfer.get(index) {
                Some(queued) => {
                    assert!(
                        queued.data.len() <= capacity,
                        "a packet the bus can't carry"
                    );
                    let offset = index * capacity;
                    data[offset..offset + queued.data.len()].copy_from_slice(&queued.data);
                    packet.received = queued.claimed.unwrap_or(queued.data.len() as u32);
                    packet.ok = queued.ok;
                }
                None => {
                    packet.received = 0;
                    packet.ok = true;
                }
            }
        }
        Ok(())
    }

    /// Always `Ok`: a device that has gone took its stream with it, as usbfs does.
    fn iso_in_stop(&mut self) -> Result<(), ErrorKind> {
        let mut device = self.state();
        if device.stream.take().is_some() {
            device.stops += 1;
        }
        Ok(())
    }
}
