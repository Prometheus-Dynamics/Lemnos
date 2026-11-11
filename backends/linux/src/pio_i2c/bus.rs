//! One PIO I2C bus: the RP1 PIO state machine that runs the program, the pins it
//! owns, and the transactions over its FIFOs. Kernel calls go through [`Pio`], so
//! the tests run the sequencing on a fake block.

use std::io;
use std::mem;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use lemnos_hal::ErrorKind;
use lemnos_hal::i2c::Operation;
use lemnos_linux_sys::rp1_pio::{self, INSTRUCTION_COUNT, SmConfig, override_value, regs};

use super::device::Pio;
use super::program::{PROGRAM_LEN, idle_wrap, initial_pc, program};
use super::protocol::{
    DecodeError, Op, Segment, check_ack_window, decode_into, encode_into, rx_word_count,
    segment_ops_into,
};
use crate::kind_of;

/// Bounce-buffer size and count for each DMA direction (bytes, buffers).
const DMA_BUF_SIZE: u32 = 64;
const DMA_BUF_COUNT: u32 = 4;
/// The PIO clock on RP1 (`clock_get_hz(clk_sys)` in `pio_rp1.h`).
const PIO_CLOCK_HZ: f64 = 200_000_000.0;
/// PIO ticks per SCL period: 3 instructions of 8 cycles each per bit.
const TICKS_PER_BIT: f64 = 24.0;
/// Most words one DMA transfer may move, in either direction.
const MAX_WORDS: usize = rp1_pio::MAX_XFER_WORDS;

/// Nine clock pulses and a STOP: releases a target that holds SDA low.
const CLOCK_OUT: [Op; 10] = clock_out();

const fn clock_out() -> [Op; 10] {
    let mut ops = [Op::Stop; 10];
    let mut i = 0;
    while i < 9 {
        ops[i] = Op::Write(0xff);
        i += 1;
    }
    ops
}

/// What a transaction failed with. Each failure reports on the call that hit it;
/// the bus is left to try again on the next call.
#[derive(Debug)]
pub(crate) enum Failure {
    /// The target did not ACK a WRITE.
    Nack,
    /// The FIFO transfer did not finish in time.
    Timeout,
    /// A device or driver error from the FIFO transfer.
    Io(io::Error),
    /// The request cannot be run on this bus (too long, or a shape it can't express).
    Refused(ErrorKind),
}

impl Failure {
    /// The hardware error this failure reports as.
    pub(crate) fn kind(self) -> ErrorKind {
        match self {
            Self::Nack => ErrorKind::Nack,
            Self::Timeout => ErrorKind::Timeout,
            Self::Io(error) => kind_of(&error),
            Self::Refused(kind) => kind,
        }
    }
}

/// A transaction's TX words handed back by the helper with its result.
type TxDone = (Vec<u32>, io::Result<()>);

/// What passes between a transaction and the bus's TX helper: one job at a time, its answer,
/// and the bus's end. A mutex and a condition variable, not a channel: a channel's blocking
/// receive allocates the first time it has to wait (its waiter registration), which can happen
/// at any transfer, so a steady stream of transfers would allocate now and then.
#[derive(Default)]
struct Handoff {
    slot: Mutex<Slot>,
    changed: Condvar,
}

#[derive(Default)]
struct Slot {
    job: Option<Vec<u32>>,
    done: Option<TxDone>,
    closed: bool,
}

impl Handoff {
    fn lock(&self) -> MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wait<'a>(&self, slot: MutexGuard<'a, Slot>) -> MutexGuard<'a, Slot> {
        self.changed
            .wait(slot)
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The helper's side: the next job, or `None` once the bus has ended.
    fn next_job(&self) -> Option<Vec<u32>> {
        let mut slot = self.lock();
        loop {
            if let Some(job) = slot.job.take() {
                return Some(job);
            }
            if slot.closed {
                return None;
            }
            slot = self.wait(slot);
        }
    }

    fn finish(&self, done: TxDone) {
        self.lock().done = Some(done);
        self.changed.notify_all();
    }

    fn post(&self, job: Vec<u32>) {
        self.lock().job = Some(job);
        self.changed.notify_all();
    }

    /// The transaction's side: the helper's answer to the job posted.
    fn answer(&self) -> TxDone {
        let mut slot = self.lock();
        loop {
            if let Some(done) = slot.done.take() {
                return done;
            }
            slot = self.wait(slot);
        }
    }

    fn close(&self) {
        self.lock().closed = true;
        self.changed.notify_all();
    }
}

