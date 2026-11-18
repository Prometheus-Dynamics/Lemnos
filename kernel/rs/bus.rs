// SPDX-License-Identifier: GPL-2.0
//! I2C and SPI transfer lists through the kernel's own cores, Rust (stage 1g of the port; replaces
//! lemnos_bus.c). The interface is lemnos.h's.
//!
//! Buffers are made once, per adapter on first use and per SPI device at bind, so a transfer allocates
//! nothing. The adapter is looked up on each transfer and not held between them, so one can go away
//! while the module is loaded. The check for addresses a kernel driver holds is i2c-dev's.
//!
//! The four ioctls keep their C signatures (lemnos_internal.h); the entry calls `lem_bus_init` and
//! `lem_bus_exit` directly. The SPI core and the uAPI structs come from the module's generated bindings
//! (rs/bindings.rs). The inline kernel helpers the C used are composed from their exported parts:
//! i2c_check_functionality and i2c_parent_is_i2c_adapter from the adapter's fields,
//! spi_message_init_with_transfers from INIT_LIST_HEAD and list_add_tail, spi_unregister_driver from
//! driver_unregister.
//!
//! The C held one rwsem across every SPI transfer so a device's removal waited for its transfers. Here
//! each device's state has its own lock: removal takes it and marks the device gone, so it waits for a
//! transfer on that device only, and a transfer that finds the device gone gets ENODEV, as one that
//! found no device did.

use crate::bindings as lb;
use core::{cell::UnsafeCell, fmt::Write, ptr};
use kernel::{
    alloc::{KBox, KVec},
    bindings,
    error::{code::*, Error},
    ffi::{c_char, c_int, c_long, c_uint, c_void},
    new_mutex,
    prelude::*,
    rbtree::{RBTree, RBTreeNode},
    sync::{Arc, Mutex},
    transmute::{AsBytes, FromBytes},
    uaccess::{UserPtr, UserSlice},
};

/// The longest resource name, with its NUL: "i2c-<u32>:0x<3ff>" and "spi-<u32>.<u8>" fit.
const LEM_RESOURCE_MAX: usize = 32;
/// An I2C message's length is 16 bits.
const LEM_I2C_SEGMENT_MAX: u32 = 65535;
/// The largest bus_buffer.
const LEM_BUS_BUFFER_MAX: c_uint = 1 << 20;
/// LEMNOS_BUS_MAX_SEGMENTS of lemnos.h.
const SEGMENTS: usize = lb::LEMNOS_BUS_MAX_SEGMENTS as usize;
/// The mode bits a transfer may set: SPI_CPHA, SPI_CPOL, SPI_CS_HIGH and SPI_LSB_FIRST. They are uAPI
/// (include/uapi/linux/spi/spi.h, `_BITUL(0..=3)`), which bindgen does not evaluate.
const LEM_SPI_MODES: u32 = 1 | 2 | 4 | 8;

// SAFETY: integers only (u64, u32, u32), no padding: every byte is initialised, and any bytes are a value.
unsafe impl FromBytes for lb::lemnos_i2c_segment {}
// SAFETY: as above.
unsafe impl AsBytes for lb::lemnos_i2c_segment {}
// SAFETY: integers only (u64, u64, u32, u32, u16, u8, u8, u32), no padding.
unsafe impl FromBytes for lb::lemnos_spi_segment {}
// SAFETY: as above.
unsafe impl AsBytes for lb::lemnos_spi_segment {}

const _: () = assert!(core::mem::size_of::<lb::lemnos_i2c_segment>() == 16);
const _: () = assert!(core::mem::size_of::<lb::lemnos_spi_segment>() == 32);
const _: () = assert!(core::mem::size_of::<lb::lemnos_i2c_smbus>() == 56);
const _: () = assert!(core::mem::size_of::<bindings::i2c_smbus_data>() == 34);

// --- The bus_buffer parameter ---

/// The parameter's value. The kernel writes it while parsing the module's parameters, before init; it
/// is 0444, so nothing writes it after.
#[repr(transparent)]
struct ParamValue(UnsafeCell<c_uint>);

// SAFETY: written once, by the kernel's parameter parser before init, and only read after.
unsafe impl Sync for ParamValue {}

static BUS_BUFFER: ParamValue = ParamValue(UnsafeCell::new(4096));

