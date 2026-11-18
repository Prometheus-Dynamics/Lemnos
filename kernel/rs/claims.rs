// SPDX-License-Identifier: GPL-2.0
//! Claims, Rust (stage 1b of the port; replaces lemnos_claims.c under CONFIG_RUST).
//!
//! A claim is a file, from `anon_inode_getfd`, that holds one resource name for as long as it is
//! open. A second claim on the name is refused (EBUSY); at most LEM_MAX_CLAIMS exist. The
//! other parts reach it through four functions: `lem_claim`, `lem_claim_held`, `lem_claim_attach` and `lem_claim_attached`.
//! The names are kept in an RBTree (the C kept them in a hash table, looked up by the same name), so
//! a duplicate name, a full table and a name found are the same results as before.
//!
//! The claim file's operations are a `struct file_operations` from `kernel::bindings`. `anon_inode_getfd`
//! is not in the bindings (anon_inodes.h is not in the kernel crate's allowlist), so it is declared
//! here; it is an exported symbol, not an inline.

use crate::abi::LemClaimOps;
use core::{
    ffi::c_void,
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
};
use kernel::{
    alloc::KBox,
    bindings,
    error::code::*,
    ffi::{c_char, c_int, c_long, c_uint},
    prelude::*,
    rbtree::{RBTree, RBTreeNode},
    str::CStr,
};

/// Each claim is kernel memory, so their number has an end.
const LEM_MAX_CLAIMS: u32 = 4096;
/// LEMNOS_NAME_MAX of lemnos.h.
const LEMNOS_NAME_MAX: usize = 64;

/// `struct lemnos_claim` of lemnos.h (the LEMNOS_IOC_CLAIM argument).
#[repr(C)]
pub(crate) struct LemnosClaim {
    resource: [c_char; LEMNOS_NAME_MAX],
    flags: u32,
    fd: i32,
}


/// One held claim: the file's private data. `ops` and `data` are set once, by `lem_claim_attach`,
/// under HELD's lock; `ops` is published with release and read with acquire, as the C did.
#[repr(C)]
struct LemHeld {
    resource: [u8; LEMNOS_NAME_MAX],
    data: AtomicPtr<c_void>,
    ops: AtomicPtr<LemClaimOps>,
}

/// The names held, and their count. `names` is the whole table: its keys are the resources.
struct Held {
    names: Option<RBTree<[u8; LEMNOS_NAME_MAX], ()>>,
    count: u32,
}

kernel::sync::global_lock! {
    unsafe(uninit) static HELD: Mutex<Held> = Held { names: None, count: 0 };
}

/// `struct file_operations` for the claim files. Its owner is the module, set by `init`.
#[repr(transparent)]
struct FopsCell(bindings::file_operations);

// SAFETY: written once, by `init`, before any claim file exists; after that only read.
unsafe impl Sync for FopsCell {}

// SAFETY: `init` writes the owner before any claim file is made, and nothing writes it after.
static mut LEM_CLAIM_FOPS: FopsCell = FopsCell(bindings::file_operations {
    owner: ptr::null_mut(),
    llseek: Some(bindings::noop_llseek),
    read: Some(lem_claim_read),
    poll: Some(lem_claim_poll),
    release: Some(lem_claim_release),
    // SAFETY: a file_operations of zeros is one with every other operation absent.
    ..unsafe { core::mem::zeroed() }
});

unsafe extern "C" {
    /// lib/anon_inodes.c, exported. Not in the kernel crate's bindings.
    fn anon_inode_getfd(
        name: *const c_char,
        fops: *const c_void,
        priv_: *mut c_void,
        flags: c_int,
    ) -> c_int;
}

fn fops_ptr() -> *const bindings::file_operations {
    // SAFETY: the table is written only by `init`, before any claim exists; this takes its address.
    unsafe { &raw const LEM_CLAIM_FOPS.0 }
}

/// Whether `file` is a claim file (its operations are ours).
///
/// # Safety
/// `file` is null or a live `struct file`.
unsafe fn is_claim(file: *const bindings::file) -> bool {
    // SAFETY: the caller's contract; reads one field.
    !file.is_null() && unsafe { (*file).f_op } == fops_ptr()
}

/// The held claim of a claim file.
///
/// # Safety
/// `file` is a claim file (`is_claim`) and is live.
unsafe fn held_of<'a>(file: *const bindings::file) -> &'a LemHeld {
    // SAFETY: the caller's contract: the file is live.
    let held = unsafe { (*file).private_data } as *const LemHeld;
    // SAFETY: a claim file's private data is its `LemHeld`, set by `lem_claim` and freed on release.
    unsafe { &*held }
}

/// The name a claim holds, as a C string.
fn name_of(h: &LemHeld) -> &CStr {
    CStr::from_bytes_until_nul(&h.resource).unwrap_or(c"")
}

