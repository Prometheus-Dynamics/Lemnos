//! A PWM channel through the module.

use std::io;
use std::os::fd::{AsFd, OwnedFd};

use lemnos_hal::ErrorKind;
use lemnos_hal::raw::{Polarity, PwmConfig, RawPwm};
use lemnos_linux_sys::module::{self as sys, PwmState};

use super::Node;
use crate::kind_of;

/// One channel, held by a claim. Dropping it leaves the output as it was.
pub(crate) struct ModulePwm {
    node: Node,
    claim: OwnedFd,
}

impl ModulePwm {
    /// `provider` is the PWM chip's parent device name.
    pub(crate) fn open(node: Node, provider: &str, channel: u32) -> io::Result<Self> {
        let claim = node.claim(format_args!("pwm-{provider}:{channel}"))?;
        sys::pwm_request(node.fd(), claim.as_fd(), provider, channel)?;
        Ok(Self { node, claim })
    }
}

impl RawPwm for ModulePwm {
    fn configure(&mut self, config: &PwmConfig) -> Result<(), ErrorKind> {
        let state = PwmState {
            enabled: config.enabled,
            inverted: config.polarity == Polarity::Inversed,
            period_ns: config.period_ns,
            duty_ns: config.duty_ns,
        };
        sys::pwm_apply(self.node.fd(), self.claim.as_fd(), &state).map_err(|e| kind_of(&e))
    }

    fn config(&mut self) -> Result<PwmConfig, ErrorKind> {
        let state = sys::pwm_get(self.node.fd(), self.claim.as_fd()).map_err(|e| kind_of(&e))?;
        Ok(PwmConfig {
            period_ns: state.period_ns,
            duty_ns: state.duty_ns,
            polarity: if state.inverted {
                Polarity::Inversed
            } else {
                Polarity::Normal
            },
            enabled: state.enabled,
        })
    }
}
