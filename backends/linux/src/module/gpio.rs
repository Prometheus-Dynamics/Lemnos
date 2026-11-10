//! A GPIO line through the module.

use std::fs::File;
use std::io::{self, Read};
use std::os::fd::AsFd;
use std::time::Duration;

use lemnos_hal::ErrorKind;
use lemnos_hal::raw::{Bias, Direction, Drive, Edge, EdgeDetect, LineConfig, RawLine};
use lemnos_linux_sys::module::{self as sys, GPIO_EVENT_SIZE, GpioEvent, gpio as flag};
use lemnos_linux_sys::poll;

use super::Node;
use crate::kind_of;

/// One line, held by a claim. Reading the claim gives the line's edges.
pub(crate) struct ModuleLine {
    node: Node,
    claim: File,
}

impl ModuleLine {
    /// Claims `offset` of gpiochip `chip`, leaving its direction and value alone.
    pub(crate) fn open(node: Node, chip: u32, offset: u32) -> io::Result<Self> {
        let claim = node.claim(format_args!("gpio-{chip}:{offset}"))?;
        sys::gpio_request(node.fd(), claim.as_fd(), chip, offset)?;
        Ok(Self {
            node,
            claim: File::from(claim),
        })
    }

    fn read_pending(&mut self) -> io::Result<Option<Edge>> {
        let ready = poll::poll_one(self.claim.as_fd(), poll::POLLIN, Some(Duration::ZERO))?;
        if ready & poll::POLLIN == 0 {
            return Ok(None);
        }
        let mut raw = [0u8; GPIO_EVENT_SIZE];
        self.claim.read_exact(&mut raw)?;
        let event = GpioEvent::from_bytes(&raw);
        Ok(Some(Edge {
            rising: event.rising != 0,
            timestamp_ns: event.ns,
            seq: event.seq,
        }))
    }
}

/// The module's flags for `config`. As on the stock path, an output has no edges and an input no
/// drive.
fn flags(config: &LineConfig) -> u32 {
    let mut flags = 0;
    if config.active_low {
        flags |= flag::ACTIVE_LOW;
    }
    flags |= match config.bias {
        Bias::AsIs => 0,
        Bias::PullUp => flag::PULL_UP,
        Bias::PullDown => flag::PULL_DOWN,
        Bias::Disabled => flag::BIAS_OFF,
    };
    match config.direction {
        Direction::Output => {
            flags |= flag::OUTPUT;
            flags |= match config.drive {
                Drive::PushPull => 0,
                Drive::OpenDrain => flag::OPEN_DRAIN,
                Drive::OpenSource => flag::OPEN_SOURCE,
            };
        }
        Direction::Input => {
            flags |= match config.edge {
                EdgeDetect::None => 0,
                EdgeDetect::Rising => flag::RISING,
                EdgeDetect::Falling => flag::FALLING,
                EdgeDetect::Both => flag::RISING | flag::FALLING,
            };
        }
    }
    flags
}

impl RawLine for ModuleLine {
    fn configure(&mut self, config: &LineConfig) -> Result<(), ErrorKind> {
        let (value, debounce_us) = match config.direction {
            Direction::Output => (config.initial, 0),
            Direction::Input => (false, config.debounce_us),
        };
        sys::gpio_config(
            self.node.fd(),
            self.claim.as_fd(),
            flags(config),
            value,
            debounce_us,
        )
        .map_err(|e| kind_of(&e))
    }

    fn get(&mut self) -> Result<bool, ErrorKind> {
        sys::gpio_get(self.node.fd(), self.claim.as_fd()).map_err(|e| kind_of(&e))
    }

    fn set(&mut self, value: bool) -> Result<(), ErrorKind> {
        sys::gpio_set(self.node.fd(), self.claim.as_fd(), value).map_err(|e| kind_of(&e))
    }

    fn read_edge(&mut self) -> Result<Option<Edge>, ErrorKind> {
        self.read_pending().map_err(|e| kind_of(&e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_input_has_edges_and_no_drive() {
        let mut config = LineConfig::input();
        config.edge = EdgeDetect::Both;
        config.drive = Drive::OpenDrain;
        config.bias = Bias::PullUp;
        assert_eq!(flags(&config), flag::RISING | flag::FALLING | flag::PULL_UP);
    }

    #[test]
    fn an_output_has_drive_and_no_edges() {
        let mut config = LineConfig::input();
        config.direction = Direction::Output;
        config.edge = EdgeDetect::Both;
        config.drive = Drive::OpenSource;
        config.active_low = true;
        assert_eq!(
            flags(&config),
            flag::OUTPUT | flag::OPEN_SOURCE | flag::ACTIVE_LOW
        );
    }
}
