use crate::stats::UsbStats;
use crate::values::{hex_u16, ports_name};
use lemnos_bus::UsbSession;
use lemnos_core::{
    DeviceAddress, DeviceLifecycleState, DeviceStateSnapshot, InteractionRequest,
    InteractionResponse, UsbDirection, UsbRequest, UsbResponse,
};
use lemnos_driver_sdk::{
    BoundDevice, CONFIG_BUS, CONFIG_INTERFACE_NUMBER, CONFIG_PORTS, CONFIG_PRODUCT_ID,
    CONFIG_VENDOR_ID, DriverResult, TELEMETRY_BULK_READ_OPS, TELEMETRY_BULK_WRITE_OPS,
    TELEMETRY_CLAIM_OPS, TELEMETRY_CLAIMED_INTERFACE_COUNT, TELEMETRY_CONTROL_OPS,
    TELEMETRY_INTERRUPT_READ_OPS, TELEMETRY_INTERRUPT_WRITE_OPS, TELEMETRY_RELEASE_OPS,
    execute_standard_request, impl_bound_device_core, impl_session_io, with_byte_telemetry,
    with_last_operation,
};

pub(crate) struct UsbBoundDevice {
    pub driver_id: String,
    pub session: Box<dyn UsbSession>,
    pub stats: UsbStats,
}

impl UsbBoundDevice {
    impl_session_io!(io, session, UsbDeviceIo);

    fn snapshot_state(&self) -> DeviceStateSnapshot {
        let mut state = with_byte_telemetry(
            DeviceStateSnapshot::new(self.session.device().id.clone())
                .with_lifecycle(DeviceLifecycleState::Idle)
                .with_telemetry(TELEMETRY_CONTROL_OPS, self.stats.control_ops)
                .with_telemetry(TELEMETRY_BULK_READ_OPS, self.stats.bulk_read_ops)
                .with_telemetry(TELEMETRY_BULK_WRITE_OPS, self.stats.bulk_write_ops)
                .with_telemetry(TELEMETRY_INTERRUPT_READ_OPS, self.stats.interrupt_read_ops)
                .with_telemetry(
                    TELEMETRY_INTERRUPT_WRITE_OPS,
                    self.stats.interrupt_write_ops,
                )
                .with_telemetry(TELEMETRY_CLAIM_OPS, self.stats.claim_ops)
                .with_telemetry(TELEMETRY_RELEASE_OPS, self.stats.release_ops)
                .with_telemetry(
                    TELEMETRY_CLAIMED_INTERFACE_COUNT,
                    self.stats.claimed_interfaces.len() as u64,
                ),
            self.stats.bytes_read,
            self.stats.bytes_written,
        );

        state = with_last_operation(state, self.stats.last_operation.as_ref());

        match self.session.device().address.as_ref() {
            Some(DeviceAddress::UsbDevice {
                bus,
                ports,
                vendor_id,
                product_id,
            }) => {
                state = state
                    .with_config(CONFIG_BUS, u64::from(*bus))
                    .with_config(CONFIG_PORTS, ports_name(ports));
                if let Some(vendor_id) = vendor_id {
                    state = state.with_config(CONFIG_VENDOR_ID, hex_u16(*vendor_id));
                }
                if let Some(product_id) = product_id {
                    state = state.with_config(CONFIG_PRODUCT_ID, hex_u16(*product_id));
                }
            }
            Some(DeviceAddress::UsbInterface {
                bus,
                ports,
                interface_number,
                vendor_id,
                product_id,
            }) => {
                state = state
                    .with_config(CONFIG_BUS, u64::from(*bus))
                    .with_config(CONFIG_PORTS, ports_name(ports))
                    .with_config(CONFIG_INTERFACE_NUMBER, u64::from(*interface_number));
                if let Some(vendor_id) = vendor_id {
                    state = state.with_config(CONFIG_VENDOR_ID, hex_u16(*vendor_id));
                }
                if let Some(product_id) = product_id {
                    state = state.with_config(CONFIG_PRODUCT_ID, hex_u16(*product_id));
                }
            }
            _ => {}
        }

        state
    }

    fn execute_usb_request(&mut self, request: &UsbRequest) -> DriverResult<UsbResponse> {
        match request {
            UsbRequest::Control(transfer) => {
                let bytes = self.io().control_transfer(transfer)?;
                match transfer.setup.direction {
                    UsbDirection::In => self.stats.record_control_read(
                        &bytes,
                        format!("read {} bytes from control endpoint", bytes.len()),
                    ),
                    UsbDirection::Out => self.stats.record_control_write(
                        &transfer.data,
                        format!("wrote {} bytes in control transfer", transfer.data.len()),
                    ),
                }
                Ok(UsbResponse::Bytes(bytes))
            }
            UsbRequest::BulkRead {
                endpoint,
                length,
                timeout_ms,
            } => {
                let bytes = self.io().bulk_read(*endpoint, *length, *timeout_ms)?;
                self.stats.record_bulk_read(
                    &bytes,
                    format!(
                        "read {} bytes from bulk endpoint 0x{endpoint:02x}",
                        bytes.len()
                    ),
                );
                Ok(UsbResponse::Bytes(bytes))
            }
            UsbRequest::BulkWrite {
                endpoint,
                bytes,
                timeout_ms,
            } => {
                self.io().bulk_write(*endpoint, bytes, *timeout_ms)?;
                self.stats.record_bulk_write(
                    bytes,
                    format!(
                        "wrote {} bytes to bulk endpoint 0x{endpoint:02x}",
                        bytes.len()
                    ),
                );
                Ok(UsbResponse::Applied)
            }
            UsbRequest::InterruptRead {
                endpoint,
                length,
                timeout_ms,
            } => {
                let bytes = self.io().interrupt_read(*endpoint, *length, *timeout_ms)?;
                self.stats.record_interrupt_read(
                    &bytes,
                    format!(
                        "read {} bytes from interrupt endpoint 0x{endpoint:02x}",
                        bytes.len()
                    ),
                );
                Ok(UsbResponse::Bytes(bytes))
            }
            UsbRequest::InterruptWrite(transfer) => {
                self.io().interrupt_write(transfer)?;
                self.stats.record_interrupt_write(
                    &transfer.bytes,
                    format!(
                        "wrote {} bytes to interrupt endpoint 0x{:02x}",
                        transfer.bytes.len(),
                        transfer.endpoint
                    ),
                );
                Ok(UsbResponse::Applied)
            }
            UsbRequest::ClaimInterface {
                interface_number,
                alternate_setting,
            } => {
                self.io()
                    .claim_interface(*interface_number, *alternate_setting)?;
                self.stats.record_claim(
                    *interface_number,
                    *alternate_setting,
                    format!("claimed interface {interface_number}"),
                );
                Ok(UsbResponse::InterfaceClaimed {
                    interface_number: *interface_number,
                    alternate_setting: *alternate_setting,
                })
            }
            UsbRequest::ReleaseInterface { interface_number } => {
                self.io().release_interface(*interface_number)?;
                self.stats.record_release(
                    *interface_number,
                    format!("released interface {interface_number}"),
                );
                Ok(UsbResponse::InterfaceReleased {
                    interface_number: *interface_number,
                })
            }
        }
    }
}

impl BoundDevice for UsbBoundDevice {
    impl_bound_device_core!(session, infallible snapshot_state);

    fn execute(&mut self, request: &InteractionRequest) -> DriverResult<InteractionResponse> {
        execute_standard_request!(self, request, Usb(request) => Usb, {
            self.execute_usb_request(request)?
        })
    }
}
