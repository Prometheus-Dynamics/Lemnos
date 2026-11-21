//! The cause, on the UART, then the SoC reset. The UART is the only channel that survives the reset
//! (the stage keeps nothing in RAM for the next boot), so the cause is printed and the reset follows
//! it at once. The reset is the exit: the ROM's download mode comes back, and the bench plan
//! (`testing/board-runs/K230-USB.md`, step R5) is the proof it does.

use crate::sysctl::{SOC_GLB_RST, SOC_RESET_VALUE};
use crate::uart;
use crate::wdt::RegAccess;
use lemnos_flash_stage::guard::StartError;
use lemnos_flash_stage::guard::{Cause, Report};
use lemnos_flash_stage::link::IoError;
use lemnos_flash_stage::serve::ServeError;

/// The report of the K230 stage: UART first, then the SoC reset, which does not return.
pub struct Uart<R: RegAccess> {
    regs: R,
}

impl<R: RegAccess> Uart<R> {
    pub fn new(regs: R) -> Self {
        Self { regs }
    }
}

impl<R: RegAccess> Report for Uart<R> {
    fn cause(&mut self, cause: Cause) {
        let _ = uart::configure_if_unset(&mut self.regs);
        let _ = uart::write_line_parts(&mut self.regs, &["flash stage: ", describe(cause)]);
        self.regs.write32(SOC_GLB_RST, SOC_RESET_VALUE);
    }
}

/// The text of a cause. Static: the report runs with no allocator.
pub fn describe(cause: Cause) -> &'static str {
    match cause {
        Cause::Start(StartError::NotArmed) => "watchdog not armed; no USB or SD call was made",
        Cause::Start(StartError::NoPeriod { .. }) => "no watchdog period covers the session budget",
        Cause::Setup { step: 1 } => "bring-up step 1 failed (USB controller)",
        Cause::Setup { step: 2 } => "the host did not configure the device in time",
        Cause::Setup { step: 3 } => "bring-up step 3 failed (SD card)",
        Cause::Setup { .. } => "bring-up failed",
        Cause::Serve(ServeError::Idle) => "the host sent no request before the idle deadline",
        Cause::Serve(ServeError::Link(IoError::Timeout)) => "a link transfer timed out",
        Cause::Serve(ServeError::Link(IoError::Reset)) => "the bus was reset during a transfer",
        Cause::Serve(ServeError::Link(IoError::Failed)) => "the link failed",
        Cause::Ended(_) => "end frame served; resetting",
    }
}
