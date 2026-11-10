use embedded_hal::pwm::SetDutyCycle;
use lemnos_hal::ErrorKind;
use lemnos_hal::raw::{Polarity, PwmConfig, RawPwm};

/// An embedded-hal duty-cycle channel as a PWM channel. Its period is fixed by the firmware's
/// timer, so a configuration asking for another period is refused.
#[derive(Debug)]
pub struct PwmChannel<P> {
    channel: P,
    config: PwmConfig,
}

impl<P: SetDutyCycle> PwmChannel<P> {
    /// A channel whose timer runs at `period_ns`. It starts disabled.
    pub const fn new(channel: P, period_ns: u64) -> Self {
        Self {
            channel,
            config: PwmConfig {
                period_ns,
                duty_ns: 0,
                polarity: Polarity::Normal,
                enabled: false,
            },
        }
    }

    pub fn into_inner(self) -> P {
        self.channel
    }
}

impl<P: SetDutyCycle> RawPwm for PwmChannel<P> {
    fn configure(&mut self, config: &PwmConfig) -> Result<(), ErrorKind> {
        let period = self.config.period_ns;
        if config.period_ns != period {
            return Err(ErrorKind::Unsupported);
        }
        if period == 0 || config.duty_ns > period {
            return Err(ErrorKind::InvalidInput);
        }
        let high_ns = match (config.enabled, config.polarity) {
            (false, _) => 0,
            (true, Polarity::Normal) => config.duty_ns,
            (true, Polarity::Inversed) => period - config.duty_ns,
        };
        let max = u128::from(self.channel.max_duty_cycle());
        // At most `max`, which is a u16.
        let duty = (u128::from(high_ns) * max / u128::from(period)) as u16;
        self.channel
            .set_duty_cycle(duty)
            .map_err(|_| ErrorKind::Failed)?;
        self.config = *config;
        Ok(())
    }

    fn config(&mut self) -> Result<PwmConfig, ErrorKind> {
        Ok(self.config)
    }
}
