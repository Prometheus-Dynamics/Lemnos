//! The board mode: measures the Linux backend's route on a real board, on hardware the caller
//! names and nothing else.
//!
//! `lemnos-module-check board [i2c=BUS:ADDR:REG:BYTES] [held=BUS:ADDR] [line=CHIP:LINE]
//! [pwm=CHIP:CHANNEL]`
//!
//! - `i2c`: a register read on a real chip, many times: a write of the one-byte register address,
//!   then a read of `BYTES` (1 to 4). Nothing else is written. Every read must give the first
//!   read's value, so name a register that does not change (a chip id).
//! - `held`: an address a kernel driver has bound. A one-byte read, which writes nothing, must
//!   be refused with `BUSY`.
//! - `line`: a GPIO line that is free, taken as an input with its bias left as it is, and read.
//!   It is never driven.
//! - `pwm`: a PWM channel that is free, configured with its output off, alternating two duties,
//!   and left off.
//!
//! The route is the module's when `/dev/lemnos` opens, and the stock one when it does not. The
//! `measure` lines are the same as the harness mode's.

use std::io::Write;

use lemnos_core::{Provider, Status};
use lemnos_hal::i2c::{I2c as _, Operation as I2cOp};
use lemnos_hal::raw::{Bias, EdgeDetect, LineConfig, Polarity, PwmConfig};
use lemnos_linux::LinuxBoard;
use lemnos_measure::test_alloc::allocations;

use crate::{REPEATS, Report, claims, measure};

/// One target of the board mode, parsed from `kind=value`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    I2c {
        bus: String,
        address: u8,
        register: u8,
        bytes: usize,
    },
    Held {
        bus: String,
        address: u8,
    },
    Line(String),
    Pwm(String),
}

fn number(text: &str) -> Option<u32> {
    match text.strip_prefix("0x") {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => text.parse().ok(),
    }
}

fn byte(text: &str) -> Option<u8> {
    u8::try_from(number(text)?).ok()
}

/// Parses one argument. `None` for anything it does not understand.
pub fn parse(arg: &str) -> Option<Target> {
    let (kind, value) = arg.split_once('=')?;
    let parts: Vec<&str> = value.split(':').collect();
    match (kind, parts.as_slice()) {
        ("i2c", [bus, address, register, bytes]) => {
            let bytes = usize::try_from(number(bytes)?).ok()?;
            (1..=4).contains(&bytes).then_some(())?;
            Some(Target::I2c {
                bus: (*bus).to_string(),
                address: byte(address).filter(|a| *a < 0x80)?,
                register: byte(register)?,
                bytes,
            })
        }
        ("held", [bus, address]) => Some(Target::Held {
            bus: (*bus).to_string(),
            address: byte(address).filter(|a| *a < 0x80)?,
        }),
        ("line", [chip, line]) => {
            number(line)?;
            Some(Target::Line(format!("{chip}:{line}")))
        }
        ("pwm", [chip, channel]) => {
            number(channel)?;
            Some(Target::Pwm(format!("{chip}:{channel}")))
        }
        _ => None,
    }
}

fn read_register(
    bus: &mut lemnos_core::I2c,
    address: u8,
    register: u8,
    out: &mut [u8],
) -> Result<(), lemnos_hal::ErrorKind> {
    bus.transaction(address, &mut [I2cOp::Write(&[register]), I2cOp::Read(out)])
}

fn check_i2c(
    report: &mut Report,
    board: &mut LinuxBoard,
    module: bool,
    bus_name: &str,
    address: u8,
    register: u8,
    bytes: usize,
) {
    report.check(board.provided(bus_name), "I2C bus provided");
    let before = claims();
    let Ok(mut bus) = board.i2c(bus_name) else {
        report.check(false, "I2C bus opened");
        return;
    };
    let mut first = [0u8; 4];
    let Some(first) = first.get_mut(..bytes) else {
        return;
    };
    let read = read_register(&mut bus, address, register, first);
    report.check(read.is_ok(), "the register reads");
    let _ = writeln!(
        std::io::stdout(),
        "     {bus_name} 0x{address:02x} register 0x{register:02x}: {first:02x?}"
    );
    report.check(
        claims() == before + usize::from(module),
        "I2C took the expected route",
    );
    let first = &*first;
    let (ok, allocated) = allocations(|| {
        (0..REPEATS).all(|_| {
            let mut out = [0u8; 4];
            out.get_mut(..bytes).is_some_and(|out| {
                read_register(&mut bus, address, register, out).is_ok() && out == first
            })
        })
    });
    report.check(ok, "the same value every time");
    report.check(allocated == 0, "register reads allocate nothing");
    measure(module, "i2c register read", |_| {
        let mut out = [0u8; 4];
        out.get_mut(..bytes)
            .is_some_and(|out| read_register(&mut bus, address, register, out).is_ok())
    });
}

