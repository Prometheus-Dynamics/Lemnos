//! GPIO through the character device, uAPI v2.

use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::AsFd;
use std::path::Path;
use std::time::Duration;

use lemnos_hal::ErrorKind;
use lemnos_hal::raw::{Bias, Direction, Drive, Edge, EdgeDetect, LineConfig, RawLine};
use lemnos_linux_sys::gpio::{self as sys, attr, flag};
use lemnos_linux_sys::poll;

use crate::kind_of;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// How to request or reconfigure one line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct LineSettings {
    direction: Option<Direction>,
    active_low: bool,
    bias: Bias,
    drive: Drive,
    edge: EdgeDetect,
    debounce_us: Option<u32>,
    output_value: bool,
}

impl LineSettings {
    /// Leaves the line as it is, so taking one over doesn't glitch it.
    const fn as_is() -> Self {
        Self {
            direction: None,
            active_low: false,
            bias: Bias::AsIs,
            drive: Drive::PushPull,
            edge: EdgeDetect::None,
            debounce_us: None,
            output_value: false,
        }
    }

    fn from_config(config: &LineConfig) -> Self {
        let mut settings = Self {
            direction: Some(config.direction),
            active_low: config.active_low,
            bias: config.bias,
            output_value: config.initial,
            ..Self::as_is()
        };
        if config.direction == Direction::Output {
            settings.drive = config.drive;
        } else {
            settings.edge = config.edge;
            settings.debounce_us = (config.debounce_us > 0).then_some(config.debounce_us);
        }
        settings
    }

    fn flags(&self) -> io::Result<u64> {
        let mut flags = match self.direction {
            Some(Direction::Input) => flag::INPUT,
            Some(Direction::Output) => flag::OUTPUT,
            None => 0,
        };
        if self.active_low {
            flags |= flag::ACTIVE_LOW;
        }
        flags |= match self.bias {
            Bias::AsIs => 0,
            Bias::PullUp => flag::BIAS_PULL_UP,
            Bias::PullDown => flag::BIAS_PULL_DOWN,
            Bias::Disabled => flag::BIAS_DISABLED,
        };
        match self.direction {
            None => {
                if self.bias != Bias::AsIs
                    || self.drive != Drive::PushPull
                    || self.edge != EdgeDetect::None
                    || self.debounce_us.is_some()
                {
                    return Err(invalid("bias, drive, edges and debounce need a direction"));
                }
            }
            Some(Direction::Output) => {
                if self.edge != EdgeDetect::None || self.debounce_us.is_some() {
                    return Err(invalid("edge events and debounce need an input line"));
                }
                flags |= match self.drive {
                    Drive::PushPull => 0,
                    Drive::OpenDrain => flag::OPEN_DRAIN,
                    Drive::OpenSource => flag::OPEN_SOURCE,
                };
            }
            Some(Direction::Input) => {
                if self.drive != Drive::PushPull {
                    return Err(invalid("open drain or source needs an output line"));
                }
                flags |= match self.edge {
                    EdgeDetect::None => 0,
                    EdgeDetect::Rising => flag::EDGE_RISING,
                    EdgeDetect::Falling => flag::EDGE_FALLING,
                    EdgeDetect::Both => flag::EDGE_RISING | flag::EDGE_FALLING,
                };
            }
        }
        Ok(flags)
    }
}

/// Adds an attribute, sharing a slot with an identical one.
fn add(config: &mut sys::LineConfig, id: u32, value: u64, index: usize) -> io::Result<()> {
    let used = config.num_attrs as usize;
    if let Some(found) = config.attrs[..used]
        .iter_mut()
        .find(|a| a.attr.id == id && a.attr.value == value)
    {
        found.mask |= 1 << index;
        return Ok(());
    }
    if used == sys::NUM_ATTRS_MAX {
        return Err(invalid("too many distinct line configurations"));
    }
    config.attrs[used] = sys::LineConfigAttribute {
        attr: sys::LineAttribute {
            id,
            padding: 0,
            value,
        },
        mask: 1 << index,
    };
    config.num_attrs += 1;
    Ok(())
}

