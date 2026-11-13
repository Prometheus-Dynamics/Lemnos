//! The host's side: a [`HostTable`] over the host's own code, and [`Loaded`], a package's bound
//! driver as the host holds it.

use core::ffi::c_void;

use super::{
    ClocksHost, ClocksTable, DeviceHost, DeviceTable, DmaHost, DmaTable, DriverTable, HostTable,
    I2cHost, I2cTable, InterruptHost, InterruptTable, LineHost, LineTable, NO_TICK, Out,
    PackageExtra, PackageHealth, PackageItem, PackageJobState, PackageSetting, PwmHost, PwmTable,
    RegistersHost, RegistersTable, ResetHost, ResetTable, Slice, SliceMut, SpiHost, SpiTable, Str,
    StreamHost, StreamTable, UartHost, UartTable, UsbDeviceHost, UsbDeviceTable, UsbFunctionHost,
    UsbFunctionTable,
};
use crate::{Handle, Status, guard, guard_or};

/// What a binding driver may ask for: the [`HostTable`]'s `provided` and `take`, in Rust.
pub trait PackageBinding {
    fn provided(&mut self, need: &str) -> bool;
    /// `kind` is one of the `TAKE_*` values.
    fn take(&mut self, kind: u32, need: &str, tag: &str) -> Result<Handle, Status>;
}

/// What a bound driver may ask of what it took: `release`, and the table of every kind of
/// resource, in Rust. A kind the host does not have is left as it is, and answers
/// [`Status::NOT_SUPPORTED`].
pub trait PackageHost:
    DeviceHost
    + I2cHost
    + SpiHost
    + LineHost
    + PwmHost
    + UartHost
    + RegistersHost
    + InterruptHost
    + DmaHost
    + ClocksHost
    + ResetHost
    + StreamHost
    + UsbDeviceHost
    + UsbFunctionHost
{
    fn release(resource: Handle) -> Status;
}

/// The host's binding behind the pointer it gave `bind`, or `None` for null.
fn binding<'a, B: PackageBinding>(binding: *mut c_void) -> Option<&'a mut B> {
    // SAFETY: the driver passes back the pointer `Loaded::bind` made from a `&mut B`, during
    // that call only, on the thread that made it: this is the only reference in use.
    unsafe { binding.cast::<B>().as_mut() }
}

extern "C" fn provided<B: PackageBinding>(raw: *mut c_void, need: Str<'_>) -> u32 {
    guard_or(0, || {
        binding::<B>(raw).is_some_and(|binding| binding.provided(need.as_str())) as u32
    })
}

extern "C" fn take<B: PackageBinding>(
    raw: *mut c_void,
    kind: u32,
    need: Str<'_>,
    tag: Str<'_>,
    resource: Out<'_, Handle>,
) -> Status {
    guard(|| {
        let Some(binding) = binding::<B>(raw) else {
            return Status::INVALID_ARGUMENT;
        };
        match binding.take(kind, need.as_str(), tag.as_str()) {
            Ok(handle) => {
                resource.set(handle);
                Status::OK
            }
            Err(status) if status.is_ok() => Status::IO,
            Err(status) => status,
        }
    })
}

extern "C" fn release<H: PackageHost>(resource: Handle) -> Status {
    guard(|| <H as PackageHost>::release(resource))
}

/// The table that serves a package's drivers: `B` while one binds, `H` afterwards.
pub const fn host_table<B: PackageBinding, H: PackageHost>() -> HostTable {
    HostTable {
        struct_size: size_of::<HostTable>() as u32,
        reserved: 0,
        provided: provided::<B>,
        take: take::<B>,
        release: release::<H>,
        device: DeviceTable::over::<H>(),
        i2c: I2cTable::over::<H>(),
        spi: SpiTable::over::<H>(),
        line: LineTable::over::<H>(),
        pwm: PwmTable::over::<H>(),
        uart: UartTable::over::<H>(),
        registers: RegistersTable::over::<H>(),
        interrupt: InterruptTable::over::<H>(),
        dma: DmaTable::over::<H>(),
        clocks: ClocksTable::over::<H>(),
        reset: ResetTable::over::<H>(),
        stream: StreamTable::over::<H>(),
        usb_device: UsbDeviceTable::over::<H>(),
        usb_function: UsbFunctionTable::over::<H>(),
    }
}