fn check_held(report: &mut Report, board: &mut LinuxBoard, bus_name: &str, address: u8) {
    let Ok(mut bus) = board.i2c(bus_name) else {
        report.check(false, "the held address's bus opened");
        return;
    };
    let mut one = [0u8; 1];
    let read = bus.transaction(address, &mut [I2cOp::Read(&mut one)]);
    let _ = writeln!(
        std::io::stdout(),
        "     {bus_name} 0x{address:02x}, held by a kernel driver: {read:?}"
    );
    report.check(
        read == Err(lemnos_hal::ErrorKind::Busy),
        "an address a kernel driver holds is refused as busy",
    );
}

fn check_line(report: &mut Report, board: &mut LinuxBoard, module: bool, name: &str) {
    report.check(board.provided(name), "line provided");
    let before = claims();
    let Ok(mut line) = board.line(name) else {
        report.check(false, "line opened");
        return;
    };
    report.check(
        claims() == before + usize::from(module),
        "line took the expected route",
    );
    report.check(
        board.line(name).map(|_| ()) == Err(Status::BUSY),
        "a second owner is refused",
    );
    let input = LineConfig {
        edge: EdgeDetect::None,
        bias: Bias::AsIs,
        ..LineConfig::input()
    };
    report.check(line.configure(&input).is_ok(), "an input, bias as it is");
    let level = line.get();
    let _ = writeln!(std::io::stdout(), "     {name} reads {level:?}");
    report.check(level.is_ok(), "it reads");
    let (ok, allocated) = allocations(|| (0..REPEATS).all(|_| line.get().is_ok()));
    report.check(ok && allocated == 0, "reading allocates nothing");
    measure(module, "gpio get", |_| line.get().is_ok());
}

fn check_pwm(report: &mut Report, board: &mut LinuxBoard, module: bool, name: &str) {
    report.check(board.provided(name), "channel provided");
    let before = claims();
    let Ok(mut pwm) = board.pwm(name) else {
        report.check(false, "channel opened");
        return;
    };
    report.check(
        claims() == before + usize::from(module),
        "channel took the expected route",
    );
    let off = PwmConfig {
        period_ns: 1_000_000,
        duty_ns: 250_000,
        polarity: Polarity::Normal,
        enabled: false,
    };
    report.check(pwm.configure(&off).is_ok(), "configured, off");
    report.check(pwm.config() == Ok(off), "reads back");
    let (ok, allocated) = allocations(|| {
        (0..REPEATS).all(|n| {
            let duty_ns = if n % 2 == 0 { 250_000 } else { 300_000 };
            pwm.configure(&PwmConfig { duty_ns, ..off }).is_ok()
        })
    });
    report.check(ok && allocated == 0, "configuring allocates nothing");
    measure(module, "pwm configure (off)", |n| {
        let duty_ns = if n % 2 == 0 { 250_000 } else { 300_000 };
        pwm.configure(&PwmConfig { duty_ns, ..off }).is_ok()
    });
    report.check(pwm.configure(&off).is_ok(), "left off");
}

/// Runs the board mode on the targets named. Returns the number of failures.
pub fn run(targets: &[Target]) -> u32 {
    let mut report = Report { failures: 0 };
    let mut board = LinuxBoard::new();
    let module = board.open_module().is_ok();
    let _ = writeln!(
        std::io::stdout(),
        "route: {}",
        if module { "module" } else { "stock" }
    );
    for target in targets {
        match target {
            Target::I2c {
                bus,
                address,
                register,
                bytes,
            } => check_i2c(
                &mut report,
                &mut board,
                module,
                bus,
                *address,
                *register,
                *bytes,
            ),
            Target::Held { bus, address } => check_held(&mut report, &mut board, bus, *address),
            Target::Line(name) => check_line(&mut report, &mut board, module, name),
            Target::Pwm(name) => check_pwm(&mut report, &mut board, module, name),
        }
    }
    report.failures
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_parse() {
        assert_eq!(
            parse("i2c=i2c-1:0x40:0x3f:2"),
            Some(Target::I2c {
                bus: "i2c-1".into(),
                address: 0x40,
                register: 0x3f,
                bytes: 2
            })
        );
        assert_eq!(
            parse("held=i2c-10:0x60"),
            Some(Target::Held {
                bus: "i2c-10".into(),
                address: 0x60
            })
        );
        assert_eq!(
            parse("line=gpiochip15:27"),
            Some(Target::Line("gpiochip15:27".into()))
        );
        assert_eq!(
            parse("pwm=pwmchip0:1"),
            Some(Target::Pwm("pwmchip0:1".into()))
        );
    }

    #[test]
    fn anything_unclear_is_refused() {
        for arg in [
            "i2c=i2c-1:0x40:0x3f:5",
            "i2c=i2c-1:0x80:0x3f:1",
            "i2c=i2c-1:0x40:0x100:1",
            "i2c=i2c-1:0x40",
            "held=i2c-10",
            "line=gpiochip15:x",
            "pwm=pwmchip0",
            "spi=spidev0.0",
            "i2c-1",
        ] {
            assert_eq!(parse(arg), None, "{arg}");
        }
    }
}
