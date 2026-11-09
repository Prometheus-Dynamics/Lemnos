//! A simulated I2C bus: targets answer to their addresses with the register
//! behaviour of the chips on real boards, and an unattached or dead target
//! does not acknowledge.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use lemnos_core::ErrorKind;
use lemnos_hal::i2c::{ErrorType, I2c, Operation};

use crate::SimClock;

/// How a target's registers and pointer behave.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Layout {
    /// Single bytes behind an 8-bit pointer every transfer steps through.
    #[default]
    Bytes,
    /// Registers of their own width behind a pointer reads don't step.
    Words,
}

/// Where a FIFO reports how full it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FifoLevel {
    /// Two little-endian bytes at this register: the byte count.
    Bytes(u8),
    /// One byte at `register`: the frame count, for frames of `frame` bytes.
    Frames { register: u8, frame: usize },
}

#[derive(Debug)]
struct Fifo {
    data: u8,
    level: FifoLevel,
    queue: VecDeque<u8>,
    capacity: usize,
    overflowed: u64,
    stream: Option<FifoStream>,
}

/// A chip that buffers one frame per period, whether or not anything reads.
#[derive(Debug)]
struct FifoStream {
    origin_ns: u64,
    period_ns: u64,
    produced: u64,
    frame: Vec<u8>,
}

#[derive(Debug, Default)]
struct Target {
    attached: bool,
    dead: bool,
    layout: Layout,
    pointer: u8,
    bytes: BTreeMap<u8, u8>,
    words: BTreeMap<u8, Vec<u8>>,
    fifo: Option<Fifo>,
}

impl Fifo {
    fn catch_up(&mut self, now_ns: u64) {
        let Some(stream) = &mut self.stream else {
            return;
        };
        if now_ns < stream.origin_ns {
            return;
        }
        let due = (now_ns - stream.origin_ns) / stream.period_ns.max(1);
        for _ in stream.produced..due {
            if self.queue.len() + stream.frame.len() > self.capacity {
                self.overflowed += 1;
            } else {
                self.queue.extend(stream.frame.iter().copied());
            }
        }
        stream.produced = stream.produced.max(due);
    }

    fn level_byte(&self, register: u8) -> Option<u8> {
        match self.level {
            FifoLevel::Bytes(at) if register == at => Some(self.queue.len() as u8),
            FifoLevel::Bytes(at) if register == at.wrapping_add(1) => {
                Some((self.queue.len() >> 8) as u8)
            }
            FifoLevel::Frames {
                register: at,
                frame,
            } if register == at => Some((self.queue.len() / frame.max(1)).min(0x7f) as u8),
            _ => None,
        }
    }
}

impl Target {
    fn write(&mut self, data: &[u8]) {
        let Some((first, rest)) = data.split_first() else {
            return;
        };
        self.pointer = *first;
        match self.layout {
            Layout::Words => {
                if !rest.is_empty() {
                    self.words.insert(self.pointer, rest.to_vec());
                }
            }
            Layout::Bytes => {
                for byte in rest {
                    self.bytes.insert(self.pointer, *byte);
                    self.pointer = self.pointer.wrapping_add(1);
                }
            }
        }
    }

    fn read(&mut self, out: &mut [u8], now_ns: u64) {
        if let Some(fifo) = &mut self.fifo {
            fifo.catch_up(now_ns);
            if fifo.data == self.pointer {
                for byte in out.iter_mut() {
                    *byte = fifo.queue.pop_front().unwrap_or(0);
                }
                return;
            }
        }
        match self.layout {
            Layout::Words => {
                let value = self.words.get(&self.pointer);
                for (i, byte) in out.iter_mut().enumerate() {
                    *byte = value.and_then(|v| v.get(i).copied()).unwrap_or(0);
                }
            }
            Layout::Bytes => {
                for byte in out.iter_mut() {
                    let level = self.fifo.as_ref().and_then(|f| f.level_byte(self.pointer));
                    let stored = self.bytes.get(&self.pointer).copied().unwrap_or(0);
                    *byte = level.unwrap_or(stored);
                    self.pointer = self.pointer.wrapping_add(1);
                }
            }
        }
    }
}

#[derive(Debug, Default)]
struct BusState {
    targets: BTreeMap<u8, Target>,
    transactions: u64,
}

