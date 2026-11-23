//! The words a refusal carries ([`lemnos_core::Lifecycle::refusal`]) for each cause its status
//! alone can't tell apart from another. Stable: clients and tests match them.

/// `set_default` or `trial` NOT_PERMITTED: the slot failed a trial; `force` overrides it.
pub const FAILED_TRIAL: &str = "the slot failed a trial (force overrides this)";
/// `set_default` NOT_PERMITTED: the slot never booted good; `force` does not override it.
pub const NEVER_GOOD: &str = "the slot has never booted good (force does not override this)";
/// `trial` BUSY: the slot is the running one.
pub const TRIAL_RUNNING: &str = "the slot is the one running";
/// `remove` BUSY, one per cause.
pub const REMOVE_RUNNING: &str = "the slot is the one running";
pub const REMOVE_DEFAULT: &str = "the slot is the default";
pub const REMOVE_TRIAL: &str = "the slot is on trial";
pub const REMOVE_ONLY: &str = "the slot is the only one that can boot";
/// `remove` NOT_PERMITTED: the slot is pinned.
pub const REMOVE_PINNED: &str = "the slot is pinned (unpin it first)";
/// `install_file` BUSY: another install runs.
pub const INSTALL_RUNNING: &str = "another install is running";
/// `install_file` INVALID_ARGUMENT, one per cause.
pub const BAD_NAME: &str =
    "the name is not one plain file name of letters, digits, '.', '_' and '-'";
pub const NOT_A_FILE: &str = "the name in .incoming is not a regular file";
pub const A_LINK: &str = "the name in .incoming is a link, which is never followed";
/// `install_file` NOT_FOUND.
pub const NOT_IN_INCOMING: &str = "no file of that name is in .incoming";
/// FAILED_PRECONDITION from any write while the state dirs can't be written; the reason follows.
pub const STATE_UNWRITABLE: &str = "the slots state can't be written";
/// `trial` NOT_PERMITTED: the board's loader can't start a trial now; the reason follows.
pub const NO_TRIALS: &str = "the board can't start a trial now";
/// `trial` IO: the restart the trial needs is refused; the reason follows.
pub const RESTART_REFUSED: &str = "the restart the trial needs is refused";
