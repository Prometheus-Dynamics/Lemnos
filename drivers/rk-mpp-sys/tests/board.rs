//! The first check on a board (`testing/board-runs/E16.md`, check 2a): open the service node,
//! read which client types are present, and for each block the driver takes, whether it is
//! confined, then bind a fresh session to it and read its hardware id.
//!
//! Ignored, and it opens nothing unless `LEMNOS_MPP_NODE` names the node:
//!
//! ```sh
//! LEMNOS_MPP_NODE=/dev/mpp_service cargo test -p lemnos-driver-rk-mpp-sys --test board \
//!     -- --ignored --nocapture
//! ```

#![cfg(target_os = "linux")]

use std::io::Write;

use lemnos_driver_rk_mpp::Block;
use lemnos_driver_rk_mpp_sys::{ServiceNode, ServiceSession, driver_name};

#[test]
#[ignore = "needs a board: set LEMNOS_MPP_NODE to the service node"]
fn open_bind_and_query_on_a_board() {
    let Some(path) = std::env::var_os("LEMNOS_MPP_NODE") else {
        return;
    };
    let node = ServiceNode::new(path);
    let mut out = std::io::stdout().lock();
    let mut session = ServiceSession::open(node.node(), &Block::ALL).expect("open the node");
    let support = session.hw_support().expect("MPP_CMD_QUERY_HW_SUPPORT");
    writeln!(out, "hw_support = {support:#010x}").unwrap();
    drop(session);

    let mut bound = 0;
    for block in Block::ALL {
        let present = support & (1 << block.facts().client) != 0;
        let confined = node.is_confined(block);
        write!(
            out,
            "{block:?} ({}): present={present} confined={confined:?}",
            driver_name(block)
        )
        .unwrap();
        if !present {
            writeln!(out).unwrap();
            continue;
        }
        let mut session = ServiceSession::open(node.node(), &[block]).expect("open the node");
        match session.bind(block).and_then(|()| session.hw_id()) {
            Ok(id) => {
                bound += 1;
                writeln!(out, " hw_id={id:#010x}").unwrap();
            }
            Err(e) => writeln!(out, " bind/hw_id failed: {e}").unwrap(),
        }
        session.reset().expect("MPP_CMD_RESET_SESSION");
    }
    assert!(bound > 0, "no block the driver takes could be bound");
}