/// `struct kernel_param`, in the `__param` section the kernel reads parameters from.
#[repr(transparent)]
struct Param(bindings::kernel_param);

// SAFETY: the kernel alone reads it (it is never accessed from Rust), and serialises that itself.
unsafe impl Sync for Param {}

/// `module_param(bus_buffer, uint, 0444)`: the kernel's own uint operations, so it reads back in sysfs
/// as the C parameter did. A built-in module's parameters carry the module's name.
#[link_section = "__param"]
#[used(compiler)]
static BUS_BUFFER_PARAM: Param = Param(bindings::kernel_param {
    name: if cfg!(MODULE) { c"bus_buffer".as_ptr().cast() } else { c"lemnos.bus_buffer".as_ptr().cast() },
    #[cfg(MODULE)]
    // `__this_module` is made by the kernel at load and lives until the module is unloaded.
    mod_: (&raw const bindings::__this_module).cast_mut(),
    #[cfg(not(MODULE))]
    mod_: ptr::null_mut(),
    // The kernel's exported uint parameter operations, a static that lives for ever.
    ops: &raw const bindings::param_ops_uint,
    perm: 0o444,
    level: -1,
    flags: 0,
    __bindgen_anon_1: bindings::kernel_param__bindgen_ty_1 { arg: BUS_BUFFER.0.get().cast() },
});

/// A `.modinfo` string as an array, for a static in that section.
const fn modinfo<const N: usize>(s: &[u8]) -> [u8; N] {
    let mut out = [0u8; N];
    let mut i = 0;
    while i < N {
        out[i] = s[i];
        i += 1;
    }
    out
}

macro_rules! modinfo_param {
    ($name:ident, $text:literal) => {
        #[cfg(MODULE)]
        #[link_section = ".modinfo"]
        #[used(compiler)]
        static $name: [u8; $text.len() + 1] = modinfo(concat!($text, "\0").as_bytes());
    };
    ($name:ident, builtin $text:literal) => {
        #[cfg(not(MODULE))]
        #[link_section = ".modinfo"]
        #[used(compiler)]
        static $name: [u8; $text.len() + 1] = modinfo(concat!($text, "\0").as_bytes());
    };
}

// What MODULE_PARM_DESC and module_param put in .modinfo, for modinfo(8).
modinfo_param!(BUS_BUFFER_PARMTYPE, "parmtype=bus_buffer:uint");
modinfo_param!(BUS_BUFFER_PARM, "parm=bus_buffer:Largest transfer, in bytes each way, per bus (default 4096)");
modinfo_param!(BUS_BUFFER_PARMTYPE_BUILTIN, builtin "lemnos.parmtype=bus_buffer:uint");
modinfo_param!(
    BUS_BUFFER_PARM_BUILTIN,
    builtin "lemnos.parm=bus_buffer:Largest transfer, in bytes each way, per bus (default 4096)"
);

/// bus_buffer, in bytes. `lem_bus_init` refused the module unless 0 < it <= LEM_BUS_BUFFER_MAX.
fn bus_buffer() -> usize {
    // SAFETY: written only by the parameter parser, before init (see ParamValue); this reads it.
    unsafe { *BUS_BUFFER.0.get() as usize }
}

// --- Shared ---

unsafe extern "C" {
    /// rs/claims.rs: whether `fd` is a claim on exactly `resource`. Allocates nothing.
    fn lem_claim_held(fd: c_int, resource: *const c_char) -> bool;
    /// lemnos_core.c: whether `dev`'s compatible is in owned=.
    fn lem_block_owned(dev: *mut c_void) -> bool;
}

/// A resource name, written in place: no allocation.
struct Resource {
    buf: [u8; LEM_RESOURCE_MAX],
    len: usize,
}

