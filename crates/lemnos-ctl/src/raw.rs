//! `lemnos-ctl` raw access: `gpio`, `pwm`, `i2c`, `spi` and `restore`.
//!
//! Claims made here end when `lemnos-ctl` exits (the line goes to its safe
//! state, the channel off), so `gpio set` and `pwm set` hold until
//! interrupted, or for `--hold SECONDS`. With `--keep` they persist after
//! exit (the service keeps them under the `lemnos-ctl` name) until `gpio
//! release HANDLE` / `pwm release HANDLE`.

use super::{Args, fail};
use lemnos_ipc::raw::{
    Bias, Drive, EdgeDetect, LineConfig, Polarity, PwmConfig, SpiConfig, SpiMode,
};
use lemnos_ipc::{
    ClientEvent, ClientOptions, DeviceClient, Event, I2cOp, LineTarget, PwmTarget, SpiXfer, Update,
};
use std::process::ExitCode;
use std::time::Duration;

fn connect(options: ClientOptions, keep: bool) -> Result<DeviceClient, ExitCode> {
    let options = if keep {
        options.keep_intents()
    } else {
        options
    };
    options.devices().map_err(fail)
}

/// `chip:offset` or a line name.
fn line_target(text: &str) -> LineTarget {
    match text.rsplit_once(':') {
        Some((chip, offset)) if offset.parse::<u32>().is_ok() => LineTarget::Chip {
            chip: chip.to_string(),
            offset: offset.parse().unwrap_or_default(),
        },
        _ => LineTarget::Name(text.to_string()),
    }
}

/// `chip:channel` or a board PWM name.
fn pwm_target(text: &str) -> PwmTarget {
    match text
        .split_once(':')
        .and_then(|(c, n)| Some((c.parse().ok()?, n.parse().ok()?)))
    {
        Some((chip, channel)) => PwmTarget::Chip { chip, channel },
        None => PwmTarget::Name(text.to_string()),
    }
}

fn number(text: &str) -> Option<u64> {
    match text.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => text.parse().ok(),
    }
}

