//! The driver package boundary: what a compiled driver file and the host that loads it say to
//! each other. Both sides are in one process, and nothing but the types here crosses.
//!
//! A package is a shared object with one data symbol, [`PACKAGE_ENTRY`], a
//! [`PackageDescriptor`]. The host reads its magic and version before it runs anything of the
//! package's, then calls [`PackageDescriptor::register`] once for the driver's manifest and its
//! [`DriverTable`]. The host hands the driver a [`HostTable`] when it binds: what the driver is
//! given comes back as [`Handle`]s that only the host can resolve, so a package links none of the
//! host's types. Every kind of resource a driver can be given crosses: the [`HostTable`] holds
//! one small table of functions for each (see [`tables`]).
//!
//! `include/lemnos/package.h` is the same boundary as a C header, checked against these types.
//!
//! Pointers cross inside [`Str`], [`Slice`], [`SliceMut`] and [`Out`]. Each is made only from a
//! Rust reference, so reading one back is sound for as long as its lifetime says. `'static` here
//! means "for as long as the package is loaded", and a package is never unloaded.
//!
//! Every entry point either side exports runs inside [`guard`](crate::guard): a panic comes back
//! as [`Status::PANIC`] and never unwinds across the boundary. A package must be built with
//! `panic = "unwind"` for that to hold.

use core::ffi::{CStr, c_void};
use core::marker::PhantomData;
use core::mem::size_of;

use crate::{AbiVersion, Handle, Status, layout_hash};

#[cfg(feature = "std")]
mod driver;
#[cfg(feature = "std")]
mod host;
pub mod tables;

#[cfg(feature = "std")]
pub use driver::{PackageInstance, driver_table};
#[cfg(feature = "std")]
pub use host::{Loaded, PackageBinding, PackageHost, host_table};
pub use tables::*;

/// The one symbol a package exports: a [`PackageDescriptor`].
pub const PACKAGE_ENTRY: &CStr = c"lemnos_package";

/// The first field of a [`PackageDescriptor`]: the bytes `LEMNOSPK`.
pub const PACKAGE_MAGIC: u64 = u64::from_le_bytes(*b"LEMNOSPK");

/// Text behind the boundary: UTF-8, not NUL-terminated.
#[repr(C)]
pub struct Str<'a> {
    ptr: *const u8,
    len: usize,
    life: PhantomData<&'a str>,
}

impl<'a> Str<'a> {
    pub const EMPTY: Self = Self::new("");

    pub const fn new(text: &'a str) -> Self {
        Self {
            ptr: text.as_ptr(),
            len: text.len(),
            life: PhantomData,
        }
    }

    /// The text. Empty when the peer sent a null pointer or bytes that are not UTF-8.
    pub fn as_str(&self) -> &'a str {
        if self.ptr.is_null() {
            return "";
        }
        // SAFETY: a `Str` is only made by `new`, from a `&'a str`, here or in the peer; its
        // pointer and length are that string's.
        let bytes = unsafe { core::slice::from_raw_parts(self.ptr, self.len) };
        core::str::from_utf8(bytes).unwrap_or("")
    }
}

impl Clone for Str<'_> {
    fn clone(&self) -> Self {
        *self
    }
}
impl Copy for Str<'_> {}

impl core::fmt::Debug for Str<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(self.as_str(), f)
    }
}

// SAFETY: a `Str` is a `&str` in C layout, and `&str` is `Send`.
unsafe impl Send for Str<'_> {}
// SAFETY: a `Str` is a `&str` in C layout, and `&str` is `Sync`.
unsafe impl Sync for Str<'_> {}

/// Values behind the boundary that the receiver only reads.
#[repr(C)]
pub struct Slice<'a, T> {
    ptr: *const T,
    len: usize,
    life: PhantomData<&'a [T]>,
}

impl<'a, T> Slice<'a, T> {
    pub const EMPTY: Self = Self::new(&[]);

    pub const fn new(values: &'a [T]) -> Self {
        Self {
            ptr: values.as_ptr(),
            len: values.len(),
            life: PhantomData,
        }
    }

