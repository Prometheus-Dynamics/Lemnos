use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use lemnos_core::ErrorKind;
use lemnos_hal::spi::{ErrorType, Operation, SpiDevice};

#[derive(Debug, Default)]
struct SpiState {
    responses: VecDeque<u8>,
    written: Vec<Vec<u8>>,
}

/// An SPI device that clocks out queued bytes (0 when none are left) and
/// records each transaction's outgoing bytes. Clones share the device.
#[derive(Debug, Clone, Default)]
pub struct SimSpi(Arc<Mutex<SpiState>>);

impl SimSpi {
    pub fn new() -> Self {
        Self::default()
    }

    fn with<R>(&self, f: impl FnOnce(&mut SpiState) -> R) -> R {
        f(&mut self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }

    pub fn respond(&self, bytes: &[u8]) {
        self.with(|s| s.responses.extend(bytes.iter().copied()));
    }

    pub fn written(&self) -> Vec<Vec<u8>> {
        self.with(|s| s.written.clone())
    }
}

impl ErrorType for SimSpi {
    type Error = ErrorKind;
}

impl SpiDevice for SimSpi {
    fn transaction(&mut self, operations: &mut [Operation<'_, u8>]) -> Result<(), ErrorKind> {
        self.with(|s| {
            let mut outgoing = Vec::new();
            for operation in operations {
                let (read, wrote): (&mut [u8], Vec<u8>) = match operation {
                    Operation::Write(data) => (&mut [], data.to_vec()),
                    Operation::Read(out) => (out, Vec::new()),
                    Operation::Transfer(out, data) => (out, data.to_vec()),
                    Operation::TransferInPlace(data) => {
                        let wrote = data.to_vec();
                        (data, wrote)
                    }
                    Operation::DelayNs(_) => (&mut [], Vec::new()),
                };
                outgoing.extend(wrote);
                for byte in read {
                    *byte = s.responses.pop_front().unwrap_or(0);
                }
            }
            s.written.push(outgoing);
            Ok(())
        })
    }
}
