//! I2C on PIO: a bit-banged I2C master on the RP1 PIO block, for pins that no
//! hardware I2C controller drives (the Raze board's BMI088 sits on GPIO8/GPIO7).
//!
//! The bus is named in a board definition as `bus = "pio-i2c:sda=8,scl=7"`
//! (optionally `,hz=100000`). It gets a virtual bus number
//! ([`lemnos_core::PioI2cPins`]) that the sessions route on. One
//! [`PioBus`] per pin pair is opened on first use and shared by every device on
//! it; transactions on it are serialized.

mod bus;
mod program;
mod protocol;

pub(crate) use bus::PioBus;

use super::{I2cControllerTransport, I2cTransport, invalid_i2c_request, transport_i2c_failure};
use bus::{DEFAULT_DEVNODE, Failure};
use lemnos_bus::{BusError, BusResult};
use lemnos_core::{DeviceDescriptor, DeviceId, I2cOperation, PioI2cPins};
use protocol::{Op, address_byte};
use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

type Registry = Mutex<HashMap<u32, Arc<PioBus>>>;

fn registry() -> &'static Registry {
    static BUSES: OnceLock<Registry> = OnceLock::new();
    BUSES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Whether a bus number is a virtual PIO bus.
pub(crate) fn is_pio_bus(bus: u32) -> bool {
    PioI2cPins::from_bus_number(bus).is_some()
}

/// The shared bus for pins and clock, opened on first use on the default
/// device node. One state machine per pin pair, shared by every user.
pub(crate) fn open_shared(pins: PioI2cPins) -> Result<Arc<PioBus>, String> {
    let number = pins.bus_number();
    let mut buses = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(existing) = buses.get(&number) {
        return Ok(Arc::clone(existing));
    }
    let opened = Arc::new(PioBus::open(
        &PathBuf::from(DEFAULT_DEVNODE),
        pins.sda,
        pins.scl,
        pins.hz,
    )?);
    buses.insert(number, Arc::clone(&opened));
    Ok(opened)
}

/// The shared bus for a virtual bus number. Failures name the device that asked.
fn bus_for(device_id: &DeviceId, bus: u32) -> BusResult<Arc<PioBus>> {
    let pins = PioI2cPins::from_bus_number(bus).ok_or_else(|| BusError::UnsupportedDevice {
        backend: "linux-pio-i2c".to_string(),
        device_id: device_id.clone(),
    })?;
    open_shared(pins).map_err(|reason| BusError::SessionUnavailable {
        device_id: device_id.clone(),
        reason: format!("PIO I2C bus sda={} scl={}: {reason}", pins.sda, pins.scl),
    })
}

/// One embedded-hal operation, as the PIO bus runs it.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Segment<'a> {
    /// Bytes written to the target (an empty write is an address probe).
    Write(&'a [u8]),
    /// This many bytes read from the target.
    Read(usize),
}

