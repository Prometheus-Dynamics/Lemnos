//! Linux hardware for Lemnos, by the names a board file uses: `i2c-N`, `spidevB.C`,
//! `gpiochipN:LINE`, `pwmchipN:CHANNEL`, a tty such as `ttyAMA0`, a USB device by its port
//! (`usb:1-1.4`) or its vendor and product (`usb:1a2b:3c4d`), and this board as a USB device by
//! the controller it appears through (`udc:fe980000.usb`, see [`FunctionFsGadget`]), and, for a
//! block bound to the kernel module, by the block's device name: its DMA memory
//! (`dma:91400000.isp`, see [`ModuleDma`]), a register region (`regs:BLOCK/N`, see
//! [`ModuleRegisters`]), an interrupt (`irq:BLOCK/N`, see [`ModuleInterrupt`]), its clocks
//! (`clocks:BLOCK/NAME,...`, see [`ModuleClocks`]) and its reset lines (`resets:BLOCK/NAME,...`,
//! see [`ModuleReset`]).

#![cfg(target_os = "linux")]

mod gadget;
mod gpio;
pub mod hotplug;
mod i2c;
mod module;
mod pio_i2c;
mod pio_out;
mod pwm;
mod spi;
mod stable;
mod uart;
mod usb;

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use lemnos_core::{
    ClocksHandle, DmaHandle, I2c, InterruptHandle, Line, LocalCaller, Pio, Provider, Pwm,
    RawSpiBus, RegistersHandle, ResetHandle, Result, Spi, Status, Tag, Uart, UsbDeviceHandle,
    UsbFunctionHandle, status_of,
};
use lemnos_hal::ErrorKind;

pub use gadget::{FunctionFsGadget, Mounts, Owner, Roots, SystemMounts, prepare as prepare_gadget};
pub use gpio::GpioLine;
pub use i2c::I2cBus;
pub use module::{ModuleClocks, ModuleDma, ModuleInterrupt, ModuleRegisters, ModuleReset};
pub use pwm::SysfsPwm;
pub use spi::{SpiMode, Spidev};
pub use stable::{GpioChip, gpio_chips};
pub use uart::TtyUart;

/// The hardware of the machine this runs on.
#[derive(Clone)]
pub struct LinuxBoard {
    dev: PathBuf,
    sys: PathBuf,
    claimed: Claims,
    /// The kernel module's control node, once [`Self::open_module`] has it.
    module: Option<module::Node>,
}

type Claims = Arc<Mutex<BTreeSet<String>>>;

/// One device's hold on a node nothing else may share. Dropping it lets go.
pub(crate) struct Claim {
    claimed: Claims,
    node: String,
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.claimed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.node);
    }
}

impl Default for LinuxBoard {
    fn default() -> Self {
        Self::at("/dev", "/sys")
    }
}

impl LinuxBoard {
    pub fn new() -> Self {
        Self::default()
    }

    fn at(dev: impl Into<PathBuf>, sys: impl Into<PathBuf>) -> Self {
        Self {
            dev: dev.into(),
            sys: sys.into(),
            claimed: Claims::default(),
            module: None,
        }
    }

    /// Opens the kernel module, so the buses it serves go through it and the rest stay stock. The
    /// module has one opener, so only the host of a machine's hardware calls this.
    pub fn open_module(&mut self) -> io::Result<()> {
        self.module = Some(module::Node::open(self.dev.join("lemnos"))?);
        Ok(())
    }

    /// The module and adapter number, if the module can reach this adapter.
    fn module_i2c(&self, need: &str) -> Option<(module::Node, u32)> {
        let node = self.module.as_ref()?;
        let bus = need.strip_prefix(I2C)?.parse().ok()?;
        exists(Some(self.sys.join("bus/i2c/devices").join(need))).then(|| (node.clone(), bus))
    }

    /// The module, bus and chip select, if the module has this SPI device bound.
    fn module_spi(&self, need: &str) -> Option<(module::Node, u32, u32)> {
        let node = self.module.as_ref()?;
        let (bus, cs) = need.strip_prefix(SPI)?.split_once('.')?;
        let (bus, cs) = (bus.parse().ok()?, cs.parse().ok()?);
        node.serves_spi(bus, cs).then(|| (node.clone(), bus, cs))
    }

    fn open_module_spi(&self, need: &str) -> Result<Option<module::ModuleSpi>> {
        let Some((node, bus, cs)) = self.module_spi(need) else {
            return Ok(None);
        };
        served(module::ModuleSpi::open(node, bus, cs))
    }

    fn open_module_line(&self, need: &str) -> Result<Option<module::ModuleLine>> {
        let Some(node) = self.module.clone() else {
            return Ok(None);
        };
        let Some((chip, offset)) = self.gpio_number(need) else {
            return Ok(None);
        };
        served(module::ModuleLine::open(node, chip, offset))
    }

    fn open_module_pwm(&self, need: &str) -> Result<Option<module::ModulePwm>> {
        let (Some(node), Some((chip, channel))) = (self.module.clone(), self.pwm_node(need)) else {
            return Ok(None);
        };
        let Ok(parent) = std::fs::read_link(chip.join("device")) else {
            return Ok(None);
        };
        let Some(provider) = parent.file_name().and_then(|name| name.to_str()) else {
            return Ok(None);
        };
        served(module::ModulePwm::open(node, provider, channel))
    }

