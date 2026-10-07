//! `lemnos-ctl`: the command-line client of `lemnosd`, for scripts and
//! other runtimes (PhotonVision's GPIO commands, Atlas's self-test).
//!
//! ```text
//! lemnos-ctl [--socket PATH] [--client NAME] [--priority N] <command>
//!   list
//!   read <device>
//!   watch <device> [--period MS] [--count N]
//!   set <device> <control> <value>
//!   get <device> <control>
//!   led status <ok|warn|error|busy|off> [led options]
//!   led color <RRGGBB> [led options]
//!   led brightness <0..1>
//!   led pixel <index> <RRGGBB> [<index> <RRGGBB>...]
//!   led frame <RRGGBB,RRGGBB,...>
//!   led progress <0..1> [--color RRGGBB] [--background RRGGBB]
//!   led spinner [--color RRGGBB]
//!   led system <updating [0..1] [--phase P]|booting|rebooting|update-failed|rolled-back>
//!   led locate [--seconds N]
//!   led off
//!   fan restore [--board PATH] [--all]
//!   validate <board.toml>...
//! led options: --device ID --effect solid|blink|breathe|chase --blink
//!   --period MS --depth 0..1 --fade MS --easing NAME --brightness 0..1 --seconds N
//! ```
//!
//! LED intents from `lemnos-ctl` stay after it exits, until replaced or
//! cleared with `led off` (one intent per client name and layer).
#![allow(clippy::print_stdout, clippy::print_stderr)]

use lemnos_board::{BoardDefinition, DriverRegistry};
use lemnos_ipc::{
    ClientEvent, ClientOptions, DEFAULT_SOCKET, Easing, EffectKind, LedRequest, LedShow, LedStatus,
    Phase, SystemState, Update,
};
use std::process::ExitCode;
use std::time::Duration;

struct Args {
    words: Vec<String>,
}

impl Args {
    /// Removes `--name VALUE` and returns the value.
    fn take(&mut self, name: &str) -> Option<String> {
        let i = self.words.iter().position(|w| w == name)?;
        self.words.remove(i);
        (i < self.words.len()).then(|| self.words.remove(i))
    }

    /// Removes a bare `--name`.
    fn flag(&mut self, name: &str) -> bool {
        match self.words.iter().position(|w| w == name) {
            Some(i) => {
                self.words.remove(i);
                true
            }
            None => false,
        }
    }

    fn next(&mut self) -> Option<String> {
        (!self.words.is_empty()).then(|| self.words.remove(0))
    }
}

fn fail(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("lemnos-ctl: {message}");
    ExitCode::FAILURE
}

fn color(text: &str) -> Option<u32> {
    u32::from_str_radix(text.trim_start_matches('#').trim_start_matches("0x"), 16).ok()
}

fn permille(text: &str) -> Option<u16> {
    text.parse::<f64>()
        .ok()
        .filter(|v| (0.0..=1.0).contains(v))
        .map(|v| (v * 1000.0).round() as u16)
}

fn main() -> ExitCode {
    let mut args = Args {
        words: std::env::args().skip(1).collect(),
    };
    let socket = args
        .take("--socket")
        .unwrap_or_else(|| DEFAULT_SOCKET.into());
    let client = args.take("--client").unwrap_or_else(|| "lemnos-ctl".into());
    let priority = args
        .take("--priority")
        .and_then(|p| p.parse().ok())
        .unwrap_or(50);
    let options = ClientOptions::new(&socket, client).priority(priority);
    let Some(command) = args.next() else {
        return fail("no command (list, read, watch, set, get, led, fan, validate)");
    };
    match command.as_str() {
        "validate" => validate(args),
        "fan" => fan(args),
        "led" => led(args, options),
        _ => devices(&command, args, options),
    }
}

