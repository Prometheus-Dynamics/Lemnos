//! A driver package written against `include/lemnos/package.h` with no Lemnos crate in it: the
//! header's types mirrored by hand and checked against the layout the C compiler gives them, and
//! the functions the boundary asks for. The loader's tests build it with `rustc` as a `cdylib`
//! (`common::raw_package`), one build per set of cfgs, and load each file through the real loader.
//!
//! The cfgs, one per variant:
//!   fail_registration  register_driver fails with -12
//!   no_driver          register_driver hands over no driver
//!   no_name            the driver has no name
//!   bad_tag            a contract tag that is not name/major
//!   bad_need           a need of a kind no host knows
//!   future_layout      a layout hash no host knows
//!   with_extras        describes one item (`level`) with its limits, and, unless `pre_extras`,
//!                      the manifest's kind and method reading through `describe_extra`
//!   pre_extras         built against the boundary before the extras: the base layout hash
//!                      (`PACKAGE_BASE_LAYOUT_HASH`) and no `describe_extra`
//!   with_resume        the resume entries (`resume_record`, `resume`): the boundary as it is now,
//!                      with the driver's counter kept across a rebind
//!   needs_gone         links `libgone.so`, which the test removes before the file is loaded
//!
//! Without `with_resume` the file is built against the boundary before the resume entries (the
//! extras layout, `PACKAGE_EXTRAS_LAYOUT_HASH`): an old-shape package, whose driver keeps nothing.
//! Its methods `leave` (3) and `bump` (4) make a driver Gone, and count, in every variant.
//!
//! Nothing here may unwind: every function is `extern "C"` and returns a status.
//!
//! The mirror is by hand, so the sizes and offsets below are pinned: they are the numbers
//! `cc` gives the header's types on a 64-bit target, and the const assertions fail the build if
//! a field moves.

// This is the ABI's own conformance fixture: a package written against the boundary, built by
// rustc (not cargo) for the loader's tests in engine/package. It needs `unsafe` for the same reason
// a package does, so it lives under engine/abi, where the boundary's unsafe code is reviewed.
#![allow(unsafe_code, dead_code, non_upper_case_globals, clippy::all)]
#![allow(unexpected_cfgs)]

use core::ffi::c_void;
use core::mem::{offset_of, size_of};

// ---- The header's types (include/lemnos/abi.h and package.h) ----

type Status = i32;
type Handle = u64;

const OK: Status = 0;
const E_VERSION_MISMATCH: Status = -4096 - 1;
const E_BUFFER_TOO_SMALL: Status = -4096 - 2;

