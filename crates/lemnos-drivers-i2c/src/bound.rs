use crate::stats::I2cStats;
use lemnos_bus::I2cSession;
use lemnos_core::{
    DeviceAddress, DeviceLifecycleState, DeviceStateSnapshot, I2cOperation, I2cRequest,
    I2cResponse, I2cTransactionResult, InteractionRequest, InteractionResponse,
};
use lemnos_driver_sdk::{
    BoundDevice, CONFIG_ADDRESS, CONFIG_ADDRESS_HEX, CONFIG_BUS, DriverError, DriverResult,
    TELEMETRY_READ_OPS, TELEMETRY_TRANSACTION_OPS, TELEMETRY_WRITE_OPS, TELEMETRY_WRITE_READ_OPS,
    execute_standard_request, impl_bound_device_core, impl_session_io, with_byte_telemetry,
    with_last_operation,
};

pub(crate) struct I2cBoundDevice {
    pub driver_id: String,
    pub session: Box<dyn I2cSession>,
    pub stats: I2cStats,
}

impl I2cBoundDevice {
    impl_session_io!(io, session, I2cDeviceIo);

    fn snapshot_state(&self) -> DeviceStateSnapshot {
        let mut state = with_byte_telemetry(
            DeviceStateSnapshot::new(self.session.device().id.clone())
                .with_lifecycle(DeviceLifecycleState::Idle)
                .with_telemetry(TELEMETRY_READ_OPS, self.stats.read_ops)
                .with_telemetry(TELEMETRY_WRITE_OPS, self.stats.write_ops)
                .with_telemetry(TELEMETRY_WRITE_READ_OPS, self.stats.write_read_ops)
                .with_telemetry(TELEMETRY_TRANSACTION_OPS, self.stats.transaction_ops),
            self.stats.bytes_read,
            self.stats.bytes_written,
        );

        state = with_last_operation(state, self.stats.last_operation.as_ref());

        if let Some(DeviceAddress::I2cDevice { bus, address }) = &self.session.device().address {
            state = state
                .with_config(CONFIG_BUS, u64::from(*bus))
                .with_config(CONFIG_ADDRESS, u64::from(*address))
                .with_config(CONFIG_ADDRESS_HEX, format!("0x{address:02x}"));
        }

        state
    }

    fn execute_i2c_request(&mut self, request: &I2cRequest) -> DriverResult<I2cResponse> {
        match request {
            I2cRequest::Read { length } => {
                let bytes = self.io().read(*length)?;
                self.stats
                    .record_read(&bytes, format!("read {} bytes", bytes.len()));
                Ok(I2cResponse::Bytes(bytes))
            }
            I2cRequest::Write { bytes } => {
                self.io().write(bytes)?;
                self.stats
                    .record_write(bytes.len(), format!("wrote {} bytes", bytes.len()));
                Ok(I2cResponse::Applied)
            }
            I2cRequest::WriteRead { write, read_length } => {
                let bytes = self.io().write_read(write, *read_length)?;
                self.stats.record_write_read(
                    write.len(),
                    &bytes,
                    format!("wrote {} bytes and read {} bytes", write.len(), bytes.len()),
                );
                Ok(I2cResponse::Bytes(bytes))
            }
            I2cRequest::Transaction { operations } => {
                let raw_results = self.io().transaction(operations)?;
                if raw_results.len() != operations.len() {
                    return Err(DriverError::InvariantViolation {
                        driver_id: self.driver_id.clone(),
                        device_id: self.session.device().id.clone(),
                        reason: format!(
                            "backend returned {} results for {} operations",
                            raw_results.len(),
                            operations.len()
                        ),
                    });
                }

                let mut results = Vec::with_capacity(operations.len());
                let mut bytes_written = 0_usize;
                let mut bytes_read = 0_usize;

                for (operation, raw) in operations.iter().zip(raw_results) {
                    match operation {
                        I2cOperation::Read { .. } => {
                            bytes_read += raw.len();
                            results.push(I2cTransactionResult::Read(raw));
                        }
                        I2cOperation::Write { bytes } => {
                            bytes_written += bytes.len();
                            results.push(I2cTransactionResult::Write {
                                bytes_written: bytes.len() as u32,
                            });
                        }
                    }
                }

                self.stats.record_transaction(
                    operations.len(),
                    bytes_written,
                    bytes_read,
                    format!(
                        "executed {} transaction operations ({} bytes written, {} bytes read)",
                        operations.len(),
                        bytes_written,
                        bytes_read
                    ),
                );
                Ok(I2cResponse::Transaction(results))
            }
        }
    }
}

impl BoundDevice for I2cBoundDevice {
    impl_bound_device_core!(session, infallible snapshot_state);

    fn execute(&mut self, request: &InteractionRequest) -> DriverResult<InteractionResponse> {
        execute_standard_request!(self, request, I2c(request) => I2c, {
            self.execute_i2c_request(request)?
        })
    }
}