    /// The node a board file's name means, if the name is one of ours.
    fn node(&self, need: &str, prefix: &str) -> Option<PathBuf> {
        (need.len() > prefix.len() && need.starts_with(prefix) && !need.contains('/'))
            .then(|| self.dev.join(need))
    }

    fn claim(&self, need: &str) -> Result<Claim> {
        let mut claimed = self.claimed.lock().unwrap_or_else(PoisonError::into_inner);
        if !claimed.insert(need.to_string()) {
            return Err(Status::BUSY);
        }
        Ok(Claim {
            claimed: Arc::clone(&self.claimed),
            node: need.to_string(),
        })
    }

    /// The chip number and line a GPIO name means: `gpiochip0:17` as it is, or `gpio:LABEL:17`
    /// once the chip with that label is found. Resolved now, not kept: the kernel may renumber.
    fn gpio_number(&self, need: &str) -> Option<(u32, u32)> {
        if let Some(stable::Stable::Gpio { label, line }) = stable::parse(need) {
            return Some((stable::gpio_chip_number(&self.dev, label)?, line));
        }
        let (chip, offset) = split(need)?;
        Some((chip.strip_prefix(GPIO)?.parse().ok()?, offset))
    }

    /// The line a name like `gpiochip0:17` or `gpio:pinctrl-rp1:17` means.
    fn line_node(&self, need: &str) -> Option<(PathBuf, u32)> {
        let (chip, offset) = self.gpio_number(need)?;
        Some((self.node(&format!("{GPIO}{chip}"), GPIO)?, offset))
    }

    /// The chip directory and channel a name like `pwmchip0:1` or `pwm:/axi/...:1` means.
    fn pwm_node(&self, need: &str) -> Option<(PathBuf, u32)> {
        if let Some(stable::Stable::Pwm { node, channel }) = stable::parse(need) {
            return Some((stable::pwm_chip_dir(&self.sys, node)?, channel));
        }
        let (chip, channel) = split(need)?;
        let valid = chip.len() > PWM.len() && chip.starts_with(PWM) && !chip.contains('/');
        valid.then(|| (self.sys.join("class/pwm").join(chip), channel))
    }

    /// Where the kernel lists the USB device controllers.
    fn udc_class(&self) -> PathBuf {
        self.sys.join("class/udc")
    }

    fn open_spi(&self, need: &str) -> Result<Spidev> {
        let path = self.node(need, SPI).ok_or(Status::NOT_FOUND)?;
        let claim = self.claim(need)?;
        let device = Spidev::open_path(&path).map_err(|e| open_status(&e))?;
        Ok(device.holding(claim))
    }
}

const I2C: &str = "i2c-";
const SPI: &str = "spidev";
const GPIO: &str = "gpiochip";
const PWM: &str = "pwmchip";
const TTY: &str = "tty";

fn split(need: &str) -> Option<(&str, u32)> {
    let (name, number) = need.split_once(':')?;
    Some((name, number.parse().ok()?))
}

/// What opening through the module came to: the device, or `None` to take the stock path.
fn served<T>(opened: io::Result<T>) -> Result<Option<T>> {
    match opened {
        Ok(device) => Ok(Some(device)),
        Err(error) if module::not_served(&error) => Ok(None),
        Err(error) => Err(open_status(&error)),
    }
}

fn exists(path: Option<PathBuf>) -> bool {
    path.as_deref().is_some_and(Path::exists)
}

impl Provider for LinuxBoard {
    fn provided(&self, need: &str) -> bool {
        pio_i2c::provided(&self.dev, need)
            || pio_out::provided(&self.dev, need)
            || exists(self.node(need, I2C))
            || exists(self.node(need, SPI))
            || exists(self.node(need, TTY))
            || exists(self.line_node(need).map(|(path, _)| path))
            || exists(self.pwm_node(need).map(|(path, _)| path))
            || self.module_i2c(need).is_some()
            || self.module_spi(need).is_some()
            || usb::UsbName::parse(need).is_some_and(|name| usb::locate(&self.sys, name).is_ok())
            || exists(gadget::controller(need).map(|name| self.udc_class().join(name)))
            || self.module_has_block(need)
    }

    /// USB devices, by their ports: see [`usb`]. Nothing else is found by a pattern.
    fn find(&self, pattern: &str, found: &mut dyn FnMut(&str)) -> Result<()> {
        usb::find(&self.sys, pattern, found)
    }

    fn fits(&self, name: &str, found: &str) -> bool {
        name == found || usb::fits(&self.sys, name, found)
    }

    fn device(&mut self, _need: &str, _tag: Tag) -> Result<LocalCaller> {
        // Devices are the host's to wire. A board only has hardware.
        Err(Status::NOT_SUPPORTED)
    }

