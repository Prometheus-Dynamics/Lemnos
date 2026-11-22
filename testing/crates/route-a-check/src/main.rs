#![allow(clippy::print_stdout, clippy::print_stderr)]

//! The route-A check (test only, never shipped): the real lemnosd, started as its packaged unit
//! starts it (`lemnos-unit-check exec --with slots`: root, the unit's capability set, system call
//! filter and file system sandbox, then lemnosd's own drop to `--user`), with `system-slots` on a
//! boot partition that is another system's, then on one that is ours. Run in the QEMU guest as root,
//! in real time, past the driver's uptime and its first slow boot-file check. Every call goes over
//! the daemon's socket.
//!
//!   lemnos-route-a-check
//!
//! Three runs:
//!
//! - **foreign**: the partition's `default.txt` is the other system's 55-byte file and the data
//!   partition holds that system's slots state, root's, as slot-init leaves it when the records are
//!   not ours. The state dir and the partition stay byte-identical after the start, the ticks and
//!   every writing call (each refused with `FAILED_PRECONDITION`); health, `list` and `info` say
//!   `foreign`; the root phase held the partition read only.
//! - **ours**: the partition is ours and the slots dirs are given to the daemon's user by slot-init's
//!   own rule (`lemnos_slot_store::owner`). The root phase hands the partition over read-write; health
//!   is ok; `confirm` and `trial` are taken, the daemon is restarted on the trial slot as the board
//!   would boot it, and `confirm` and `set_default` are taken there. After each, the boot records are
//!   read back raw from the partition: the trial record, then `default.txt`.
//!   Then the restart into the board's way back (`[boot] loop_return = "rpiboot"`, a `system-reboot` device on
//!   the firmware mailbox's stand-in, `/dev/vcio`, root's 0600 as on a board, held by the reboot device's root
//!   phase across the drop): `reboot_to("rpiboot")` sets the reboot order once (3, the flashing mode) and the
//!   helper is asked the check and the restart; another target is refused with nothing asked; a mailbox that
//!   refuses answers `IO` and nothing restarts (the helper is asked its check only).
//! - **image-side**: the partition is ours, but the slots dirs are root's and the unit has no slots
//!   drop-in. The daemon must say so by name (`state-unwritable`) and refuse the switch.
//!
//! The slots driver takes its boot chain from the device tree on a board. An x86 guest has none, so
//! the chain is stood in through lemnosd's `testing-boot-roots` seam (`/lemnosd-route-a`, built apart
//! from the shipped daemon): a fake `chosen` and sysfs under `/run/route-a`, and the partition node a
//! root-owned 0600 file under `/dev/route-a`, which only the root phase can open.
//!
//! `LEMNOS_ROUTE_A_PC=1` runs the same calls on a PC as an ordinary user, with no launcher, no drop
//! and no hand-over, under `LEMNOS_ROUTE_A_ROOT`: a rehearsal of the slot calls only.

use std::fs::{self, File};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use lemnos_client::{Client, DeviceHealth, Error, Handle};
use lemnos_core::{Driver, Encode, Status, Writer};
use lemnos_daemon_ready::Ready;
use lemnos_driver_system_slots::{SystemReboot, SystemSlots};
use lemnos_slot_store::bays::{Layout as Bays, as_layout_5};
use lemnos_slot_store::boot_raw::{Boot, Pointer, Pointers, Records};
use lemnos_slot_store::bootimage::{self, Family, Parts};
use lemnos_slot_store::owner;
use lemnos_slot_store::twocopy::FileSectors;

/// The other system's slot: the id an older build of it would have.
const OTHER: &str = "system-2026.2.0-dev.993-abcd-e1097ec7";
/// Our running slot, in bay 0 of the partition.
const OURS: &str = "ours-1";
/// Our second slot, installed into a free bay before the daemon starts.
const NEXT: &str = "ours-2";
/// The board's id, which a slot's manifest must name.
const BOARD_ID: &str = "route-a";
/// The daemon's ids, as numbers: the guest has no account database. The unit names `lemnos`.
const DAEMON: u32 = 999;
/// The driver's uptime before a default boot is good, set short so the run passes it in seconds.
const UPTIME_S: u64 = 4;
/// How long the daemon runs before the calls: past the uptime and the first slow boot-file check.
const RUN: Duration = Duration::from_secs(UPTIME_S + 6);
const READY_WAIT: Duration = Duration::from_secs(20);
/// How long a health read may take to be known: a device in a call reads Ok until it returns.
const SETTLE: Duration = Duration::from_secs(10);

fn pc() -> bool {
    std::env::var_os("LEMNOS_ROUTE_A_PC").is_some()
}

