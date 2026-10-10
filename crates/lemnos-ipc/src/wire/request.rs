//! Client requests on the wire, and the LED intent encoding.

use super::codec::{Decoder, Encoder};
use super::*;

pub(super) fn easing_code(easing: Option<lemnos_light::Easing>) -> (u8, [u16; 4]) {
    use lemnos_light::Easing as E;
    match easing {
        None => (0, [0; 4]),
        Some(E::Linear) => (1, [0; 4]),
        Some(E::EaseIn) => (2, [0; 4]),
        Some(E::EaseOut) => (3, [0; 4]),
        Some(E::EaseInOut) => (4, [0; 4]),
        Some(E::Sine) => (5, [0; 4]),
        Some(E::CubicBezier { x1, y1, x2, y2 }) => (6, [x1, y1 as u16, x2, y2 as u16]),
    }
}

pub(super) fn easing_from(code: u8, p: [u16; 4]) -> Option<lemnos_light::Easing> {
    use lemnos_light::Easing as E;
    match code {
        1 => Some(E::Linear),
        2 => Some(E::EaseIn),
        3 => Some(E::EaseOut),
        4 => Some(E::EaseInOut),
        5 => Some(E::Sine),
        6 => Some(E::CubicBezier {
            x1: p[0],
            y1: p[1] as i16,
            x2: p[2],
            y2: p[3] as i16,
        }),
        _ => None,
    }
}

pub(super) fn status_led_code(status: lemnos_light::Status) -> u8 {
    use lemnos_light::Status as S;
    match status {
        S::Ok => 0,
        S::Warn => 1,
        S::Error => 2,
        S::Busy => 3,
        S::Off => 4,
    }
}

pub(super) fn status_led_from(code: u8) -> lemnos_light::Status {
    use lemnos_light::Status as S;
    match code {
        0 => S::Ok,
        1 => S::Warn,
        2 => S::Error,
        3 => S::Busy,
        _ => S::Off,
    }
}

pub(super) fn effect_code(effect: Option<lemnos_light::EffectKind>) -> u8 {
    use lemnos_light::EffectKind as K;
    match effect {
        None => 0,
        Some(K::Solid) => 1,
        Some(K::Blink) => 2,
        Some(K::Breathe) => 3,
        Some(K::Chase) => 4,
    }
}

pub(super) fn effect_from(code: u8) -> Option<lemnos_light::EffectKind> {
    use lemnos_light::EffectKind as K;
    match code {
        1 => Some(K::Solid),
        2 => Some(K::Blink),
        3 => Some(K::Breathe),
        4 => Some(K::Chase),
        _ => None,
    }
}

pub(super) fn system_encode(e: &mut Encoder, state: lemnos_light::SystemState) {
    use lemnos_light::{Phase, SystemState as S};
    let (code, progress, phase) = match state {
        S::Updating { progress, phase } => (
            0,
            progress,
            match phase {
                Phase::Verifying => 0,
                Phase::Writing => 1,
                Phase::Staged => 2,
                Phase::Applying => 3,
            },
        ),
        S::Booting => (1, None, 0),
        S::Rebooting => (2, None, 0),
        S::UpdateFailed => (3, None, 0),
        S::RolledBack => (4, None, 0),
        S::Confirmed => (5, None, 0),
    };
    e.u8(code).opt_u16(progress).u8(phase);
}

pub(super) fn system_decode(d: &mut Decoder<'_>) -> Result<lemnos_light::SystemState, WireError> {
    use lemnos_light::{Phase, SystemState as S};
    let code = d.u8()?;
    let progress = d.opt_u16()?;
    let phase = match d.u8()? {
        0 => Phase::Verifying,
        1 => Phase::Writing,
        2 => Phase::Staged,
        _ => Phase::Applying,
    };
    Ok(match code {
        0 => S::Updating { progress, phase },
        1 => S::Booting,
        2 => S::Rebooting,
        3 => S::UpdateFailed,
        4 => S::RolledBack,
        5 => S::Confirmed,
        other => return Err(bad(format!("system state {other}"))),
    })
}

