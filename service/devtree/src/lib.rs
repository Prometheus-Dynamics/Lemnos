//! A device-tree overlay for a Lemnos board, made from the board file and a SoC map.
//!
//! The board file says which hardware the board has and how each piece is named (`[[hardware]]`,
//! `[[lines]]`). The SoC map (`socs/<soc>.toml`) says, for each controller, which node enables it,
//! the node's path in the base tree, and its pin sets. [`generate`] writes one overlay that enables
//! exactly what the board file names, and disables the base-tree nodes that hold a line the board
//! gives to Lemnos, each with its reason beside it. [`boot_fragment`] writes what must be true
//! before the kernel runs. [`check`] compares a live device tree with both.
//!
//! Nothing here knows a kind of hardware: that is the map's, as data.

pub mod check;
pub mod generate;
pub mod map;
pub mod udev;

pub use check::check;
pub use generate::{Output, generate};
pub use map::{Controller, Form, Kind, SocMap};
