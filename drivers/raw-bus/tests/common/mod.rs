//! What the test files share: the driver reached the way a host reaches it (argument bytes in,
//! reply bytes out), wired to a simulated board.
#![allow(dead_code)]

use std::time::Duration;

use lemnos_contract_raw_bus::RawBus as RawBusApi;
use lemnos_core::{
    Bound, Caller, Config, ConfigValue, Driver, ErrorKind, LOCAL_ARGS, MethodId, Reply, Resources,
    Result,
};
use lemnos_driver_raw_bus::RawBus;
use lemnos_sim::mock::{MockLine, MockPwm, MockUart};
use lemnos_sim::{Layout, SimBoard, SimI2c, SimSpi};

/// The driver's one local caller: argument bytes in a `LOCAL_ARGS` buffer, as a driver-to-driver
/// call has them.
pub struct Direct<'a> {
    pub driver: &'a mut RawBus,
    args: [u8; LOCAL_ARGS],
}

impl Caller for Direct<'_> {
    fn args(&mut self) -> &mut [u8] {
        &mut self.args
    }

    fn call(&mut self, method: u16, args_len: usize, reply: &mut [u8]) -> Result<usize> {
        let mut writer = Reply::new(reply);
        let id = MethodId {
            contract: 0,
            method,
        };
        let status = self
            .driver
            .dispatch(id, &self.args[..args_len], &mut writer);
        if status.is_ok() {
            Ok(writer.len())
        } else {
            Err(status)
        }
    }
}

pub fn client(driver: &mut RawBus) -> Bound<dyn RawBusApi, Direct<'_>> {
    Bound::new(Direct {
        driver,
        args: [0; LOCAL_ARGS],
    })
}

/// Binds the driver to `board`, with `entries` as its `config`.
pub fn bind(board: &mut SimBoard, entries: &[(&str, ConfigValue<'_>)]) -> Result<RawBus> {
    let mut resources = Resources::new(board, None);
    RawBus::bind(&mut resources, &Config::new(entries))
}

/// A board with one of each resource, named as the driver's needs name them: `i2c`, `spi`, `line`,
/// `pwm` and `uart`. The I2C bus has chips at 0x17, 0x18, 0x19, 0x40, 0x50 (registers from 0x10), 0x68 and
/// 0x7f.
pub struct Wiring {
    pub board: SimBoard,
    pub i2c: SimI2c,
    pub spi: SimSpi,
    pub line: MockLine,
    pub pwm: MockPwm,
    pub uart: MockUart,
}

pub fn wired() -> Wiring {
    let board = SimBoard::new();
    let i2c = board.add_i2c("i2c");
    for address in [0x17, 0x18, 0x19, 0x40, 0x50, 0x68, 0x7f] {
        i2c.attach(address, Layout::Bytes);
    }
    i2c.set_bytes(0x50, 0x10, &[0xaa, 0xbb]);
    let spi = board.add_spi("spi");
    let line = board.add_line("line");
    let pwm = board.add_pwm("pwm");
    let uart = board.add_uart("uart");
    Wiring {
        board,
        i2c,
        spi,
        line,
        pwm,
        uart,
    }
}

/// Makes every bus of the board fail with `Busy` from now on. A call that reaches a bus then returns
/// `BUSY`, and a call the policy refuses still returns the policy's own answer. So a refused call
/// that returns its refusal, while the bus is down, never reached the bus.
pub fn all_buses_down(wiring: &Wiring) {
    for name in ["i2c", "spi", "line", "pwm", "uart"] {
        if let Some(faults) = wiring.board.faults(name) {
            faults.outage(Duration::ZERO, None, ErrorKind::Busy);
        }
    }
}
