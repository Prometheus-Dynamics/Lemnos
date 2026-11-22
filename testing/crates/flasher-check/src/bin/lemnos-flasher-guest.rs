#![allow(clippy::print_stdout)]

//! The first flash on an eMMC, in a QEMU guest (test only, never shipped; testing/flasher/run.sh). Run as the
//! guest's init: it mounts the kernel's file systems, waits for the eMMC (QEMU's `emmc` with two hardware boot
//! areas, as the board's eMMC has), and starts the real lemnosd on a board file whose `system-flash` device names
//! its target by the eMMC's controller (`/pcie@10000000`, the guest's PCIe host, where the board names its eMMC
//! controller). Then, as the board's flasher does:
//!
//! 1. The bridge's own stream (`lemnos_orion::stream::Streamer`) takes the transport manifest and the xz image in
//!    chunks, and commits: the answer is `committed`, its detail the fields `raw_bytes=<n> outcome=1` (unsigned).
//! 2. A `verify` job reads the target back: its result is `raw_bytes=<the raw byte count>`.
//! 3. The eMMC's user area holds the image (its SHA-256 read back from `/dev/mmcblk0`), the boot areas are
//!    byte-identical to what they held before, and the kernel read the new partition table (`mmcblk0p1` is there,
//!    its size the image's).
//!
//! It prints `flasher: passed (<n> checks)` or each failure, then powers the guest off.

use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, ExitCode};
use std::thread;
use std::time::Duration;

use lemnos_contract_system_flash::Flash;
use lemnos_core::Contract;
use lemnos_orion::stream::{Streamer, committed_detail};
use orion::files::FileState;
use sha2::{Digest, Sha256};

const SOCKET: &str = "/run/lemnosd.sock";
const BOARD: &str = "/run/board.toml";
const STREAMS: &str = "/run/streams";
const NAME: &str = "image.img.xz";
/// The guest's eMMC controller in the device tree: QEMU's `sdhci-pci` sits under the PCIe host.
const CONTROLLER: &str = "/pcie@10000000";
const CHUNK: usize = 64 * 1024;

fn say(line: &str) {
    println!("{line}");
}

struct Checks {
    n: u32,
    failed: Vec<String>,
}

impl Checks {
    fn check(&mut self, ok: bool, what: &str) -> bool {
        self.n += 1;
        if ok {
            say(&format!("flasher: ok: {what}"));
        } else {
            say(&format!("flasher: FAIL: {what}"));
            self.failed.push(what.to_string());
        }
        ok
    }
}

fn mount(source: &str, at: &str, fs_type: &str) {
    let _ = fs::create_dir_all(at);
    let _ = rustix::mount::mount(
        source,
        at,
        fs_type,
        rustix::mount::MountFlags::empty(),
        None::<&std::ffi::CStr>,
    );
}

