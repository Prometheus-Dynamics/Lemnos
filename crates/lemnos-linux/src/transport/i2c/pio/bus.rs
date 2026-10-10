//! One PIO I2C bus: the RP1 PIO state machine that runs the program, the pins
//! it owns, and the transactions over its FIFOs.

use super::program::{MAX_WRITES_PER_PUSH, PROGRAM_LEN, idle_wrap, initial_pc, program};
use super::protocol::{self, DecodeError, Nack, Op};
use lemnos_linux_sys::gpio;
use lemnos_linux_sys::rp1_pio::{self, PioDevice, SmConfig, override_value, regs};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The RP1 PIO character device.
pub const DEFAULT_DEVNODE: &str = "/dev/pio0";
/// Bounce-buffer size and count for each DMA direction (bytes, buffers).
const DMA_BUF_SIZE: u32 = 64;
const DMA_BUF_COUNT: u32 = 4;
/// The PIO clock on RP1 (`clock_get_hz(clk_sys)` in `pio_rp1.h`).
const PIO_CLOCK_HZ: f64 = 200_000_000.0;
/// PIO ticks per SCL period: 3 instructions of 8 cycles each per bit.
const TICKS_PER_BIT: f64 = 24.0;

/// What a transaction failed with.
#[derive(Debug)]
pub enum Failure {
    /// The target did not ACK the WRITE at this op.
    Nack(Nack),
    /// The FIFO transfer did not finish (the bus was stuck or the machine stalled).
    Timeout,
    /// Any other driver or protocol failure.
    Io(String),
}

/// A configured PIO I2C master on one SDA/SCL pair.
pub struct PioBus {
    sda: u8,
    scl: u8,
    device: PioDevice,
    devnode: PathBuf,
    sm: u16,
    base: u16,
    config: SmConfig,
    /// Serializes transactions: one state machine, one FIFO pair.
    gate: Mutex<()>,
}

impl std::fmt::Debug for PioBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PioBus")
            .field("sda", &self.sda)
            .field("scl", &self.scl)
            .field("devnode", &self.devnode)
            .field("sm", &self.sm)
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

/// SCL clock divider for `hz`: `200 MHz / (24 * hz)` as integer and 1/256 parts.
pub fn clkdiv_for(hz: u32) -> Result<(u16, u8), String> {
    if hz == 0 {
        return Err("PIO I2C frequency must be above zero".into());
    }
    let div = PIO_CLOCK_HZ / (TICKS_PER_BIT * f64::from(hz));
    let int = div.floor();
    if !(1.0..=65_535.0).contains(&int) {
        return Err(format!("PIO I2C frequency {hz} Hz is out of range"));
    }
    let frac = ((div - int) * 256.0).floor().clamp(0.0, 255.0) as u8;
    Ok((int as u16, frac))
}

/// The state machine configuration for `sda`/`scl` at `hz`.
pub fn sm_config(sda: u8, scl: u8, hz: u32) -> Result<SmConfig, String> {
    let (int, frac) = clkdiv_for(hz)?;
    Ok(SmConfig {
        clkdiv: regs::clkdiv(u32::from(int), u32::from(frac)),
        // Optional side-set drives SCL's pin direction; JMP pin is SDA.
        // Wrap over the whole instruction memory (the program never falls through its end).
        execctrl: regs::SIDE_PINDIR
            | regs::execctrl_jmp_pin(u32::from(sda))
            | regs::execctrl_wrap(0, 31),
        shiftctrl: regs::shiftctrl_in(false, false, 32) | regs::shiftctrl_out(false, false, 14),
        pinctrl: regs::pinctrl_out(u32::from(sda), 1)
            | regs::pinctrl_set(u32::from(scl), 1)
            | regs::pinctrl_in(u32::from(sda))
            | regs::pinctrl_sideset(u32::from(scl), 1),
    })
}

/// Fails if either pin is already requested from the kernel (for example by the
/// `i2c-gpio` overlay's driver). Looks for the RP1 GPIO chip by its label.
fn check_pins_free(sda: u8, scl: u8) -> Result<(), String> {
    for number in 0..16 {
        let path = format!("/dev/gpiochip{number}");
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        use std::os::fd::AsFd;
        let Ok(info) = gpio::chip_info(file.as_fd()) else {
            continue;
        };
        let label = name_of(&info.label);
        if !label.contains("rp1") {
            continue;
        }
        for pin in [sda, scl] {
            let Ok(line) = gpio::line_info(file.as_fd(), u32::from(pin)) else {
                continue;
            };
            if line.flags & gpio::flag::USED != 0 {
                return Err(format!(
                    "GPIO{pin} is already requested by '{}' ({path}); the PIO I2C bus needs \
                     SDA={sda} and SCL={scl} free: unbind the kernel i2c-gpio device first \
                     (see docs/system-service.md, PIO I2C bus)",
                    name_of(&line.consumer)
                ));
            }
        }
        return Ok(());
    }
    Ok(())
}

