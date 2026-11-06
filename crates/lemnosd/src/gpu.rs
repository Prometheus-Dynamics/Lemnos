//! The advisory GPU lease (`lemnos_ipc::GpuAcquire`): who holds each GPU,
//! who waits and in what order, holders past their time, reported hangs.
//!
//! Cooperative only. Mesa opens the render node itself, so nothing here is
//! enforced: a lease orders the clients that ask, and a client that never
//! asks keeps working. No GPU code runs in the service; it only lists the
//! render nodes under `/sys/class/drm`.

use lemnos_ipc::{
    GpuAcquire, GpuHangReport, GpuHolder, GpuInfo, GpuLeaseState, GpuMode, Message, Refusal,
};
use std::path::{Path, PathBuf};

/// Where the render nodes are listed.
pub const DRM_CLASS: &str = "/sys/class/drm";

/// A message for a client: (client id, message).
pub(crate) type Out = Vec<(u32, Message)>;

struct Lease {
    lease: u32,
    /// The acquire's request id: every message about the lease carries it.
    id: u32,
    client: u32,
    client_name: String,
    label: String,
    mode: GpuMode,
    priority: u8,
    /// Granted at (a holder) or queued at (a waiter), boot-clock ms.
    since_ms: u64,
    max_hold_ms: u32,
    overdue: bool,
    yield_asked: bool,
}

impl Lease {
    fn holder(&self, now_ms: u64) -> GpuHolder {
        GpuHolder {
            lease: self.lease,
            client_id: self.client,
            client: self.client_name.clone(),
            label: self.label.clone(),
            mode: self.mode,
            priority: self.priority,
            for_ms: now_ms.saturating_sub(self.since_ms),
            max_hold_ms: self.max_hold_ms,
            overdue: self.overdue,
        }
    }

    fn notice(&self, state: GpuLeaseState) -> (u32, Message) {
        (
            self.client,
            Message::GpuLease {
                id: self.id,
                lease: self.lease,
                state,
            },
        )
    }
}

struct Hang {
    client: String,
    label: String,
    detail: String,
    at_ms: u64,
}

struct Gpu {
    name: String,
    driver: String,
    node: String,
    holders: Vec<Lease>,
    waiters: Vec<Lease>,
    hangs: u32,
    last_hang: Option<Hang>,
}

impl Gpu {
    /// Whether a lease of `mode` can be held beside the holders now.
    fn fits(&self, mode: GpuMode) -> bool {
        self.holders.is_empty()
            || (mode == GpuMode::Shared && self.holders.iter().all(|h| h.mode == GpuMode::Shared))
    }

    /// Grants waiters in order while the first one fits, then asks holders
    /// of lower priority than a waiter still held back to yield (once each).
    fn settle(&mut self, now_ms: u64, out: &mut Out) {
        while self.waiters.first().is_some_and(|w| self.fits(w.mode)) {
            let mut lease = self.waiters.remove(0);
            lease.since_ms = now_ms;
            out.push(lease.notice(GpuLeaseState::Granted));
            self.holders.push(lease);
        }
        if let Some(first) = self.waiters.first() {
            let priority = first.priority;
            for h in self
                .holders
                .iter_mut()
                .filter(|h| h.priority < priority && !h.yield_asked)
            {
                h.yield_asked = true;
                out.push(h.notice(GpuLeaseState::YieldAsked));
            }
        }
    }

    fn info(&self, now_ms: u64) -> GpuInfo {
        GpuInfo {
            name: self.name.clone(),
            driver: self.driver.clone(),
            node: self.node.clone(),
            holders: self.holders.iter().map(|l| l.holder(now_ms)).collect(),
            waiters: self.waiters.iter().map(|l| l.holder(now_ms)).collect(),
            hangs: self.hangs,
            last_hang: self.last_hang.as_ref().map(|h| GpuHangReport {
                client: h.client.clone(),
                label: h.label.clone(),
                detail: h.detail.clone(),
                ago_ms: now_ms.saturating_sub(h.at_ms),
            }),
        }
    }
}

/// The GPUs and their leases.
pub(crate) struct GpuLeases {
    root: PathBuf,
    gpus: Vec<Gpu>,
    next_lease: u32,
}

/// The render nodes under `root`: (driver, node path), in node order.
fn render_nodes(root: &Path) -> Vec<(String, String)> {
    let Ok(dir) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut nodes: Vec<(u32, String, String)> = dir
        .flatten()
        .filter_map(|e| {
            let entry = e.file_name().into_string().ok()?;
            let minor = entry.strip_prefix("renderD")?.parse().ok()?;
            let driver = std::fs::read_link(e.path().join("device/driver")).ok()?;
            let driver = driver.file_name()?.to_str()?.to_string();
            Some((minor, driver, format!("/dev/dri/{entry}")))
        })
        .collect();
    nodes.sort_unstable();
    nodes.into_iter().map(|(_, d, n)| (d, n)).collect()
}

