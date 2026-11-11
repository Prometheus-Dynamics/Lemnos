//! The host's end of a loaded package: a [`Binder`] that has the package build its driver, and
//! the [`Instance`] the host then runs like any other. Also what the driver is given: the table
//! of handles behind the package's calls back into the host.

use std::panic::resume_unwind;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lemnos_abi::package::{
    DRIVER_TICKS, DRIVER_TRACKS_CALLERS, DriverTable, HostTable, Loaded, MAX_TRANSFER_STEPS,
    NEED_DEVICE, PackageBinding, PackageDriver, PackageHealth, PackageHost, PackageItem,
    PackageTransfer, TRANSFER_READ, host_table,
};
use lemnos_abi::{Handle, Slab};
use lemnos_core::package::{bus_of, health_in, health_out, item_in, settings_out, status_of_bus};
use lemnos_core::{
    BusKind, Caller, CallerId, Config, Description, Dev, Health, I2c, MAX_ITEMS, Manifest,
    MethodId, NeedKind, Reply, Resources, Result, Status,
};
use lemnos_hal::i2c::{I2c as _, Operation};
use lemnos_host::{Binder, DriverEntry, Instance};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Something a package's driver holds, by handle. Each is behind its own lock, so a call stuck
/// in one holds up nothing else.
#[derive(Clone)]
enum Resource {
    Device(Arc<Mutex<Dev<()>>>),
    I2c(Arc<Mutex<I2c>>),
}

/// Everything every package's driver holds. Locked only to look a handle up, never across a
/// call.
static RESOURCES: Mutex<Slab<Resource>> = Mutex::new(Slab::new());

pub(crate) fn resources_held() -> usize {
    lock(&RESOURCES).len()
}

fn resource(handle: Handle) -> Result<Resource> {
    lock(&RESOURCES).get(handle).cloned()
}

fn release(handle: Handle) -> Status {
    let removed = lock(&RESOURCES).remove(handle);
    // Dropped here, with the table unlocked.
    match removed {
        Ok(_) => Status::OK,
        Err(status) => status,
    }
}

/// The handles one bind gave out. Whatever the driver has not released by the time it is gone
/// is released here, so a driver that panics or forgets leaks nothing.
#[derive(Default)]
struct Taken(Vec<Handle>);

impl Drop for Taken {
    fn drop(&mut self) {
        for handle in self.0.drain(..) {
            let _ = release(handle);
        }
    }
}

/// What a binding driver may ask for: what its board entry says it may have.
struct Asked<'a, 'b> {
    resources: &'a mut Resources<'b>,
    manifest: &'static Manifest,
    taken: Taken,
}

impl PackageBinding for Asked<'_, '_> {
    fn provided(&mut self, need: &str) -> bool {
        self.resources.provided(need)
    }

    fn take(&mut self, kind: u32, need: &str, tag: &str) -> Result<Handle> {
        let resource = if kind == NEED_DEVICE {
            // Only a device the manifest declared, for the contract it declared.
            let declared = self.manifest.needs.iter().find_map(|n| match n.kind {
                NeedKind::Device(declared) if n.name == need && declared.as_str() == tag => {
                    Some(declared)
                }
                _ => None,
            });
            let declared = declared.ok_or(Status::INVALID_ARGUMENT)?;
            let caller = self.resources.provider().device(need, declared)?;
            Resource::Device(Arc::new(Mutex::new(Dev::new(caller))))
        } else {
            match bus_of(kind) {
                Some(BusKind::I2c) => {
                    Resource::I2c(Arc::new(Mutex::new(self.resources.i2c(need)?)))
                }
                _ => return Err(Status::NOT_SUPPORTED),
            }
        };
        let handle = lock(&RESOURCES).insert(resource)?;
        self.taken.0.push(handle);
        Ok(handle)
    }
}

/// What a bound driver may do with what it holds.
struct Held;

impl PackageHost for Held {
    fn release(resource: Handle) -> Status {
        release(resource)
    }

    fn device_call(device: Handle, method: u16, args: &[u8], reply: &mut [u8]) -> Result<usize> {
        let Resource::Device(device) = resource(device)? else {
            return Err(Status::INVALID_ARGUMENT);
        };
        let mut device = lock(&device);
        device
            .caller
            .args()
            .get_mut(..args.len())
            .ok_or(Status::BUFFER_TOO_SMALL)?
            .copy_from_slice(args);
        device.caller.call(method, args.len(), reply)
    }

    fn device_health(device: Handle) -> PackageHealth {
        match resource(device) {
            Ok(Resource::Device(device)) => health_out(lock(&device).health()),
            _ => health_out(Health::Failed("missing")),
        }
    }