#[repr(C)]
#[derive(Clone, Copy)]
struct AbiVersion {
    struct_size: u32,
    major: u16,
    minor: u16,
    layout: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Str {
    ptr: *const u8,
    len: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Slice {
    ptr: *const c_void,
    len: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SliceMut {
    ptr: *mut c_void,
    len: usize,
}

const fn text(bytes: &'static [u8]) -> Str {
    Str {
        ptr: bytes.as_ptr(),
        len: bytes.len(),
    }
}

const NOTHING: Str = Str {
    ptr: core::ptr::null(),
    len: 0,
};

const NO_SLICE: Slice = Slice {
    ptr: core::ptr::null(),
    len: 0,
};

#[repr(C)]
#[derive(Clone, Copy)]
struct Need {
    name: Str,
    kind: u32,
    optional: u32,
    tag: Str,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Shape {
    kind: u32,
    count: u32,
    of: Slice,
    fields: Slice,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Field {
    name: Str,
    shape: Shape,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Method {
    id: u16,
    has_out_buffer: u16,
    flags: u32,
    name: Str,
    params: Slice,
    returns: Shape,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Contract {
    tag: Str,
    methods: Slice,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Manifest {
    name: Str,
    needs: Slice,
    provides: Slice,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Health {
    kind: u32,
    reserved: u32,
    reason: Str,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Item {
    kind: u8,
    unit: u8,
    exponent: i8,
    flags: u8,
    contract: u8,
    reserved: u8,
    method: u16,
    min: i64,
    max: i64,
    name: Str,
    field: Str,
}

#[cfg(all(with_extras, not(pre_extras)))]
#[repr(C)]
#[derive(Clone, Copy)]
struct Extra {
    what: u8,
    reading: u8,
    flags: u8,
    contract: u8,
    method: u16,
    reserved: u16,
    index: u32,
    step: i64,
    warn_low: i64,
    warn_high: i64,
    text: Str,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct LineConfig {
    direction: u8,
    initial: u8,
    active_low: u8,
    bias: u8,
    drive: u8,
    edge: u8,
    reserved: u16,
    debounce_us: u32,
}

/// The host's table, up to the line table: the fields this package calls. The rest of the table
/// (device, i2c, spi, pwm, and so on) is not named here, but `struct_size` is the whole table's.
#[repr(C)]
struct HostTable {
    struct_size: u32,
    reserved: u32,
    provided: extern "C" fn(*mut c_void, Str) -> u32,
    take: extern "C" fn(*mut c_void, u32, Str, Str, *mut Handle) -> Status,
    release: extern "C" fn(Handle) -> Status,
    device: [usize; 2],
    i2c: [usize; 1],
    spi: [usize; 2],
    line: LineTable,
    rest: [u8; HOST_TABLE_BYTES - 104],
}

#[repr(C)]
struct LineTable {
    configure: extern "C" fn(Handle, LineConfig) -> Status,
    get: extern "C" fn(Handle, *mut u32) -> Status,
    set: extern "C" fn(Handle, u32) -> Status,
    read_edge: extern "C" fn(Handle, *mut c_void) -> Status,
}

/// What the host's `struct_size` is for the whole table (the C compiler's `sizeof`).
const HOST_TABLE_BYTES: usize = 592;

#[repr(C)]
#[derive(Clone, Copy)]
struct Setting {
    key: Str,
    kind: u32,
    reserved: u32,
    int_value: i64,
    float_value: f64,
    text: Str,
}

use core::cell::Cell;

#[repr(C)]
#[derive(Clone, Copy)]
struct DriverTable {
    struct_size: u32,
    flags: u32,
    bind: extern "C" fn(*const HostTable, *mut c_void, Slice, Str, u32, *mut *mut c_void) -> Status,
    describe: extern "C" fn(*mut c_void, SliceMut, *mut u32) -> Status,
    health: extern "C" fn(*mut c_void, *mut Health) -> Status,
    dispatch: extern "C" fn(*mut c_void, u8, u16, Slice, SliceMut, *mut usize) -> Status,
    tick: extern "C" fn(*mut c_void, u64, *mut u64) -> Status,
    caller: extern "C" fn(*mut c_void, u64) -> Status,
    caller_gone: extern "C" fn(*mut c_void, u64) -> Status,
    drop: extern "C" fn(*mut c_void) -> Status,
    job: Option<extern "C" fn(*mut c_void, u64, SliceMut, *mut usize, *mut c_void) -> Status>,
    job_cancel: Option<extern "C" fn(*mut c_void, u64) -> Status>,
    #[cfg(not(pre_extras))]
    describe_extra: Option<ExtraFn>,
    #[cfg(with_resume)]
    resume_record: Option<ResumeRecordFn>,
    #[cfg(with_resume)]
    resume: Option<ResumeFn>,
}

type ExtraFn = extern "C" fn(*mut c_void, SliceMut, *mut u32) -> Status;
#[cfg(with_resume)]
type ResumeRecordFn = extern "C" fn(*mut c_void, SliceMut, *mut usize) -> Status;
#[cfg(with_resume)]
type ResumeFn = extern "C" fn(*mut c_void, Slice, *mut u32) -> Status;

#[repr(C)]
struct PackageDriver {
    manifest: Manifest,
    table: DriverTable,
}

#[repr(C)]
pub struct Descriptor {
    magic: u64,
    version: AbiVersion,
    register_driver: extern "C" fn(*mut Slice) -> Status,
}

/// A `static` must be `Sync`; these hold raw pointers into other statics and are only read.
#[repr(transparent)]
pub struct Shared<T>(pub T);
unsafe impl<T> Sync for Shared<T> {}

// ---- The pinned layout (cc on include/lemnos/package.h, 64-bit) ----

const _: () = {
    assert!(size_of::<AbiVersion>() == 16);
    assert!(size_of::<Str>() == 16);
    assert!(size_of::<Slice>() == 16);
    assert!(size_of::<Need>() == 40);
    assert!(size_of::<Shape>() == 40);
    assert!(size_of::<Field>() == 56);
    assert!(size_of::<Method>() == 80);
    assert!(size_of::<Contract>() == 32);
    assert!(size_of::<Manifest>() == 48);
    assert!(size_of::<Health>() == 24);
    assert!(size_of::<Item>() == 56);
    assert!(size_of::<Setting>() == 56);
    assert!(size_of::<LineConfig>() == 12);
    assert!(size_of::<HostTable>() == HOST_TABLE_BYTES);
    assert!(offset_of!(HostTable, line) == 72);
    assert!(offset_of!(HostTable, struct_size) == 0);
    assert!(offset_of!(Need, tag) == 24);
    assert!(offset_of!(Method, params) == 24);
    assert!(offset_of!(Method, returns) == 40);
    assert!(offset_of!(Item, min) == 8);
    assert!(offset_of!(Item, name) == 24);
    assert!(offset_of!(Item, field) == 40);
    assert!(offset_of!(Setting, int_value) == 24);
    assert!(offset_of!(LineConfig, debounce_us) == 8);
    assert!(offset_of!(DriverTable, bind) == 8);
    assert!(offset_of!(DriverTable, tick) == 40);
    #[cfg(not(pre_extras))]
    assert!(offset_of!(DriverTable, describe_extra) == 88);
    assert!(offset_of!(PackageDriver, table) == 48);
    assert!(offset_of!(Descriptor, register_driver) == 24);
};

#[cfg(with_resume)]
const _: () = {
    assert!(size_of::<DriverTable>() == 112);
    assert!(offset_of!(DriverTable, resume_record) == 96);
    assert!(offset_of!(DriverTable, resume) == 104);
};

#[cfg(all(with_extras, not(pre_extras)))]
const _: () = {
    assert!(size_of::<Extra>() == 56);
    assert!(offset_of!(Extra, index) == 8);
    assert!(offset_of!(Extra, text) == 40);
};

// ---- The package's constants ----

const MAJOR: u16 = 2;
const MINOR: u16 = 0;
#[cfg(all(not(pre_extras), not(with_resume), not(future_layout)))]
const LAYOUT: u64 = 0x432460040a3bbd5e;
#[cfg(all(with_resume, not(future_layout)))]
const LAYOUT: u64 = 0x35459eaa3ecfa7e6;
#[cfg(all(pre_extras, not(future_layout)))]
const LAYOUT: u64 = 0x57cde501ca6c0a8c;
#[cfg(future_layout)]
const LAYOUT: u64 = 0x1234;

const MAGIC: u64 = 0x4b50534f4e4d454c;

const TAKE_LINE: u32 = 3;
const LINE_OUTPUT: u8 = 1;
const KIND_UNKNOWN: u32 = 99;

const CONTRACT_PLAIN: u8 = 0;
const SETTING_INT: u32 = 0;
const ITEM_HAS_MIN: u8 = 1;
const ITEM_HAS_MAX: u8 = 2;
#[cfg(all(with_extras, not(pre_extras)))]
const EXTRA_CONTRACT: u8 = 0;
#[cfg(all(with_extras, not(pre_extras)))]
const EXTRA_METHOD: u8 = 1;
#[cfg(all(with_extras, not(pre_extras)))]
const EXTRA_ITEM: u8 = 2;
#[cfg(all(with_extras, not(pre_extras)))]
const EXTRA_HAS_READING: u8 = 1;
#[cfg(all(with_extras, not(pre_extras)))]
const EXTRA_HAS_STEP: u8 = 2;
#[cfg(all(with_extras, not(pre_extras)))]
const EXTRA_HAS_WARN_LOW: u8 = 4;
#[cfg(all(with_extras, not(pre_extras)))]
const EXTRA_HAS_WARN_HIGH: u8 = 8;
#[cfg(all(with_extras, not(pre_extras)))]
const EXTRA_HAS_GROUP: u8 = 16;
#[cfg(all(with_extras, not(pre_extras)))]
const EXTRA_HAS_UNIT_STATE: u8 = 32;
#[cfg(all(with_extras, not(pre_extras)))]
const EXTRA_UNIT_DIMENSIONLESS: u8 = 64;

// ---- The driver ----

/// What `bind` makes and `drop` takes back.
struct Driver {
    host: *const HostTable,
    line: Handle,
    has_line: bool,
    base: u32,
    /// What `bump` counts, kept across a rebind when the package has the resume entries.
    value: Cell<u32>,
    /// Set by `leave`: the driver says what it was bound with is gone.
    gone: Cell<bool>,
}

#[cfg(no_name)]
const NAME: Str = NOTHING;
#[cfg(not(no_name))]
const NAME: Str = text(b"raw-package");

const LINE: &[u8] = b"line";
const BASE: &[u8] = b"base";

extern "C" fn bind(
    host: *const HostTable,
    binding: *mut c_void,
    settings: Slice,
    _link: Str,
    _has_link: u32,
    driver: *mut *mut c_void,
) -> Status {
    // SAFETY: the host passes its table, valid for the call, and a settings slice of
    // LemnosPackageSetting it says has `len` elements.
    let host_ref = unsafe { &*host };
    if (host_ref.struct_size as usize) < HOST_TABLE_BYTES {
        return E_VERSION_MISMATCH;
    }
    let mut base = 0;
    let found = settings.ptr as *const Setting;
    for at in 0..settings.len {
        // SAFETY: `at` is below the settings' length, so the element is there.
        let setting = unsafe { *found.add(at) };
        // SAFETY: the key is text of `len` bytes that the host keeps for the call.
        let key = unsafe { core::slice::from_raw_parts(setting.key.ptr, setting.key.len) };
        if key == BASE && setting.kind == SETTING_INT {
            base = setting.int_value as u32;
        }
    }
    let need = text(LINE);
    let no_tag = NOTHING;
    let has_line = (host_ref.provided)(binding, need) != 0;
    let mut line = 0;
    if has_line {
        let status = (host_ref.take)(binding, TAKE_LINE, need, no_tag, &mut line);
        if status != OK {
            return status;
        }
        // An output, starting low.
        let config = LineConfig {
            direction: LINE_OUTPUT,
            initial: 0,
            active_low: 0,
            bias: 0,
            drive: 0,
            edge: 0,
            reserved: 0,
            debounce_us: 0,
        };
        let status = (host_ref.line.configure)(line, config);
        if status != OK {
            (host_ref.release)(line);
            return status;
        }
    }
    let made = Box::new(Driver {
        host,
        line,
        has_line,
        base,
        value: Cell::new(0),
        gone: Cell::new(false),
    });
    // SAFETY: `driver` is the host's out pointer, written once.
    unsafe { *driver = Box::into_raw(made) as *mut c_void };
    OK
}

extern "C" fn describe(driver: *mut c_void, items: SliceMut, count: *mut u32) -> Status {
    #[cfg(with_extras)]
    {
        if !driver.is_null() && items.len >= 1 {
            let level = Item {
                kind: 0,
                unit: 0,
                exponent: 0,
                flags: ITEM_HAS_MIN | ITEM_HAS_MAX,
                contract: 0,
                reserved: 0,
                method: 1,
                min: 0,
                max: 100,
                name: text(b"level"),
                field: NOTHING,
            };
            // SAFETY: `items` is writable for its length, and the length is at least one.
            unsafe { *(items.ptr as *mut Item) = level };
        }
        // SAFETY: `count` is the host's out pointer.
        unsafe { *count = if driver.is_null() { 0 } else { 1 } };
    }
    #[cfg(not(with_extras))]
    {
        let _ = (driver, items);
        // SAFETY: `count` is the host's out pointer.
        unsafe { *count = 0 };
    }
    OK
}

#[cfg(all(with_extras, not(pre_extras)))]
extern "C" fn describe_extra(driver: *mut c_void, extras: SliceMut, count: *mut u32) -> Status {
    let out = extras.ptr as *mut Extra;
    if !driver.is_null() {
        if extras.len >= 1 {
            let item = Extra {
                what: EXTRA_ITEM,
                reading: 1,
                flags: EXTRA_HAS_READING
                    | EXTRA_HAS_STEP
                    | EXTRA_HAS_WARN_LOW
                    | EXTRA_HAS_WARN_HIGH
                    | EXTRA_HAS_GROUP
                    | EXTRA_HAS_UNIT_STATE
                    | EXTRA_UNIT_DIMENSIONLESS,
                contract: 0,
                method: 0,
                reserved: 0,
                index: 0,
                step: 5,
                warn_low: 10,
                warn_high: 90,
                text: text(b"grp"),
            };
            // SAFETY: `extras` is writable for its length, and the length is at least one.
            unsafe { *out = item };
        }
        // SAFETY: `count` is the host's out pointer.
        unsafe { *count = 1 };
        return OK;
    }
    if extras.len >= 2 {
        let kind = Extra {
            what: EXTRA_CONTRACT,
            reading: 0,
            flags: EXTRA_HAS_GROUP,
            contract: 0,
            method: 0,
            reserved: 0,
            index: 0,
            step: 0,
            warn_low: 0,
            warn_high: 0,
            text: text(b"sensor"),
        };
        let reading = Extra {
            what: EXTRA_METHOD,
            reading: 2,
            flags: EXTRA_HAS_READING,
            contract: 0,
            method: 1,
            reserved: 0,
            index: 0,
            step: 0,
            warn_low: 0,
            warn_high: 0,
            text: NOTHING,
        };
        // SAFETY: `extras` holds two elements, as its length says; this is the first.
        unsafe { *out = kind };
        // SAFETY: the second element is there too, as the same length says.
        unsafe { *out.add(1) = reading };
    }
    // SAFETY: `count` is the host's out pointer.
    unsafe { *count = 2 };
    OK
}

extern "C" fn health(driver: *mut c_void, out: *mut Health) -> Status {
    // SAFETY: the host calls one function at a time on this driver, and never after drop.
    let made = unsafe { &*(driver as *const Driver) };
    // SAFETY: `out` is the host's out pointer.
    unsafe {
        *out = Health {
            kind: if made.gone.get() { 3 } else { 0 },
            reserved: 0,
            reason: NOTHING,
        }
    };
    OK
}

/// Writes the counter into the host's `record`: the record the driver keeps across a rebind.
#[cfg(with_resume)]
extern "C" fn resume_record_fn(driver: *mut c_void, record: SliceMut, len: *mut usize) -> Status {
    // SAFETY: the host calls one function at a time on this driver, and never after drop.
    let made = unsafe { &*(driver as *const Driver) };
    if record.len < 4 {
        return E_BUFFER_TOO_SMALL;
    }
    let bytes = made.value.get().to_le_bytes();
    // SAFETY: `record` is writable for four bytes, checked above; `len` is the host's out pointer.
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), record.ptr as *mut u8, 4);
        *len = 4;
    }
    OK
}

/// Restores the counter from the record, on the driver the host bound in place of the old one.
#[cfg(with_resume)]
extern "C" fn resume_fn(driver: *mut c_void, record: Slice, kept: *mut u32) -> Status {
    // SAFETY: the host calls one function at a time on this driver, and never after drop.
    let made = unsafe { &*(driver as *const Driver) };
    if record.len != 4 {
        // SAFETY: `kept` is the host's out pointer.
        unsafe { *kept = 0 };
        return OK;
    }
    let mut bytes = [0u8; 4];
    // SAFETY: the record is four bytes, as its length says.
    unsafe { core::ptr::copy_nonoverlapping(record.ptr as *const u8, bytes.as_mut_ptr(), 4) };
    made.value.set(u32::from_le_bytes(bytes));
    // SAFETY: `kept` is the host's out pointer.
    unsafe { *kept = 1 };
    OK
}

extern "C" fn dispatch(
    driver: *mut c_void,
    contract: u8,
    method: u16,
    args: Slice,
    reply: SliceMut,
    len: *mut usize,
) -> Status {
    // SAFETY: `len` is the host's out pointer.
    unsafe { *len = 0 };
    if contract != CONTRACT_PLAIN {
        return -22;
    }
    // SAFETY: the host calls one function at a time on this driver, and never after drop.
    let made = unsafe { &*(driver as *const Driver) };
    match method {
        1 => {
            // level() -> u32: the line's value (0 with no line), plus the `base` setting.
            let mut value = 0;
            if made.has_line {
                // SAFETY: the host's line table, valid while the package is loaded.
                let host = unsafe { &*made.host };
                let status = (host.line.get)(made.line, &mut value);
                if status != OK {
                    return status;
                }
            }
            if reply.len < 4 {
                return E_BUFFER_TOO_SMALL;
            }
            let level = (made.base + value).to_le_bytes();
            // SAFETY: `reply` is writable for four bytes, checked above, and `level` has four.
            unsafe { core::ptr::copy_nonoverlapping(level.as_ptr(), reply.ptr as *mut u8, 4) };
            // SAFETY: `len` is the host's out pointer.
            unsafe { *len = 4 };
            OK
        }
        2 => {
            // set(value: bool)
            if args.len < 1 {
                return -22;
            }
            if !made.has_line {
                return -19;
            }
            // SAFETY: the argument is one byte, as its length says.
            let value = unsafe { *(args.ptr as *const u8) };
            // SAFETY: the host's line table, valid while the package is loaded.
            let host = unsafe { &*made.host };
            (host.line.set)(made.line, value as u32)
        }
        3 => {
            // leave(): from now on this driver says what it was bound with is gone.
            made.gone.set(true);
            OK
        }
        4 => {
            // bump() -> u32: counts up, and answers the count.
            made.value.set(made.value.get() + 1);
            if reply.len < 4 {
                return E_BUFFER_TOO_SMALL;
            }
            let bytes = made.value.get().to_le_bytes();
            // SAFETY: `reply` is writable for four bytes, checked above; `len` is the host's.
            unsafe {
                core::ptr::copy_nonoverlapping(bytes.as_ptr(), reply.ptr as *mut u8, 4);
                *len = 4;
            }
            OK
        }
        _ => -95,
    }
}

extern "C" fn tick(_driver: *mut c_void, _now_ns: u64, next_ns: *mut u64) -> Status {
    // SAFETY: `next_ns` is the host's out pointer.
    unsafe { *next_ns = u64::MAX };
    OK
}

extern "C" fn caller(_driver: *mut c_void, _who: u64) -> Status {
    OK
}

extern "C" fn drop_driver(driver: *mut c_void) -> Status {
    if !driver.is_null() {
        // SAFETY: `driver` is what bind made, and the host calls drop once.
        let made = unsafe { Box::from_raw(driver as *mut Driver) };
        if made.has_line {
            // SAFETY: the host's release table, valid while the package is loaded.
            let host = unsafe { &*made.host };
            (host.release)(made.line);
        }
    }
    OK
}

// ---- The manifest ----

#[cfg(bad_need)]
const NEED_KIND: u32 = KIND_UNKNOWN;
#[cfg(not(bad_need))]
const NEED_KIND: u32 = TAKE_LINE;

static NEEDS: Shared<[Need; 1]> = Shared([Need {
    name: text(LINE),
    kind: NEED_KIND,
    optional: 1,
    tag: NOTHING,
}]);

const PLAIN: Shape = Shape {
    kind: 0,
    count: 0,
    of: NO_SLICE,
    fields: NO_SLICE,
};

const fn plain(kind: u32) -> Shape {
    Shape {
        kind,
        count: 0,
        of: NO_SLICE,
        fields: NO_SLICE,
    }
}

static SET_PARAMS: Shared<[Field; 1]> = Shared([Field {
    name: text(b"value"),
    shape: plain(1),
}]);

static METHODS: Shared<[Method; 2]> = Shared([
    Method {
        id: 1,
        has_out_buffer: 0,
        flags: 0,
        name: text(b"level"),
        params: NO_SLICE,
        returns: plain(4),
    },
    Method {
        id: 2,
        has_out_buffer: 0,
        flags: 0,
        name: text(b"set"),
        params: Slice {
            ptr: &SET_PARAMS as *const _ as *const c_void,
            len: 1,
        },
        returns: PLAIN,
    },
]);

#[cfg(bad_tag)]
const TAG: &[u8] = b"example.raw";
#[cfg(not(bad_tag))]
const TAG: &[u8] = b"example.raw/1";

static CONTRACTS: Shared<[Contract; 1]> = Shared([Contract {
    tag: text(TAG),
    methods: Slice {
        ptr: &METHODS as *const _ as *const c_void,
        len: 2,
    },
}]);

static MANIFEST: Shared<Manifest> = Shared(Manifest {
    name: NAME,
    needs: Slice {
        ptr: &NEEDS as *const _ as *const c_void,
        len: 1,
    },
    provides: Slice {
        ptr: &CONTRACTS as *const _ as *const c_void,
        len: 1,
    },
});

/// The package's `describe_extra`, from the extras on: none with no extras to give.
#[cfg(all(with_extras, not(pre_extras)))]
const EXTRA_TABLE: Option<ExtraFn> = Some(describe_extra);
#[cfg(not(all(with_extras, not(pre_extras))))]
const EXTRA_TABLE: Option<ExtraFn> = None;

static DRIVER: Shared<PackageDriver> = Shared(PackageDriver {
    manifest: MANIFEST.0,
    table: DriverTable {
        struct_size: size_of::<DriverTable>() as u32,
        flags: 0,
        bind,
        describe,
        health,
        dispatch,
        tick,
        caller,
        caller_gone: caller,
        drop: drop_driver,
        job: None,
        job_cancel: None,
        #[cfg(not(pre_extras))]
        describe_extra: EXTRA_TABLE,
        #[cfg(with_resume)]
        resume_record: Some(resume_record_fn),
        #[cfg(with_resume)]
        resume: Some(resume_fn),
    },
});

extern "C" fn register_driver(driver: *mut Slice) -> Status {
    let len = if cfg!(no_driver) { 0 } else { 1 };
    // SAFETY: `driver` is the host's out slice, and is written whatever the outcome.
    unsafe {
        *driver = Slice {
            ptr: &DRIVER as *const _ as *const c_void,
            len,
        };
    }
    // What it wrote counts for nothing once it says it failed.
    if cfg!(fail_registration) { -12 } else { OK }
}

/// The one symbol a package exports: the bytes "LEMNOSPK", the version it was built against,
/// and the registration.
#[unsafe(no_mangle)]
pub static lemnos_package: Shared<Descriptor> = Shared(Descriptor {
    magic: MAGIC,
    version: AbiVersion {
        struct_size: size_of::<AbiVersion>() as u32,
        major: MAJOR,
        minor: MINOR,
        layout: LAYOUT,
    },
    register_driver,
});

// The version handshake's struct size, and the entry name the host looks up, are the header's.
const _: () = assert!(size_of::<Descriptor>() == 32);

#[cfg(needs_gone)]
#[link(name = "gone")]
unsafe extern "C" {
    fn lemnos_test_gone() -> i32;
}

/// Calls the library the test removed: only to make the file need it, so the dynamic loader
/// refuses the package.
#[cfg(needs_gone)]
#[unsafe(no_mangle)]
pub extern "C" fn lemnos_raw_package_needs_gone() -> i32 {
    // SAFETY: the symbol is in the library this package links; the loader refuses the file
    // before this is ever called.
    unsafe { lemnos_test_gone() }
}
