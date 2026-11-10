//! Runs drivers from a table compiled into the firmware, for a target with no allocator and no
//! threads.
//!
//! The same driver crates `lemnos-host` runs are bound and called here the same way: through
//! [`Driver::bind`] with [`Resources`], and [`Driver::dispatch`] with a method number. A device
//! that uses another gets a [`Dev`] whose calls come straight back into the [`Table`], in the
//! caller's stack frame. Nothing here allocates, spawns or knows what any device is.
//!
//! The firmware declares one [`Slot`] per device and a [`Table`] over them, all `static`:
//!
//! ```ignore
//! static SENSOR: Slot<SomeDriver, Locked<Device<SomeDriver>>> =
//!     Slot::new(Spec::new("sensor").bus("i2c0"), Locked::new(Device::new()));
//! static FUSED: Slot<OtherDriver, Locked<Device<OtherDriver>>> = Slot::new(
//!     Spec::new("fused").uses(&[("input", "sensor")]).config(Config::new(&[("rate", ConfigValue::Int(100))])),
//!     Locked::new(Device::new()),
//! );
//! static TABLE: Table = Table::new(&[&SENSOR, &FUSED], &CLOCK, Options::DEFAULT);
//!
//! TABLE.start(&mut buses); // children first
//! loop {
//!     let next = TABLE.poll(&mut buses); // retries and ticks that are due
//!     TABLE.call(1, method, &args, &mut reply);
//! }
//! ```
//!
//! `Locked` is the firmware's: any [`Lock`], such as a critical section around a `RefCell`. This
//! crate has no `unsafe`, so it can't make a shared cell itself. The buses are any
//! [`Provider`](lemnos_core::Provider), normally `lemnos-mcu`'s.
//!
//! # The failure rule
//!
//! A device that fails to bind is retried, after 1 s doubling to at most 8 s, forever, and its
//! state says so. Nothing is switched off unless someone calls [`Table::off`]. A bound driver is
//! never dropped: without an allocator its buses are `&'static mut`, given once, so a driver that
//! was dropped could not get them back. Drivers retry their own hardware (see each driver).
//! Before a bind, every need that isn't optional must be there, so a bind that is bound to fail
//! for want of another device doesn't use up a bus.
//!
//! There is no panic containment: a panic on a microcontroller ends the firmware.
//!
//! [`Driver::bind`]: lemnos_core::Driver::bind
//! [`Driver::dispatch`]: lemnos_core::Driver::dispatch
//! [`Resources`]: lemnos_core::Resources
//! [`Dev`]: lemnos_core::Dev

#![no_std]

mod lock;
mod slot;
mod table;

pub use lock::Lock;
pub use slot::{Device, Entry, Slot, Spec};
pub use table::{Options, State, Table};
