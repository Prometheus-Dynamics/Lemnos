//! Which devices take files pushed through Orion, and what a board that gave the bridge no place
//! for one's files says about it.
//!
//! A device takes files when its description has the files protocol's own methods: `accept` (a
//! file into a directory, which the device then installs), or `manifest`, `write`, `data` and
//! `info` together (a file fed to the device's long job as it arrives). Nothing here knows what
//! the device does with the file. The bridge takes files for a device only when
//! `LEMNOS_ORION_FILES` names it. One the board did not name is said once in the log by its name,
//! its status topic's `pushes` says so, and a push to `lemnos/<device>/…` is refused with that
//! reason at its start: nothing is written and nothing is kept by the node.

use lemnos_client::{Client, DeviceDescription};

use crate::config::Config;

/// `pushes` for a device the bridge takes files for, while its file sink is Orion's: a push to
/// `lemnos/<device>/…` reaches the bridge.
pub const ACCEPTED: &str = "accepted";

/// `pushes` for a device the bridge takes files for, while its file sink is not Orion's: from the
/// bridge's start until the sink is accepted, and again from the moment its files session is lost
/// until the next sink is accepted. A push sent now reaches no receiver, so a sender waits for
/// [`ACCEPTED`]. It is published as soon as the device's status topic is declared, so a
/// subscriber held across a bridge restart reads it in place of the last bridge's `accepted`.
pub const NOT_READY: &str = "not ready: the bridge is not receiving this device's files yet";

/// Whether a device described as `description` takes files ([`lemnos_wire::text::takes_files`]).
pub fn takes_files(description: &DeviceDescription) -> bool {
    lemnos_wire::text::takes_files(|name| {
        description
            .contracts
            .iter()
            .any(|contract| contract.methods.iter().any(|method| method.name == name))
    })
}

/// Why a push to `device` is refused on this board: the bridge has no place for its files.
pub fn refused_why(device: &str) -> String {
    format!(
        "{device} takes no pushes on this board: the bridge has no LEMNOS_ORION_FILES entry for it"
    )
}

/// What `pushes` says for a device the bridge has no place for.
pub fn refused_text(device: &str) -> String {
    format!("refused: {}", refused_why(device))
}

/// The devices lemnosd serves that take files and that `config` gives no place to, by name.
///
/// # Errors
///
/// When lemnosd can't list its devices or describe one, in words.
pub fn unplaced(client: &Client, config: &Config) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for entry in client.list().map_err(|e| format!("lemnosd: {e}"))? {
        if config.files.iter().any(|(device, _)| *device == entry.name) {
            continue;
        }
        let description = client
            .describe(entry.index)
            .map_err(|e| format!("lemnosd: describing {}: {e}", entry.name))?;
        if takes_files(&description) {
            out.push(entry.name);
        }
    }
    Ok(out)
}

/// The lines the bridge logs once at start, one per device in `unplaced`, by name.
pub fn notices(unplaced: &[String]) -> Vec<String> {
    unplaced
        .iter()
        .map(|device| format!("files: {}", refused_why(device)))
        .collect()
}
