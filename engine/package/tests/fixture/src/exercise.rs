//! What `exercise` does with each kind of resource. Every step is a call on the resource as
//! any driver makes it; what comes back goes out as plain bytes for the test to read.
//!
//! Numbers go in and come out little-endian. A step's arguments are in `data` in the order the
//! call takes them.

use std::os::unix::fs::FileExt;
use std::time::Duration;

use super::*;

const WAIT: Duration = Duration::from_millis(5);

/// A resource's error as the status the caller sees: each kind of error has its own.
fn failed(kind: ErrorKind) -> Status {
    status_of_bus(kind)
}

fn missing() -> Status {
    Status::NOT_FOUND
}

/// The `N` bytes at `at` in `data`.
fn bytes<const N: usize>(data: &[u8], at: usize) -> Result<[u8; N]> {
    data.get(at..at + N)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(Status::INVALID_ARGUMENT)
}

fn u32_at(data: &[u8], at: usize) -> Result<u32> {
    bytes(data, at).map(u32::from_le_bytes)
}

fn u64_at(data: &[u8], at: usize) -> Result<u64> {
    bytes(data, at).map(u64::from_le_bytes)
}

fn byte(data: &[u8], at: usize) -> Result<u8> {
    data.get(at).copied().ok_or(Status::INVALID_ARGUMENT)
}

/// Writes `value` to the start of `into` and returns its length.
fn put(into: &mut [u8], value: &[u8]) -> Result<usize> {
    into.get_mut(..value.len())
        .ok_or(Status::BUFFER_TOO_SMALL)?
        .copy_from_slice(value);
    Ok(value.len())
}

fn room(into: &mut [u8], len: usize) -> Result<&mut [u8]> {
    into.get_mut(..len).ok_or(Status::BUFFER_TOO_SMALL)
}

fn setup(data: &[u8]) -> Result<ControlSetup> {
    Ok(ControlSetup {
        request_type: byte(data, 0)?,
        request: byte(data, 1)?,
        value: u16::from_le_bytes(bytes(data, 2)?),
        index: u16::from_le_bytes(bytes(data, 4)?),
    })
}

impl Fixture {
    pub(crate) fn run(
        &mut self,
        kind: u8,
        step: u8,
        data: &[u8],
        into: &mut [u8],
    ) -> Result<usize> {
        match kind {
            kind::SPI => self.spi(step, data, into),
            kind::RAW_SPI => self.raw_spi(data, into),
            kind::LINE => self.line(step, data, into),
            kind::PWM => self.pwm(step, data, into),
            kind::UART => self.uart(step, data, into),
            kind::REGISTERS => self.registers(step, data, into),
            kind::INTERRUPT => self.interrupt(step, into),
            kind::DMA => self.dma(step, data, into),
            kind::CLOCKS => self.clocks(step, data, into),
            kind::RESET => self.reset(step, data),
            kind::STREAM => self.stream(step, data, into),
            kind::USB_DEVICE => self.usb(step, data, into),
            kind::USB_FUNCTION => self.function(step, data, into),
            _ => Err(Status::INVALID_ARGUMENT),
        }
    }

    /// 0: writes `data`, then reads 2 bytes. 1: sends `data` while as many bytes come back.
    /// 2: sends `data` and takes what came back in its place, then waits a microsecond.
    fn spi(&mut self, step: u8, data: &[u8], into: &mut [u8]) -> Result<usize> {
        let spi = self.spi.as_mut().ok_or_else(missing)?;
        match step {
            0 => {
                let read = room(into, 2)?;
                spi.transaction(&mut [Operation::Write(data), Operation::Read(read)])
                    .map_err(failed)?;
                Ok(2)
            }
            1 => {
                let read = room(into, data.len())?;
                spi.transaction(&mut [Operation::Transfer(read, data)])
                    .map_err(failed)?;
                Ok(data.len())
            }
            2 => {
                let len = put(into, data)?;
                spi.transaction(&mut [
                    Operation::TransferInPlace(&mut into[..len]),
                    Operation::DelayNs(1000),
                ])
                .map_err(failed)?;
                Ok(len)
            }
            _ => Err(Status::INVALID_ARGUMENT),
        }
    }

