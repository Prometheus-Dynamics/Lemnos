//! Firmware overlays and config lines (`[boot] firmware_overlays`, `config_lines`), end to end through
//! the binary's own commands: `tree` makes the payloads, `bundle --firmware` the signed bundle,
//! `boot-partition --firmware` the boot image, and `data-partition` writes and verifies the tree. A
//! board with neither key is the reference board, unchanged. Overlay names are neutral (`test-*`).

use lemnos_slot_store::bootfat;
use lemnos_slot_store::bootimage::Mem;
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
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("firmware-overlays-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The documented test key (as the data-partition tests use it).
const SEED7: [u8; 32] = [7; 32];
const PUBKEY7: &str = "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c";
const TIME: &str = "2026-11-14T20:00:00Z";
const DATA_SIZE: &str = "8589934592";

/// The reference board with `[boot]` keys added: the lines, and the overlays they name.
fn board_text(lines: &[&str], overlays: &[&str]) -> String {
    let mut text = std::fs::read_to_string(repo().join("testing/boards/reference.toml")).unwrap();
    let quote = |items: &[&str]| {
        items
            .iter()
            .map(|s| format!("{s:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut keys = String::new();
    if !lines.is_empty() {
        keys.push_str(&format!("config_lines = [{}]\n", quote(lines)));
    }
    if !overlays.is_empty() {
        keys.push_str(&format!("firmware_overlays = [{}]\n", quote(overlays)));
    }
    assert!(text.matches("[boot]\n").count() == 1);
    text = text.replacen("[boot]\n", &format!("[boot]\n{keys}"), 1);
    text
}

fn write_board(dir: &Path, text: &str) -> String {
    let path = dir.join("board.toml");
    std::fs::write(&path, text).unwrap();
    path.display().to_string()
}

/// A firmware directory holding each named overlay's `.dtbo`.
fn firmware_dir(dir: &Path, names: &[&str]) -> String {
    let fw = dir.join("firmware");
    std::fs::create_dir_all(&fw).unwrap();
    for name in names {
        std::fs::write(fw.join(format!("{name}.dtbo")), format!("overlay {name}\n")).unwrap();
    }
    fw.display().to_string()
}

fn erofs(blocks: usize) -> Vec<u8> {
    let mut image: Vec<u8> = (0..blocks * 4096).map(|i| (i % 249) as u8).collect();
    image[1024..1028].copy_from_slice(&[0xe2, 0xe1, 0xf5, 0xe0]);
    image[1024 + 12] = 12;
    image[1024 + 36..1024 + 40].copy_from_slice(&(blocks as u32).to_le_bytes());
    image
}

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

/// The roles payload directory `tree --cmdline-slot` writes for `slot`.
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

/// A signed bundle of `slot` for `board`, with `firmware` as its `--firmware` directory when given.
fn bundle(dir: &Path, board: &str, slot: &str, firmware: Option<&str>) -> (Result<(), String>, String, PathBuf) {
    let payload = payload(dir, board, slot);
    let socs = socs();
    let (rootfs, table) = rootfs(dir, 300);
    let seed = dir.join("signing.seed");
    std::fs::write(&seed, SEED7).unwrap();
    std::fs::set_permissions(&seed, std::fs::Permissions::from_mode(0o600)).unwrap();
    let out = dir.join(format!("{slot}.bundle"));
    // The payloads as the bundle holds them (the root hash filled in): what the boot partition takes.
    let final_dir = dir.join("final");
    let final_text = final_dir.display().to_string();
    let mut args = vec![
        "bundle", "--layout-of", board, "--socs", &socs, "--slot", slot, "--payload", &payload,
    ];
    if let Some(fw) = firmware {
        args.extend(["--firmware", fw]);
    }
    let seed_path = seed.display().to_string();
    let out_path = out.display().to_string();
    args.extend([
        "--rootfs", &rootfs, "--verity-table", &table, "--product", "an-os", "--version", "1.0",
        "--key-file", &seed_path, "--payload-out", &final_text, "--out", &out_path,
    ]);
    let (r, text) = run(&args);
    (r, text, out)
}

/// A boot image for `slot` from its roles and `firmware`, at the smallest whole MiB.
fn boot_image(dir: &Path, board: &str, slot: &str, payloads: &str, firmware: Option<&str>) -> (Result<String, String>, String) {
    let socs = socs();
    let (r, min) = run(&["boot-partition", "--layout-of", board, "--socs", &socs, "--min-size"]);
    r.unwrap();
    let size = min.trim().parse::<u64>().unwrap().div_ceil(1 << 20) << 20;
    let image = dir.join(format!("boot-{slot}.img")).display().to_string();
    let mut args = vec![
        "boot-partition", "--layout-of", board, "--socs", &socs, "--slot", slot, "--payload", payloads,
    ];
    if let Some(fw) = firmware {
        args.extend(["--firmware", fw]);
    }
    let size_text = size.to_string();
    args.extend(["--size", &size_text, "--out", &image]);
    let (r, out) = run(&args);
    (r.map(|()| image), out)
}

fn keys(dir: &Path) -> String {
    let path = dir.join("keys");
    std::fs::write(&path, PUBKEY7).unwrap();
    path.display().to_string()
}

fn text_of(image: &str, name: &str) -> Result<Vec<u8>, String> {
    let mut mem = Mem(std::fs::read(image).unwrap());
    bootfat::read_file(&mut mem, name, 64 * 1024).map_err(|e| format!("{name}: {e}"))
}

#[test]
fn a_board_with_two_overlays_goes_from_tree_to_bundle_to_boot_partition_to_verified_tree() {
    let dir = scratch("pipeline");
    let board = write_board(&dir, &board_text(
        &["dtoverlay=test-ramoops", "dtoverlay=test-gpu,gpiopin=5"],
        &["test-ramoops", "test-gpu"],
    ));
    let fw = firmware_dir(&dir, &["test-ramoops", "test-gpu"]);
    let (r, out, bundle_path) = bundle(&dir, &board, "a1", Some(&fw));
    r.unwrap_or_else(|e| panic!("bundle: {e}\n{out}"));
    let (image, boot_out) = boot_image(&dir, &board, "a1", &dir.join("final").display().to_string(), Some(&fw));
    let image = image.unwrap_or_else(|e| panic!("boot-partition: {e}\n{boot_out}"));

    // The firmware text carries the board's lines, and the root holds the declared list.
    let config = String::from_utf8(text_of(&image, "config.txt").unwrap()).unwrap();
    assert!(config.contains("dtoverlay=test-ramoops\n"), "{config}");
    assert!(config.contains("dtoverlay=test-gpu,gpiopin=5\n"), "{config}");
    let list = String::from_utf8(text_of(&image, "firmware-overlays.txt").unwrap()).unwrap();
    assert!(list.starts_with("config "), "{list}");
    assert!(list.contains("overlay test-ramoops\noverlay test-gpu\n"), "{list}");

    // The image verifies, with both overlays in the bay's record.
    let (r, report) = run(&["boot-partition", "--layout-of", &board, "--socs", &socs(), "--verify", &image]);
    r.unwrap_or_else(|e| panic!("verify: {e}\n{report}"));
    assert!(report.contains("bay0 (a1): the five payloads and 2 firmware overlay(s) match their record"), "{report}");
    assert!(report.contains("firmware-overlays.txt: holds its generated bytes"), "{report}");

    // The bundle carries the same two overlays and the lines' digest, and the tree writes and verifies.
    let k = keys(&dir);
    let out_dir = dir.join("tree");
    let (r, text) = run(&[
        "data-partition", "--layout-of", &board, "--socs", &socs(), "--install",
        &bundle_path.display().to_string(), "--boot-image", &image, "--keys", &k, "--time", TIME,
        "--data-size", DATA_SIZE, "--out", &out_dir.display().to_string(),
    ]);
    r.unwrap_or_else(|e| panic!("data-partition: {e}\n{text}"));
    let (r, text) = run(&[
        "data-partition", "--layout-of", &board, "--socs", &socs(), "--verify",
        &out_dir.display().to_string(), "--boot-image", &image,
    ]);
    r.unwrap_or_else(|e| panic!("verify tree: {e}\n{text}"));
}

#[test]
fn a_declared_overlay_missing_from_the_firmware_directory_is_refused_by_name() {
    let dir = scratch("missing");
    let board = write_board(&dir, &board_text(
        &["dtoverlay=test-ramoops", "dtoverlay=test-gpu,gpiopin=5"],
        &["test-ramoops", "test-gpu"],
    ));
    let full = firmware_dir(&dir, &["test-ramoops", "test-gpu"]);
    let (r, out, _) = bundle(&dir, &board, "a1", Some(&full));
    r.unwrap_or_else(|e| panic!("{e}\n{out}"));
    // A directory of its own, holding only the first overlay.
    let short_dir = dir.join("firmware-short");
    std::fs::create_dir_all(&short_dir).unwrap();
    std::fs::write(short_dir.join("test-ramoops.dtbo"), b"overlay test-ramoops\n").unwrap();
    let short = short_dir.display().to_string();
    let (r, _) = boot_image(&dir, &board, "a1", &dir.join("final").display().to_string(), Some(&short));
    let why = r.unwrap_err();
    assert!(why.contains("test-gpu"), "{why}");
    let short_bundle_dir = dir.join("short-bundle");
    std::fs::create_dir_all(&short_bundle_dir).unwrap();
    let (r, _, _) = bundle(&short_bundle_dir, &board, "a1", Some(&short));
    assert!(r.unwrap_err().contains("test-gpu"));
}

#[test]
fn a_board_with_overlays_but_no_firmware_directory_is_refused_and_says_what_to_give() {
    let dir = scratch("nodir");
    let board = write_board(&dir, &board_text(&["dtoverlay=test-gpu"], &["test-gpu"]));
    let (r, _, _) = bundle(&dir, &board, "a1", None);
    assert!(r.unwrap_err().contains("--firmware DIR"));
}

#[test]
fn a_config_line_that_loads_an_undeclared_overlay_is_refused_with_both_names() {
    let dir = scratch("undeclared");
    let board = write_board(&dir, &board_text(&["dtoverlay=test-gpu", "dtoverlay=test-other"], &["test-gpu"]));
    let (r, _) = run(&["boot-partition", "--layout-of", &board, "--socs", &socs(), "--min-size"]);
    let why = r.unwrap_err();
    assert!(why.contains("test-other") && why.contains("firmware_overlays"), "{why}");
}

#[test]
fn a_declared_overlay_no_config_line_loads_is_refused_with_its_name() {
    let dir = scratch("unloaded");
    let board = write_board(&dir, &board_text(&["dtoverlay=test-gpu"], &["test-gpu", "test-ramoops"]));
    let (r, _) = run(&["boot-partition", "--layout-of", &board, "--socs", &socs(), "--min-size"]);
    let why = r.unwrap_err();
    assert!(why.contains("test-ramoops") && why.contains("no config_lines entry loads it"), "{why}");
}

#[test]
fn a_config_line_that_is_not_one_line_is_refused() {
    let dir = scratch("twolines");
    let board = write_board(&dir, &board_text(&["dtoverlay=test-gpu\ndtoverlay=test-x"], &["test-gpu"]));
    let (r, _) = run(&["boot-partition", "--layout-of", &board, "--socs", &socs(), "--min-size"]);
    assert!(r.unwrap_err().contains("is not one firmware line"));
}

#[test]
fn a_bundle_whose_config_lines_differ_from_the_boot_partitions_is_refused_by_the_install_with_both_digests() {
    let dir = scratch("drift");
    let overlay_board = write_board(&dir, &board_text(
        &["dtoverlay=test-gpu,gpiopin=5"],
        &["test-gpu"],
    ));
    let fw = firmware_dir(&dir, &["test-gpu"]);
    let (r, out, bundle_path) = bundle(&dir, &overlay_board, "a1", Some(&fw));
    r.unwrap_or_else(|e| panic!("{e}\n{out}"));
    // A partition with the same overlays and another line (gpiopin 6): the roles agree, the lines do not.
    let plain_dir = dir.join("plain");
    std::fs::create_dir_all(&plain_dir).unwrap();
    let other = write_board(&plain_dir, &board_text(&["dtoverlay=test-gpu,gpiopin=6"], &["test-gpu"]));
    let (image, boot_out) = boot_image(&dir, &other, "a1", &dir.join("final").display().to_string(), Some(&fw));
    let image = image.unwrap_or_else(|e| panic!("{e}\n{boot_out}"));
    let (r, text) = run(&[
        "data-partition", "--layout-of", &overlay_board, "--socs", &socs(), "--install",
        &bundle_path.display().to_string(), "--boot-image", &image, "--keys", &keys(&dir), "--time", TIME,
        "--data-size", DATA_SIZE, "--out", &dir.join("refused").display().to_string(),
    ]);
    let why = r.unwrap_err();
    // The layout check names the two digests first; the install's own check is the same rule.
    assert!(why.contains("firmware config lines") && why.contains("other lines"), "{why}\n{text}");
}

#[test]
fn a_board_with_neither_key_writes_no_firmware_list_and_its_bundle_has_no_config_digest() {
    let dir = scratch("nokeys");
    let board = write_board(&dir, &board_text(&[], &[]));
    let (r, out, bundle_path) = bundle(&dir, &board, "a1", None);
    r.unwrap_or_else(|e| panic!("{e}\n{out}"));
    let (image, boot_out) = boot_image(&dir, &board, "a1", &dir.join("final").display().to_string(), None);
    let image = image.unwrap_or_else(|e| panic!("{e}\n{boot_out}"));
    let mut mem = Mem(std::fs::read(&image).unwrap());
    assert!(bootfat::read_file(&mut mem, "firmware-overlays.txt", 4096).is_err());
    let bundle_text = String::from_utf8_lossy(&std::fs::read(&bundle_path).unwrap()).into_owned();
    assert!(!bundle_text.contains("\"config\":"), "the bundle carries no config digest");
    assert!(bundle_text.contains("slot.json"));
}
