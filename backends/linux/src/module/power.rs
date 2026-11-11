//! A block's clocks and reset lines, through the kernel module: [`ModuleClocks`] and
//! [`ModuleReset`].
//!
//! The module finds each by the name the block's firmware description gives it and keeps the
//! handle, so after the first use nothing allocates, here or there. What is turned on stays on
//! across a close; the module turns it off when the block is unbound.

use std::io;
use std::os::fd::OwnedFd;

use lemnos_hal::ErrorKind;
use lemnos_hal::blocks::{Clocks, Reset};
use lemnos_linux_sys::module::{self as sys, power};

use super::Node;
use crate::kind_of;

/// A block's clocks or resets: the names, in index order, each looked up and held under a claim.
struct Named {
    node: Node,
    block: u32,
    kind: u32,
    names: Vec<String>,
    _claims: Vec<OwnedFd>,
}

impl Named {
    /// Looks each name up with the module now, so a name the block doesn't have is refused at
    /// bind (`NotFound`) and not on first use.
    fn open(node: &Node, name: &str, kind: u32, names: &[&str]) -> Result<Self, ErrorKind> {
        let (block, _) = super::block::find(node, name)?;
        let what = if kind == power::CLOCK {
            "clock"
        } else {
            "reset"
        };
        let mut claims = Vec::with_capacity(names.len());
        for name in names {
            claims.push(
                node.claim(format_args!("{what}:{block}/{name}"))
                    .map_err(|e| kind_of(&e))?,
            );
            let looked_up = sys::power(node.fd(), block, (kind, power::STATUS), name, (0, 0));
            found(looked_up)?;
        }
        Ok(Self {
            node: node.clone(),
            block,
            kind,
            names: names.iter().map(|name| (*name).to_string()).collect(),
            _claims: claims,
        })
    }

    fn call(&self, index: u32, op: u32, value: u64) -> Result<u64, ErrorKind> {
        let name = usize::try_from(index)
            .ok()
            .and_then(|i| self.names.get(i))
            .ok_or(ErrorKind::NotFound)?;
        sys::power(
            self.node.fd(),
            self.block,
            (self.kind, op),
            name,
            (value, 0),
        )
        .map_err(|e| kind_of(&e))
    }
}

/// What looking a clock or reset up came to. A controller that can't report a status still has
/// the line, so `EOPNOTSUPP` there is found.
fn found(looked_up: io::Result<u64>) -> Result<(), ErrorKind> {
    match looked_up {
        Ok(_) => Ok(()),
        Err(e) if e.raw_os_error() == Some(libc::EOPNOTSUPP) => Ok(()),
        Err(e) => Err(kind_of(&e)),
    }
}

/// The clocks of a block bound to the module (`clocks:<block>/<a>,<b>`), index 0 first.
pub struct ModuleClocks(Named);

impl ModuleClocks {
    pub(crate) fn open(node: &Node, name: &str, names: &[&str]) -> Result<Self, ErrorKind> {
        Named::open(node, name, power::CLOCK, names).map(Self)
    }

    /// Whether clock `clock` is on, as this module turned it on.
    pub fn is_on(&mut self, clock: u32) -> Result<bool, ErrorKind> {
        Ok(self.0.call(clock, power::STATUS, 0)? != 0)
    }
}

impl Clocks for ModuleClocks {
    type Error = ErrorKind;

    fn enable(&mut self, clock: u32) -> Result<(), ErrorKind> {
        self.0.call(clock, power::ENABLE, 0).map(drop)
    }

    fn disable(&mut self, clock: u32) -> Result<(), ErrorKind> {
        self.0.call(clock, power::DISABLE, 0).map(drop)
    }

    fn rate_hz(&mut self, clock: u32) -> Result<u64, ErrorKind> {
        self.0.call(clock, power::GET, 0)
    }

    fn set_rate_hz(&mut self, clock: u32, hz: u64) -> Result<u64, ErrorKind> {
        self.0.call(clock, power::SET, hz)
    }
}

/// The reset lines of a block bound to the module (`resets:<block>/<a>,<b>`), index 0 first.
pub struct ModuleReset(Named);

impl ModuleReset {
    pub(crate) fn open(node: &Node, name: &str, names: &[&str]) -> Result<Self, ErrorKind> {
        Named::open(node, name, power::RESET, names).map(Self)
    }

    /// Whether line `line` is asserted, as its controller reports it. `Unsupported` for a
    /// controller that can't say.
    pub fn is_asserted(&mut self, line: u32) -> Result<bool, ErrorKind> {
        Ok(self.0.call(line, power::STATUS, 0)? != 0)
    }

    /// Asserts line `line` and releases it, as its controller does that.
    pub fn pulse(&mut self, line: u32) -> Result<(), ErrorKind> {
        self.0.call(line, power::PULSE, 0).map(drop)
    }
}

impl Reset for ModuleReset {
    type Error = ErrorKind;

    fn assert(&mut self, line: u32) -> Result<(), ErrorKind> {
        self.0.call(line, power::DISABLE, 0).map(drop)
    }

    fn deassert(&mut self, line: u32) -> Result<(), ErrorKind> {
        self.0.call(line, power::ENABLE, 0).map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_whose_status_cant_be_read_is_still_found() {
        assert_eq!(found(Ok(0)), Ok(()));
        assert_eq!(
            found(Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP))),
            Ok(())
        );
        assert_eq!(
            found(Err(io::Error::from_raw_os_error(libc::ENOENT))),
            Err(ErrorKind::NotFound)
        );
        assert_eq!(
            found(Err(io::Error::from_raw_os_error(libc::ENODEV))),
            Err(ErrorKind::Unavailable)
        );
    }
}
