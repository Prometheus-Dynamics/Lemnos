//! Service messages on the wire.

use super::codec::{Code, Decoder, Encoder};
use super::*;
use lemnos_device::{Axis, DeviceClass, DeviceStatus, Quantity};
use lemnos_hal::ErrorKind;

/// The frame for a device reading: what `Message::Reading` encodes to, built
/// from its parts so a sender need not own the strings and values. One
/// allocation, sized to the frame.
pub fn encode_reading(
    device: &str,
    timestamp_us: u64,
    status: DeviceStatus,
    values: &[i32],
) -> Vec<u8> {
    let mut frame = Vec::new();
    encode_reading_into(&mut frame, device, timestamp_us, status, values);
    frame
}

/// [`encode_reading`] into `frame`, replacing what it held: no allocation once `frame` has
/// room, so a sender that keeps its frames encodes readings without allocating.
pub fn encode_reading_into(
    frame: &mut Vec<u8>,
    device: &str,
    timestamp_us: u64,
    status: DeviceStatus,
    values: &[i32],
) {
    let device_len = device.len().min(u16::MAX as usize);
    // Frame length, kind, device (length and bytes), timestamp, status, count, values.
    let size = 6 + 2 + device_len + 8 + 1 + 2 + 4 * values.len();
    let mut e = Encoder::reuse(READING, std::mem::take(frame), size);
    e.str(device)
        .u64(timestamp_us)
        .u8(status.code())
        .u16(values.len() as u16);
    for v in values {
        e.i32(*v);
    }
    *frame = e.finish();
}

