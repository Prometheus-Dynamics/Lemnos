// SPDX-License-Identifier: GPL-2.0
//! The stand-in for the RP1 PIO driver (the port of rp1_pio_mock.c). Calls are counted, sm_get
//! returns what sm_put was given plus one, data sent to a machine is what comes back from it, and a
//! machine past the configured limit has nothing to deliver, so the caller's wait runs out. Nothing
//! here is hardware.
//!
//! The 27 functions are exported under the driver's own names and C signatures (linux/pio_rp1.h), as
//! EXPORT_SYMBOL_GPL exports them: lemnos.ko's PIO part (kernel/rs/pio.rs) links against them. The two
//! argument blocks read here are laid out by bindgen from that header (the Kbuild).
//!
//! Copyright (c) 2026 the Lemnos authors.

// The fixtures' shared module: beside this directory in the source tree and in the harness's copy.
#[path = "../test_block/fixture.rs"]
mod fixture;

use core::sync::atomic::{AtomicI32, Ordering};
use fixture::{lb, param_typed, Shared};
use kernel::{
    alloc::KBox,
    bindings,
    error::code::*,
    ffi::{c_int, c_uint, c_void},
    prelude::*,
};

static CALLS: AtomicI32 = AtomicI32::new(0);
static CLIENTS: AtomicI32 = AtomicI32::new(0);
static LAST_PUT: AtomicI32 = AtomicI32::new(0);
/// The one machine whose transfers never finish, or -1.
static STICKY_SM: AtomicI32 = AtomicI32::new(-1);
static LAST_ADDR_NONZERO: AtomicI32 = AtomicI32::new(0);
param_typed!(calls, 0o444, param_ops_int, CALLS.as_ptr().cast(), "int");
param_typed!(
    clients,
    0o444,
    param_ops_int,
    CLIENTS.as_ptr().cast(),
    "int"
);
param_typed!(
    last_addr_nonzero,
    0o444,
    param_ops_int,
    LAST_ADDR_NONZERO.as_ptr().cast(),
    "int"
);
param_typed!(
    sticky_sm,
    0o644,
    param_ops_int,
    STICKY_SM.as_ptr().cast(),
    "int"
);

const STORE: usize = 64 * 1024;
/// What was last sent to a machine. Nothing serialises it, as in the C: the check runs one transfer
/// at a time.
static STORE_BYTES: Shared<[u8; STORE]> = Shared::new([0; STORE]);

/// `struct rp1_pio_client`: what a client is here.
struct Client {
    _unused: c_int,
}

/// `export_gpl!(name)`: EXPORT_SYMBOL_GPL(name), as include/linux/export.h writes it: an entry in the
/// `.export_symbol` section, which modpost turns into the module's symbol table.
macro_rules! export_gpl {
    ($name:ident) => {
        #[cfg(CONFIG_64BIT)]
        core::arch::global_asm!(concat!(
            ".section \".export_symbol\",\"a\"\n",
            "__export_symbol_",
            stringify!($name),
            ":\n",
            ".asciz \"GPL\"\n",
            ".ascii \"\\0\"\n",
            ".balign 8\n",
            ".quad ",
            stringify!($name),
            "\n",
            ".previous\n"
        ));
        #[cfg(not(CONFIG_64BIT))]
        core::arch::global_asm!(concat!(
            ".section \".export_symbol\",\"a\"\n",
            "__export_symbol_",
            stringify!($name),
            ":\n",
            ".asciz \"GPL\"\n",
            ".ascii \"\\0\"\n",
            ".balign 4\n",
            ".long ",
            stringify!($name),
            "\n",
            ".previous\n"
        ));
    };
}

/// `PIO rp1_pio_open(void)`.
#[no_mangle]
pub extern "C" fn rp1_pio_open() -> *mut c_void {
    match KBox::new(Client { _unused: 0 }, GFP_KERNEL) {
        Ok(c) => {
            CLIENTS.fetch_add(1, Ordering::Relaxed);
            KBox::into_raw(c).cast()
        }
        // ERR_PTR(-ENOMEM).
        Err(_) => (ENOMEM.to_errno() as isize) as *mut c_void,
    }
}
export_gpl!(rp1_pio_open);

/// `void rp1_pio_close(PIO client)`.
///
/// # Safety
///
/// `client` came from `rp1_pio_open`, and this is its one close.
#[no_mangle]
pub unsafe extern "C" fn rp1_pio_close(client: *mut c_void) {
    CLIENTS.fetch_sub(1, Ordering::Relaxed);
    if !client.is_null() {
        // SAFETY: the caller's: the box rp1_pio_open made, freed once here.
        drop(unsafe { KBox::from_raw(client.cast::<Client>()) });
    }
}
export_gpl!(rp1_pio_close);

/// `int rp1_pio_sm_config_xfer(PIO client, uint sm, uint dir, uint buf_size, uint buf_count)`.
#[no_mangle]
pub extern "C" fn rp1_pio_sm_config_xfer(
    _client: *mut c_void,
    _sm: c_uint,
    _dir: c_uint,
    buf_size: c_uint,
    _buf_count: c_uint,
) -> c_int {
    if buf_size as usize <= STORE {
        0
    } else {
        EINVAL.to_errno()
    }
}
export_gpl!(rp1_pio_sm_config_xfer);