/// Waits for `path` to appear, for a bounded time: the eMMC is scanned after the init starts.
fn wait_for(path: &str) -> bool {
    for _ in 0..400 {
        if Path::new(path).exists() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

fn sha256_of(path: &str, limit: Option<u64>) -> Result<String, String> {
    let file = fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let mut reader: Box<dyn Read> = match limit {
        Some(n) => Box::new(file.take(n)),
        None => Box::new(file),
    };
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("{path}: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn board_text() -> String {
    format!(
        r#"format = "lemnos.board"
schema_version = 1

[board]
id = "flasher-guest"

[[devices]]
id = "flash"
driver = "system-flash"
config = {{ controller = "{CONTROLLER}", allow_first_flash = true }}
"#
    )
}

fn start_daemon() -> Result<Child, String> {
    fs::write(BOARD, board_text()).map_err(|e| format!("the board file: {e}"))?;
    Command::new("/lemnosd")
        .args(["--board", BOARD, "--socket", SOCKET])
        .spawn()
        .map_err(|e| format!("start lemnosd: {e}"))
}

/// The facts the harness wrote beside the image: the raw image's size and SHA-256, and its first partition's size in
/// sectors.
struct Facts {
    raw: u64,
    sha: String,
    part_sectors: u64,
}

fn facts() -> Result<Facts, String> {
    let text = fs::read_to_string("/image.facts").map_err(|e| format!("/image.facts: {e}"))?;
    let mut words = text.split_whitespace();
    let mut next = |what: &str| words.next().ok_or(format!("/image.facts has no {what}"));
    let raw = next("size")?.parse().map_err(|_| "size".to_string())?;
    let sha = next("sha256")?.to_string();
    let part_sectors = next("partition size")?
        .parse()
        .map_err(|_| "partition size".to_string())?;
    Ok(Facts {
        raw,
        sha,
        part_sectors,
    })
}

fn run(c: &mut Checks) -> Result<(), String> {
    let facts = facts()?;
    for node in [
        "/sys/class/block/mmcblk0",
        "/sys/class/block/mmcblk0boot0",
        "/sys/class/block/mmcblk0boot1",
    ] {
        if !c.check(wait_for(node), &format!("{node} is there")) {
            return Err("the eMMC did not come up".into());
        }
    }
    let boot_before = [
        sha256_of("/dev/mmcblk0boot0", None)?,
        sha256_of("/dev/mmcblk0boot1", None)?,
    ];
    c.check(
        !Path::new("/sys/class/block/mmcblk0p1").exists(),
        "the eMMC is blank: no partition before the flash",
    );

    let mut daemon = start_daemon()?;
    let result = flash(c, &facts, &boot_before);
    let _ = daemon.kill();
    let _ = daemon.wait();
    result
}

fn flash(c: &mut Checks, facts: &Facts, boot_before: &[String; 2]) -> Result<(), String> {
    let client =
        lemnos_client::Client::connect_within(SOCKET, "flasher-check", Duration::from_secs(20))
            .map_err(|e| format!("connect: {e:?}"))?;
    // The transport manifest first, then the stream, as the sender pushes them.
    fs::create_dir_all(STREAMS).map_err(|e| e.to_string())?;
    fs::copy(
        "/image.img.xz.json",
        Path::new(STREAMS).join(format!("{NAME}.json")),
    )
    .map_err(|e| format!("the manifest: {e}"))?;
    let stream = fs::read("/image.img.xz").map_err(|e| format!("the image: {e}"))?;
    let digest: [u8; 32] = Sha256::digest(&stream).into();
    let mut streamer = Streamer::new(
        "flash",
        Path::new(STREAMS),
        vec![NAME.into()],
        Path::new(SOCKET),
    );
    let begun = streamer.begin(1, NAME, stream.len() as u64, digest);
    if !c.check(
        begun.state == FileState::Receiving,
        &format!(
            "the bridge's stream began: {:?} {}",
            begun.state, begun.detail
        ),
    ) {
        return Err("the stream did not begin".into());
    }
    let mut at = 0u64;
    for piece in stream.chunks(CHUNK) {
        let answer = streamer.chunk(1, at, piece);
        if answer.state != FileState::Receiving {
            c.check(
                false,
                &format!("chunk at {at}: {:?} {}", answer.state, answer.detail),
            );
            return Err("the stream broke".into());
        }
        at += piece.len() as u64;
    }
    let committed = streamer.commit(1);
    c.check(
        committed.state == FileState::Committed && committed.detail == committed_detail(facts.raw, Some(1)),
        &format!(
            "the bridge committed the image, its detail the fields raw_bytes={} outcome=1: {:?} {:?}",
            facts.raw, committed.state, committed.detail
        ),
    );

    // The verify: a second job, which reads the target back for the manifest's size.
    let verify = <dyn Flash as Contract>::INFO
        .methods
        .iter()
        .find(|m| m.name == "verify")
        .map(|m| m.id)
        .ok_or("no verify method")?;
    let mut handle = client
        .open_tag("flash", <dyn Flash as Contract>::INFO.tag.as_str())
        .map_err(|e| format!("open: {e:?}"))?;
    let job = client
        .start_job(&mut handle, verify, &[])
        .map_err(|e| format!("verify: {e:?}"))?;
    let ended = client.wait_job(job, Duration::from_secs(120), &mut |_, _| {});
    c.check(
        matches!(&ended, Some((status, result)) if status.0 == 0 && *result == format!("raw_bytes={}", facts.raw).into_bytes()),
        &format!(
            "the verify read the target back, its result in fields: {:?}",
            ended.as_ref().map(|(s, r)| (s.0, String::from_utf8_lossy(r).into_owned()))
        ),
    );

    let user = sha256_of("/dev/mmcblk0", Some(facts.raw))?;
    c.check(
        user == facts.sha,
        &format!("the eMMC's user area holds the image, read back: {user}"),
    );
    let boot_after = [
        sha256_of("/dev/mmcblk0boot0", None)?,
        sha256_of("/dev/mmcblk0boot1", None)?,
    ];
    c.check(
        &boot_after == boot_before,
        "the eMMC's hardware boot areas are byte-identical to what they held before",
    );
    let part = fs::read_to_string("/sys/class/block/mmcblk0p1/size")
        .map(|s| s.trim().to_string())
        .map_err(|e| e.to_string());
    c.check(
        part.as_deref().ok() == Some(facts.part_sectors.to_string().as_str()),
        &format!(
            "the kernel read the new table: mmcblk0p1 is {} sectors: {part:?}",
            facts.part_sectors
        ),
    );
    Ok(())
}

fn main() -> ExitCode {
    mount("proc", "/proc", "proc");
    mount("sysfs", "/sys", "sysfs");
    mount("devtmpfs", "/dev", "devtmpfs");
    mount("tmpfs", "/run", "tmpfs");
    let mut c = Checks {
        n: 0,
        failed: Vec::new(),
    };
    if let Err(why) = run(&mut c) {
        c.check(false, &why);
    }
    rustix::fs::sync();
    if c.failed.is_empty() {
        say(&format!("flasher: passed ({} checks)", c.n));
    } else {
        say(&format!(
            "flasher: failed ({} of {} checks)",
            c.failed.len(),
            c.n
        ));
    }
    let _ = rustix::system::reboot(rustix::system::RebootCommand::PowerOff);
    ExitCode::FAILURE
}