    /// The values. Empty when the peer sent a null or misaligned pointer.
    pub fn as_slice(&self) -> &'a [T] {
        if self.ptr.is_null() || !self.ptr.is_aligned() {
            return &[];
        }
        // SAFETY: a `Slice` is only made by `new`, from a `&'a [T]`, here or in the peer; its
        // pointer and length are that slice's.
        unsafe { core::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl<T> Clone for Slice<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Slice<'_, T> {}

impl<T: core::fmt::Debug> core::fmt::Debug for Slice<'_, T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(self.as_slice(), f)
    }
}

// SAFETY: a `Slice` is a `&[T]` in C layout, which is `Send` when `T` is `Sync`.
unsafe impl<T: Sync> Send for Slice<'_, T> {}
// SAFETY: a `Slice` is a `&[T]` in C layout, which is `Sync` when `T` is `Sync`.
unsafe impl<T: Sync> Sync for Slice<'_, T> {}

/// Room behind the boundary that the receiver fills.
#[repr(C)]
pub struct SliceMut<'a, T> {
    ptr: *mut T,
    len: usize,
    life: PhantomData<&'a mut [T]>,
}

impl<'a, T> SliceMut<'a, T> {
    pub const fn new(values: &'a mut [T]) -> Self {
        Self {
            len: values.len(),
            ptr: values.as_mut_ptr(),
            life: PhantomData,
        }
    }

    /// Room for nothing.
    pub const fn empty() -> Self {
        Self::new(&mut [])
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The room, for as long as this is borrowed. Empty when the peer sent a null or misaligned
    /// pointer.
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        if self.ptr.is_null() || !self.ptr.is_aligned() {
            return &mut [];
        }
        // SAFETY: a `SliceMut` is only made by `new`, from a `&'a mut [T]`, here or in the peer,
        // and it is not `Copy`: this borrow of it is the only way to that slice.
        unsafe { core::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    /// The room, for its whole lifetime.
    pub fn into_slice(self) -> &'a mut [T] {
        if self.ptr.is_null() || !self.ptr.is_aligned() {
            return &mut [];
        }
        // SAFETY: as in `as_mut_slice`; taking `self` ends every other way to the slice.
        unsafe { core::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl<T> core::fmt::Debug for SliceMut<'_, T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SliceMut").field("len", &self.len).finish()
    }
}

/// Where the receiver writes one value.
#[repr(transparent)]
pub struct Out<'a, T> {
    ptr: *mut T,
    life: PhantomData<&'a mut T>,
}

impl<'a, T> Out<'a, T> {
    pub const fn new(place: &'a mut T) -> Self {
        Self {
            ptr: place,
            life: PhantomData,
        }
    }

    /// Writes `value`. The value there before is dropped. A null or misaligned pointer from the
    /// peer drops `value` instead.
    pub fn set(self, value: T) {
        if self.ptr.is_null() || !self.ptr.is_aligned() {
            return;
        }
        // SAFETY: an `Out` is only made by `new`, from a `&'a mut T`, here or in the peer, and
        // taking `self` makes this the only use of it.
        let place = unsafe { &mut *self.ptr };
        *place = value;
    }
}

impl<T> core::fmt::Debug for Out<'_, T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Out")
    }
}

/// [`PackageNeed::kind`] for another device, named by the contract in [`PackageNeed::tag`]. Every
/// other value is a kind of bus, numbered as the `TAKE_*` values are ([`TAKE_I2C`] to
/// [`TAKE_STREAM`]).
pub const NEED_DEVICE: u32 = TAKE_DEVICE;

/// One thing the driver needs, by the name a board file gives it.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PackageNeed {
    pub name: Str<'static>,
    /// [`NEED_DEVICE`], or a kind of bus.
    pub kind: u32,
    /// Not 0: the driver binds without it.
    pub optional: u32,
    /// The contract a needed device provides, as `name/major`. Empty for a bus.
    pub tag: Str<'static>,
}

/// What a value looks like on the wire. `kind` is numbered by the driver model; `of` is the one
/// shape an option or an array holds, `count` an array's length and `fields` a record's fields.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PackageShape {
    pub kind: u32,
    pub count: u32,
    pub of: Slice<'static, PackageShape>,
    pub fields: Slice<'static, PackageField>,
}

/// A named shape: a record's field or a method's parameter.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PackageField {
    pub name: Str<'static>,
    pub shape: PackageShape,
}