impl Write for Resource {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        // Leaves room for the NUL. The names this module writes always fit (LEM_RESOURCE_MAX).
        let end = self.len + s.len();
        if end >= LEM_RESOURCE_MAX {
            return Err(core::fmt::Error);
        }
        self.buf[self.len..end].copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// Whether claim `fd` holds the resource `args` names.
fn held(fd: i32, args: core::fmt::Arguments<'_>) -> bool {
    let mut r = Resource { buf: [0; LEM_RESOURCE_MAX], len: 0 };
    if r.write_fmt(args).is_err() {
        return false;
    }
    // SAFETY: `r.buf` is NUL-terminated: the writer never fills its last byte, and it started zeroed.
    unsafe { lem_claim_held(fd, r.buf.as_ptr().cast()) }
}

/// The C return of a Result.
fn ret(r: Result) -> c_long {
    match r {
        Ok(()) => 0,
        Err(e) => e.to_errno() as c_long,
    }
}

/// A C status (0 or a negative errno) as a Result.
fn status(v: c_int) -> Result {
    if v < 0 {
        Err(Error::from_errno(v))
    } else {
        Ok(())
    }
}

/// `len` bytes from the user address `addr` into `out`.
fn copy_in(out: &mut [u8], addr: u64) -> Result {
    UserSlice::new(UserPtr::from_addr(addr as usize), out.len()).reader().read_slice(out)
}

/// `src` to the user address `addr`.
fn copy_out(addr: u64, src: &[u8]) -> Result {
    UserSlice::new(UserPtr::from_addr(addr as usize), src.len()).writer().write_slice(src)
}

// --- I2C ---

/// One adapter's buffers. The messages point into `buf`; all of it is used under the adapter's lock.
struct I2cWork {
    buf: KVec<u8>,
    msgs: [bindings::i2c_msg; SEGMENTS],
    seg: [lb::lemnos_i2c_segment; SEGMENTS],
}

// SAFETY: the message pointers point into this value's own buffer, and the value is only touched
// under its Mutex.
unsafe impl Send for I2cWork {}

#[pin_data]
struct LemI2c {
    #[pin]
    work: Mutex<I2cWork>,
}

kernel::sync::global_lock! {
    // SAFETY: initialised by lem_bus_init, before any transfer, and only there.
    unsafe(uninit) static I2CS: Mutex<Option<RBTree<u32, Arc<LemI2c>>>> = None;
}

/// The bus's state, made on its first transfer; adapters exist, so their number bounds this.
fn lem_i2c_state(bus: u32) -> Result<Arc<LemI2c>> {
    let mut map = I2CS.lock();
    let tree = map.get_or_insert_with(RBTree::new);
    if let Some(s) = tree.get(&bus) {
        return Ok(s.clone());
    }
    let buf = KVec::from_elem(0u8, bus_buffer(), GFP_KERNEL)?;
    let work = I2cWork {
        buf,
        msgs: [bindings::i2c_msg { addr: 0, flags: 0, len: 0, buf: ptr::null_mut() }; SEGMENTS],
        seg: [lb::lemnos_i2c_segment::default(); SEGMENTS],
    };
    let s = Arc::pin_init(pin_init!(LemI2c { work <- new_mutex!(work) }), GFP_KERNEL)?;
    tree.insert(RBTreeNode::new(bus, s.clone(), GFP_KERNEL)?);
    Ok(s)
}

/// i2c_parent_is_i2c_adapter: the adapter this one is a mux channel of, or NULL.
///
/// # Safety
/// `adap` is a live, held adapter; its parents live as long as it does.
unsafe fn parent_adapter(adap: *mut bindings::i2c_adapter) -> *mut bindings::i2c_adapter {
    // SAFETY: the caller's contract; reads one field.
    let parent = unsafe { (*adap).dev.parent };
    // SAFETY: a device's parent, when set, is a live device; reads its type.
    if parent.is_null() || unsafe { (*parent).type_ } != &raw const bindings::i2c_adapter_type {
        return ptr::null_mut();
    }
    // An adapter's device is its `dev` field: the adapter is its container (to_i2c_adapter).
    parent.wrapping_byte_sub(core::mem::offset_of!(bindings::i2c_adapter, dev)).cast()
}

/// device_for_each_child's callback: a client at the address with a driver bound holds it.
///
/// # Safety
/// `dev` is a live device; `addrp` points at the address, a c_uint.
unsafe extern "C" fn lem_i2c_check_addr(dev: *mut bindings::device, addrp: *mut c_void) -> c_int {
    // SAFETY: the caller's contract: `dev` is live.
    let client = unsafe { bindings::i2c_verify_client(dev) };
    if client.is_null() {
        return 0;
    }
    // SAFETY: `addrp` is the address lem_i2c_check_held passes.
    let addr = unsafe { *addrp.cast::<c_uint>() };
    // SAFETY: `client` is `dev`'s client, live with it.
    if c_uint::from(unsafe { (*client).addr }) != addr {
        return 0;
    }
    // SAFETY: `dev` is live; reads whether a driver is bound.
    if unsafe { (*dev).driver }.is_null() {
        0
    } else {
        EBUSY.to_errno()
    }
}

/// The same, through mux children: an adapter below is searched in turn.
///
/// # Safety
/// As lem_i2c_check_addr.
unsafe extern "C" fn lem_i2c_check_mux_children(dev: *mut bindings::device, addrp: *mut c_void) -> c_int {
    // SAFETY: `dev` is live; reads its type.
    if unsafe { (*dev).type_ } == &raw const bindings::i2c_adapter_type {
        // SAFETY: `dev` is live, and the callback keeps the caller's contract for each child.
        return unsafe { bindings::device_for_each_child(dev, addrp, Some(lem_i2c_check_mux_children)) };
    }
    // SAFETY: the caller's contract.
    unsafe { lem_i2c_check_addr(dev, addrp) }
}

/// An address a kernel driver holds on this adapter, its muxes, or the buses above it: 0 or -EBUSY.
///
/// # Safety
/// `adap` is a live, held adapter.
unsafe fn lem_i2c_check_held(adap: *mut bindings::i2c_adapter, addr: c_uint) -> c_int {
    let mut addr = addr;
    let addrp = (&raw mut addr).cast::<c_void>();
    // SAFETY: the adapter is held, so its device is live; the callback reads `addr` only.
    let mut ret = unsafe { bindings::device_for_each_child(&raw mut (*adap).dev, addrp, Some(lem_i2c_check_mux_children)) };
    // SAFETY: the caller's contract.
    let mut parent = unsafe { parent_adapter(adap) };
    while !parent.is_null() && ret == 0 {
        // SAFETY: a parent of a held adapter lives as long as it; the callback reads `addr` only.
        ret = unsafe { bindings::device_for_each_child(&raw mut (*parent).dev, addrp, Some(lem_i2c_check_addr)) };
        // SAFETY: as above.
        parent = unsafe { parent_adapter(parent) };
    }
    ret
}

/// A held adapter, put when dropped.
struct Adapter(*mut bindings::i2c_adapter);

impl Drop for Adapter {
    fn drop(&mut self) {
        // SAFETY: the reference i2c_get_adapter took, put once.
        unsafe { bindings::i2c_put_adapter(self.0) };
    }
}

/// The claim is right, the address fits, and the adapter exists: returns it held.
fn lem_i2c_target(claim: i32, bus: u32, addr: u16, flags: u16) -> Result<Adapter> {
    let ten = u32::from(flags) & lb::LEMNOS_I2C_TEN_BIT != 0;
    if u32::from(flags) & !lb::LEMNOS_I2C_TEN_BIT != 0 || addr > if ten { 0x3ff } else { 0x7f } {
        return Err(EINVAL);
    }
    if !held(claim, format_args!("i2c-{bus}:0x{addr:x}")) {
        return Err(EPERM);
    }
    let Ok(nr) = c_int::try_from(bus) else {
        return Err(ENODEV);
    };
    // SAFETY: looks the adapter up by number and takes a reference, or returns NULL.
    let adap = unsafe { bindings::i2c_get_adapter(nr) };
    if adap.is_null() {
        return Err(ENODEV);
    }
    let adap = Adapter(adap);
    // SAFETY: `adap` is held.
    status(unsafe { lem_i2c_check_held(adap.0, c_uint::from(addr)) })?;
    Ok(adap)
}

/// i2c_check_functionality(adap, I2C_FUNC_I2C).
fn plain_i2c(adap: &Adapter) -> bool {
    // SAFETY: the adapter is held, so its algorithm is set and live.
    let functionality = unsafe { (*(*adap.0).algo).functionality };
    // SAFETY: the algorithm's own callback, on its own held adapter.
    functionality.is_some_and(|f| unsafe { f(adap.0) } & bindings::I2C_FUNC_I2C == bindings::I2C_FUNC_I2C)
}

fn lem_i2c_run(w: &mut I2cWork, adap: &Adapter, t: &lb::lemnos_i2c_transfer) -> Result {
    let count = t.count as usize;
    copy_in(w.seg[..count].as_bytes_mut(), t.segments)?;
    let ten = if u32::from(t.flags) & lb::LEMNOS_I2C_TEN_BIT != 0 { bindings::I2C_M_TEN as u16 } else { 0 };
    let bus_buffer = w.buf.len();
    let mut used = 0usize;
    for i in 0..count {
        let g = w.seg[i];
        let read = g.flags & lb::LEMNOS_I2C_READ != 0;
        if g.flags & !lb::LEMNOS_I2C_READ != 0 {
            return Err(EINVAL);
        }
        let len = g.len as usize;
        if g.len > LEM_I2C_SEGMENT_MAX || len > bus_buffer - used {
            return Err(EMSGSIZE);
        }
        let buf = &mut w.buf[used..used + len];
        w.msgs[i] = bindings::i2c_msg {
            addr: t.addr,
            flags: ten | if read { bindings::I2C_M_RD as u16 } else { 0 },
            len: g.len as u16,
            buf: buf.as_mut_ptr(),
        };
        if !read {
            copy_in(buf, g.buf)?;
        }
        used += len;
    }
    // SAFETY: the adapter is held; the `count` messages point into `w.buf`, which the lock keeps ours.
    let n = unsafe { bindings::i2c_transfer(adap.0, w.msgs.as_mut_ptr(), count as c_int) };
    status(n)?;
    if n as usize != count {
        return Err(EIO);
    }
    let mut used = 0usize;
    for i in 0..count {
        let len = w.seg[i].len as usize;
        if u32::from(w.msgs[i].flags) & bindings::I2C_M_RD != 0 {
            copy_out(w.seg[i].buf, &w.buf[used..used + len])?;
        }
        used += len;
    }
    Ok(())
}

fn i2c_transfer(t: &lb::lemnos_i2c_transfer) -> Result {
    if t.count == 0 || t.count as usize > SEGMENTS {
        return Err(EINVAL);
    }
    let adap = lem_i2c_target(t.claim, t.bus, t.addr, t.flags)?;
    if !plain_i2c(&adap) {
        return Err(EOPNOTSUPP);
    }
    let s = lem_i2c_state(t.bus)?;
    let mut w = s.work.lock();
    lem_i2c_run(&mut w, &adap, t)
}

/// `LEMNOS_IOC_I2C_TRANSFER`.
///
/// # Safety
/// `t` is the ioctl's argument, from the copy.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_i2c_transfer(t: *const lb::lemnos_i2c_transfer) -> c_long {
    // SAFETY: the caller's argument.
    ret(i2c_transfer(unsafe { &*t }))
}

