//! The surface as the wrapper calls it: this crate's own exports in a static
//! build, and with the `shared` feature the installed `liblemnos_capi.so.1`.
//! A process holds the surface once.

#[cfg(not(feature = "shared"))]
pub(crate) use crate::surface::{
    lemnos_call, lemnos_check_version, lemnos_client_close, lemnos_client_open,
    lemnos_device_count, lemnos_device_text, lemnos_next_value, lemnos_open, lemnos_package_info,
    lemnos_package_list, lemnos_package_text, lemnos_stream_buffer_handle, lemnos_stream_close,
    lemnos_stream_end, lemnos_stream_generation, lemnos_stream_info, lemnos_stream_next_frame,
    lemnos_stream_open, lemnos_stream_plane, lemnos_stream_plane_handle, lemnos_stream_plane_len,
    lemnos_stream_pool_page, lemnos_stream_post_job, lemnos_stream_producer, lemnos_stream_read,
    lemnos_stream_region, lemnos_stream_release, lemnos_stream_skipped, lemnos_stream_write,
    lemnos_subscribe,
};

#[cfg(feature = "shared")]
pub(crate) use shared::*;

#[cfg(feature = "shared")]
mod shared {
    use core::ffi::c_char;

    use lemnos_abi::{AbiVersion, Handle, MessageSink, Status};

    use crate::surface::{Frame, PackageInfo, StreamInfo, Subscription, Value};

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
        pub(crate) unsafe fn lemnos_package_list(
            client: Handle,
            mode: *mut u32,
            sink: MessageSink,
        ) -> Status;
        pub(crate) unsafe fn lemnos_package_info(
            client: Handle,
            package: u32,
            info: *mut PackageInfo,
        ) -> Status;
        pub(crate) unsafe fn lemnos_package_text(
            client: Handle,
            package: u32,
            field: u32,
            out: *mut c_char,
            capacity: usize,
            len: *mut usize,
        ) -> Status;
        pub(crate) unsafe fn lemnos_stream_open(
            client: Handle,
            device: u32,
            name: *const c_char,
            flags: u32,
            stream: *mut Handle,
            sink: MessageSink,
        ) -> Status;
        pub(crate) unsafe fn lemnos_stream_info(
            client: Handle,
            stream: Handle,
            info: *mut StreamInfo,
        ) -> Status;
        pub(crate) unsafe fn lemnos_stream_next_frame(
            client: Handle,
            stream: Handle,
            timeout_ms: u32,
            frame: *mut Frame,
            data: *mut *const u8,
            sink: MessageSink,
        ) -> Status;
        pub(crate) safe fn lemnos_stream_release(
            client: Handle,
            stream: Handle,
            buffer: u32,
            sink: MessageSink,
        ) -> Status;
        pub(crate) unsafe fn lemnos_stream_read(
            client: Handle,
            stream: Handle,
            buffer: u32,
            offset: usize,
            out: *mut u8,
            len: usize,
            sink: MessageSink,
        ) -> Status;
        pub(crate) unsafe fn lemnos_stream_write(
            client: Handle,
            stream: Handle,
            buffer: u32,
            offset: usize,
            data: *const u8,
            len: usize,
            sink: MessageSink,
        ) -> Status;
        pub(crate) unsafe fn lemnos_stream_post_job(
            client: Handle,
            stream: Handle,
            job: *const u8,
            len: usize,
            sink: MessageSink,
        ) -> Status;
        pub(crate) unsafe fn lemnos_stream_buffer_handle(
            client: Handle,
            stream: Handle,
            buffer: u32,
            fd: *mut i32,
            sink: MessageSink,
        ) -> Status;
        pub(crate) unsafe fn lemnos_stream_plane(
            client: Handle,
            stream: Handle,
            buffer: u32,
            plane: u32,
            data: *mut *const u8,
            len: *mut usize,
        ) -> Status;
        pub(crate) unsafe fn lemnos_stream_plane_len(
            client: Handle,
            stream: Handle,
            buffer: u32,
            plane: u32,
            len: *mut usize,
        ) -> Status;
        pub(crate) unsafe fn lemnos_stream_plane_handle(
            client: Handle,
            stream: Handle,
            buffer: u32,
            plane: u32,
            fd: *mut i32,
            sink: MessageSink,
        ) -> Status;
        pub(crate) unsafe fn lemnos_stream_region(
            client: Handle,
            stream: Handle,
            buffer: u32,
            plane: u32,
            region: *mut crate::surface::StreamRegion,
            sink: MessageSink,
        ) -> Status;
        pub(crate) unsafe fn lemnos_stream_pool_page(
            client: Handle,
            stream: Handle,
            fd: *mut i32,
            sink: MessageSink,
        ) -> Status;
        pub(crate) unsafe fn lemnos_stream_generation(
            client: Handle,
            stream: Handle,
            generation: *mut u32,
        ) -> Status;
        pub(crate) unsafe fn lemnos_stream_end(
            client: Handle,
            stream: Handle,
            kind: *mut u32,
        ) -> Status;
        pub(crate) unsafe fn lemnos_stream_producer(
            client: Handle,
            stream: Handle,
            state: *mut u32,
            since_ns: *mut u64,
        ) -> Status;
        pub(crate) unsafe fn lemnos_stream_skipped(
            client: Handle,
            stream: Handle,
            skipped: *mut u64,
        ) -> Status;
        pub(crate) safe fn lemnos_stream_close(
            client: Handle,
            stream: Handle,
            sink: MessageSink,
        ) -> Status;
    }
}