/// Where the fake boot chain, the board file and the daemon's log go.
fn root_dir() -> PathBuf {
    std::env::var_os("LEMNOS_ROUTE_A_ROOT")
        .map_or_else(|| PathBuf::from("/run/route-a"), PathBuf::from)
}

/// Where the partition node goes: under /dev in the guest, writable under the unit as on a board.
fn dev_dir() -> PathBuf {
    if pc() {
        root_dir().join("dev")
    } else {
        PathBuf::from("/dev/route-a")
    }
}

/// The data partition: `/data`, the path the slots drop-in grants.
fn data_dir() -> PathBuf {
    if pc() {
        root_dir().join("data")
    } else {
        PathBuf::from("/data")
    }
}

/// The unit's runtime directory, stood in.
fn runtime_dir() -> PathBuf {
    if pc() {
        root_dir().join("run")
    } else {
        PathBuf::from("/run/route-a-unit")
    }
}

fn socket() -> PathBuf {
    runtime_dir().join("lemnosd.sock")
}

fn daemon_log() -> PathBuf {
    root_dir().join("lemnosd.log")
}

/// A boot partition from the image builder with slot [`OURS`] in bay 0 and every pointer naming it.
/// `foreign` sets `default.txt`'s size to the 55 bytes of the other system's own file, as on a
/// partition a slot was added to: everything else is ours, so only the pointer says whose it is.
fn partition(foreign: bool) -> Result<Vec<u8>, String> {
    let parts = Parts {
        family: Family::Tryboot,
        layout: Bays::new([4096, 2048, 1024, 512, 512]).map_err(|e| format!("{e:?}"))?,
        root: vec![
            ("slot-layout.toml", b"version = 6\n".to_vec()),
            (
                "config.txt",
                lemnos_slot_store::firmware::CONFIG_TXT.as_bytes().to_vec(),
            ),
        ],
    };
    let payloads = payloads(OURS);
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

fn payloads(slot: &str) -> Vec<Vec<u8>> {
    [
        "kernel",
        "initramfs",
        "device tree",
        "console=tty1",
        "overlay",
    ]
    .iter()
    .map(|what| format!("{what} {slot}").into_bytes())
    .collect()
}

/// The fake boot chain: `chosen` and sysfs under the root dir, the partition node under the dev dir,
/// owned by root with mode 0600 (on a board, root and group disk; no group of the daemon's).
fn boot_chain(image: &[u8]) -> std::io::Result<(PathBuf, String)> {
    let root = root_dir();
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
    let dev = dev_dir();
    fs::create_dir_all(&dev)?;
    let file = dev.join("mmcblk0p1");
    fs::write(&file, image)?;
    if !pc() {
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600))?;
    }
    Ok((
        file,
        format!("{}:{}:{}", chosen.display(), sys.display(), dev.display()),
    ))
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

/// A slot's image and its schema-3 manifest, as a bundle's install leaves them: one data block and
/// its hash tree, and the five boot files hashed as the install stores them in the bay.
fn slot_files(slot: &str) -> (Vec<u8>, String) {
    use lemnos_slot_store::bootcheck::sha256_hex;
    use lemnos_slot_store::manifest::{BLOCK, SCHEMA, tree_bytes};
    let hash_offset = BLOCK;
    let size = hash_offset + tree_bytes(1);
    let image = vec![0u8; usize::try_from(size).unwrap_or(0)];
    let files: Vec<String> = lemnos_slot_store::bays::ROLES
        .iter()
        .zip(payloads(slot))
        .map(|(role, bytes)| format!("\"{}\":\"{}\"", role.path, sha256_hex(&bytes)))
        .collect();
    let zeros = "0".repeat(64);
    let manifest = format!(
        "{{\"schema\":{SCHEMA},\"id\":\"{slot}\",\"product\":\"route-a\",\"version\":\"1.0\",\"board\":\"{BOARD_ID}\",\
         \"image\":{{\"size\":{size},\"sha256\":\"{}\"}},\
         \"verity\":{{\"root_hash\":\"{zeros}\",\"salt\":\"{zeros}\",\"data_blocks\":1,\"hash_offset\":{hash_offset}}},\
         \"boot\":{{\"files\":{{{}}}}}}}",
        sha256_hex(&image),
        files.join(",")
    );
    (image, manifest)
}

