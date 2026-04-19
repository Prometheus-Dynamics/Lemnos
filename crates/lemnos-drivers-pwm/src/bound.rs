use crate::stats::PwmStats;
use crate::values::polarity_name;
use lemnos_bus::PwmSession;
use lemnos_core::{
    DeviceAddress, DeviceLifecycleState, DeviceStateSnapshot, InteractionRequest,
    InteractionResponse, PwmRequest, PwmResponse,
};
use lemnos_driver_sdk::{
    BoundDevice, CONFIG_CHANNEL, CONFIG_CHIP_NAME, CONFIG_DUTY_CYCLE_NS, CONFIG_ENABLED,
    CONFIG_PERIOD_NS, CONFIG_POLARITY, DriverResult, TELEMETRY_CONFIGURE_OPS,
    TELEMETRY_DUTY_CYCLE_PERCENT, TELEMETRY_DUTY_CYCLE_RATIO, TELEMETRY_ENABLE_OPS,
    TELEMETRY_SET_DUTY_CYCLE_OPS, TELEMETRY_SET_PERIOD_OPS, execute_standard_request,
    impl_bound_device_core, impl_session_io, with_last_operation,
};

pub(crate) struct PwmBoundDevice {
    pub driver_id: String,
    pub session: Box<dyn PwmSession>,
    pub stats: PwmStats,
}

impl PwmBoundDevice {
    impl_session_io!(io, session, PwmDeviceIo);

    fn snapshot_state(&mut self) -> DriverResult<DeviceStateSnapshot> {
        let configuration = self.io().configuration()?;
        let mut state = DeviceStateSnapshot::new(self.session.device().id.clone())
            .with_lifecycle(DeviceLifecycleState::Idle)
            .with_config(CONFIG_PERIOD_NS, configuration.period_ns)
            .with_config(CONFIG_DUTY_CYCLE_NS, configuration.duty_cycle_ns)
            .with_config(CONFIG_ENABLED, configuration.enabled)
            .with_telemetry(TELEMETRY_ENABLE_OPS, self.stats.enable_ops)
            .with_telemetry(TELEMETRY_CONFIGURE_OPS, self.stats.configure_ops)
            .with_telemetry(TELEMETRY_SET_PERIOD_OPS, self.stats.set_period_ops)
            .with_telemetry(TELEMETRY_SET_DUTY_CYCLE_OPS, self.stats.set_duty_cycle_ops)
            .with_config(CONFIG_POLARITY, polarity_name(configuration.polarity));

        state = with_last_operation(state, self.stats.last_operation.as_ref());

        if let Some(DeviceAddress::PwmChannel { chip_name, channel }) =
            &self.session.device().address
        {
            state = state
                .with_config(CONFIG_CHIP_NAME, chip_name.clone())
                .with_config(CONFIG_CHANNEL, u64::from(*channel));
        }

        let duty_cycle_ratio = if configuration.period_ns == 0 {
            0.0
        } else {
            configuration.duty_cycle_ns as f64 / configuration.period_ns as f64
        };

        Ok(state
            .with_telemetry(TELEMETRY_DUTY_CYCLE_RATIO, duty_cycle_ratio)
            .with_telemetry(TELEMETRY_DUTY_CYCLE_PERCENT, duty_cycle_ratio * 100.0))
    }
}

impl BoundDevice for PwmBoundDevice {
    impl_bound_device_core!(session, fallible snapshot_state);

    fn execute(&mut self, request: &InteractionRequest) -> DriverResult<InteractionResponse> {
        execute_standard_request!(self, request, Pwm(request) => Pwm, {
            match request {
                PwmRequest::Enable { enabled } => {
                    self.io().set_enabled(*enabled)?;
                    self.stats
                        .record_enable(*enabled, format!("set PWM enabled to {}", enabled));
                    PwmResponse::Applied
                }
                PwmRequest::Configure(configuration) => {
                    self.io().configure(configuration)?;
                    self.stats.record_configure(
                        configuration.period_ns,
                        configuration.duty_cycle_ns,
                        "configured PWM channel",
                    );
                    PwmResponse::Applied
                }
                PwmRequest::SetPeriod { period_ns } => {
                    self.io().set_period_ns(*period_ns)?;
                    self.stats.record_set_period(
                        *period_ns,
                        format!("set PWM period to {} ns", period_ns),
                    );
                    PwmResponse::Applied
                }
                PwmRequest::SetDutyCycle { duty_cycle_ns } => {
                    self.io().set_duty_cycle_ns(*duty_cycle_ns)?;
                    self.stats.record_set_duty_cycle(
                        *duty_cycle_ns,
                        format!("set PWM duty cycle to {} ns", duty_cycle_ns),
                    );
                    PwmResponse::Applied
                }
                PwmRequest::GetConfiguration => {
                    PwmResponse::Configuration(self.io().configuration()?)
                }
            }
        })
    }
}
