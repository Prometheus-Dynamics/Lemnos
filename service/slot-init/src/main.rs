//! `lemnos-slot-init`: the `/init` of a slot's initramfs. It finds the data partition, opens the
//! slot's EROFS image through dm-verity with the root hash on the kernel command line, and
//! `switch_root`s into it; when the slot can't come up it falls back (see [`decide`]). The
//! layout and the boot records are `lemnos-slot-store`'s, shared with the `system-slots` driver.
//!
//! Built static for the initramfs: `docs/slots.md` has the build and what goes beside it.

mod boot;
mod cmdline;
mod decide;
mod ops;
mod partition;
mod sys;

use std::time::Duration;

use ops::Ops;

fn main() {
    let system = sys::System::new();
    let env = boot::Env::board();
    let why = boot::boot(&system, &env);
    // PID 1 never exits: the kernel would panic. Keep asking for a restart.
    loop {
        system.log(&format!(
            "slot-init: still here after a restart was asked for: {why}"
        ));
        system.sleep(Duration::from_secs(5));
        system.restart(None);
    }
}