impl Message {
    /// The frame for this message.
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Frame(frame) => {
                let mut e = Encoder::new(FRAME);
                e.str(&frame.device)
                    .u32(frame.seq)
                    .u16(frame.pixels.len().min(u16::MAX as usize) as u16);
                for pixel in frame.pixels.iter().take(u16::MAX as usize) {
                    e.u32(*pixel);
                }
                e.finish()
            }
            Self::LightInfo(info) => {
                let mut e = Encoder::new(LIGHT_INFO);
                e.str(&info.device)
                    .u16(info.count)
                    .u16(info.offset)
                    .u8(u8::from(info.clockwise))
                    .str(&info.look)
                    .str(&info.layer)
                    .str(&info.owner)
                    .u16(info.look_brightness);
                e.finish()
            }
            Self::CalibrationStatus { id, device, status } => {
                let mut e = Encoder::new(CALIBRATION_STATUS_REPLY);
                e.u32(*id).str(device);
                super::calibration::status_encode(&mut e, status);
                e.finish()
            }
            Self::Text { id, result } => {
                let mut e = Encoder::new(TEXT);
                e.u32(*id);
                match result {
                    Ok(text) => e.u8(0).str(text),
                    Err(text) => e.u8(1).str(text),
                };
                e.finish()
            }
            Self::Welcome {
                version,
                board,
                client_id,
                features,
            } => {
                let mut e = Encoder::new(WELCOME);
                e.u16(*version).str(board).u32(*client_id).u32(*features);
                e.finish()
            }
            Self::GpuLease { id, lease, state } => {
                let mut e = Encoder::new(GPU_LEASE_REPLY);
                e.u32(*id).u32(*lease);
                super::gpu::state_encode(&mut e, *state);
                e.finish()
            }
            Self::GpuState { id, gpus } => {
                let mut e = Encoder::new(GPU_STATE_REPLY);
                e.u32(*id);
                super::gpu::gpus_encode(&mut e, gpus);
                e.finish()
            }
            Self::Stats(stats) => {
                let mut e = Encoder::new(STATS_REPLY);
                e.u16(stats.len() as u16);
                for s in stats {
                    e.str(&s.device)
                        .u64(s.reads)
                        .u64(s.total_us)
                        .u64(s.max_us)
                        .u64(s.last_us)
                        .u64(s.misses);
                }
                e.finish()
            }
            Self::Devices(devices) => {
                let mut e = Encoder::new(DEVICES);
                e.u16(devices.len() as u16);
                for d in devices {
                    e.str(&d.id)
                        .str(&d.label)
                        .u8(d.class.code())
                        .str(&d.model)
                        .u8(d.status.code())
                        .u16(d.pixels)
                        .u16(d.channels.len() as u16);
                    for c in &d.channels {
                        e.str(&c.name)
                            .u8(c.quantity.code())
                            .u8(c.axis.code())
                            .i8(c.exponent);
                    }
                    e.u16(d.controls.len() as u16);
                    for c in &d.controls {
                        e.str(&c.name)
                            .u8(c.quantity.code())
                            .i8(c.exponent)
                            .i32(c.min)
                            .i32(c.max);
                    }
                }
                // Reasons follow the whole list, so older clients (which
                // stop after it) still read it.
                e.u16(devices.len() as u16);
                for d in devices {
                    e.str(&d.reason);
                }
                e.finish()
            }
            Self::Reading(r) => {
                let mut frame = encode_reading(&r.device, r.timestamp_us, r.status, &r.values);
                if r.id != 0 {
                    // Appended within protocol 1, only for an answer to a read with an id.
                    frame.extend_from_slice(&r.id.to_le_bytes());
                    let length = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]) + 4;
                    frame[..4].copy_from_slice(&length.to_le_bytes());
                }
                frame
            }
            Self::Reply { id, result } => {
                let mut e = Encoder::new(REPLY);
                e.u32(*id);
                match result {
                    Ok(value) => e.u8(0).u8(0).f64(*value),
                    Err(refusal) => {
                        let (code, kind) = refusal.code();
                        e.u8(code).u8(kind).f64(0.0)
                    }
                };
                e.finish()
            }
            Self::Event(event) => {
                let mut e = Encoder::new(EVENT);
                match event {
                    Event::Status {
                        device,
                        status,
                        error,
                        reason,
                    } => {
                        e.u8(0)
                            .str(device)
                            .u8(status.code())
                            .u8(error.map_or(255, |k| k.code()))
                            .str(reason);
                    }
                    Event::Control {
                        device,
                        control,
                        value,
                        by,
                    } => {
                        e.u8(1).str(device).str(control).f64(*value).str(by);
                    }
                    Event::LedOwner {
                        device,
                        owner,
                        layer,
                    } => {
                        e.u8(2).str(device).str(owner).str(layer);
                    }
                    Event::Edge {
                        handle,
                        rising,
                        timestamp_ns,
                        seq,
                    } => {
                        e.u8(3)
                            .u32(*handle)
                            .u8(u8::from(*rising))
                            .u64(*timestamp_ns)
                            .u32(*seq);
                    }
                    Event::Dropped { count } => {
                        e.u8(4).u32(*count);
                    }
                }
                e.finish()
            }
            Self::Claimed { id, result } => {
                let mut e = Encoder::new(CLAIMED);
                e.u32(*id);
                match result {
                    Ok(handle) => e.u8(0).u8(0).u32(*handle),
                    Err(refusal) => {
                        let (code, kind) = refusal.code();
                        e.u8(code).u8(kind).u32(0)
                    }
                };
                e.finish()
            }
            Self::Data { id, result } => {
                let mut e = Encoder::new(DATA);
                e.u32(*id);
                match result {
                    Ok(bytes) => e.u8(0).u8(0).bytes(bytes),
                    Err(refusal) => {
                        let (code, kind) = refusal.code();
                        e.u8(code).u8(kind).bytes(&[])
                    }
                };
                e.finish()
            }
        }
    }

    pub(crate) fn decode(kind: u16, payload: &[u8]) -> Result<Self, WireError> {
        let mut d = Decoder { buf: payload };
        Ok(match kind {
            WELCOME => Self::Welcome {
                version: d.u16()?,
                board: d.str()?,
                client_id: d.u32()?,
                // Appended later in protocol 1: older services send none.
                features: if d.is_empty() { 0 } else { d.u32()? },
            },
            GPU_LEASE_REPLY => Self::GpuLease {
                id: d.u32()?,
                lease: d.u32()?,
                state: super::gpu::state_decode(&mut d)?,
            },
            GPU_STATE_REPLY => Self::GpuState {
                id: d.u32()?,
                gpus: super::gpu::gpus_decode(&mut d)?,
            },
            DEVICES => {
                let n = d.u16()?;
                let mut devices = Vec::with_capacity(usize::from(n));
                for _ in 0..n {
                    let id = d.str()?;
                    let label = d.str()?;
                    let class = DeviceClass::from_code(d.u8()?);
                    let model = d.str()?;
                    let status = DeviceStatus::from_code(d.u8()?);
                    let pixels = d.u16()?;
                    let mut channels = Vec::new();
                    for _ in 0..d.u16()? {
                        channels.push(ChannelDesc {
                            name: d.str()?,
                            quantity: Quantity::from_code(d.u8()?),
                            axis: Axis::from_code(d.u8()?),
                            exponent: d.i8()?,
                        });
                    }
                    let mut controls = Vec::new();
                    for _ in 0..d.u16()? {
                        controls.push(ControlDesc {
                            name: d.str()?,
                            quantity: Quantity::from_code(d.u8()?),
                            exponent: d.i8()?,
                            min: d.i32()?,
                            max: d.i32()?,
                        });
                    }
                    devices.push(DeviceDesc {
                        id,
                        label,
                        class,
                        model,
                        status,
                        reason: String::new(),
                        channels,
                        controls,
                        pixels,
                    });
                }
                if !d.is_empty() {
                    if usize::from(d.u16()?) != devices.len() {
                        return Err(bad("device reason count"));
                    }
                    for device in &mut devices {
                        device.reason = d.str()?;
                    }
                }
                Self::Devices(devices)
            }
            STATS_REPLY => {
                let count = usize::from(d.u16()?);
                let mut stats = Vec::with_capacity(count.min(64));
                for _ in 0..count {
                    stats.push(ReadStats {
                        device: d.str()?,
                        reads: d.u64()?,
                        total_us: d.u64()?,
                        max_us: d.u64()?,
                        last_us: d.u64()?,
                        misses: d.u64()?,
                    });
                }
                Self::Stats(stats)
            }
            READING => {
                let device = d.str()?;
                let timestamp_us = d.u64()?;
                let status = DeviceStatus::from_code(d.u8()?);
                let n = d.u16()?;
                let mut values = Vec::with_capacity(usize::from(n));
                for _ in 0..n {
                    values.push(d.i32()?);
                }
                let id = if d.is_empty() { 0 } else { d.u32()? };
                Self::Reading(RawReading {
                    device,
                    timestamp_us,
                    status,
                    values,
                    id,
                })
            }
            REPLY => {
                let id = d.u32()?;
                let code = d.u8()?;
                let kind = d.u8()?;
                let value = d.f64()?;
                Self::Reply {
                    id,
                    result: if code == 0 {
                        Ok(value)
                    } else {
                        Err(Refusal::from_code(code, kind))
                    },
                }
            }
            EVENT => Self::Event(match d.u8()? {
                0 => Event::Status {
                    device: d.str()?,
                    status: DeviceStatus::from_code(d.u8()?),
                    error: Some(d.u8()?)
                        .filter(|k| *k != 255)
                        .map(ErrorKind::from_code),
                    reason: if d.is_empty() {
                        String::new()
                    } else {
                        d.str()?
                    },
                },
                1 => Event::Control {
                    device: d.str()?,
                    control: d.str()?,
                    value: d.f64()?,
                    by: d.str()?,
                },
                2 => Event::LedOwner {
                    device: d.str()?,
                    owner: d.str()?,
                    layer: d.str()?,
                },
                3 => Event::Edge {
                    handle: d.u32()?,
                    rising: d.u8()? != 0,
                    timestamp_ns: d.u64()?,
                    seq: d.u32()?,
                },
                4 => Event::Dropped { count: d.u32()? },
                other => return Err(bad(format!("unknown event {other}"))),
            }),
            CLAIMED => {
                let id = d.u32()?;
                let (code, kind) = (d.u8()?, d.u8()?);
                let handle = d.u32()?;
                Self::Claimed {
                    id,
                    result: if code == 0 {
                        Ok(handle)
                    } else {
                        Err(Refusal::from_code(code, kind))
                    },
                }
            }
            FRAME => {
                let device = d.str()?;
                let seq = d.u32()?;
                let n = d.u16()?;
                let mut pixels = Vec::with_capacity(usize::from(n));
                for _ in 0..n {
                    pixels.push(d.u32()?);
                }
                Self::Frame(LightFrame {
                    device,
                    seq,
                    pixels,
                })
            }
            LIGHT_INFO => Self::LightInfo(LightInfo {
                device: d.str()?,
                count: d.u16()?,
                offset: d.u16()?,
                clockwise: d.u8()? != 0,
                look: d.str()?,
                layer: d.str()?,
                owner: d.str()?,
                // Appended later: an older service sends none (full brightness).
                look_brightness: if d.is_empty() { 1_000 } else { d.u16()? },
            }),
            TEXT => {
                let id = d.u32()?;
                let code = d.u8()?;
                let text = d.str()?;
                Self::Text {
                    id,
                    result: if code == 0 { Ok(text) } else { Err(text) },
                }
            }
            DATA => {
                let id = d.u32()?;
                let (code, kind) = (d.u8()?, d.u8()?);
                let bytes = d.bytes()?;
                Self::Data {
                    id,
                    result: if code == 0 {
                        Ok(bytes)
                    } else {
                        Err(Refusal::from_code(code, kind))
                    },
                }
            }
            CALIBRATION_STATUS_REPLY => Self::CalibrationStatus {
                id: d.u32()?,
                device: d.str()?,
                status: super::calibration::status_decode(&mut d)?,
            },
            other => return Err(bad(format!("unknown message kind {other}"))),
        })
    }
}