fn lem_smbus_valid(s: &lb::lemnos_i2c_smbus) -> bool {
    let len = u32::from(s.data[0]);
    let block = (1..=bindings::I2C_SMBUS_BLOCK_MAX).contains(&len);
    if s.read > 1 {
        return false;
    }
    match u32::from(s.protocol) {
        bindings::I2C_SMBUS_QUICK
        | bindings::I2C_SMBUS_BYTE
        | bindings::I2C_SMBUS_BYTE_DATA
        | bindings::I2C_SMBUS_WORD_DATA
        | bindings::I2C_SMBUS_PROC_CALL => true,
        bindings::I2C_SMBUS_BLOCK_DATA => s.read != 0 || block,
        bindings::I2C_SMBUS_I2C_BLOCK_DATA | bindings::I2C_SMBUS_BLOCK_PROC_CALL => block,
        _ => false,
    }
}

fn i2c_smbus(s: &mut lb::lemnos_i2c_smbus) -> Result {
    if !lem_smbus_valid(s) {
        return Err(EINVAL);
    }
    let adap = lem_i2c_target(s.claim, s.bus, s.addr, s.flags)?;
    let mut data = bindings::i2c_smbus_data { block: s.data };
    let flags = if u32::from(s.flags) & lb::LEMNOS_I2C_TEN_BIT != 0 { bindings::I2C_CLIENT_TEN as u16 } else { 0 };
    let read_write = if s.read != 0 { bindings::I2C_SMBUS_READ } else { bindings::I2C_SMBUS_WRITE };
    // SAFETY: the adapter is held; `data` is a whole union i2c_smbus_data on this stack.
    let r = unsafe {
        bindings::i2c_smbus_xfer(
            adap.0,
            s.addr,
            flags,
            read_write as c_char,
            s.command,
            c_int::from(s.protocol),
            &raw mut data,
        )
    };
    drop(adap);
    status(r)?;
    // SAFETY: every field of the union is bytes; `block` covers all of it.
    s.data = unsafe { data.block };
    Ok(())
}