impl Request {
    /// The frame for this request.
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Hello {
                version,
                client,
                priority,
                keep,
                events,
            } => {
                let mut e = Encoder::new(HELLO);
                e.u16(*version)
                    .str(client)
                    .u8(*priority)
                    .u8(u8::from(*keep))
                    .u8(u8::from(*events));
                e.finish()
            }
            Self::Restore {
                id,
                device,
                control,
            } => {
                let mut e = Encoder::new(RESTORE);
                e.u32(*id).str(device).str(control);
                e.finish()
            }
            Self::Raw(raw) => raw.encode(),
            Self::List => Encoder::new(LIST).finish(),
            Self::Read { device } => {
                let mut e = Encoder::new(READ);
                e.str(device);
                e.finish()
            }
            Self::Subscribe {
                id,
                device,
                period_ms,
            } => {
                let mut e = Encoder::new(SUBSCRIBE);
                e.str(device).u32(*period_ms);
                // Appended in protocol 1 (older services stop before it).
                e.u32(*id);
                e.finish()
            }
            Self::SubscribeChannels {
                id,
                device,
                channels,
                period_ms,
            } => {
                let mut e = Encoder::new(SUBSCRIBE_CHANNELS);
                e.str(device).u32(*period_ms).u16(channels.len() as u16);
                for channel in channels {
                    e.str(channel);
                }
                e.u32(*id);
                e.finish()
            }
            Self::Set {
                id,
                device,
                control,
                value,
            } => {
                let mut e = Encoder::new(SET);
                e.u32(*id).str(device).str(control).f64(*value);
                e.finish()
            }
            Self::Get {
                id,
                device,
                control,
            } => {
                let mut e = Encoder::new(GET);
                e.u32(*id).str(device).str(control);
                e.finish()
            }
            Self::Led(led) => {
                let mut e = Encoder::new(LED);
                e.str(&led.device);
                match &led.show {
                    LedShow::Clear => e.u8(0),
                    LedShow::Status(s) => e.u8(1).u8(status_led_code(*s)),
                    LedShow::Color(c) => e.u8(2).u32(*c),
                    LedShow::Frame(frame) => {
                        e.u8(3).u16(frame.len().min(u16::MAX as usize) as u16);
                        for c in frame.iter().take(u16::MAX as usize) {
                            e.u32(*c);
                        }
                        &mut e
                    }
                    LedShow::Locate => e.u8(4),
                    LedShow::Pixels(pixels) => {
                        e.u8(5).u16(pixels.len().min(u16::MAX as usize) as u16);
                        for (index, color) in pixels.iter().take(u16::MAX as usize) {
                            e.u16(*index).u32(*color);
                        }
                        &mut e
                    }
                    LedShow::Progress {
                        fraction,
                        color,
                        background,
                    } => e
                        .u8(6)
                        .u16(*fraction)
                        .opt_color(*color)
                        .opt_color(*background),
                    LedShow::Indeterminate { color } => e.u8(7).opt_color(*color),
                    LedShow::System(state) => {
                        e.u8(8);
                        system_encode(&mut e, *state);
                        &mut e
                    }
                    LedShow::Orbit {
                        color,
                        tail,
                        heads,
                        base,
                    } => e
                        .u8(9)
                        .opt_color(*color)
                        .opt_u16(*tail)
                        .u8(*heads)
                        .opt_u16(*base),
                    LedShow::Look { name, progress } => e.u8(10).str(name).opt_u16(*progress),
                    LedShow::Inline { spec, progress } => {
                        e.u8(11);
                        super::look::encode(&mut e, spec.as_ref());
                        e.opt_u16(*progress)
                    }
                };
                let (easing, params) = easing_code(led.easing);
                e.u8(effect_code(led.effect))
                    .opt_u32(led.period_ms)
                    .opt_u16(led.depth)
                    .opt_u16(led.brightness)
                    .opt_u32(led.fade_ms)
                    .u8(easing);
                for p in params {
                    e.u16(p);
                }
                e.opt_u32(led.duration_ms);
                // Flags, appended in protocol 1 (older peers stop before them).
                // The reply id is appended after them.
                e.u8(u8::from(led.test));
                e.u32(led.id);
                e.finish()
            }
            Self::Release { id, device } => {
                let mut e = Encoder::new(RELEASE);
                e.u32(*id).str(device);
                e.finish()
            }
            Self::WatchFrames { id, device, fps } => {
                let mut e = Encoder::new(WATCH_FRAMES);
                e.u32(*id).str(device).u16(*fps);
                e.finish()
            }
            Self::LightSetting {
                id,
                device,
                look_brightness,
                persist,
            } => {
                let mut e = Encoder::new(LIGHT_SETTING);
                e.u32(*id)
                    .str(device)
                    .u16(*look_brightness)
                    .u8(u8::from(*persist));
                e.finish()
            }
            Self::Looks { id, op } => {
                let mut e = Encoder::new(LOOKS);
                e.u32(*id);
                match op {
                    LooksOp::List => e.u8(0),
                    LooksOp::Show(name) => e.u8(1).str(name),
                    LooksOp::Reload => e.u8(2),
                    LooksOp::Save { name, text } => e.u8(3).str(name).str(text),
                    LooksOp::PresetList => e.u8(4),
                    LooksOp::PresetShow(name) => e.u8(5).str(name),
                    LooksOp::PresetApply(name) => e.u8(6).str(name),
                    LooksOp::PresetSave { name, text } => e.u8(7).str(name).str(text),
                    LooksOp::PresetDelete(name) => e.u8(8).str(name),
                    LooksOp::Delete(name) => e.u8(9).str(name),
                };
                e.finish()
            }
            Self::Calibration {
                id,
                device,
                command,
            } => {
                let mut e = Encoder::new(CALIBRATION);
                e.u32(*id).str(device);
                super::calibration::command_encode(&mut e, *command);
                e.finish()
            }
            Self::CalibrationStatus { id, device } => {
                let mut e = Encoder::new(CALIBRATION_STATUS);
                e.u32(*id).str(device);
                e.finish()
            }
        }
    }

    pub(crate) fn decode(kind: u16, payload: &[u8]) -> Result<Self, WireError> {
        let mut d = Decoder { buf: payload };
        Ok(match kind {
            HELLO => Self::Hello {
                version: d.u16()?,
                client: d.str()?,
                priority: d.u8()?,
                keep: d.u8()? != 0,
                // Appended later in protocol 1: older clients read events.
                events: d.is_empty() || d.u8()? != 0,
            },
            RESTORE => Self::Restore {
                id: d.u32()?,
                device: d.str()?,
                control: d.str()?,
            },
            kind if super::raw::is_raw(kind) => Self::Raw(RawRequest::decode(kind, &mut d)?),
            LIST => Self::List,
            READ => Self::Read { device: d.str()? },
            SUBSCRIBE_CHANNELS => {
                let device = d.str()?;
                let period_ms = d.u32()?;
                let count = usize::from(d.u16()?);
                let mut channels = Vec::with_capacity(count.min(64));
                for _ in 0..count {
                    channels.push(d.str()?);
                }
                Self::SubscribeChannels {
                    id: d.u32()?,
                    device,
                    channels,
                    period_ms,
                }
            }
            SUBSCRIBE => Self::Subscribe {
                device: d.str()?,
                period_ms: d.u32()?,
                // Older clients send no id: they get no reply to a success.
                id: if d.is_empty() { 0 } else { d.u32()? },
            },
            SET => Self::Set {
                id: d.u32()?,
                device: d.str()?,
                control: d.str()?,
                value: d.f64()?,
            },
            GET => Self::Get {
                id: d.u32()?,
                device: d.str()?,
                control: d.str()?,
            },
            LED => {
                let device = d.str()?;
                let show = match d.u8()? {
                    0 => LedShow::Clear,
                    1 => LedShow::Status(status_led_from(d.u8()?)),
                    2 => LedShow::Color(d.u32()?),
                    3 => {
                        let n = d.u16()?;
                        let mut frame = Vec::with_capacity(usize::from(n));
                        for _ in 0..n {
                            frame.push(d.u32()?);
                        }
                        LedShow::Frame(frame)
                    }
                    4 => LedShow::Locate,
                    5 => {
                        let n = d.u16()?;
                        let mut pixels = Vec::with_capacity(usize::from(n));
                        for _ in 0..n {
                            pixels.push((d.u16()?, d.u32()?));
                        }
                        LedShow::Pixels(pixels)
                    }
                    6 => LedShow::Progress {
                        fraction: d.u16()?,
                        color: d.opt_color()?,
                        background: d.opt_color()?,
                    },
                    7 => LedShow::Indeterminate {
                        color: d.opt_color()?,
                    },
                    8 => LedShow::System(system_decode(&mut d)?),
                    9 => LedShow::Orbit {
                        color: d.opt_color()?,
                        tail: d.opt_u16()?,
                        heads: d.u8()?,
                        base: d.opt_u16()?,
                    },
                    10 => LedShow::Look {
                        name: d.str()?,
                        progress: d.opt_u16()?,
                    },
                    11 => LedShow::Inline {
                        spec: Box::new(super::look::decode(&mut d)?),
                        progress: d.opt_u16()?,
                    },
                    other => return Err(bad(format!("LED show {other}"))),
                };
                let effect = effect_from(d.u8()?);
                let period_ms = d.opt_u32()?;
                let depth = d.opt_u16()?;
                let brightness = d.opt_u16()?;
                let fade_ms = d.opt_u32()?;
                let easing_kind = d.u8()?;
                let params = [d.u16()?, d.u16()?, d.u16()?, d.u16()?];
                let duration_ms = d.opt_u32()?;
                // Older clients stop before the flags and the reply id.
                let test = !d.is_empty() && d.u8()? & 1 != 0;
                let id = if d.is_empty() { 0 } else { d.u32()? };
                Self::Led(LedRequest {
                    device,
                    show,
                    effect,
                    period_ms,
                    depth,
                    brightness,
                    fade_ms,
                    easing: easing_from(easing_kind, params),
                    duration_ms,
                    test,
                    id,
                })
            }
            RELEASE => Self::Release {
                id: d.u32()?,
                device: d.str()?,
            },
            WATCH_FRAMES => Self::WatchFrames {
                id: d.u32()?,
                device: d.str()?,
                fps: d.u16()?,
            },
            LIGHT_SETTING => Self::LightSetting {
                id: d.u32()?,
                device: d.str()?,
                look_brightness: d.u16()?,
                persist: d.u8()? != 0,
            },
            LOOKS => Self::Looks {
                id: d.u32()?,
                op: match d.u8()? {
                    0 => LooksOp::List,
                    1 => LooksOp::Show(d.str()?),
                    2 => LooksOp::Reload,
                    3 => LooksOp::Save {
                        name: d.str()?,
                        text: d.str()?,
                    },
                    4 => LooksOp::PresetList,
                    5 => LooksOp::PresetShow(d.str()?),
                    6 => LooksOp::PresetApply(d.str()?),
                    7 => LooksOp::PresetSave {
                        name: d.str()?,
                        text: d.str()?,
                    },
                    8 => LooksOp::PresetDelete(d.str()?),
                    9 => LooksOp::Delete(d.str()?),
                    other => return Err(bad(format!("looks operation {other}"))),
                },
            },
            CALIBRATION => Self::Calibration {
                id: d.u32()?,
                device: d.str()?,
                command: super::calibration::command_decode(&mut d)?,
            },
            CALIBRATION_STATUS => Self::CalibrationStatus {
                id: d.u32()?,
                device: d.str()?,
            },
            other => return Err(bad(format!("unknown request kind {other}"))),
        })
    }
}