/// `int rp1_pio_sm_xfer_data(PIO client, uint sm, uint dir, uint data_bytes, void *data,
/// dma_addr_t dma_addr, void (*callback)(void *param), void *param)`.
///
/// # Safety
///
/// `data` holds `data_bytes` bytes, and `callback` may be called with `param`.
#[no_mangle]
pub unsafe extern "C" fn rp1_pio_sm_xfer_data(
    _client: *mut c_void,
    sm: c_uint,
    dir: c_uint,
    data_bytes: c_uint,
    data: *mut c_void,
    dma_addr: bindings::dma_addr_t,
    callback: unsafe extern "C" fn(*mut c_void),
    param: *mut c_void,
) -> c_int {
    if dma_addr != 0 {
        LAST_ADDR_NONZERO.store(1, Ordering::Relaxed);
    }
    if data_bytes as usize > STORE {
        return EINVAL.to_errno();
    }
    if sm as c_int == STICKY_SM.load(Ordering::Relaxed) {
        return 0;
    }
    let store = STORE_BYTES.get().cast::<u8>();
    if dir == 0 {
        // SAFETY: the caller's `data` holds `data_bytes` bytes, and the store holds at least as many.
        unsafe { core::ptr::copy_nonoverlapping(data.cast::<u8>(), store, data_bytes as usize) };
    } else {
        // SAFETY: as above, the other way.
        unsafe { core::ptr::copy_nonoverlapping(store, data.cast::<u8>(), data_bytes as usize) };
    }
    // SAFETY: the caller's: the callback takes `param`.
    unsafe { callback(param) };
    0
}
export_gpl!(rp1_pio_sm_xfer_data);

/// `mock_define!(name)`: `int name(PIO client, void *param)`, which counts the call.
macro_rules! mock_define {
    ($name:ident) => {
        /// Counts the call and reports success.
        #[no_mangle]
        pub extern "C" fn $name(_client: *mut c_void, _param: *mut c_void) -> c_int {
            CALLS.fetch_add(1, Ordering::Relaxed);
            0
        }
        export_gpl!($name);
    };
}

mock_define!(rp1_pio_remove_program);
mock_define!(rp1_pio_sm_claim);
mock_define!(rp1_pio_sm_unclaim);
mock_define!(rp1_pio_sm_init);
mock_define!(rp1_pio_sm_set_config);
mock_define!(rp1_pio_sm_exec);
mock_define!(rp1_pio_sm_clear_fifos);
mock_define!(rp1_pio_sm_set_clkdiv);
mock_define!(rp1_pio_sm_set_pins);
mock_define!(rp1_pio_sm_set_pindirs);
mock_define!(rp1_pio_sm_set_enabled);
mock_define!(rp1_pio_sm_restart);
mock_define!(rp1_pio_sm_fifo_state);
mock_define!(rp1_pio_sm_drain_tx);
mock_define!(rp1_pio_sm_get_flags);
mock_define!(rp1_pio_gpio_init);
mock_define!(rp1_pio_gpio_set_function);
mock_define!(rp1_pio_gpio_set_pulls);
mock_define!(rp1_pio_gpio_set_outover);
mock_define!(rp1_pio_gpio_set_input_enabled);

/// A program goes in at offset 5, as the driver reports where it landed.
#[no_mangle]
pub extern "C" fn rp1_pio_add_program(_client: *mut c_void, _param: *mut c_void) -> c_int {
    CALLS.fetch_add(1, Ordering::Relaxed);
    5
}
export_gpl!(rp1_pio_add_program);

/// `int rp1_pio_sm_put(PIO client, void *param)`.
///
/// # Safety
///
/// `param` is a `struct rp1_pio_sm_put_args`.
#[no_mangle]
pub unsafe extern "C" fn rp1_pio_sm_put(_client: *mut c_void, param: *mut c_void) -> c_int {
    // SAFETY: the caller's: the argument block of this operation.
    let data = unsafe { (*param.cast::<lb::rp1_pio_sm_put_args>()).data };
    LAST_PUT.store(data as i32, Ordering::Relaxed);
    CALLS.fetch_add(1, Ordering::Relaxed);
    0
}
export_gpl!(rp1_pio_sm_put);

/// `int rp1_pio_sm_get(PIO client, void *param)`.
///
/// # Safety
///
/// `param` is a `struct rp1_pio_sm_get_args`.
#[no_mangle]
pub unsafe extern "C" fn rp1_pio_sm_get(_client: *mut c_void, param: *mut c_void) -> c_int {
    let data = LAST_PUT.load(Ordering::Relaxed).wrapping_add(1) as u32;
    // SAFETY: the caller's: the argument block of this operation.
    unsafe { (*param.cast::<lb::rp1_pio_sm_get_args>()).data = data };
    CALLS.fetch_add(1, Ordering::Relaxed);
    0
}
export_gpl!(rp1_pio_sm_get);

struct PioMock;

impl kernel::Module for PioMock {
    fn init(_module: &'static ThisModule) -> Result<Self> {
        Ok(PioMock)
    }
}

module! {
    type: PioMock,
    name: "rp1_pio_mock",
    authors: ["Lemnos"],
    description: "A stand-in for the RP1 PIO driver, for testing the Lemnos module",
    license: "GPL",
}
