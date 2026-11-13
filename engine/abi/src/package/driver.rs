//! The package's side: a [`DriverTable`] over any type that implements [`PackageInstance`].

use alloc::boxed::Box;
use core::ffi::c_void;

use super::{
    DriverTable, HostTable, NO_TICK, Out, PackageExtra, PackageHealth, PackageItem,
    PackageJobState, PackageSetting, Slice, SliceMut, Str,
};
use crate::{Status, guard};

/// A driver as its package serves it to the host: the [`DriverTable`]'s functions, in Rust.
pub trait PackageInstance: Sized + Send + 'static {
    /// Builds the driver. `binding` is the host's, good for this call only.
    fn bind(
        host: &HostTable,
        binding: *mut c_void,
        settings: &[PackageSetting<'_>],
        link: Option<&str>,
    ) -> Result<Self, Status>;
    /// Fills `items` and returns how many there are in all.
    fn describe(&self, items: &mut [PackageItem]) -> u32;
    /// Fills `extras` with what the items carry beyond [`PackageItem`]: a reading, a step, warning
    /// limits and a group, each by the item's position in [`describe`](Self::describe). Returns
    /// how many there are in all. Nothing by default.
    fn describe_extra(&self, extras: &mut [PackageExtra]) -> u32 {
        let _ = extras;
        0
    }
    /// The contract kinds and method readings of the package's manifest, which need no driver:
    /// asked with a null driver. Nothing by default.
    fn manifest_extra(extras: &mut [PackageExtra]) -> u32
    where
        Self: Sized,
    {
        let _ = extras;
        0
    }
    fn health(&self) -> PackageHealth;
    /// Runs a method and returns its status and the reply's length.
    fn dispatch(
        &mut self,
        contract: u8,
        method: u16,
        args: &[u8],
        reply: &mut [u8],
    ) -> (Status, usize);
    fn tick(&mut self, now_ns: u64) -> Option<u64>;
    fn caller(&mut self, caller: u64);
    fn caller_gone(&mut self, caller: u64);
    /// How job `job` stands; when it has finished, its result's length in `result`.
    fn job(&mut self, job: u64, result: &mut [u8]) -> (PackageJobState, usize);
    fn job_cancel(&mut self, job: u64);
}

extern "C" fn describe_extra<T: PackageInstance>(
    driver: *mut c_void,
    mut extras: SliceMut<'_, PackageExtra>,
    count: Out<'_, u32>,
) -> Status {
    guard(|| {
        let written = if driver.is_null() {
            T::manifest_extra(extras.as_mut_slice())
        } else {
            let Some(instance) = instance::<T>(driver) else {
                return Status::STALE_HANDLE;
            };
            instance.describe_extra(extras.as_mut_slice())
        };
        count.set(written);
        Status::OK
    })
}

/// The driver behind the pointer `bind` gave the host, or `None` for null.
fn instance<'a, T: PackageInstance>(driver: *mut c_void) -> Option<&'a mut T> {
    // SAFETY: the host passes back what `bind` returned, a `Box<T>` made into a pointer, calls
    // one function at a time on it and none after `drop`: this is the only reference to it.
    unsafe { driver.cast::<T>().as_mut() }
}

extern "C" fn bind<T: PackageInstance>(
    host: &HostTable,
    binding: *mut c_void,
    settings: Slice<'_, PackageSetting<'_>>,
    link: Str<'_>,
    has_link: u32,
    driver: Out<'_, *mut c_void>,
) -> Status {
    guard(|| {
        let link = (has_link != 0).then(|| link.as_str());
        match T::bind(host, binding, settings.as_slice(), link) {
            Ok(instance) => {
                driver.set(Box::into_raw(Box::new(instance)).cast());
                Status::OK
            }
            // A bind that failed has no driver, whatever status it gave.
            Err(status) if status.is_ok() => Status::IO,
            Err(status) => status,
        }
    })
}

extern "C" fn describe<T: PackageInstance>(
    driver: *mut c_void,
    mut items: SliceMut<'_, PackageItem>,
    count: Out<'_, u32>,
) -> Status {
    guard(|| {
        let Some(instance) = instance::<T>(driver) else {
            return Status::STALE_HANDLE;
        };
        count.set(instance.describe(items.as_mut_slice()));
        Status::OK
    })
}

