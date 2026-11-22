//! `lemnos-ctl data-partition` end to end, against the repository's reference board: a signed bundle
//! and the boot image made from its payloads go in, the tree comes out, `--verify` passes it, and the
//! real slot driver over a copy of it reads the slot as installed, confirmed and default. Each way of
//! breaking the inputs is refused by name, and leaves no output directory behind. The umask cases run
//! the binary itself under `sh -c 'umask N; ...'`, so the test process's own umask is never changed.

use lemnos_contract_system_slots::Slots as _;
use lemnos_driver_system_slots::boot::{Restarter, Tryboot};
use lemnos_driver_system_slots::{SystemSlots, parse_keys};
use lemnos_restart::stand_in::{Manager, StandIn};
use lemnos_slot_store::store::{Layout, Meta, State};
use lemnos_slot_store::tar::{Kind, Sink, Tar, write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

#[path = "support/pi.rs"]
mod pi;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn socs() -> String {
    repo().join("socs").display().to_string()
}

fn run(args: &[&str]) -> (Result<(), String>, String) {
    let args = pi::with_firmware(args);
    let mut out = Vec::new();
    let r = lemnos_ctl::run(&args, &mut out);
    (r, String::from_utf8(out).unwrap())
}

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("data-partition-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The documented test key: the seed `0x07` repeated, and its public key and key id.
const SEED7: [u8; 32] = [7; 32];
const KEY_ID7: &str = "fe812c12f3ab4ce6ac5db69ac352f906cb1b11ef43fb33e252ef7ff552263889";
const PUBKEY7: &str = "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c";
/// Another documented seed, whose key the test's key list does not hold.
const SEED8: [u8; 32] = [8; 32];

const TIME: &str = "2026-11-14T20:00:00Z";
/// Eight GiB of free room: far more than a first slot of this size needs past the reserve.
const DATA_SIZE: &str = "8589934592";

fn file_with_mode(dir: &Path, name: &str, bytes: &[u8], mode: u32) -> String {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    path.display().to_string()
}

/// The reference board, with `tryboot` as its record. `id` replaces the board id when given.
fn board(dir: &Path, id: Option<&str>) -> String {
    let mut text = std::fs::read_to_string(repo().join("testing/boards/reference.toml")).unwrap();
    if let Some(id) = id {
        text = text.replacen("id = \"reference\"", &format!("id = \"{id}\""), 1);
    }
    let path = dir.join("board.toml");
    std::fs::write(&path, text).unwrap();
    path.display().to_string()
}

fn erofs(blocks: usize) -> Vec<u8> {
    let mut image: Vec<u8> = (0..blocks * 4096).map(|i| (i % 249) as u8).collect();
    image[1024..1028].copy_from_slice(&[0xe2, 0xe1, 0xf5, 0xe0]);
    image[1024 + 12] = 12;
    image[1024 + 36..1024 + 40].copy_from_slice(&(blocks as u32).to_le_bytes());
    image
}

/// The root image with its verity tree appended, and the builder's table text.
fn rootfs(dir: &Path, blocks: usize) -> (String, String) {
    let data = erofs(blocks);
    let salt: [u8; 32] = std::array::from_fn(|i| i as u8);
    let tree = lemnos_slot_store::verity::tree_of(salt, &data).unwrap();
    let mut whole = data.clone();
    whole.extend_from_slice(&tree.tree);
    let image = dir.join("rootfs.erofs");
    std::fs::write(&image, &whole).unwrap();
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    let table = format!(
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
    let path = dir.join("verity.txt");
    std::fs::write(&path, table).unwrap();
    (image.display().to_string(), path.display().to_string())
}

/// The payload directory `tree --cmdline-slot` writes for `slot`.
fn payload(dir: &Path, board: &str, slot: &str) -> String {
    let p = dir.join("payload");
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    pi::write_payload(&p, pi::CMDLINE, 4096);
    pi::write_firmware(&pi::firmware_beside(&p), &[]);
    let line = p.join("cmdline");
    let (r, _) = run(&[
        "tree",
        "--socs",
        &socs(),
        board,
        "--slot",
        slot,
        "--cmdline-slot",
        line.to_str().unwrap(),
    ]);
    r.unwrap();
    p.display().to_string()
}

/// One first slot, made from the reference board: its bundle, its final payloads, the boot image
/// built from them, and the keys file that names the test key.
struct Slot {
    dir: PathBuf,
    board: String,
    bundle: String,
    payloads: String,
    image: String,
    keys: String,
}

fn slot_in(name: &str) -> Slot {
    let dir = scratch(name);
    let board = board(&dir, None);
    let payload = payload(&dir, &board, "a1");
    let (rootfs, table) = rootfs(&dir, 300);
    let seed = file_with_mode(&dir, "signing.seed", &SEED7, 0o600);
    let payloads = dir.join("final").display().to_string();
    let bundle = dir.join("a1.bundle").display().to_string();
    let (r, out) = run(&[
        "bundle",
        "--layout-of",
        &board,
        "--socs",
        &socs(),
        "--slot",
        "a1",
        "--payload",
        &payload,
        "--rootfs",
        &rootfs,
        "--verity-table",
        &table,
        "--product",
        "an-os",
        "--version",
        "1.0",
        "--key-file",
        &seed,
        "--payload-out",
        &payloads,
        "--out",
        &bundle,
    ]);
    r.unwrap_or_else(|e| panic!("{e}\n{out}"));
    let keys = file_with_mode(&dir, "keys", PUBKEY7.as_bytes(), 0o644);
    let image = boot_image(&dir, &board, "a1", &payloads);
    Slot {
        dir,
        board,
        bundle,
        payloads,
        image,
        keys,
    }
}

/// The boot partition for `slot`, built from `payloads`, at the smallest whole MiB that holds it.
fn boot_image(dir: &Path, board: &str, slot: &str, payloads: &str) -> String {
    let (r, min) = run(&[
        "boot-partition",
        "--layout-of",
        board,
        "--socs",
        &socs(),
        "--min-size",
    ]);
    r.unwrap();
    let size = min.trim().parse::<u64>().unwrap().div_ceil(1 << 20) << 20;
    let image = dir.join(format!("boot-{slot}.img")).display().to_string();
    let (r, out) = run(&[
        "boot-partition",
        "--layout-of",
        board,
        "--socs",
        &socs(),
        "--slot",
        slot,
        "--payload",
        payloads,
        "--size",
        &size.to_string(),
        "--out",
        &image,
    ]);
    r.unwrap_or_else(|e| panic!("{e}\n{out}"));
    image
}

/// `data-partition` with the usual inputs, writing a new directory `out`.
fn write_partition(slot: &Slot, out: &Path, extra: &[&str]) -> (Result<(), String>, String) {
    let out = out.display().to_string();
    let socs = socs();
    let mut args = vec![
        "data-partition",
        "--layout-of",
        &slot.board,
        "--socs",
        &socs,
        "--install",
        &slot.bundle,
        "--boot-image",
        &slot.image,
        "--keys",
        &slot.keys,
        "--time",
        TIME,
        "--data-size",
        DATA_SIZE,
        "--out",
        &out,
    ];
    args.extend_from_slice(extra);
    run(&args)
}

/// Every path under `root` with its bytes (`None` for a directory) and its mode.
fn tree(root: &Path) -> std::collections::BTreeMap<String, (Option<Vec<u8>>, u32)> {
    let mut out = std::collections::BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap().display().to_string();
            let mode = entry.metadata().unwrap().permissions().mode() & 0o7777;
            if path.is_dir() {
                out.insert(rel, (None, mode));
                stack.push(path);
            } else {
                out.insert(rel, (Some(std::fs::read(&path).unwrap()), mode));
            }
        }
    }
    out
}

fn copy_tree(from: &Path, to: &Path) {
    for (rel, (bytes, _)) in tree(from) {
        let dest = to.join(&rel);
        match bytes {
            None => std::fs::create_dir_all(&dest).unwrap(),
            Some(bytes) => {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent).unwrap();
                }
                std::fs::write(&dest, bytes).unwrap();
            }
        }
    }
}