/// An I2C bus of simulated chips. Clones share the bus, so a test changes
/// the registers a driver reads.
#[derive(Debug, Clone, Default)]
pub struct SimI2c {
    state: Arc<Mutex<BusState>>,
    clock: SimClock,
}

impl SimI2c {
    pub fn new(clock: SimClock) -> Self {
        Self {
            state: Arc::default(),
            clock,
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut BusState) -> R) -> R {
        f(&mut self.state.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn with_target<R>(&self, address: u8, f: impl FnOnce(&mut Target) -> R) -> R {
        self.with(|s| f(s.targets.entry(address).or_default()))
    }

    /// Puts a chip at `address`. Registers set earlier stay.
    pub fn attach(&self, address: u8, layout: Layout) -> &Self {
        self.with_target(address, |t| {
            t.attached = true;
            t.layout = layout;
        });
        self
    }

    /// Makes a chip stop or resume acknowledging.
    pub fn set_dead(&self, address: u8, dead: bool) {
        self.with_target(address, |t| t.dead = dead);
    }

    /// Sets consecutive byte registers.
    pub fn set_bytes(&self, address: u8, register: u8, bytes: &[u8]) {
        self.with_target(address, |t| {
            for (i, byte) in bytes.iter().enumerate() {
                t.bytes.insert(register.wrapping_add(i as u8), *byte);
            }
        });
    }

    /// Sets one word register.
    pub fn set_word(&self, address: u8, register: u8, bytes: &[u8]) {
        self.with_target(address, |t| {
            t.words.insert(register, bytes.to_vec());
        });
    }

    pub fn byte(&self, address: u8, register: u8) -> Option<u8> {
        self.with_target(address, |t| t.bytes.get(&register).copied())
    }

    pub fn word(&self, address: u8, register: u8) -> Option<Vec<u8>> {
        self.with_target(address, |t| t.words.get(&register).cloned())
    }

    /// Gives a chip a FIFO a test fills with [`fifo_push`](Self::fifo_push).
    pub fn add_fifo(&self, address: u8, data: u8, level: FifoLevel, capacity: usize) {
        self.with_target(address, |t| {
            t.fifo = Some(Fifo {
                data,
                level,
                queue: VecDeque::new(),
                capacity,
                overflowed: 0,
                stream: None,
            });
        });
    }

    pub fn fifo_push(&self, address: u8, bytes: &[u8]) {
        self.with_target(address, |t| {
            if let Some(fifo) = &mut t.fifo {
                fifo.queue.extend(bytes.iter().copied());
            }
        });
    }

    /// Makes the chip's FIFO fill with `frame` once per `period` of simulated
    /// time, starting one period from now.
    pub fn fifo_stream(&self, address: u8, period: Duration, frame: &[u8]) {
        let origin_ns = self.clock.now_ns();
        self.with_target(address, |t| {
            if let Some(fifo) = &mut t.fifo {
                fifo.stream = Some(FifoStream {
                    origin_ns,
                    period_ns: period.as_nanos() as u64,
                    produced: 0,
                    frame: frame.to_vec(),
                });
            }
        });
    }

    /// Bytes waiting in the chip's FIFO, as of simulated now.
    pub fn fifo_len(&self, address: u8) -> usize {
        let now = self.clock.now_ns();
        self.with_target(address, |t| match &mut t.fifo {
            Some(fifo) => {
                fifo.catch_up(now);
                fifo.queue.len()
            }
            None => 0,
        })
    }

    /// Frames the chip had to throw away because its FIFO was full.
    pub fn fifo_overflowed(&self, address: u8) -> u64 {
        let now = self.clock.now_ns();
        self.with_target(address, |t| match &mut t.fifo {
            Some(fifo) => {
                fifo.catch_up(now);
                fifo.overflowed
            }
            None => 0,
        })
    }

    pub fn transactions(&self) -> u64 {
        self.with(|s| s.transactions)
    }
}

impl ErrorType for SimI2c {
    type Error = ErrorKind;
}

impl I2c for SimI2c {
    fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), ErrorKind> {
        let now = self.clock.now_ns();
        self.with(|s| {
            s.transactions += 1;
            let target = s
                .targets
                .get_mut(&address)
                .filter(|t| t.attached && !t.dead)
                .ok_or(ErrorKind::Nack)?;
            for operation in operations {
                match operation {
                    Operation::Write(data) => target.write(data),
                    Operation::Read(out) => target.read(out, now),
                }
            }
            Ok(())
        })
    }
}
