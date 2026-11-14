//! The driver model. A driver states what it needs and which contracts it
//! provides, receives its resources when it binds, and describes its device.
//!
//! The engine matches tags as strings and carries method numbers and bytes. It
//! has no idea what any of them mean.
//!
//! Works with no allocator. A resource a driver holds is a [`Held`], the same type either way:
//! boxed by a host that has an allocator (feature `alloc`), or lent from a `static` place it goes
//! back to when the driver is dropped.

#![no_std]

#[cfg(feature = "alloc")]
extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

mod codec;
mod config;
mod contract;
mod description;
mod driver;
mod fixed;
mod held;
pub mod layout;
#[cfg(feature = "std")]
pub mod package;
mod resources;
mod tag;

pub use codec::{Decode, Encode, Field, Reader, Shape, Tail, Vector3, Writer};
pub use config::{Config, ConfigError, ConfigValue, Key, Kind};
pub use contract::{
    Bound, Caller, Contract, ContractInfo, Dev, LOCAL_ARGS, LocalCaller, MethodInfo, Param, Polled,
    REPLY_INLINE, Router,
};
pub use description::{
    Description, Item, ItemKind, MAX_ITEMS, MAX_LOCATION, Reading, Unit, UnitState,
};
pub use driver::{
    BoardDevice, BoardView, CallerId, Driver, Health, HostClock, HostOwned, JobId, JobState,
    Lifecycle, Manifest, MethodId, Origin, Reply, Retry,
};
#[cfg(feature = "float")]
pub use fixed::to_f32;
pub use fixed::{NO_VALUE, rescale};
pub use lemnos_abi::Status;
pub use lemnos_hal::ErrorKind;
pub use lemnos_hal::blocks::BufferHandle;
pub use lemnos_hal::register::AddressWidth;
pub use lemnos_macros::{Decode, Encode, contract, driver};
pub use resources::{
    BusKind, ClocksHandle, DmaHandle, Held, Hint, HintSource, Home, I2c, InterruptHandle, Line,
    Lock, Need, NeedKind, Pio, Provider, Pwm, RawSpiBus, RegistersHandle, ResetHandle, Resource,
    Resources, Spi, StreamHandle, StreamKind, Uart, UsbDeviceHandle, UsbFunctionHandle, Watchdog,
    checked_stream,
};
pub use tag::Tag;

/// What every driver call returns.
pub type Result<T> = core::result::Result<T, Status>;

/// The status a hardware error is reported as.
pub const fn status_of(kind: ErrorKind) -> Status {
    match kind {
        ErrorKind::NotFound => Status::NOT_FOUND,
        ErrorKind::Unavailable => Status::NO_DEVICE,
        ErrorKind::Busy => Status::BUSY,
        ErrorKind::PermissionDenied => Status::ACCESS,
        ErrorKind::Timeout => Status::TIMED_OUT,
        ErrorKind::InvalidInput | ErrorKind::Configuration => Status::INVALID_ARGUMENT,
        ErrorKind::Unsupported => Status::NOT_SUPPORTED,
        _ => Status::IO,
    }
}

#[cfg(test)]
mod tests;
