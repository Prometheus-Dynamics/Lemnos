//! The watchdog on a host, as the daemon runs it: a keep-alive device whose feeds stop when the
//! daemon's liveness says it is not healthy. The node is a plain file, which counts the feeds; the
//! timeout request fails on a file, which the driver reports and feeds through anyway.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use lemnos_driver_hw_watchdog::HwWatchdog;
use lemnos_host::board::parse;
use lemnos_host::{Host, Liveness, Options, entry};
use lemnos_sim::{SimBoard, SimClock};

struct Gate(Arc<AtomicBool>);

impl Liveness for Gate {
    fn alive(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

struct Rig {
    host: Host,
    clock: SimClock,
    alive: Arc<AtomicBool>,
    node: PathBuf,
    root: PathBuf,
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn rig(name: &str) -> Rig {
    let root = std::env::temp_dir().join(format!("hw-watchdog-host-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("dev")).unwrap();
    let node = root.join("dev/watchdog0");
    fs::write(&node, b"").unwrap();
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"dog\"\n\
         [[devices]]\nid = \"watchdog\"\ndriver = \"hw-watchdog\"\nkeepalive = true\n\
         config = {{ device = \"watchdog0\", timeout_s = 15, dev_root = \"{}\", sys_root = \"{}\" }}\n",
        root.join("dev").display(),
        root.join("sys").display(),
    );
    let sim = SimBoard::new();
    let clock = sim.clock();
    let host = Host::start(
        &parse(&text).unwrap(),
        &[entry::<HwWatchdog>()],
        Box::new(sim),
        Arc::new(clock.clone()),
        Options::default(),
    )
    .unwrap();
    let alive = Arc::new(AtomicBool::new(true));
    host.set_liveness(Arc::new(Gate(Arc::clone(&alive))));
    Rig {
        host,
        clock,
        alive,
        node,
        root,
    }
}

impl Rig {
    /// Millisecond steps on the simulated clock, with the host's due ticks.
    fn run(&self, ms: u64) {
        for _ in 0..ms {
            self.clock.advance(Duration::from_millis(1));
            self.host.run_ticks();
        }
    }

    /// Feed bytes the node has taken.
    fn fed(&self) -> u64 {
        fs::metadata(&self.node).unwrap().len()
    }
}

#[test]
fn the_node_is_fed_while_the_daemon_is_healthy_and_not_once_it_is_not() {
    let rig = rig("gate");
    rig.run(10);
    let fed = rig.fed();
    assert!(fed >= 1, "fed while healthy");

    rig.alive.store(false, Ordering::SeqCst);
    rig.run(3_000);
    assert_eq!(rig.fed(), fed, "no feed while the daemon is not healthy");

    // A gated device is asked again every 100 ms, so it is fed again within that.
    rig.alive.store(true, Ordering::SeqCst);
    rig.run(150);
    assert!(rig.fed() > fed, "fed again once it is healthy");
}

#[test]
fn the_feed_is_only_ever_the_feed_byte() {
    let rig = rig("bytes");
    rig.run(2_000);
    let bytes = fs::read(&rig.node).unwrap();
    assert!(!bytes.is_empty());
    assert!(
        bytes.iter().all(|&b| b == 0),
        "only the feed byte, never the magic close"
    );
    let _ = rig.host.device("watchdog").unwrap();
}
