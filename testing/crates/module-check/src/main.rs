//! Drives the Linux backend's buses the way a host does, in the QEMU harness, and fails unless
//! each one took the expected route and behaved the same on it.
//!
//! `lemnos-module-check <route> <spi route> <gpio chip number> <simulated chip directory>`, where
//! a route is `module` or `stock`. The first covers I2C, GPIO and PWM, and the test block's DMA
//! memory, registers, interrupts, clock and reset, which must be bound to the module on the
//! module route, loaded with `test_map_unconfined` and `test_irq_inject` (on the stock route there
//! are none).
//!
//! Each bus also prints a `measure` line per operation: the time it takes on the route it took
//! (median and 99th percentile, from the monotonic clock) and what it allocated. They are numbers
//! to read, not checks: nothing fails on time.
//!
//! `lemnos-module-check board ...` is the mode for a real board: it touches only the devices
//! named on its command line, in the ways [`board`] lists, and never the harness's fixed test
//! addresses.

mod board;

use std::io::Write;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use lemnos_core::{Provider, Status};
use lemnos_hal::ErrorKind;
use lemnos_hal::blocks::{Clocks, DmaBuffers, Interrupt, Registers, Reset};
use lemnos_hal::i2c::{I2c as _, Operation as I2cOp};
use lemnos_hal::raw::{
    Bias, Direction, Edge, EdgeDetect, LineConfig, Polarity, PwmConfig, RawLine, SpiConfig,
    SpiMode, SpiSegment,
};
use lemnos_hal::spi::{Operation as SpiOp, SpiDevice as _};
use lemnos_linux::LinuxBoard;
use lemnos_measure::test_alloc::{Counting, allocations};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const SPI_NAME: &str = "spidev40.0";
/// DMA memory for the test block, which the harness binds to the module.
const DMA_NAME: &str = "dma:lemnos-test-block";
/// A block the module refuses to bind (not in `owned=`).
const DMA_NOT_OWNED: &str = "dma:lemnos-test-other";
/// The test block's register page, its interrupts, its clock and its reset line.
const REGS_NAME: &str = "regs:lemnos-test-block";
const IRQ0_NAME: &str = "irq:lemnos-test-block";
const IRQ1_NAME: &str = "irq:lemnos-test-block/1";
const CLOCK_NAME: &str = "clocks:lemnos-test-block/core";
const RESET_NAME: &str = "resets:lemnos-test-block/rst";
/// What the test block's module writes to word 0 of the block's register page.
const TEST_BLOCK_MAGIC: u32 = 0x4c45_4d4e;
/// The rate of the test clock, a fixed one.
const TEST_CLOCK_HZ: u64 = 25_000_000;
const TEST_BUS: &str = "i2c-40";
const STUB_ADDR: u8 = 0x30;
const MEM_ADDR: u8 = 0x50;
const HELD_ADDR: u8 = 0x51;
const NOBODY_ADDR: u8 = 0x60;
const REPEATS: usize = 200;
/// Runs of each measured operation, after `WARMUP` untimed ones.
const SAMPLES: usize = 2000;
const WARMUP: usize = 100;

struct Report {
    failures: u32,
}

impl Report {
    fn check(&mut self, ok: bool, what: &str) {
        let _ = writeln!(
            std::io::stdout(),
            "{} {what}",
            if ok { "ok  " } else { "FAIL" }
        );
        self.failures += u32::from(!ok);
    }
}

fn route(through_module: bool) -> &'static str {
    if through_module { "module" } else { "stock" }
}

/// Runs `op` many times and prints its median and 99th percentile time and its allocations.
fn measure(through_module: bool, what: &str, mut op: impl FnMut(usize) -> bool) {
    let mut times = [0u64; SAMPLES];
    let (ok, allocated) = allocations(|| {
        let mut ok = (0..WARMUP).all(&mut op);
        for (n, time) in times.iter_mut().enumerate() {
            let start = Instant::now();
            ok &= op(n);
            *time = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        }
        ok
    });
    times.sort_unstable();
    let _ = writeln!(
        std::io::stdout(),
        "measure {:<6} {what:<22} median {:>7} ns  p99 {:>7} ns  {allocated} allocations{}",
        route(through_module),
        times[SAMPLES / 2],
        times[SAMPLES * 99 / 100],
        if ok { "" } else { "  (some runs failed)" }
    );
}

