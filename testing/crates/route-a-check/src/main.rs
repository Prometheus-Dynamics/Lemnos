#![allow(clippy::print_stdout, clippy::print_stderr)]

//! The route-A check (test only, never shipped): the engine host, the same `Host::start` that
//! lemnosd calls, with `system-slots` on a board whose boot partition is another system's (its
//! `default.txt` is that system's 55-byte file) and whose data partition holds that system's slots
//! state, in this system's own eight-key format, as an older build of it left it. Run in the QEMU
//! guest, in real time, past the driver's uptime and its first slow boot-file check.
//!
//!   lemnos-route-a-check
//!
//! It passes when the state dir and the boot partition are byte-identical after the start, after
//! the ticks, and after every call that would write (each refused with `FAILED_PRECONDITION`); when
//! the device's health and its `list` and `info` say `foreign`; and when the same run on a boot
//! partition of ours does write (its start and its `confirm`), so the check can tell the two apart.
//!
//! The slots driver takes its boot chain from the device tree on a board. An x86 guest has none, so
//! the chain is stood in with `boot_node::TEST_ROOTS` (the `testing` feature): a fake `chosen` and a
//! fake sysfs whose boot partition node is a file holding the partition.

use std::fs;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use lemnos_core::{CallerId, Encode, Health, HostClock, MethodId, Status, Writer};
use lemnos_host::board::parse;
use lemnos_host::{Host, Options, entry};
use lemnos_slot_store::bays::Layout as Bays;
use lemnos_slot_store::boot_node::{BootRoots, TEST_ROOTS};
use lemnos_slot_store::bootimage::{self, Family, Parts};

/// Where the check stands the board in: a tmpfs under `/run` in the guest. `LEMNOS_ROUTE_A_ROOT`
/// names another place, for a run on a PC first.
fn root_dir() -> PathBuf {
    std::env::var_os("LEMNOS_ROUTE_A_ROOT")
        .map_or_else(|| PathBuf::from("/run/route-a"), PathBuf::from)
}
/// The other system's slot: the id an older build of it would have.
const OTHER: &str = "system-2026.2.0-dev.993-abcd-e1097ec7";
/// Our slot, in bay 0 of the partition.
const OURS: &str = "ours-1";
/// The driver's uptime before a default boot is good, set short so the run passes it in seconds.
const UPTIME_S: u64 = 4;
/// How long the host runs before the calls: past the uptime, and past the first slow boot-file
/// check (at the first tick; the next is ten minutes on, which the driver's unit test runs).
const RUN: Duration = Duration::from_secs(UPTIME_S + 6);

/// The driver clock: nanoseconds since the host started.
struct Clock(Instant);