/// A package's bound driver, as the host holds it. Dropping it has the package drop the driver.
///
/// Every call's status is the package's own. [`Status::PANIC`] means the driver panicked inside
/// the package and its state may be half-updated: let go of it.
#[derive(Debug)]
pub struct Loaded {
    table: &'static DriverTable,
    driver: *mut c_void,
}

// SAFETY: the pointer is a driver the package built from a type that is `Send`
// (`PackageInstance: Send`), and `Loaded` gives it out to nobody.
unsafe impl Send for Loaded {}

impl Loaded {
    /// Has the package build its driver. `binding` answers what the driver asks for while it
    /// binds; `host` must be the table made for `B` by [`host_table`].
    pub fn bind<B: PackageBinding>(
        table: &'static DriverTable,
        host: &'static HostTable,
        binding: &mut B,
        settings: &[PackageSetting<'_>],
        link: Option<&str>,
    ) -> Result<Self, Status> {
        let mut driver = core::ptr::null_mut();
        let status = (table.bind)(
            host,
            core::ptr::from_mut(binding).cast(),
            Slice::new(settings),
            Str::new(link.unwrap_or("")),
            link.is_some() as u32,
            Out::new(&mut driver),
        );
        if !status.is_ok() {
            return Err(status);
        }
        if driver.is_null() {
            return Err(Status::IO);
        }
        Ok(Self { table, driver })
    }

    /// Fills `items` and returns how many the device has in all.
    pub fn describe(&self, items: &mut [PackageItem]) -> Result<u32, Status> {
        let mut count = 0;
        let status = (self.table.describe)(self.driver, SliceMut::new(items), Out::new(&mut count));
        status.into_result().map(|_| count)
    }

    /// Fills `extras` and returns how many there are in all. `None` when the package has no
    /// extras (a package built before them): nothing is written then.
    pub fn describe_extra(&self, extras: &mut [PackageExtra]) -> Option<Result<u32, Status>> {
        let describe = self.table.describe_extra?;
        let mut count = 0;
        let status = describe(self.driver, SliceMut::new(extras), Out::new(&mut count));
        Some(status.into_result().map(|_| count))
    }

    /// The manifest's extras, given with no driver: `None` when the package has none.
    pub fn manifest_extra(
        table: &DriverTable,
        extras: &mut [PackageExtra],
    ) -> Option<Result<u32, Status>> {
        let describe = table.describe_extra?;
        let mut count = 0;
        let status = describe(
            core::ptr::null_mut(),
            SliceMut::new(extras),
            Out::new(&mut count),
        );
        Some(status.into_result().map(|_| count))
    }

    pub fn health(&self) -> Result<PackageHealth, Status> {
        let mut health = PackageHealth::OK;
        let status = (self.table.health)(self.driver, Out::new(&mut health));
        status.into_result().map(|_| health)
    }

    /// Runs a method: its status and the reply's length.
    pub fn dispatch(
        &mut self,
        contract: u8,
        method: u16,
        args: &[u8],
        reply: &mut [u8],
    ) -> (Status, usize) {
        let room = reply.len();
        let mut len = 0;
        let status = (self.table.dispatch)(
            self.driver,
            contract,
            method,
            Slice::new(args),
            SliceMut::new(reply),
            Out::new(&mut len),
        );
        (status, len.min(room))
    }

    pub fn tick(&mut self, now_ns: u64) -> Result<Option<u64>, Status> {
        let mut next = NO_TICK;
        let status = (self.table.tick)(self.driver, now_ns, Out::new(&mut next));
        status
            .into_result()
            .map(|_| (next != NO_TICK).then_some(next))
    }

    pub fn caller(&mut self, caller: u64) -> Status {
        (self.table.caller)(self.driver, caller)
    }

    pub fn caller_gone(&mut self, caller: u64) -> Status {
        (self.table.caller_gone)(self.driver, caller)
    }

    /// How a long job stands, and its result's length once it has finished.
    pub fn job(&mut self, job: u64, result: &mut [u8]) -> Result<(PackageJobState, usize), Status> {
        let room = result.len();
        let mut len = 0;
        let mut state = PackageJobState::default();
        let status = (self.table.job)(
            self.driver,
            job,
            SliceMut::new(result),
            Out::new(&mut len),
            Out::new(&mut state),
        );
        status.into_result().map(|_| (state, len.min(room)))
    }

    pub fn job_cancel(&mut self, job: u64) -> Status {
        (self.table.job_cancel)(self.driver, job)
    }
}

impl Drop for Loaded {
    fn drop(&mut self) {
        let _ = (self.table.drop)(self.driver);
    }
}