/// A transaction's buffers, kept for the bus's life under its lock: a steady stream of
/// transactions of the same shape allocates nothing once the buffers have grown.
struct Scratch {
    ops: Vec<Op>,
    /// The TX words; lent to the helper for each transaction and handed back.
    tx: Vec<u32>,
    rx: Vec<u32>,
    data: Vec<u8>,
    pending: Vec<usize>,
}

/// A configured PIO I2C master on one SDA/SCL pair.
pub(crate) struct PioBus<D: Pio> {
    sda: u8,
    scl: u8,
    device: Arc<D>,
    /// The TX helper's jobs and answers, one transfer at a time.
    handoff: Arc<Handoff>,
    /// The TX helper, joined when the bus drops: it holds the device too, and the
    /// device's client (and on the module's route its claim) goes only with the last
    /// holder, so the bus is gone only once the helper is.
    helper: Option<std::thread::JoinHandle<()>>,
    sm: u16,
    base: u16,
    config: SmConfig,
    /// Serializes transactions (one state machine, one FIFO pair) and holds their buffers.
    gate: Mutex<Scratch>,
}

impl<D: Pio> PioBus<D> {
    /// Claims a state machine on `device`, loads the program, and configures SDA and
    /// SCL for `hz`. On any failure the machine, the program and the pins go back.
    pub(crate) fn start(device: D, sda: u8, scl: u8, hz: u32) -> io::Result<Self> {
        if sda == scl
            || u32::from(sda) >= rp1_pio::GPIO_COUNT
            || u32::from(scl) >= rp1_pio::GPIO_COUNT
        {
            return Err(refused());
        }
        let config = sm_config(sda, scl, hz).map_err(|_| refused())?;
        let sm = device.claim_any_sm()?;
        let base = match load_program(&device, scl) {
            Ok(base) => base,
            Err(error) => {
                let _ = device.unclaim_sms(1 << sm);
                return Err(error);
            }
        };
        let config = with_idle_wrap(config, base);
        if let Err(error) = setup(&device, sm, base, config, sda, scl) {
            release(&device, sm, base, sda, scl);
            return Err(error);
        }
        let device = Arc::new(device);
        // One TX helper per bus, for the bus's life: a transfer hands it the TX words
        // instead of spawning a thread.
        let handoff = Arc::new(Handoff::default());
        let helper = Arc::clone(&device);
        let jobs = Arc::clone(&handoff);
        let spawned = std::thread::Builder::new()
            .name(format!("pio-i2c-tx:{sda}-{scl}"))
            .spawn(move || {
                while let Some(words) = jobs.next_job() {
                    let result = helper.xfer_to_sm(sm, &words);
                    jobs.finish((words, result));
                }
            });
        let helper = match spawned {
            Ok(helper) => helper,
            Err(error) => {
                release(&*device, sm, base, sda, scl);
                return Err(error);
            }
        };
        Ok(Self {
            sda,
            scl,
            device,
            handoff,
            helper: Some(helper),
            sm,
            base,
            config,
            gate: Mutex::new(Scratch {
                ops: Vec::with_capacity(64),
                tx: Vec::with_capacity(64),
                rx: Vec::with_capacity(32),
                data: Vec::with_capacity(32),
                pending: Vec::with_capacity(8),
            }),
        })
    }

    /// Runs one embedded-hal transaction: adjacent operations of one direction are one
    /// transfer, and the bytes read fill the read buffers in order. A failure reports
    /// here, and the next call tries again.
    pub(crate) fn run_operations(
        &self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), ErrorKind> {
        if address > 0x7f {
            return Err(ErrorKind::InvalidInput);
        }
        let mut guard = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        let s = &mut *guard;
        s.ops.clear();
        segment_ops_into(
            &mut s.ops,
            address,
            operations.iter().map(|operation| match operation {
                Operation::Read(buffer) => Segment::Read(buffer.len()),
                Operation::Write(bytes) => Segment::Write(bytes),
            }),
        );
        if s.ops.is_empty() {
            return Ok(());
        }
        self.transact(s).map_err(Failure::kind)?;
        let mut cursor = 0;
        for operation in operations.iter_mut() {
            if let Operation::Read(buffer) = operation {
                let end = cursor + buffer.len();
                let chunk = s.data.get(cursor..end).ok_or(ErrorKind::Failed)?;
                buffer.copy_from_slice(chunk);
                cursor = end;
            }
        }
        Ok(())
    }