/// The ops for segments, with adjacent operations of one direction merged into
/// one transfer (no repeated START between them, as embedded-hal requires).
/// The last byte of every read run is NACKed.
pub(crate) fn segment_ops(address: u8, segments: &[Segment<'_>]) -> Vec<Op> {
    let mut ops = Vec::new();
    let mut reading: Option<bool> = None;
    let mut last_read: Option<usize> = None;
    let close_read = |ops: &mut Vec<Op>, last_read: &mut Option<usize>| {
        if let Some(index) = last_read.take() {
            ops[index] = Op::Read { ack: false };
        }
    };
    for segment in segments {
        match *segment {
            Segment::Write(bytes) => {
                if reading != Some(false) {
                    close_read(&mut ops, &mut last_read);
                    ops.push(Op::Start);
                    ops.push(Op::Write(address_byte(address, false)));
                }
                ops.extend(bytes.iter().copied().map(Op::Write));
                reading = Some(false);
            }
            Segment::Read(0) => {}
            Segment::Read(len) => {
                if reading != Some(true) {
                    close_read(&mut ops, &mut last_read);
                    ops.push(Op::Start);
                    ops.push(Op::Write(address_byte(address, true)));
                }
                for _ in 0..len {
                    ops.push(Op::Read { ack: true });
                    last_read = Some(ops.len() - 1);
                }
                reading = Some(true);
            }
        }
    }
    close_read(&mut ops, &mut last_read);
    if !ops.is_empty() {
        ops.push(Op::Stop);
    }
    ops
}

/// Runs embedded-hal segments on `bus`; returns the bytes read, in order.
pub(crate) fn run_segments(
    bus: &PioBus,
    address: u8,
    segments: &[Segment<'_>],
) -> io::Result<Vec<u8>> {
    let ops = segment_ops(address, segments);
    if ops.is_empty() {
        return Ok(Vec::new());
    }
    bus.transaction(&ops).map_err(|failure| match failure {
        Failure::Nack(nack) => io::Error::other(format!(
            "no ACK to byte {:#04x} (op {}); the target is absent or not ready",
            nack.byte, nack.op
        )),
        Failure::Timeout => io::Error::new(io::ErrorKind::TimedOut, "PIO I2C transfer timed out"),
        Failure::Io(reason) => io::Error::other(reason),
    })
}

/// Maps a bus failure onto the bus error for `operation`.
fn map_failure(device_id: &DeviceId, operation: &'static str, failure: Failure) -> BusError {
    match failure {
        Failure::Nack(nack) => transport_i2c_failure(
            device_id,
            operation,
            format!(
                "no ACK to byte {:#04x} (op {}); the target is absent or not ready",
                nack.byte, nack.op
            ),
        ),
        Failure::Timeout => BusError::Timeout {
            device_id: device_id.clone(),
            operation,
        },
        Failure::Io(reason) => transport_i2c_failure(device_id, operation, reason),
    }
}

fn run(
    bus: &PioBus,
    device_id: &DeviceId,
    operation: &'static str,
    ops: &[Op],
) -> BusResult<Vec<u8>> {
    bus.transaction(ops)
        .map_err(|failure| map_failure(device_id, operation, failure))
}

fn check_len(device_id: &DeviceId, operation: &'static str, length: usize) -> BusResult<()> {
    if length > u32::MAX as usize {
        return Err(invalid_i2c_request(
            device_id,
            operation,
            "transfer is too long",
        ));
    }
    Ok(())
}

/// The ops of a `transaction`: each operation as a segment with a repeated
/// START, and one STOP. Reads return their bytes in order.
fn transaction_ops(address: u8, operations: &[I2cOperation]) -> (Vec<Op>, Vec<usize>) {
    let mut ops = Vec::new();
    let mut lengths = Vec::new();
    for operation in operations {
        match operation {
            I2cOperation::Write { bytes } => {
                ops.push(Op::Start);
                ops.push(Op::Write(address_byte(address, false)));
                ops.extend(bytes.iter().copied().map(Op::Write));
            }
            I2cOperation::Read { length } => {
                let length = *length as usize;
                if length == 0 {
                    continue;
                }
                ops.push(Op::Start);
                ops.push(Op::Write(address_byte(address, true)));
                ops.extend((0..length).map(|i| Op::Read {
                    ack: i + 1 < length,
                }));
                lengths.push(length);
            }
        }
    }
    ops.push(Op::Stop);
    (ops, lengths)
}

/// Splits the concatenated read bytes of a transaction by segment.
fn split_reads(data: Vec<u8>, lengths: &[usize]) -> Vec<Vec<u8>> {
    let mut rest = data.as_slice();
    lengths
        .iter()
        .map(|&len| {
            let (head, tail) = rest.split_at(len.min(rest.len()));
            rest = tail;
            head.to_vec()
        })
        .collect()
}

/// A device session's transport on a PIO bus: the target's 7-bit address.
pub(crate) struct PioI2cTransport {
    device_id: DeviceId,
    bus: Arc<PioBus>,
    address: u8,
}

/// The transport for `device` (at `address` on virtual bus `bus`).
pub(crate) fn device_transport(
    device: &DeviceDescriptor,
    bus: u32,
    address: u16,
) -> BusResult<PioI2cTransport> {
    if address > 0x7f {
        return Err(invalid_i2c_request(
            &device.id,
            "i2c.open",
            format!("address {address:#x} is not a 7-bit address"),
        ));
    }
    Ok(PioI2cTransport {
        device_id: device.id.clone(),
        bus: bus_for(&device.id, bus)?,
        address: address as u8,
    })
}

impl I2cTransport for PioI2cTransport {
    fn read_into(&mut self, buffer: &mut [u8]) -> BusResult<()> {
        let bytes = self.read(buffer.len() as u32)?;
        buffer.copy_from_slice(&bytes);
        Ok(())
    }

    fn read(&mut self, length: u32) -> BusResult<Vec<u8>> {
        let ops = protocol::write_read_ops(self.address, &[], length as usize);
        run(&self.bus, &self.device_id, "i2c.read", &ops)
    }

    fn write(&mut self, bytes: &[u8]) -> BusResult<()> {
        check_len(&self.device_id, "i2c.write", bytes.len())?;
        let ops = protocol::write_read_ops(self.address, bytes, 0);
        run(&self.bus, &self.device_id, "i2c.write", &ops).map(|_| ())
    }

    fn write_read_into(&mut self, write: &[u8], read: &mut [u8]) -> BusResult<()> {
        let bytes = self.write_read(write, read.len() as u32)?;
        read.copy_from_slice(&bytes);
        Ok(())
    }

    fn write_read(&mut self, write: &[u8], read_length: u32) -> BusResult<Vec<u8>> {
        check_len(&self.device_id, "i2c.write_read", write.len())?;
        let ops = protocol::write_read_ops(self.address, write, read_length as usize);
        run(&self.bus, &self.device_id, "i2c.write_read", &ops)
    }

    fn transaction(&mut self, operations: &[I2cOperation]) -> BusResult<Vec<Vec<u8>>> {
        let (ops, lengths) = transaction_ops(self.address, operations);
        let data = run(&self.bus, &self.device_id, "i2c.transaction", &ops)?;
        Ok(split_reads(data, &lengths))
    }
}

/// The controller transport for raw access on a PIO bus: any 7-bit target.
pub(crate) struct PioI2cControllerTransport {
    owner_id: DeviceId,
    bus: Arc<PioBus>,
}

/// The controller transport on virtual bus `bus`, opened for `owner`.
pub(crate) fn controller_transport(
    owner: &DeviceDescriptor,
    bus: u32,
) -> BusResult<PioI2cControllerTransport> {
    Ok(PioI2cControllerTransport {
        owner_id: owner.id.clone(),
        bus: bus_for(&owner.id, bus)?,
    })
}

fn check_target(device_id: &DeviceId, operation: &'static str, address: u16) -> BusResult<u8> {
    if address > 0x7f {
        return Err(invalid_i2c_request(
            device_id,
            operation,
            format!("address {address:#x} is not a 7-bit address"),
        ));
    }
    Ok(address as u8)
}

impl I2cControllerTransport for PioI2cControllerTransport {
    fn read_into(&mut self, address: u16, buffer: &mut [u8]) -> BusResult<()> {
        let bytes = self.read(address, buffer.len() as u32)?;
        buffer.copy_from_slice(&bytes);
        Ok(())
    }

    fn read(&mut self, address: u16, length: u32) -> BusResult<Vec<u8>> {
        let address = check_target(&self.owner_id, "i2c.read", address)?;
        let ops = protocol::write_read_ops(address, &[], length as usize);
        run(&self.bus, &self.owner_id, "i2c.read", &ops)
    }

    fn write(&mut self, address: u16, bytes: &[u8]) -> BusResult<()> {
        let address = check_target(&self.owner_id, "i2c.write", address)?;
        check_len(&self.owner_id, "i2c.write", bytes.len())?;
        let ops = protocol::write_read_ops(address, bytes, 0);
        run(&self.bus, &self.owner_id, "i2c.write", &ops).map(|_| ())
    }

    fn write_read_into(&mut self, address: u16, write: &[u8], read: &mut [u8]) -> BusResult<()> {
        let bytes = self.write_read(address, write, read.len() as u32)?;
        read.copy_from_slice(&bytes);
        Ok(())
    }

    fn write_read(&mut self, address: u16, write: &[u8], read_length: u32) -> BusResult<Vec<u8>> {
        let address = check_target(&self.owner_id, "i2c.write_read", address)?;
        check_len(&self.owner_id, "i2c.write_read", write.len())?;
        let ops = protocol::write_read_ops(address, write, read_length as usize);
        run(&self.bus, &self.owner_id, "i2c.write_read", &ops)
    }

    fn transaction(
        &mut self,
        address: u16,
        operations: &[I2cOperation],
    ) -> BusResult<Vec<Vec<u8>>> {
        let address = check_target(&self.owner_id, "i2c.transaction", address)?;
        let (ops, lengths) = transaction_ops(address, operations);
        let data = run(&self.bus, &self.owner_id, "i2c.transaction", &ops)?;
        Ok(split_reads(data, &lengths))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transaction_segments_split_the_reads() {
        let ops = transaction_ops(
            0x18,
            &[
                I2cOperation::Write { bytes: vec![0x12] },
                I2cOperation::Read { length: 2 },
                I2cOperation::Read { length: 1 },
            ],
        );
        assert_eq!(ops.1, vec![2, 1]);
        assert_eq!(*ops.0.last().unwrap(), Op::Stop);
        assert_eq!(
            split_reads(vec![1, 2, 3], &ops.1),
            vec![vec![1, 2], vec![3]]
        );
    }

    #[test]
    fn map_failure_names_the_nacked_byte() {
        let id = DeviceId::new("imu").unwrap();
        let error = map_failure(
            &id,
            "i2c.read",
            Failure::Nack(protocol::Nack { op: 1, byte: 0x30 }),
        );
        assert!(error.to_string().contains("0x30"), "{error}");
        let timeout = map_failure(&id, "i2c.read", Failure::Timeout);
        assert!(matches!(timeout, BusError::Timeout { .. }));
    }

    #[test]
    fn segments_merge_same_direction_and_nack_the_last_read_byte() {
        // write, write, read 2, read 1 -> one write, one read run (the last byte NACKed).
        let ops = segment_ops(
            0x18,
            &[
                Segment::Write(&[0x12]),
                Segment::Write(&[0x13]),
                Segment::Read(2),
                Segment::Read(1),
            ],
        );
        assert_eq!(
            ops,
            vec![
                Op::Start,
                Op::Write(0x30),
                Op::Write(0x12),
                Op::Write(0x13),
                Op::Start,
                Op::Write(0x31),
                Op::Read { ack: true },
                Op::Read { ack: true },
                Op::Read { ack: false },
                Op::Stop,
            ]
        );
        // A read run followed by a write closes with a NACK before the restart.
        let ops = segment_ops(0x18, &[Segment::Read(1), Segment::Write(&[1])]);
        assert_eq!(ops[2], Op::Read { ack: false });
        assert!(segment_ops(0x18, &[Segment::Read(0)]).is_empty());
    }
}