unsafe extern "C" fn lem_claim_release(_inode: *mut bindings::inode, file: *mut bindings::file) -> c_int {
    // SAFETY: called by the VFS for our claim file, once, when its last reference closes.
    let h = unsafe { (*file).private_data as *mut LemHeld };
    // SAFETY: `h` is live until freed just below.
    let ops = unsafe { (*h).ops.load(Ordering::Acquire) };
    if !ops.is_null() {
        // SAFETY: `ops` was attached and points at the module's static table.
        if let Some(release) = unsafe { (*ops).release } {
            // SAFETY: `h` is live until freed just below.
            let data = unsafe { (*h).data.load(Ordering::Relaxed) };
            // SAFETY: `data` is what the attacher gave, and the attach stored it before `ops`.
            unsafe { release(data) };
        }
    }
    // Ended before the name comes free, so the next holder finds it let go.
    {
        let mut held = HELD.lock();
        // SAFETY: `h` holds the name it was inserted under, and the table has it.
        let key = unsafe { (*h).resource };
        held.names.get_or_insert_with(RBTree::new).remove(&key);
        held.count -= 1;
    }
    // SAFETY: `h` came from KBox::into_raw in `lem_claim`, and this is its only free.
    drop(unsafe { KBox::from_raw(h) });
    0
}

unsafe extern "C" fn lem_claim_read(file: *mut bindings::file, buf: *mut c_char, len: usize, _ppos: *mut bindings::loff_t) -> isize {
    // SAFETY: the VFS calls this with our claim file.
    let h = unsafe { held_of(file) };
    let ops = h.ops.load(Ordering::Acquire);
    if ops.is_null() {
        return EINVAL.to_errno() as isize;
    }
    // SAFETY: attached `ops` points at the module's static table.
    match unsafe { (*ops).read } {
        // SAFETY: the callback gets the data its attacher set and the file and buffer the VFS gave.
        Some(read) => unsafe { read(h.data.load(Ordering::Relaxed), file, buf, len) },
        None => EINVAL.to_errno() as isize,
    }
}

unsafe extern "C" fn lem_claim_poll(file: *mut bindings::file, wait: *mut bindings::poll_table_struct) -> bindings::__poll_t {
    // SAFETY: the VFS calls this with our claim file.
    let h = unsafe { held_of(file) };
    let ops = h.ops.load(Ordering::Acquire);
    if ops.is_null() {
        return 0;
    }
    // SAFETY: attached `ops` points at the module's static table.
    match unsafe { (*ops).poll } {
        // SAFETY: the callback gets the data its attacher set and the file and wait the VFS gave.
        Some(poll) => unsafe { poll(h.data.load(Ordering::Relaxed), file, wait) },
        None => 0,
    }
}

/// Whether `file` is a claim on exactly `resource`.
///
/// # Safety
/// `file` is null or live; `resource` is a NUL-terminated string.
unsafe fn is_claim_on(file: *const bindings::file, resource: &CStr) -> bool {
    // SAFETY: the caller's contract: null or live.
    if !unsafe { is_claim(file) } {
        return false;
    }
    // SAFETY: a claim file, found so just above, and live by the caller's contract.
    name_of(unsafe { held_of(file) }) == resource
}

/// Sets the module that owns the claim files' operations, and initialises the claim table. Called
/// once, by the Rust entry (lemnos_entry.rs), before any claim can be made.
///
/// # Safety
/// `owner` is the module's `THIS_MODULE`, live for the module's life.
pub(crate) unsafe extern "C" fn lem_claims_init(owner: *mut bindings::module) {
    // SAFETY: no claim file exists yet, so nothing reads the table; this is the one write.
    unsafe { (*(&raw mut LEM_CLAIM_FOPS)).0.owner = owner };
    // SAFETY: HELD is initialised exactly once, here, before any claim can be made.
    unsafe { HELD.init() };
}

/// Refused while the name is held or the table is full.
fn insert(h: &LemHeld) -> Result {
    let mut held = HELD.lock();
    // The same order as the C hash lookup: a held name first, then a full table.
    let names = held.names.get_or_insert_with(RBTree::new);
    if names.get(&h.resource).is_some() {
        return Err(EBUSY);
    }
    if held.count >= LEM_MAX_CLAIMS {
        return Err(ENOSPC);
    }
    let names = held.names.get_or_insert_with(RBTree::new);
    names.insert(RBTreeNode::new(h.resource, (), GFP_KERNEL)?);
    held.count += 1;
    Ok(())
}