/// Our two slots on the data partition, as the installer leaves them: image, manifest and a good meta.
fn our_state(data: &Path, booted: &str) -> std::io::Result<()> {
    let state = data.join("board/slots");
    let images = data.join("slots");
    fs::create_dir_all(&state)?;
    fs::create_dir_all(&images)?;
    for (seq, slot) in [OURS, NEXT].iter().enumerate() {
        let (image, manifest) = slot_files(slot);
        fs::write(images.join(format!("{slot}.erofs")), image)?;
        fs::write(images.join(format!("{slot}.json")), manifest)?;
        fs::write(
            state.join(format!("{slot}.meta")),
            format!("GOOD=yes\nFAILED=no\nPINNED=no\nSEQ={}\n", seq + 1),
        )?;
    }
    fs::write(state.join("booted"), format!("{booted}\n"))
}

/// The bay the pointers name for `slot` (`bay<N>`, as a pointer file holds it), from the
/// partition's own records.
fn bay_dir(node: &Path, slot: &str) -> Result<String, String> {
    Ok(format!("bay{}", Boot::open(node)?.bay_of(slot)?))
}

/// Every entry under `root`, in path order: its path, kind, mode, owner and bytes.
type Tree = Vec<(PathBuf, bool, u32, u32, Vec<u8>)>;

