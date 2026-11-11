//! The host's side: a [`HostTable`] over the host's own code, and [`Loaded`], a package's bound
//! driver as the host holds it.

use core::ffi::c_void;

use super::{
    DriverTable, HostTable, NO_TICK, Out, PackageHealth, PackageItem, PackageSetting,
    PackageTransfer, Slice, SliceMut, Str,
};
use crate::{Handle, Status, guard, guard_or};

/// What a binding driver may ask for: the [`HostTable`]'s `provided` and `take`, in Rust.
pub trait PackageBinding {
    fn provided(&mut self, need: &str) -> bool;
    fn take(&mut self, kind: u32, need: &str, tag: &str) -> Result<Handle, Status>;
}

/// What a bound driver may ask of what it took: the rest of the [`HostTable`], in Rust.
pub trait PackageHost {
    fn release(resource: Handle) -> Status;
    fn device_call(
        device: Handle,
        method: u16,
        args: &[u8],
        reply: &mut [u8],
    ) -> Result<usize, Status>;
    fn device_health(device: Handle) -> PackageHealth;
    fn i2c_transaction(bus: Handle, address: u16, steps: &mut [PackageTransfer<'_>]) -> Status;
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
    guard(|| H::release(resource))
}

extern "C" fn device_call<H: PackageHost>(
    device: Handle,
    method: u16,
    args: Slice<'_, u8>,
    reply: SliceMut<'_, u8>,
    len: Out<'_, usize>,
) -> Status {
    guard(
        || match H::device_call(device, method, args.as_slice(), reply.into_slice()) {
            Ok(used) => {
                len.set(used);
                Status::OK
            }
            Err(status) if status.is_ok() => Status::IO,
            Err(status) => status,
        },
    )
}

extern "C" fn device_health<H: PackageHost>(
    device: Handle,
    health: Out<'_, PackageHealth>,
) -> Status {
    guard(|| {
        health.set(H::device_health(device));
        Status::OK
    })
}

extern "C" fn i2c_transaction<H: PackageHost>(
    bus: Handle,
    address: u16,
    mut steps: SliceMut<'_, PackageTransfer<'_>>,
) -> Status {
    guard(|| H::i2c_transaction(bus, address, steps.as_mut_slice()))
}

/// The table that serves a package's drivers: `B` while one binds, `H` afterwards.
pub const fn host_table<B: PackageBinding, H: PackageHost>() -> HostTable {
    HostTable {
        struct_size: size_of::<HostTable>() as u32,
        reserved: 0,
        provided: provided::<B>,
        take: take::<B>,
        release: release::<H>,
        device_call: device_call::<H>,
        device_health: device_health::<H>,
        i2c_transaction: i2c_transaction::<H>,
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
}

impl Drop for Loaded {
    fn drop(&mut self) {
        let _ = (self.table.drop)(self.driver);
    }
}
