//! The root phase of the bridge: when it starts as root, it drops to the identity its environment
//! names (`LEMNOS_ORION_USER`, `LEMNOS_ORION_GROUP`, `LEMNOS_ORION_GROUPS`), the same drop
//! `lemnosd` makes, and verifies it. It keeps no capability: the bridge needs none.

use lemnos_linux_sys::privilege::{self, Plan};

use crate::config::RunAs;

/// Drops to `run_as`, then checks the process is the user, groups and capability state asked for.
///
/// # Errors
///
/// A name that does not resolve, a drop that fails, or a state after the drop that is not the one
/// asked for. Each says why, and the caller stops.
pub fn drop_as(run_as: &RunAs) -> Result<(), String> {
    let (uid, gid) = privilege::resolve(&run_as.user, run_as.group.as_deref())?;
    let groups = run_as
        .groups
        .iter()
        .map(|group| privilege::resolve_group(group))
        .collect::<Result<Vec<u32>, String>>()?;
    let plan = Plan {
        uid,
        gid,
        groups,
        keep: Vec::new(),
    };
    privilege::drop_to(&plan).map_err(|error| format!("dropping to {uid}:{gid}: {error}"))?;
    let now = privilege::observe().map_err(|error| format!("after the drop: {error}"))?;
    privilege::verify(&plan, &now).map_err(|reason| format!("refusing to run: {reason}"))
}
