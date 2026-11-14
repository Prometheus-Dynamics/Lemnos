//! The comm core, in a process of its own in the guest: a USB link for each name given and the
//! board socket the bridge connects to. It is the only place this check builds a comm core, so a
//! move of the pinned Orion (for instance one that takes END as a third descriptor) is an edit here.

use std::path::Path;
use std::rc::Rc;

use orion_hello::Hello as HelloService;
use orion_keys::{Keys, KeysConfig, SharedKeys};
use orion_link_serial::StreamCarrier;
use orion_link_usb::UsbLinks;
use orion_server::{Comm as CommCore, Limits, Security};
use orion_unix::UnixCarrier;

/// The node's name: what a host session reads in its welcome.
pub const NODE: &str = "board";

/// Serves forever: the comm core named [`NODE`] with the USB links `links`, on the socket `socket`.
pub fn run(socket: &Path, links: &[String]) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("the comm core's runtime");
    tokio::task::LocalSet::new().block_on(&runtime, async move {
        let comm = comm_core(links);
        let unix = UnixCarrier::bind(socket, Some(0o600), &[]).expect("the comm core's socket");
        let serving = comm.clone();
        tokio::task::spawn_local(async move { serving.serve(unix).await });
        std::future::pending::<()>().await;
    });
}

/// The comm core with its services and one carrier per USB link, serving. Everything that depends
/// on the pinned Orion's shape is here.
fn comm_core(links: &[String]) -> CommCore {
    let keys = SharedKeys::new(
        Keys::open(KeysConfig::in_memory("lab"), Security::Off).expect("the comm core's keys"),
    );
    let comm = CommCore::new(
        NODE.parse().expect("the node's name"),
        Security::Off,
        Some(Rc::new(keys.clone())),
        Limits::default(),
    );
    comm.add_service(HelloService::new(
        NODE.parse().expect("the node's name"),
        keys.clone(),
        None,
    ));
    let usb = UsbLinks::new();
    comm.add_service(usb.service());
    for name in links {
        let carrier = StreamCarrier::new(name.parse().expect("the link's name"), usb.link(name));
        let serving = comm.clone();
        tokio::task::spawn_local(async move { serving.serve(carrier).await });
    }
    comm
}