/// The first line's flags as the default, an attribute for each other distinct flag set and each
/// debounce period, then the outputs' starting values.
fn build_config(lines: &[(u32, LineSettings)]) -> io::Result<sys::LineConfig> {
    if lines.is_empty() || lines.len() > sys::MAX_LINES {
        return Err(invalid("a request takes at least one line and at most 64"));
    }
    let mut config = sys::LineConfig {
        flags: lines[0].1.flags()?,
        ..sys::LineConfig::default()
    };
    let mut values = 0u64;
    let mut outputs = 0u64;
    for (index, (_, line)) in lines.iter().enumerate() {
        let flags = line.flags()?;
        if flags != config.flags {
            add(&mut config, attr::FLAGS, flags, index)?;
        }
        if let Some(us) = line.debounce_us {
            add(&mut config, attr::DEBOUNCE, u64::from(us), index)?;
        }
        if line.direction == Some(Direction::Output) {
            outputs |= 1 << index;
            values |= u64::from(line.output_value) << index;
        }
    }
    if outputs != 0 {
        let used = config.num_attrs as usize;
        if used == sys::NUM_ATTRS_MAX {
            return Err(invalid("too many distinct line configurations"));
        }
        config.attrs[used] = sys::LineConfigAttribute {
            attr: sys::LineAttribute {
                id: attr::OUTPUT_VALUES,
                padding: 0,
                value: values,
            },
            mask: outputs,
        };
        config.num_attrs += 1;
    }
    Ok(config)
}

fn build_request(consumer: &str, lines: &[(u32, LineSettings)]) -> io::Result<sys::LineRequest> {
    let mut request = sys::LineRequest {
        config: build_config(lines)?,
        num_lines: lines.len() as u32,
        ..sys::LineRequest::default()
    };
    for (slot, (offset, _)) in request.offsets.iter_mut().zip(lines) {
        *slot = *offset;
    }
    let name = consumer.as_bytes();
    let n = name.len().min(sys::MAX_NAME_SIZE - 1);
    request.consumer[..n].copy_from_slice(&name[..n]);
    Ok(request)
}

/// One claimed line, released when dropped. The kernel refuses a line someone holds.
pub struct GpioLine {
    file: File,
    offset: u32,
}

impl GpioLine {
    /// Claims `offset` on the chip at `path`, leaving its direction and value alone.
    pub fn open(path: impl AsRef<Path>, offset: u32) -> io::Result<Self> {
        let chip = OpenOptions::new().read(true).write(true).open(path)?;
        if offset >= sys::chip_info(chip.as_fd())?.lines {
            return Err(invalid("no such line on this chip"));
        }
        let request = build_request("lemnos", &[(offset, LineSettings::as_is())])?;
        let fd = sys::request_lines(chip.as_fd(), &request)?;
        Ok(Self {
            file: File::from(fd),
            offset,
        })
    }

    fn read_pending(&mut self) -> io::Result<Option<Edge>> {
        let ready = poll::poll_one(self.file.as_fd(), poll::POLLIN, Some(Duration::ZERO))?;
        if ready & poll::POLLIN == 0 {
            return Ok(None);
        }
        let mut raw = [0u8; sys::LINE_EVENT_SIZE];
        self.file.read_exact(&mut raw)?;
        let event = sys::LineEvent::from_bytes(&raw).ok_or(io::ErrorKind::InvalidData)?;
        Ok(Some(Edge {
            rising: event.id == sys::EVENT_RISING_EDGE,
            timestamp_ns: event.timestamp_ns,
            seq: event.line_seqno,
        }))
    }
}

impl RawLine for GpioLine {
    fn configure(&mut self, config: &LineConfig) -> Result<(), ErrorKind> {
        let lines = [(self.offset, LineSettings::from_config(config))];
        build_config(&lines)
            .and_then(|config| sys::set_config(self.file.as_fd(), &config))
            .map_err(|e| kind_of(&e))
    }

    fn get(&mut self) -> Result<bool, ErrorKind> {
        sys::get_values(self.file.as_fd(), 1)
            .map(|values| values & 1 != 0)
            .map_err(|e| kind_of(&e))
    }

    fn set(&mut self, value: bool) -> Result<(), ErrorKind> {
        let values = sys::LineValues {
            bits: u64::from(value),
            mask: 1,
        };
        sys::set_values(self.file.as_fd(), values).map_err(|e| kind_of(&e))
    }