/// `LEMNOS_IOC_I2C_SMBUS`.
///
/// # Safety
/// `s` is the ioctl's argument, from the copy.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_i2c_smbus(s: *mut lb::lemnos_i2c_smbus) -> c_long {
    // SAFETY: the caller's argument.
    ret(i2c_smbus(unsafe { &mut *s }))
}

// --- SPI ---

/// What a transfer builds: the message and its transfers, and the segments as copied in. The message's
/// list points into its own transfers, so this lives in its own allocation and never moves.
#[repr(C)]
struct SpiXfer {
    msg: lb::spi_message,
    x: [lb::spi_transfer; SEGMENTS],
    seg: [lb::lemnos_spi_segment; SEGMENTS],
}

/// One bound device's state. `spi` is NULL once the device is removed.
struct SpiWork {
    spi: *mut lb::spi_device,
    tx: KVec<u8>,
    rx: KVec<u8>,
    /// The mode bits as last applied.
    mode: u32,
    xfer: KBox<SpiXfer>,
}

// SAFETY: `spi` is a device the core keeps live until remove, which clears it under the lock; the
// transfer pointers point into this value's own buffers. All of it is used under its Mutex.
unsafe impl Send for SpiWork {}

#[pin_data]
struct LemSpi {
    #[pin]
    work: Mutex<SpiWork>,
}

