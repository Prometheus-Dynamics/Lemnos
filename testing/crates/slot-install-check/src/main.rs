#![allow(clippy::print_stdout, clippy::print_stderr)]

//! The slot install check (test only, never shipped). The QEMU guest's client installs a bundle through
//! the real lemnosd's socket, the way any program does, and reads the result back; the host side
//! makes the payload and the rootfs the bundle is built from (`testing/qemu/run-slot-install.sh`).
//!
//!   lemnos-slot-install-check payload DIR SLOT          the five build roles, a realistic boot set
//!   lemnos-slot-install-check rootfs IMAGE TABLE BLOCKS the erofs stand-in with its verity tree, and the table
//!   lemnos-slot-install-check give DATA UID GID        as the guest's slot-init: the daemon's dirs on the data
//!       partition given to its user by slot-init's own rule (`lemnos_slot_store::owner::give_daemon_dirs`)
//!   lemnos-slot-install-check install SOCKET BUNDLE SLOT BOOT_NODE
//!       as the guest's client: `install` then `write` in chunks over the socket, the job's end, then `info`
//!       and `list` (the install's record) and the boot partition read raw (the bay the slot is in).
//!
//! Each check prints one line, `SLOT-INSTALL-GUEST PASS n TEXT` or `... FAIL n TEXT`, and the run's last
//! line is `SLOT-INSTALL-GUEST DONE pass` or `DONE fail`. The install's elapsed time and the job's end
//! are printed too, so the host reads them from the console.

use std::fs;
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use lemnos_client::{Client, Handle};
use lemnos_core::{Driver, Encode, Status, Writer};
use lemnos_driver_system_slots::SystemSlots;
use lemnos_firmware_check::sample;
use lemnos_slot_store::bays::ROLES;
use lemnos_slot_store::boot_raw::Boot;
use lemnos_slot_store::bundle::hex;
use lemnos_slot_store::verity;

/// The kernel's bytes: with the device tree and the overlay, the boot set is about 7.7 MB, as a Pi 5 slot's is.
const KERNEL_BYTES: usize = 7_500_000;
/// The job's chunk size: one write call per chunk.
const CHUNK: usize = 64 << 10;
/// How long the install's end may take before the check gives up (the daemon's own bound is the job).
const JOB_WAIT: Duration = Duration::from_secs(900);

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("payload") if args.len() == 3 => payload(Path::new(&args[1]), &args[2]),
        Some("rootfs") if args.len() == 4 => rootfs(
            Path::new(&args[1]),
            Path::new(&args[2]),
            args[3].parse().map_err(|_| "BLOCKS is a number".to_string()),
        ),
        Some("give") if args.len() == 4 => give(Path::new(&args[1]), &args[2], &args[3]),
        Some("install") if args.len() == 5 => install(&args[1], Path::new(&args[2]), &args[3], Path::new(&args[4])),
        _ => Err("usage: payload DIR SLOT | rootfs IMAGE TABLE BLOCKS | give DATA UID GID | install SOCKET BUNDLE SLOT BOOT_NODE".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("slot-install-check: {why}");
            ExitCode::FAILURE
        }
    }
}