    /// Two segments: the first sends `data[1..]` in mode `data[0]` at 1 MHz while 2 bytes come
    /// back, then deselects and waits 7 us; the second reads 2 more with the device's settings.
    fn raw_spi(&mut self, data: &[u8], into: &mut [u8]) -> Result<usize> {
        let spi = self.raw_spi.as_mut().ok_or_else(missing)?;
        let (first, second) = room(into, 4)?.split_at_mut(2);
        let mut segments = [
            SpiSegment {
                config: SpiConfig {
                    mode: SpiMode::from_bits(byte(data, 0)?),
                    speed_hz: 1_000_000,
                    bits_per_word: 8,
                },
                tx: &data[1..],
                rx: first,
                cs_change: true,
                delay_us: 7,
            },
            SpiSegment {
                config: SpiConfig::default(),
                tx: &[],
                rx: second,
                cs_change: false,
                delay_us: 0,
            },
        ];
        spi.transfer(&mut segments).map_err(failed)?;
        Ok(4)
    }

    /// 0: an output starting at `data[0]`. 1: an active-low input, pulled up, debounced 250 us,
    /// reporting both edges. 2: the value. 3: sets `data[0]`. 4: the next edge as present,
    /// rising, sequence (4), timestamp (8).
    fn line(&mut self, step: u8, data: &[u8], into: &mut [u8]) -> Result<usize> {
        let line = self.line.as_mut().ok_or_else(missing)?;
        match step {
            0 => {
                let config = LineConfig::output(byte(data, 0)? != 0);
                line.configure(&config).map_err(failed)?;
                Ok(0)
            }
            1 => {
                let config = LineConfig::input()
                    .active_low()
                    .with_bias(Bias::PullUp)
                    .with_edge(EdgeDetect::Both)
                    .with_debounce_us(250);
                line.configure(&config).map_err(failed)?;
                Ok(0)
            }
            2 => put(into, &[u8::from(line.get().map_err(failed)?)]),
            3 => {
                line.set(byte(data, 0)? != 0).map_err(failed)?;
                Ok(0)
            }
            4 => match line.read_edge().map_err(failed)? {
                None => put(into, &[0]),
                Some(edge) => {
                    put(into, &[1, u8::from(edge.rising)])?;
                    put(&mut into[2..], &edge.seq.to_le_bytes())?;
                    put(&mut into[6..], &edge.timestamp_ns.to_le_bytes())?;
                    Ok(14)
                }
            },
            _ => Err(Status::INVALID_ARGUMENT),
        }
    }

    /// 0: applies period (8), duty (8), inversed, enabled. 1: the settings, in the same form.
    fn pwm(&mut self, step: u8, data: &[u8], into: &mut [u8]) -> Result<usize> {
        let pwm = self.pwm.as_mut().ok_or_else(missing)?;
        match step {
            0 => {
                let config = PwmConfig {
                    period_ns: u64_at(data, 0)?,
                    duty_ns: u64_at(data, 8)?,
                    polarity: if byte(data, 16)? != 0 {
                        Polarity::Inversed
                    } else {
                        Polarity::Normal
                    },
                    enabled: byte(data, 17)? != 0,
                };
                pwm.configure(&config).map_err(failed)?;
                Ok(0)
            }
            1 => {
                let config = pwm.config().map_err(failed)?;
                put(into, &config.period_ns.to_le_bytes())?;
                put(&mut into[8..], &config.duty_ns.to_le_bytes())?;
                put(
                    &mut into[16..],
                    &[
                        u8::from(config.polarity == Polarity::Inversed),
                        u8::from(config.enabled),
                    ],
                )?;
                Ok(18)
            }
            _ => Err(Status::INVALID_ARGUMENT),
        }
    }

    /// 0: sets the baud rate (4). 1: sends `data`, answers how many went (8). 2: what arrived.
    fn uart(&mut self, step: u8, data: &[u8], into: &mut [u8]) -> Result<usize> {
        let uart = self.uart.as_mut().ok_or_else(missing)?;
        match step {
            0 => {
                uart.set_baud(u32_at(data, 0)?).map_err(failed)?;
                Ok(0)
            }
            1 => {
                let sent = uart.write(data).map_err(failed)?;
                put(into, &(sent as u64).to_le_bytes())
            }
            2 => uart.read(into, WAIT).map_err(failed),
            _ => Err(Status::INVALID_ARGUMENT),
        }
    }

