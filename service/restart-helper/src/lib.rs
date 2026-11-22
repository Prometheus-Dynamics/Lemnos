//! The restart helper and its protocol.
//!
//! The daemon runs as its own user, which the service manager does not let reboot the board (a
//! reboot request from a user that is not root goes to polkit, which a board image has none of).
//! The helper is a small root service on a Unix socket (`lemnos-restart.socket`): the daemon asks it
//! `check` before it writes anything for a restart and `reboot` to restart; the helper asks the
//! service manager over its private socket, as root, and answers. See `packaging/README.md`, "The
//! restart helper".
//!
//! - [`proto`]: the request and answer lines.
//! - [`serve`]: the helper's side of a connection.
//! - [`client`]: the caller's side.
//! - [`dbus`]: the method calls to the service manager.
//! - `stand_in` (feature `stand-in`): the helper on a socket for other crates' tests.

pub mod client;
pub mod dbus;
pub mod proto;
pub mod serve;
#[cfg(feature = "stand-in")]
pub mod stand_in;
