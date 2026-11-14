//! The `system.security/1` contract: the board's security switch, one value per board, off by
//! default. The switch's view and record live in `lemnos-slot-store`'s `security` module.
//!
//! Text replies, one fact per line, so a person and Orion's tooling read the same words:
//!
//! - `state`: `switch on|off`, `generation N`, `view current|pending`, `orion present|absent`,
//!   then `precheck: ok` or one `precheck: missing: ...` / `precheck: busy: ...` line per fact
//!   the last check named.
//! - `precheck`: `ok`, or one `missing: ...` or `busy: ...` line per fact that stops the change.
//!   Ours alone: Orion's side answers its own precheck, and this one never asks Orion. For an off
//!   on a board with a trust root, `ok` is followed by a `note:` line that names the authorization.
//! - `request_off`: the facts an authorization signs (see `docs/slots.md`, "Turning it off").

#![no_std]

use lemnos_core::{Result, contract};

/// The most a reply may say: the caller's buffer for `state` and `precheck` must be this large.
pub const TEXT_MAX: usize = 1024;

#[contract("system.security", version = 1, kind = "system")]
pub trait Security {
    /// The switch as it stands, and what the last precheck said.
    #[method(1)]
    fn state(&mut self, #[out] into: &mut [u8]) -> Result<usize>;

    /// Whether the switch may change to `on`, as the text above. `false` asks only about
    /// work in progress (a flip is never allowed under an install or a trial).
    #[method(2)]
    fn precheck(&mut self, on: bool, #[out] into: &mut [u8]) -> Result<usize>;

    /// Changes the switch: the precheck, Orion's answer when Orion is on the board, the record
    /// written, the view rendered. Refused by name when a fact stops it.
    ///
    /// Turning it off needs `authorization` when the board holds a trust root: a 108-byte signature
    /// record (`LMNSSIG1`) over the message `request_off` describes, signed under a key the board
    /// holds. Without a trust root an off needs none, and one given is ignored. Turning it on
    /// takes no authorization: `authorization` is ignored (empty by convention). Method 3 is the
    /// argument list of `system.security/1` version 1, which is not yet released.
    #[method(3)]
    fn set(&mut self, on: bool, authorization: &[u8]) -> Result<()>;

    /// What an authorization to turn the switch off must sign, from the board: the target, the
    /// generation, the board's serial, the message in hex, and the keys the board holds. One fact
    /// per line; the message is the only thing the signer signs.
    #[method(4)]
    fn request_off(&mut self, #[out] into: &mut [u8]) -> Result<usize>;
}
