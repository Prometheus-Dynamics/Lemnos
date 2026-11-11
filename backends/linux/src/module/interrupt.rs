//! A block's interrupt, through the kernel module: [`ModuleInterrupt`].

use std::os::fd::OwnedFd;
use std::time::Duration;

use lemnos_hal::ErrorKind;
use lemnos_hal::blocks::{Interrupt, InterruptEvent};
use lemnos_linux_sys::module as sys;

use super::Node;
use crate::kind_of;

/// One interrupt of a block bound to the module (`irq:<block>/<n>`).
///
/// The module masks an interrupt from when it fires until [`Interrupt::enable`]: the driver
/// clears the cause in the block's registers, then enables it again. It starts masked. The
/// module has no request that masks it from user space, so [`Interrupt::disable`] stops keeping
/// its events instead: one that fires after it is dropped, and the interrupt then stays masked
/// until the next enable. Waiting allocates nothing.
pub struct ModuleInterrupt {
    node: Node,
    block: u32,
    index: u32,
    _claim: OwnedFd,
}

impl ModuleInterrupt {
    /// Interrupt `index` of the block bound to the module as `name`. `Unavailable` when no such
    /// block is bound, `NotFound` past its interrupts, `Busy` when someone holds it.
    pub(crate) fn open(node: &Node, name: &str, index: u32) -> Result<Self, ErrorKind> {
        let (block, info) = super::block::find(node, name)?;
        if index >= info.irqs {
            return Err(ErrorKind::NotFound);
        }
        let claim = node
            .claim(format_args!("irq:{block}/{index}"))
            .map_err(|e| kind_of(&e))?;
        node.events().register(block, index);
        Ok(Self {
            node: node.clone(),
            block,
            index,
            _claim: claim,
        })
    }

    /// Sets the interrupt pending at its controller, as the block would. Only for testing: the
    /// module refuses it (`PermissionDenied`) unless it was loaded with `test_irq_inject`.
    pub fn inject_for_test(&mut self) -> Result<(), ErrorKind> {
        sys::test_irq(self.node.fd(), self.block, self.index).map_err(|e| kind_of(&e))
    }
}

impl Interrupt for ModuleInterrupt {
    type Error = ErrorKind;

    fn enable(&mut self) -> Result<(), ErrorKind> {
        self.node.events().set_enabled(self.block, self.index, true);
        sys::irq_enable(self.node.fd(), self.block, self.index).map_err(|e| kind_of(&e))
    }

    fn disable(&mut self) -> Result<(), ErrorKind> {
        self.node
            .events()
            .set_enabled(self.block, self.index, false);
        Ok(())
    }

    fn wait(&mut self, timeout: Option<Duration>) -> Result<Option<InterruptEvent>, ErrorKind> {
        self.node
            .events()
            .wait(self.node.fd(), (self.block, self.index), timeout)
    }
}

impl Drop for ModuleInterrupt {
    fn drop(&mut self) {
        self.node.events().unregister(self.block, self.index);
    }
}