    /// 0: the window's size (8). 1: reads offset (4). 2: writes offset, value. 3: modifies
    /// offset, clear, set. 4: writes the values after offset as a block.
    fn registers(&mut self, step: u8, data: &[u8], into: &mut [u8]) -> Result<usize> {
        let registers = self.registers.as_mut().ok_or_else(missing)?;
        if step == 0 {
            return put(into, &(registers.size() as u64).to_le_bytes());
        }
        let offset = u32_at(data, 0)? as usize;
        match step {
            1 => {
                let value = registers.read32(offset).map_err(failed)?;
                put(into, &value.to_le_bytes())
            }
            2 => {
                registers
                    .write32(offset, u32_at(data, 4)?)
                    .map_err(failed)?;
                Ok(0)
            }
            3 => {
                registers
                    .modify32(offset, u32_at(data, 4)?, u32_at(data, 8)?)
                    .map_err(failed)?;
                Ok(0)
            }
            4 => {
                let mut values = [0u32; 8];
                let count = (data.len() - 4) / 4;
                let values = values.get_mut(..count).ok_or(Status::INVALID_ARGUMENT)?;
                for (at, value) in values.iter_mut().enumerate() {
                    *value = u32_at(data, 4 + 4 * at)?;
                }
                registers.write_block(offset, values).map_err(failed)?;
                Ok(0)
            }
            _ => Err(Status::INVALID_ARGUMENT),
        }
    }

    /// 0: enables. 1: disables. 2: waits 5 ms, 3: waits without limit; both answer present,
    /// count (4), timestamp (8).
    fn interrupt(&mut self, step: u8, into: &mut [u8]) -> Result<usize> {
        let interrupt = self.interrupt.as_mut().ok_or_else(missing)?;
        let timeout = match step {
            0 => return interrupt.enable().map(|()| 0).map_err(failed),
            1 => return interrupt.disable().map(|()| 0).map_err(failed),
            2 => Some(WAIT),
            3 => None,
            _ => return Err(Status::INVALID_ARGUMENT),
        };
        match interrupt.wait(timeout).map_err(failed)? {
            None => put(into, &[0]),
            Some(event) => {
                put(into, &[1])?;
                put(&mut into[1..], &event.count.to_le_bytes())?;
                put(&mut into[5..], &event.timestamp_ns.to_le_bytes())?;
                Ok(13)
            }
        }
    }

    /// 0: allocates length (8), answers id (4), length (8), address (8), and keeps the id.
    /// 1: writes `data[8..]` at offset (8). 2: reads length (4) at offset (8). 3: hands the
    /// buffer to the block. 4: takes it back. 5: releases it.
    fn dma(&mut self, step: u8, data: &[u8], into: &mut [u8]) -> Result<usize> {
        let dma = self.dma.as_mut().ok_or_else(missing)?;
        let id = self.dma_buffer;
        match step {
            0 => {
                let buffer = dma.allocate(u64_at(data, 0)?).map_err(failed)?;
                self.dma_buffer = buffer.id;
                put(into, &buffer.id.to_le_bytes())?;
                put(&mut into[4..], &buffer.len.to_le_bytes())?;
                put(&mut into[12..], &buffer.device_address.to_le_bytes())?;
                Ok(20)
            }
            1 => {
                dma.write(id, u64_at(data, 0)?, &data[8..])
                    .map_err(failed)?;
                Ok(0)
            }
            2 => {
                let len = u32_at(data, 8)? as usize;
                dma.read(id, u64_at(data, 0)?, room(into, len)?)
                    .map_err(failed)?;
                Ok(len)
            }
            3 => dma.sync_for_device(id).map(|()| 0).map_err(failed),
            4 => dma.sync_for_cpu(id).map(|()| 0).map_err(failed),
            5 => dma.release(id).map(|()| 0).map_err(failed),
            _ => Err(Status::INVALID_ARGUMENT),
        }
    }

    /// The clock is `data[..4]`. 0: enables. 1: disables. 2: its rate (8). 3: asks for a rate
    /// (8) and answers the one it settled on (8).
    fn clocks(&mut self, step: u8, data: &[u8], into: &mut [u8]) -> Result<usize> {
        let clocks = self.clocks.as_mut().ok_or_else(missing)?;
        let clock = u32_at(data, 0)?;
        match step {
            0 => clocks.enable(clock).map(|()| 0).map_err(failed),
            1 => clocks.disable(clock).map(|()| 0).map_err(failed),
            2 => put(into, &clocks.rate_hz(clock).map_err(failed)?.to_le_bytes()),
            3 => {
                let settled = clocks
                    .set_rate_hz(clock, u64_at(data, 4)?)
                    .map_err(failed)?;
                put(into, &settled.to_le_bytes())
            }
            _ => Err(Status::INVALID_ARGUMENT),
        }
    }

