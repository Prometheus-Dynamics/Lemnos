//! Modbus RTU over a UART: the frames a description's reads and writes become. Only what the
//! description form needs: read holding or input registers (functions 3 and 4) and write one
//! register (function 6). Frames are built and checked in fixed buffers, so nothing allocates.
//!
//! The frame facts are the Modbus application protocol (function codes, exception replies) and
//! the serial line guide (RTU framing, CRC-16 with polynomial 0xa001 reflected, start 0xffff, low
//! byte first), both published by the Modbus Organization.

use core::time::Duration;

use lemnos_hal::{ErrorKind, Uart};

/// The most registers one read asks for: a channel spans at most 32 bits.
pub const MAX_REGISTERS: usize = 2;

/// The longest reply this module reads: unit, function, count, two registers, CRC.
const MAX_REPLY: usize = 5 + 2 * MAX_REGISTERS;

/// How many times a request is written before a full transmit buffer is a fault.
const WRITE_TRIES: usize = 4;

/// What went wrong with one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// The port failed, or nothing whole came back in time.
    Bus(ErrorKind),
    /// A reply came back that is not this request's: wrong CRC, unit, function or length.
    Garbled,
    /// The device answered with an exception code.
    Exception(u8),
}

/// The Modbus CRC of `data`.
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0xffffu16;
    for byte in data {
        crc ^= u16::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xa001
            } else {
                crc >> 1
            };
        }
    }
    crc
}

/// Whether a frame's last two bytes are the CRC of the rest.
fn crc_holds(frame: &[u8]) -> bool {
    let Some((body, crc)) = frame.split_last_chunk::<2>() else {
        return false;
    };
    crc16(body) == u16::from_le_bytes(*crc)
}

/// One request and its reply on `uart`, to unit `unit`.
pub struct Link<'a, U: ?Sized> {
    pub uart: &'a mut U,
    pub unit: u8,
    /// How long to wait for each part of a reply.
    pub wait: Duration,
}

impl<U: Uart<Error = ErrorKind> + ?Sized> Link<'_, U> {
    /// Reads `out.len() / 2` registers from `register` (function 4 when `input`, 3 otherwise).
    /// `out` gets the registers' bytes as they come, each register high byte first.
    pub fn read(&mut self, input: bool, register: u16, out: &mut [u8]) -> Result<(), Fault> {
        let count = out.len() / 2;
        debug_assert!((1..=MAX_REGISTERS).contains(&count) && out.len().is_multiple_of(2));
        let function = if input { 4 } else { 3 };
        let [hi, lo] = register.to_be_bytes();
        let request = self.frame([function, hi, lo, 0, count as u8]);
        let mut reply = [0u8; MAX_REPLY];
        let len = 5 + out.len();
        self.exchange(&request, &mut reply[..len])?;
        if reply[2] as usize != out.len() {
            return Err(Fault::Garbled);
        }
        out.copy_from_slice(&reply[3..3 + out.len()]);
        Ok(())
    }

    /// Writes one register (function 6). The device echoes the request when it took it.
    pub fn write(&mut self, register: u16, value: u16) -> Result<(), Fault> {
        let [rh, rl] = register.to_be_bytes();
        let [vh, vl] = value.to_be_bytes();
        let request = self.frame([6, rh, rl, vh, vl]);
        let mut reply = [0u8; 8];
        self.exchange(&request, &mut reply)?;
        if reply != request {
            return Err(Fault::Garbled);
        }
        Ok(())
    }

    /// The request frame: unit, `pdu`, CRC.
    fn frame(&self, pdu: [u8; 5]) -> [u8; 8] {
        let mut frame = [0u8; 8];
        frame[0] = self.unit;
        frame[1..6].copy_from_slice(&pdu);
        let crc = crc16(&frame[..6]).to_le_bytes();
        frame[6..].copy_from_slice(&crc);
        frame
    }

    /// Sends `request` and reads a reply of `reply.len()` bytes, or an exception's five.
    fn exchange(&mut self, request: &[u8; 8], reply: &mut [u8]) -> Result<(), Fault> {
        self.drain()?;
        let mut sent = 0;
        for _ in 0..WRITE_TRIES {
            sent += self.uart.write(&request[sent..]).map_err(Fault::Bus)?;
            if sent == request.len() {
                break;
            }
        }
        if sent < request.len() {
            return Err(Fault::Bus(ErrorKind::Timeout));
        }
        let mut got = 0;
        let mut want = reply.len();
        while got < want {
            let n = self
                .uart
                .read(&mut reply[got..want], self.wait)
                .map_err(Fault::Bus)?;
            if n == 0 {
                return Err(Fault::Bus(ErrorKind::Timeout));
            }
            got += n;
            // An exception is five bytes: unit, function with its top bit, code, CRC.
            if got >= 2 && reply[1] == request[1] | 0x80 {
                want = 5;
            }
        }
        let reply = &reply[..want];
        if !crc_holds(reply) || reply[0] != request[0] {
            return Err(Fault::Garbled);
        }
        if reply[1] == request[1] | 0x80 {
            return Err(Fault::Exception(reply[2]));
        }
        if reply[1] != request[1] {
            return Err(Fault::Garbled);
        }
        Ok(())
    }

    /// Drops whatever arrived before this request: a late reply to an earlier one, or noise.
    fn drain(&mut self) -> Result<(), Fault> {
        let mut scrap = [0u8; 32];
        for _ in 0..8 {
            if self
                .uart
                .read(&mut scrap, Duration::ZERO)
                .map_err(Fault::Bus)?
                == 0
            {
                break;
            }
        }
        Ok(())
    }
}
