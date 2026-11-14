//! `lemnos-ctl data-partition` end to end, against the repository's reference board: a signed bundle
//! and the boot image made from its payloads go in, the tree comes out, `--verify` passes it, and the
//! real slot driver over a copy of it reads the slot as installed, confirmed and default. Each way of
//! breaking the inputs is refused by name, and leaves no output directory behind. The umask cases run
//! the binary itself under `sh -c 'umask N; ...'`, so the test process's own umask is never changed.

use lemnos_contract_system_slots::Slots as _;
use lemnos_driver_system_slots::boot::{Restarter, Tryboot};
use lemnos_driver_system_slots::{SystemSlots, parse_keys};
use lemnos_slot_store::store::{Layout, Meta, State};
use lemnos_slot_store::tar::{Kind, Sink, Tar, write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn socs() -> String {
    repo().join("socs").display().to_string()
}

fn run(args: &[&str]) -> (Result<(), String>, String) {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
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
    std::fs::write(p.join("kernel"), vec![0u8; 3000]).unwrap();
    std::fs::write(p.join("initramfs"), b"initramfs").unwrap();
    std::fs::write(p.join("device-tree"), b"device tree").unwrap();
    std::fs::write(p.join("board-overlay"), b"overlay").unwrap();
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
    let members: Vec<(String, Vec<u8>)> = m
        .0
        .into_iter()
        .map(|(n, b)| if n == "slot.sig" { (n, sig.to_vec()) } else { (n, b) })
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
        restarter: Restarter {
            systemctl: PathBuf::from("/bin/true"),
            check: None,
        },
    };
    let keys = parse_keys(&format!("{PUBKEY7} pubkey")).unwrap();
    (SystemSlots::new(layout.clone(), Box::new(tryboot), keys), layout)
}

#[test]
fn the_written_partition_verifies_and_the_driver_reads_the_slot_as_installed_confirmed_and_default() {
    let slot = slot_in("flow");
    let out = slot.dir.join("data");
    let (r, text) = write_partition(&slot, &out, &[]);
    r.unwrap_or_else(|e| panic!("{e}\n{text}"));
    assert!(
        text.starts_with(&format!("wrote {}: ", out.display())),
        "{text}"
    );
    assert!(text.contains("slot a1, board reference, root hash "), "{text}");
    assert!(
        text.trim_end().ends_with(&format!("signed with key id {KEY_ID7}")),
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
    assert!(info.contains("\"default_signature\":\"verified\""), "the driver verifies the default: {info}");
}

#[test]
fn a_bundle_signed_by_a_key_the_list_does_not_hold_is_refused_and_nothing_is_left() {
    let slot = slot_in("unknown-key");
    let other = bundle_with(&slot, "other.bundle", Some(SEED8));
    let out = slot.dir.join("data");
    let other_slot = Slot { bundle: other, ..slot };
    let (r, _) = write_partition(&other_slot, &out, &[]);
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
    let u = Slot { bundle: unsigned, ..slot };
    let out = u.dir.join("data");
    let (r, _) = write_partition(&u, &out, &[]);
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
    assert!(why.contains("signature is \"required\" or \"not-required\""), "{why}");
    assert!(!out.exists());
}

#[test]
fn a_bundle_whose_signature_does_not_verify_under_its_key_is_refused_and_writes_nothing() {
    let slot = slot_in("bad-signature");
    let mut sig = slot_sig(&slot.bundle);
    let last = sig.len() - 1;
    sig[last] ^= 0x01;
    let bad = with_signature(&slot.dir, "bad.bundle", &slot.bundle, &sig);
    let bad_slot = Slot { bundle: bad, ..slot };
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
    let mismatched = Slot { image: other, ..slot };
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
    assert!(why.contains("set it after the first boot with lemnos-ctl security set on"), "{why}");
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
            assert_eq!(value, &reference[path], "umask {umask}: {path} has the same bytes and mode");
        }
    }
    // The fixed modes: 0644 for files, 0755 for directories, whatever the umask.
    assert_eq!(std::fs::metadata(&library).unwrap().permissions().mode() & 0o7777, 0o755);
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
    assert!(report.contains("BAD slot a1 is not good, or has failed"), "{report}");
    assert!(report.contains("refused: 1"), "{report}");
}
