// SPDX-License-Identifier: GPL-2.0
//! The module entry of lemnos.ko, Rust side (stage 1a of the port, docs/kernel-module.md).
//!
//! The Rust half owns `module_init` and `module_exit`, the module's name, description, license and
//! imports, and the registrations of the module's platform driver and its `/dev/lemnos` node. The
//! csi2 receiver and the descriptors the registrations take (`lem_driver`, `lem_misc`) are
//! still C in this stage: lemnos_core.c exports them with the Rust owner. Built only with
//! CONFIG_RUST; without it lemnos_core.c keeps `module_init`, as before.
//!
//! The order and the unwinding are those of the C `lem_init` and `lem_exit` it replaces.

use core::ptr::addr_of;
// The Rust half is one crate: the ports are its modules (kernel/rs/), and the ABI helpers are shared (rs/abi.rs).
#[path = "rs/abi.rs"]
mod abi;
#[path = "rs/claims.rs"]
mod claims;
#[path = "rs/pwm.rs"]
mod pwm;
#[path = "rs/gpio.rs"]
mod gpio;
#[path = "rs/power.rs"]
mod power;
#[path = "rs/irq.rs"]
mod irq;
#[path = "rs/bindings.rs"]
mod bindings;
#[path = "rs/bus.rs"]
mod bus;
#[path = "rs/events.rs"]
mod events;
#[path = "rs/param.rs"]
mod param;
#[path = "rs/regjob.rs"]
mod regjob;
#[path = "rs/regjob_profile.rs"]
mod regjob_profile;
#[path = "rs/regjob_ve.rs"]
mod regjob_ve;
#[path = "rs/regjob_isp.rs"]
mod regjob_isp;
#[path = "rs/pio.rs"]
mod pio;
#[path = "rs/stream.rs"]
mod stream;
#[path = "rs/stream_ring.rs"]
mod stream_ring;
#[path = "rs/stream_frame.rs"]
mod stream_frame;
#[path = "rs/stream_producer.rs"]
mod stream_producer;
#[path = "rs/stream_region.rs"]
mod stream_region;
#[path = "rs/stream_reader.rs"]
mod stream_reader;
#[path = "rs/stream_file.rs"]
mod stream_file;
#[path = "rs/gsdma.rs"]
mod gsdma;
#[path = "rs/vip.rs"]
mod vip;
#[path = "rs/pisp_fe.rs"]
mod pisp_fe;
#[path = "rs/rp1_csi2.rs"]
mod rp1_csi2;

use kernel::{error::to_result, ffi::{c_char, c_int, c_void}, prelude::*};

// Opaque: the C descriptors are passed by address only. Their types are struct platform_driver and
// struct miscdevice, which this file never reads.
unsafe extern "C" {
    static lem_driver: [u8; 0];
    static lem_misc: [u8; 0];
    /// `platform_driver_register` is a macro over this, with THIS_MODULE and KBUILD_MODNAME.
    fn __platform_driver_register(drv: *mut c_void, owner: *mut c_void, name: *const c_char) -> c_int;
    fn platform_driver_unregister(drv: *mut c_void);
    fn misc_register(misc: *mut c_void) -> c_int;
    fn misc_deregister(misc: *mut c_void);
    /// rs/claims.rs: the claim table and the claim files' owner. Built with CONFIG_RUST.
    fn lem_claims_init(owner: *mut c_void);
}

struct LemnosEntry;

impl kernel::Module for LemnosEntry {
    fn init(module: &'static ThisModule) -> Result<Self> {
        // The event queue and the watches' locks (rs/events.rs), first: binding a block posts an event.
        events::lem_events_init();
        // The table of streams (rs/stream_file.rs), before any block can be bound.
        stream_file::init();
        // SAFETY: called once, before any block can be bound or any interrupt requested.
        unsafe { irq::lem_irq_setup() };
        // The descriptors are statics the C module defines (LEM_STATIC, lemnos_core.c). Only their
        // addresses are taken; the C side never frees them.
        let (driver, misc) = (addr_of!(lem_driver) as *mut c_void, addr_of!(lem_misc) as *mut c_void);
        // SAFETY: THIS_MODULE is `module`, live for the module's life; the claim table is made once here,
        // before any claim can be made.
        unsafe { lem_claims_init(module.as_ptr().cast()) };
        // The bus glue (rs/bus.rs), initialised once, here; its exit is paired below.
        bus::lem_bus_init(module)?;
        // SAFETY: THIS_MODULE is `module`; the name is the module's, the same string KBUILD_MODNAME gives.
        if let Err(e) = to_result(unsafe {
            __platform_driver_register(driver, module.as_ptr().cast(), c"lemnos".as_ptr().cast())
        }) {
            bus::lem_bus_exit();
            return Err(e);
        }
        // The csi2 receiver's platform driver (rs/rp1_csi2.rs); undone in the error path below and in Drop.
        if let Err(e) = to_result(rp1_csi2::lem_rp1_csi2_register()) {
            // SAFETY: undoes the platform driver, registered above; then the bus.
            unsafe { platform_driver_unregister(driver) };
            bus::lem_bus_exit();
            return Err(e);
        }
        // SAFETY: registers /dev/lemnos from the C descriptor; undone in the error path and in Drop.
        if let Err(e) = to_result(unsafe { misc_register(misc) }) {
            rp1_csi2::lem_rp1_csi2_unregister();
            // SAFETY: undoes the platform driver, registered above; then the bus.
            unsafe { platform_driver_unregister(driver) };
            bus::lem_bus_exit();
            return Err(e);
        }
        Ok(LemnosEntry)
    }
}

impl Drop for LemnosEntry {
    fn drop(&mut self) {
        // The reverse of init, as lem_exit does it.
        let (driver, misc) = (addr_of!(lem_driver) as *mut c_void, addr_of!(lem_misc) as *mut c_void);
        // SAFETY: called once, at unload, after a successful init; undoes misc_register.
        unsafe { misc_deregister(misc) };
        rp1_csi2::lem_rp1_csi2_unregister();
        // SAFETY: as above; undoes the platform driver's registration.
        unsafe { platform_driver_unregister(driver) };
        bus::lem_bus_exit();
    }
}

module! {
    type: LemnosEntry,
    name: "lemnos",
    authors: ["Lemnos"],
    description: "Lemnos: platform blocks for drivers in user space",
    license: "GPL",
    imports_ns: ["DMA_BUF", "PWM"],
}