    fn read_edge(&mut self) -> Result<Option<Edge>, ErrorKind> {
        self.read_pending().map_err(|e| kind_of(&e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> LineSettings {
        LineSettings {
            direction: Some(Direction::Input),
            ..LineSettings::as_is()
        }
    }

    fn output(value: bool) -> LineSettings {
        LineSettings {
            direction: Some(Direction::Output),
            output_value: value,
            ..LineSettings::as_is()
        }
    }

    fn consumer(raw: &[u8]) -> &str {
        let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
        core::str::from_utf8(&raw[..end]).unwrap()
    }

    #[test]
    fn builds_output_requests() {
        let lines = [
            (5, output(true)),
            (
                7,
                LineSettings {
                    active_low: true,
                    ..output(false)
                },
            ),
            (
                9,
                LineSettings {
                    active_low: true,
                    ..output(true)
                },
            ),
        ];
        let request = build_request("camera-power", &lines).unwrap();
        assert_eq!(&request.offsets[..3], &[5, 7, 9]);
        assert_eq!(request.num_lines, 3);
        assert_eq!(consumer(&request.consumer), "camera-power");
        assert_eq!(request.config.flags, flag::OUTPUT);
        assert_eq!(request.config.num_attrs, 2);
        let flags = request.config.attrs[0];
        assert_eq!(flags.attr.id, attr::FLAGS);
        assert_eq!(flags.attr.value, flag::OUTPUT | flag::ACTIVE_LOW);
        assert_eq!(flags.mask, 0b110);
        let values = request.config.attrs[1];
        assert_eq!(values.attr.id, attr::OUTPUT_VALUES);
        assert_eq!((values.attr.value, values.mask), (0b101, 0b111));
    }

    #[test]
    fn builds_input_requests_with_bias_edges_and_debounce() {
        let button = LineSettings {
            bias: Bias::PullUp,
            edge: EdgeDetect::Falling,
            debounce_us: Some(5000),
            ..input()
        };
        let request = build_request("lemnos", &[(3, button), (4, output(true))]).unwrap();
        assert_eq!(
            request.config.flags,
            flag::INPUT | flag::BIAS_PULL_UP | flag::EDGE_FALLING
        );
        let attrs = &request.config.attrs[..request.config.num_attrs as usize];
        let has = |id, value, mask| {
            attrs
                .iter()
                .any(|a| a.attr.id == id && a.attr.value == value && a.mask == mask)
        };
        assert!(has(attr::DEBOUNCE, 5000, 0b01));
        assert!(has(attr::FLAGS, flag::OUTPUT, 0b10));
        assert!(has(attr::OUTPUT_VALUES, 0b10, 0b10));
    }

    #[test]
    fn rejects_bad_requests() {
        let request = build_request(&"x".repeat(40), &[(0, input())]).unwrap();
        assert_eq!(consumer(&request.consumer).len(), 31);
        assert!(build_request("c", &[]).is_err());
        let bad = LineSettings {
            edge: EdgeDetect::Rising,
            ..output(false)
        };
        assert!(build_request("c", &[(0, bad)]).is_err());
        let bad = LineSettings {
            drive: Drive::OpenDrain,
            ..input()
        };
        assert!(bad.flags().is_err());
        let many: Vec<(u32, LineSettings)> = (0..12)
            .map(|i| {
                let settings = LineSettings {
                    debounce_us: Some(100 + i),
                    ..input()
                };
                (i, settings)
            })
            .collect();
        assert!(build_request("c", &many).is_err());
    }

    #[test]
    fn taking_over_a_line_leaves_it_alone() {
        let request = build_request("c", &[(1, LineSettings::as_is())]).unwrap();
        assert_eq!(request.config.flags, 0);
        assert_eq!(request.config.num_attrs, 0);
    }

    #[test]
    fn an_output_ignores_input_only_settings() {
        let settings =
            LineSettings::from_config(&LineConfig::output(true).with_edge(EdgeDetect::Both));
        assert_eq!(settings.edge, EdgeDetect::None);
        assert!(settings.flags().is_ok());
    }

    #[test]
    fn opening_a_missing_chip_fails_cleanly() {
        let error = GpioLine::open("/dev/gpiochip-does-not-exist", 0)
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }
}