impl HostClock for Clock {
    fn now_ns(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}

/// A boot partition from the image builder with slot [`OURS`] in bay 0 and every pointer naming it.
/// `foreign` sets `default.txt`'s size to the 55 bytes of the other system's own file, as on a
/// partition a slot was added to: everything else is ours, so only the pointer says whose it is.
fn partition(foreign: bool) -> Result<Vec<u8>, String> {
    let parts = Parts {
        family: Family::Tryboot,
        layout: Bays::new([4096, 2048, 1024, 512, 512]).map_err(|e| format!("{e:?}"))?,
        root: vec![
            ("slot-layout.toml", b"version = 5\n".to_vec()),
            (
                "config.txt",
                lemnos_slot_store::firmware::CONFIG_TXT.as_bytes().to_vec(),
            ),
        ],
        fixed: Vec::new(),
    };
    let payloads: Vec<Vec<u8>> = [
        "kernel",
        "initramfs",
        "device tree",
        "console=tty1",
        "overlay",
    ]
    .iter()
    .map(|what| format!("{what} {OURS}").into_bytes())
    .collect();
    let refs: Vec<&[u8]> = payloads.iter().map(Vec::as_slice).collect();
    let size = bootimage::min_size(&parts)?;
    let mut image = bootimage::build(&parts, size, OURS, &refs)?;
    if foreign {
        let at = image
            .windows(11)
            .position(|w| w == b"DEFAULT TXT")
            .ok_or("no default.txt entry")?;
        image[at + 28..at + 32].copy_from_slice(&55u32.to_le_bytes());
    }
    Ok(image)
}

/// The fake boot chain over `root`, with the partition at `root/dev/mmcblk0p1`.
fn boot_chain(root: &Path, image: &[u8]) -> std::io::Result<BootRoots> {
    let sys = root.join("sys");
    let node = sys.join("firmware/devicetree/base/soc/mmc@7e202000");
    fs::create_dir_all(&node)?;
    let controller = sys.join("devices/sd-host");
    fs::create_dir_all(&controller)?;
    symlink(&node, controller.join("of_node"))?;
    let disk = controller.join("mmcblk0");
    fs::create_dir_all(&disk)?;
    fs::create_dir_all(sys.join("class/block"))?;
    symlink(&disk, sys.join("class/block/mmcblk0"))?;
    let part = disk.join("mmcblk0p1");
    fs::create_dir_all(&part)?;
    fs::write(part.join("partition"), "1\n")?;
    symlink(&part, sys.join("class/block/mmcblk0p1"))?;
    let chosen = root.join("chosen");
    fs::create_dir_all(&chosen)?;
    fs::write(chosen.join("lemnos,boot-source"), b"/soc/mmc@7e202000\0")?;
    let dev = root.join("dev");
    fs::create_dir_all(&dev)?;
    fs::write(dev.join("mmcblk0p1"), image)?;
    Ok(BootRoots { chosen, sys, dev })
}

/// The other system's slots state, as an older build of it left it.
fn foreign_state(data: &Path) -> std::io::Result<()> {
    let state = data.join("board/slots");
    fs::create_dir_all(&state)?;
    fs::write(
        state.join("state.env"),
        format!(
            "DEFAULT='{OTHER}'\nPREVIOUS=''\nTRIAL=''\nSTAGED=''\nSTATE='idle'\nERROR=''\nPROGRESS='0'\nSTARTED_BY=''\n"
        ),
    )?;
    fs::write(
        state.join(format!("{OTHER}.meta")),
        "GOOD=yes\nFAILED=no\nPINNED=no\nSEQ=1\n",
    )?;
    fs::write(state.join("booted"), format!("{OTHER}\n"))?;
    fs::write(state.join("trial-keeps-default"), format!("{OTHER}\n"))?;
    fs::write(state.join("settings.env"), "KEEP='2'\n")?;
    let images = data.join("slots");
    fs::create_dir_all(images.join(OTHER))?;
    fs::write(
        images.join(OTHER).join("rootfs.erofs"),
        b"the other system's root",
    )?;
    fs::create_dir_all(images.join(".incoming"))?;
    fs::write(images.join(".incoming/part"), b"half a bundle")?;
    Ok(())
}

/// Every entry under `root`, in path order: its path, kind, mode, owner and bytes.
type Tree = Vec<(PathBuf, bool, u32, u32, Vec<u8>)>;

fn tree(root: &Path) -> std::io::Result<Tree> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let meta = fs::symlink_metadata(&path)?;
        let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        let bytes = if meta.is_dir() {
            for entry in fs::read_dir(&path)? {
                stack.push(entry?.path());
            }
            Vec::new()
        } else {
            fs::read(&path)?
        };
        out.push((rel, meta.is_dir(), meta.mode(), meta.uid(), bytes));
    }
    out.sort();
    Ok(out)
}

/// The first entry two trees differ in, said in words.
fn first_difference(before: &Tree, after: &Tree) -> String {
    for (at, entry) in after.iter().enumerate() {
        match before.get(at) {
            Some(was) if was == entry => {}
            Some(was) if was.0 == entry.0 => {
                return format!("{} changed", entry.0.display());
            }
            _ => return format!("{} appeared", entry.0.display()),
        }
    }
    before
        .get(after.len())
        .map_or_else(String::new, |gone| format!("{} went", gone.0.display()))
}

fn board_text(data: &Path, boot: &Path) -> String {
    format!(
        r#"format = "lemnos.board"
schema_version = 1

[board]
id = "route-a"

[[devices]]
id = "slots"
driver = "system-slots"
config = {{ data = "{data}", boot = "{boot}", boot_sources = ["/soc/mmc@7e202000=/soc/mmc@7e202000"], systemctl = "/bin/false", partuuid = "abcd-02", boot_args = "board", reserve_mb = 0, confirm_uptime_s = {UPTIME_S}, device_tree = "{none}", vcio = "{none}" }}
"#,
        data = data.display(),
        boot = boot.display(),
        none = boot.with_file_name("none").display(),
    )
}