    /// The line is `data[..4]`. 0: asserts. 1: deasserts.
    fn reset(&mut self, step: u8, data: &[u8]) -> Result<usize> {
        let reset = self.reset.as_mut().ok_or_else(missing)?;
        let line = u32_at(data, 0)?;
        match step {
            0 => reset.assert(line).map(|()| 0).map_err(failed),
            1 => reset.deassert(line).map(|()| 0).map_err(failed),
            _ => Err(Status::INVALID_ARGUMENT),
        }
    }

    /// 0: how many buffers (4). 1: takes the next buffer, answers present, buffer (4), and
    /// keeps it. 2: writes `data[1..]` into it at `data[0]`, answers whether a consumer is
    /// attached. 3: reads `data[1]` bytes of it at `data[0]`, answers attached, then the bytes.
    /// 4: publishes it with `data[0]` bytes, sequence `data[1]`. 5: the next job, as present,
    /// then its bytes. 6: asks for the buffer's handle and answers present, then the first
    /// `data[0]` bytes read through the descriptor. 7: whether the buffer was taken under the
    /// consumer attached now. 8: whether the buffers are behind handles. 9: faults the consumer.
    fn stream(&mut self, step: u8, data: &[u8], into: &mut [u8]) -> Result<usize> {
        let stream = self.stream.as_mut().ok_or_else(missing)?;
        let buffer = self.stream_buffer;
        match step {
            0 => put(into, &stream.buffer_count().to_le_bytes()),
            1 => match stream.next_buffer().map_err(failed)? {
                None => put(into, &[0]),
                Some(buffer) => {
                    self.stream_buffer = buffer;
                    put(into, &[1])?;
                    put(&mut into[1..], &buffer.to_le_bytes())?;
                    Ok(5)
                }
            },
            2 => {
                let attached = stream
                    .write(buffer, usize::from(byte(data, 0)?), &data[1..])
                    .map_err(failed)?;
                put(into, &[u8::from(attached)])
            }
            3 => {
                let len = usize::from(byte(data, 1)?);
                let (flag, out) = room(into, 1 + len)?.split_at_mut(1);
                let attached = stream
                    .read(buffer, usize::from(byte(data, 0)?), out)
                    .map_err(failed)?;
                flag[0] = u8::from(attached);
                Ok(1 + len)
            }
            4 => {
                let frame = StreamFrame {
                    buffer,
                    bytes: u32::from(byte(data, 0)?),
                    sequence: u64::from(byte(data, 1)?),
                    identity: 0xfeed,
                    timestamp_ns: 99,
                };
                stream.publish(frame).map(|()| 0).map_err(failed)
            }
            5 => {
                let (flag, out) = into.split_at_mut(1);
                match stream.next_job(out).map_err(failed)? {
                    None => {
                        flag[0] = 0;
                        Ok(1)
                    }
                    Some(len) => {
                        flag[0] = 1;
                        Ok(1 + len)
                    }
                }
            }
            6 => {
                let len = usize::from(byte(data, 0)?);
                let Some(handle) = stream.buffer_handle(buffer).map_err(failed)? else {
                    return put(into, &[0]);
                };
                // A hold of this driver's own on the memory, as hardware would be given.
                let file = handle
                    .fd()
                    .try_clone_to_owned()
                    .map(std::fs::File::from)
                    .map_err(|error| Status::from_io_error(&error))?;
                let (flag, out) = room(into, 1 + len)?.split_at_mut(1);
                file.read_exact_at(out, 0)
                    .map_err(|error| Status::from_io_error(&error))?;
                flag[0] = 1;
                Ok(1 + len)
            }
            7 => {
                let current = stream.buffer_current(buffer).map_err(failed)?;
                put(into, &[u8::from(current)])
            }
            8 => put(
                into,
                &[u8::from(stream.buffer_kind() == BufferKind::Handles)],
            ),
            9 => stream.fault_consumer().map(|()| 0).map_err(failed),
            _ => Err(Status::INVALID_ARGUMENT),
        }
    }