    /// Runs the ops in `s.ops` and decodes the bytes read into `s.data`.
    fn transact(&self, s: &mut Scratch) -> Result<(), Failure> {
        if check_ack_window(&s.ops).is_err() {
            return Err(Failure::Refused(ErrorKind::Unsupported));
        }
        let ops = mem::take(&mut s.ops);
        let sent = self.exchange(s, &ops);
        s.ops = ops;
        match sent {
            Ok(()) => {}
            Err(refusal @ Failure::Refused(_)) => return Err(refusal),
            Err(failure) => {
                self.recover(s);
                return Err(failure);
            }
        }
        match decode_into(&s.ops, &s.rx, &mut s.data, &mut s.pending) {
            Ok(()) => Ok(()),
            Err(DecodeError::Nack(_)) => Err(Failure::Nack),
            Err(DecodeError::Length { .. }) => Err(Failure::Refused(ErrorKind::Failed)),
        }
    }

    /// Encodes `ops`, sends the TX words to the helper, and drains the RX FIFO while
    /// the helper feeds the machine. The machine pushes a read byte per READ, so a long
    /// transaction would stall on a full RX FIFO if the TX DMA ran first.
    fn exchange(&self, s: &mut Scratch, ops: &[Op]) -> Result<(), Failure> {
        encode_into(self.base, u16::from(self.scl), ops, &mut s.tx);
        let rx_len = rx_word_count(ops);
        if s.tx.len() > MAX_WORDS || rx_len > MAX_WORDS {
            return Err(Failure::Refused(ErrorKind::InvalidInput));
        }
        s.rx.clear();
        s.rx.resize(rx_len, 0);
        self.handoff.post(mem::take(&mut s.tx));
        let rx_result = self.device.xfer_from_sm(self.sm, &mut s.rx);
        let (words, tx_result) = self.handoff.answer();
        s.tx = words;
        tx_result.and(rx_result).map_err(classify)
    }

    /// Clears a stuck transfer: halts the machine, sets its DMA up again, releases both
    /// pins, restarts it at the main loop, and clocks out nine bits and a STOP so a target
    /// holding SDA low lets go. Errors here are ignored; the next transaction reports the
    /// bus state.
    ///
    /// The DMA is set up again because a transfer that timed out may still be on its
    /// channel, and the module's route refuses every later transfer on a channel in that
    /// state until it is configured again: without this the bus would stay failed for good.
    fn recover(&self, s: &mut Scratch) {
        let mask = 1u16 << self.sm;
        let _ = self.device.sm_set_enabled(mask, false);
        let _ = self.device.sm_clear_fifos(self.sm);
        let _ = configure_dma(&*self.device, self.sm);
        let _ = self
            .device
            .sm_set_pindirs(self.sm, 0, pin_mask(self.sda, self.scl));
        let _ = self
            .device
            .sm_init(self.sm, initial_pc(self.base), self.config);
        let _ = self.device.sm_set_enabled(mask, true);
        let _ = self.exchange(s, &CLOCK_OUT);
    }
}

impl<D: Pio> Drop for PioBus<D> {
    fn drop(&mut self) {
        release(&*self.device, self.sm, self.base, self.sda, self.scl);
        // Closing the handoff ends the helper; waiting for it drops its hold on the device.
        self.handoff.close();
        if let Some(helper) = self.helper.take() {
            let _ = helper.join();
        }
    }
}

fn classify(error: io::Error) -> Failure {
    if error.kind() == io::ErrorKind::TimedOut {
        Failure::Timeout
    } else {
        Failure::Io(error)
    }
}

/// The error for a request this crate refuses before it reaches the device.
fn refused() -> io::Error {
    io::Error::from_raw_os_error(libc::EINVAL)
}