/// Files this process has open whose target contains `text`.
fn open_files(text: &str) -> usize {
    std::fs::read_dir("/proc/self/fd")
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| std::fs::read_link(entry.path()).ok())
                .filter(|target| target.to_string_lossy().contains(text))
                .count()
        })
        .unwrap_or(0)
}

fn claims() -> usize {
    open_files("lemnos-claim")
}

/// The adapter `i2c-stub` registered, by name.
fn stub_bus() -> Option<String> {
    (0..64).map(|n| format!("i2c-{n}")).find(|name| {
        std::fs::read_to_string(format!("/sys/bus/i2c/devices/{name}/name"))
            .is_ok_and(|text| text.starts_with("SMBus stub"))
    })
}

/// The PWM chip the test module registered.
fn test_pwm() -> Option<String> {
    std::fs::read_dir("/sys/class/pwm")
        .ok()?
        .flatten()
        .find_map(|entry| {
            let parent = std::fs::read_link(entry.path().join("device")).ok()?;
            parent
                .ends_with("lemnos-test-parent")
                .then(|| entry.file_name().to_string_lossy().into_owned())
        })
}

fn check_smbus_adapter(report: &mut Report, board: &mut LinuxBoard, through_module: bool) {
    let Some(name) = stub_bus() else {
        report.check(false, "i2c-stub's adapter found");
        return;
    };
    report.check(board.provided(&name), "SMBus adapter provided");
    let before = claims();
    let Ok(mut bus) = board.i2c(&name) else {
        report.check(false, "SMBus adapter opened");
        return;
    };
    report.check(
        bus.transaction(STUB_ADDR, &mut [I2cOp::Write(&[0x10, 0xa5])])
            .is_ok(),
        "SMBus write byte",
    );
    report.check(
        claims() == before + usize::from(through_module),
        "SMBus adapter took the expected route",
    );
    let mut got = [0u8; 1];
    let read = bus.transaction(
        STUB_ADDR,
        &mut [I2cOp::Write(&[0x10]), I2cOp::Read(&mut got)],
    );
    report.check(read.is_ok() && got[0] == 0xa5, "SMBus read byte");
    report.check(
        bus.transaction(STUB_ADDR, &mut [I2cOp::Write(&[0x20, 1, 2, 3, 4])])
            .is_ok(),
        "SMBus write block",
    );
    let mut block = [0u8; 4];
    let read = bus.transaction(
        STUB_ADDR,
        &mut [I2cOp::Write(&[0x20]), I2cOp::Read(&mut block)],
    );
    report.check(read.is_ok() && block == [1, 2, 3, 4], "SMBus read block");
    let (ok, allocated) = allocations(|| {
        (0..REPEATS).all(|_| {
            let mut out = [0u8; 4];
            bus.transaction(
                STUB_ADDR,
                &mut [I2cOp::Write(&[0x20]), I2cOp::Read(&mut out)],
            )
            .is_ok()
        })
    });
    report.check(ok && allocated == 0, "SMBus reads allocate nothing");
    measure(through_module, "i2c smbus write+read", |_| {
        let mut out = [0u8; 1];
        bus.transaction(
            STUB_ADDR,
            &mut [I2cOp::Write(&[0x10]), I2cOp::Read(&mut out)],
        )
        .is_ok()
    });
}