#[derive(Default)]
struct Members(Vec<(String, Vec<u8>)>);

impl Sink for Members {
    fn begin(&mut self, name: &str, kind: Kind, _: u64) -> Result<(), String> {
        let name = if kind == Kind::Dir {
            format!("{name}/")
        } else {
            name.to_string()
        };
        self.0.push((name, Vec::new()));
        Ok(())
    }
    fn data(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.0.last_mut().unwrap().1.extend_from_slice(bytes);
        Ok(())
    }
    fn end(&mut self) -> Result<(), String> {
        Ok(())
    }
}

/// A bundle with `slot.sig` replaced by `sig`, every other member unchanged.
fn with_signature(dir: &Path, name: &str, bundle: &str, sig: &[u8]) -> String {
    let mut m = Members::default();
    Tar::default()
        .feed(&std::fs::read(bundle).unwrap(), &mut m)
        .unwrap();
    let members: Vec<(String, Vec<u8>)> =
        m.0.into_iter()
            .map(|(n, b)| {
                if n == "slot.sig" {
                    (n, sig.to_vec())
                } else {
                    (n, b)
                }
            })
            .collect();
    let borrowed: Vec<(&str, &[u8])> = members
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_slice()))
        .collect();
    let path = dir.join(name);
    std::fs::write(&path, write::archive(&borrowed)).unwrap();
    path.display().to_string()
}