fn tree(root: &Path) -> std::io::Result<Tree> {
    let mut out = Vec::new();
    if !root.exists() {
        return Ok(out);
    }
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

fn board_text(way_back: bool) -> String {
    let root = root_dir();
    let none = root.join("none");
    let mut text = format!(
        r#"format = "lemnos.board"
schema_version = 1

[board]
id = "{BOARD_ID}"

[[devices]]
id = "slots"
driver = "system-slots"
config = {{ data = "{data}", boot = "{boot}", boot_sources = ["/soc/mmc@7e202000=/soc/mmc@7e202000"], restart_socket = "{helper}", partuuid = "abcd-02", boot_args = "board", reserve_mb = 0, confirm_uptime_s = {UPTIME_S}, device_tree = "{none}", vcio = "{none}" }}
"#,
        data = data_dir().display(),
        boot = root.join("boot").display(),
        none = none.display(),
        helper = helper_socket().display(),
    );
    if way_back {
        text.push_str(&format!(
            r#"
[[devices]]
id = "reboot"
driver = "system-reboot"
config = {{ data = "{data}", boot = "{boot}", boot_sources = ["/soc/mmc@7e202000=/soc/mmc@7e202000"], restart_socket = "{helper}", device_tree = "{none}", vcio = "{VCIO}" }}

[boot]
loop_return = "rpiboot"
"#,
            data = data_dir().display(),
            boot = root.join("boot").display(),
            none = none.display(),
            helper = helper_socket().display(),
        ));
    }
    text
}

/// The firmware mailbox's stand-in (testing/qemu/test_block/lemnos_test_vcio.rs), which the init stanza loads.
const VCIO: &str = "/dev/vcio";

/// One of the stand-in's counts (`orders`, `order`, `asked`), or `None` when it can't be read.
fn vcio_param(name: &str) -> Option<u32> {
    fs::read_to_string(format!("/sys/module/lemnos_test_vcio/parameters/{name}"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Calls the reboot device's `reboot_to` with `target`, over the daemon's socket.
fn reboot_to(client: &Client, target: &str) -> Result<(), Error> {
    let refused = |text: String| Error::Refused {
        status: Status::NOT_FOUND,
        text,
    };
    let (tag, id) = SystemReboot::MANIFEST
        .provides
        .iter()
        .find_map(|info| {
            info.methods
                .iter()
                .find(|m| m.name == "reboot_to")
                .map(|m| (info.tag.as_str(), m.id))
        })
        .ok_or_else(|| refused("no method reboot_to".into()))?;
    let mut handle = client
        .open_tag("reboot", tag)
        .map_err(|e| refused(format!("open {tag}: {e:?}")))?;
    client
        .call(&mut handle, id, Args::new().put(&target).bytes(), &mut [])
        .map(drop)
}

/// The restart into the way back, on the daemon `ours` started with it: see the module comment.
fn way_back(c: &mut Checks, slots: &Slots) -> Result<(), String> {
    let log = log_text();
    c.check(
        log.contains(&format!("root phase: reboot: {VCIO}: held for the restart")),
        &format!(
            "the reboot device's root phase held the mailbox: {}",
            log.trim()
        ),
    );
    let mode = fs::metadata(VCIO).map(|m| (m.uid(), m.mode() & 0o777));
    c.check(
        matches!(mode, Ok((0, 0o600))),
        &format!("the mailbox is root's, 0600, as on a board: {mode:?}"),
    );
    let client = &slots.client;
    // The way back is announced where the bridge reads it: the device's description lists `reboot_to`.
    let listed = client
        .list()
        .map_err(|e| format!("list: {e:?}"))?
        .into_iter()
        .find(|d| d.name == "reboot")
        .ok_or("no reboot device")?;
    let described = client
        .describe(listed.index)
        .map_err(|e| format!("describe reboot: {e:?}"))?;
    c.check(
        described.contracts.iter().any(|contract| {
            contract.tag == "system.reboot/1"
                && contract
                    .methods
                    .iter()
                    .any(|m| m.name == "reboot_to" && m.id == 2)
        }),
        "the reboot device's description lists reboot_to (2) under system.reboot/1",
    );
    let (orders, asked) = (vcio_param("orders"), helper_asked().len());
    let got = reboot_to(client, "uboot");
    c.check(
        status_of(&got) == Some(Status::INVALID_ARGUMENT)
            && vcio_param("orders") == orders
            && helper_asked().len() == asked,
        &format!("another target is refused and nothing is asked: {got:?}"),
    );
    let got = reboot_to(client, "rpiboot");
    c.check(got.is_ok(), &format!("reboot_to rpiboot is taken: {got:?}"));
    c.check(
        vcio_param("orders") == orders.map(|n| n + 1) && vcio_param("order") == Some(3),
        &format!(
            "the firmware was given one reboot order, the flashing mode (3), by the daemon's user through the held node: orders {:?} -> {:?}, order {:?}",
            orders,
            vcio_param("orders"),
            vcio_param("order")
        ),
    );
    c.check(
        helper_asked()[asked..] == ["check", "check", "start"],
        &format!(
            "the helper was asked the way back's check, then the reboot's check and start: {:?}",
            &helper_asked()[asked..]
        ),
    );
    fs::write("/sys/module/lemnos_test_vcio/parameters/refuse", "1\n")
        .map_err(|e| format!("the stand-in's refuse: {e}"))?;
    let (orders, asked) = (vcio_param("orders"), helper_asked().len());
    let got = reboot_to(client, "rpiboot");
    let _ = fs::write("/sys/module/lemnos_test_vcio/parameters/refuse", "0\n");
    c.check(
        status_of(&got) == Some(Status::IO),
        &format!("a mailbox that refuses answers IO: {got:?}"),
    );
    c.check(
        vcio_param("orders") == orders && helper_asked()[asked..] == ["check"],
        &format!(
            "no order taken and nothing restarted, the helper asked its check only: {:?}",
            &helper_asked()[asked..]
        ),
    );
    Ok(())
}

/// The restart helper's directory and its socket, as the board file names the socket. The helper is started by
/// `start_helper`, after the negative step of the ours run: until then the socket is not there.
fn helper_dir() -> PathBuf {
    root_dir().join("restart-helper")
}

fn helper_socket() -> PathBuf {
    helper_dir().join("restart.sock")
}

/// Starts the restart helper the daemon asks for its restarts: the helper's own serve loop on the socket, admitting the
/// daemon's user, with a service manager that stops at once (the trial's restart is a reboot request, and nothing here may
/// reboot the guest). This is the stand-in of the helper's protocol, not the `lemnos-restart` binary: the binary's init
/// backend would send SIGTERM to this guest's PID 1 and wait for a stop that never comes. Started once.
fn start_helper() -> Result<(), String> {
    use lemnos_restart::stand_in::{Manager, StandIn};
    use std::os::unix::fs::PermissionsExt;
    let dir = helper_dir();
    let helper = helper_cell().get_or_init(|| {
        fs::create_dir_all(&dir).map_err(|e| format!("the helper's directory: {e}"))?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("the helper's directory: {e}"))?;
        let stand = StandIn::start_admitting(
            &dir,
            Manager::Stops,
            std::time::Duration::from_secs(5),
            DAEMON,
        )
        .map_err(|e| format!("the helper: {e}"))?;
        fs::set_permissions(&stand.socket, fs::Permissions::from_mode(0o666))
            .map_err(|e| format!("the helper's socket: {e}"))?;
        Ok(stand)
    });
    helper.as_ref().map(drop).map_err(Clone::clone)
}

/// The started helper, once (`start_helper`).
fn helper_cell() -> &'static std::sync::OnceLock<Result<lemnos_restart::stand_in::StandIn, String>>
{
    static HELPER: std::sync::OnceLock<Result<lemnos_restart::stand_in::StandIn, String>> =
        std::sync::OnceLock::new();
    &HELPER
}

/// What the helper's service manager was asked, in order (`check` for each check, `start` for the restart).
fn helper_asked() -> Vec<&'static str> {
    match helper_cell().get() {
        Some(Ok(stand)) => stand.asked(),
        _ => Vec::new(),
    }
}

/// How the daemon is started.
#[derive(Clone, Copy)]
struct Start {
    /// The unit's slots drop-in is given to the launcher.
    drop_in: bool,
    /// The board declares the way back and has a restart device on the mailbox's stand-in (guest only).
    way_back: bool,
}

/// One lemnosd, killed by its PID when dropped.
struct Daemon(Option<Child>);

impl Daemon {
    /// Whether the daemon this check started is still running (it has not exited).
    fn alive(&mut self) -> bool {
        self.0
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(None)))
    }

    fn start(roots: &str, how: Start) -> Result<Self, String> {
        let me = std::env::current_exe().map_err(|e| e.to_string())?;
        let _ = fs::remove_file(socket());
        let board = root_dir().join("board.toml");
        fs::write(&board, board_text(how.way_back)).map_err(|e| format!("the board file: {e}"))?;
        let lemnosd = me.with_file_name("lemnosd-route-a");
        let mut command = if pc() {
            Command::new(&lemnosd)
        } else {
            let mut c = Command::new(me.with_file_name("lemnos-unit-check"));
            c.arg("exec");
            if how.drop_in {
                c.args(["--with", "slots"]);
            }
            c.arg("--runtime-dir").arg(runtime_dir()).arg("--");
            c.arg(&lemnosd);
            c
        };
        command
            .arg("--board")
            .arg(&board)
            .arg("--socket")
            .arg(socket());
        if !pc() {
            command.args([
                "--user",
                &DAEMON.to_string(),
                "--group",
                &DAEMON.to_string(),
            ]);
        }
        command.env("LEMNOSD_TEST_BOOT_ROOTS", roots);
        let log = File::options()
            .create(true)
            .append(true)
            .open(daemon_log())
            .map_err(|e| format!("the daemon's log: {e}"))?;
        // The unit launcher says why it refuses to start on stdout (`say!`), lemnosd on stderr: both
        // go to the daemon's log, so a start that fails says why.
        let stdout = log
            .try_clone()
            .map_err(|e| format!("the daemon's log: {e}"))?;
        command.stderr(Stdio::from(log)).stdout(Stdio::from(stdout));
        let ready = Ready::new()?;
        ready.give(&mut command);
        let mut child = command.spawn().map_err(|e| format!("start lemnosd: {e}"))?;
        match ready.wait(&mut child, READY_WAIT) {
            Ok(()) => Ok(Self(Some(child))),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(format!("{error}; its log: {}", log_text()))
            }
        }
    }
}