fn check_plain_adapter(report: &mut Report, board: &mut LinuxBoard, through_module: bool) {
    let Ok(mut bus) = board.i2c(TEST_BUS) else {
        report.check(false, "test adapter opened");
        return;
    };
    let before = claims();
    report.check(
        bus.transaction(
            MEM_ADDR,
            &mut [I2cOp::Write(&[0x10, 0xde, 0xad, 0xbe, 0xef])],
        )
        .is_ok(),
        "write",
    );
    report.check(
        claims() == before + usize::from(through_module),
        "test adapter took the expected route",
    );
    let mut got = [0u8; 4];
    let read = bus.transaction(
        MEM_ADDR,
        &mut [I2cOp::Write(&[0x10]), I2cOp::Read(&mut got)],
    );
    report.check(
        read.is_ok() && got == [0xde, 0xad, 0xbe, 0xef],
        "write then read",
    );
    let joined = bus.transaction(
        MEM_ADDR,
        &mut [I2cOp::Write(&[0x20]), I2cOp::Write(&[0x11, 0x22])],
    );
    let mut back = [0u8; 2];
    let read = bus.transaction(
        MEM_ADDR,
        &mut [I2cOp::Write(&[0x20]), I2cOp::Read(&mut back)],
    );
    report.check(
        joined.is_ok() && read.is_ok() && back == [0x11, 0x22],
        "adjacent writes are one write",
    );
    report.check(
        bus.transaction(HELD_ADDR, &mut [I2cOp::Write(&[0])]) == Err(ErrorKind::Busy),
        "an address a kernel driver holds is busy",
    );
    let held = claims();
    let nobody = bus.transaction(NOBODY_ADDR, &mut [I2cOp::Write(&[0])]);
    report.check(
        nobody == Err(ErrorKind::from_errno(libc::ENXIO)),
        "nothing answering reads the same",
    );
    report.check(claims() == held, "an address that didn't answer isn't kept");
    let (ok, allocated) = allocations(|| {
        (0..REPEATS).all(|_| {
            let mut out = [0u8; 4];
            bus.transaction(
                MEM_ADDR,
                &mut [I2cOp::Write(&[0x10]), I2cOp::Read(&mut out)],
            )
            .is_ok()
        })
    });
    report.check(ok && allocated == 0, "transfers allocate nothing");
    measure(through_module, "i2c write+read", |_| {
        let mut out = [0u8; 4];
        bus.transaction(
            MEM_ADDR,
            &mut [I2cOp::Write(&[0x10]), I2cOp::Read(&mut out)],
        )
        .is_ok()
    });
}

fn check_spi(report: &mut Report, board: &mut LinuxBoard, through_module: bool) {
    report.check(board.provided(SPI_NAME), "SPI device provided");
    let before = claims();
    let Ok(mut spi) = board.spi(SPI_NAME) else {
        report.check(false, "SPI device opened");
        return;
    };
    report.check(
        claims() == before + usize::from(through_module),
        "SPI device took the expected route",
    );
    report.check(
        board.spi(SPI_NAME).map(|_| ()) == Err(Status::BUSY),
        "a second owner is refused",
    );
    let mut buffer = [1u8, 2, 3, 4, 5, 6, 7, 8];
    let done = spi.transaction(&mut [SpiOp::TransferInPlace(&mut buffer)]);
    report.check(
        done.is_ok() && buffer == [1, 2, 3, 4, 5, 6, 7, 8],
        "in place",
    );
    let mut read = [0u8; 2];
    let done = spi.transaction(&mut [
        SpiOp::Write(&[9, 9]),
        SpiOp::DelayNs(2000),
        SpiOp::Transfer(&mut read, &[7, 8]),
    ]);
    report.check(done.is_ok() && read == [7, 8], "write, delay, duplex");
    let mut shorter = [0u8; 2];
    let done = spi.transaction(&mut [SpiOp::Transfer(&mut shorter, &[3, 4, 5, 6])]);
    report.check(
        done.is_ok() && shorter == [3, 4],
        "unequal halves are padded",
    );
    let (ok, allocated) = allocations(|| {
        (0..REPEATS).all(|_| {
            let mut out = [1u8, 2, 3, 4];
            spi.transaction(&mut [SpiOp::TransferInPlace(&mut out)])
                .is_ok()
                && out == [1, 2, 3, 4]
        })
    });
    report.check(ok && allocated == 0, "SPI transfers allocate nothing");
    measure(through_module, "spi duplex 4 bytes", |_| {
        let mut out = [0u8; 4];
        spi.transaction(&mut [SpiOp::Transfer(&mut out, &[1, 2, 3, 4])])
            .is_ok()
            && out == [1, 2, 3, 4]
    });
    drop(spi);

    let Ok(mut raw) = board.raw_spi(SPI_NAME) else {
        report.check(false, "raw SPI opened");
        return;
    };
    let config = SpiConfig {
        mode: SpiMode::Mode3,
        speed_hz: 1_000_000,
        bits_per_word: 8,
    };
    let mut rx = [0u8; 2];
    let done = raw.transfer(&mut [
        SpiSegment {
            config,
            tx: &[1, 2],
            rx: &mut [],
            cs_change: false,
            delay_us: 3,
        },
        SpiSegment {
            config,
            tx: &[5, 6],
            rx: &mut rx,
            cs_change: false,
            delay_us: 0,
        },
    ]);
    report.check(done.is_ok() && rx == [5, 6], "raw segments in mode 3");
    let mode0 = SpiConfig {
        mode: SpiMode::Mode0,
        ..config
    };
    let mixed = raw.transfer(&mut [
        SpiSegment {
            config,
            tx: &[1],
            rx: &mut [],
            cs_change: false,
            delay_us: 0,
        },
        SpiSegment {
            config: mode0,
            tx: &[1],
            rx: &mut [],
            cs_change: false,
            delay_us: 0,
        },
    ]);
    report.check(mixed.is_err(), "one message has one mode");
    drop(raw);
    report.check(board.spi(SPI_NAME).is_ok(), "dropping it let go");
}

