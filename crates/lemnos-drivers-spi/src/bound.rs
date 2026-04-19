use crate::stats::SpiStats;
use crate::values::{bit_order_name, mode_name};
use lemnos_bus::SpiSession;
use lemnos_core::{
    DeviceAddress, DeviceLifecycleState, DeviceStateSnapshot, InteractionRequest,
    InteractionResponse, SpiRequest, SpiResponse,
};
use lemnos_driver_sdk::{
    BoundDevice, CONFIG_BIT_ORDER, CONFIG_BITS_PER_WORD, CONFIG_BUS, CONFIG_CHIP_SELECT,
    CONFIG_MAX_FREQUENCY_HZ, CONFIG_MODE, DriverResult, TELEMETRY_TRANSFER_OPS,
    TELEMETRY_WRITE_OPS, execute_standard_request, impl_bound_device_core, impl_session_io,
    with_byte_telemetry, with_last_operation,
};

pub(crate) struct SpiBoundDevice {
    pub driver_id: String,
    pub session: Box<dyn SpiSession>,
    pub stats: SpiStats,
}

impl SpiBoundDevice {
    impl_session_io!(io, session, SpiDeviceIo);

    fn snapshot_state(&mut self) -> DriverResult<DeviceStateSnapshot> {
        let configuration = self.io().configuration()?;
        let mut state = with_byte_telemetry(
            DeviceStateSnapshot::new(self.session.device().id.clone())
                .with_lifecycle(DeviceLifecycleState::Idle)
                .with_config(CONFIG_MODE, mode_name(configuration.mode))
                .with_config(CONFIG_BIT_ORDER, bit_order_name(configuration.bit_order))
                .with_telemetry(TELEMETRY_TRANSFER_OPS, self.stats.transfer_ops)
                .with_telemetry(TELEMETRY_WRITE_OPS, self.stats.write_ops),
            self.stats.bytes_read,
            self.stats.bytes_written,
        );

        if let Some(max_frequency_hz) = configuration.max_frequency_hz {
            state = state.with_config(CONFIG_MAX_FREQUENCY_HZ, u64::from(max_frequency_hz));
        }
        if let Some(bits_per_word) = configuration.bits_per_word {
            state = state.with_config(CONFIG_BITS_PER_WORD, u64::from(bits_per_word));
        }

        state = with_last_operation(state, self.stats.last_operation.as_ref());

        if let Some(DeviceAddress::SpiDevice { bus, chip_select }) = &self.session.device().address
        {
            state = state
                .with_config(CONFIG_BUS, u64::from(*bus))
                .with_config(CONFIG_CHIP_SELECT, u64::from(*chip_select));
        }

        Ok(state)
    }

    fn execute_spi_request(&mut self, request: &SpiRequest) -> DriverResult<SpiResponse> {
        match request {
            SpiRequest::Transfer { write } => {
                let bytes = self.io().transfer(write)?;
                self.stats.record_transfer(
                    write,
                    &bytes,
                    format!("transferred {} bytes", write.len()),
                );
                Ok(SpiResponse::Bytes(bytes))
            }
            SpiRequest::Write { bytes } => {
                self.io().write(bytes)?;
                self.stats
                    .record_write(bytes, format!("wrote {} bytes", bytes.len()));
                Ok(SpiResponse::Applied)
            }
            SpiRequest::Configure(configuration) => {
                self.io().configure(configuration)?;
                Ok(SpiResponse::Applied)
            }
            SpiRequest::GetConfiguration => {
                Ok(SpiResponse::Configuration(self.io().configuration()?))
            }
        }
    }
}

impl BoundDevice for SpiBoundDevice {
    impl_bound_device_core!(session, fallible snapshot_state);

    fn execute(&mut self, request: &InteractionRequest) -> DriverResult<InteractionResponse> {
        execute_standard_request!(self, request, Spi(request) => Spi, {
            self.execute_spi_request(request)?
        })
    }
}