    fn i2c(&mut self, need: &str) -> Result<I2c> {
        if pio_i2c::names(need) {
            return Ok(I2c(lemnos_core::Held::boxed(Box::new(pio_i2c::open(
                &self.dev,
                need,
                self.module.as_ref(),
            )?))));
        }
        if let Some((node, bus)) = self.module_i2c(need) {
            return Ok(I2c(lemnos_core::Held::boxed(Box::new(
                module::ModuleI2c::new(node, bus),
            ))));
        }
        // Every device on a bus opens it: the kernel serialises their transfers.
        let path = self.node(need, I2C).ok_or(Status::NOT_FOUND)?;
        let bus = I2cBus::open_path(&path).map_err(|e| open_status(&e))?;
        Ok(I2c(lemnos_core::Held::boxed(Box::new(bus))))
    }

    fn spi(&mut self, need: &str) -> Result<Spi> {
        if let Some(device) = self.open_module_spi(need)? {
            return Ok(Spi(lemnos_core::Held::boxed(Box::new(device))));
        }
        Ok(Spi(lemnos_core::Held::boxed(Box::new(
            self.open_spi(need)?,
        ))))
    }

    fn raw_spi(&mut self, need: &str) -> Result<RawSpiBus> {
        if let Some(device) = self.open_module_spi(need)? {
            return Ok(lemnos_core::Held::boxed(Box::new(device)));
        }
        Ok(lemnos_core::Held::boxed(Box::new(self.open_spi(need)?)))
    }

    fn pio(&mut self, need: &str) -> Result<Pio> {
        Ok(lemnos_core::Held::boxed(Box::new(pio_out::open(
            &self.dev,
            need,
            self.module.as_ref(),
        )?)))
    }

    fn line(&mut self, need: &str) -> Result<Line> {
        if let Some(line) = self.open_module_line(need)? {
            return Ok(lemnos_core::Held::boxed(Box::new(line)));
        }
        // The kernel refuses a line that's held, so this needs no claim of its own.
        let (path, offset) = self.line_node(need).ok_or(Status::NOT_FOUND)?;
        let line = GpioLine::open(&path, offset).map_err(|e| open_status(&e))?;
        Ok(lemnos_core::Held::boxed(Box::new(line)))
    }

    fn pwm(&mut self, need: &str) -> Result<Pwm> {
        if let Some(pwm) = self.open_module_pwm(need)? {
            return Ok(lemnos_core::Held::boxed(Box::new(pwm)));
        }
        let (chip, channel) = self.pwm_node(need).ok_or(Status::NOT_FOUND)?;
        let claim = self.claim(need)?;
        let pwm = SysfsPwm::open(&chip, channel).map_err(|e| open_status(&e))?;
        Ok(lemnos_core::Held::boxed(Box::new(pwm.holding(claim))))
    }

    fn uart(&mut self, need: &str) -> Result<Uart> {
        let path = self.node(need, TTY).ok_or(Status::NOT_FOUND)?;
        let claim = self.claim(need)?;
        let uart = TtyUart::open_path(&path).map_err(|e| open_status(&e))?;
        Ok(lemnos_core::Held::boxed(Box::new(uart.holding(claim))))
    }

    /// The device is found by its name each time, so after it re-enumerates the same name, or
    /// the one for its new vendor and product, opens what is there now. One owner at a time,
    /// however the device was named.
    fn usb_device(&mut self, need: &str) -> Result<UsbDeviceHandle> {
        let name = usb::UsbName::parse(need).ok_or(Status::NOT_FOUND)?;
        let id = usb::locate(&self.sys, name)?;
        let claim = self.claim(&usb::claim_key(id))?;
        Ok(lemnos_core::Held::boxed(Box::new(usb::open(
            &self.dev, &self.sys, id, claim,
        )?)))
    }

    fn usb_function(&mut self, need: &str) -> Result<UsbFunctionHandle> {
        Ok(lemnos_core::Held::boxed(Box::new(
            self.open_usb_function(need)?,
        )))
    }

    fn dma(&mut self, need: &str) -> Result<DmaHandle> {
        Ok(lemnos_core::Held::boxed(Box::new(self.module_dma(need)?)))
    }

    fn registers(&mut self, need: &str) -> Result<RegistersHandle> {
        Ok(lemnos_core::Held::boxed(Box::new(
            self.module_registers(need)?,
        )))
    }

    fn interrupt(&mut self, need: &str) -> Result<InterruptHandle> {
        Ok(lemnos_core::Held::boxed(Box::new(
            self.module_interrupt(need)?,
        )))
    }

    fn clocks(&mut self, need: &str) -> Result<ClocksHandle> {
        Ok(lemnos_core::Held::boxed(Box::new(
            self.module_clocks(need)?,
        )))
    }

    fn reset(&mut self, need: &str) -> Result<ResetHandle> {
        Ok(lemnos_core::Held::boxed(Box::new(self.module_reset(need)?)))
    }
}

impl LinuxBoard {
    /// Whether `need` names one of a block's resources (`dma:`, `regs:`, `irq:`, `clocks:`,
    /// `resets:`) and that block is bound to the open module.
    fn module_has_block(&self, need: &str) -> bool {
        let block = module::dma::block_name(need).or_else(|| module::block::names_a_block(need));
        let (Some(block), Some(node)) = (block, self.module.as_ref()) else {
            return false;
        };
        node.has_block(block)
    }

