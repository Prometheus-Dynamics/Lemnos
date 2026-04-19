use crate::stats::GpioStats;
use crate::values::{bias_name, direction_name, drive_name, edge_name, level_name};
use lemnos_bus::GpioSession;
use lemnos_core::{
    DeviceLifecycleState, DeviceStateSnapshot, GpioLevel, GpioRequest, GpioResponse,
    InteractionRequest, InteractionResponse,
};
use lemnos_driver_sdk::{
    BoundDevice, CONFIG_ACTIVE_LOW, CONFIG_BIAS, CONFIG_DEBOUNCE_US, CONFIG_DIRECTION,
    CONFIG_DRIVE, CONFIG_EDGE, CONFIG_INITIAL_LEVEL, CONFIG_LEVEL, DriverResult,
    TELEMETRY_CONFIGURE_OPS, TELEMETRY_READ_OPS, TELEMETRY_WRITE_OPS, execute_standard_request,
    impl_bound_device_core, impl_session_io, with_last_operation,
};

pub(crate) struct GpioBoundDevice {
    pub driver_id: String,
    pub session: Box<dyn GpioSession>,
    pub stats: GpioStats,
}

impl GpioBoundDevice {
    fn level_label(level: GpioLevel) -> &'static str {
        match level {
            GpioLevel::Low => "low",
            GpioLevel::High => "high",
        }
    }

    impl_session_io!(io, session, GpioDeviceIo);

    fn snapshot_state(&mut self) -> DriverResult<DeviceStateSnapshot> {
        let configuration = self.io().configuration()?;
        let level = self.io().read_level()?;

        let mut state = DeviceStateSnapshot::new(self.session.device().id.clone())
            .with_lifecycle(DeviceLifecycleState::Idle)
            .with_config(CONFIG_DIRECTION, direction_name(configuration.direction))
            .with_config(CONFIG_ACTIVE_LOW, configuration.active_low)
            .with_telemetry(TELEMETRY_READ_OPS, self.stats.read_ops)
            .with_telemetry(TELEMETRY_WRITE_OPS, self.stats.write_ops)
            .with_telemetry(TELEMETRY_CONFIGURE_OPS, self.stats.configure_ops)
            .with_telemetry(CONFIG_LEVEL, level_name(level));

        state = with_last_operation(state, self.stats.last_operation.as_ref());

        if let Some(bias) = configuration.bias {
            state = state.with_config(CONFIG_BIAS, bias_name(bias));
        }
        if let Some(drive) = configuration.drive {
            state = state.with_config(CONFIG_DRIVE, drive_name(drive));
        }
        if let Some(edge) = configuration.edge {
            state = state.with_config(CONFIG_EDGE, edge_name(edge));
        }
        if let Some(debounce_us) = configuration.debounce_us {
            state = state.with_config(CONFIG_DEBOUNCE_US, u64::from(debounce_us));
        }
        if let Some(initial_level) = configuration.initial_level {
            state = state.with_config(CONFIG_INITIAL_LEVEL, level_name(initial_level));
        }

        Ok(state)
    }
}

impl BoundDevice for GpioBoundDevice {
    impl_bound_device_core!(session, fallible snapshot_state);

    fn execute(&mut self, request: &InteractionRequest) -> DriverResult<InteractionResponse> {
        execute_standard_request!(self, request, Gpio(request) => Gpio, {
            match request {
                GpioRequest::Read => {
                    let level = self.io().read_level()?;
                    let label = Self::level_label(level);
                    self.stats
                        .record_read(format!("read GPIO level {label}"), label);
                    GpioResponse::Level(level)
                }
                GpioRequest::Write { level } => {
                    self.io().write_level(*level)?;
                    let label = Self::level_label(*level);
                    self.stats
                        .record_write(format!("wrote GPIO level {label}"), label);
                    GpioResponse::Applied
                }
                GpioRequest::Configure(configuration) => {
                    self.io().configure_line(configuration)?;
                    self.stats.record_configure("configured GPIO line");
                    GpioResponse::Applied
                }
                GpioRequest::GetConfiguration => {
                    GpioResponse::Configuration(self.io().configuration()?)
                }
            }
        })
    }
}
