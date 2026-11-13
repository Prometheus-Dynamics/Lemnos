use lemnos_hal::i2c::{self, I2c as HalI2c, Operation};
use lemnos_hal::raw::{RawLine, RawPwm, RawSpi};
use lemnos_hal::spi::{self, SpiDevice};
use lemnos_hal::{
    Clocks, DmaBuffers, ErrorKind, Interrupt, Registers, Reset, StreamEndpoint, Uart as UartTrait,
    UsbDevice, UsbFunction,
};

use crate::{Contract, Dev, LocalCaller, Result, Status, Tag};

pub use crate::held::{Held, Home, Lock};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BusKind {
    I2c,
    Spi,
    Gpio,
    Pwm,
    Uart,
    Usb,
    UsbFunction,
    Registers,
    Interrupt,
    Dma,
    Clocks,
    Reset,
    Stream,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeedKind {
    Bus(BusKind),
    Device(Tag),
}

/// One thing a driver needs, by the name the board file gives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Need {
    pub name: &'static str,
    pub kind: NeedKind,
    pub optional: bool,
}

/// An I2C bus.
pub struct I2c(pub Held<dyn HalI2c<Error = ErrorKind> + Send>);

impl i2c::ErrorType for I2c {
    type Error = ErrorKind;
}

impl HalI2c for I2c {
    fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> core::result::Result<(), ErrorKind> {
        self.0.transaction(address, operations)
    }
}

/// An SPI device: one bus and one chip select.
pub struct Spi(pub Held<dyn SpiDevice<Error = ErrorKind> + Send>);

impl spi::ErrorType for Spi {
    type Error = ErrorKind;
}

impl SpiDevice for Spi {
    fn transaction(
        &mut self,
        operations: &mut [spi::Operation<'_, u8>],
    ) -> core::result::Result<(), ErrorKind> {
        self.0.transaction(operations)
    }
}

pub type RawSpiBus = Held<dyn RawSpi + Send>;
pub type Line = Held<dyn RawLine + Send>;
pub type Pwm = Held<dyn RawPwm + Send>;
pub type Uart = Held<dyn UartTrait<Error = ErrorKind> + Send>;
pub type UsbDeviceHandle = Held<dyn UsbDevice<Error = ErrorKind> + Send>;
pub type UsbFunctionHandle = Held<dyn UsbFunction<Error = ErrorKind> + Send>;
pub type RegistersHandle = Held<dyn Registers<Error = ErrorKind> + Send>;
pub type InterruptHandle = Held<dyn Interrupt<Error = ErrorKind> + Send>;
pub type DmaHandle = Held<dyn DmaBuffers<Error = ErrorKind> + Send>;
pub type ClocksHandle = Held<dyn Clocks<Error = ErrorKind> + Send>;
pub type ResetHandle = Held<dyn Reset<Error = ErrorKind> + Send>;
pub type StreamHandle = Held<dyn StreamEndpoint<Error = ErrorKind> + Send>;

/// What a backend tells the host when hardware comes or goes, by the names a board file uses.
/// The host never learns what the hardware is: it looks for devices whose board entry has the
/// name.
///
/// A hint only makes the host act sooner. Everything it brings about also happens without it, by
/// the retries of the failure rule, so a hint that is lost or wrong costs time and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hint<'a> {
    /// What is behind this name may have arrived, left or changed. A device waiting to bind
    /// with it is tried now. A running device bound with it is ticked now, if its driver ticks,
    /// so the driver can look at what it holds and report [`Health::Gone`](crate::Health::Gone)
    /// if that is dead. The host drops no running driver on a hint alone.
    Changed(&'a str),
    /// Something changed and the backend can't say what: the same, for every device.
    Any,
}

/// Where a backend's [`Hint`]s come from. A host that wants them gives this a thread.
pub trait HintSource: Send {
    /// Waits up to `timeout` for hardware to come or go and reports what did. Returns `false`
    /// when there will never be more to report. It must come back by `timeout`, hints or not, so
    /// that whoever runs it can stop.
    fn wait(&mut self, timeout: core::time::Duration, report: &mut dyn FnMut(Hint<'_>)) -> bool;
}

/// What a host implements so drivers can get what they need. The default for
/// anything this target lacks is `NOT_SUPPORTED`.
pub trait Provider {
    /// Whether the board file supplies this need.
    fn provided(&self, need: &str) -> bool;
    /// Reports each resource that fits `pattern` now, by a name that reaches that one resource
    /// alone and stays the same for as long as it is wired the way it is (a USB device by its
    /// port, say). The host binds one device for each, for the board file's `[[each]]` entries.
    /// What a pattern can say is the backend's, as every name is. `NOT_SUPPORTED`, the default,
    /// when the backend finds nothing by pattern; `pattern` itself is never reported unless it
    /// is such a name.
    fn find(&self, _pattern: &str, _found: &mut dyn FnMut(&str)) -> Result<()> {
        Err(Status::NOT_SUPPORTED)
    }
    /// Whether `name`, any name or pattern a board file uses, reaches `found`, a name
    /// [`find`](Self::find) reported, now. The host keeps a resource a board entry names by
    /// any name from every `[[each]]` entry. The default says only that a name reaches itself.
    fn fits(&self, name: &str, found: &str) -> bool {
        name == found
    }
    fn device(&mut self, need: &str, tag: Tag) -> Result<LocalCaller>;

    fn i2c(&mut self, _need: &str) -> Result<I2c> {
        Err(Status::NOT_SUPPORTED)
    }
    fn spi(&mut self, _need: &str) -> Result<Spi> {
        Err(Status::NOT_SUPPORTED)
    }
    fn raw_spi(&mut self, _need: &str) -> Result<RawSpiBus> {
        Err(Status::NOT_SUPPORTED)
    }
    fn line(&mut self, _need: &str) -> Result<Line> {
        Err(Status::NOT_SUPPORTED)
    }
    fn pwm(&mut self, _need: &str) -> Result<Pwm> {
        Err(Status::NOT_SUPPORTED)
    }
    fn uart(&mut self, _need: &str) -> Result<Uart> {
        Err(Status::NOT_SUPPORTED)
    }
    fn usb_device(&mut self, _need: &str) -> Result<UsbDeviceHandle> {
        Err(Status::NOT_SUPPORTED)
    }
    fn usb_function(&mut self, _need: &str) -> Result<UsbFunctionHandle> {
        Err(Status::NOT_SUPPORTED)
    }
    fn registers(&mut self, _need: &str) -> Result<RegistersHandle> {
        Err(Status::NOT_SUPPORTED)
    }
    fn interrupt(&mut self, _need: &str) -> Result<InterruptHandle> {
        Err(Status::NOT_SUPPORTED)
    }
    fn dma(&mut self, _need: &str) -> Result<DmaHandle> {
        Err(Status::NOT_SUPPORTED)
    }
    fn clocks(&mut self, _need: &str) -> Result<ClocksHandle> {
        Err(Status::NOT_SUPPORTED)
    }
    fn reset(&mut self, _need: &str) -> Result<ResetHandle> {
        Err(Status::NOT_SUPPORTED)
    }
    fn stream(&mut self, _need: &str) -> Result<StreamHandle> {
        Err(Status::NOT_SUPPORTED)
    }
}

/// Everything a driver can ask for while it binds.
pub struct Resources<'a> {
    provider: &'a mut dyn Provider,
    link: Option<&'a str>,
}

impl<'a> Resources<'a> {
    pub fn new(provider: &'a mut dyn Provider, link: Option<&'a str>) -> Self {
        Self { provider, link }
    }