    /// The open module, for a name that `parsed` says is one of a block's: `NOT_FOUND` for a
    /// name that isn't, `NOT_SUPPORTED` without the module. A block's resources come from the
    /// module alone, so nothing falls back.
    fn block_node<T>(&self, parsed: Option<T>) -> Result<(&module::Node, T)> {
        let parsed = parsed.ok_or(Status::NOT_FOUND)?;
        let node = self.module.as_ref().ok_or(Status::NOT_SUPPORTED)?;
        Ok((node, parsed))
    }

    /// The register region a `regs:<block>/<n>` name names, mapped through the module: see
    /// [`ModuleRegisters`]. Refused unless the module is open and has that block bound
    /// (`NOT_SUPPORTED`, `NO_DEVICE`), the region exists (`NOT_FOUND`), and the module says the
    /// block's registers may be mapped (`ACCESS`).
    pub fn module_registers(&self, need: &str) -> Result<ModuleRegisters> {
        let (node, (block, index)) =
            self.block_node(module::block::indexed(need, module::block::REGS))?;
        ModuleRegisters::open(node, block, index).map_err(status_of)
    }

    /// The interrupt an `irq:<block>/<n>` name names, through the module: see
    /// [`ModuleInterrupt`]. Refused as [`Self::module_registers`] is, but for mapping.
    pub fn module_interrupt(&self, need: &str) -> Result<ModuleInterrupt> {
        let (node, (block, index)) =
            self.block_node(module::block::indexed(need, module::block::IRQ))?;
        ModuleInterrupt::open(node, block, index).map_err(status_of)
    }

    /// The clocks a `clocks:<block>/<a>,<b>` name names, through the module: see
    /// [`ModuleClocks`]. A name the block doesn't have is refused here (`NOT_FOUND`).
    pub fn module_clocks(&self, need: &str) -> Result<ModuleClocks> {
        let (node, (block, names)) =
            self.block_node(module::block::named(need, module::block::CLOCKS))?;
        ModuleClocks::open(node, block, &names).map_err(status_of)
    }

    /// The reset lines a `resets:<block>/<a>,<b>` name names, through the module: see
    /// [`ModuleReset`]. A name the block doesn't have is refused here (`NOT_FOUND`).
    pub fn module_reset(&self, need: &str) -> Result<ModuleReset> {
        let (node, (block, names)) =
            self.block_node(module::block::named(need, module::block::RESETS))?;
        ModuleReset::open(node, block, &names).map_err(status_of)
    }

    /// DMA memory for the block a `dma:<block>` name names, from the kernel module: see
    /// [`ModuleDma`]. There is no other source of DMA memory, so this is refused, with no
    /// fallback, unless the module is open ([`Self::open_module`]) and has that block bound:
    /// `NOT_FOUND` for a name that isn't `dma:<block>`, `NOT_SUPPORTED` without the module,
    /// `NO_DEVICE` for a block the module doesn't have.
    pub fn module_dma(&self, need: &str) -> Result<ModuleDma> {
        let block = module::dma::block_name(need).ok_or(Status::NOT_FOUND)?;
        let node = self.module.clone().ok_or(Status::NOT_SUPPORTED)?;
        ModuleDma::open(node, block).map_err(status_of)
    }

    /// The gadget for a `udc:NAME` name: see [`FunctionFsGadget`]. The controller must be
    /// there, and one owner holds it at a time. Nothing is set up until the function is
    /// enabled.
    fn open_usb_function(&self, need: &str) -> Result<FunctionFsGadget> {
        let name = gadget::controller(need).ok_or(Status::NOT_FOUND)?;
        let class = self.udc_class();
        if !class.join(name).exists() {
            return Err(Status::NO_DEVICE);
        }
        let claim = self.claim(need)?;
        let configfs = self.sys.join("kernel/config");
        let gadget = FunctionFsGadget::new(&configfs, &self.dev.join("usb-ffs"), &class, name)
            .ok_or(Status::NOT_FOUND)?;
        Ok(gadget.holding(claim))
    }
}

/// What a failed I/O call amounts to.
pub(crate) fn kind_of(error: &io::Error) -> ErrorKind {
    error
        .raw_os_error()
        .map_or_else(|| ErrorKind::from_io(error.kind()), ErrorKind::from_errno)
}

/// Why a node couldn't be opened. `ENXIO` here is a node with no device behind it, where during
/// a transfer it would be a target that didn't answer.
fn open_status(error: &io::Error) -> Status {
    match error.raw_os_error() {
        Some(libc::ENOENT | libc::ENXIO | libc::ENODEV) => Status::NO_DEVICE,
        _ => status_of(kind_of(error)),
    }
}