kernel::sync::global_lock! {
    // SAFETY: initialised by lem_bus_init, before the driver is registered, and only there.
    unsafe(uninit) static SPIS: Mutex<Option<RBTree<u64, Arc<LemSpi>>>> = None;
}

fn lem_spi_key(bus: u32, cs: u32) -> u64 {
    u64::from(bus) << 8 | u64::from(cs)
}

/// The bound device at the key, if any.
fn lem_spi_find(key: u64) -> Option<Arc<LemSpi>> {
    SPIS.lock().as_ref().and_then(|tree| tree.get(&key).cloned())
}

/// A zeroed SpiXfer, as the C's kzalloc made it.
fn spi_xfer_zeroed() -> Result<KBox<SpiXfer>> {
    let mut b = KBox::<SpiXfer>::new_uninit(GFP_KERNEL)?;
    // SAFETY: writes zeroes over the whole of the new allocation.
    unsafe { b.as_mut_ptr().write_bytes(0, 1) };
    // SAFETY: SpiXfer is C structs of integers, pointers and lists, for which all zeroes are valid.
    Ok(unsafe { b.assume_init() })
}

fn lem_spi_apply_mode(w: &mut SpiWork, mode: u32) -> Result {
    if mode == w.mode {
        return Ok(());
    }
    let spi = w.spi;
    // SAFETY: `spi` is the bound device, live until remove clears it under this lock.
    let saved = unsafe { (*spi).mode };
    // SAFETY: as above; the mode is written under this lock, as the C did under its own.
    unsafe { (*spi).mode = (saved & !LEM_SPI_MODES) | mode };
    // SAFETY: as above.
    if let Err(e) = status(unsafe { lb::spi_setup(spi) }) {
        // SAFETY: as above.
        unsafe { (*spi).mode = saved };
        return Err(e);
    }
    w.mode = mode;
    Ok(())
}

