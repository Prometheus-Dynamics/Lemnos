//! Drives the Linux backend's buses the way a host does, in the QEMU harness, and fails unless
//! each one took the expected route and behaved the same on it.
//!
//! `lemnos-module-check <route> <spi route> <gpio chip number> <simulated chip directory>`, where
//! a route is `module` or `stock`. The first covers I2C, GPIO and PWM.

use std::io::Write;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use lemnos_core::{Provider, Status};
use lemnos_hal::ErrorKind;
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
const TEST_BUS: &str = "i2c-40";
const STUB_ADDR: u8 = 0x30;
const MEM_ADDR: u8 = 0x50;
const HELD_ADDR: u8 = 0x51;
const NOBODY_ADDR: u8 = 0x60;
const REPEATS: usize = 200;

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
    report.check(pwm.configure(&idle).is_ok(), "left off");
    drop(pwm);
    report.check(board.pwm(&name).is_ok(), "dropping it let go");
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
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
        let second = LinuxBoard::new().open_module();
        report.check(
            second.is_err_and(|e| e.raw_os_error() == Some(libc::EBUSY)),
            "the node has one opener",
        );
    }

    check_smbus_adapter(&mut report, &mut board, module);
    check_plain_adapter(&mut report, &mut board, module);
    check_spi(&mut report, &mut board, spi_module);
    check_line(&mut report, &mut board, module, chip, dir);
    check_pwm(&mut report, &mut board, module);

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