fn sim_value(dir: &str, line: u32) -> Option<u32> {
    std::fs::read_to_string(format!("{dir}/sim_gpio{line}/value"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn sim_pull(dir: &str, line: u32, how: &str) -> bool {
    std::fs::write(format!("{dir}/sim_gpio{line}/pull"), how).is_ok()
}

fn next_edge(line: &mut (impl RawLine + ?Sized)) -> Option<Edge> {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(2) {
        match line.read_edge() {
            Ok(Some(edge)) => return Some(edge),
            Ok(None) => std::thread::sleep(Duration::from_millis(1)),
            Err(_) => return None,
        }
    }
    None
}

fn output(initial: bool, active_low: bool) -> LineConfig {
    LineConfig {
        direction: Direction::Output,
        initial,
        active_low,
        ..LineConfig::input()
    }
}

fn input(active_low: bool, edge: EdgeDetect) -> LineConfig {
    LineConfig {
        active_low,
        edge,
        bias: Bias::AsIs,
        ..LineConfig::input()
    }
}

fn check_edges(
    report: &mut Report,
    line: &mut (impl RawLine + ?Sized),
    dir: &str,
    active_low: bool,
) {
    let label = if active_low {
        "active-low"
    } else {
        "active-high"
    };
    // The simulated line ignores a pull while it's an output.
    report.check(
        line.configure(&input(false, EdgeDetect::None)).is_ok(),
        "an input",
    );
    report.check(sim_pull(dir, 1, "pull-down"), "line pulled down");
    report.check(
        line.configure(&input(active_low, EdgeDetect::Both)).is_ok(),
        "edges on",
    );
    report.check(line.get() == Ok(active_low), "reads the pull, logically");
    report.check(sim_pull(dir, 1, "pull-up"), "line pulled up");
    let first = next_edge(line);
    report.check(
        first.is_some_and(|edge| edge.rising != active_low && edge.timestamp_ns > 0),
        &format!("{label}: the rise is seen, logically"),
    );
    report.check(sim_pull(dir, 1, "pull-down"), "line pulled down");
    let second = next_edge(line);
    report.check(
        second.is_some_and(|edge| edge.rising == active_low),
        &format!("{label}: the fall is seen, logically"),
    );
    report.check(
        matches!((first, second), (Some(a), Some(b)) if b.seq == a.seq + 1),
        &format!("{label}: sequence numbers count up"),
    );
}

fn check_line(
    report: &mut Report,
    board: &mut LinuxBoard,
    through_module: bool,
    chip: &str,
    dir: &str,
) {
    let name = format!("gpiochip{chip}:1");
    report.check(board.provided(&name), "line provided");
    let before = claims();
    let Ok(mut line) = board.line(&name) else {
        report.check(false, "line opened");
        return;
    };
    report.check(
        claims() == before + usize::from(through_module),
        "line took the expected route",
    );
    report.check(
        board.line(&name).map(|_| ()) == Err(Status::BUSY),
        "a second owner is refused",
    );
    report.check(
        line.configure(&output(true, false)).is_ok(),
        "output, starting high",
    );
    report.check(
        line.get() == Ok(true) && sim_value(dir, 1) == Some(1),
        "it drives high",
    );
    report.check(
        line.set(false).is_ok() && sim_value(dir, 1) == Some(0),
        "it drives low",
    );
    report.check(
        line.configure(&output(true, true)).is_ok(),
        "active-low output",
    );
    report.check(
        line.get() == Ok(true) && sim_value(dir, 1) == Some(0),
        "logical 1 drives low",
    );
    let (ok, allocated) =
        allocations(|| (0..REPEATS).all(|n| line.set(n % 2 == 0).is_ok() && line.get().is_ok()));
    report.check(ok && allocated == 0, "setting and reading allocate nothing");
    measure(through_module, "gpio set", |n| line.set(n % 2 == 0).is_ok());
    measure(through_module, "gpio get", |_| line.get().is_ok());
    check_edges(report, &mut *line, dir, false);
    check_edges(report, &mut *line, dir, true);
    let quiet = line.configure(&input(false, EdgeDetect::None));
    report.check(
        quiet.is_ok() && line.read_edge() == Ok(None),
        "no edges pending",
    );
    report.check(line.set(true).is_err(), "setting an input is refused");
    drop(line);
    report.check(board.line(&name).is_ok(), "dropping it let go");
}

fn check_pwm(report: &mut Report, board: &mut LinuxBoard, through_module: bool) {
    let Some(chip) = test_pwm() else {
        report.check(false, "test PWM chip found");
        return;
    };
    let name = format!("{chip}:0");
    report.check(board.provided(&name), "channel provided");
    let before = claims();
    let Ok(mut pwm) = board.pwm(&name) else {
        report.check(false, "channel opened");
        return;
    };
    report.check(
        claims() == before + usize::from(through_module),
        "channel took the expected route",
    );
    report.check(
        board.pwm(&name).map(|_| ()) == Err(Status::BUSY),
        "a second owner is refused",
    );
    let running = PwmConfig {
        period_ns: 1_000_000,
        duty_ns: 250_000,
        polarity: Polarity::Normal,
        enabled: true,
    };
    report.check(pwm.configure(&running).is_ok(), "applied");
    report.check(pwm.config() == Ok(running), "reads back");
    let inverted = PwmConfig {
        period_ns: 500_000,
        duty_ns: 100_000,
        polarity: Polarity::Inversed,
        enabled: false,
    };
    report.check(
        pwm.configure(&inverted).is_ok() && pwm.config() == Ok(inverted),
        "shorter period, inverted, off",
    );
    let impossible = PwmConfig {
        duty_ns: 2_000_000,
        ..running
    };
    report.check(
        pwm.configure(&impossible).is_err(),
        "more duty than period is refused",
    );
    let idle = PwmConfig {
        polarity: Polarity::Normal,
        ..inverted
    };
    measure(through_module, "pwm configure", |n| {
        let duty_ns = if n % 2 == 0 { 250_000 } else { 300_000 };
        pwm.configure(&PwmConfig { duty_ns, ..running }).is_ok()
    });
    report.check(pwm.configure(&idle).is_ok(), "left off");
    drop(pwm);
    report.check(board.pwm(&name).is_ok(), "dropping it let go");
}

/// DMA memory through the module for the test block: the address the provider hands out is the
/// one the module reports for the block, the CPU sees the memory, and it is let go on release and
/// on drop. Without the module there is none.
fn check_dma(report: &mut Report, board: &mut LinuxBoard, through_module: bool) {
    if !through_module {
        report.check(
            !board.provided(DMA_NAME),
            "no DMA memory without the module",
        );
        report.check(
            board.dma(DMA_NAME).err() == Some(Status::NOT_SUPPORTED),
            "DMA memory refused without the module (NOT_SUPPORTED)",
        );
        return;
    }
    report.check(
        board.provided(DMA_NAME),
        "the bound block's DMA memory is provided",
    );
    report.check(
        !board.provided(DMA_NOT_OWNED) && board.dma(DMA_NOT_OWNED).err() == Some(Status::NO_DEVICE),
        "a block the module doesn't own has none (NO_DEVICE)",
    );
    report.check(
        board.dma("dma:").err() == Some(Status::NOT_FOUND),
        "a name with no block is refused (NOT_FOUND)",
    );
    let Ok(mut dma) = board.module_dma(DMA_NAME) else {
        report.check(false, "DMA memory opened");
        return;
    };
    let before = open_files("dmabuf");
    let Ok(buffer) = dma.allocate(10_000) else {
        report.check(false, "10000 bytes allocated");
        return;
    };
    let _ = writeln!(
        std::io::stdout(),
        "dma: buffer {} of {} bytes at device address {:#x}",
        buffer.id,
        buffer.len,
        buffer.device_address
    );
    report.check(
        buffer.len >= 10_000 && buffer.len % 4096 == 0 && open_files("dmabuf") == before + 1,
        "allocated: at least what was asked, in pages, one dma-buf held",
    );
    let imported = dma.import_address(buffer.id);
    let _ = writeln!(
        std::io::stdout(),
        "dma: the module maps it on the block again at {imported:#x?}"
    );
    report.check(
        buffer.device_address != 0
            && buffer.device_address % 4096 == 0
            && imported.is_ok_and(|address| address == buffer.device_address),
        "the address is the one the module reports for the block",
    );
    let pattern: Vec<u8> = (0..buffer.len).map(|i| (i * 7) as u8).collect();
    let mut back = vec![0; pattern.len()];
    report.check(
        dma.write(buffer.id, 0, &pattern).is_ok()
            && dma.read(buffer.id, 0, &mut back).is_ok()
            && back == pattern,
        "the CPU writes and reads the whole buffer",
    );
    report.check(
        dma.read(buffer.id, buffer.len - 1, &mut [0; 2]) == Err(ErrorKind::InvalidInput),
        "a read past the end is refused",
    );
    report.check(
        dma.sync_for_device(buffer.id).is_ok()
            && dma.read(buffer.id, 0, &mut [0; 1]) == Err(ErrorKind::Busy)
            && dma.sync_for_cpu(buffer.id).is_ok()
            && dma.read(buffer.id, 0, &mut [0; 1]).is_ok(),
        "the CPU keeps out while the block has it",
    );
    let mut word = [0u8; 64];
    let ((), allocated) = lemnos_measure::test_alloc::allocations(|| {
        for i in 0..100u64 {
            let _ = dma.sync_for_cpu(buffer.id);
            let _ = dma.write(buffer.id, i * 64, &word);
            let _ = dma.read(buffer.id, i * 64, &mut word);
            let _ = dma.sync_for_device(buffer.id);
        }
    });
    report.check(
        allocated == 0,
        "syncing, reading and writing allocate nothing",
    );
    report.check(
        dma.release(buffer.id).is_ok()
            && open_files("dmabuf") == before
            && dma.release(buffer.id) == Err(ErrorKind::NotFound),
        "released: the dma-buf is closed, and a second release is refused",
    );
    let kept = dma.allocate(4096).is_ok() && dma.allocate(4096).is_ok();
    report.check(
        kept && open_files("dmabuf") == before + 2,
        "two more allocated",
    );
    drop(dma);
    report.check(
        open_files("dmabuf") == before,
        "dropping the memory lets every buffer go",
    );
}

/// The test block's registers, interrupts, clock and reset through the module, by board-file
/// names: what is handed out is the block's own, and nothing is handed out without the module.
fn check_block(report: &mut Report, board: &mut LinuxBoard, through_module: bool) {
    let names = [REGS_NAME, IRQ0_NAME, CLOCK_NAME, RESET_NAME];
    if !through_module {
        report.check(
            names.iter().all(|name| !board.provided(name)),
            "no block registers, interrupts, clocks or resets without the module",
        );
        report.check(
            board.registers(REGS_NAME).err() == Some(Status::NOT_SUPPORTED)
                && board.interrupt(IRQ0_NAME).err() == Some(Status::NOT_SUPPORTED)
                && board.clocks(CLOCK_NAME).err() == Some(Status::NOT_SUPPORTED)
                && board.reset(RESET_NAME).err() == Some(Status::NOT_SUPPORTED),
            "each is refused without the module (NOT_SUPPORTED)",
        );
        return;
    }
    report.check(
        names.iter().all(|name| board.provided(name)),
        "the bound block's registers, interrupt, clock and reset are provided",
    );
    report.check(
        !board.provided("regs:lemnos-test-other")
            && board.registers("regs:lemnos-test-other").err() == Some(Status::NO_DEVICE)
            && board.interrupt("irq:lemnos-test-other").err() == Some(Status::NO_DEVICE)
            && board.reset("resets:lemnos-test-other/rst").err() == Some(Status::NO_DEVICE),
        "a block the module doesn't own has none (NO_DEVICE)",
    );
    report.check(
        board.registers("regs:lemnos-test-block/1").err() == Some(Status::NOT_FOUND)
            && board.interrupt("irq:lemnos-test-block/2").err() == Some(Status::NOT_FOUND)
            && board.registers("regs:").err() == Some(Status::NOT_FOUND),
        "a region or interrupt the block doesn't have is refused (NOT_FOUND)",
    );
    report.check(
        board.registers(REGS_NAME).is_ok()
            && board.interrupt(IRQ0_NAME).is_ok()
            && board.clocks(CLOCK_NAME).is_ok()
            && board.reset(RESET_NAME).is_ok(),
        "each opens through the driver interface",
    );
    check_registers(report, board);
    check_interrupts(report, board);
    check_power(report, board);
}

fn check_registers(report: &mut Report, board: &mut LinuxBoard) {
    let Ok(mut regs) = board.module_registers(REGS_NAME) else {
        report.check(false, "registers mapped");
        return;
    };
    report.check(
        board.module_registers(REGS_NAME).err() == Some(Status::BUSY),
        "one holder of a register region at a time (BUSY)",
    );
    let magic = regs.read32(0);
    let _ = writeln!(
        std::io::stdout(),
        "registers: {} bytes, word 0 {:#x}",
        regs.size(),
        magic.unwrap_or(0)
    );
    report.check(
        regs.size() == 4096 && magic == Ok(TEST_BLOCK_MAGIC),
        "the region is the test block's page: its module's marker is at word 0",
    );
    let written = regs.write32(0x40, 0xa5a5_0001).is_ok()
        && regs.modify32(0x40, 0xff, 0x7e).is_ok()
        && regs.read32(0x40) == Ok(0xa5a5_007e);
    report.check(written, "a register write reads back");
    report.check(
        regs.read32(4096) == Err(ErrorKind::InvalidInput)
            && regs.write32(4094, 1) == Err(ErrorKind::InvalidInput)
            && regs.read32(2) == Err(ErrorKind::InvalidInput),
        "a word past the end or out of line is refused",
    );
    let ((), allocated) = allocations(|| {
        for i in 0..1000u32 {
            let _ = regs.write32(0x80, i);
            let _ = regs.read32(0x80);
        }
    });
    report.check(allocated == 0, "register reads and writes allocate nothing");
    measure(true, "register read32", |_| regs.read32(0).is_ok());
    drop(regs);
    // A fresh mapping sees what the first wrote: it went to the block's memory, not a copy.
    let Ok(mut again) = board.module_registers(REGS_NAME) else {
        report.check(false, "registers mapped again");
        return;
    };
    report.check(
        again.read32(0x40) == Ok(0xa5a5_007e) && again.read32(0x80) == Ok(999),
        "the writes landed in the block's register page",
    );
    let _ = again.write32(0x40, 0);
    let _ = again.write32(0x80, 0);
}

/// Waits for `irq`, up to `ms`.
fn fired(irq: &mut impl Interrupt<Error = ErrorKind>, ms: u64) -> Option<u32> {
    irq.wait(Some(Duration::from_millis(ms)))
        .ok()
        .flatten()
        .map(|event| event.count)
}

fn check_interrupts(report: &mut Report, board: &mut LinuxBoard) {
    let (Ok(mut irq0), Ok(mut irq1)) = (
        board.module_interrupt(IRQ0_NAME),
        board.module_interrupt(IRQ1_NAME),
    ) else {
        report.check(false, "both interrupts opened");
        return;
    };
    report.check(
        board.module_interrupt(IRQ0_NAME).err() == Some(Status::BUSY),
        "one holder of an interrupt at a time (BUSY)",
    );
    report.check(
        irq0.wait(Some(Duration::ZERO)) == Ok(None),
        "nothing waiting at first",
    );
    report.check(
        irq0.enable().is_ok()
            && irq0.inject_for_test().is_ok()
            && fired(&mut irq0, 1000) == Some(1),
        "an injected interrupt is delivered",
    );
    report.check(
        irq0.inject_for_test().is_ok() && fired(&mut irq0, 200).is_none(),
        "masked once it fired: the next is not delivered before enable",
    );
    // The one injected while masked may be resent on enable, depending on the controller.
    let resent = irq0.enable().is_ok() && fired(&mut irq0, 200).is_some();
    let _ = writeln!(
        std::io::stdout(),
        "interrupt: the masked one was {}resent on enable",
        if resent { "" } else { "not " }
    );
    report.check(
        irq0.enable().is_ok() && irq0.inject_for_test().is_ok() && fired(&mut irq0, 1000).is_some(),
        "re-enabled, the next one is delivered",
    );
    report.check(
        irq0.enable().is_ok()
            && irq1.enable().is_ok()
            && irq1.inject_for_test().is_ok()
            && fired(&mut irq1, 1000).is_some()
            && fired(&mut irq0, 100).is_none(),
        "each interrupt gets its own events",
    );
    report.check(
        irq0.disable().is_ok()
            && irq0.inject_for_test().is_ok()
            && fired(&mut irq0, 200).is_none()
            && irq0.enable().is_ok()
            && fired(&mut irq0, 100).is_none(),
        "disabled, what fires is dropped",
    );
    let ((), allocated) = allocations(|| {
        for _ in 0..100 {
            let _ = irq0.enable();
            let _ = irq0.inject_for_test();
            let _ = irq0.wait(Some(Duration::from_secs(1)));
        }
    });
    report.check(allocated == 0, "enabling and waiting allocate nothing");
    measure(true, "interrupt round trip", |_| {
        irq0.enable().is_ok() && irq0.inject_for_test().is_ok() && fired(&mut irq0, 1000).is_some()
    });
}

fn check_power(report: &mut Report, board: &mut LinuxBoard) {
    report.check(
        board.clocks("clocks:lemnos-test-block/nope").err() == Some(Status::NOT_FOUND),
        "a clock the block doesn't have is refused when opened (NOT_FOUND)",
    );
    report.check(
        board.reset("resets:lemnos-test-block/nope").is_err(),
        "a reset line the block doesn't have is refused when opened",
    );
    match board.module_clocks(CLOCK_NAME) {
        Ok(mut clocks) => {
            report.check(
                clocks.rate_hz(0) == Ok(TEST_CLOCK_HZ),
                "the clock's rate reads back",
            );
            report.check(
                clocks.enable(0).is_ok()
                    && clocks.is_on(0) == Ok(true)
                    && clocks.disable(0).is_ok()
                    && clocks.is_on(0) == Ok(false),
                "the clock turns on and off, and reads back each time",
            );
            report.check(
                clocks.enable(1) == Err(ErrorKind::NotFound),
                "a clock index past the names is refused",
            );
            report.check(
                board.module_clocks(CLOCK_NAME).err() == Some(Status::BUSY),
                "one holder of a clock at a time (BUSY)",
            );
        }
        Err(status) => report.check(false, &format!("clock opened ({status:?})")),
    }
    match board.module_reset(RESET_NAME) {
        Ok(mut reset) => {
            report.check(
                reset.assert(0).is_ok()
                    && reset.is_asserted(0) == Ok(true)
                    && reset.deassert(0).is_ok()
                    && reset.is_asserted(0) == Ok(false),
                "the reset line asserts and releases, and reads back each time",
            );
            let ((), allocated) = allocations(|| {
                for _ in 0..100 {
                    let _ = reset.assert(0);
                    let _ = reset.deassert(0);
                }
            });
            report.check(allocated == 0, "toggling the reset allocates nothing");
            report.check(
                reset.is_asserted(0) == Ok(false),
                "the reset is left released",
            );
        }
        Err(status) => report.check(false, &format!("reset opened ({status:?})")),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [mode, targets @ ..] = args.as_slice()
        && mode == "board"
    {
        let Some(targets) = targets
            .iter()
            .map(|t| board::parse(t))
            .collect::<Option<Vec<_>>>()
        else {
            return ExitCode::from(2);
        };
        let failures = board::run(&targets);
        let _ = writeln!(
            std::io::stdout(),
            "lemnos-module-check board: {failures} failed"
        );
        return if failures == 0 {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    let [route, spi_route, chip, dir] = args.as_slice() else {
        return ExitCode::from(2);
    };
    let module = route == "module";
    let spi_module = spi_route == "module";
    let mut report = Report { failures: 0 };

    let mut board = LinuxBoard::new();
    report.check(
        open_files("/dev/lemnos") == 0,
        "a plain board leaves the module alone",
    );
    let opened = board.open_module();
    report.check(
        opened.is_ok() == module,
        "the module opens exactly when it's loaded",
    );
    if module {
        // The module has one opener; within it, every board and driver shares the one open
        // file. Another process is refused (`lemnos_claim_check`).
        let mut second = LinuxBoard::new();
        report.check(
            second.open_module().is_ok() && open_files("/dev/lemnos") == 1,
            "a second board in the process shares the one open file",
        );
        drop(second);
        report.check(
            open_files("/dev/lemnos") == 1,
            "and the first board still holds it",
        );
    }

    check_smbus_adapter(&mut report, &mut board, module);
    check_plain_adapter(&mut report, &mut board, module);
    check_spi(&mut report, &mut board, spi_module);
    check_line(&mut report, &mut board, module, chip, dir);
    check_pwm(&mut report, &mut board, module);
    check_dma(&mut report, &mut board, module);
    check_block(&mut report, &mut board, module);

    let _ = writeln!(
        std::io::stdout(),
        "lemnos-module-check {route} {spi_route}: {} failed",
        report.failures
    );
    if report.failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
