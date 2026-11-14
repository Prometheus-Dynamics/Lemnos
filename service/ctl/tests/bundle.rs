//! `lemnos-ctl bundle` against the repository's reference board: a bundle written from the same payload
//! directory `boot-partition` takes verifies clean, is the same bytes for the same inputs, carries the
//! signature `docs/slots.md` documents, and each way of breaking it is refused by a pinned sentence. The
//! install of such a bundle on the simulated board through the real driver is
//! `a_bundle_made_by_the_writer_installs_on_both_record_forms` (system-slots), which calls the same
//! writer function this tool calls.

use ed25519_dalek::{Signer, SigningKey};
use lemnos_slot_store::bundle::hex;
use lemnos_slot_store::signing;
use lemnos_slot_store::tar::{Kind, Sink, Tar, write};
use lemnos_slot_store::verity;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

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
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("bundle-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The documented test key: the seed `0x07` repeated.
const SEED7: [u8; 32] = [7; 32];
const KEY_ID7: &str = "fe812c12f3ab4ce6ac5db69ac352f906cb1b11ef43fb33e252ef7ff552263889";
const PUBKEY7: &str = "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c";

fn key_file(dir: &Path, name: &str, bytes: &[u8], mode: u32) -> String {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    path.display().to_string()
}

/// A board file: the reference board, or its U-Boot form.
fn board(dir: &Path, uboot: bool) -> String {
    let text = std::fs::read_to_string(repo().join("testing/boards/reference.toml")).unwrap();
    let path = dir.join(if uboot { "uboot.toml" } else { "tryboot.toml" });
    let text = if uboot {
        text.replace(
            "record = \"tryboot\"",
            "record = \"uboot\", uboot_env_size = 8192",
        )
    } else {
        text
    };
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

/// The erofs image with its tree appended, and the builder's table text.
fn rootfs(dir: &Path, blocks: usize) -> (String, String) {
    let data = erofs(blocks);
    let salt: [u8; 32] = std::array::from_fn(|i| i as u8);
    let tree = verity::tree_of(salt, &data).unwrap();
    let mut whole = data.clone();
    whole.extend_from_slice(&tree.tree);
    let image = dir.join("rootfs.erofs");
    std::fs::write(&image, &whole).unwrap();
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

/// The payload directory: the command line is what `tree --cmdline-slot` writes, hole and all.
fn payload(dir: &Path, board: &str, slot: &str, gzip: bool) -> String {
    let p = dir.join("payload");
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    let mut kernel = vec![0u8; 3000];
    if gzip {
        kernel[..2].copy_from_slice(&[0x1f, 0x8b]);
    }
    std::fs::write(p.join("kernel"), kernel).unwrap();
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

struct Made {
    dir: PathBuf,
    board: String,
    payload: String,
    rootfs: String,
    table: String,
}

fn made(name: &str, uboot: bool, gzip: bool) -> Made {
    let dir = scratch(name);
    let board = board(&dir, uboot);
    let payload = payload(&dir, &board, "a1", gzip);
    let (rootfs, table) = rootfs(&dir, 300);
    Made {
        dir,
        board,
        payload,
        rootfs,
        table,
    }
}

impl Made {
    fn args<'a>(&'a self, out: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
        let mut a = vec![
            "bundle",
            "--layout-of",
            &self.board,
            "--socs",
            Box::leak(socs().into_boxed_str()),
            "--slot",
            "a1",
            "--payload",
            &self.payload,
            "--rootfs",
            &self.rootfs,
            "--verity-table",
            &self.table,
            "--product",
            "an-os",
            "--version",
            "1.0",
            "--out",
            out,
        ];
        a.extend_from_slice(extra);
        a
    }

    fn make(&self, name: &str, extra: &[&str]) -> (Result<(), String>, String, String) {
        let out = self.dir.join(name).display().to_string();
        let (r, text) = run(&self.args(&out, extra));
        (r, text, out)
    }

    fn verify(&self, file: &str, extra: &[&str]) -> (Result<(), String>, String) {
        let mut a = vec![
            "bundle",
            "--layout-of",
            &self.board,
            "--socs",
            Box::leak(socs().into_boxed_str()),
            "--verify",
            file,
        ];
        a.extend_from_slice(extra);
        run(&a)
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

fn members_of(file: &str) -> Vec<(String, Vec<u8>)> {
    let mut m = Members::default();
    Tar::default()
        .feed(&std::fs::read(file).unwrap(), &mut m)
        .unwrap();
    m.0
}

fn repack(dir: &Path, name: &str, members: &[(String, Vec<u8>)]) -> String {
    let borrowed: Vec<(&str, &[u8])> = members
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_slice()))
        .collect();
    let path = dir.join(name);
    std::fs::write(&path, write::archive(&borrowed)).unwrap();
    path.display().to_string()
}

fn bad_lines(report: &str) -> Vec<&str> {
    report.lines().filter(|l| l.starts_with("BAD")).collect()
}

/// The reference flow: `tree` writes the command line with its hole, `bundle` writes and signs, the
/// verify accepts it with the public key, the boot files it wrote feed `boot-partition`.
#[test]
fn a_bundle_is_written_signed_verified_and_its_boot_files_feed_the_boot_partition() {
    let m = made("flow", false, true);
    let seed = key_file(&m.dir, "signing.seed", &SEED7, 0o600);
    let pubkey = key_file(&m.dir, "signing.pub", PUBKEY7.as_bytes(), 0o644);
    let payload_out = m.dir.join("final").display().to_string();
    let (r, out, file) = m.make(
        "a1.bundle",
        &[
            "--key-file",
            &seed,
            "--expect-key-id",
            KEY_ID7,
            "--payload-out",
            &payload_out,
        ],
    );
    r.unwrap_or_else(|e| panic!("{e}\n{out}"));
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 5, "{out}");
    assert!(
        lines[0].starts_with("slot a1: product an-os, version 1.0, board "),
        "{out}"
    );
    assert!(
        lines[1].starts_with("root hash ") && lines[1].len() == 10 + 64,
        "{out}"
    );
    assert_eq!(lines[2], format!("signed with key id {KEY_ID7}"));
    assert_eq!(lines[3], format!("payload written to {payload_out}"));
    assert!(
        lines[4].starts_with(&format!("wrote {file}: ")) && lines[4].contains(", SHA-256 "),
        "{out}"
    );

    let (r, report) = m.verify(&file, &["--pubkey", &pubkey]);
    r.unwrap_or_else(|e| panic!("{e}\n{report}"));
    let kinds: Vec<&str> = report
        .lines()
        .map(|l| l.split(':').next().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "ok  members",
            "ok  manifest",
            "ok  signature",
            "ok  image",
            "ok  verity",
            "ok  boot files",
            "ok  roles",
            "ok  board",
            "ok  kernel",
            "ok  bays",
            "ok  cmdline",
            "ok  overlay",
            "ok"
        ],
        "{report}"
    );
    assert!(
        report.contains(&format!(
            "signature: signed by key id {KEY_ID7}: signature valid"
        )),
        "{report}"
    );

    // The root hash it wrote is in the command line the boot partition's first slot carries.
    let root = lines[1].strip_prefix("root hash ").unwrap();
    let cmdline = std::fs::read_to_string(Path::new(&payload_out).join("cmdline")).unwrap();
    assert!(
        cmdline.contains(&format!("os.slot=a1 os.roothash={root} ")),
        "{cmdline}"
    );
    let (r, min) = run(&[
        "boot-partition",
        "--layout-of",
        &m.board,
        "--socs",
        &socs(),
        "--min-size",
    ]);
    r.unwrap();
    let size = min.trim().parse::<u64>().unwrap().div_ceil(1 << 20) << 20;
    let image = m.dir.join("boot.img").display().to_string();
    let (r, _) = run(&[
        "boot-partition",
        "--layout-of",
        &m.board,
        "--socs",
        &socs(),
        "--slot",
        "a1",
        "--payload",
        &payload_out,
        "--size",
        &size.to_string(),
        "--out",
        &image,
    ]);
    r.unwrap();
}

/// The documented signature: the CLI's `slot.sig` is the record for its own `slot.json` under the seed
/// `0x07` repeated (public key and key id as `docs/slots.md` gives them), made over the documented
/// domain-tagged message.
#[test]
fn the_signature_is_the_documented_record_over_the_documented_message() {
    let m = made("vector", false, false);
    let seed = key_file(&m.dir, "signing.seed", &SEED7, 0o600);
    let (r, _, file) = m.make("a1.bundle", &["--key-file", &seed]);
    r.unwrap();
    let members = members_of(&file);
    assert_eq!(members[0].0, "slot.json");
    assert_eq!(members[1].0, "slot.sig");
    let key = SigningKey::from_bytes(&SEED7);
    assert_eq!(hex(key.verifying_key().as_bytes()), PUBKEY7);
    assert_eq!(
        hex(&signing::key_id(key.verifying_key().as_bytes())),
        KEY_ID7
    );
    let message = signing::slot_bundle_message(&members[0].1);
    assert!(message.starts_with(b"lemnos-slot-bundle\0\x01\0\0\0"));
    let want = signing::record_bytes(
        &signing::key_id(key.verifying_key().as_bytes()),
        &key.sign(&message).to_bytes(),
    );
    assert_eq!(members[1].1, want);
    // And the documented vector itself, through the same record encoder.
    let vector = signing::slot_bundle_message(b"{\"id\":\"vector\"}");
    assert_eq!(
        hex(&vector),
        "6c656d6e6f732d736c6f742d62756e646c6500010000000f0000007b226964223a22766563746f72227d"
    );
    let record = signing::record_bytes(
        &signing::key_id(key.verifying_key().as_bytes()),
        &key.sign(&vector).to_bytes(),
    );
    assert_eq!(
        hex(&record),
        "4c4d4e535349473101000000fe812c12f3ab4ce6ac5db69ac352f906cb1b11ef43fb33e252ef7ff55226388956be579db020703eb207da579036a89f0321fcd8ce2bac78a9aa939a58217e265ad27a7e1fc05829b0db05d37ac4d8dba35263c7a1b2644322b45ada4a101402"
    );
}

#[test]
fn the_same_inputs_give_the_same_bytes_and_unsigned_says_so() {
    let m = made("determinism", false, true);
    let seed = key_file(&m.dir, "signing.seed", &SEED7, 0o600);
    let (_, one, f1) = m.make("one.bundle", &["--key-file", &seed]);
    let (_, two, f2) = m.make("two.bundle", &["--key-file", &seed]);
    assert_eq!(std::fs::read(&f1).unwrap(), std::fs::read(&f2).unwrap());
    let sha = |t: &str| {
        t.lines()
            .last()
            .unwrap()
            .rsplit("SHA-256 ")
            .next()
            .unwrap()
            .to_string()
    };
    assert_eq!(sha(&one), sha(&two));
    let (r, out, file) = m.make("none.bundle", &[]);
    r.unwrap();
    assert!(out.lines().any(|l| l == "unsigned"), "{out}");
    let (r, report) = m.verify(&file, &[]);
    r.unwrap();
    assert!(report.contains("ok  signature: unsigned"), "{report}");
}

#[test]
fn a_signed_bundle_without_the_public_half_is_not_claimed_valid() {
    let m = made("nopub", false, false);
    let seed = key_file(&m.dir, "signing.seed", &SEED7, 0o600);
    let (_, _, file) = m.make("a1.bundle", &["--key-file", &seed]);
    let (r, report) = m.verify(&file, &[]);
    r.unwrap();
    assert!(
        report.contains(&format!("n/a signature: signed by key id {KEY_ID7}: not checked, the board's key file /etc/lemnos/slot-keys can't be read here")),
        "{report}"
    );
    // A board that names no key file: no list at all.
    let bare = m.dir.join("bare.toml");
    let text = std::fs::read_to_string(&m.board).unwrap();
    std::fs::write(
        &bare,
        text.lines()
            .filter(|l| !l.trim_start().starts_with("keys ="))
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let (r, report) = run(&[
        "bundle",
        "--layout-of",
        bare.to_str().unwrap(),
        "--socs",
        &socs(),
        "--verify",
        &file,
    ]);
    r.unwrap();
    assert!(
        report.contains(&format!(
            "n/a signature: signed by key id {KEY_ID7}: not checked, no key list given"
        )),
        "{report}"
    );
    // A different key's list: the key is not in it.
    let other = key_file(
        &m.dir,
        "other.pub",
        format!(
            "{} someone\n",
            hex(SigningKey::from_bytes(&[9; 32]).verifying_key().as_bytes())
        )
        .as_bytes(),
        0o644,
    );
    let (r, report) = m.verify(&file, &["--pubkey", &other]);
    r.unwrap();
    assert!(
        report.contains("not checked, the key is not in the key list"),
        "{report}"
    );
    // The key list format the board uses: the key and its owner.
    let list = key_file(
        &m.dir,
        "keys",
        format!("{PUBKEY7} the build key\n").as_bytes(),
        0o644,
    );
    let (r, report) = m.verify(&file, &["--pubkey", &list]);
    r.unwrap();
    assert!(report.contains("signature valid"), "{report}");
}

#[test]
fn both_record_forms_take_the_kernel_form_that_is_theirs() {
    let pi = made("form-pi", false, true);
    assert!(
        pi.make("gz.bundle", &[]).0.is_ok(),
        "the Pi form stores a gzip kernel as shipped"
    );
    let ub = made("form-uboot", true, true);
    let (r, _, file) = ub.make("gz.bundle", &[]);
    let why = r.unwrap_err();
    assert!(
        why.contains(
            "the kernel is gzip-compressed: the U-Boot block form takes an uncompressed kernel"
        ),
        "{why}"
    );
    assert!(!Path::new(&file).exists(), "nothing was written");
    let raw = made("form-uboot-raw", true, false);
    let (r, _, file) = raw.make("raw.bundle", &[]);
    r.unwrap();
    let (r, report) = raw.verify(&file, &[]);
    r.unwrap();
    assert!(
        report.contains("ok  kernel: raw, as the U-Boot form takes it"),
        "{report}"
    );
    // The Pi form's bundle, checked against the U-Boot board.
    let (r, _, pi_file) = pi.make("pi.bundle", &[]);
    r.unwrap();
    let (r, report) = ub.verify(&pi_file, &[]);
    assert!(r.is_err());
    assert!(
        bad_lines(&report)
            .iter()
            .any(|l| l.starts_with("BAD kernel: the kernel is gzip-compressed")),
        "{report}"
    );
}

#[test]
fn verify_refuses_each_corruption_by_name() {
    let m = made("corrupt", false, false);
    let seed = key_file(&m.dir, "signing.seed", &SEED7, 0o600);
    let pubkey = key_file(&m.dir, "signing.pub", PUBKEY7.as_bytes(), 0o644);
    let (r, _, file) = m.make("a1.bundle", &["--key-file", &seed]);
    r.unwrap();
    let good = members_of(&file);
    let verify = |name: &str, members: &[(String, Vec<u8>)]| {
        let path = repack(&m.dir, name, members);
        let (r, report) = m.verify(&path, &["--pubkey", &pubkey]);
        assert_eq!(
            r.is_err(),
            report.lines().last().unwrap().starts_with("refused"),
            "{report}"
        );
        report
    };
    assert!(verify("good.bundle", &good).ends_with("ok\n"));
    let flip = |name: &str, at: usize| {
        let mut m2 = good.clone();
        m2.iter_mut().find(|(n, _)| n == name).unwrap().1[at] ^= 1;
        m2
    };
    // slot.sig, slot.json, the image's data and tree, each boot file.
    let report = verify("sig.bundle", &flip("slot.sig", 60));
    assert!(report.contains(&format!("BAD signature: signed by key id {KEY_ID7}: the signature does NOT verify with the key of pubkey")), "{report}");
    let report = verify("json.bundle", &flip("slot.json", 20));
    assert!(!bad_lines(&report).is_empty(), "{report}");
    let report = verify("data.bundle", &flip("rootfs.erofs", 5000));
    assert!(
        report.contains("BAD image: rootfs.erofs doesn't match slot.json's sha256")
            && report.contains("BAD verity: the image's root hash is"),
        "{report}"
    );
    let report = verify("tree.bundle", &flip("rootfs.erofs", 300 * 4096 + 9));
    assert!(
        report.contains("BAD verity:") && report.contains("not the tree of the image"),
        "{report}"
    );
    for file in [
        "kernel8.img",
        "initramfs.cpio",
        "board.dtb",
        "cmdline.txt",
        "overlays/lemnos-board.dtbo",
    ] {
        let report = verify("boot.bundle", &flip(&format!("boot/{file}"), 0));
        assert!(
            report.contains(&format!(
                "BAD boot files: boot file {file} doesn't match slot.json"
            )),
            "{file}: {report}"
        );
    }
    // Reordered members, an extra boot file, a missing one.
    let mut order = good.clone();
    order.swap(0, 1);
    let report = verify("order.bundle", &order);
    assert!(
        report.contains(
            "BAD members: the bundle starts with slot.sig, and slot.json must be its first member"
        ),
        "{report}"
    );
    let mut order = good.clone();
    order.swap(1, 2);
    assert!(
        verify("order2.bundle", &order)
            .contains("BAD members: slot.sig must be the bundle's second member")
    );
    let mut extra = good.clone();
    extra.push(("boot/overlays/second.dtbo".into(), vec![1]));
    assert!(
        verify("extra.bundle", &extra)
            .contains("BAD members: boot file overlays/second.dtbo isn't in slot.json")
    );
    let mut missing = good.clone();
    missing.retain(|(n, _)| n != "boot/board.dtb");
    assert!(
        verify("missing.bundle", &missing)
            .contains("BAD boot files: the bundle has no boot file board.dtb")
    );
    // An over-size file: the board's bay for the overlay is 4096.
    let mut big = good.clone();
    big.iter_mut()
        .find(|(n, _)| n == "boot/overlays/lemnos-board.dtbo")
        .unwrap()
        .1 = vec![1; 5000];
    let report = verify("big.bundle", &big);
    assert!(
        report.contains(
            "BAD boot files: boot file overlays/lemnos-board.dtbo doesn't match slot.json"
        ),
        "{report}"
    );
    assert!(
        report.contains("BAD bays: ")
            && report.contains("the board-overlay payload is 5000 bytes but its bay holds 4096"),
        "{report}"
    );
    // Another board file: another board id.
    let text = std::fs::read_to_string(&m.board)
        .unwrap()
        .replace("id = \"reference\"", "id = \"other\"");
    let other = m.dir.join("other.toml");
    std::fs::write(&other, text).unwrap();
    let (r, report) = run(&[
        "bundle",
        "--layout-of",
        other.to_str().unwrap(),
        "--socs",
        &socs(),
        "--verify",
        &file,
    ]);
    if std::fs::read_to_string(&m.board)
        .unwrap()
        .contains("id = \"reference\"")
    {
        assert!(r.is_err());
        assert!(
            report.contains(
                "BAD board: this slot is built for board reference, and this is board other"
            ),
            "{report}"
        );
    }
}

#[test]
fn the_writer_refuses_each_mistake_by_name_and_writes_nothing() {
    let m = made("refuse", false, false);
    let check = |label: &str, why: &str, extra: &[&str]| {
        let (r, _, file) = m.make(&format!("{label}.bundle"), extra);
        let e = r.unwrap_err();
        assert!(e.contains(why), "{label}: {e}");
        assert!(!Path::new(&file).exists(), "{label}: wrote a file");
    };
    // A missing role, an extra file.
    std::fs::remove_file(Path::new(&m.payload).join("initramfs")).unwrap();
    check("missing", "the initramfs payload is missing", &[]);
    std::fs::write(Path::new(&m.payload).join("initramfs"), b"i").unwrap();
    std::fs::write(Path::new(&m.payload).join("extra"), b"x").unwrap();
    check("extra", "\"extra\" is not a build role", &[]);
    std::fs::remove_file(Path::new(&m.payload).join("extra")).unwrap();
    // The slot id, the product, the version.
    let bad_id = |id: &str| {
        let out = m.dir.join("id.bundle").display().to_string();
        let mut a = m.args(&out, &[]);
        let at = a.iter().position(|s| *s == "a1").unwrap();
        a[at] = id;
        run(&a).0.unwrap_err()
    };
    assert!(bad_id("Bad Id").contains("--slot \"Bad Id\" isn't a slot id"));
    let out = m.dir.join("product.bundle").display().to_string();
    let mut a = m.args(&out, &[]);
    let at = a.iter().position(|s| *s == "an-os").unwrap();
    a[at] = "P_";
    assert!(
        run(&a)
            .0
            .unwrap_err()
            .contains("--product \"P_\" isn't lowercase")
    );
    // Key files.
    let dir = &m.dir;
    let loose = key_file(dir, "loose.seed", &SEED7, 0o644);
    check(
        "mode",
        &format!(
            "the key file {loose} is mode 0644: it must not be readable by group or others (0600 or 0400)"
        ),
        &["--key-file", &loose],
    );
    let short = key_file(dir, "short.seed", &[7; 31], 0o600);
    check(
        "short",
        &format!(
            "the key file {short} is 31 bytes: it must be a raw 32-byte Ed25519 seed, or 64 hex digits"
        ),
        &["--key-file", &short],
    );
    let notes = key_file(dir, "notes.seed", &[b'z'; 64], 0o600);
    check(
        "hex",
        "64 characters and not all of them hex digits",
        &["--key-file", &notes],
    );
    let seed = key_file(dir, "ok.seed", &SEED7, 0o400);
    check(
        "id",
        &format!(
            "the key file's key id is {KEY_ID7}; the build expects {}",
            "00".repeat(32)
        ),
        &["--key-file", &seed, "--expect-key-id", &"00".repeat(32)],
    );
    // Hex form and a trailing newline are the same key.
    let hexed = key_file(
        dir,
        "hex.seed",
        format!("{}\n", hex(&SEED7)).as_bytes(),
        0o600,
    );
    let (r, out, _) = m.make("hexed.bundle", &["--key-file", &hexed]);
    r.unwrap();
    assert!(
        out.contains(&format!("signed with key id {KEY_ID7}")),
        "{out}"
    );
}

#[test]
fn the_table_is_held_to_the_image() {
    let m = made("table", false, false);
    let table = std::fs::read_to_string(&m.table).unwrap();
    let run_with = |label: &str, text: String, image: Option<Vec<u8>>| {
        let t = m.dir.join(format!("{label}.txt"));
        std::fs::write(&t, text).unwrap();
        let mut mm = Made {
            dir: m.dir.clone(),
            board: m.board.clone(),
            payload: m.payload.clone(),
            rootfs: m.rootfs.clone(),
            table: t.display().to_string(),
        };
        if let Some(bytes) = image {
            let p = m.dir.join(format!("{label}.erofs"));
            std::fs::write(&p, bytes).unwrap();
            mm.rootfs = p.display().to_string();
        }
        mm.make(&format!("{label}.bundle"), &[]).0.unwrap_err()
    };
    let root = table
        .lines()
        .find(|l| l.starts_with("root_hash "))
        .unwrap()
        .to_string();
    let why = run_with(
        "root",
        table.replace(&root, &format!("root_hash {}", "00".repeat(32))),
        None,
    );
    assert!(
        why.contains("data hashes to root hash") && why.contains(&"00".repeat(32)),
        "{why}"
    );
    let why = run_with(
        "blocks",
        table.replace("data_blocks 300", "data_blocks 299"),
        None,
    );
    assert!(why.contains("verity table: hash_offset"), "{why}");
    let why = run_with(
        "algo",
        table.replace("algorithm sha256", "algorithm sha512"),
        None,
    );
    assert!(why.contains("algorithm is sha512"), "{why}");
    let mut longer = std::fs::read(&m.rootfs).unwrap();
    longer.push(0);
    let why = run_with("longer", table.clone(), Some(longer));
    assert!(
        why.contains("bytes, and the verity table's image_bytes is"),
        "{why}"
    );
}

/// No output of any run carries the seed, in hex or raw: a distinctive seed through every path.
#[test]
fn no_output_carries_the_key() {
    let m = made("secret", false, false);
    let seed: [u8; 32] = std::array::from_fn(|i| 0xA0 ^ (i as u8).wrapping_mul(7));
    let seed_hex = hex(&seed);
    let good = key_file(&m.dir, "good.seed", seed_hex.as_bytes(), 0o600);
    let loose = key_file(&m.dir, "loose.seed", &seed, 0o644);
    let mut seen = String::new();
    for extra in [
        vec!["--key-file", &good],
        vec!["--key-file", &loose],
        vec!["--key-file", &good, "--expect-key-id", "ab"],
    ] {
        let (r, out, _) = m.make(&format!("s{}.bundle", extra.len()), &extra);
        seen.push_str(&out);
        if let Err(e) = r {
            seen.push_str(&e);
        }
    }
    let (_, out, file) = m.make("signed.bundle", &["--key-file", &good]);
    seen.push_str(&out);
    seen.push_str(&m.verify(&file, &[]).1);
    assert!(!seen.is_empty());
    assert!(!seen.contains(&seed_hex), "the seed's hex is in the output");
    assert!(
        !seen.as_bytes().windows(32).any(|w| w == seed),
        "the seed's bytes are in the output"
    );
    // The bundle itself carries the public side only.
    let bytes = std::fs::read(&file).unwrap();
    assert!(!bytes.windows(32).any(|w| w == seed));
}

/// The file the command wrote is byte for byte what the library makes from the same inputs (the
/// library's `build` and `write_to` are the writer the installer's tests use): the tool adds nothing of
/// its own to the bytes.
#[test]
fn the_cli_file_is_byte_identical_to_the_librarys_for_the_same_inputs() {
    use lemnos_slot_store::bays::Layout;
    use lemnos_slot_store::bootimage::Family;
    use lemnos_slot_store::bundle::{Build, Rules, Signer, build, write_to};
    use lemnos_slot_store::manifest::Overlay;
    let m = made("identical", false, true);
    let seed = key_file(&m.dir, "signing.seed", &SEED7, 0o600);
    let (r, _, file) = m.make(
        "cli.bundle",
        &[
            "--key-file",
            &seed,
            "--variant",
            "lite",
            "--kernel",
            "6.12",
            "--display-name",
            "An OS",
        ],
    );
    r.unwrap();
    let text = std::fs::read_to_string(&m.board).unwrap();
    let board = lemnos_host::board::parse(&text).unwrap();
    let map = lemnos_devtree::SocMap::parse(
        &std::fs::read_to_string(repo().join("socs/bcm2712-rp1.toml")).unwrap(),
    )
    .unwrap();
    let manifests: Vec<_> = lemnosd::builtin::drivers()
        .iter()
        .map(|e| *e.manifest())
        .collect();
    let generated = lemnos_devtree::generate(&board, &map, &manifests).unwrap();
    let b = &board.boot;
    let rules = Rules {
        board: board.board.id.clone(),
        prefix: "os".into(),
        boot_sources: vec!["rpi-boot-mode-1=/soc@107c000000/mmc@fff000".into()],
        family: Family::Tryboot,
        layout: Layout::new([
            b.bay_kernel_bytes.unwrap(),
            b.bay_initramfs_bytes.unwrap(),
            b.bay_device_tree_bytes.unwrap(),
            b.bay_cmdline_bytes.unwrap(),
            b.bay_overlay_bytes.unwrap(),
        ])
        .unwrap(),
        overlay: generated.overlay_sha256.map(|sha256| Overlay {
            board: board.board.id.clone(),
            sha256,
        }),
    };
    let table = verity::Table::parse(&std::fs::read_to_string(&m.table).unwrap()).unwrap();
    let payload = |role: &str| std::fs::read(Path::new(&m.payload).join(role)).unwrap();
    let payloads = [
        "kernel",
        "initramfs",
        "device-tree",
        "cmdline",
        "board-overlay",
    ]
    .map(payload);
    let key = SigningKey::from_bytes(&SEED7);
    let sign = |message: &[u8]| key.sign(message).to_bytes();
    let whole = std::fs::read(&m.rootfs).unwrap();
    let built = build(
        &Build {
            id: "a1",
            product: "an-os",
            version: "1.0",
            variant: "lite",
            kernel: "6.12",
            display_name: "An OS",
            table: &table,
            rules: &rules,
            signer: Some(Signer {
                key_id: signing::key_id(key.verifying_key().as_bytes()),
                sign: &sign,
            }),
        },
        payloads,
        &mut &whole[..],
    )
    .unwrap();
    let mut library = Vec::new();
    write_to(&built, &mut std::io::Cursor::new(whole), &mut library).unwrap();
    assert!(
        std::fs::read(&file).unwrap() == library,
        "the CLI's bundle differs from the library's"
    );
}
