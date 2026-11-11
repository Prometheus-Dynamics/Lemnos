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
//! state says so. Nothing is switched off unless someone calls [`Table::off`]. Drivers retry their
//! own hardware (see each driver). Before a bind, every need that isn't optional must be there, so
//! a bind that is bound to fail for want of another device doesn't take a bus.
//!
//! A bound driver is dropped and bound again, under the same rule, in the two cases the main
//! host does it: when the driver says what it was bound with is dead ([`Health::Gone`], looked at
//! after every call and every tick), and when its calls have failed the way a device that has
//! gone fails (`IO`, `NO_DEVICE`, `TIMED_OUT`) [`Options::error_limit`] times in a row. A driver
//! that goes sooner than the retry cap after each bind waits twice as long each time, up to the
//! cap. Dropping a driver gives back every bus it held ([`Held`]), so the next bind takes them
//! again; devices that use the dropped one keep their [`Dev`], whose calls answer `NO_DEVICE`
//! until it is back.
//!
//! A driver is built in its slot ([`Driver::bind_in`]), not on the stack.
//!
//! There is no panic containment: a panic on a microcontroller ends the firmware.
//!
//! [`Driver::bind`]: lemnos_core::Driver::bind
//! [`Driver::bind_in`]: lemnos_core::Driver::bind_in
//! [`Health::Gone`]: lemnos_core::Health::Gone
//! [`Held`]: lemnos_core::Held
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