/// One method of a contract.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PackageMethod {
    pub id: u16,
    /// Not 0: the method also fills a caller-supplied byte buffer.
    pub has_out_buffer: u16,
    pub reserved: u32,
    pub name: Str<'static>,
    pub params: Slice<'static, PackageField>,
    pub returns: PackageShape,
}

/// One contract the driver provides: its id and version as `name/major`, and its methods.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PackageContract {
    pub tag: Str<'static>,
    pub methods: Slice<'static, PackageMethod>,
}

/// What the driver needs and provides.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PackageManifest {
    pub name: Str<'static>,
    pub needs: Slice<'static, PackageNeed>,
    pub provides: Slice<'static, PackageContract>,
}

/// How a driver, or a device it uses, is doing.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PackageHealth {
    /// One of the `HEALTH_*` values. One the receiver does not know is a failure.
    pub kind: u32,
    pub reserved: u32,
    pub reason: Str<'static>,
}

pub const HEALTH_OK: u32 = 0;
pub const HEALTH_DEGRADED: u32 = 1;
pub const HEALTH_FAILED: u32 = 2;
/// What the driver was bound with is dead: the host drops it and binds it again.
pub const HEALTH_GONE: u32 = 3;

impl PackageHealth {
    pub const OK: Self = Self::new(HEALTH_OK, "");

    pub const fn new(kind: u32, reason: &'static str) -> Self {
        Self {
            kind,
            reserved: 0,
            reason: Str::new(reason),
        }
    }
}

/// One thing a device offers, tied to the method that does it. `kind` and `unit` are numbered by
/// the driver model.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PackageItem {
    pub kind: u8,
    pub unit: u8,
    pub exponent: i8,
    /// [`ITEM_HAS_MIN`], [`ITEM_HAS_MAX`], [`ITEM_HAS_FIELD`].
    pub flags: u8,
    /// Position of the method's contract in the manifest.
    pub contract: u8,
    pub reserved: u8,
    pub method: u16,
    pub min: i64,
    pub max: i64,
    pub name: Str<'static>,
    pub field: Str<'static>,
}

pub const ITEM_HAS_MIN: u8 = 1;
pub const ITEM_HAS_MAX: u8 = 2;
pub const ITEM_HAS_FIELD: u8 = 4;

impl PackageItem {
    pub const EMPTY: Self = Self {
        kind: 0,
        unit: 0,
        exponent: 0,
        flags: 0,
        contract: 0,
        reserved: 0,
        method: 0,
        min: 0,
        max: 0,
        name: Str::EMPTY,
        field: Str::EMPTY,
    };
}

/// One setting from the device's board entry, lent for the bind.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PackageSetting<'a> {
    pub key: Str<'a>,
    /// One of the `SETTING_*` values, saying which of the fields below holds the value.
    pub kind: u32,
    pub reserved: u32,
    /// The integer, or 0 or 1 for a boolean.
    pub int: i64,
    pub float: f64,
    pub text: Str<'a>,
}

pub const SETTING_INT: u32 = 0;
pub const SETTING_FLOAT: u32 = 1;
pub const SETTING_BOOL: u32 = 2;
pub const SETTING_TEXT: u32 = 3;

/// [`DriverTable::flags`]: the host should call `tick`.
pub const DRIVER_TICKS: u32 = 1;
/// [`DriverTable::flags`]: the host should call `caller` and `caller_gone`.
pub const DRIVER_TRACKS_CALLERS: u32 = 2;

/// [`DriverTable::tick`]'s answer when the driver wants no tick.
pub const NO_TICK: u64 = u64::MAX;