fn hex_bytes(text: &str) -> Option<Vec<u8>> {
    let text = text.trim_start_matches("0x");
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

fn print_hex(bytes: &[u8]) {
    let text: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
    println!("{}", text.join(" "));
}

/// Holds a claim: for `--hold` seconds, else until interrupted (with
/// `--keep`, returns at once and the claim persists).
fn hold(args: &mut Args, keep: bool, handle: u32) -> ExitCode {
    if keep {
        println!("handle {handle} (kept; release with `release {handle}`)");
        return ExitCode::SUCCESS;
    }
    match args.take("--hold").and_then(|s| s.parse::<f64>().ok()) {
        Some(seconds) => std::thread::sleep(Duration::from_secs_f64(seconds.max(0.0))),
        None => loop {
            std::thread::sleep(Duration::from_secs(3600));
        },
    }
    ExitCode::SUCCESS
}

fn line_config(args: &mut Args, mut config: LineConfig) -> Result<LineConfig, ExitCode> {
    config.bias = match args.take("--bias").as_deref() {
        None => Bias::AsIs,
        Some("up") => Bias::PullUp,
        Some("down") => Bias::PullDown,
        Some("off") => Bias::Disabled,
        Some(other) => return Err(fail(format!("--bias up|down|off, not {other}"))),
    };
    config.drive = match args.take("--drive").as_deref() {
        None | Some("push-pull") => Drive::PushPull,
        Some("open-drain") => Drive::OpenDrain,
        Some("open-source") => Drive::OpenSource,
        Some(other) => return Err(fail(format!("unknown drive {other}"))),
    };
    config.active_low = args.flag("--active-low");
    Ok(config)
}

pub(super) fn gpio(mut args: Args, options: ClientOptions) -> ExitCode {
    let keep = args.flag("--keep");
    let what = args.next().unwrap_or_default();
    let mut client = match connect(options, keep || what == "release") {
        Ok(c) => c,
        Err(code) => return code,
    };
    match what.as_str() {
        "get" => {
            let Some(line) = args.next() else {
                return fail("gpio get <chip:offset|name>");
            };
            let config = match line_config(&mut args, LineConfig::input()) {
                Ok(c) => c,
                Err(code) => return code,
            };
            match client
                .claim_line(line_target(&line), config)
                .and_then(|l| l.get(&mut client))
            {
                Ok(value) => {
                    println!("{}", u8::from(value));
                    ExitCode::SUCCESS
                }
                Err(e) => fail(e),
            }
        }
        "set" => {
            let (Some(line), Some(value)) = (args.next(), args.next()) else {
                return fail("gpio set <chip:offset|name> <0|1> [--hold S | --keep]");
            };
            let config = match line_config(&mut args, LineConfig::output(value == "1")) {
                Ok(c) => c,
                Err(code) => return code,
            };
            match client.claim_line(line_target(&line), config) {
                Ok(claimed) => hold(&mut args, keep, claimed.handle()),
                Err(e) => fail(e),
            }
        }
        "watch" => {
            let Some(line) = args.next() else {
                return fail("gpio watch <chip:offset|name> [--edge rising|falling|both]");
            };
            let edge = match args.take("--edge").as_deref() {
                None | Some("both") => EdgeDetect::Both,
                Some("rising") => EdgeDetect::Rising,
                Some("falling") => EdgeDetect::Falling,
                Some(other) => return fail(format!("unknown edge {other}")),
            };
            let count = args.take("--count").and_then(|c| c.parse::<u64>().ok());
            let config = match line_config(&mut args, LineConfig::input().with_edge(edge)) {
                Ok(c) => c,
                Err(code) => return code,
            };
            let claimed = match client.claim_line(line_target(&line), config) {
                Ok(l) => l,
                Err(e) => return fail(e),
            };
            let mut seen = 0;
            while count.is_none_or(|c| seen < c) {
                match client.next_event() {
                    Ok(ClientEvent::Data(Update::Event(Event::Edge {
                        handle,
                        rising,
                        timestamp_ns,
                        seq,
                    }))) if handle == claimed.handle() => {
                        let kind = if rising { "rising" } else { "falling" };
                        println!("{timestamp_ns} {kind} #{seq}");
                        seen += 1;
                    }
                    Ok(_) => {}
                    Err(e) => return fail(e),
                }
            }
            ExitCode::SUCCESS
        }
        "release" => release(&mut args, &mut client),
        _ => fail("gpio get|set|watch|release"),
    }
}

fn release(args: &mut Args, client: &mut DeviceClient) -> ExitCode {
    match args.next().and_then(|h| h.parse::<u32>().ok()) {
        Some(handle) => match client.unclaim(handle) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(e),
        },
        None => fail("release <handle>"),
    }
}

pub(super) fn pwm(mut args: Args, options: ClientOptions) -> ExitCode {
    let keep = args.flag("--keep");
    let what = args.next().unwrap_or_default();
    let mut client = match connect(options, keep || what == "release") {
        Ok(c) => c,
        Err(code) => return code,
    };
    match what.as_str() {
        "set" => {
            let Some(target) = args.next() else {
                return fail("pwm set <chip:channel|name> --period NS --duty NS [--inversed]");
            };
            let period = args.take("--period").as_deref().and_then(number);
            let duty = args.take("--duty").as_deref().and_then(number);
            let (Some(period_ns), Some(duty_ns)) = (period, duty) else {
                return fail("pwm set needs --period NS and --duty NS");
            };
            let config = PwmConfig {
                period_ns,
                duty_ns,
                polarity: if args.flag("--inversed") {
                    Polarity::Inversed
                } else {
                    Polarity::Normal
                },
                enabled: true,
            };
            match client
                .claim_pwm(pwm_target(&target))
                .and_then(|p| p.configure(&mut client, config).map(|()| p))
            {
                Ok(claimed) => hold(&mut args, keep, claimed.handle()),
                Err(e) => fail(e),
            }
        }
        "release" => release(&mut args, &mut client),
        _ => fail("pwm set|release"),
    }
}