impl Daemon {
    /// Kills the daemon by the PID this check started, and reaps it.
    fn stop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop();
    }
}

fn log_text() -> String {
    fs::read_to_string(daemon_log()).unwrap_or_default()
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

/// The slots device over the socket: its handles by contract, and its methods by name.
struct Slots {
    client: Client,
}

impl Slots {
    fn connect() -> Result<Self, String> {
        let client = Client::connect(socket(), "route-a").map_err(|e| format!("connect: {e:?}"))?;
        Ok(Self { client })
    }

    /// The contract tag and method id of the method called `name`.
    fn method(name: &str) -> Option<(&'static str, u16)> {
        for info in SystemSlots::MANIFEST.provides {
            for method in info.methods {
                if method.name == name {
                    return Some((info.tag.as_str(), method.id));
                }
            }
        }
        None
    }

    fn handle(&self, name: &str) -> Result<(Handle, u16), String> {
        let (tag, id) = Self::method(name).ok_or(format!("no method {name}"))?;
        let handle = self
            .client
            .open_tag("slots", tag)
            .map_err(|e| format!("open {tag}: {e:?}"))?;
        Ok((handle, id))
    }

    fn call(&self, name: &str, args: &Args) -> Result<(), Error> {
        let (mut handle, id) = self.handle(name).map_err(|text| Error::Refused {
            status: Status::NOT_FOUND,
            text,
        })?;
        self.client
            .call(&mut handle, id, args.bytes(), &mut [])
            .map(drop)
    }

    fn job(&self, name: &str, args: &Args) -> Result<(), Error> {
        let (mut handle, id) = self.handle(name).map_err(|text| Error::Refused {
            status: Status::NOT_FOUND,
            text,
        })?;
        self.client
            .start_job(&mut handle, id, args.bytes())
            .map(drop)
    }

    /// The text of a method that only fills a buffer (`list`, `info`).
    fn text(&self, name: &str) -> Result<String, String> {
        let (mut handle, id) = self.handle(name)?;
        let mut reply = vec![0u8; 1 << 16];
        let cap = u32::try_from(reply.len()).unwrap_or(u32::MAX).to_le_bytes();
        let n = self
            .client
            .call(&mut handle, id, &cap, &mut reply)
            .map_err(|e| format!("{name}: {e:?}"))?;
        Ok(String::from_utf8_lossy(&reply[..n]).into_owned())
    }

    /// The device's health and its reason, from the daemon's list.
    fn health(&self) -> Result<(DeviceHealth, String), String> {
        let devices = self.client.list().map_err(|e| format!("list: {e:?}"))?;
        devices
            .into_iter()
            .find(|d| d.name == "slots")
            .map(|d| (d.health, d.reason))
            .ok_or_else(|| "no slots device".to_string())
    }
}

fn status_of(got: &Result<(), Error>) -> Option<Status> {
    match got {
        Err(Error::Refused { status, .. }) => Some(*status),
        _ => None,
    }
}

/// The partition's records, read back raw by sector as the boot loader reads them.
fn raw_default(node: &Path) -> Result<Option<String>, String> {
    let file = File::open(node).map_err(|e| e.to_string())?;
    let mut dev = FileSectors::new(file, 0);
    let pointers = Pointers::place(&mut dev).map_err(|(n, e)| format!("{n}: {e:?}"))?;
    pointers
        .read(&mut dev, Pointer::Default)
        .map_err(|e| e.to_string())
}

fn raw_trial(node: &Path) -> Result<Option<String>, String> {
    let file = File::open(node).map_err(|e| e.to_string())?;
    let mut dev = FileSectors::new(file, 0);
    let records = Records::place(&mut dev).map_err(|e| format!("{e:?}"))?;
    records
        .trial(&mut dev)
        .map(|t| t.map(|t| t.slot))
        .map_err(|e| format!("{e:?}"))
}

struct Checks(u32);

impl Checks {
    fn check(&mut self, ok: bool, what: &str) -> bool {
        if ok {
            println!("route-a: ok: {what}");
        } else {
            println!("route-a: FAIL: {what}");
            self.0 += 1;
        }
        ok
    }
}

/// The slots dirs as slot-init leaves them: given to the daemon's user on ours, by its own rule.
fn give_slots_dirs(data: &Path) -> Result<(), String> {
    if pc() {
        return Ok(());
    }
    for dir in [data.join("board/slots"), data.join("slots")] {
        owner::give_dir(&dir, DAEMON, DAEMON, owner::lchown)?;
    }
    Ok(())
}

fn route_a(c: &mut Checks) -> Result<(), String> {
    foreign_run(c, &partition(true)?, "default.txt", true)
}

/// Another system's records with no slots state at all: what slot-init leaves when the records are
/// not ours (it makes no state dirs, `service/slot-init/src/boot.rs`). The unit's slots drop-in must
/// still let lemnosd start, and the device must say why.
fn route_a_no_dirs(c: &mut Checks) -> Result<(), String> {
    foreign_run(c, &partition(true)?, "default.txt", false)
}

/// A partition another layout version made (layout 5 records, `bays::as_layout_5`), with no slots
/// state: the same start as [`route_a_no_dirs`], refused by the record's own form.
fn route_a_layout_5(c: &mut Checks) -> Result<(), String> {
    let image = as_layout_5(&partition(false)?, OURS);
    foreign_run(c, &image, "before layout version 6", false)
}

/// One foreign start: lemnosd is started as its unit starts it on `image`, with the slots state
/// as `state` says (the other system's, or none at all). `why` is the reason the records give.
fn foreign_run(c: &mut Checks, image: &[u8], why: &str, state: bool) -> Result<(), String> {
    let data = data_dir();
    if state {
        foreign_state(&data).map_err(|e| format!("the state: {e}"))?;
    }
    let (node, roots) = boot_chain(image).map_err(|e| format!("the boot chain: {e}"))?;
    let before = tree(&data).map_err(|e| e.to_string())?;
    let mut daemon = Daemon::start(
        &roots,
        Start {
            drop_in: true,
            way_back: false,
        },
    )?;
    let slots = Slots::connect()?;
    let after_start = tree(&data).map_err(|e| e.to_string())?;
    c.check(
        after_start == before,
        &format!(
            "the state dir is byte-identical after start {}",
            first_difference(&before, &after_start)
        ),
    );
    if !pc() {
        let log = log_text();
        c.check(
            log.contains("root phase: slots: ") && log.contains("handed over read only"),
            &format!(
                "the root phase held the partition read only: {}",
                log.trim()
            ),
        );
    }
    thread::sleep(RUN);
    c.check(
        daemon.alive(),
        "lemnosd is still running after the ticks: it did not exit and was not restarted",
    );
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
    let health = slots.health()?;
    c.check(
        health.0 == DeviceHealth::Degraded && health.1.contains(foreign),
        &format!("health is degraded, foreign: {health:?}"),
    );
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
        let got = if *job {
            slots.job(name, args)
        } else {
            slots.call(name, args)
        };
        c.check(
            status_of(&got) == Some(Status::FAILED_PRECONDITION),
            &format!("{name} is refused: {got:?}"),
        );
    }
    let list = slots.text("list")?;
    c.check(
        list.contains(&format!("\"foreign\":\"{foreign}: ")) && list.contains(why),
        &format!("list says foreign, with the reason: {list}"),
    );
    let info = slots.text("info")?;
    c.check(
        info.contains(&format!("\"foreign\":\"{foreign}: ")) && info.contains(why),
        &format!("info says foreign, with the reason: {info}"),
    );
    thread::sleep(Duration::from_secs(2));
    drop(slots);
    drop(daemon);
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

/// Our partition with [`NEXT`] installed in a free bay, as the installer leaves it, and its node.
fn our_partition() -> Result<(PathBuf, String), String> {
    let (node, roots) =
        boot_chain(&partition(false)?).map_err(|e| format!("the boot chain: {e}"))?;
    let payloads = payloads(NEXT);
    let refs: Vec<&[u8]> = payloads.iter().map(Vec::as_slice).collect();
    Boot::open(&node)?.install_slot(NEXT, &refs)?;
    Ok((node, roots))
}

fn ours(c: &mut Checks) -> Result<(), String> {
    let data = data_dir();
    our_state(&data, OURS).map_err(|e| format!("the state: {e}"))?;
    give_slots_dirs(&data)?;
    let (node, roots) = our_partition()?;
    let mut daemon = Daemon::start(
        &roots,
        Start {
            drop_in: true,
            way_back: !pc(),
        },
    )?;
    let slots = Slots::connect()?;
    if !pc() {
        let log = log_text();
        c.check(
            log.contains("root phase: slots: ") && log.contains("handed over read-write"),
            &format!(
                "the root phase handed the partition over read-write: {}",
                log.trim()
            ),
        );
    }
    let health = slots.health()?;
    c.check(
        health.0 == DeviceHealth::Ok,
        &format!("on ours the health is ok: {health:?}"),
    );
    // A device in a call reads Ok to the host, so the driver's own words are read too.
    let info = slots.text("info")?;
    c.check(
        info.contains("\"state_unwritable\":\"\"") && info.contains("\"write_failed\":\"\""),
        &format!("on ours info says the state is writable: {info}"),
    );
    let got = slots.call("confirm", &Args::new());
    c.check(got.is_ok(), &format!("on ours confirm is taken: {got:?}"));
    let state = data.join("board/slots/state.env");
    c.check(
        fs::metadata(&state).is_ok_and(|m| pc() || m.uid() == DAEMON),
        "the daemon's user wrote the state",
    );
    // Before the helper starts, the trial is refused by its reason, and nothing is written.
    let before = fs::read(&node).map_err(|e| format!("the partition: {e}"))?;
    let refused = slots.call(
        "trial",
        &Args::new().put(&NEXT).put(&false).put(&false).put(&false),
    );
    c.check(
        refused.is_err(),
        &format!("before the restart helper starts, trial {NEXT} is refused: {refused:?}"),
    );
    let list = slots.text("list")?;
    c.check(
        list.contains("the restart helper at") && list.contains("No such file or directory"),
        &format!("the refusal names the helper's socket: {list}"),
    );
    c.check(
        fs::read(&node).is_ok_and(|now| now == before) && raw_trial(&node).ok().flatten().is_none(),
        "nothing written: the boot partition is byte-identical and no trial record",
    );
    start_helper()?;
    let got = slots.call(
        "trial",
        &Args::new().put(&NEXT).put(&false).put(&false).put(&false),
    );
    if !c.check(got.is_ok(), &format!("trial {NEXT} is taken: {got:?}")) {
        println!("route-a: info: {}", slots.text("info").unwrap_or_default());
        println!("route-a: list: {}", slots.text("list").unwrap_or_default());
        println!("route-a: log: {}", log_text());
    }
    let trial = raw_trial(&node);
    c.check(
        trial.as_ref().is_ok_and(|t| t.as_deref() == Some(NEXT)),
        &format!("the partition's trial record names {NEXT}, read back raw: {trial:?}"),
    );
    // The trial's check, then its restart: a reboot request, which the helper checks the manager for again before it
    // starts it (serve.rs), and which this helper's manager answers without rebooting the guest. Nothing else is asked.
    c.check(
        helper_asked() == ["check", "check", "start"],
        &format!(
            "the helper's manager was asked the trial's check, the reboot's check and its start, and nothing else: {:?}",
            helper_asked()
        ),
    );
    drop(slots);
    daemon.stop();
    // The trial boot, as slot-init records it: the slot the board came up on.
    fs::write(data.join("board/slots/booted"), format!("{NEXT}\n")).map_err(|e| e.to_string())?;
    daemon = Daemon::start(
        &roots,
        Start {
            drop_in: true,
            way_back: !pc(),
        },
    )?;
    let slots = Slots::connect()?;
    let got = slots.call("confirm", &Args::new());
    c.check(
        got.is_ok(),
        &format!("on the trial slot confirm is taken: {got:?}"),
    );
    let next_bay = bay_dir(&node, NEXT)?;
    let ours_bay = bay_dir(&node, OURS)?;
    let default = raw_default(&node);
    let trial = raw_trial(&node);
    c.check(
        default.as_ref().is_ok_and(|d| d.as_deref() == Some(next_bay.as_str()))
            && trial.as_ref().is_ok_and(Option::is_none),
        &format!(
            "the confirm switched default.txt to {NEXT} ({next_bay}) and cleared the trial, read back raw: {default:?} {trial:?}"
        ),
    );
    let got = slots.call("set_default", &Args::new().put(&OURS).put(&false));
    c.check(
        got.is_ok(),
        &format!("set_default {OURS} is taken: {got:?}"),
    );
    let default = raw_default(&node);
    c.check(
        default
            .as_ref()
            .is_ok_and(|d| d.as_deref() == Some(ours_bay.as_str())),
        &format!("default.txt names {OURS} ({ours_bay}), read back raw: {default:?}"),
    );
    if !pc() {
        way_back(c, &slots)?;
    }
    drop(slots);
    drop(daemon);
    Ok(())
}

/// Ours, but the dirs are root's and the unit has no slots drop-in: the image left out both.
fn image_side(c: &mut Checks) -> Result<(), String> {
    if pc() {
        return Ok(());
    }
    let data = data_dir();
    our_state(&data, OURS).map_err(|e| format!("the state: {e}"))?;
    let (node, roots) = our_partition()?;
    let before = fs::read(&node).map_err(|e| e.to_string())?;
    let daemon = Daemon::start(
        &roots,
        Start {
            drop_in: false,
            way_back: false,
        },
    )?;
    let slots = Slots::connect()?;
    let word = lemnos_driver_system_slots::STATE_UNWRITABLE;
    // A device in a call reads Ok until the call returns (the host does not know yet): the first
    // health that is known is the one checked, within the bound.
    let until = Instant::now() + SETTLE;
    let health = loop {
        let health = slots.health()?;
        if health.0 != DeviceHealth::Ok || Instant::now() >= until {
            break health;
        }
        thread::sleep(Duration::from_millis(100));
    };
    c.check(
        health.0 == DeviceHealth::Degraded && health.1.contains(word),
        &format!("with root's dirs and no drop-in the health names it: {health:?}"),
    );
    let got = slots.call("set_default", &Args::new().put(&NEXT).put(&false));
    c.check(
        status_of(&got) == Some(Status::FAILED_PRECONDITION),
        &format!("set_default is refused before anything is written: {got:?}"),
    );
    let info = slots.text("info")?;
    c.check(
        info.contains("\"state_unwritable\":\"/data/board/slots: "),
        &format!("info names the dir and why: {info}"),
    );
    drop(slots);
    drop(daemon);
    c.check(
        fs::read(&node).is_ok_and(|now| now == before),
        "the boot partition is unchanged",
    );
    Ok(())
}

/// Clears what one run stood in: the guest's /run and /dev are small.
fn clear() {
    let _ = fs::remove_dir_all(root_dir());
    let _ = fs::remove_dir_all(dev_dir());
    let data = data_dir();
    let _ = fs::remove_dir_all(data.join("board"));
    let _ = fs::remove_dir_all(data.join("slots"));
    let _ = fs::create_dir_all(root_dir());
    let _ = fs::create_dir_all(runtime_dir());
}

fn main() -> ExitCode {
    let mut c = Checks(0);
    for (name, run) in [
        ("route A", route_a as fn(&mut Checks) -> Result<(), String>),
        ("route A without the state dirs", route_a_no_dirs),
        ("route A on layout 5", route_a_layout_5),
        ("ours", ours),
        ("image-side", image_side),
    ] {
        clear();
        if let Err(why) = run(&mut c) {
            c.check(false, &format!("{name}: {why}"));
            println!("route-a: the daemon's log: {}", log_text().trim());
        }
    }
    clear();
    let _ = fs::remove_dir_all(root_dir());
    let _ = fs::remove_dir_all(runtime_dir());
    if c.0 == 0 {
        println!("route-a: PASS");
        ExitCode::SUCCESS
    } else {
        println!("route-a: {} failed", c.0);
        ExitCode::FAILURE
    }
}