fn slot_sig(bundle: &str) -> Vec<u8> {
    let mut m = Members::default();
    Tar::default()
        .feed(&std::fs::read(bundle).unwrap(), &mut m)
        .unwrap();
    m.0.into_iter()
        .find(|(n, _)| n == "slot.sig")
        .expect("the bundle has a slot.sig")
        .1
}

/// The slot's bundle again, signed with `seed` or unsigned (`None`), named `name` in its directory.
fn bundle_with(slot: &Slot, name: &str, seed: Option<[u8; 32]>) -> String {
    let out = slot.dir.join(name).display().to_string();
    let (rootfs, table) = rootfs(&slot.dir, 300);
    let payload = slot.dir.join("payload").display().to_string();
    let seed_file;
    let mut args = vec![
        "bundle",
        "--layout-of",
        &slot.board,
        "--socs",
        Box::leak(socs().into_boxed_str()),
        "--slot",
        "a1",
        "--payload",
        &payload,
        "--rootfs",
        &rootfs,
        "--verity-table",
        &table,
        "--product",
        "an-os",
        "--version",
        "1.0",
        "--out",
        &out,
    ];
    if let Some(seed) = seed {
        seed_file = file_with_mode(&slot.dir, &format!("{name}.seed"), &seed, 0o600);
        args.extend(["--key-file", &seed_file]);
    }
    let (r, text) = run(&args);
    r.unwrap_or_else(|e| panic!("{e}\n{text}"));
    out
}

/// The reference board with its system-slots device's `signature` set to `value`.
fn board_signed(dir: &Path, value: &str) -> String {
    let text = std::fs::read_to_string(repo().join("testing/boards/reference.toml")).unwrap();
    let text = text.replacen(
        "config = { data = \"/data\", boot = \"/boot/firmware\", record = \"tryboot\"",
        &format!("config = {{ signature = \"{value}\", data = \"/data\", boot = \"/boot/firmware\", record = \"tryboot\""),
        1,
    );
    let path = dir.join(format!("board-{value}.toml"));
    std::fs::write(&path, text).unwrap();
    path.display().to_string()
}

