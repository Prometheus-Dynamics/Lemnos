//! The service's environment.

use std::path::PathBuf;

use orion::Address;

/// lemnosd's socket, as `lemnos-ctl` finds it.
pub const DEFAULT_LEMNOSD_SOCKET: &str = "/run/lemnos/lemnosd.sock";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub lemnosd_socket: PathBuf,
    /// The comm core to publish to: the board's own socket unless `ORION_ADDRESS` says.
    pub orion: Address,
    /// The node the devices must be published on, when `ORION_NODE_ID` names one. A comm core
    /// that welcomes this service as any other node gets nothing.
    pub node_id: Option<String>,
}

impl Config {
    /// Reads `LEMNOSD_SOCKET`, `ORION_ADDRESS` and `ORION_NODE_ID`.
    ///
    /// # Errors
    ///
    /// When `ORION_ADDRESS` is not an address.
    pub fn from_env() -> Result<Self, String> {
        Ok(Self {
            lemnosd_socket: std::env::var_os("LEMNOSD_SOCKET")
                .map_or_else(|| PathBuf::from(DEFAULT_LEMNOSD_SOCKET), PathBuf::from),
            orion: address(std::env::var("ORION_ADDRESS").ok().as_deref())?,
            node_id: node_id(std::env::var("ORION_NODE_ID").ok().as_deref()),
        })
    }
}

/// `ORION_ADDRESS` as an address. Unset or empty means the board's own comm core.
///
/// # Errors
///
/// When it is set and is not an address.
pub fn address(value: Option<&str>) -> Result<Address, String> {
    match value {
        Some(text) if !text.is_empty() => text
            .parse()
            .map_err(|error| format!("ORION_ADDRESS {text:?}: {error}")),
        _ => Ok(Address::default()),
    }
}

/// `ORION_NODE_ID` as the node to publish on. Unset or empty means whichever node the comm
/// core is.
pub fn node_id(value: Option<&str>) -> Option<String> {
    value.filter(|id| !id.is_empty()).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_or_empty_node_id_is_any_node() {
        assert_eq!(node_id(None), None);
        assert_eq!(node_id(Some("")), None);
    }

    #[test]
    fn a_set_node_id_is_used_as_given() {
        assert_eq!(node_id(Some("node-a")).as_deref(), Some("node-a"));
        assert_eq!(node_id(Some(" node-a")).as_deref(), Some(" node-a"));
    }

    #[test]
    fn an_unset_address_is_the_boards_own_socket() {
        assert_eq!(address(None), Ok(Address::default()));
        assert_eq!(address(Some("")), Ok(Address::default()));
        assert_eq!(
            address(Some("/tmp/o.sock")),
            Ok(Address::Local(PathBuf::from("/tmp/o.sock")))
        );
        assert!(address(Some("nope://x")).is_err());
    }
}