/// The `LEMNOS_IOC_CLAIM` path: a claim on `c->resource`, returned as `c->fd`.
///
/// # Safety
/// `c` is a live `struct lemnos_claim` from the ioctl copy.
pub(crate) unsafe extern "C" fn lem_claim(c: *mut LemnosClaim) -> c_long {
    // SAFETY: the caller passes its own argument struct.
    let c = unsafe { &mut *c };
    let resource = &c.resource;
    let len = resource.iter().position(|&b| b == 0).unwrap_or(LEMNOS_NAME_MAX);
    if c.flags != 0 || len == 0 || len == LEMNOS_NAME_MAX {
        return EINVAL.to_errno() as c_long;
    }
    let mut name = [0u8; LEMNOS_NAME_MAX];
    for (dst, src) in name.iter_mut().zip(resource.iter()).take(len) {
        *dst = *src as u8;
    }
    let h = match KBox::new(
        LemHeld {
            resource: name,
            data: AtomicPtr::new(ptr::null_mut()),
            ops: AtomicPtr::new(ptr::null_mut()),
        },
        GFP_KERNEL,
    ) {
        Ok(h) => h,
        Err(_) => return ENOMEM.to_errno() as c_long,
    };
    if let Err(e) = insert(&h) {
        return e.to_errno() as c_long;
    }
    let raw = KBox::into_raw(h);
    // SAFETY: the name is a NUL-terminated string in a static string; the fops table is ours; `raw`
    // is the private data, freed by lem_claim_release, or below if the file is not made.
    let fd = unsafe {
        anon_inode_getfd(c"[lemnos-claim]".as_ptr().cast(), fops_ptr().cast(), raw as *mut c_void, bindings::O_RDWR as c_int | bindings::O_CLOEXEC as c_int)
    };
    if fd < 0 {
        // SAFETY: the file was not made, so nothing else holds `raw`.
        let h = unsafe { KBox::from_raw(raw) };
        let key = h.resource;
        let mut held = HELD.lock();
        held.names.get_or_insert_with(RBTree::new).remove(&key);
        held.count -= 1;
        return fd as c_long;
    }
    c.fd = fd;
    0
}

/// Whether `fd` is a claim on exactly `resource`. Holds no reference past the call.
///
/// # Safety
/// `resource` is a NUL-terminated string.
pub(crate) unsafe extern "C" fn lem_claim_held(fd: c_int, resource: *const c_char) -> bool {
    // SAFETY: the C caller passes a NUL-terminated name.
    let resource = unsafe { CStr::from_char_ptr(resource) };
    // SAFETY: fget takes a reference; it is dropped by fput below.
    let file = unsafe { bindings::fget(fd as c_uint) };
    // SAFETY: `file` is what fget returned, live until fput.
    let held = unsafe { is_claim_on(file, resource) };
    if !file.is_null() {
        // SAFETY: drops the reference fget took.
        unsafe { bindings::fput(file) };
    }
    held
}

/// Binds `data` to the claim `fd`, which must be on exactly `resource`. Once per claim.
///
/// # Safety
/// `resource` is a NUL-terminated string; `ops` is a static table of the C side.
pub(crate) unsafe extern "C" fn lem_claim_attach(fd: c_int, resource: *const c_char, ops: *const LemClaimOps, data: *mut c_void) -> c_int {
    // SAFETY: the C caller passes a NUL-terminated name.
    let resource = unsafe { CStr::from_char_ptr(resource) };
    // SAFETY: fget takes a reference; dropped by fput below.
    let file = unsafe { bindings::fget(fd as c_uint) };
    // SAFETY: `file` is live until fput.
    if !unsafe { is_claim_on(file, resource) } {
        if !file.is_null() {
            // SAFETY: drops the reference fget took.
            unsafe { bindings::fput(file) };
        }
        return EPERM.to_errno();
    }
    // SAFETY: a claim file, live until fput; its private data is the LemHeld.
    let h = unsafe { held_of(file) };
    let guard = HELD.lock();
    let ret = if !h.ops.load(Ordering::Acquire).is_null() {
        EBUSY.to_errno()
    } else {
        h.data.store(data, Ordering::Relaxed);
        h.ops.store(ops as *mut LemClaimOps, Ordering::Release);
        0
    };
    drop(guard);
    // SAFETY: drops the reference fget took.
    unsafe { bindings::fput(file) };
    ret
}

/// What `file` is bound to with `ops`, or NULL. `file` may be NULL.
///
/// # Safety
/// `file` is null or live; `ops` is a static table of the C side.
pub(crate) unsafe extern "C" fn lem_claim_attached(file: *mut bindings::file, ops: *const LemClaimOps) -> *mut c_void {
    // SAFETY: the caller's contract.
    if !unsafe { is_claim(file) } {
        return ptr::null_mut();
    }
    // SAFETY: a claim file, live for the call.
    let h = unsafe { held_of(file) };
    if h.ops.load(Ordering::Acquire) as *const LemClaimOps == ops {
        h.data.load(Ordering::Relaxed)
    } else {
        ptr::null_mut()
    }
}