/// Sets the pins, DMA channels, state machine and program for a bus, and starts the
/// machine.
fn setup<D: Pio>(
    device: &D,
    sm: u16,
    base: u16,
    config: SmConfig,
    sda: u8,
    scl: u8,
) -> io::Result<()> {
    for pin in [sda, scl] {
        let pin = u16::from(pin);
        device.gpio_init(pin)?;
        device.gpio_set_function(pin, rp1_pio::GPIO_FUNC_PIO)?;
        device.gpio_set_input_enabled(pin, true)?;
        // The pins only ever drive low (direction), so force the output low.
        device.gpio_set_outover(pin, override_value::LOW)?;
    }
    configure_dma(device, sm)?;
    device.sm_init(sm, initial_pc(base), config)?;
    device.sm_set_pindirs(sm, 0, pin_mask(sda, scl))?;
    device.sm_set_enabled(1 << sm, true)
}

/// Sets up the machine's DMA channels, both ways.
fn configure_dma<D: Pio>(device: &D, sm: u16) -> io::Result<()> {
    device.config_xfer32(sm, rp1_pio::DIR_TO_SM, DMA_BUF_SIZE, DMA_BUF_COUNT)?;
    device.config_xfer32(sm, rp1_pio::DIR_FROM_SM, DMA_BUF_SIZE, DMA_BUF_COUNT)
}

/// Stops the machine, releases the pins to the kernel's SIO function, and removes the
/// program and the claim. Best effort: the client close releases whatever is left.
fn release<D: Pio>(device: &D, sm: u16, base: u16, sda: u8, scl: u8) {
    let _ = device.sm_set_enabled(1 << sm, false);
    let _ = device.sm_set_pindirs(sm, 0, pin_mask(sda, scl));
    for pin in [sda, scl] {
        let _ = device.gpio_set_outover(u16::from(pin), override_value::NORMAL);
        let _ = device.gpio_set_function(u16::from(pin), rp1_pio::GPIO_FUNC_SIO);
    }
    let _ = device.remove_program(base, PROGRAM_LEN as u16);
    let _ = device.unclaim_sms(1 << sm);
}

/// SCL clock divider for `hz`: `200 MHz / (24 * hz)` as integer and 1/256 parts.
pub(crate) fn clkdiv_for(hz: u32) -> Result<(u16, u8), String> {
    if hz == 0 {
        return Err("PIO I2C frequency must be above zero".into());
    }
    let div = PIO_CLOCK_HZ / (TICKS_PER_BIT * f64::from(hz));
    let int = div.floor();
    if !(1.0..=65_535.0).contains(&int) {
        return Err(format!("PIO I2C frequency {hz} Hz is out of range"));
    }
    let frac = ((div - int) * 256.0).floor().clamp(0.0, 255.0) as u8;
    Ok((int as u16, frac))
}

/// The state machine configuration for `sda`/`scl` at `hz`.
pub(crate) fn sm_config(sda: u8, scl: u8, hz: u32) -> Result<SmConfig, String> {
    let (int, frac) = clkdiv_for(hz)?;
    Ok(SmConfig {
        clkdiv: regs::clkdiv(u32::from(int), u32::from(frac)),
        // Optional side-set drives SCL's pin direction; JMP pin is SDA.
        // Wrap over the whole instruction memory (the program never falls through its end).
        execctrl: regs::SIDE_PINDIR
            | regs::execctrl_jmp_pin(u32::from(sda))
            | regs::execctrl_wrap(0, 31),
        shiftctrl: regs::shiftctrl_in(false, false, 32) | regs::shiftctrl_out(false, false, 14),
        pinctrl: regs::pinctrl_out(u32::from(sda), 1)
            | regs::pinctrl_set(u32::from(scl), 1)
            | regs::pinctrl_in(u32::from(sda))
            | regs::pinctrl_sideset(u32::from(scl), 1),
    })
}

/// The configuration with the wrap window of the idle loop at `base`.
fn with_idle_wrap(config: SmConfig, base: u16) -> SmConfig {
    let (bottom, top) = idle_wrap(base);
    const WRAP_BITS: u32 = 0x1f000 | 0xf80;
    SmConfig {
        execctrl: (config.execctrl & !WRAP_BITS)
            | regs::execctrl_wrap(u32::from(bottom), u32::from(top)),
        ..config
    }
}

fn pin_mask(sda: u8, scl: u8) -> u32 {
    (1u32 << sda) | (1u32 << scl)
}

