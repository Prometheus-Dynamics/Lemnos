use embedded_hal::digital::{InputPin, StatefulOutputPin};
use lemnos_hal::ErrorKind;
use lemnos_hal::raw::{Bias, Direction, Drive, Edge, EdgeDetect, LineConfig, RawLine};

fn pin_error<E: embedded_hal::digital::Error>(error: E) -> ErrorKind {
    ErrorKind::from_digital(error.kind())
}

/// Whether `config` asks for nothing a pin's type doesn't already fix: the firmware chose its
/// bias and drive when it made the pin, and edges need its interrupt.
fn plain(config: &LineConfig) -> bool {
    config.bias == Bias::AsIs
        && config.drive == Drive::PushPull
        && config.edge == EdgeDetect::None
        && config.debounce_us == 0
}

/// An output pin as a line. Its direction, bias and drive are the pin type's.
#[derive(Debug)]
pub struct OutputLine<P> {
    pin: P,
    active_low: bool,
}

impl<P: StatefulOutputPin> OutputLine<P> {
    pub const fn new(pin: P) -> Self {
        Self {
            pin,
            active_low: false,
        }
    }

    pub fn into_inner(self) -> P {
        self.pin
    }
}

impl<P: StatefulOutputPin> RawLine for OutputLine<P> {
    fn configure(&mut self, config: &LineConfig) -> Result<(), ErrorKind> {
        if config.direction != Direction::Output || !plain(config) {
            return Err(ErrorKind::Unsupported);
        }
        self.active_low = config.active_low;
        self.set(config.initial)
    }

    fn get(&mut self) -> Result<bool, ErrorKind> {
        Ok(self.pin.is_set_high().map_err(pin_error)? != self.active_low)
    }

    fn set(&mut self, value: bool) -> Result<(), ErrorKind> {
        if value != self.active_low {
            self.pin.set_high().map_err(pin_error)
        } else {
            self.pin.set_low().map_err(pin_error)
        }
    }

    fn read_edge(&mut self) -> Result<Option<Edge>, ErrorKind> {
        Ok(None)
    }
}

/// An input pin as a line. Edges aren't seen: they need the firmware's interrupt.
#[derive(Debug)]
pub struct InputLine<P> {
    pin: P,
    active_low: bool,
}

impl<P: InputPin> InputLine<P> {
    pub const fn new(pin: P) -> Self {
        Self {
            pin,
            active_low: false,
        }
    }

    pub fn into_inner(self) -> P {
        self.pin
    }
}

impl<P: InputPin> RawLine for InputLine<P> {
    fn configure(&mut self, config: &LineConfig) -> Result<(), ErrorKind> {
        if config.direction != Direction::Input || !plain(config) {
            return Err(ErrorKind::Unsupported);
        }
        self.active_low = config.active_low;
        Ok(())
    }

    fn get(&mut self) -> Result<bool, ErrorKind> {
        Ok(self.pin.is_high().map_err(pin_error)? != self.active_low)
    }

    fn set(&mut self, _value: bool) -> Result<(), ErrorKind> {
        Err(ErrorKind::Unsupported)
    }

    fn read_edge(&mut self) -> Result<Option<Edge>, ErrorKind> {
        Ok(None)
    }
}