/// What the host calls on a package's driver. `driver` is what `bind` returned, and belongs to
/// the package. The host calls one function at a time on one driver, never after `drop`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DriverTable {
    pub struct_size: u32,
    /// [`DRIVER_TICKS`], [`DRIVER_TRACKS_CALLERS`].
    pub flags: u32,
    /// Builds the driver. `binding` is the host's, valid for this call only, and is what the
    /// driver passes to [`HostTable::provided`] and [`HostTable::take`]. `link` counts only when
    /// `has_link` is not 0.
    pub bind: extern "C" fn(
        host: &HostTable,
        binding: *mut c_void,
        settings: Slice<'_, PackageSetting<'_>>,
        link: Str<'_>,
        has_link: u32,
        driver: Out<'_, *mut c_void>,
    ) -> Status,
    /// Fills `items` with what the device offers and writes how many it has in all to `count`:
    /// more than `items` holds means some were left out.
    pub describe: extern "C" fn(
        driver: *mut c_void,
        items: SliceMut<'_, PackageItem>,
        count: Out<'_, u32>,
    ) -> Status,
    pub health: extern "C" fn(driver: *mut c_void, health: Out<'_, PackageHealth>) -> Status,
    /// Runs a method. `contract` is its position in the manifest. The reply's length goes to
    /// `len` whatever the status.
    pub dispatch: extern "C" fn(
        driver: *mut c_void,
        contract: u8,
        method: u16,
        args: Slice<'_, u8>,
        reply: SliceMut<'_, u8>,
        len: Out<'_, usize>,
    ) -> Status,
    /// Does what is due at `now_ns` and writes when to be called next, or [`NO_TICK`].
    pub tick: extern "C" fn(driver: *mut c_void, now_ns: u64, next_ns: Out<'_, u64>) -> Status,
    /// Says who the method about to run is for.
    pub caller: extern "C" fn(driver: *mut c_void, caller: u64) -> Status,
    pub caller_gone: extern "C" fn(driver: *mut c_void, caller: u64) -> Status,
    /// Lets go of the driver and everything it holds.
    pub drop: extern "C" fn(driver: *mut c_void) -> Status,
}

/// What a package's driver calls on the host. The table lives as long as the process.
///
/// A [`Handle`] from `take` is the driver's until it calls `release`, or until the host lets go
/// of the driver, which releases whatever it still holds. What is done with a handle is in the
/// table of its kind; a handle of another kind, or one released, is refused there with
/// [`Status::STALE_HANDLE`] or [`Status::INVALID_ARGUMENT`].
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct HostTable {
    pub struct_size: u32,
    pub reserved: u32,
    /// Whether the board file supplies `need`. Not 0 for yes. Only during `bind`.
    pub provided: extern "C" fn(binding: *mut c_void, need: Str<'_>) -> u32,
    /// Takes what the board file names for `need`: `kind` is one of the `TAKE_*` values, and
    /// `tag` the contract of a device. Only during `bind`. [`Status::NOT_SUPPORTED`] for a kind
    /// this host does not have.
    pub take: extern "C" fn(
        binding: *mut c_void,
        kind: u32,
        need: Str<'_>,
        tag: Str<'_>,
        resource: Out<'_, Handle>,
    ) -> Status,
    pub release: extern "C" fn(resource: Handle) -> Status,
    pub device: DeviceTable,
    pub i2c: I2cTable,
    pub spi: SpiTable,
    pub line: LineTable,
    pub pwm: PwmTable,
    pub uart: UartTable,
    pub registers: RegistersTable,
    pub interrupt: InterruptTable,
    pub dma: DmaTable,
    pub clocks: ClocksTable,
    pub reset: ResetTable,
    pub stream: StreamTable,
    pub usb_device: UsbDeviceTable,
    pub usb_function: UsbFunctionTable,
}

/// What `register` hands the host: one driver.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PackageDriver {
    pub manifest: PackageManifest,
    pub table: DriverTable,
}

/// What a package's [`PACKAGE_ENTRY`] symbol is. The host reads `magic` and `version` and runs
/// nothing of the package's unless both are right.
#[repr(C)]
#[derive(Debug)]
pub struct PackageDescriptor {
    /// [`PACKAGE_MAGIC`].
    pub magic: u64,
    /// The [`PACKAGE_VERSION`] the package was built against.
    pub version: AbiVersion,
    /// Called once. Writes the package's driver, which stays valid while the package is loaded.
    pub register: extern "C" fn(driver: Out<'_, Slice<'static, PackageDriver>>) -> Status,
}

/// The size, at the start of a [`PackageDescriptor`], of the fields that say whether it is one:
/// `magic` and `version`.
pub const DESCRIPTOR_HEAD: usize = size_of::<u64>() + size_of::<AbiVersion>();

impl PackageDescriptor {
    pub const fn new(
        register: extern "C" fn(driver: Out<'_, Slice<'static, PackageDriver>>) -> Status,
    ) -> Self {
        Self {
            magic: PACKAGE_MAGIC,
            version: PACKAGE_VERSION,
            register,
        }
    }
}

use tables::shape;

/// The layout hash of the package boundary: every type here and in [`tables`], with this
/// crate's own [`LAYOUT_HASH`](crate::LAYOUT_HASH). The types hold pointers, so the hash follows
/// the target's pointer width, which a package and its host share.
pub const PACKAGE_LAYOUT_HASH: u64 = {
    let parts: [&[u64]; 19] = [
        &[crate::LAYOUT_HASH],
        shape!(Str<'static>, ptr, len),
        shape!(Slice<'static, u8>, ptr, len),
        shape!(SliceMut<'static, u8>, ptr, len),
        shape!(Out<'static, u8>, ptr),
        shape!(PackageNeed, name, kind, optional, tag),
        shape!(PackageShape, kind, count, of, fields),
        shape!(PackageField, name, shape),
        shape!(
            PackageMethod,
            id,
            has_out_buffer,
            reserved,
            name,
            params,
            returns
        ),
        shape!(PackageContract, tag, methods),
        shape!(PackageManifest, name, needs, provides),
        shape!(PackageHealth, kind, reserved, reason),
        shape!(
            PackageItem,
            kind,
            unit,
            exponent,
            flags,
            contract,
            reserved,
            method,
            min,
            max,
            name,
            field
        ),
        shape!(
            PackageSetting<'static>,
            key,
            kind,
            reserved,
            int,
            float,
            text
        ),
        shape!(
            DriverTable,
            struct_size,
            flags,
            bind,
            describe,
            health,
            dispatch,
            tick,
            caller,
            caller_gone,
            drop
        ),
        shape!(
            HostTable,
            struct_size,
            reserved,
            provided,
            take,
            release,
            device,
            i2c,
            spi,
            line,
            pwm,
            uart,
            registers,
            interrupt,
            dma,
            clocks,
            reset,
            stream,
            usb_device,
            usb_function
        ),
        shape!(PackageDriver, manifest, table),
        shape!(PackageDescriptor, magic, version, register),
        &[MAX_TRANSFER_STEPS as u64, MAX_ISO_PACKETS as u64],
    ];
    let groups: [&[&[u64]]; 3] = [&parts, &tables::VALUE_SHAPES, &tables::TABLE_SHAPES];
    let mut words = [0u64; 512];
    let (mut n, mut g) = (0, 0);
    while g < groups.len() {
        let mut p = 0;
        while p < groups[g].len() {
            let mut i = 0;
            while i < groups[g][p].len() {
                words[n] = groups[g][p][i];
                n += 1;
                i += 1;
            }
            p += 1;
        }
        g += 1;
    }
    layout_hash(&words)
};

/// The version of the package boundary. A host loads a package whose major is this one, whose
/// minor is no newer, and whose layout hash is the same.
pub const PACKAGE_VERSION: AbiVersion = AbiVersion::new(2, 0, PACKAGE_LAYOUT_HASH);

#[cfg(all(test, feature = "std"))]
mod header;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_a_broken_peer_sends_reads_as_nothing() {
        // A package not built from this crate could send these. They must not be read.
        let null = Str {
            ptr: core::ptr::null(),
            len: 9,
            life: PhantomData,
        };
        assert_eq!(null.as_str(), "");
        let not_text = [0xffu8, 0xfe];
        let bytes = Slice::new(&not_text);
        let not_utf8 = Str {
            ptr: bytes.as_slice().as_ptr(),
            len: 2,
            life: PhantomData,
        };
        assert_eq!(not_utf8.as_str(), "");
        let words = [0u64; 2];
        let odd = Slice::<u64> {
            ptr: words.as_ptr().cast::<u8>().wrapping_add(1).cast(),
            len: 1,
            life: PhantomData,
        };
        assert!(odd.as_slice().is_empty());
        let nowhere = Out::<u32> {
            ptr: core::ptr::null_mut(),
            life: PhantomData,
        };
        nowhere.set(3);
    }
}