/// Loads the program at the first origin that fits, returning its base.
fn load_program<D: Pio>(device: &D, scl: u8) -> io::Result<u16> {
    let mut last = None;
    for base in 0..=(INSTRUCTION_COUNT as u16 - PROGRAM_LEN as u16) {
        match device.add_program(base, &program(base, u16::from(scl))) {
            Ok(offset) if offset == base => return Ok(base),
            Ok(offset) => {
                let _ = device.remove_program(offset, PROGRAM_LEN as u16);
            }
            Err(error) => last = Some(error),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::from_raw_os_error(libc::ENOSPC)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pio_i2c::protocol::{encode, segment_ops};
    use lemnos_measure::test_alloc::allocations;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// What the fake block was asked to do, and the answers it gives.
    #[derive(Default)]
    struct Shared {
        /// Setup and teardown calls, in order (transfers are not logged).
        calls: Mutex<Vec<&'static str>>,
        /// TX words of each transfer, kept only while `record` is set.
        tx: Mutex<Vec<Vec<u32>>>,
        record: AtomicBool,
        /// Answers for the next RX transfers, in order. An empty queue reads zeros.
        rx: Mutex<VecDeque<Result<Vec<u32>, i32>>>,
        /// A setup call that fails with `EIO`.
        fail: Mutex<Option<&'static str>>,
        /// An RX transfer timed out: as on the module's route, every later transfer on
        /// that channel fails with `EIO` until the DMA is configured again.
        stuck: AtomicBool,
        /// Each TX transfer takes this long, so the transaction waits for the helper.
        tx_delay_us: std::sync::atomic::AtomicU64,
        /// Each RX transfer takes this long, so the helper is done first.
        rx_delay_us: std::sync::atomic::AtomicU64,
    }

    impl Shared {
        fn log(&self, name: &'static str) {
            self.calls.lock().unwrap().push(name);
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }

        fn check(&self, name: &'static str) -> io::Result<()> {
            self.log(name);
            if *self.fail.lock().unwrap() == Some(name) {
                return Err(io::Error::from_raw_os_error(libc::EIO));
            }
            Ok(())
        }
    }

    struct Mock(Arc<Shared>);

    impl Pio for Mock {
        fn claim_any_sm(&self) -> io::Result<u16> {
            self.0.check("claim")?;
            Ok(0)
        }
        fn unclaim_sms(&self, _mask: u16) -> io::Result<()> {
            self.0.check("unclaim")
        }
        fn add_program(&self, origin: u16, _instrs: &[u16]) -> io::Result<u16> {
            self.0.check("add_program")?;
            Ok(origin)
        }
        fn remove_program(&self, _origin: u16, _len: u16) -> io::Result<()> {
            self.0.check("remove_program")
        }
        fn sm_init(&self, _sm: u16, _pc: u16, _config: SmConfig) -> io::Result<()> {
            self.0.check("sm_init")
        }
        fn sm_set_pindirs(&self, _sm: u16, _dirs: u32, _mask: u32) -> io::Result<()> {
            self.0.check("pindirs")
        }
        fn sm_set_enabled(&self, _mask: u16, enable: bool) -> io::Result<()> {
            self.0.check(if enable { "enable" } else { "disable" })
        }
        fn sm_clear_fifos(&self, _sm: u16) -> io::Result<()> {
            self.0.check("clear")
        }
        fn config_xfer32(&self, _sm: u16, _dir: u16, _size: u32, _count: u32) -> io::Result<()> {
            self.0.stuck.store(false, Ordering::Relaxed);
            self.0.check("config_xfer32")
        }
        fn xfer_to_sm(&self, _sm: u16, words: &[u32]) -> io::Result<()> {
            let delay = self.0.tx_delay_us.load(Ordering::Relaxed);
            if delay > 0 {
                std::thread::sleep(std::time::Duration::from_micros(delay));
            }
            if self.0.record.load(Ordering::Relaxed) {
                self.0.tx.lock().unwrap().push(words.to_vec());
            }
            Ok(())
        }
        fn xfer_from_sm(&self, _sm: u16, words: &mut [u32]) -> io::Result<()> {
            let delay = self.0.rx_delay_us.load(Ordering::Relaxed);
            if delay > 0 {
                std::thread::sleep(std::time::Duration::from_micros(delay));
            }
            if self.0.stuck.load(Ordering::Relaxed) {
                return Err(io::Error::from_raw_os_error(libc::EIO));
            }
            match self.0.rx.lock().unwrap().pop_front() {
                None => {
                    words.fill(0);
                    Ok(())
                }
                Some(Err(errno)) => {
                    if errno == libc::ETIMEDOUT {
                        self.0.stuck.store(true, Ordering::Relaxed);
                    }
                    Err(io::Error::from_raw_os_error(errno))
                }
                Some(Ok(answer)) => {
                    if answer.len() != words.len() {
                        return Err(io::Error::from_raw_os_error(libc::EIO));
                    }
                    words.copy_from_slice(&answer);
                    Ok(())
                }
            }
        }
        fn gpio_init(&self, _gpio: u16) -> io::Result<()> {
            self.0.check("gpio")
        }
        fn gpio_set_function(&self, _gpio: u16, _func: u16) -> io::Result<()> {
            self.0.check("gpio")
        }
        fn gpio_set_input_enabled(&self, _gpio: u16, _enabled: bool) -> io::Result<()> {
            self.0.check("gpio")
        }
        fn gpio_set_outover(&self, _gpio: u16, _value: u16) -> io::Result<()> {
            self.0.check("gpio")
        }
    }

    fn bus() -> (PioBus<Mock>, Arc<Shared>) {
        let shared = Arc::new(Shared::default());
        let bus = PioBus::start(Mock(Arc::clone(&shared)), 8, 7, 400_000).unwrap();
        (bus, shared)
    }

    /// The RX words of a bus that ACKs every byte, for the ops that run, with `bytes`
    /// as the read data in order.
    fn acked(ops: &[Op], bytes: &[u8]) -> Vec<u32> {
        let mut bytes = bytes.iter();
        let mut words = Vec::new();
        for op in ops {
            match op {
                Op::Write(_) => {}
                Op::Start | Op::Stop => words.push(0),
                Op::Read { .. } => words.push(u32::from(*bytes.next().unwrap())),
            }
        }
        words
    }

    /// The ops a transaction of these segments runs.
    fn ops_of(address: u8, segments: &[Segment<'_>]) -> Vec<Op> {
        segment_ops(address, segments)
    }

    #[test]
    fn start_claims_loads_and_enables_the_block() {
        let (_bus, shared) = bus();
        let calls = shared.calls();
        for name in ["claim", "add_program", "config_xfer32", "pindirs", "enable"] {
            assert!(calls.contains(&name), "{name} missing from {calls:?}");
        }
    }

    #[test]
    fn a_read_returns_the_bytes_in_order() {
        let (bus, shared) = bus();
        let ops = ops_of(0x68, &[Segment::Read(3)]);
        shared
            .rx
            .lock()
            .unwrap()
            .push_back(Ok(acked(&ops, &[0xaa, 0xbb, 0xcc])));
        let mut out = [0; 3];
        bus.run_operations(0x68, &mut [Operation::Read(&mut out)])
            .unwrap();
        assert_eq!(out, [0xaa, 0xbb, 0xcc]);
    }

    #[test]
    fn a_write_then_a_read_sends_the_encoded_ops() {
        let (bus, shared) = bus();
        shared.record.store(true, Ordering::Relaxed);
        let ops = ops_of(0x18, &[Segment::Write(&[0x00]), Segment::Read(1)]);
        shared
            .rx
            .lock()
            .unwrap()
            .push_back(Ok(acked(&ops, &[0x55])));
        let mut out = [0; 1];
        bus.run_operations(
            0x18,
            &mut [Operation::Write(&[0x00]), Operation::Read(&mut out)],
        )
        .unwrap();
        assert_eq!(out, [0x55]);
        let sent = shared.tx.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0], encode(bus.base, u16::from(bus.scl), &ops));
    }

    #[test]
    fn a_nack_is_reported_on_that_call_and_the_next_call_tries_again() {
        let (bus, shared) = bus();
        // START, address W, register W, START, address R, read 1, STOP. The second START's
        // word carries the ACKs of the address (bit 1) and register (bit 0) writes.
        let ops = ops_of(0x18, &[Segment::Write(&[0x00]), Segment::Read(1)]);
        let mut out = [0; 1];
        shared
            .rx
            .lock()
            .unwrap()
            .push_back(Ok(vec![0, 0b10, 0x55, 0]));
        let first = bus.run_operations(
            0x18,
            &mut [Operation::Write(&[0x00]), Operation::Read(&mut out)],
        );
        assert_eq!(first, Err(ErrorKind::Nack));
        shared
            .rx
            .lock()
            .unwrap()
            .push_back(Ok(acked(&ops, &[0x55])));
        bus.run_operations(
            0x18,
            &mut [Operation::Write(&[0x00]), Operation::Read(&mut out)],
        )
        .unwrap();
        assert_eq!(out, [0x55]);
    }

    #[test]
    fn a_failed_transfer_reports_then_the_bus_recovers_for_the_next_call() {
        let (bus, shared) = bus();
        let ops = ops_of(0x68, &[Segment::Read(1)]);
        let mut out = [0; 1];
        shared.rx.lock().unwrap().push_back(Err(libc::ETIMEDOUT));
        let first = bus.run_operations(0x68, &mut [Operation::Read(&mut out)]);
        assert_eq!(first, Err(ErrorKind::Timeout));
        // The recovery halted the machine, set its DMA up again (a timed-out channel
        // refuses until then) and clocked the bus out.
        let calls = shared.calls();
        assert!(
            calls.ends_with(&[
                "disable",
                "clear",
                "config_xfer32",
                "config_xfer32",
                "pindirs",
                "sm_init",
                "enable"
            ]),
            "{calls:?}"
        );
        shared
            .rx
            .lock()
            .unwrap()
            .push_back(Ok(acked(&ops, &[0x42])));
        bus.run_operations(0x68, &mut [Operation::Read(&mut out)])
            .unwrap();
        assert_eq!(out, [0x42]);
    }

    #[test]
    fn a_bad_address_is_refused_before_anything_is_sent() {
        let (bus, shared) = bus();
        shared.record.store(true, Ordering::Relaxed);
        assert_eq!(
            bus.run_operations(0x80, &mut [Operation::Write(&[1])]),
            Err(ErrorKind::InvalidInput)
        );
        assert!(shared.tx.lock().unwrap().is_empty());
    }

    #[test]
    fn a_write_run_past_the_ack_window_is_refused_before_anything_is_sent() {
        let (bus, shared) = bus();
        shared.record.store(true, Ordering::Relaxed);
        // The address byte and 32 data bytes make 33 writes between START and STOP.
        let data = [0u8; 32];
        assert_eq!(
            bus.run_operations(0x18, &mut [Operation::Write(&data)]),
            Err(ErrorKind::Unsupported)
        );
        assert!(shared.tx.lock().unwrap().is_empty());
        // The longest run that fits (31 data bytes plus the address) still runs.
        shared.rx.lock().unwrap().push_back(Ok(acked(
            &ops_of(0x18, &[Segment::Write(&data[..31])]),
            &[],
        )));
        bus.run_operations(0x18, &mut [Operation::Write(&data[..31])])
            .unwrap();
    }

    #[test]
    fn a_steady_transfer_allocates_nothing() {
        let (bus, shared) = bus();
        let ops = ops_of(0x68, &[Segment::Write(&[0x3b]), Segment::Read(6)]);
        let bytes = [1u8, 2, 3, 4, 5, 6];
        // Warm up: the buffers grow to this shape once.
        shared.rx.lock().unwrap().push_back(Ok(acked(&ops, &bytes)));
        let mut out = [0; 6];
        bus.run_operations(
            0x68,
            &mut [Operation::Write(&[0x3b]), Operation::Read(&mut out)],
        )
        .unwrap();
        // The next transfers of that shape allocate nothing on this thread.
        for _ in 0..3 {
            shared.rx.lock().unwrap().push_back(Ok(acked(&ops, &bytes)));
            let (result, n) = allocations(|| {
                bus.run_operations(
                    0x68,
                    &mut [Operation::Write(&[0x3b]), Operation::Read(&mut out)],
                )
            });
            result.unwrap();
            assert_eq!(n, 0, "a steady transfer allocated {n} times");
        }
        assert_eq!(out, bytes);
    }

    /// The first time a transaction has to wait for the TX helper is a steady transfer too:
    /// warmed up with a helper that is always done first, then with one that is slow.
    #[test]
    fn waiting_for_the_tx_helper_allocates_nothing() {
        let (bus, shared) = bus();
        let ops = ops_of(0x68, &[Segment::Write(&[0x3b]), Segment::Read(6)]);
        let bytes = [1u8, 2, 3, 4, 5, 6];
        let mut out = [0; 6];
        let mut run = || {
            shared.rx.lock().unwrap().push_back(Ok(acked(&ops, &bytes)));
            allocations(|| {
                bus.run_operations(
                    0x68,
                    &mut [Operation::Write(&[0x3b]), Operation::Read(&mut out)],
                )
            })
        };
        // Warm up with the helper always done before the transaction waits for it, as on a
        // board where the RX side is the whole transfer and TX is quick.
        shared.rx_delay_us.store(2000, Ordering::Relaxed);
        for _ in 0..3 {
            run().0.unwrap();
        }
        shared.rx_delay_us.store(0, Ordering::Relaxed);
        shared.tx_delay_us.store(2000, Ordering::Relaxed);
        for _ in 0..3 {
            let (result, n) = run();
            result.unwrap();
            assert_eq!(n, 0, "waiting for the helper allocated {n} times");
        }
    }

    #[test]
    fn a_failed_setup_gives_the_state_machine_and_the_pins_back() {
        let shared = Arc::new(Shared::default());
        *shared.fail.lock().unwrap() = Some("config_xfer32");
        assert!(PioBus::start(Mock(Arc::clone(&shared)), 8, 7, 400_000).is_err());
        let calls = shared.calls();
        for name in ["remove_program", "unclaim", "disable"] {
            assert!(calls.contains(&name), "{name} missing from {calls:?}");
        }
    }

    #[test]
    fn dropping_the_bus_releases_the_block() {
        let (bus, shared) = bus();
        drop(bus);
        // Nothing holds the device any more, the TX helper included: the client (and on
        // the module's route the claim) is gone when the drop returns, not some time after.
        assert_eq!(Arc::strong_count(&shared), 1);
        let calls = shared.calls();
        assert!(
            calls.ends_with(&[
                "disable",
                "pindirs",
                "gpio",
                "gpio",
                "gpio",
                "gpio",
                "remove_program",
                "unclaim"
            ]),
            "{calls:?}"
        );
    }

    #[test]
    fn a_pin_pair_that_is_not_two_gpios_is_refused_before_the_device_is_touched() {
        let shared = Arc::new(Shared::default());
        assert!(PioBus::start(Mock(Arc::clone(&shared)), 8, 8, 400_000).is_err());
        assert!(PioBus::start(Mock(Arc::clone(&shared)), 8, 28, 400_000).is_err());
        assert!(shared.calls().is_empty());
    }

    #[test]
    fn the_clock_divider_gives_400khz_and_100khz() {
        // 200 MHz / (24 * 400 kHz) = 20.833...: integer 20, fraction 0.833 * 256 = 213.
        assert_eq!(clkdiv_for(400_000).unwrap(), (20, 213));
        // 200 MHz / (24 * 100 kHz) = 83.333...
        assert_eq!(clkdiv_for(100_000).unwrap(), (83, 85));
        assert!(clkdiv_for(0).is_err());
        assert!(clkdiv_for(1).is_err());
    }

    #[test]
    fn sm_config_places_the_pins() {
        let config = sm_config(8, 7, 400_000).unwrap();
        // OUT and IN base 8, SET base 7 (count 1), SIDESET base 7 (count 1).
        assert_eq!(config.pinctrl & 0x1f, 8);
        assert_eq!((config.pinctrl >> 15) & 0x1f, 8);
        assert_eq!((config.pinctrl >> 10) & 0x1f, 7);
        assert_eq!((config.pinctrl >> 5) & 0x1f, 7);
        assert_eq!((config.pinctrl >> 29) & 0x7, 1);
        assert_eq!(
            config.execctrl & regs::SIDE_EN,
            0,
            "side-set is non-optional"
        );
        assert_ne!(config.execctrl & regs::SIDE_PINDIR, 0);
    }

    #[test]
    fn pin_mask_covers_both_pins() {
        assert_eq!(pin_mask(8, 7), 0x180);
    }

    #[test]
    fn the_clock_out_is_nine_bits_and_a_stop() {
        assert_eq!(CLOCK_OUT.len(), 10);
        assert!(CLOCK_OUT[..9].iter().all(|op| *op == Op::Write(0xff)));
        assert_eq!(CLOCK_OUT[9], Op::Stop);
    }
}