/// slot-init's step, stood in by the same code: each of the daemon's dirs under `data` given to
/// `uid:gid` (the dirs alone), one line each. Any refusal fails the step.
fn give(data: &Path, uid: &str, gid: &str) -> Result<(), String> {
    let uid: u32 = uid.parse().map_err(|_| "UID is a number".to_string())?;
    let gid: u32 = gid.parse().map_err(|_| "GID is a number".to_string())?;
    let layout = lemnos_slot_store::store::Layout::new(data, "/boot");
    let mut failed = Vec::new();
    for (dir, given) in lemnos_slot_store::owner::give_daemon_dirs(
        &layout,
        uid,
        gid,
        lemnos_slot_store::owner::lchown,
    ) {
        match given {
            Ok(how) => println!("slot-install-check: give {}: {how:?}", dir.display()),
            Err(why) => failed.push(format!("{}: {why}", dir.display())),
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(failed.join("; "))
    }
}

/// The five build roles for `slot`, in `DIR`, each named by its role (`kernel`, `initramfs`, `device-tree`,
/// `cmdline`, `board-overlay`), as `boot-partition --payload` and `bundle --payload` take them: the kernel and the
/// initramfs at their sizes, a device tree, the command line with the root hash's hole (`bundle` fills it), and
/// the board overlay.
/// The slot's kernel: an arm64 Image of [`KERNEL_BYTES`] with the slot's name written through it, so
/// two slots' kernels differ and an install copies its kernel into the boot-file store whole.
fn kernel(slot: &str) -> Vec<u8> {
    let mut image = sample::arm64_image(KERNEL_BYTES);
    for at in (0x1000..image.len().saturating_sub(slot.len())).step_by(0x10000) {
        image[at..at + slot.len()].copy_from_slice(slot.as_bytes());
    }
    image
}

fn payload(dir: &Path, slot: &str) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    // A plain cpio: the firmware-layout check reads it to its trailer, so nothing may follow.
    let initramfs = sample::cpio();
    let cmdline = format!(
        "os.slot={slot} os.roothash=@roothash@ os.src./soc/mmc@7e202000=/soc/mmc@7e202000 panic=5\n"
    );
    let bytes: [Vec<u8>; 5] = [
        kernel(slot),
        initramfs,
        sample::base_dtb(),
        cmdline.into_bytes(),
        sample::overlay("/soc", "gpio"),
    ];
    for (role, bytes) in ROLES.iter().zip(bytes) {
        fs::write(dir.join(role.role), bytes).map_err(|e| format!("{}: {e}", role.role))?;
    }
    // The firmware files a board declares (`overlay_map.dtb`, `hat_map.dtb`), beside the payloads, as
    // `--firmware` takes them.
    let firmware = dir.parent().unwrap_or(Path::new(".")).join("firmware");
    fs::create_dir_all(&firmware).map_err(|e| e.to_string())?;
    fs::write(firmware.join("overlay_map.dtb"), sample::map_dtb()).map_err(|e| e.to_string())?;
    fs::write(firmware.join("hat_map.dtb"), sample::map_dtb()).map_err(|e| e.to_string())?;
    println!(
        "payload: {} in {}",
        ROLES.map(|r| r.role).join(" "),
        dir.display()
    );
    Ok(())
}

/// The erofs stand-in: a superblock the installer reads (magic, block count), its data blocks, and the
/// verity tree appended; the table text is the one `lemnos-ctl bundle` reads (the same as its tests use).
fn rootfs(image: &Path, table: &Path, blocks: Result<usize, String>) -> Result<(), String> {
    let blocks = blocks?;
    let mut data: Vec<u8> = (0..blocks * 4096).map(|i| (i % 249) as u8).collect();
    data[1024..1028].copy_from_slice(&[0xe2, 0xe1, 0xf5, 0xe0]);
    data[1024 + 12] = 12;
    data[1024 + 36..1024 + 40].copy_from_slice(&(blocks as u32).to_le_bytes());
    let salt: [u8; 32] = std::array::from_fn(|i| i as u8);
    let tree = verity::tree_of(salt, &data)?;
    let mut whole = data.clone();
    whole.extend_from_slice(&tree.tree);
    fs::write(image, &whole).map_err(|e| e.to_string())?;
    let text = format!(
        "hash_type 1\nalgorithm sha256\nroot_hash {}\nsalt {}\nuuid 00000000-0000-4000-8000-000000000000\n\
         data_blocks {blocks}\ndata_block_size 4096\nhash_block_size 4096\nhash_offset {}\ntree_bytes {}\n\
         image_bytes {}\nhash_start_block {blocks}\ntable 0 {} verity 1 /dev/x /dev/x 4096 4096 {blocks} {blocks} sha256 {} {}\n",
        hex(&tree.root_hash),
        hex(&salt),
        data.len(),
        tree.tree.len(),
        whole.len(),
        blocks * 8,
        hex(&tree.root_hash),
        hex(&salt),
    );
    fs::write(table, text).map_err(|e| e.to_string())?;
    println!("rootfs: {} bytes, data {} blocks", whole.len(), blocks);
    Ok(())
}

/// Where the job's window (wall-clock seconds, the start and the end) is written for the guest's wrapper.
const WINDOW: &str = "/var/lib/slot-install/window";

/// The guest's wall clock, in seconds with microseconds.
fn unix_seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// The check's lines: each is a PASS or a FAIL, with its number.
struct Checks {
    n: u32,
    failed: u32,
}

impl Checks {
    fn new() -> Self {
        Self { n: 0, failed: 0 }
    }

    fn check(&mut self, ok: bool, text: &str) {
        self.n += 1;
        if ok {
            println!("SLOT-INSTALL-GUEST PASS {} {text}", self.n);
        } else {
            println!("SLOT-INSTALL-GUEST FAIL {} {text}", self.n);
            self.failed += 1;
        }
    }
}

/// The arguments, encoded as the generated client encodes them.
struct Args(Vec<u8>, usize);