fn name_of(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

impl PioBus {
    /// Claims a state machine on `devnode`, loads the program, and configures
    /// SDA/SCL for `hz`. Fails clearly when the pins are owned by the kernel,
    /// when the device is missing or not permitted, or when no state machine or
    /// instruction space is free.
    pub fn open(devnode: &Path, sda: u8, scl: u8, hz: u32) -> Result<Self, String> {
        if sda == scl
            || u32::from(sda) >= rp1_pio::GPIO_COUNT
            || u32::from(scl) >= rp1_pio::GPIO_COUNT
        {
            return Err(format!(
                "PIO I2C pins sda={sda} scl={scl} must be distinct RP1 GPIOs"
            ));
        }
        check_pins_free(sda, scl)?;
        let config = sm_config(sda, scl, hz)?;
        let device = PioDevice::open(devnode).map_err(|e| {
            format!(
                "opening {} failed ({e}); the PIO block needs root or a rule for the device node",
                devnode.display()
            )
        })?;
        let sm = device
            .claim_any_sm()
            .map_err(|e| format!("claiming a PIO state machine failed: {e}"))?;
        let base = load_program(&device, scl)?;
        let config = with_idle_wrap(config, base);
        let setup = || -> io::Result<()> {
            for pin in [sda, scl] {
                let pin = u16::from(pin);
                device.gpio_init(pin)?;
                device.gpio_set_function(pin, rp1_pio::GPIO_FUNC_PIO)?;
                device.gpio_set_input_enabled(pin, true)?;
                // The pins only ever drive low (direction), so force the output low.
                device.gpio_set_outover(pin, override_value::LOW)?;
            }
            device.config_xfer32(sm, rp1_pio::DIR_TO_SM, DMA_BUF_SIZE, DMA_BUF_COUNT)?;
            device.config_xfer32(sm, rp1_pio::DIR_FROM_SM, DMA_BUF_SIZE, DMA_BUF_COUNT)?;
            device.sm_init(sm, initial_pc(base), config)?;
            device.sm_set_pindirs(sm, 0, pin_mask(sda, scl))?;
            device.sm_set_enabled(1 << sm, true)
        };
        if let Err(e) = setup() {
            let _ = device.remove_program(base, PROGRAM_LEN as u16);
            let _ = device.unclaim_sms(1 << sm);
            return Err(format!("configuring the PIO I2C bus failed: {e}"));
        }
        Ok(Self {
            sda,
            scl,
            device,
            devnode: devnode.to_path_buf(),
            sm,
            base,
            config,
            gate: Mutex::new(()),
        })
    }

    /// Runs `ops` to completion and returns the bytes read, in order.
    ///
    /// The TX words go to the machine on a helper thread while this thread
    /// drains the RX FIFO: the machine pushes a read byte per READ, so a long
    /// transaction would stall on a full RX FIFO if the TX DMA ran first.
    pub fn transaction(&self, ops: &[Op]) -> Result<Vec<u8>, Failure> {
        if let Err(writes) = protocol::check_ack_window(ops) {
            return Err(Failure::Io(format!(
                "{writes} writes between START/STOP exceed the PIO ACK window of {MAX_WRITES_PER_PUSH}"
            )));
        }
        let _guard = self
            .gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let tx = protocol::encode(self.base, u16::from(self.scl), ops);
        let mut rx = vec![0u32; protocol::rx_word_count(ops)];
        let (tx_result, rx_result) = std::thread::scope(|scope| {
            let sender = scope.spawn(|| self.device.xfer_to_sm(self.sm, &tx));
            let rx_result = self.device.xfer_from_sm(self.sm, &mut rx);
            let tx_result = sender
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("PIO TX thread panicked")));
            (tx_result, rx_result)
        });
        if let Err(error) = tx_result.and(rx_result) {
            self.recover();
            return Err(classify(error));
        }
        match protocol::decode(ops, &rx) {
            Ok(data) => Ok(data),
            Err(DecodeError::Nack(nack)) => Err(Failure::Nack(nack)),
            Err(DecodeError::Length { expected, actual }) => Err(Failure::Io(format!(
                "PIO returned {actual} RX words for {expected} expected"
            ))),
        }
    }

    /// Clears a stuck transfer: halts the machine, releases both pins, restarts
    /// it at the main loop, and clocks out nine bits and a STOP so a target
    /// holding SDA low lets go. Errors here are ignored; the next transaction
    /// reports the bus state.
    fn recover(&self) {
        let mask = 1u16 << self.sm;
        let _ = self.device.sm_set_enabled(mask, false);
        let _ = self.device.sm_clear_fifos(self.sm);
        let _ = self
            .device
            .sm_set_pindirs(self.sm, 0, pin_mask(self.sda, self.scl));
        let _ = self
            .device
            .sm_init(self.sm, initial_pc(self.base), self.config);
        let _ = self.device.sm_set_enabled(mask, true);
        let mut ops: Vec<Op> = vec![Op::Write(0xff); 9];
        ops.push(Op::Stop);
        let tx = protocol::encode(self.base, self.scl.into(), &ops);
        let mut rx = vec![0u32; protocol::rx_word_count(&ops)];
        let _ = std::thread::scope(|scope| {
            let sender = scope.spawn(|| self.device.xfer_to_sm(self.sm, &tx));
            let _ = self.device.xfer_from_sm(self.sm, &mut rx);
            sender.join()
        });
    }
}