extern "C" fn health<T: PackageInstance>(
    driver: *mut c_void,
    health: Out<'_, PackageHealth>,
) -> Status {
    guard(|| {
        let Some(instance) = instance::<T>(driver) else {
            return Status::STALE_HANDLE;
        };
        health.set(instance.health());
        Status::OK
    })
}

extern "C" fn dispatch<T: PackageInstance>(
    driver: *mut c_void,
    contract: u8,
    method: u16,
    args: Slice<'_, u8>,
    reply: SliceMut<'_, u8>,
    len: Out<'_, usize>,
) -> Status {
    guard(|| {
        let Some(instance) = instance::<T>(driver) else {
            return Status::STALE_HANDLE;
        };
        let (status, used) =
            instance.dispatch(contract, method, args.as_slice(), reply.into_slice());
        len.set(used);
        status
    })
}

extern "C" fn tick<T: PackageInstance>(
    driver: *mut c_void,
    now_ns: u64,
    next_ns: Out<'_, u64>,
) -> Status {
    guard(|| {
        let Some(instance) = instance::<T>(driver) else {
            return Status::STALE_HANDLE;
        };
        next_ns.set(instance.tick(now_ns).unwrap_or(NO_TICK));
        Status::OK
    })
}

extern "C" fn caller<T: PackageInstance>(driver: *mut c_void, caller: u64) -> Status {
    guard(|| {
        let Some(instance) = instance::<T>(driver) else {
            return Status::STALE_HANDLE;
        };
        instance.caller(caller);
        Status::OK
    })
}

extern "C" fn caller_gone<T: PackageInstance>(driver: *mut c_void, caller: u64) -> Status {
    guard(|| {
        let Some(instance) = instance::<T>(driver) else {
            return Status::STALE_HANDLE;
        };
        instance.caller_gone(caller);
        Status::OK
    })
}

extern "C" fn job<T: PackageInstance>(
    driver: *mut c_void,
    job: u64,
    result: SliceMut<'_, u8>,
    len: Out<'_, usize>,
    state: Out<'_, PackageJobState>,
) -> Status {
    guard(|| {
        let Some(instance) = instance::<T>(driver) else {
            return Status::STALE_HANDLE;
        };
        let (now, used) = instance.job(job, result.into_slice());
        len.set(used);
        state.set(now);
        Status::OK
    })
}

extern "C" fn job_cancel<T: PackageInstance>(driver: *mut c_void, job: u64) -> Status {
    guard(|| {
        let Some(instance) = instance::<T>(driver) else {
            return Status::STALE_HANDLE;
        };
        instance.job_cancel(job);
        Status::OK
    })
}

extern "C" fn drop_driver<T: PackageInstance>(driver: *mut c_void) -> Status {
    guard(|| {
        if driver.is_null() {
            return Status::OK;
        }
        // SAFETY: `driver` is what `bind` returned, a `Box<T>` made into a pointer, and the host
        // never uses it again after this call.
        let instance = unsafe { Box::from_raw(driver.cast::<T>()) };
        drop(instance);
        Status::OK
    })
}

/// The table that serves `T` to the host. `flags` is [`DRIVER_TICKS`](super::DRIVER_TICKS),
/// [`DRIVER_TRACKS_CALLERS`](super::DRIVER_TRACKS_CALLERS) and [`DRIVER_JOBS`](super::DRIVER_JOBS).
pub const fn driver_table<T: PackageInstance>(flags: u32) -> DriverTable {
    DriverTable {
        struct_size: size_of::<DriverTable>() as u32,
        flags,
        bind: bind::<T>,
        describe: describe::<T>,
        health: health::<T>,
        dispatch: dispatch::<T>,
        tick: tick::<T>,
        caller: caller::<T>,
        caller_gone: caller_gone::<T>,
        drop: drop_driver::<T>,
        job: job::<T>,
        job_cancel: job_cancel::<T>,
        describe_extra: Some(describe_extra::<T>),
    }
}
