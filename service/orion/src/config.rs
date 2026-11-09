//! The service's environment. Orion's own socket variables are read by Orion's
//! client, so they follow the same rules as the rest of the node.

use std::path::PathBuf;

/// lemnosd's socket, as `lemnos-ctl` finds it.
pub const DEFAULT_LEMNOSD_SOCKET: &str = "/run/lemnos/lemnosd.sock";
/// The node Orion runs as when `ORION_NODE_ID` is not set. Orion uses the same default.
pub const DEFAULT_NODE_ID: &str = "node.local";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub lemnosd_socket: PathBuf,
    pub orion_socket: PathBuf,
    pub orion_stream: PathBuf,
    /// The node the devices are published on. Orion's session welcome must name this node.
    pub node_id: String,
}

impl Config {
    /// Reads `LEMNOSD_SOCKET`, `ORION_NODE_ID` and Orion's socket variables.
    pub fn from_env() -> Self {
        Self {
            lemnosd_socket: std::env::var_os("LEMNOSD_SOCKET")
                .map_or_else(|| PathBuf::from(DEFAULT_LEMNOSD_SOCKET), PathBuf::from),
            orion_socket: orion_client::default_ipc_socket_path(),
            orion_stream: orion_client::default_ipc_stream_socket_path(),
            node_id: node_id(std::env::var("ORION_NODE_ID").ok().as_deref()),
        }
    }
}

/// `ORION_NODE_ID` as the node id. Unset or empty means [`DEFAULT_NODE_ID`].
pub fn node_id(value: Option<&str>) -> String {
    match value {
        Some(id) if !id.is_empty() => id.to_owned(),
        _ => DEFAULT_NODE_ID.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_or_empty_node_id_is_the_nodes_default() {
        assert_eq!(node_id(None), DEFAULT_NODE_ID);
        assert_eq!(node_id(Some("")), DEFAULT_NODE_ID);
    }

    #[test]
    fn a_set_node_id_is_used_as_given() {
        assert_eq!(node_id(Some("node-a")), "node-a");
        assert_eq!(node_id(Some(" node-a")), " node-a");
    }
}