impl Drop for PioBus {
    fn drop(&mut self) {
        // Stop the machine, release the pins, and hand them back to the kernel's
        // SIO function. Each step is best effort: the client close releases the rest.
        let _ = self.device.sm_set_enabled(1 << self.sm, false);
        let _ = self
            .device
            .sm_set_pindirs(self.sm, 0, pin_mask(self.sda, self.scl));
        for pin in [self.sda, self.scl] {
            let _ = self
                .device
                .gpio_set_outover(u16::from(pin), override_value::NORMAL);
            let _ = self
                .device
                .gpio_set_function(u16::from(pin), rp1_pio::GPIO_FUNC_SIO);
        }
        let _ = self.device.remove_program(self.base, PROGRAM_LEN as u16);
        let _ = self.device.unclaim_sms(1 << self.sm);
    }
}

/// The configuration with the wrap window of the idle loop at `base`.
fn with_idle_wrap(config: SmConfig, base: u16) -> SmConfig {
    let (bottom, top) = idle_wrap(base);
    const WRAP_BITS: u32 = 0x1f000 | 0xf80;
    SmConfig {
        execctrl: (config.execctrl & !WRAP_BITS)
            | regs::execctrl_wrap(u32::from(bottom), u32::from(top)),
        ..config
    }
}

fn pin_mask(sda: u8, scl: u8) -> u32 {
    (1u32 << sda) | (1u32 << scl)
}

/// Loads the program at the first origin that fits, returning its base.
fn load_program(device: &PioDevice, scl: u8) -> Result<u16, String> {
    let mut last = None;
    for base in 0..=(rp1_pio::INSTRUCTION_COUNT as u16 - PROGRAM_LEN as u16) {
        match device.add_program(base, &program(base, u16::from(scl))) {
            Ok(offset) if offset == base => return Ok(base),
            Ok(offset) => {
                let _ = device.remove_program(offset, PROGRAM_LEN as u16);
            }
            Err(e) => last = Some(e),
        }
    }
    Err(format!(
        "no free PIO instruction space for the I2C program ({})",
        last.map(|e| e.to_string())
            .unwrap_or_else(|| "all origins taken".into())
    ))
}

fn classify(error: io::Error) -> Failure {
    if error.kind() == io::ErrorKind::TimedOut {
        Failure::Timeout
    } else {
        Failure::Io(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_divider_gives_400khz_and_100khz() {
        // 200 MHz / (24 * 400 kHz) = 20.833...: integer 20, fraction 0.833 * 256 = 213.
        assert_eq!(clkdiv_for(400_000).unwrap(), (20, 213));
        // 200 MHz / (24 * 100 kHz) = 83.333...
        assert_eq!(clkdiv_for(100_000).unwrap(), (83, 85));
        assert!(clkdiv_for(0).is_err());
        assert!(clkdiv_for(1).is_err());
    }

    #[test]
    fn sm_config_places_the_pins() {
        let config = sm_config(8, 7, 400_000).unwrap();
        // OUT and IN base 8, SET base 7 (count 1), SIDESET base 7 (count 1).
        assert_eq!(config.pinctrl & 0x1f, 8);
        assert_eq!((config.pinctrl >> 15) & 0x1f, 8);
        assert_eq!((config.pinctrl >> 10) & 0x1f, 7);
        assert_eq!((config.pinctrl >> 5) & 0x1f, 7);
        assert_eq!((config.pinctrl >> 29) & 0x7, 1);
        assert_eq!(
            config.execctrl & regs::SIDE_EN,
            0,
            "side-set is non-optional"
        );
        assert_ne!(config.execctrl & regs::SIDE_PINDIR, 0);
    }

    #[test]
    fn pin_mask_covers_both_pins() {
        assert_eq!(pin_mask(8, 7), 0x180);
    }
}