/// The method of the device's contracts named `name`.
fn method_of(host: &Host, device: u32, name: &str) -> Option<MethodId> {
    let manifest = host.manifest(device)?;
    for (contract, info) in manifest.provides.iter().enumerate() {
        for method in info.methods {
            if method.name == name {
                return Some(MethodId {
                    contract: u8::try_from(contract).ok()?,
                    method: method.id,
                });
            }
        }
    }
    None
}

/// The arguments, encoded as the generated client encodes them.
struct Args(Vec<u8>, usize);

impl Args {
    fn new() -> Self {
        Self(vec![0u8; 256], 0)
    }

    fn put<T: Encode + ?Sized>(mut self, value: &T) -> Self {
        let mut w = Writer::new(&mut self.0[self.1..]);
        value.encode(&mut w).expect("the arguments fit");
        self.1 += w.len();
        self
    }

    fn bytes(&self) -> &[u8] {
        &self.0[..self.1]
    }
}

/// A started host over `data` and the partition `image`, with the slots device's index.
fn start(root: &Path, data: &Path, image: &[u8]) -> Result<(Host, u32), String> {
    let roots = boot_chain(root, image).map_err(|e| format!("the boot chain: {e}"))?;
    *TEST_ROOTS.lock().map_err(|_| "the roots lock")? = Some(roots);
    let board = parse(&board_text(data, &root.join("boot"))).map_err(|e| format!("{e}"))?;
    let host = Host::start(
        &board,
        &[entry::<lemnos_driver_system_slots::SystemSlots>()],
        Box::new(lemnos_linux::LinuxBoard::new()),
        Arc::new(Clock(Instant::now())),
        Options::default(),
    )
    .map_err(|e| format!("the host did not start: {e:?}"))?;
    let slots = host
        .devices()
        .into_iter()
        .find(|d| d.id == "slots")
        .map(|d| d.index)
        .ok_or("no slots device")?;
    Ok((host, slots))
}

fn health(host: &Host, device: u32) -> Health {
    host.devices()
        .into_iter()
        .find(|d| d.index == device)
        .map_or(Health::Ok, |d| d.health)
}

/// The text of a method that only fills a buffer (`list`, `info`).
fn text(host: &Host, device: u32, name: &str) -> Result<String, String> {
    let method = method_of(host, device, name).ok_or(format!("no method {name}"))?;
    let mut reply = vec![0u8; 1 << 16];
    let cap = u32::try_from(reply.len()).unwrap_or(u32::MAX).to_le_bytes();
    let n = host
        .call(device, method, &cap, &mut reply)
        .map_err(|s| format!("{name}: {s:?}"))?;
    Ok(String::from_utf8_lossy(&reply[..n]).into_owned())
}

struct Checks(u32);

impl Checks {
    fn check(&mut self, ok: bool, what: &str) {
        if ok {
            println!("route-a: ok: {what}");
        } else {
            println!("route-a: FAIL: {what}");
            self.0 += 1;
        }
    }
}