fn lem_spi_run(w: &mut SpiWork, t: &lb::lemnos_spi_transfer) -> Result {
    lem_spi_apply_mode(w, t.mode)?;
    let count = t.count as usize;
    let x = &mut *w.xfer;
    copy_in(x.seg[..count].as_bytes_mut(), t.segments)?;
    let bus_buffer = w.tx.len();
    let mut used = 0usize;
    for i in 0..count {
        let g = x.seg[i];
        x.x[i] = lb::spi_transfer::default();
        let xi = &mut x.x[i];
        if g.reserved != 0 || (g.len != 0 && g.tx == 0 && g.rx == 0) {
            return Err(EINVAL);
        }
        let len = g.len as usize;
        if len > bus_buffer - used {
            return Err(EMSGSIZE);
        }
        if g.tx != 0 {
            let tx = &mut w.tx[used..used + len];
            copy_in(tx, g.tx)?;
            xi.tx_buf = tx.as_ptr().cast();
        }
        if g.rx != 0 {
            xi.rx_buf = w.rx[used..used + len].as_mut_ptr().cast();
        }
        xi.len = g.len;
        xi.speed_hz = g.speed_hz;
        xi.bits_per_word = g.bits;
        // A one-bit field: the C assignment keeps the low bit.
        xi.set_cs_change(c_uint::from(g.cs_change & 1));
        xi.delay.value = g.delay_us;
        xi.delay.unit = lb::SPI_DELAY_UNIT_USECS as u8;
        used += len;
    }
    // spi_message_init_with_transfers: zeroed, its two lists empty, then each transfer at the tail.
    x.msg = lb::spi_message::default();
    // SAFETY: the list heads are in `x`, which is a stable allocation the lock keeps ours.
    unsafe { bindings::INIT_LIST_HEAD(&raw mut x.msg.transfers) };
    // SAFETY: as above.
    unsafe { bindings::INIT_LIST_HEAD(&raw mut x.msg.resources) };
    for xi in x.x[..count].iter_mut() {
        // SAFETY: as above; each transfer is on no other list.
        unsafe { bindings::list_add_tail(&raw mut xi.transfer_list, &raw mut x.msg.transfers) };
    }
    // SAFETY: `spi` is bound (checked under this lock); the message and its buffers are ours until it returns.
    status(unsafe { lb::spi_sync(w.spi, &raw mut x.msg) })?;
    let mut used = 0usize;
    for g in x.seg[..count].iter() {
        let len = g.len as usize;
        if g.rx != 0 {
            copy_out(g.rx, &w.rx[used..used + len])?;
        }
        used += len;
    }
    Ok(())
}

fn spi_transfer(t: &lb::lemnos_spi_transfer) -> Result {
    if t.count == 0 || t.count as usize > SEGMENTS || t.reserved != 0 || t.mode & !LEM_SPI_MODES != 0 || t.cs > 255 {
        return Err(EINVAL);
    }
    if !held(t.claim, format_args!("spi-{}.{}", t.bus, t.cs)) {
        return Err(EPERM);
    }
    let d = lem_spi_find(lem_spi_key(t.bus, t.cs)).ok_or(ENODEV)?;
    let mut w = d.work.lock();
    if w.spi.is_null() {
        return Err(ENODEV);
    }
    lem_spi_run(&mut w, t)
}

/// `LEMNOS_IOC_SPI_TRANSFER`.
///
/// # Safety
/// `t` is the ioctl's argument, from the copy.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_spi_transfer(t: *const lb::lemnos_spi_transfer) -> c_long {
    // SAFETY: the caller's argument.
    ret(spi_transfer(unsafe { &*t }))
}

fn spi_info(t: &mut lb::lemnos_spi_info) -> Result {
    if t.reserved != 0 || t.cs > 255 {
        return Err(EINVAL);
    }
    let d = lem_spi_find(lem_spi_key(t.bus, t.cs)).ok_or(ENODEV)?;
    let w = d.work.lock();
    if w.spi.is_null() {
        return Err(ENODEV);
    }
    t.mode = w.mode;
    Ok(())
}

/// `LEMNOS_IOC_SPI_INFO`.
///
/// # Safety
/// `t` is the ioctl's argument, from the copy.
#[no_mangle]
pub(crate) unsafe extern "C" fn lem_spi_info(t: *mut lb::lemnos_spi_info) -> c_long {
    // SAFETY: the caller's argument.
    ret(spi_info(unsafe { &mut *t }))
}

/// The key of a bound device: its controller's bus number and its first chip select.
///
/// # Safety
/// `spi` is a live device.
unsafe fn device_key(spi: *mut lb::spi_device) -> u64 {
    // SAFETY: the caller's contract; a device's controller lives as long as it does.
    let bus = unsafe { (*(*spi).controller).bus_num };
    // SAFETY: the caller's contract (spi_get_chipselect(spi, 0)).
    let cs = unsafe { (*spi).chip_select[0] };
    lem_spi_key(i32::from(bus) as u32, u32::from(cs))
}

