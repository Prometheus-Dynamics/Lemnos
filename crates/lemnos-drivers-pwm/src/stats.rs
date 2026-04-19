use lemnos_core::OperationRecord;
use lemnos_driver_sdk::{pwm, record_output_operation};

#[derive(Debug, Clone, Default)]
pub(crate) struct PwmStats {
    pub enable_ops: u64,
    pub configure_ops: u64,
    pub set_period_ops: u64,
    pub set_duty_cycle_ops: u64,
    pub last_operation: Option<OperationRecord>,
}

impl PwmStats {
    pub fn record_enable(&mut self, enabled: bool, summary: impl Into<String>) {
        self.enable_ops += 1;
        record_output_operation(
            &mut self.last_operation,
            pwm::ENABLE_INTERACTION,
            summary,
            enabled,
        );
    }

    pub fn record_configure(
        &mut self,
        period_ns: u64,
        duty_cycle_ns: u64,
        summary: impl Into<String>,
    ) {
        self.configure_ops += 1;
        record_output_operation(
            &mut self.last_operation,
            pwm::CONFIGURE_INTERACTION,
            summary,
            format!("{period_ns}:{duty_cycle_ns}"),
        );
    }

    pub fn record_set_period(&mut self, period_ns: u64, summary: impl Into<String>) {
        self.set_period_ops += 1;
        record_output_operation(
            &mut self.last_operation,
            pwm::SET_PERIOD_INTERACTION,
            summary,
            period_ns,
        );
    }

    pub fn record_set_duty_cycle(&mut self, duty_cycle_ns: u64, summary: impl Into<String>) {
        self.set_duty_cycle_ops += 1;
        record_output_operation(
            &mut self.last_operation,
            pwm::SET_DUTY_CYCLE_INTERACTION,
            summary,
            duty_cycle_ns,
        );
    }
}