impl Args {
    fn new() -> Self {
        Self(vec![0u8; CHUNK + 256], 0)
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

    /// The text of a method that fills a buffer (`info`, `list`).
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
}

/// The install over the socket, the way a sender does it: `install` starts the job, `write` delivers the
/// bytes from each offset, and the job's end is waited for. Then the record and the boot partition.
fn install(socket: &str, bundle: &Path, slot: &str, boot_node: &Path) -> Result<(), String> {
    let bytes = fs::read(bundle).map_err(|e| format!("{}: {e}", bundle.display()))?;
    let client = Client::connect(socket, "slot-install").map_err(|e| format!("connect: {e:?}"))?;
    let slots = Slots { client };
    let mut c = Checks::new();

    // The daemon opens its socket before the slots driver is bound (the first bind is a retry away),
    // and a call before that is refused NO_DEVICE (-19). Wait, bounded, until the driver answers, so
    // the install is not refused for the bind; nothing is retried after the install starts.
    let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        match slots.text("info") {
            Ok(_) => break,
            Err(e) if e.contains("Status(-19)") && std::time::Instant::now() < until => {
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            Err(e) => return Err(format!("the slots driver did not bind: {e}")),
        }
    }
    let (mut install_handle, install_id) = slots.handle("install")?;
    let (mut write_handle, write_id) = slots.handle("write")?;
    let args = Args::new()
        .put(&(bytes.len() as u64))
        .put(&false)
        .put(&false);
    let started = Instant::now();
    let from = unix_seconds();
    let job = slots
        .client
        .start_job(&mut install_handle, install_id, args.bytes())
        .map_err(|e| format!("install: {e:?}"))?;
    c.check(
        true,
        &format!(
            "install started as job {job:#x} for {} bytes of {slot}",
            bytes.len()
        ),
    );

    let mut offset = 0usize;
    let mut refused = None;
    while offset < bytes.len() {
        let end = (offset + CHUNK).min(bytes.len());
        let args = Args::new().put(&(offset as u64)).put(&bytes[offset..end]);
        if let Err(e) = slots
            .client
            .call(&mut write_handle, write_id, args.bytes(), &mut [])
        {
            refused = Some(format!("write at {offset}: {e:?}"));
            break;
        }
        offset = end;
    }
    let written = started.elapsed();
    c.check(
        refused.is_none() && offset == bytes.len(),
        &format!(
            "the bundle was written: {offset} of {} bytes in {} ms{}",
            bytes.len(),
            written.as_millis(),
            refused
                .as_deref()
                .map_or(String::new(), |r| format!(" ({r})"))
        ),
    );

    let end = slots.client.wait_job(job, JOB_WAIT, &mut |_, _| {});
    let elapsed = started.elapsed();
    let to = unix_seconds();
    // The job's window on the guest's wall clock: the wrapper reads the daemon's journal lines in it.
    fs::write(WINDOW, format!("{from:.6} {to:.6}\n")).map_err(|e| format!("{WINDOW}: {e}"))?;
    println!("SLOT-INSTALL-GUEST WINDOW from={from:.6} to={to:.6}");
    let ended = end.is_some();
    let (status, _result) = end.unwrap_or((Status(i32::MIN), Vec::new()));
    println!(
        "SLOT-INSTALL-GUEST JOB job={job:#x} status={} elapsed_ms={} ended={ended}",
        status.0,
        elapsed.as_millis(),
    );
    c.check(
        status == Status::OK,
        &format!(
            "the install's job ended committed (status {}, 0 is OK; -4 is INTERRUPTED)",
            status.0
        ),
    );

    let info = slots.text("info")?;
    let last = last_attempt(&info);
    c.check(
        last.contains("\"state\":\"installed\"") && last.contains(&format!("\"slot\":\"{slot}\"")),
        &format!("info's last attempt is installed for {slot}: {last}"),
    );
    let list = slots.text("list")?;
    let entry = list.contains(&format!("\"id\":\"{slot}\"")) && list.contains("\"intact\":true");
    c.check(entry, &format!("list holds {slot} intact"));

    let bay = Boot::open(boot_node).and_then(|mut boot| boot.bay_of(slot));
    match bay {
        Ok(bay) => c.check(
            true,
            &format!("the boot partition holds {slot} in bay {bay}"),
        ),
        Err(e) => c.check(false, &format!("the boot partition holds {slot}: {e}")),
    }
    println!(
        "SLOT-INSTALL-GUEST DONE {}",
        if c.failed == 0 { "pass" } else { "fail" }
    );
    if c.failed == 0 {
        Ok(())
    } else {
        Err(format!("{} checks failed", c.failed))
    }
}

/// The `last_attempt` object of `info`, as text (the whole text when it has none).
fn last_attempt(info: &str) -> &str {
    info.find("\"last_attempt\"").map_or(info, |at| &info[at..])
}