/// Runs `f`, polling `path` every 200 microseconds, and reports whether it ever existed meanwhile.
fn exists_during<T>(path: &Path, f: impl FnOnce() -> T) -> (T, bool) {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let stop = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(AtomicBool::new(false));
    let (p, stop2, seen2) = (path.to_path_buf(), stop.clone(), seen.clone());
    let poller = std::thread::spawn(move || {
        while !stop2.load(Ordering::SeqCst) {
            if p.exists() {
                seen2.store(true, Ordering::SeqCst);
            }
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
    });
    let out = f();
    stop.store(true, Ordering::SeqCst);
    poller.join().unwrap();
    (out, seen.load(Ordering::SeqCst))
}

/// The driver over a copy of `data` and `image`, the way the device would load them.
fn driver(dir: &Path, data: &Path, image: &str) -> (SystemSlots, Layout) {
    let read = dir.join("read");
    let _ = std::fs::remove_dir_all(&read);
    std::fs::create_dir_all(&read).unwrap();
    copy_tree(data, &read.join("data"));
    let node = read.join("boot.img");
    std::fs::copy(image, &node).unwrap();
    let layout = Layout::new(read.join("data"), read.join("boot-mount"));
    let tryboot = Tryboot {
        boot: read.join("boot-mount"),
        node: Ok(node),
        restarter: Restarter::helper(
            StandIn::start(&read, Manager::Stops, Duration::from_secs(1))
                .unwrap()
                .socket
                .clone(),
        ),
    };
    let keys = parse_keys(&format!("{PUBKEY7} pubkey")).unwrap();
    (
        SystemSlots::new(layout.clone(), Box::new(tryboot), keys),
        layout,
    )
}

#[test]
fn the_written_partition_verifies_and_the_driver_reads_the_slot_as_installed_confirmed_and_default()
{
    let slot = slot_in("flow");
    let out = slot.dir.join("data");
    let (r, text) = write_partition(&slot, &out, &[]);
    r.unwrap_or_else(|e| panic!("{e}\n{text}"));
    assert!(
        text.starts_with(&format!("wrote {}: ", out.display())),
        "{text}"
    );
    assert!(
        text.contains("slot a1, board reference, root hash "),
        "{text}"
    );
    assert!(
        text.trim_end()
            .ends_with(&format!("signed with key id {KEY_ID7}")),
        "the summary names the key id: {text}"
    );

    // `--verify` passes the tree, with the board, the boot image and the deep hash.
    let (r, report) = run(&[
        "data-partition",
        "--verify",
        out.to_str().unwrap(),
        "--layout-of",
        &slot.board,
        "--socs",
        &socs(),
        "--boot-image",
        &slot.image,
        "--deep",
    ]);
    r.unwrap_or_else(|e| panic!("{e}\n{report}"));
    assert_eq!(report.lines().last(), Some("ok"), "{report}");
    assert!(!report.contains("BAD"), "{report}");

    // The real driver over a copy of the tree and the boot image.
    let (mut slots, layout) = driver(&slot.dir, &out, &slot.image);
    let state = State::load(&layout);
    assert_eq!(state.default, "a1");
    assert_eq!(state.state, "confirmed");
    assert!(state.trial.is_empty() && state.staged.is_empty());
    let meta = Meta::load(&layout, "a1");
    assert_eq!(meta.get("GOOD"), "yes");
    assert!(!meta.failed());
    let mut buf = vec![0u8; 65536];
    let n = slots.list(&mut buf).unwrap();
    let listed = String::from_utf8_lossy(&buf[..n]).to_string();
    assert!(listed.contains("a1"), "the driver lists the slot: {listed}");
    let mut info = vec![0u8; 65536];
    let n = slots.info(&mut info).unwrap();
    let info = String::from_utf8_lossy(&info[..n]).to_string();
    assert!(
        info.contains("\"default_signature\":\"verified\""),
        "the driver verifies the default: {info}"
    );
}

/// `data-partition` with the usual inputs and an explicit `--data-size`, writing `out`.
fn write_with_size(slot: &Slot, out: &Path, size: &str) -> (Result<(), String>, String) {
    let out = out.display().to_string();
    let socs = socs();
    run(&[
        "data-partition",
        "--layout-of",
        &slot.board,
        "--socs",
        &socs,
        "--install",
        &slot.bundle,
        "--boot-image",
        &slot.image,
        "--keys",
        &slot.keys,
        "--time",
        TIME,
        "--data-size",
        size,
        "--out",
        &out,
    ])
}

#[test]
fn a_data_size_of_the_slot_and_its_records_is_written_and_one_byte_less_is_refused_with_both_sizes()
{
    let slot = slot_in("small-room");
    let out = slot.dir.join("data");
    let (r, text) = write_partition(&slot, &out, &[]);
    r.unwrap_or_else(|e| panic!("{e}\n{text}"));
    let need = lemnos_driver_system_slots::offline::tree_bytes(&out);
    std::fs::remove_dir_all(&out).unwrap();
    // Far below the 2048 MiB live reserve: the writer keeps none back.
    assert!(need < (2048_u64 << 20), "the tree is {need} bytes");

    let (r, text) = write_with_size(&slot, &out, &need.to_string());
    r.unwrap_or_else(|e| panic!("{e}\n{text}"));
    assert!(out.is_dir());
    std::fs::remove_dir_all(&out).unwrap();

    let (r, text) = write_with_size(&slot, &out, &(need - 1).to_string());
    let why = r.unwrap_err();
    assert_eq!(
        why,
        format!(
            "no room for this slot: the slot and its records need {need} bytes, --data-size gives {} bytes: give the data partition more room",
            need - 1
        ),
        "{text}"
    );
    assert!(!out.exists(), "a refused write leaves no directory");
}

#[test]
fn verify_shows_the_reserve_the_board_applies_to_live_installs_from_its_file() {
    let slot = slot_in("reserve-shown");
    let out = slot.dir.join("data");
    let (r, text) = write_partition(&slot, &out, &[]);
    r.unwrap_or_else(|e| panic!("{e}\n{text}"));

    // The reference board says reserve_mb = 2048; a copy saying 64 shows 64, so the file is read.
    let board64 = slot.dir.join("board-64.toml");
    let text = std::fs::read_to_string(&slot.board)
        .unwrap()
        .replace("reserve_mb = 2048", "reserve_mb = 64");
    std::fs::write(&board64, text).unwrap();
    let (r, report) = run(&[
        "data-partition",
        "--verify",
        out.to_str().unwrap(),
        "--layout-of",
        board64.to_str().unwrap(),
        "--socs",
        &socs(),
    ]);
    r.unwrap_or_else(|e| panic!("{e}\n{report}"));
    assert!(
        report.contains("ok  the board keeps 64 MiB free for live installs (its reserve_mb); the offline writer applies none"),
        "{report}"
    );
    assert_eq!(report.lines().last(), Some("ok"), "{report}");
}

#[test]
fn a_bundle_signed_by_a_key_the_list_does_not_hold_is_refused_and_nothing_is_left() {
    let slot = slot_in("unknown-key");
    let other = bundle_with(&slot, "other.bundle", Some(SEED8));
    let out = slot.dir.join("data");
    let other_slot = Slot {
        bundle: other,
        ..slot
    };
    let parent = out.parent().unwrap().to_path_buf();
    let before: Vec<_> = std::fs::read_dir(&parent)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    let (r, ever) = exists_during(&out, || write_partition(&other_slot, &out, &[]).0);
    assert!(!ever, "the output directory is never created on a refusal");
    let after: Vec<_> = std::fs::read_dir(&parent)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(
        before.len(),
        after.len(),
        "the directory's listing is unchanged"
    );
    let why = r.unwrap_err();
    assert!(
        why.contains("the bundle is signed by key id ")
            && why.contains(&format!("which {} does not hold", other_slot.keys)),
        "{why}"
    );
    assert!(!out.exists(), "a refused write leaves no directory");
}

#[test]
fn an_unsigned_bundle_is_refused_by_a_board_that_requires_signing() {
    let slot = slot_in("unsigned-required");
    let unsigned = bundle_with(&slot, "unsigned.bundle", None);
    let u = Slot {
        bundle: unsigned,
        ..slot
    };
    let out = u.dir.join("data");
    let (r, ever) = exists_during(&out, || write_partition(&u, &out, &[]).0);
    assert!(!ever, "the output directory is never created on a refusal");
    let why = r.unwrap_err();
    assert!(
        why.contains("the bundle is unsigned, and this board requires signed bundles: sign it, or set signature = \"not-required\" on the board's system-slots device"),
        "{why}"
    );
    assert!(!out.exists(), "a refused write leaves no directory");
}

#[test]
fn an_unsigned_bundle_is_written_on_a_board_that_does_not_require_signing_and_verify_says_so() {
    let slot = slot_in("unsigned-allowed");
    let unsigned = bundle_with(&slot, "unsigned.bundle", None);
    let required_board = slot.board.clone();
    let board = board_signed(&slot.dir, "not-required");
    let u = Slot {
        bundle: unsigned,
        board: board.clone(),
        ..slot
    };
    let out = u.dir.join("data");
    let (r, text) = write_partition(&u, &out, &[]);
    r.unwrap_or_else(|e| panic!("{e}\n{text}"));
    assert!(text.trim_end().ends_with("unsigned"), "{text}");
    let socs = socs();
    let (r, report) = run(&[
        "data-partition",
        "--verify",
        out.to_str().unwrap(),
        "--layout-of",
        &board,
        "--socs",
        &socs,
        "--boot-image",
        &u.image,
    ]);
    r.unwrap_or_else(|e| panic!("{e}\n{report}"));
    assert!(
        report.contains("ok  slot a1 is unsigned: this board does not require signing"),
        "{report}"
    );
    // The same tree, checked against a board that requires signing, is refused by name.
    let (r, report) = run(&[
        "data-partition",
        "--verify",
        out.to_str().unwrap(),
        "--layout-of",
        &required_board,
        "--socs",
        &socs,
    ]);
    assert!(r.is_err());
    assert!(
        report.contains("BAD slot a1 is unsigned: this board requires signed bundles"),
        "{report}"
    );
}

#[test]
fn a_signing_policy_other_than_required_or_not_required_is_refused_by_name() {
    let slot = slot_in("bad-policy");
    let board = board_signed(&slot.dir, "sometimes");
    let bad = Slot { board, ..slot };
    let out = bad.dir.join("data");
    let (r, _) = write_partition(&bad, &out, &[]);
    let why = r.unwrap_err();
    assert!(
        why.contains("signature is \"required\" or \"not-required\""),
        "{why}"
    );
    assert!(!out.exists());
}

#[test]
fn a_bundle_whose_signature_does_not_verify_under_its_key_is_refused_and_writes_nothing() {
    let slot = slot_in("bad-signature");
    let mut sig = slot_sig(&slot.bundle);
    let last = sig.len() - 1;
    sig[last] ^= 0x01;
    let bad = with_signature(&slot.dir, "bad.bundle", &slot.bundle, &sig);
    let bad_slot = Slot {
        bundle: bad,
        ..slot
    };
    let out = bad_slot.dir.join("data");
    let (r, _) = write_partition(&bad_slot, &out, &[]);
    let why = r.unwrap_err();
    assert!(why.contains("does not verify with the key of"), "{why}");
    assert!(!out.exists(), "a refused write leaves no directory");
}

#[test]
fn a_boot_image_without_the_bundles_slot_is_refused_by_name() {
    let slot = slot_in("bay0");
    let other = boot_image(&slot.dir, &slot.board, "a2", &slot.payloads);
    let mismatched = Slot {
        image: other,
        ..slot
    };
    let out = mismatched.dir.join("data");
    let (r, _) = write_partition(&mismatched, &out, &[]);
    let why = r.unwrap_err();
    // The bay check is the slot id's for now (`check_bay`); M35 adds the bay's payload comparison.
    assert!(
        why.contains("the boot image holds no bay for slot a1: build the image from this bundle"),
        "{why}"
    );
    assert!(!out.exists(), "a refused write leaves no directory");
}

#[test]
fn a_bundle_for_another_board_is_refused_by_the_board_it_names() {
    let slot = slot_in("wrong-board");
    let other_board = board(&slot.dir, Some("other-board"));
    let wrong = Slot {
        board: other_board,
        ..slot
    };
    let out = wrong.dir.join("data");
    let (r, _) = write_partition(&wrong, &out, &[]);
    let why = r.unwrap_err();
    assert!(
        why.contains("this slot is built for board reference, and this is board other-board"),
        "{why}"
    );
    assert!(!out.exists(), "a refused write leaves no directory");
}

#[test]
fn an_unreadable_key_file_is_refused_with_the_flag_to_pass() {
    let slot = slot_in("no-keys");
    let out = slot.dir.join("data");
    let missing = slot.dir.join("missing.pub").display().to_string();
    let keyless = Slot {
        keys: missing,
        ..slot
    };
    let (r, _) = write_partition(&keyless, &out, &[]);
    let why = r.unwrap_err();
    assert!(why.contains("pass --keys FILE"), "{why}");
    assert!(!out.exists());
}

#[test]
fn the_switch_is_not_set_by_this_command_and_its_refusal_names_the_later_step() {
    let slot = slot_in("switch");
    let out = slot.dir.join("data");
    let (r, _) = write_partition(&slot, &out, &["--switch", "on"]);
    let why = r.unwrap_err();
    assert!(
        why.contains("set it after the first boot with lemnos-ctl security set on"),
        "{why}"
    );
    assert!(!out.exists());
}

/// The binary's own run: the same inputs under a given umask, into `out`.
fn binary_under_umask(slot: &Slot, umask: &str, out: &Path) {
    let bin = env!("CARGO_BIN_EXE_lemnos-ctl");
    let script = format!("umask {umask}; exec \"$0\" \"$@\"");
    let status = Command::new("sh")
        .arg("-c")
        .arg(&script)
        .arg(bin)
        .args([
            "data-partition",
            "--layout-of",
            &slot.board,
            "--socs",
            &socs(),
            "--install",
            &slot.bundle,
            "--boot-image",
            &slot.image,
            "--keys",
            &slot.keys,
            "--time",
            TIME,
            "--data-size",
            DATA_SIZE,
            "--out",
            out.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "umask {umask}: {}",
        String::from_utf8_lossy(&status.stderr)
    );
}

#[test]
fn the_modes_and_bytes_are_the_same_whatever_the_umask_is() {
    let slot = slot_in("umask");
    let library = slot.dir.join("library");
    let (r, text) = write_partition(&slot, &library, &[]);
    r.unwrap_or_else(|e| panic!("{e}\n{text}"));
    let reference = tree(&library);

    for umask in ["077", "022"] {
        let out = slot.dir.join(format!("umask-{umask}"));
        binary_under_umask(&slot, umask, &out);
        let got = tree(&out);
        assert_eq!(
            got.keys().collect::<Vec<_>>(),
            reference.keys().collect::<Vec<_>>(),
            "umask {umask}: the same paths"
        );
        for (path, value) in &got {
            assert_eq!(
                value, &reference[path],
                "umask {umask}: {path} has the same bytes and mode"
            );
        }
    }
    // The fixed modes: 0644 for files, 0755 for directories, whatever the umask.
    assert_eq!(
        std::fs::metadata(&library).unwrap().permissions().mode() & 0o7777,
        0o755
    );
    for (path, (bytes, mode)) in &reference {
        let want = if bytes.is_some() { 0o644 } else { 0o755 };
        assert_eq!(*mode, want, "{path}");
    }
}

#[test]
fn verify_refuses_a_tree_whose_slot_is_not_good_and_names_the_line() {
    let slot = slot_in("verify-bad");
    let out = slot.dir.join("data");
    let (r, text) = write_partition(&slot, &out, &[]);
    r.unwrap_or_else(|e| panic!("{e}\n{text}"));
    let copy = slot.dir.join("copy");
    copy_tree(&out, &copy);
    let meta = copy.join("board/slots/a1.meta");
    let text = std::fs::read_to_string(&meta).unwrap();
    assert!(text.contains("GOOD=yes"), "{text}");
    std::fs::write(&meta, text.replace("GOOD=yes", "GOOD=no")).unwrap();
    let (r, report) = run(&["data-partition", "--verify", copy.to_str().unwrap()]);
    let why = r.unwrap_err();
    assert!(why.contains("check(s) failed"), "{why}");
    assert!(
        report.contains("BAD slot a1 is not good, or has failed"),
        "{report}"
    );
    assert!(report.contains("refused: 1"), "{report}");
}

/// `security render` and `security set` on a boot partition image: the switch is read from the
/// image's `SECURE.REC`, and a fresh image reads off.
fn security_run(args: &[&str]) -> (Result<(), String>, String) {
    run(args)
}

#[test]
fn security_render_reads_a_fresh_image_as_off_and_a_set_round_trips_through_it() {
    let slot = slot_in("security-image");
    let image = slot.image.clone();
    let view = slot.dir.join("view");
    std::fs::create_dir_all(&view).unwrap();
    let view_path = view.display().to_string();
    let render = |image: &str| {
        security_run(&[
            "security",
            "render",
            "--boot-image",
            image,
            "--dir",
            &view_path,
        ])
    };
    let (r, out) = render(&image);
    r.unwrap_or_else(|e| panic!("{e}\n{out}"));
    assert!(
        out.contains(": off generation"),
        "a fresh image reads off: {out}"
    );

    let (r, out) = security_run(&["security", "set", "--boot-image", &image, "on"]);
    r.unwrap_or_else(|e| panic!("{e}\n{out}"));
    assert!(out.starts_with("switch on in "), "{out}");
    let (_, out) = render(&image);
    assert!(out.contains(": on generation"), "set on reads on: {out}");

    let (r, out) = security_run(&["security", "set", "--boot-image", &image, "off"]);
    r.unwrap_or_else(|e| panic!("{e}\n{out}"));
    let (_, out) = render(&image);
    assert!(out.contains(": off generation"), "set off reads off: {out}");
}

#[test]
fn security_takes_exactly_one_of_record_and_boot_image_and_a_missing_record_reads_on() {
    let slot = slot_in("security-one");
    let view = slot.dir.join("view");
    std::fs::create_dir_all(&view).unwrap();
    let view_path = view.display().to_string();
    let (r, _) = security_run(&[
        "security",
        "render",
        "--record",
        "x",
        "--boot-image",
        &slot.image,
        "--dir",
        &view_path,
    ]);
    assert!(r.is_err(), "--record and --boot-image together are refused");

    // An image with no SECURE.REC: the record can't be read, and the switch reads on, said so.
    let bare = slot.dir.join("bare.img");
    std::fs::write(&bare, vec![0u8; 1024 * 1024]).unwrap();
    let (r, out) = security_run(&[
        "security",
        "render",
        "--boot-image",
        bare.to_str().unwrap(),
        "--dir",
        &view_path,
    ]);
    r.unwrap_or_else(|e| panic!("{e}\n{out}"));
    assert!(
        out.contains("unreadable") && out.contains("the switch reads on"),
        "{out}"
    );
    let rendered = std::fs::read_to_string(view.join(lemnos_slot_store::security::FILE_NAME))
        .unwrap_or_default();
    assert!(rendered.contains("on"), "the view renders on: {rendered}");
}

/// The first sector of the switch record's two copies in the boot image `image`.
fn secure_copies(image: &str) -> u64 {
    let mut file = std::fs::File::open(image).unwrap();
    let name = lemnos_slot_store::twocopy::Kind::SECURITY
        .file_name()
        .unwrap();
    lemnos_slot_store::bootfat::locate(&mut file, name, lemnos_slot_store::boot_raw::RECORD_BYTES)
        .unwrap()
        .first_lba
}

/// Flips byte `at` of sector `lba` in `image`: a damage that breaks that copy's CRC.
fn damage(image: &str, lba: u64, at: usize) {
    use std::os::unix::fs::FileExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(image)
        .unwrap();
    let offset = lba * 512 + at as u64;
    let mut one = [0u8; 1];
    file.read_exact_at(&mut one, offset).unwrap();
    file.write_all_at(&[one[0] ^ 0xFF], offset).unwrap();
}

/// `data-partition --verify` of the tree `out`, against the boot image `image`.
fn verify_against(slot: &Slot, out: &Path, image: &str) -> (Result<(), String>, String) {
    let socs = socs();
    run(&[
        "data-partition",
        "--verify",
        out.to_str().unwrap(),
        "--layout-of",
        &slot.board,
        "--socs",
        &socs,
        "--boot-image",
        image,
    ])
}

#[test]
fn verify_refuses_a_boot_image_without_secure_rec_and_names_the_builder_fix() {
    let slot = slot_in("secure-missing");
    let out = slot.dir.join("data");
    let (r, text) = write_partition(&slot, &out, &[]);
    r.unwrap_or_else(|e| panic!("{e}\n{text}"));

    // The same image with its SECURE.REC directory entry renamed: the volume is valid, the record is gone.
    let bare = slot.dir.join("no-secure.img").display().to_string();
    std::fs::copy(&slot.image, &bare).unwrap();
    let name = lemnos_slot_store::twocopy::Kind::SECURITY
        .file_name()
        .unwrap();
    let mut bytes = std::fs::read(&bare).unwrap();
    let hits: Vec<usize> = bytes
        .windows(name.len())
        .enumerate()
        .filter(|(_, w)| *w == name)
        .map(|(at, _)| at)
        .collect();
    assert_eq!(hits.len(), 1, "one directory entry names SECURE.REC");
    bytes[hits[0]] = b'X';
    std::fs::write(&bare, &bytes).unwrap();

    let (r, report) = verify_against(&slot, &out, &bare);
    let why = r.unwrap_err();
    assert!(why.contains("check(s) failed"), "{why}");
    assert!(
        report.contains(
            "BAD the boot image has no SECURE.REC: the image builder must pre-create it, off, as a new image has it \
             (`security set --boot-image` only rewrites an existing SECURE.REC and cannot create one); \
             without it the switch reads as on"
        ),
        "{report}"
    );
    assert!(report.contains("refused: 1"), "{report}");

    // `security set` does not create the record: it is refused, as the sentence above says.
    let (r, _) = run(&["security", "set", "--boot-image", &bare, "off"]);
    assert!(
        r.is_err(),
        "security set cannot create a missing SECURE.REC"
    );
}

#[test]
fn verify_accepts_a_good_off_and_a_good_on_record_with_both_copies_good() {
    let slot = slot_in("secure-good");
    let out = slot.dir.join("data");
    let (r, text) = write_partition(&slot, &out, &[]);
    r.unwrap_or_else(|e| panic!("{e}\n{text}"));

    let (r, report) = verify_against(&slot, &out, &slot.image);
    r.unwrap_or_else(|e| panic!("{e}\n{report}"));
    assert!(
        report.contains(
            "the boot image's SECURE.REC has both copies good and reads off at generation 1"
        ),
        "{report}"
    );
    assert_eq!(report.lines().last(), Some("ok"), "{report}");

    let (r, out_set) = run(&["security", "set", "--boot-image", &slot.image, "on"]);
    r.unwrap_or_else(|e| panic!("{e}\n{out_set}"));
    let (r, report) = verify_against(&slot, &out, &slot.image);
    r.unwrap_or_else(|e| panic!("{e}\n{report}"));
    assert!(
        report.contains(
            "the boot image's SECURE.REC has both copies good and reads on at generation 2"
        ),
        "{report}"
    );
    assert_eq!(report.lines().last(), Some("ok"), "{report}");
}

/// A fresh image with its record damaged at byte 9 of the copies named by `which` (`0` first, `1` second,
/// `2` both), verified: the refusal text, and the report.
fn verify_with_copies_damaged(name: &str, which: &[u64]) -> (Result<(), String>, String) {
    let slot = slot_in(name);
    let out = slot.dir.join("data");
    let (r, text) = write_partition(&slot, &out, &[]);
    r.unwrap_or_else(|e| panic!("{e}\n{text}"));
    let copies = secure_copies(&slot.image);
    for copy in which {
        damage(&slot.image, copies + copy, 9);
    }
    let (r, report) = verify_against(&slot, &out, &slot.image);
    (r, report)
}

const FIRST_DAMAGED: &str = "BAD the boot image's SECURE.REC first copy is damaged: the CRC does not match the copy; the image check needs both copies good: rebuild the boot image";
const SECOND_DAMAGED: &str = "BAD the boot image's SECURE.REC second copy is damaged: the CRC does not match the copy; the image check needs both copies good: rebuild the boot image";

#[test]
fn verify_refuses_a_first_copy_damaged_and_names_it() {
    let (r, report) = verify_with_copies_damaged("secure-first", &[0]);
    let why = r.unwrap_err();
    assert!(why.contains("check(s) failed"), "{why}");
    assert!(report.contains(FIRST_DAMAGED), "{report}");
    assert!(!report.contains("second copy is damaged"), "{report}");
    assert!(report.contains("refused: 1"), "{report}");
}

#[test]
fn verify_refuses_a_second_copy_damaged_and_names_it() {
    let (r, report) = verify_with_copies_damaged("secure-second", &[1]);
    let why = r.unwrap_err();
    assert!(why.contains("check(s) failed"), "{why}");
    assert!(report.contains(SECOND_DAMAGED), "{report}");
    assert!(!report.contains("first copy is damaged"), "{report}");
    assert!(report.contains("refused: 1"), "{report}");
}

#[test]
fn verify_refuses_both_copies_damaged_and_names_each() {
    let (r, report) = verify_with_copies_damaged("secure-both", &[0, 1]);
    let why = r.unwrap_err();
    assert!(why.contains("check(s) failed"), "{why}");
    assert!(report.contains(FIRST_DAMAGED), "{report}");
    assert!(report.contains(SECOND_DAMAGED), "{report}");
    assert!(report.contains("refused: 2"), "{report}");
}
