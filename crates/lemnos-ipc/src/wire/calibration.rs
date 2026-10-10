//! Calibration on the wire: the command a client sends and the status a
//! service answers with. Routines are a stable code (1 accel-six, 2
//! mag-rotate, 3 gyro-hold); a command is an operation code and, for `Start`,
//! its routine.

use super::codec::{Decoder, Encoder};
use super::{WireError, bad};
use lemnos_device::{CalibrationCommand, CalibrationPart, CalibrationRoutine, CalibrationStatus};

fn routine_code(routine: CalibrationRoutine) -> u8 {
    match routine {
        CalibrationRoutine::AccelSix => 1,
        CalibrationRoutine::MagRotate => 2,
        CalibrationRoutine::GyroHold => 3,
    }
}

fn routine_from(code: u8) -> Result<CalibrationRoutine, WireError> {
    Ok(match code {
        1 => CalibrationRoutine::AccelSix,
        2 => CalibrationRoutine::MagRotate,
        3 => CalibrationRoutine::GyroHold,
        other => return Err(bad(format!("calibration routine {other}"))),
    })
}

pub(super) fn command_encode(e: &mut Encoder, command: CalibrationCommand) {
    match command {
        CalibrationCommand::Start(routine) => e.u8(0).u8(routine_code(routine)),
        CalibrationCommand::Stop => e.u8(1),
        CalibrationCommand::Apply => e.u8(2),
        CalibrationCommand::Discard => e.u8(3),
        CalibrationCommand::Reset => e.u8(4),
    };
}

pub(super) fn command_decode(d: &mut Decoder<'_>) -> Result<CalibrationCommand, WireError> {
    Ok(match d.u8()? {
        0 => CalibrationCommand::Start(routine_from(d.u8()?)?),
        1 => CalibrationCommand::Stop,
        2 => CalibrationCommand::Apply,
        3 => CalibrationCommand::Discard,
        4 => CalibrationCommand::Reset,
        other => return Err(bad(format!("calibration command {other}"))),
    })
}

pub(super) fn status_encode(e: &mut Encoder, status: &CalibrationStatus) {
    e.u32(status.revision)
        .u8(status.running.map_or(0, routine_code))
        .u16(status.progress)
        .u8(u8::from(status.candidate))
        .u8(u8::from(status.failed));
    for part in &status.parts {
        e.u32(part.samples)
            .u16(part.confidence)
            .u16(part.coverage)
            .u16(part.residual)
            .u8(u8::from(part.active));
    }
}

pub(super) fn status_decode(d: &mut Decoder<'_>) -> Result<CalibrationStatus, WireError> {
    let revision = d.u32()?;
    let running = match d.u8()? {
        0 => None,
        code => Some(routine_from(code)?),
    };
    let progress = d.u16()?;
    let candidate = d.u8()? != 0;
    let failed = d.u8()? != 0;
    let mut parts = [CalibrationPart::default(); 3];
    for part in &mut parts {
        *part = CalibrationPart {
            samples: d.u32()?,
            confidence: d.u16()?,
            coverage: d.u16()?,
            residual: d.u16()?,
            active: d.u8()? != 0,
        };
    }
    Ok(CalibrationStatus {
        revision,
        running,
        progress,
        candidate,
        failed,
        parts,
    })
}