    fn i2c_transaction(bus: Handle, address: u16, steps: &mut [PackageTransfer<'_>]) -> Status {
        let Ok(Resource::I2c(bus)) = resource(bus) else {
            return Status::STALE_HANDLE;
        };
        let Ok(address) = u8::try_from(address) else {
            return Status::INVALID_ARGUMENT;
        };
        if steps.len() > MAX_TRANSFER_STEPS {
            return Status::INVALID_ARGUMENT;
        }
        let count = steps.len();
        let mut operations: [Operation<'_>; MAX_TRANSFER_STEPS] =
            core::array::from_fn(|_| Operation::Write(&[]));
        for (operation, step) in operations.iter_mut().zip(steps.iter_mut()) {
            *operation = if step.kind == TRANSFER_READ {
                Operation::Read(step.read.as_mut_slice())
            } else {
                Operation::Write(step.write.as_slice())
            };
        }
        match lock(&bus).transaction(address, &mut operations[..count]) {
            Ok(()) => Status::OK,
            Err(kind) => status_of_bus(kind),
        }
    }
}

/// The one table every package's driver calls the host through.
static HOST: HostTable = host_table::<Asked<'static, 'static>, Held>();

/// A panic inside the package came back as a status. It goes on as a panic here, so the host's
/// own panic boundary sees what it sees from a built-in driver and does the same about it.
fn panicked() -> ! {
    resume_unwind(Box::new("a package's driver panicked"))
}

/// A package's bound driver, run by the host as any [`Instance`] is.
struct Running {
    // Dropped first: the package drops its driver, which releases what it holds.
    loaded: Loaded,
    _taken: Taken,
}

impl Instance for Running {
    fn describe(&self, out: &mut Description) {
        let mut items = [PackageItem::EMPTY; MAX_ITEMS];
        let count = match self.loaded.describe(&mut items) {
            Ok(count) => count as usize,
            Err(Status::PANIC) => panicked(),
            Err(_) => return,
        };
        let mut last = None;
        for item in items.iter().take(count).filter_map(item_in) {
            out.add(item);
            last = Some(item);
        }
        // Items the package had no room for count as left out here too.
        if let Some(item) = last {
            for _ in MAX_ITEMS..count.min(2 * MAX_ITEMS) {
                out.add(item);
            }
        }
    }

    fn health(&self) -> Health {
        match self.loaded.health() {
            Ok(health) => health_in(health),
            Err(Status::PANIC) => panicked(),
            Err(_) => Health::Failed("the package did not answer"),
        }
    }

    fn dispatch(&mut self, method: MethodId, args: &[u8], reply: &mut Reply<'_>) -> Status {
        let room = reply.remaining();
        let Ok(buffer) = reply.out(room) else {
            return Status::BUFFER_TOO_SMALL;
        };
        let (status, used) = self
            .loaded
            .dispatch(method.contract, method.method, args, buffer);
        if status == Status::PANIC {
            panicked();
        }
        match reply.advance(used) {
            Ok(()) => status,
            Err(status) => status,
        }
    }

    fn tick(&mut self, now_ns: u64) -> Option<u64> {
        match self.loaded.tick(now_ns) {
            Ok(next) => next,
            Err(Status::PANIC) => panicked(),
            Err(_) => None,
        }
    }

    fn caller(&mut self, caller: CallerId) {
        if self.loaded.caller(caller.0) == Status::PANIC {
            panicked();
        }
    }

    fn caller_gone(&mut self, caller: CallerId) {
        if self.loaded.caller_gone(caller.0) == Status::PANIC {
            panicked();
        }
    }
}

/// Has a package build its driver, on every bind and every retry.
struct PackageBinder {
    table: &'static DriverTable,
    manifest: &'static Manifest,
}

impl Binder for PackageBinder {
    fn bind(&self, res: &mut Resources<'_>, cfg: &Config<'_>) -> Result<Box<dyn Instance>> {
        let settings = settings_out(cfg);
        let link = res.link().map(str::to_owned);
        let mut asked = Asked {
            resources: res,
            manifest: self.manifest,
            taken: Taken::default(),
        };
        // A panic in the package's bind is `Status::PANIC` here, which is what the host records
        // for a built-in driver that panics while it binds.
        let loaded = Loaded::bind(self.table, &HOST, &mut asked, &settings, link.as_deref())?;
        Ok(Box::new(Running {
            loaded,
            _taken: asked.taken,
        }))
    }
}

/// The entry the host starts a package's driver from. Kept for good, as the package is.
pub(crate) fn entry(driver: &'static PackageDriver, manifest: &'static Manifest) -> DriverEntry {
    let binder: &'static PackageBinder = Box::leak(Box::new(PackageBinder {
        table: &driver.table,
        manifest,
    }));
    DriverEntry::bound_by(
        *manifest,
        driver.table.flags & DRIVER_TICKS != 0,
        driver.table.flags & DRIVER_TRACKS_CALLERS != 0,
        binder,
    )
}