    /// 0: the configuration descriptor. 1: claims interface `data[0]`. 2: releases it. 3: sets
    /// alternate `data[1]` on it. 4: detaches its kernel driver. 5: resets the device.
    /// 6: control in, with the setup in `data[..6]`. 7: control out, the same, sending
    /// `data[6..]`, answers how many went (8). 8: bulk in from endpoint `data[0]`. 9: bulk out
    /// of `data[1..]` to it (8). 10: interrupt in from it. 11: starts a stream of two
    /// transfers of three 8-byte isochronous packets from it, takes one transfer and stops:
    /// the 24 bytes, then received and intact for each.
    fn usb(&mut self, step: u8, data: &[u8], into: &mut [u8]) -> Result<usize> {
        let usb = self.usb.as_mut().ok_or_else(missing)?;
        match step {
            0 => usb.config_descriptor(0, into).map_err(failed),
            1 => usb
                .claim_interface(byte(data, 0)?)
                .map(|()| 0)
                .map_err(failed),
            2 => usb
                .release_interface(byte(data, 0)?)
                .map(|()| 0)
                .map_err(failed),
            3 => usb
                .set_alternate(byte(data, 0)?, byte(data, 1)?)
                .map(|()| 0)
                .map_err(failed),
            4 => usb
                .detach_kernel_driver(byte(data, 0)?)
                .map(|()| 0)
                .map_err(failed),
            5 => usb.reset().map(|()| 0).map_err(failed),
            6 => usb.control_in(setup(data)?, into, WAIT).map_err(failed),
            7 => {
                let sent = usb
                    .control_out(setup(data)?, &data[6..], WAIT)
                    .map_err(failed)?;
                put(into, &(sent as u64).to_le_bytes())
            }
            8 => usb.bulk_in(byte(data, 0)?, into, WAIT).map_err(failed),
            9 => {
                let sent = usb
                    .bulk_out(byte(data, 0)?, &data[1..], WAIT)
                    .map_err(failed)?;
                put(into, &(sent as u64).to_le_bytes())
            }
            10 => usb.interrupt_in(byte(data, 0)?, into, WAIT).map_err(failed),
            11 => {
                let mut packets = [IsoPacket::default(); 3];
                let (bytes, results) = room(into, 30)?.split_at_mut(24);
                usb.iso_in_start(byte(data, 0)?, 2, 3, 8).map_err(failed)?;
                let taken = usb.iso_in_next(bytes, &mut packets, WAIT);
                usb.iso_in_stop().map_err(failed)?;
                taken.map_err(failed)?;
                for (result, packet) in results.chunks_mut(2).zip(packets) {
                    result[0] = packet.received as u8;
                    result[1] = u8::from(packet.ok);
                }
                Ok(30)
            }
            _ => Err(Status::INVALID_ARGUMENT),
        }
    }

    /// 0: enables with the descriptors in `data`. 1: disables. 2: the next event, as kind (0
    /// none, 1 configured, 2 deconfigured, 3 suspended, 4 resumed, 5 setup), then for a setup
    /// its six bytes and its length (2). 3: answers the pending request with `data` (8).
    /// 4: takes `data[0]` bytes of its data stage. 5: stalls it. 6: halts endpoint `data[0]`
    /// if `data[1]`, or clears it. 7: reads from endpoint `data[0]`. 8: writes `data[1..]` to
    /// it (8).
    fn function(&mut self, step: u8, data: &[u8], into: &mut [u8]) -> Result<usize> {
        let function = self.function.as_mut().ok_or_else(missing)?;
        match step {
            0 => function.enable(data).map(|()| 0).map_err(failed),
            1 => function.disable().map(|()| 0).map_err(failed),
            2 => match function.next_event().map_err(failed)? {
                None => put(into, &[0]),
                Some(UsbFunctionEvent::Configured) => put(into, &[1]),
                Some(UsbFunctionEvent::Deconfigured) => put(into, &[2]),
                Some(UsbFunctionEvent::Suspended) => put(into, &[3]),
                Some(UsbFunctionEvent::Resumed) => put(into, &[4]),
                Some(UsbFunctionEvent::Setup { setup, length }) => {
                    put(into, &[5, setup.request_type, setup.request])?;
                    put(&mut into[3..], &setup.value.to_le_bytes())?;
                    put(&mut into[5..], &setup.index.to_le_bytes())?;
                    put(&mut into[7..], &length.to_le_bytes())?;
                    Ok(9)
                }
            },
            3 => {
                let sent = function.control_in(data, WAIT).map_err(failed)?;
                put(into, &(sent as u64).to_le_bytes())
            }
            4 => {
                let len = usize::from(byte(data, 0)?);
                function.control_out(room(into, len)?, WAIT).map_err(failed)
            }
            5 => function.control_stall().map(|()| 0).map_err(failed),
            6 => function
                .set_halt(byte(data, 0)?, byte(data, 1)? != 0)
                .map(|()| 0)
                .map_err(failed),
            7 => function.read(byte(data, 0)?, into, WAIT).map_err(failed),
            8 => {
                let sent = function
                    .write(byte(data, 0)?, &data[1..], WAIT)
                    .map_err(failed)?;
                put(into, &(sent as u64).to_le_bytes())
            }
            _ => Err(Status::INVALID_ARGUMENT),
        }
    }
}
