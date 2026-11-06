//! The surface as the wrapper and the tests call it: this crate's own exports
//! ([`crate::surface`]) in a static build, and with the `shared` feature the
//! installed library, `liblemnos_capi.so.1`, linked by the normal dynamic
//! linker (never dlopen). A process holds the surface once: with `shared`,
//! this crate exports nothing and its handle table is the library's.

#[cfg(all(test, not(feature = "shared")))]
pub(crate) use crate::surface::lemnos_version;
#[cfg(not(feature = "shared"))]
pub(crate) use crate::surface::{
    lemnos_channel_info, lemnos_check_version, lemnos_client_close, lemnos_client_open,
    lemnos_device_count, lemnos_device_info, lemnos_next_reading, lemnos_read, lemnos_subscribe,
};

#[cfg(feature = "shared")]
pub(crate) use shared::*;

#[cfg(feature = "shared")]
mod shared {
    use core::ffi::c_char;

    use lemnos_abi::{AbiVersion, Handle, MessageSink, Status};

    use crate::surface::{ChannelInfo, DeviceInfo, RawReading};

    // The declarations of include/lemnos.h. A test checks each against the
    // function in `surface` it stands for (`ffi_matches_the_surface`).
    #[link(name = "liblemnos_capi.so.1", kind = "dylib", modifiers = "+verbatim")]
    unsafe extern "C" {
        // The wrapper uses `lemnos_check_version`; the tests call this one.
        #[cfg_attr(not(test), allow(dead_code))]
        pub(crate) safe fn lemnos_version() -> AbiVersion;
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
        pub(crate) unsafe fn lemnos_device_info(
            client: Handle,
            device: u32,
            info: *mut DeviceInfo,
            id: *mut u8,
            id_capacity: usize,
            id_len: *mut usize,
        ) -> Status;
        pub(crate) unsafe fn lemnos_channel_info(
            client: Handle,
            device: u32,
            channel: u32,
            info: *mut ChannelInfo,
            name: *mut u8,
            name_capacity: usize,
            name_len: *mut usize,
        ) -> Status;
        pub(crate) unsafe fn lemnos_read(
            client: Handle,
            device: u32,
            reading: *mut RawReading,
            sink: MessageSink,
        ) -> Status;
        pub(crate) unsafe fn lemnos_subscribe(
            client: Handle,
            device: u32,
            period_ms: u32,
            granted_ms: *mut u32,
            sink: MessageSink,
        ) -> Status;
        pub(crate) unsafe fn lemnos_next_reading(
            client: Handle,
            timeout_ms: u32,
            device: *mut u32,
            reading: *mut RawReading,
            sink: MessageSink,
        ) -> Status;
    }
}