fn spi_probe(spi: *mut lb::spi_device) -> Result {
    // SAFETY: the core probes a live device; the mode is read once, at bind.
    let mode = unsafe { (*spi).mode } & LEM_SPI_MODES;
    let work = SpiWork {
        spi,
        tx: KVec::from_elem(0u8, bus_buffer(), GFP_KERNEL)?,
        rx: KVec::from_elem(0u8, bus_buffer(), GFP_KERNEL)?,
        mode,
        xfer: spi_xfer_zeroed()?,
    };
    let d = Arc::pin_init(pin_init!(LemSpi { work <- new_mutex!(work) }), GFP_KERNEL)?;
    // SAFETY: as above.
    let key = unsafe { device_key(spi) };
    let node = RBTreeNode::new(key, d, GFP_KERNEL)?;
    let mut map = SPIS.lock();
    let tree = map.get_or_insert_with(RBTree::new);
    if tree.get(&key).is_some() {
        return Err(EBUSY);
    }
    tree.insert(node);
    Ok(())
}

/// The SPI driver's probe: only a device whose compatible is in owned=.
///
/// # Safety
/// Called by the SPI core with a live device.
unsafe extern "C" fn lem_spi_probe(spi: *mut lb::spi_device) -> c_int {
    // SAFETY: the caller's contract; the device is the spi device's first field.
    let dev = unsafe { &raw mut (*spi).dev };
    // SAFETY: as above.
    if !unsafe { lem_block_owned(dev.cast()) } {
        // SAFETY: `dev` is live; the format has no arguments.
        return unsafe { bindings::dev_err_probe(dev, ENODEV.to_errno(), c"not a device Lemnos owns (see owned=)\n".as_ptr().cast()) };
    }
    match spi_probe(spi) {
        Ok(()) => 0,
        Err(e) => e.to_errno(),
    }
}

/// The SPI driver's remove: the device leaves the table, then waits out a transfer on it and is
/// marked gone, so nothing uses it after this returns.
///
/// # Safety
/// Called by the SPI core with a device this driver bound.
unsafe extern "C" fn lem_spi_remove(spi: *mut lb::spi_device) {
    // SAFETY: the caller's contract.
    let key = unsafe { device_key(spi) };
    let d = SPIS.lock().as_mut().and_then(|tree| tree.remove(&key));
    if let Some(d) = d {
        d.work.lock().spi = ptr::null_mut();
    }
}

/// `struct spi_driver`, with no id table: bound only through driver_override. Its owner is set by the
/// core at registration.
#[repr(transparent)]
struct SpiDriver(UnsafeCell<lb::spi_driver>);

// SAFETY: written by the SPI core at registration and at unregistration, both under its own locks;
// this module only passes its address.
unsafe impl Sync for SpiDriver {}

static LEM_SPI_DRIVER: SpiDriver = SpiDriver(UnsafeCell::new(lb::spi_driver {
    id_table: ptr::null(),
    probe: Some(lem_spi_probe),
    remove: Some(lem_spi_remove),
    shutdown: None,
    driver: bindings::device_driver {
        name: c"lemnos".as_ptr().cast(),
        // SAFETY: the rest of a struct device_driver is pointers, integers and flags, zero by default.
        ..unsafe { core::mem::zeroed() }
    },
}));

/// The bus glue's init: checks bus_buffer and registers the SPI driver. `lem_bus_exit` undoes it.
pub(crate) fn lem_bus_init(module: &'static ThisModule) -> Result {
    let bus_buffer = bus_buffer();
    if bus_buffer == 0 || bus_buffer > LEM_BUS_BUFFER_MAX as usize {
        return Err(EINVAL);
    }
    // SAFETY: called once, at module load, before any transfer and before the driver can probe.
    unsafe { I2CS.init() };
    // SAFETY: as above.
    unsafe { SPIS.init() };
    // SAFETY: the driver is a static; THIS_MODULE is `module`, live for the module's life.
    status(unsafe { lb::__spi_register_driver(module.as_ptr(), LEM_SPI_DRIVER.0.get()) })
}

/// The bus glue's exit: unregisters the SPI driver (which removes every bound device) and frees every
/// adapter's buffers.
pub(crate) fn lem_bus_exit() {
    // SAFETY: the driver was registered by lem_bus_init (spi_unregister_driver's body).
    unsafe { bindings::driver_unregister(&raw mut (*LEM_SPI_DRIVER.0.get()).driver) };
    // No transfer runs once the module is unloading: the node is gone.
    *I2CS.lock() = None;
}