#[cfg(test)]
#[global_allocator]
static ALLOCATOR: lemnos_measure::test_alloc::Counting = lemnos_measure::test_alloc::Counting;

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_board(name: &str) -> (LinuxBoard, PathBuf) {
        let dir = std::env::temp_dir().join(format!("lemnos-linux-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        (LinuxBoard::at(&dir, &dir), dir)
    }

    /// A machine whose `/dev` and `/sys` are a directory of the test's, with the I2C bus list in
    /// place and no adapter yet; the host on it, with a chip driver waiting on adapter 7.
    fn waiting_on_an_adapter(
        name: &str,
    ) -> (
        lemnos_host::Host,
        lemnos_sim::SimClock,
        PathBuf,
        hotplug::BoardHints,
    ) {
        use lemnos_host::board::parse;
        use lemnos_host::{Host, Options, entry};

        let (board, dir) = temp_board(name);
        let dir = std::fs::canonicalize(dir).unwrap();
        std::fs::create_dir_all(dir.join("bus/i2c/devices")).unwrap();
        std::fs::create_dir_all(dir.join("devices/platform/soc/i2c-7")).unwrap();
        let paths = hotplug::LinuxPaths::new()
            .with_sys_class_root(dir.join("class"))
            .with_sys_bus_root(dir.join("bus"))
            .with_dev_root(&dir);
        let filter = hotplug::BoardHints::filter_for(["i2c-7"]).unwrap();
        let watcher = hotplug::DeviceWatcher::inotify(paths, filter).expect("inotify");
        let hints = hotplug::BoardHints::new(watcher, ["i2c-7"]).unwrap();
        let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"t\"\n\
                    [[devices]]\nid = \"mag\"\ndriver = \"bmm150\"\nbus = \"i2c-7\"\n";
        let clock = lemnos_sim::SimBoard::new().clock();
        let host = Host::start(
            &parse(text).unwrap(),
            &[entry::<lemnos_driver_bmm150::Bmm150>()],
            Box::new(board),
            Arc::new(clock.clone()),
            Options::default(),
        )
        .unwrap();
        (host, clock, dir, hints)
    }

    /// How many times the device's bind has been tried and has failed.
    fn attempts(host: &lemnos_host::Host) -> u32 {
        match host.device("mag").unwrap().state {
            lemnos_host::DeviceState::Backoff { attempts, .. } => attempts,
            other => panic!("{other:?}"),
        }
    }

    /// The adapter arrives: the kernel lists it and makes its node. The node here is a plain
    /// file, which no I2C request works on, so a bind that reaches it fails: what the tests
    /// below count is when the bind is tried, which is all a hint changes.
    fn adapter_arrives(dir: &Path) {
        std::os::unix::fs::symlink(
            dir.join("devices/platform/soc/i2c-7"),
            dir.join("bus/i2c/devices/i2c-7"),
        )
        .unwrap();
        std::fs::write(dir.join("i2c-7"), b"").unwrap();
    }

    #[test]
    fn an_i2c_adapter_arriving_brings_the_bind_waiting_on_it_forward() {
        let (host, clock, dir, mut hints) = waiting_on_an_adapter("hint-i2c");
        assert_eq!(attempts(&host), 1, "no adapter yet");
        adapter_arrives(&dir);
        // Well before the retry, which is a second away, and past the gap between two binds.
        clock.advance(std::time::Duration::from_millis(300));
        let mut hinted = 0;
        hints.read(&mut |hint| {
            hinted += 1;
            host.hint(hint);
        });
        assert!(hinted >= 1, "the adapter's arrival was a hint");
        host.tick();
        assert_eq!(
            attempts(&host),
            2,
            "bound again at once, not a second later"
        );
        // A storm of the same hint binds nothing more.
        for _ in 0..100 {
            host.hint(lemnos_core::Hint::Changed("i2c-7"));
            host.tick();
        }
        assert_eq!(attempts(&host), 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn without_the_hint_the_same_bind_waits_for_its_retry() {
        let (host, clock, dir, _hints) = waiting_on_an_adapter("hint-none");
        adapter_arrives(&dir);
        clock.advance(std::time::Duration::from_millis(300));
        host.tick();
        assert_eq!(attempts(&host), 1, "nothing said the adapter came");
        clock.advance(std::time::Duration::from_millis(700));
        host.tick();
        assert_eq!(attempts(&host), 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn dma_memory_comes_only_from_the_module() {
        let (mut board, dir) = temp_board("dma");
        // Not a `dma:<block>` name.
        for name in ["dma:", "dma:a/b", "i2c-1", "91400000.isp"] {
            assert_eq!(board.dma(name).err(), Some(Status::NOT_FOUND), "{name}");
            assert!(!board.provided(name));
        }
        // No module: refused, with nothing else tried.
        assert_eq!(
            board.dma("dma:91400000.isp").err(),
            Some(Status::NOT_SUPPORTED)
        );
        assert!(!board.provided("dma:91400000.isp"));
        // A node that isn't the module answers no block request.
        board.module = Some(module::Node::open("/dev/null").unwrap());
        assert_eq!(
            board.dma("dma:91400000.isp").err(),
            Some(Status::NOT_SUPPORTED)
        );
        assert!(!board.provided("dma:91400000.isp"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_blocks_registers_interrupts_clocks_and_resets_come_only_from_the_module() {
        let (mut board, dir) = temp_board("block");
        let names = ["regs:blk/1", "irq:blk", "clocks:blk/core", "resets:blk/rst"];
        let open = |board: &mut LinuxBoard, name: &str| -> Vec<Option<Status>> {
            vec![
                board.registers(name).err(),
                board.interrupt(name).err(),
                board.clocks(name).err(),
                board.reset(name).err(),
            ]
        };
        // Not one of the names: each getter refuses what isn't its own.
        for (i, name) in names.iter().enumerate() {
            let mut expected = vec![Some(Status::NOT_FOUND); 4];
            expected[i] = Some(Status::NOT_SUPPORTED);
            assert_eq!(open(&mut board, name), expected, "{name}");
            assert!(!board.provided(name));
        }
        for name in ["regs:", "irq:blk/x", "clocks:blk/", "i2c-1", "blk"] {
            assert_eq!(
                open(&mut board, name),
                vec![Some(Status::NOT_FOUND); 4],
                "{name}"
            );
        }
        // A node that isn't the module answers no block request.
        board.module = Some(module::Node::open("/dev/null").unwrap());
        for (i, name) in names.iter().enumerate() {
            assert_eq!(
                open(&mut board, name)[i],
                Some(Status::NOT_SUPPORTED),
                "{name}"
            );
            assert!(!board.provided(name));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn names_stay_inside_dev() {
        let (board, dir) = temp_board("names");
        assert!(board.node("i2c-1", I2C).is_some());
        assert!(board.node("spidev0.1", SPI).is_some());
        assert!(board.node("i2c-", I2C).is_none());
        assert!(board.node("i2c-../../etc/passwd", I2C).is_none());
        assert!(board.node("spi0", SPI).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_node_is_provided_only_when_it_exists() {
        let (board, dir) = temp_board("provided");
        assert!(!board.provided("spidev0.0"));
        std::fs::write(dir.join("spidev0.0"), b"").unwrap();
        assert!(board.provided("spidev0.0"));
        assert!(!board.provided("pio-i2c:sda=8,scl=7"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn line_and_channel_names_are_split() {
        let (board, dir) = temp_board("split");
        assert_eq!(
            board.line_node("gpiochip0:17"),
            Some((dir.join("gpiochip0"), 17))
        );
        assert_eq!(board.line_node("gpiochip0"), None);
        assert_eq!(board.line_node("gpiochip0:x"), None);
        assert_eq!(board.line_node("gpiochip/../x:1"), None);
        assert_eq!(
            board.pwm_node("pwmchip2:1"),
            Some((dir.join("class/pwm/pwmchip2"), 1))
        );
        assert_eq!(board.pwm_node("pwmchip/x:1"), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_pwm_channel_has_one_owner_until_it_is_dropped() {
        let (mut board, dir) = temp_board("pwm");
        let root = dir.join("class/pwm/pwmchip0/pwm1");
        std::fs::create_dir_all(&root).unwrap();
        for name in ["period", "duty_cycle", "polarity", "enable"] {
            std::fs::write(root.join(name), "0\n").unwrap();
        }
        assert!(board.provided("pwmchip0:1"));
        let first = board.pwm("pwmchip0:1").map_err(|_| ()).unwrap();
        assert_eq!(
            board.pwm("pwmchip0:1").map(|_| ()).unwrap_err(),
            Status::BUSY
        );
        drop(first);
        assert!(board.pwm("pwmchip0:1").is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_spi_device_has_one_owner_until_it_is_dropped() {
        let (mut board, dir) = temp_board("claim");
        std::fs::write(dir.join("spidev0.0"), b"").unwrap();
        let first = board.open_spi("spidev0.0").unwrap();
        assert_eq!(
            board.raw_spi("spidev0.0").map(|_| ()).unwrap_err(),
            Status::BUSY
        );
        drop(first);
        assert!(board.open_spi("spidev0.0").is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_missing_node_is_no_device() {
        let (mut board, dir) = temp_board("missing");
        assert_eq!(
            board.i2c("i2c-99").map(|_| ()).unwrap_err(),
            Status::NO_DEVICE
        );
        assert_eq!(
            board.spi("spidev9.9").map(|_| ()).unwrap_err(),
            Status::NO_DEVICE
        );
        // A failed open doesn't keep the claim.
        assert_eq!(
            board.spi("spidev9.9").map(|_| ()).unwrap_err(),
            Status::NO_DEVICE
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A sysfs entry for a device, and a regular file where its node would be. No test goes
    /// near a real one.
    fn plug(dir: &Path, port: &str, bus: u8, address: u8, vendor: &str, product: &str) {
        assert!(!dir.starts_with("/dev") && !dir.starts_with("/sys"));
        let entry = dir.join("bus/usb/devices").join(port);
        std::fs::create_dir_all(&entry).unwrap();
        let attributes = [
            ("busnum", bus.to_string()),
            ("devnum", address.to_string()),
            ("idVendor", vendor.to_string()),
            ("idProduct", product.to_string()),
            ("bDeviceClass", "ef".to_string()),
            ("bDeviceSubClass", "02".to_string()),
            ("bDeviceProtocol", "01".to_string()),
            ("speed", "480".to_string()),
        ];
        for (name, value) in attributes {
            std::fs::write(entry.join(name), format!("{value}\n")).unwrap();
        }
        let node = dir.join(format!("bus/usb/{bus:03}"));
        std::fs::create_dir_all(&node).unwrap();
        // A device descriptor with one configuration, then that configuration.
        let mut descriptors = vec![18, 1, 0, 2, 0, 0, 0, 64, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1];
        descriptors.extend_from_slice(&[9, 2, 9, 0, 1, 1, 0, 0x80, 50]);
        std::fs::write(node.join(format!("{address:03}")), descriptors).unwrap();
    }

    #[test]
    fn usb_names_are_a_port_or_a_vendor_and_product() {
        use lemnos_hal::usb_match::UsbPattern;
        use usb::UsbName;
        assert_eq!(UsbName::parse("usb:1-1"), Some(UsbName::Port("1-1")));
        assert_eq!(
            UsbName::parse("usb:3-1.4.2"),
            Some(UsbName::Port("3-1.4.2"))
        );
        assert_eq!(
            UsbName::parse("usb:1A2b:00ff"),
            Some(UsbName::Is(UsbPattern::Id {
                vendor: 0x1a2b,
                product: 0x00ff
            }))
        );
        assert_eq!(
            UsbName::parse("usb:class:0e:01"),
            Some(UsbName::Is(UsbPattern::Class {
                class: 0x0e,
                subclass: Some(1),
                protocol: None
            }))
        );
        let refused = [
            "usb:",
            "usb:1",
            "usb:usb1",
            "usb:1-",
            "usb:-1",
            "usb:1-1.",
            "usb:1-1..2",
            "usb:1-1:1.0",
            "usb:1-../../x",
            "usb:1-1/x",
            "usb:1a2b",
            "usb:1a2b:3c4",
            "usb:1a2b:3c4d5",
            "usb:+a2b:3c4d",
            "usb:1a2b:3c4d:1",
            "usb:class:e",
            "usb:class:0e:01:00:00",
            "1-1",
            "ttyUSB0",
        ];
        for name in refused {
            assert_eq!(UsbName::parse(name), None, "{name}");
        }
    }

    #[test]
    fn a_usb_device_is_provided_while_its_name_finds_it() {
        let (mut board, dir) = temp_board("usb-provided");
        assert!(!board.provided("usb:1-4"));
        assert!(!board.provided("usb:1a2b:3c4d"));
        assert_eq!(
            board.usb_device("usb:1-4").map(|_| ()).unwrap_err(),
            Status::NO_DEVICE
        );
        assert_eq!(
            board.usb_device("usb:1a2b:3c4d").map(|_| ()).unwrap_err(),
            Status::NO_DEVICE
        );
        assert_eq!(
            board.usb_device("usb:bogus").map(|_| ()).unwrap_err(),
            Status::NOT_FOUND
        );
        plug(&dir, "1-4", 1, 7, "1a2b", "3c4d");
        assert!(board.provided("usb:1-4"));
        assert!(board.provided("usb:1a2b:3c4d"));
        assert!(!board.provided("usb:1-5"));
        assert!(!board.provided("usb:1a2b:3c4e"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_usb_name_opens_the_node_of_the_device_it_finds() {
        let (mut board, dir) = temp_board("usb-open");
        plug(&dir, "1-4", 1, 7, "1a2b", "3c4d");
        plug(&dir, "2-1.3", 2, 12, "0001", "0002");
        for name in ["usb:1-4", "usb:1a2b:3c4d", "usb:2-1.3", "usb:0001:0002"] {
            let mut device = board.usb_device(name).map_err(|_| ()).unwrap();
            let mut config = [0u8; 16];
            assert_eq!(device.config_descriptor(0, &mut config), Ok(9), "{name}");
            // The node is a regular file, so the kernel refuses what only a USB device takes.
            assert_eq!(device.reset(), Err(ErrorKind::Unsupported));
            assert_eq!(device.detach_kernel_driver(0), Err(ErrorKind::Unsupported));
            assert_eq!(device.claim_interface(0), Err(ErrorKind::Unsupported));
        }
        // A sysfs entry with no node behind it: the device left in between.
        std::fs::remove_file(dir.join("bus/usb/002/012")).unwrap();
        assert_eq!(
            board.usb_device("usb:2-1.3").map(|_| ()).unwrap_err(),
            Status::NO_DEVICE
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A sysfs entry for an interface of the device on `port`, as the kernel lists it beside
    /// the device.
    fn interface(dir: &Path, port: &str, number: u8, class: &str, subclass: &str) {
        let entry = dir
            .join("bus/usb/devices")
            .join(format!("{port}:1.{number}"));
        std::fs::create_dir_all(&entry).unwrap();
        for (name, value) in [
            ("bInterfaceClass", class),
            ("bInterfaceSubClass", subclass),
            ("bInterfaceProtocol", "00"),
        ] {
            std::fs::write(entry.join(name), format!("{value}\n")).unwrap();
        }
    }

    fn found(board: &LinuxBoard, pattern: &str) -> core::result::Result<Vec<String>, Status> {
        let mut names = Vec::new();
        board.find(pattern, &mut |name| names.push(name.to_string()))?;
        Ok(names)
    }

    #[test]
    fn a_usb_class_finds_every_device_with_it_by_port() {
        let (board, dir) = temp_board("usb-class");
        assert_eq!(found(&board, "usb:class:0e").unwrap(), Vec::<String>::new());
        // Two cameras alike and a disk, the cameras' class only on their interfaces.
        plug(&dir, "1-4", 1, 7, "1a2b", "3c4d");
        interface(&dir, "1-4", 0, "0e", "01");
        interface(&dir, "1-4", 1, "0e", "02");
        plug(&dir, "2-1.3", 2, 12, "1a2b", "3c4d");
        interface(&dir, "2-1.3", 0, "0e", "01");
        interface(&dir, "2-1.3", 1, "0e", "02");
        plug(&dir, "1-2", 1, 3, "0001", "0002");
        interface(&dir, "1-2", 0, "08", "06");
        // A root hub is not plugged in.
        std::fs::create_dir_all(dir.join("bus/usb/devices/usb1")).unwrap();

        assert_eq!(
            found(&board, "usb:class:0e").unwrap(),
            ["usb:1-4", "usb:2-1.3"]
        );
        assert_eq!(
            found(&board, "usb:class:0e:02:00").unwrap(),
            ["usb:1-4", "usb:2-1.3"]
        );
        assert_eq!(found(&board, "usb:class:08").unwrap(), ["usb:1-2"]);
        assert_eq!(found(&board, "usb:class:ef:02:01").unwrap().len(), 3);
        assert_eq!(
            found(&board, "usb:1a2b:3c4d").unwrap(),
            ["usb:1-4", "usb:2-1.3"]
        );
        assert_eq!(found(&board, "usb:2-1.3").unwrap(), ["usb:2-1.3"]);
        assert_eq!(found(&board, "usb:9-9").unwrap(), Vec::<String>::new());
        assert_eq!(found(&board, "i2c-1"), Err(Status::NOT_SUPPORTED));

        // Which names reach which device, now.
        assert!(board.fits("usb:1-4", "usb:1-4"));
        assert!(board.fits("usb:1a2b:3c4d", "usb:1-4"));
        assert!(board.fits("usb:class:0e:01", "usb:2-1.3"));
        assert!(!board.fits("usb:class:0e", "usb:1-2"));
        assert!(!board.fits("usb:1-4", "usb:2-1.3"));
        assert!(!board.fits("i2c-1", "usb:1-4"));
        assert!(!board.fits("usb:class:0e", "usb:9-9"));

        // A name that says what a device is opens it only when one device alone fits.
        assert!(board.provided("usb:class:08"));
        assert!(!board.provided("usb:class:0e"));
        assert!(!board.provided("usb:class:03"));
        let mut board = board;
        assert_eq!(
            board.usb_device("usb:class:0e").map(|_| ()).unwrap_err(),
            Status::INVALID_ARGUMENT
        );
        assert_eq!(
            board.usb_device("usb:class:03").map(|_| ()).unwrap_err(),
            Status::NO_DEVICE
        );
        let mut disk = board.usb_device("usb:class:08:06").map_err(|_| ()).unwrap();
        let mut config = [0u8; 16];
        assert_eq!(disk.config_descriptor(0, &mut config), Ok(9));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_usb_device_has_one_owner_until_it_is_dropped_however_it_is_named() {
        let (mut board, dir) = temp_board("usb-claim");
        plug(&dir, "1-4", 1, 7, "1a2b", "3c4d");
        let first = board.usb_device("usb:1-4").map_err(|_| ()).unwrap();
        for name in ["usb:1-4", "usb:1a2b:3c4d"] {
            assert_eq!(
                board.usb_device(name).map(|_| ()).unwrap_err(),
                Status::BUSY
            );
        }
        drop(first);
        assert!(board.usb_device("usb:1a2b:3c4d").is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_vendor_and_product_that_fit_two_devices_name_neither() {
        let (mut board, dir) = temp_board("usb-twins");
        plug(&dir, "1-4", 1, 7, "1a2b", "3c4d");
        plug(&dir, "1-5", 1, 8, "1a2b", "3c4d");
        assert!(!board.provided("usb:1a2b:3c4d"));
        assert_eq!(
            board.usb_device("usb:1a2b:3c4d").map(|_| ()).unwrap_err(),
            Status::INVALID_ARGUMENT
        );
        assert!(board.usb_device("usb:1-4").is_ok());
        assert!(board.usb_device("usb:1-5").is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_device_that_re_enumerated_is_found_by_the_name_that_fits_it_now() {
        let (mut board, dir) = temp_board("usb-again");
        plug(&dir, "1-4", 1, 7, "1a2b", "3c4d");
        drop(board.usb_device("usb:1a2b:3c4d").map_err(|_| ()).unwrap());
        // The same port, a new address and a new identity, as after a firmware load.
        std::fs::remove_dir_all(dir.join("bus/usb")).unwrap();
        plug(&dir, "1-4", 1, 8, "5e6f", "0001");
        assert!(!board.provided("usb:1a2b:3c4d"));
        assert!(board.usb_device("usb:5e6f:0001").is_ok());
        assert!(board.usb_device("usb:1-4").is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn many_usb_devices_are_all_looked_through() {
        let (board, dir) = temp_board("usb-many");
        for n in 1..=40u8 {
            plug(&dir, &format!("1-{n}"), 1, n, "1a2b", &format!("{n:04x}"));
        }
        assert!(board.provided("usb:1a2b:0028"));
        assert!(!board.provided("usb:1a2b:0029"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn open_errors_say_why() {
        let status = |errno| open_status(&io::Error::from_raw_os_error(errno));
        assert_eq!(status(libc::EBUSY), Status::BUSY);
        assert_eq!(status(libc::EACCES), Status::ACCESS);
        assert_eq!(status(libc::ENXIO), Status::NO_DEVICE);
    }
}