pub(super) fn i2c(mut args: Args, options: ClientOptions) -> ExitCode {
    let what = args.next().unwrap_or_default();
    let (Some(bus), Some(address)) = (args.next(), args.next().as_deref().and_then(number)) else {
        return fail("i2c read|write|xfer <bus> <address> ...");
    };
    let mut client = match connect(options, false) {
        Ok(c) => c,
        Err(code) => return code,
    };
    let device = client.i2c(bus, address as u16);
    let result = match what.as_str() {
        // read <bus> <addr> <register> [count]
        "read" => {
            let register = args.next().as_deref().and_then(number);
            let count = args.next().as_deref().and_then(number).unwrap_or(1);
            match register {
                Some(register) => device.read_regs(&mut client, register as u8, count as u16),
                None => return fail("i2c read <bus> <address> <register> [count]"),
            }
        }
        // write <bus> <addr> <register> <byte>...
        "write" => {
            let bytes: Option<Vec<u8>> = std::iter::from_fn(|| args.next())
                .map(|b| number(&b).map(|v| v as u8))
                .collect();
            match bytes {
                Some(bytes) if !bytes.is_empty() => {
                    device.write(&mut client, &bytes).map(|()| Vec::new())
                }
                _ => return fail("i2c write <bus> <address> <register> <byte>..."),
            }
        }
        // xfer <bus> <addr> w:HEX r:N ...
        "xfer" => {
            let ops: Option<Vec<I2cOp>> = std::iter::from_fn(|| args.next())
                .map(|op| match op.split_once(':') {
                    Some(("w", hex)) => hex_bytes(hex).map(I2cOp::Write),
                    Some(("r", n)) => n.parse().ok().map(I2cOp::Read),
                    _ => None,
                })
                .collect();
            match ops {
                Some(ops) if !ops.is_empty() => device.transfer(&mut client, ops),
                _ => return fail("i2c xfer <bus> <address> w:HEX r:N ..."),
            }
        }
        _ => return fail("i2c read|write|xfer"),
    };
    match result {
        Ok(bytes) => {
            if !bytes.is_empty() {
                print_hex(&bytes);
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail(e),
    }
}

pub(super) fn spi(mut args: Args, options: ClientOptions) -> ExitCode {
    let mode = args
        .take("--mode")
        .and_then(|m| m.parse::<u8>().ok())
        .unwrap_or(0);
    let speed = args
        .take("--speed")
        .as_deref()
        .and_then(number)
        .unwrap_or(0);
    let what = args.next().unwrap_or_default();
    let target = args.next().and_then(|t| {
        let (bus, cs) = t.split_once('.')?;
        Some((bus.parse::<u32>().ok()?, cs.parse::<u16>().ok()?))
    });
    let tx = args.next().as_deref().and_then(hex_bytes);
    let (true, Some((bus, chip_select)), Some(tx)) = (what == "xfer", target, tx) else {
        return fail("spi xfer <bus.cs> <hex bytes> [--mode 0-3] [--speed HZ]");
    };
    let mut client = match connect(options, false) {
        Ok(c) => c,
        Err(code) => return code,
    };
    let config = SpiConfig {
        mode: SpiMode::from_bits(mode),
        speed_hz: speed as u32,
        bits_per_word: 8,
    };
    let rx_len = u16::try_from(tx.len()).unwrap_or(u16::MAX);
    let mut transfer = SpiXfer::new(tx, rx_len);
    transfer.config = config;
    match client
        .spi(bus, chip_select)
        .transfer(&mut client, vec![transfer])
    {
        Ok(bytes) => {
            print_hex(&bytes);
            ExitCode::SUCCESS
        }
        Err(e) => fail(e),
    }
}

/// `restore <device> [control]`: undoes `lemnos-ctl`'s earlier writes.
pub(super) fn restore(mut args: Args, options: ClientOptions) -> ExitCode {
    let Some(device) = args.next() else {
        return fail("restore <device> [control]");
    };
    let control = args.next();
    match connect(options, true).map(|mut c| c.restore(&device, control.as_deref())) {
        Ok(Ok(())) => ExitCode::SUCCESS,
        Ok(Err(e)) => fail(e),
        Err(code) => code,
    }
}