impl GpuLeases {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            gpus: Vec::new(),
            next_lease: 1,
        }
    }

    /// Lists the render nodes again: new ones are added, and a GPU that is
    /// gone is dropped once nothing holds or waits for it.
    fn discover(&mut self) {
        let nodes = render_nodes(&self.root);
        self.gpus.retain(|g| {
            !g.holders.is_empty()
                || !g.waiters.is_empty()
                || nodes.iter().any(|(_, n)| *n == g.node)
        });
        for (driver, node) in nodes {
            if self.gpus.iter().any(|g| g.node == node) {
                continue;
            }
            let same = self.gpus.iter().filter(|g| g.driver == driver).count();
            let name = if same == 0 {
                driver.clone()
            } else {
                format!("{driver}{same}")
            };
            self.gpus.push(Gpu {
                name,
                driver,
                node,
                holders: Vec::new(),
                waiters: Vec::new(),
                hangs: 0,
                last_hang: None,
            });
        }
    }

    fn take_lease_number(&mut self) -> u32 {
        let lease = self.next_lease;
        self.next_lease = self.next_lease.wrapping_add(1).max(1);
        lease
    }

    /// A client asks for a lease; the answer (and any grants) go to `out`.
    pub fn acquire(
        &mut self,
        (client, client_name): (u32, &str),
        id: u32,
        a: GpuAcquire,
        now_ms: u64,
        out: &mut Out,
    ) {
        self.discover();
        let refuse = |r| Message::GpuLease {
            id,
            lease: 0,
            state: GpuLeaseState::Refused(r),
        };
        let Some(gi) = (if a.gpu.is_empty() {
            (!self.gpus.is_empty()).then_some(0)
        } else {
            self.gpus.iter().position(|g| g.name == a.gpu)
        }) else {
            out.push((client, refuse(Refusal::UnknownDevice)));
            return;
        };
        // Behind every waiter of the same or higher priority.
        let gpu = &self.gpus[gi];
        let at = gpu
            .waiters
            .iter()
            .position(|w| w.priority < a.priority)
            .unwrap_or(gpu.waiters.len());
        let now = at == 0 && gpu.fits(a.mode);
        if !now && !a.wait {
            out.push((client, refuse(Refusal::Claimed)));
            return;
        }
        let lease = Lease {
            lease: self.take_lease_number(),
            id,
            client,
            client_name: client_name.to_string(),
            label: a.label,
            mode: a.mode,
            priority: a.priority,
            since_ms: now_ms,
            max_hold_ms: a.max_hold_ms,
            overdue: false,
            yield_asked: false,
        };
        let gpu = &mut self.gpus[gi];
        if !now {
            let ahead = (gpu.holders.len() + at).min(usize::from(u16::MAX)) as u16;
            out.push(lease.notice(GpuLeaseState::Queued { ahead }));
        }
        gpu.waiters.insert(at, lease);
        gpu.settle(now_ms, out);
    }

    /// Ends `client`'s lease `lease`, held or waiting.
    pub fn release(
        &mut self,
        client: u32,
        lease: u32,
        now_ms: u64,
        out: &mut Out,
    ) -> Result<(), Refusal> {
        for gpu in &mut self.gpus {
            let before = gpu.holders.len() + gpu.waiters.len();
            gpu.holders
                .retain(|l| !(l.lease == lease && l.client == client));
            gpu.waiters
                .retain(|l| !(l.lease == lease && l.client == client));
            if gpu.holders.len() + gpu.waiters.len() != before {
                gpu.settle(now_ms, out);
                return Ok(());
            }
        }
        Err(Refusal::UnknownHandle)
    }

    /// Records a hang `client` saw under its lease `lease`.
    // A hang is worth reading in the journal.
    #[allow(clippy::print_stderr)]
    pub fn hang(
        &mut self,
        (client, client_name): (u32, &str),
        lease: u32,
        detail: &str,
        now_ms: u64,
    ) -> Result<(), Refusal> {
        let (gpu, label) = self
            .gpus
            .iter_mut()
            .find_map(|g| {
                let label = g
                    .holders
                    .iter()
                    .chain(&g.waiters)
                    .find(|l| l.lease == lease && l.client == client)?
                    .label
                    .clone();
                Some((g, label))
            })
            .ok_or(Refusal::UnknownHandle)?;
        gpu.hangs = gpu.hangs.saturating_add(1);
        eprintln!(
            "lemnosd: gpu {}: {client_name} ({label}) reports a hang: {detail}",
            gpu.name
        );
        gpu.last_hang = Some(Hang {
            client: client_name.to_string(),
            label,
            detail: detail.to_string(),
            at_ms: now_ms,
        });
        Ok(())
    }

    pub fn state(&mut self, now_ms: u64) -> Vec<GpuInfo> {
        self.discover();
        self.gpus.iter().map(|g| g.info(now_ms)).collect()
    }

    /// A connection closed: its leases end (whatever its `keep` says: a lease
    /// nobody can release would hold the GPU until the service restarts).
    pub fn client_gone(&mut self, client: u32, now_ms: u64, out: &mut Out) {
        for gpu in &mut self.gpus {
            let before = gpu.holders.len() + gpu.waiters.len();
            gpu.holders.retain(|l| l.client != client);
            gpu.waiters.retain(|l| l.client != client);
            if gpu.holders.len() + gpu.waiters.len() != before {
                gpu.settle(now_ms, out);
            }
        }
    }

    /// Tells holders past their `max_hold_ms` (once each).
    // An overdue holder is worth reading in the journal.
    #[allow(clippy::print_stderr)]
    pub fn tick(&mut self, now_ms: u64, out: &mut Out) {
        for gpu in &mut self.gpus {
            for h in gpu.holders.iter_mut().filter(|h| {
                !h.overdue && h.max_hold_ms > 0 && now_ms >= h.since_ms + u64::from(h.max_hold_ms)
            }) {
                h.overdue = true;
                eprintln!(
                    "lemnosd: gpu {}: {} ({}) holds it past its {} ms",
                    gpu.name, h.client_name, h.label, h.max_hold_ms
                );
                out.push(h.notice(GpuLeaseState::Overdue));
            }
        }
    }

    /// When [`tick`](Self::tick) has something to do next, boot-clock ms.
    pub fn next_ms(&self) -> Option<u64> {
        self.gpus
            .iter()
            .flat_map(|g| &g.holders)
            .filter(|h| !h.overdue && h.max_hold_ms > 0)
            .map(|h| h.since_ms + u64::from(h.max_hold_ms))
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A drm class directory with one render node per driver.
    fn root(drivers: &[&str]) -> PathBuf {
        // One directory per call: the tests run in parallel.
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lemnosd-gpu-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // As sysfs has it: `renderDN/device` is the device, whose `driver`
        // links to the driver's directory.
        for (i, d) in drivers.iter().enumerate() {
            let node = dir.join(format!("renderD{}", 128 + i));
            let device = dir.join("devices").join(i.to_string());
            let driver = dir.join("drivers").join(d);
            for p in [&node, &device, &driver] {
                std::fs::create_dir_all(p).unwrap();
            }
            std::os::unix::fs::symlink(&device, node.join("device")).unwrap();
            std::os::unix::fs::symlink(&driver, device.join("driver")).unwrap();
        }
        dir
    }

    fn ask(mode: GpuMode, priority: u8, wait: bool) -> GpuAcquire {
        GpuAcquire {
            gpu: String::new(),
            mode,
            priority,
            wait,
            max_hold_ms: 0,
            label: "t".into(),
        }
    }

    fn states(out: &Out) -> Vec<(u32, u32, GpuLeaseState)> {
        out.iter()
            .map(|(c, m)| match m {
                Message::GpuLease { id, state, .. } => (*c, *id, *state),
                other => panic!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn render_nodes_are_named_by_driver_and_numbered_when_repeated() {
        let mut g = GpuLeases::new(root(&["v3d", "v3d", "vc4"]));
        let s = g.state(0);
        let names: Vec<_> = s
            .iter()
            .map(|i| (i.name.as_str(), i.node.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                ("v3d", "/dev/dri/renderD128"),
                ("v3d1", "/dev/dri/renderD129"),
                ("vc4", "/dev/dri/renderD130")
            ]
        );
    }

    #[test]
    fn exclusive_waits_for_shared_and_is_granted_on_release() {
        let mut g = GpuLeases::new(root(&["v3d"]));
        let mut out = Vec::new();
        g.acquire((1, "a"), 10, ask(GpuMode::Shared, 50, false), 0, &mut out);
        g.acquire((2, "b"), 20, ask(GpuMode::Shared, 50, false), 0, &mut out);
        g.acquire((3, "c"), 30, ask(GpuMode::Exclusive, 50, true), 0, &mut out);
        // A shared one behind the exclusive waiter waits too (no overtaking).
        g.acquire((4, "d"), 40, ask(GpuMode::Shared, 50, false), 0, &mut out);
        assert_eq!(
            states(&out),
            [
                (1, 10, GpuLeaseState::Granted),
                (2, 20, GpuLeaseState::Granted),
                (3, 30, GpuLeaseState::Queued { ahead: 2 }),
                (4, 40, GpuLeaseState::Refused(Refusal::Claimed)),
            ]
        );
        out.clear();
        g.release(1, 1, 5, &mut out).unwrap();
        assert!(out.is_empty());
        g.client_gone(2, 6, &mut out);
        assert_eq!(states(&out), [(3, 30, GpuLeaseState::Granted)]);
        assert_eq!(g.state(6)[0].holders[0].client, "c");
        assert_eq!(g.release(1, 1, 7, &mut out), Err(Refusal::UnknownHandle));
    }

    #[test]
    fn a_higher_priority_waiter_goes_first_and_asks_the_holder_to_yield() {
        let mut g = GpuLeases::new(root(&["v3d"]));
        let mut out = Vec::new();
        g.acquire(
            (1, "low"),
            1,
            ask(GpuMode::Exclusive, 10, true),
            0,
            &mut out,
        );
        g.acquire(
            (2, "mid"),
            2,
            ask(GpuMode::Exclusive, 50, true),
            0,
            &mut out,
        );
        g.acquire(
            (3, "high"),
            3,
            ask(GpuMode::Exclusive, 90, true),
            0,
            &mut out,
        );
        assert_eq!(
            states(&out),
            [
                (1, 1, GpuLeaseState::Granted),
                (2, 2, GpuLeaseState::Queued { ahead: 1 }),
                (1, 1, GpuLeaseState::YieldAsked),
                (3, 3, GpuLeaseState::Queued { ahead: 1 }),
            ]
        );
        let waiters: Vec<_> = g.state(0)[0].waiters.iter().map(|w| w.priority).collect();
        assert_eq!(waiters, [90, 50]);
        out.clear();
        g.client_gone(1, 1, &mut out);
        assert_eq!(states(&out), [(3, 3, GpuLeaseState::Granted)]);
    }

    #[test]
    fn a_holder_past_its_time_is_told_once_and_keeps_the_lease() {
        let mut g = GpuLeases::new(root(&["v3d"]));
        let mut out = Vec::new();
        let mut a = ask(GpuMode::Exclusive, 50, false);
        a.max_hold_ms = 100;
        g.acquire((1, "a"), 1, a, 1_000, &mut out);
        assert_eq!(g.next_ms(), Some(1_100));
        out.clear();
        g.tick(1_099, &mut out);
        assert!(out.is_empty());
        g.tick(1_100, &mut out);
        g.tick(1_200, &mut out);
        assert_eq!(states(&out), [(1, 1, GpuLeaseState::Overdue)]);
        assert_eq!(g.next_ms(), None);
        let s = g.state(1_250);
        assert!(s[0].holders[0].overdue);
        assert_eq!(s[0].holders[0].for_ms, 250);
    }

    #[test]
    fn hangs_are_recorded_on_the_lease_gpu() {
        let mut g = GpuLeases::new(root(&["v3d"]));
        let mut out = Vec::new();
        g.acquire((1, "a"), 1, ask(GpuMode::Shared, 50, false), 0, &mut out);
        assert_eq!(g.hang((2, "b"), 1, "x", 0), Err(Refusal::UnknownHandle));
        g.hang((1, "a"), 1, "fence never signalled", 10).unwrap();
        let s = g.state(30);
        assert_eq!(s[0].hangs, 1);
        let h = s[0].last_hang.as_ref().unwrap();
        assert_eq!((h.client.as_str(), h.ago_ms), ("a", 20));
    }

    #[test]
    fn an_unknown_gpu_or_none_at_all_is_refused() {
        let mut g = GpuLeases::new(root(&[]));
        let mut out = Vec::new();
        g.acquire((1, "a"), 1, ask(GpuMode::Shared, 50, true), 0, &mut out);
        let mut named = ask(GpuMode::Shared, 50, true);
        named.gpu = "v3d".into();
        g.acquire((1, "a"), 2, named, 0, &mut out);
        assert_eq!(
            states(&out),
            [
                (1, 1, GpuLeaseState::Refused(Refusal::UnknownDevice)),
                (1, 2, GpuLeaseState::Refused(Refusal::UnknownDevice)),
            ]
        );
    }
}
