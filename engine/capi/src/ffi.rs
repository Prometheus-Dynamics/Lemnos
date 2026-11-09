//! The surface as the wrapper calls it: this crate's own exports in a static
//! build, and with the `shared` feature the installed `liblemnos_capi.so.1`.
//! A process holds the surface once.

#[cfg(not(feature = "shared"))]
pub(crate) use crate::surface::{
    lemnos_call, lemnos_check_version, lemnos_client_close, lemnos_client_open,
    lemnos_device_count, lemnos_device_text, lemnos_next_value, lemnos_open, lemnos_subscribe,
};

#[cfg(feature = "shared")]
pub(crate) use shared::*;

#[cfg(feature = "shared")]
mod shared {
    use core::ffi::c_char;

    use lemnos_abi::{AbiVersion, Handle, MessageSink, Status};

    use crate::surface::{Subscription, Value};

    // The declarations of include/lemnos.h.
    #[link(name = "liblemnos_capi.so.1", kind = "dylib", modifiers = "+verbatim")]
    unsafe extern "C" {
        pub(crate) unsafe fn lemnos_check_version(caller: *const AbiVersion) -> Status;
        pub(crate) unsafe fn lemnos_client_open(
            socket: *const c_char,
            name: *const c_char,
            timeout_ms: u32,
            client: *mut Handle,
            sink: MessageSink,
        ) -> Status;
        pub(crate) safe fn lemnos_client_close(client: Handle) -> Status;
        pub(crate) safe fn lemnos_device_count(client: Handle) -> Status;
        pub(crate) unsafe fn lemnos_device_text(
            client: Handle,
            device: u32,
            field: u32,
            out: *mut c_char,
            capacity: usize,
            len: *mut usize,
        ) -> Status;
        pub(crate) unsafe fn lemnos_open(
            client: Handle,
            device: *const c_char,
            tag: *const c_char,
            opened: *mut u32,
            sink: MessageSink,
        ) -> Status;
        #[allow(clippy::too_many_arguments)]
        pub(crate) unsafe fn lemnos_call(
            client: Handle,
            opened: u32,
            method: u16,
            args: *const u8,
            args_len: usize,
            reply: *mut u8,
            reply_capacity: usize,
            sink: MessageSink,
        ) -> Status;
        pub(crate) unsafe fn lemnos_subscribe(
            client: Handle,
            opened: u32,
            method: u16,
            period_us: u64,
            only_changes: u32,
            subscription: *mut Subscription,
            sink: MessageSink,
        ) -> Status;
        pub(crate) unsafe fn lemnos_next_value(
            client: Handle,
            timeout_ms: u32,
            value: *mut Value,
            into: *mut u8,
            capacity: usize,
            sink: MessageSink,
        ) -> Status;
    }
}