    /// Another device this one is wired to, from the board file's `link`.
    pub fn link(&self) -> Option<&str> {
        self.link
    }

    pub fn provided(&self, need: &str) -> bool {
        self.provider.provided(need)
    }

    pub fn device<C: Contract + ?Sized>(&mut self, need: &str) -> Result<Dev<C>> {
        self.provider.device(need, C::TAG).map(Dev::new)
    }

    pub fn provider(&mut self) -> &mut dyn Provider {
        self.provider
    }
}

/// Something a `#[needs]` field can be.
pub trait Resource: Sized {
    const KIND: NeedKind;
    const OPTIONAL: bool = false;

    fn take(res: &mut Resources<'_>, need: &str) -> Result<Self>;
}

macro_rules! resources {
    ($($t:ty => $kind:ident, $get:ident;)*) => {
        impl Resources<'_> {
            $(pub fn $get(&mut self, need: &str) -> Result<$t> {
                self.provider.$get(need)
            })*
        }
        $(impl Resource for $t {
            const KIND: NeedKind = NeedKind::Bus(BusKind::$kind);
            fn take(res: &mut Resources<'_>, need: &str) -> Result<Self> {
                res.$get(need)
            }
        })*
    };
}

resources! {
    I2c => I2c, i2c;
    Spi => Spi, spi;
    RawSpiBus => Spi, raw_spi;
    Line => Gpio, line;
    Pwm => Pwm, pwm;
    Uart => Uart, uart;
    UsbDeviceHandle => Usb, usb_device;
    UsbFunctionHandle => UsbFunction, usb_function;
    RegistersHandle => Registers, registers;
    InterruptHandle => Interrupt, interrupt;
    DmaHandle => Dma, dma;
    ClocksHandle => Clocks, clocks;
    ResetHandle => Reset, reset;
    StreamHandle => Stream, stream;
}

impl<C: Contract + ?Sized> Resource for Dev<C> {
    const KIND: NeedKind = NeedKind::Device(C::TAG);

    fn take(res: &mut Resources<'_>, need: &str) -> Result<Self> {
        res.device::<C>(need)
    }
}

impl<T: Resource> Resource for Option<T> {
    const KIND: NeedKind = T::KIND;
    const OPTIONAL: bool = true;

    fn take(res: &mut Resources<'_>, need: &str) -> Result<Self> {
        if res.provided(need) {
            T::take(res, need).map(Some)
        } else {
            Ok(None)
        }
    }
}
