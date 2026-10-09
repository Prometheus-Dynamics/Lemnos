//! The [`lemnos_hal::raw`] traits on Linux: GPIO character-device lines and
//! spidev devices, for hosts (such as `lemnosd`) that hand claimed lines and
//! devices to clients.

#[cfg(feature = "gpio-cdev")]
mod line {
    use crate::hal::{GpioLine, LineBias, LineDrive, LineEdge, LineSettings};
    use lemnos_hal::ErrorKind;
    use lemnos_hal::raw::{Bias, Direction, Drive, Edge, EdgeDetect, LineConfig, RawLine};
    use std::time::Duration;

    fn kind(error: std::io::Error) -> ErrorKind {
        ErrorKind::from_io(error.kind())
    }

    /// The character-device settings for `config`.
    pub fn settings(config: &LineConfig) -> LineSettings {
        let mut settings = match config.direction {
            Direction::Input => LineSettings::input(),
            Direction::Output => LineSettings::output(config.initial),
        };
        settings.active_low = config.active_low;
        settings.bias = match config.bias {
            Bias::AsIs => LineBias::AsIs,
            Bias::PullUp => LineBias::PullUp,
            Bias::PullDown => LineBias::PullDown,
            Bias::Disabled => LineBias::Disabled,
        };
        if config.direction == Direction::Output {
            settings.drive = match config.drive {
                Drive::PushPull => LineDrive::PushPull,
                Drive::OpenDrain => LineDrive::OpenDrain,
                Drive::OpenSource => LineDrive::OpenSource,
            };
        } else {
            settings.edge = match config.edge {
                EdgeDetect::None => LineEdge::None,
                EdgeDetect::Rising => LineEdge::Rising,
                EdgeDetect::Falling => LineEdge::Falling,
                EdgeDetect::Both => LineEdge::Both,
            };
            settings.debounce_us = (config.debounce_us > 0).then_some(config.debounce_us);
        }
        settings
    }

    impl RawLine for GpioLine {
        fn configure(&mut self, config: &LineConfig) -> Result<(), ErrorKind> {
            self.reconfigure(settings(config)).map_err(kind)
        }

        fn get(&mut self) -> Result<bool, ErrorKind> {
            GpioLine::get(self).map_err(kind)
        }

        fn set(&mut self, value: bool) -> Result<(), ErrorKind> {
            GpioLine::set(self, value).map_err(kind)
        }

        fn read_edge(&mut self) -> Result<Option<Edge>, ErrorKind> {
            Ok(self
                .wait_event(Some(Duration::ZERO))
                .map_err(kind)?
                .map(|event| Edge {
                    rising: event.kind == crate::hal::EdgeKind::Rising,
                    timestamp_ns: event.timestamp_ns,
                    seq: event.line_seqno,
                }))
        }
    }
}

#[cfg(feature = "gpio-cdev")]
pub use line::settings as line_settings;

#[cfg(feature = "spi")]
mod spi {
    use crate::hal::{SpiMode, SpiTransfer, Spidev};
    use lemnos_hal::ErrorKind;
    use lemnos_hal::raw::{RawSpi, SpiSegment};

    /// Mode is per device in spidev: one transaction uses one mode (the
    /// first segment's); speed and word size are per segment.
    impl RawSpi for Spidev {
        fn transfer(&mut self, segments: &mut [SpiSegment<'_>]) -> Result<(), ErrorKind> {
            let Some(first) = segments.first() else {
                return Ok(());
            };
            let mode = first.config.mode;
            if segments.iter().any(|s| s.config.mode != mode) {
                return Err(ErrorKind::InvalidInput);
            }
            let wanted = SpiMode::from_bits(u32::from(mode.bits()));
            let io = |e: std::io::Error| ErrorKind::from_io(e.kind());
            if self.mode().map_err(io)? != wanted {
                self.set_mode(wanted).map_err(io)?;
            }
            // Unequal halves go through padded buffers.
            let mut padded: Vec<(Vec<u8>, Vec<u8>)> = segments
                .iter()
                .map(|s| {
                    if !s.tx.is_empty() && !s.rx.is_empty() && s.tx.len() != s.rx.len() {
                        let n = s.tx.len().max(s.rx.len());
                        let mut tx = vec![0u8; n];
                        tx[..s.tx.len()].copy_from_slice(s.tx);
                        (tx, vec![0u8; n])
                    } else {
                        (Vec::new(), Vec::new())
                    }
                })
                .collect();
            {
                let mut transfers = Vec::with_capacity(segments.len());
                for (segment, (ptx, prx)) in segments.iter_mut().zip(padded.iter_mut()) {
                    let mut t = if !ptx.is_empty() {
                        SpiTransfer::duplex(prx, ptx).map_err(io)?
                    } else if !segment.tx.is_empty() && !segment.rx.is_empty() {
                        SpiTransfer::duplex(segment.rx, segment.tx).map_err(io)?
                    } else if !segment.tx.is_empty() {
                        SpiTransfer::write(segment.tx)
                    } else if !segment.rx.is_empty() {
                        SpiTransfer::read(segment.rx)
                    } else {
                        SpiTransfer::delay(0)
                    };
                    t.speed_hz = segment.config.speed_hz;
                    t.bits_per_word = segment.config.bits_per_word;
                    t.cs_change = segment.cs_change;
                    t.delay_us = segment.delay_us;
                    transfers.push(t);
                }
                Spidev::transfer(self, &mut transfers).map_err(io)?;
            }
            for (segment, (ptx, prx)) in segments.iter_mut().zip(&padded) {
                if !ptx.is_empty() {
                    let n = segment.rx.len();
                    segment.rx.copy_from_slice(&prx[..n]);
                }
            }
            Ok(())
        }
    }
}
