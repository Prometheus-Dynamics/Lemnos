//! `lemnos-ctl calibration`: a device's calibration, through `lemnosd`
//! (`docs/imu-calibration-fusion.md`). `show` also summarises the calibration
//! file the service keeps for the device (the words themselves are not sent
//! over the socket).

use super::{Args, fail};
use lemnos_ipc::{
    CalibrationCommand, CalibrationPart, CalibrationRoutine, CalibrationStatus, ClientOptions,
};
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "calibration: show|status|reset|stop|apply|discard <device>, or start <device> <accel-six|mag-rotate|gyro-hold>";

/// `lemnos-ctl calibration <op> <device> [routine]`.
pub(super) fn calibration(mut args: Args, options: ClientOptions) -> ExitCode {
    let (Some(op), Some(device)) = (args.next(), args.next()) else {
        return fail(USAGE);
    };
    let command = match op.as_str() {
        "show" | "status" => None,
        "reset" => Some(CalibrationCommand::Reset),
        "stop" => Some(CalibrationCommand::Stop),
        "apply" => Some(CalibrationCommand::Apply),
        "discard" => Some(CalibrationCommand::Discard),
        "start" => {
            let Some(name) = args.next() else {
                return fail("calibration start <device> <accel-six|mag-rotate|gyro-hold>");
            };
            match CalibrationRoutine::from_name(&name) {
                Some(routine) => Some(CalibrationCommand::Start(routine)),
                None => {
                    return fail(format!(
                        "unknown routine {name:?} (accel-six, mag-rotate or gyro-hold)"
                    ));
                }
            }
        }
        other => return fail(format!("{USAGE} (not {other:?})")),
    };
    let mut client = match options.devices() {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    if let Some(command) = command {
        return match client.calibration(&device, command) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(e),
        };
    }
    match client.calibration_status(&device) {
        Ok(status) => {
            print_status(&device, &status);
            if op == "show" {
                print_file(&device);
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail(e),
    }
}

fn print_status(device: &str, status: &CalibrationStatus) {
    let running = match status.running {
        Some(routine) => format!(
            "running {} {:.1}%",
            routine.name(),
            f64::from(status.progress) / 10.0
        ),
        None => "idle".to_string(),
    };
    let candidate = if status.candidate {
        ", candidate ready (apply or discard)"
    } else {
        ""
    };
    let failed = if status.failed {
        ", the last routine failed"
    } else {
        ""
    };
    println!(
        "{device}: revision {} {running}{candidate}{failed}",
        status.revision
    );
    for (name, part) in ["accel", "gyro", "mag"].iter().zip(&status.parts) {
        if *part == CalibrationPart::default() {
            continue;
        }
        println!(
            "  {name:<6} confidence {:>4}/1000  coverage {:>4}/1000  residual {:>4}/1000  samples {:>7}  {}",
            part.confidence,
            part.coverage,
            part.residual,
            part.samples,
            if part.active { "active" } else { "not applied" }
        );
    }
}

/// The calibration file `lemnosd` keeps for `device`, summarised.
fn print_file(device: &str) {
    let dir = std::env::var_os("LEMNOSD_CALIBRATION_DIR").map_or_else(
        || PathBuf::from("/var/lib/lemnos/calibration"),
        PathBuf::from,
    );
    let file = dir.join(format!("{device}.toml"));
    let summary = match std::fs::read_to_string(&file) {
        Ok(text) => match text.parse::<toml::Table>() {
            Ok(table) => {
                let driver = table
                    .get("driver")
                    .and_then(toml::Value::as_str)
                    .unwrap_or("?");
                let revision = table
                    .get("revision")
                    .and_then(toml::Value::as_integer)
                    .unwrap_or(0);
                let words = table
                    .get("words")
                    .and_then(toml::Value::as_array)
                    .map_or(0, Vec::len);
                format!(
                    "saved: driver {driver}, revision {revision}, {words} words ({})",
                    file.display()
                )
            }
            Err(e) => format!("saved: {} is not readable: {e}", file.display()),
        },
        Err(_) => "saved: none (factory calibration)".to_string(),
    };
    println!("  {summary}");
}