fn route_a(c: &mut Checks) -> Result<(), String> {
    let root = root_dir().join("foreign");
    let data = root.join("data");
    foreign_state(&data).map_err(|e| format!("the state: {e}"))?;
    let image = partition(true)?;
    let node = root.join("dev/mmcblk0p1");
    let before = tree(&data).map_err(|e| e.to_string())?;
    let (host, slots) = start(&root, &data, &image)?;
    let after_start = tree(&data).map_err(|e| e.to_string())?;
    c.check(
        after_start == before,
        &format!(
            "the state dir is byte-identical after start {}",
            first_difference(&before, &after_start)
        ),
    );
    thread::sleep(RUN);
    let after_ticks = tree(&data).map_err(|e| e.to_string())?;
    c.check(
        after_ticks == before,
        &format!(
            "the state dir is byte-identical after {} s of ticks, past the uptime ({UPTIME_S} s) and the slow check {}",
            RUN.as_secs(),
            first_difference(&before, &after_ticks)
        ),
    );
    let foreign = lemnos_driver_system_slots::FOREIGN;
    c.check(
        health(&host, slots) == Health::Degraded(foreign),
        &format!("health is degraded: {foreign}"),
    );
    let refused = Err(Status::FAILED_PRECONDITION);
    let caller = CallerId(5);
    let calls: [(&str, Args, bool); 12] = [
        (
            "install",
            Args::new().put(&1024u64).put(&false).put(&false),
            true,
        ),
        ("accept", Args::new().put(&1024u64), false),
        (
            "install_file",
            Args::new().put(&"part").put(&false).put(&false),
            true,
        ),
        ("write", Args::new().put(&0u64).put(&b"bytes"[..]), false),
        ("set_default", Args::new().put(&OTHER).put(&true), false),
        (
            "trial",
            Args::new().put(&OTHER).put(&false).put(&false).put(&true),
            false,
        ),
        ("confirm", Args::new(), false),
        ("remove", Args::new().put(&OTHER), false),
        ("pin", Args::new().put(&OTHER).put(&true), false),
        ("set_keep", Args::new().put(&9u32), false),
        ("apply", Args::new().put(&true), false),
        ("apply", Args::new().put(&false), false),
    ];
    for (name, args, job) in &calls {
        let method = method_of(&host, slots, name).ok_or(format!("no method {name}"))?;
        let got = if *job {
            host.start_job(caller, slots, method, args.bytes())
                .map(drop)
        } else {
            host.call_as(caller, slots, method, args.bytes(), &mut [])
                .map(drop)
        };
        c.check(got == refused, &format!("{name} is refused: {got:?}"));
    }
    let list = text(&host, slots, "list")?;
    c.check(
        list.contains(&format!("\"foreign\":\"{foreign}: default.txt")),
        &format!("list says foreign: {list}"),
    );
    let info = text(&host, slots, "info")?;
    c.check(
        info.contains(&format!("\"foreign\":\"{foreign}: default.txt")),
        "info says foreign",
    );
    thread::sleep(Duration::from_secs(2));
    host.shutdown();
    let after = tree(&data).map_err(|e| e.to_string())?;
    c.check(
        after == before,
        &format!(
            "the state dir is byte-identical after the calls and more ticks {}",
            first_difference(&before, &after)
        ),
    );
    let boot_after = fs::read(&node).map_err(|e| e.to_string())?;
    c.check(boot_after == image, "the boot partition is byte-identical");
    Ok(())
}

fn ours(c: &mut Checks) -> Result<(), String> {
    let root = root_dir().join("ours");
    let data = root.join("data");
    fs::create_dir_all(data.join("board/slots")).map_err(|e| e.to_string())?;
    fs::create_dir_all(data.join("slots").join(OURS)).map_err(|e| e.to_string())?;
    fs::write(data.join("board/slots/booted"), format!("{OURS}\n")).map_err(|e| e.to_string())?;
    let before = tree(&data).map_err(|e| e.to_string())?;
    let (host, slots) = start(&root, &data, &partition(false)?)?;
    c.check(
        tree(&data).map_err(|e| e.to_string())? != before,
        "on a partition of ours the start writes its settings",
    );
    c.check(
        health(&host, slots) == Health::Ok,
        "on ours the health is ok",
    );
    let confirm = method_of(&host, slots, "confirm").ok_or("no confirm")?;
    let got = host.call_as(CallerId(5), slots, confirm, &[], &mut []);
    c.check(got.is_ok(), &format!("on ours confirm is taken: {got:?}"));
    let fallbacks = fs::read_to_string(data.join("board/slots/fallbacks")).unwrap_or_default();
    c.check(fallbacks == "0\n", "on ours confirm writes the state");
    host.shutdown();
    Ok(())
}

fn main() -> ExitCode {
    let _ = fs::remove_dir_all(root_dir());
    let mut c = Checks(0);
    for (name, run) in [
        ("route A", route_a as fn(&mut Checks) -> Result<(), String>),
        ("ours", ours),
    ] {
        if let Err(why) = run(&mut c) {
            c.check(false, &format!("{name}: {why}"));
        }
    }
    *TEST_ROOTS.lock().unwrap_or_else(|p| p.into_inner()) = None;
    let _ = fs::remove_dir_all(root_dir());
    if c.0 == 0 {
        println!("route-a: PASS");
        ExitCode::SUCCESS
    } else {
        println!("route-a: {} failed", c.0);
        ExitCode::FAILURE
    }
}
