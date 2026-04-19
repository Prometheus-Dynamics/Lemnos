use crate::stats::UartStats;
use crate::values::{data_bits_name, flow_control_name, parity_name, stop_bits_name};
use lemnos_bus::UartSession;
use lemnos_core::{
    DeviceAddress, DeviceLifecycleState, DeviceStateSnapshot, InteractionRequest,
    InteractionResponse, UartRequest, UartResponse,
};
use lemnos_driver_sdk::{
    BoundDevice, CONFIG_BAUD_RATE, CONFIG_DATA_BITS, CONFIG_FLOW_CONTROL, CONFIG_PARITY,
    CONFIG_PORT, CONFIG_STOP_BITS, DriverResult, TELEMETRY_FLUSH_OPS, TELEMETRY_READ_OPS,
    TELEMETRY_WRITE_OPS, execute_standard_request, impl_bound_device_core, impl_session_io,
    with_byte_telemetry, with_last_operation,
};

pub(crate) struct UartBoundDevice {
    pub driver_id: String,
    pub session: Box<dyn UartSession>,
    pub stats: UartStats,
}

impl UartBoundDevice {
    impl_session_io!(io, session, UartDeviceIo);

    fn snapshot_state(&mut self) -> DriverResult<DeviceStateSnapshot> {
        let configuration = self.io().configuration()?;

        let mut state = with_byte_telemetry(
            DeviceStateSnapshot::new(self.session.device().id.clone())
                .with_lifecycle(DeviceLifecycleState::Idle)
                .with_config(CONFIG_BAUD_RATE, u64::from(configuration.baud_rate))
                .with_config(CONFIG_DATA_BITS, data_bits_name(configuration.data_bits))
                .with_config(CONFIG_PARITY, parity_name(configuration.parity))
                .with_config(CONFIG_STOP_BITS, stop_bits_name(configuration.stop_bits))
                .with_config(
                    CONFIG_FLOW_CONTROL,
                    flow_control_name(configuration.flow_control),
                )
                .with_telemetry(TELEMETRY_READ_OPS, self.stats.read_ops)
                .with_telemetry(TELEMETRY_WRITE_OPS, self.stats.write_ops)
                .with_telemetry(TELEMETRY_FLUSH_OPS, self.stats.flush_ops),
            self.stats.bytes_read,
            self.stats.bytes_written,
        );

        state = with_last_operation(state, self.stats.last_operation.as_ref());

        if let Some(DeviceAddress::UartPort { port }) = &self.session.device().address {
            state = state.with_config(CONFIG_PORT, port.clone());
        }

        Ok(state)
    }

    fn execute_uart_request(&mut self, request: &UartRequest) -> DriverResult<UartResponse> {
        match request {
            UartRequest::Read { max_bytes } => {
                let bytes = self.io().read(*max_bytes)?;
                self.stats
                    .record_read(&bytes, format!("read {} bytes", bytes.len()));
                Ok(UartResponse::Bytes(bytes))
            }
            UartRequest::Write { bytes } => {
                self.io().write(bytes)?;
                self.stats
                    .record_write(bytes, format!("wrote {} bytes", bytes.len()));
                Ok(UartResponse::Applied)
            }
            UartRequest::Configure(configuration) => {
                self.io().configure(configuration)?;
                Ok(UartResponse::Applied)
            }
            UartRequest::Flush => {
                self.io().flush()?;
                self.stats.record_flush("flushed UART port");
                Ok(UartResponse::Applied)
            }
            UartRequest::GetConfiguration => {
                Ok(UartResponse::Configuration(self.io().configuration()?))
            }
        }
    }
}

impl BoundDevice for UartBoundDevice {
    impl_bound_device_core!(session, fallible snapshot_state);

    fn execute(&mut self, request: &InteractionRequest) -> DriverResult<InteractionResponse> {
        execute_standard_request!(self, request, Uart(request) => Uart, {
            self.execute_uart_request(request)?
        })
    }
}