fn validate(mut args: Args) -> ExitCode {
    let registry = DriverRegistry::builtin();
    let mut failed = false;
    while let Some(path) = args.next() {
        match BoardDefinition::from_path(&path).and_then(|b| b.validate(&registry).map(|()| b)) {
            Ok(board) => println!("{path}: ok ({} devices)", board.devices.len()),
            Err(error) => {
                eprintln!("{path}: {error}");
                failed = true;
            }
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// `fan restore`: hands fans back to the kernel's automatic control by
/// writing sysfs directly, so it works when `lemnosd` is gone (systemd runs
/// it as `ExecStopPost`).
fn fan(mut args: Args) -> ExitCode {
    if args.next().as_deref() != Some("restore") {
        return fail("usage: fan restore [--board PATH] [--all]");
    }
    let board = args
        .take("--board")
        .unwrap_or_else(|| lemnosd::DEFAULT_BOARD.into());
    let all = args.flag("--all");
    let sys = lemnos_drivers_linux::SysRoot::default();
    let mut restored = 0;
    if let Ok(definition) = BoardDefinition::from_path(&board) {
        for spec in definition
            .devices
            .iter()
            .filter(|d| d.driver == "hwmon-fan")
        {
            let mode = spec
                .config
                .get("restore_mode")
                .and_then(lemnos_board::ConfigValue::as_i64)
                .unwrap_or(2) as i32;
            let fan = match &spec.path {
                Some(path) => Some(lemnos_drivers_linux::HwmonFan::new(path)),
                None => lemnos_drivers_linux::HwmonFan::find(
                    &sys.hwmon(),
                    spec.matches.get("name").map(String::as_str),
                )
                .ok()
                .flatten(),
            };
            if let Some(fan) = fan
                && fan.set_mode(mode).is_ok()
            {
                restored += 1;
            }
        }
    }
    if all {
        for entry in lemnos_drivers_linux::sysfs::entries(&sys.hwmon()).unwrap_or_default() {
            if entry.join("pwm1_enable").exists()
                && lemnos_drivers_linux::HwmonFan::new(&entry)
                    .restore_automatic()
                    .is_ok()
            {
                restored += 1;
            }
        }
    }
    println!("restored {restored} fan(s)");
    ExitCode::SUCCESS
}

fn led(mut args: Args, options: ClientOptions) -> ExitCode {
    let device = args.take("--device").unwrap_or_default();
    let effect = args.take("--effect");
    let blink = args.flag("--blink");
    let period = args.take("--period").and_then(|p| p.parse().ok());
    let depth = args.take("--depth").as_deref().and_then(permille);
    let fade = args.take("--fade").and_then(|f| f.parse().ok());
    let easing = args.take("--easing");
    let brightness = args.take("--brightness").as_deref().and_then(permille);
    let seconds = args.take("--seconds").and_then(|s| s.parse::<f64>().ok());
    let color_opt = args.take("--color");
    let background = args.take("--background");
    let phase = args.take("--phase");
    let Some(what) = args.next() else {
        return fail(
            "led: status, color, brightness, pixel, frame, progress, spinner, system, locate or off",
        );
    };
    let show = match what.as_str() {
        "status" => match args.next().as_deref().and_then(LedStatus::parse) {
            Some(status) => LedShow::Status(status),
            None => return fail("led status <ok|warn|error|busy|off>"),
        },
        "color" => match args.next().as_deref().and_then(color) {
            Some(rgb) => LedShow::Color(rgb),
            None => return fail("led color <RRGGBB>"),
        },
        "brightness" => {
            let Some(level) = args.next().as_deref().and_then(permille) else {
                return fail("led brightness <0..1>");
            };
            let mut devices = match options.devices() {
                Ok(c) => c,
                Err(e) => return fail(e),
            };
            let light = if device.is_empty() {
                devices
                    .devices()
                    .find(|d| d.pixels > 0)
                    .map(|d| d.id.clone())
                    .unwrap_or_default()
            } else {
                device
            };
            return match devices.set(&light, "brightness", f64::from(level) / 1000.0) {
                Ok(applied) => {
                    println!("{light} brightness {applied}");
                    ExitCode::SUCCESS
                }
                Err(e) => fail(e),
            };
        }
        "pixel" => {
            let mut pixels = Vec::new();
            while let (Some(index), Some(rgb)) = (args.next(), args.next()) {
                match (index.parse::<u16>(), color(&rgb)) {
                    (Ok(index), Some(rgb)) => pixels.push((index, rgb)),
                    _ => return fail("led pixel <index> <RRGGBB>..."),
                }
            }
            LedShow::Pixels(pixels)
        }
        "frame" => {
            let list = args.next().unwrap_or_default();
            let frame: Option<Vec<u32>> = list.split(',').map(color).collect();
            match frame {
                Some(frame) => LedShow::Frame(frame),
                None => return fail("led frame <RRGGBB,RRGGBB,...>"),
            }
        }
        "progress" => match args.next().as_deref().and_then(permille) {
            Some(fraction) => LedShow::Progress {
                fraction,
                color: color_opt.as_deref().and_then(color),
                background: background.as_deref().and_then(color),
            },
            None => return fail("led progress <0..1>"),
        },
        "spinner" => LedShow::Indeterminate {
            color: color_opt.as_deref().and_then(color),
        },
        "system" => {
            let state = match args.next().as_deref() {
                Some("updating") => SystemState::Updating {
                    progress: args.next().as_deref().and_then(permille),
                    phase: phase
                        .as_deref()
                        .and_then(Phase::parse)
                        .unwrap_or(Phase::Writing),
                },
                Some("booting") => SystemState::Booting,
                Some("rebooting") => SystemState::Rebooting,
                Some("update-failed") => SystemState::UpdateFailed,
                Some("rolled-back") => SystemState::RolledBack,
                _ => {
                    return fail(
                        "led system <updating [0..1]|booting|rebooting|update-failed|rolled-back>",
                    );
                }
            };
            LedShow::System(state)
        }
        "locate" => LedShow::Locate,
        "off" | "clear" => LedShow::Clear,
        other => return fail(format!("led: unknown {other}")),
    };
    let mut request = LedRequest::new(show);
    request.device = device;
    request.effect = match (effect.as_deref(), blink) {
        (Some(name), _) => match EffectKind::parse(name) {
            Some(effect) => Some(effect),
            None => return fail(format!("unknown effect {name}")),
        },
        (None, true) => Some(EffectKind::Blink),
        (None, false) => None,
    };
    request.period_ms = period;
    request.depth = depth;
    request.fade_ms = fade;
    request.brightness = brightness;
    if let Some(name) = easing {
        match Easing::parse(&name) {
            Some(e) => request.easing = Some(e),
            None => return fail(format!("unknown easing {name}")),
        }
    }
    request.duration_ms = match (seconds, &request.show) {
        (Some(s), _) => Some((s * 1000.0) as u32),
        (None, LedShow::Locate) => Some(10_000),
        _ => None,
    };
    let mut leds = match options.keep_intents().leds() {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    match leds.send(request).and_then(|()| leds.sync()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(e),
    }
}

fn devices(command: &str, mut args: Args, options: ClientOptions) -> ExitCode {
    let mut client = match options.devices() {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    match command {
        "list" => match client.list() {
            Ok(devices) => {
                for d in devices {
                    let channels: Vec<String> = d
                        .channels
                        .iter()
                        .map(|c| format!("{} ({})", c.name, c.quantity.unit().symbol()))
                        .collect();
                    let controls: Vec<&str> = d.controls.iter().map(|c| c.name.as_str()).collect();
                    println!(
                        "{:<16} {:<14} {:<12} {:<10} channels: [{}] controls: [{}]",
                        d.id,
                        d.class.name(),
                        d.model,
                        d.status.name(),
                        channels.join(", "),
                        controls.join(", ")
                    );
                }
                ExitCode::SUCCESS
            }
            Err(e) => fail(e),
        },
        "read" => {
            let Some(device) = args.next() else {
                return fail("read <device>");
            };
            match client.read(&device) {
                Ok(reading) => {
                    print_reading(&reading);
                    ExitCode::SUCCESS
                }
                Err(e) => fail(e),
            }
        }
        "watch" => {
            let Some(device) = args.next() else {
                return fail("watch <device> [--period MS] [--count N]");
            };
            let period = args
                .take("--period")
                .and_then(|p| p.parse().ok())
                .unwrap_or(100);
            let count: Option<u64> = args.take("--count").and_then(|c| c.parse().ok());
            if let Err(e) = client.subscribe(&device, period) {
                return fail(e);
            }
            let mut seen = 0;
            while count.is_none_or(|c| seen < c) {
                match client.next_event_timeout(Duration::from_secs(5)) {
                    Ok(Some(ClientEvent::Data(Update::Reading(reading)))) => {
                        print_reading(&reading);
                        seen += 1;
                    }
                    Ok(Some(ClientEvent::Data(Update::Event(event)))) => println!("{event:?}"),
                    Ok(Some(_)) => {}
                    Ok(None) => return fail("no readings for 5 s"),
                    Err(e) => return fail(e),
                }
            }
            ExitCode::SUCCESS
        }
        "set" => {
            let (Some(device), Some(control), Some(value)) =
                (args.next(), args.next(), args.next())
            else {
                return fail("set <device> <control> <value>");
            };
            let Ok(value) = value.parse::<f64>() else {
                return fail("the value must be a number");
            };
            match client.set(&device, &control, value) {
                Ok(applied) => {
                    println!("{device} {control} = {applied}");
                    ExitCode::SUCCESS
                }
                Err(e) => fail(e),
            }
        }
        "get" => {
            let (Some(device), Some(control)) = (args.next(), args.next()) else {
                return fail("get <device> <control>");
            };
            match client.get(&device, &control) {
                Ok(value) => {
                    println!("{device} {control} = {value}");
                    ExitCode::SUCCESS
                }
                Err(e) => fail(e),
            }
        }
        other => fail(format!("unknown command {other}")),
    }
}

fn print_reading(reading: &lemnos_ipc::Reading) {
    let values: Vec<String> = reading
        .channels()
        .iter()
        .zip(reading.values())
        .map(|(c, (name, v))| match v {
            Some(v) => format!("{name}={v:.6}{}", c.quantity.unit().symbol()),
            None => format!("{name}=-"),
        })
        .collect();
    println!(
        "{} {:>10}us {} {}",
        reading.device,
        reading.timestamp_us,
        reading.status.name(),
        values.join(" ")
    );
}
