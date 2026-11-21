//! Firmware overlays and config lines (`[boot] firmware_overlays`, `config_lines`), end to end through
//! the binary's own commands: `tree` makes the payloads, `bundle --firmware` the signed bundle,
//! `boot-partition --firmware` the boot image, and `data-partition` writes and verifies the tree. A
//! board with neither key is the reference board, unchanged. Overlay names are neutral (`test-*`).

use lemnos_slot_store::bootfat;
use lemnos_slot_store::bootimage::Mem;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

#[path = "support/pi.rs"]
mod pi;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn socs() -> String {
    repo().join("socs").display().to_string()
}

fn run(args: &[&str]) -> (Result<(), String>, String) {
    // Each test gives --firmware itself: what a missing directory does is tested here.
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
    board_with(lines, overlays, "")
}

/// The reference board with `[boot]` keys added: `extra` is written as given, after the others.
fn board_with(lines: &[&str], overlays: &[&str], extra: &str) -> String {
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
    keys.push_str(extra);
    assert!(text.matches("[boot]\n").count() == 1);
    text = text.replacen("[boot]\n", &format!("[boot]\n{keys}"), 1);
    text
}

fn write_board(dir: &Path, text: &str) -> String {
    let path = dir.join("board.toml");
    std::fs::write(&path, text).unwrap();
    path.display().to_string()
}

/// A firmware directory holding the maps and each named overlay's `.dtbo`.
fn firmware_dir(dir: &Path, names: &[&str]) -> String {
    let fw = dir.join("firmware");
    let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    pi::write_firmware(&fw, &names);
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

/// A signed bundle of `slot` for `board`, with `firmware` as its `--firmware` directory when given.
fn bundle(
    dir: &Path,
    board: &str,
    slot: &str,
    firmware: Option<&str>,
) -> (Result<(), String>, String, PathBuf) {
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
        "bundle",
        "--layout-of",
        board,
        "--socs",
        &socs,
        "--slot",
        slot,
        "--payload",
        &payload,
    ];
    if let Some(fw) = firmware {
        args.extend(["--firmware", fw]);
    }
    let seed_path = seed.display().to_string();
    let out_path = out.display().to_string();
    args.extend([
        "--rootfs",
        &rootfs,
        "--verity-table",
        &table,
        "--product",
        "an-os",
        "--version",
        "1.0",
        "--key-file",
        &seed_path,
        "--payload-out",
        &final_text,
        "--out",
        &out_path,
    ]);
    let (r, text) = run(&args);
    (r, text, out)
}

/// A boot image for `slot` from its roles and `firmware`, at the smallest whole MiB.
fn boot_image(
    dir: &Path,
    board: &str,
    slot: &str,
    payloads: &str,
    firmware: Option<&str>,
) -> (Result<String, String>, String) {
    let socs = socs();
    let (r, min) = run(&[
        "boot-partition",
        "--layout-of",
        board,
        "--socs",
        &socs,
        "--min-size",
    ]);
    r.unwrap();
    let size = min.trim().parse::<u64>().unwrap().div_ceil(1 << 20) << 20;
    let image = dir.join(format!("boot-{slot}.img")).display().to_string();
    let mut args = vec![
        "boot-partition",
        "--layout-of",
        board,
        "--socs",
        &socs,
        "--slot",
        slot,
        "--payload",
        payloads,
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
    let board = write_board(
        &dir,
        &board_text(
            &["dtoverlay=test-ramoops", "dtoverlay=test-gpu,gpiopin=5"],
            &["test-ramoops", "test-gpu"],
        ),
    );
    let fw = firmware_dir(&dir, &["test-ramoops", "test-gpu"]);
    let (r, out, bundle_path) = bundle(&dir, &board, "a1", Some(&fw));
    r.unwrap_or_else(|e| panic!("bundle: {e}\n{out}"));
    let (image, boot_out) = boot_image(
        &dir,
        &board,
        "a1",
        &dir.join("final").display().to_string(),
        Some(&fw),
    );
    let image = image.unwrap_or_else(|e| panic!("boot-partition: {e}\n{boot_out}"));

    // The firmware text carries the board's lines, and the root holds the declared list.
    let config = String::from_utf8(text_of(&image, "config.txt").unwrap()).unwrap();
    assert!(config.contains("dtoverlay=test-ramoops\n"), "{config}");
    assert!(
        config.contains("dtoverlay=test-gpu,gpiopin=5\n"),
        "{config}"
    );
    let list = String::from_utf8(text_of(&image, "firmware-overlays.txt").unwrap()).unwrap();
    assert!(list.starts_with("form firmware\n"), "{list}");
    assert!(list.contains("\nconfig "), "{list}");
    assert!(
        list.contains("overlay test-ramoops\noverlay test-gpu\n"),
        "{list}"
    );

    // The image verifies, with both overlays in the bay's record.
    let (r, report) = run(&[
        "boot-partition",
        "--layout-of",
        &board,
        "--socs",
        &socs(),
        "--verify",
        &image,
    ]);
    r.unwrap_or_else(|e| panic!("verify: {e}\n{report}"));
    assert!(
        report.contains("bay0 (a1): its 10 files match their record"),
        "{report}"
    );
    assert!(
        report.contains("firmware-overlays.txt: holds its generated bytes"),
        "{report}"
    );

    // The bundle carries the same two overlays and the lines' digest, and the tree writes and verifies.
    let k = keys(&dir);
    let out_dir = dir.join("tree");
    let (r, text) = run(&[
        "data-partition",
        "--layout-of",
        &board,
        "--socs",
        &socs(),
        "--install",
        &bundle_path.display().to_string(),
        "--boot-image",
        &image,
        "--keys",
        &k,
        "--time",
        TIME,
        "--data-size",
        DATA_SIZE,
        "--out",
        &out_dir.display().to_string(),
    ]);
    r.unwrap_or_else(|e| panic!("data-partition: {e}\n{text}"));
    let (r, text) = run(&[
        "data-partition",
        "--layout-of",
        &board,
        "--socs",
        &socs(),
        "--verify",
        &out_dir.display().to_string(),
        "--boot-image",
        &image,
    ]);
    r.unwrap_or_else(|e| panic!("verify tree: {e}\n{text}"));
}

#[test]
fn a_declared_overlay_missing_from_the_firmware_directory_is_refused_by_name() {
    let dir = scratch("missing");
    let board = write_board(
        &dir,
        &board_text(
            &["dtoverlay=test-ramoops", "dtoverlay=test-gpu,gpiopin=5"],
            &["test-ramoops", "test-gpu"],
        ),
    );
    let full = firmware_dir(&dir, &["test-ramoops", "test-gpu"]);
    let (r, out, _) = bundle(&dir, &board, "a1", Some(&full));
    r.unwrap_or_else(|e| panic!("{e}\n{out}"));
    // A directory of its own, holding only the first overlay.
    let short_dir = dir.join("firmware-short");
    std::fs::create_dir_all(&short_dir).unwrap();
    std::fs::write(
        short_dir.join("test-ramoops.dtbo"),
        b"overlay test-ramoops\n",
    )
    .unwrap();
    let short = short_dir.display().to_string();
    let (r, _) = boot_image(
        &dir,
        &board,
        "a1",
        &dir.join("final").display().to_string(),
        Some(&short),
    );
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
    let board = write_board(
        &dir,
        &board_text(
            &["dtoverlay=test-gpu", "dtoverlay=test-other"],
            &["test-gpu"],
        ),
    );
    let (r, _) = run(&[
        "boot-partition",
        "--layout-of",
        &board,
        "--socs",
        &socs(),
        "--min-size",
    ]);
    let why = r.unwrap_err();
    assert!(
        why.contains("test-other") && why.contains("firmware_overlays"),
        "{why}"
    );
}

#[test]
fn a_declared_overlay_no_config_line_loads_is_refused_with_its_name() {
    let dir = scratch("unloaded");
    let board = write_board(
        &dir,
        &board_text(&["dtoverlay=test-gpu"], &["test-gpu", "test-ramoops"]),
    );
    let (r, _) = run(&[
        "boot-partition",
        "--layout-of",
        &board,
        "--socs",
        &socs(),
        "--min-size",
    ]);
    let why = r.unwrap_err();
    assert!(
        why.contains("test-ramoops") && why.contains("no config_lines entry loads it"),
        "{why}"
    );
}

#[test]
fn a_config_line_that_is_not_one_line_is_refused() {
    let dir = scratch("twolines");
    let board = write_board(
        &dir,
        &board_text(&["dtoverlay=test-gpu\ndtoverlay=test-x"], &["test-gpu"]),
    );
    let (r, _) = run(&[
        "boot-partition",
        "--layout-of",
        &board,
        "--socs",
        &socs(),
        "--min-size",
    ]);
    assert!(r.unwrap_err().contains("is not one firmware line"));
}

#[test]
fn a_bundle_whose_config_lines_differ_from_the_boot_partitions_is_refused_by_the_install_with_both_digests()
 {
    let dir = scratch("drift");
    let overlay_board = write_board(
        &dir,
        &board_text(&["dtoverlay=test-gpu,gpiopin=5"], &["test-gpu"]),
    );
    let fw = firmware_dir(&dir, &["test-gpu"]);
    let (r, out, bundle_path) = bundle(&dir, &overlay_board, "a1", Some(&fw));
    r.unwrap_or_else(|e| panic!("{e}\n{out}"));
    // A partition with the same overlays and another line (gpiopin 6): the roles agree, the lines do not.
    let plain_dir = dir.join("plain");
    std::fs::create_dir_all(&plain_dir).unwrap();
    let other = write_board(
        &plain_dir,
        &board_text(&["dtoverlay=test-gpu,gpiopin=6"], &["test-gpu"]),
    );
    let (image, boot_out) = boot_image(
        &dir,
        &other,
        "a1",
        &dir.join("final").display().to_string(),
        Some(&fw),
    );
    let image = image.unwrap_or_else(|e| panic!("{e}\n{boot_out}"));
    let (r, text) = run(&[
        "data-partition",
        "--layout-of",
        &overlay_board,
        "--socs",
        &socs(),
        "--install",
        &bundle_path.display().to_string(),
        "--boot-image",
        &image,
        "--keys",
        &keys(&dir),
        "--time",
        TIME,
        "--data-size",
        DATA_SIZE,
        "--out",
        &dir.join("refused").display().to_string(),
    ]);
    let why = r.unwrap_err();
    // The layout check names the two digests first; the install's own check is the same rule.
    assert!(
        why.contains("firmware config lines") && why.contains("other lines"),
        "{why}\n{text}"
    );
}

#[test]
fn a_board_with_neither_key_writes_no_firmware_list_and_its_bundle_has_no_config_digest() {
    let dir = scratch("nokeys");
    let board = write_board(&dir, &board_text(&[], &[]));
    // Only the firmware's own maps: the board declares no overlay.
    let maps = firmware_dir(&dir, &[]);
    let (r, out, bundle_path) = bundle(&dir, &board, "a1", Some(&maps));
    r.unwrap_or_else(|e| panic!("{e}\n{out}"));
    let (image, boot_out) = boot_image(
        &dir,
        &board,
        "a1",
        &dir.join("final").display().to_string(),
        Some(&maps),
    );
    let image = image.unwrap_or_else(|e| panic!("{e}\n{boot_out}"));
    // The member list is written for every bay (it names the form, the tree's names and the
    // firmware files), and it has no config line and no overlay.
    let list = String::from_utf8(text_of(&image, "firmware-overlays.txt").unwrap()).unwrap();
    assert!(!list.contains("config "), "{list}");
    assert!(!list.contains("overlay "), "{list}");
    assert!(list.contains("file overlay_map.dtb\n"), "{list}");
    let bundle_text = String::from_utf8_lossy(&std::fs::read(&bundle_path).unwrap()).into_owned();
    assert!(
        !bundle_text.contains("\"config\":"),
        "the bundle carries no config digest"
    );
    assert!(bundle_text.contains("slot.json"));
}

// ---- The host merge: `boot-partition --verify --dtmerge TOOL` ----
//
// The base tree and the overlays are real device-tree blobs, compiled by dtc. The tool is a stand-in
// for raspberrypi/utils' dtmerge (see `stand_in`), so these tests show the Lemnos side: the overlays
// are applied in order with their config-line parameters, a failure is named, and `expect_nodes` is
// checked in the merged tree. The real dtmerge is not on this host, so these tests did not run it.

/// A labelled node, so dtc's `-@` writes `__symbols__` and the board overlay's label resolves, as on a
/// real board tree.
const BASE_DTS: &str = "/dts-v1/;\n/ {\n\tsoc {\n\t\tgpio: gpio {\n\t\t};\n\t\tgpu {\n\t\t\tstatus = \"disabled\";\n\t\t};\n\t};\n\treserved-memory {\n\t};\n};\n";
/// `ram_size` is the test parameter: `__overrides__` maps it to the cell, as dtmerge reads it.
const RAM_DTS: &str = "/dts-v1/;\n/plugin/;\n/ {\n\tfragment@0 {\n\t\ttarget-path = \"/reserved-memory\";\n\t\t__overlay__ {\n\t\t\ttest_ram: test-ram {\n\t\t\t\trecord-size = <0x4000>;\n\t\t\t};\n\t\t};\n\t};\n\t__overrides__ {\n\t\tram_size = <&test_ram>, \"record-size:0\";\n\t};\n};\n";
const GPU_DTS: &str = "/dts-v1/;\n/plugin/;\n/ {\n\tfragment@0 {\n\t\ttarget-path = \"/soc/gpu\";\n\t\t__overlay__ {\n\t\t\tstatus = \"okay\";\n\t\t};\n\t};\n};\n";
/// Its target is not in the base tree: it does not apply.
const MISSING_DTS: &str = "/dts-v1/;\n/plugin/;\n/ {\n\tfragment@0 {\n\t\ttarget-path = \"/no/such/node\";\n\t\t__overlay__ {\n\t\t\tstatus = \"okay\";\n\t\t};\n\t};\n};\n";

/// dtc must be installed: these tests compile real blobs with it. Refused by name when it is not.
fn compile(dir: &Path, name: &str, text: &str) -> Vec<u8> {
    let src = dir.join(format!("{name}.dts"));
    std::fs::write(&src, text).unwrap();
    let out = dir.join(format!("{name}.blob"));
    let status = std::process::Command::new("dtc")
        .args(["-@", "-I", "dts", "-O", "dtb", "-o"])
        .arg(&out)
        .arg(&src)
        .status()
        .expect("dtc is needed for the host-merge tests (device-tree-compiler)");
    assert!(status.success(), "dtc failed on {name}");
    std::fs::read(&out).unwrap()
}

/// A stand-in for dtmerge, used when LEMNOS_DTMERGE is not set: the real tool's command line
/// (`BASE MERGED OVERLAY [name=value]...`) and its failure statuses (255 when a file does not load,
/// 1 when the merge fails, as the real tool exits), with libfdt's `fdtoverlay` applying the overlay.
/// NOT shared with the real tool: `__overrides__` is not read, so parameters are accepted and not
/// applied (a test overlay's literal value must already match); the real tool's messages are not
/// reproduced; and only the first four bytes of each file are checked before applying. CI sets
/// LEMNOS_DTMERGE to the real tool (built from raspberrypi/utils at a pinned commit), so every test
/// here runs against it there, and the parameter test below runs only there.
const STAND_IN: &str = r#"#!/bin/sh
if [ "$#" -lt 3 ]; then echo "usage: dtmerge [<options] <base dtb> <merged dtb> <overlay dtb> [param=value] ..." >&2; exit 1; fi
base=$1; merged=$2; overlay=$3
if [ "$overlay" = "-" ]; then echo "* stand-in: parameters-only mode is not implemented" >&2; exit 255; fi
if [ "$(od -A n -t x1 -N 4 "$base" 2>/dev/null | tr -d ' \n')" != "d00dfeed" ]; then echo "* failed to load '$base'" >&2; exit 255; fi
if [ "$(od -A n -t x1 -N 4 "$overlay" 2>/dev/null | tr -d ' \n')" != "d00dfeed" ]; then echo "* failed to load '$overlay'" >&2; exit 255; fi
if ! fdtoverlay -i "$base" -o "$merged" "$overlay"; then echo "* merge failed" >&2; exit 1; fi
exit 0
"#;

/// The merge tool the host-merge tests use: the real dtmerge named by LEMNOS_DTMERGE, else the stand-in.
fn merge_tool(dir: &Path) -> String {
    match std::env::var("LEMNOS_DTMERGE") {
        Ok(path) if !path.is_empty() => {
            assert!(
                Path::new(&path).is_file(),
                "LEMNOS_DTMERGE={path} is not a file"
            );
            path
        }
        _ => stand_in(dir),
    }
}

/// The real dtmerge, when LEMNOS_DTMERGE names one. Under CI (CI is set) it must be named, so a
/// missing tool fails there instead of skipping.
fn real_tool() -> Option<String> {
    let named = std::env::var("LEMNOS_DTMERGE")
        .ok()
        .filter(|p| !p.is_empty());
    if named.is_none() && std::env::var_os("CI").is_some() {
        panic!("CI runs the real dtmerge: LEMNOS_DTMERGE must name it");
    }
    named
}

fn stand_in(dir: &Path) -> String {
    let path = dir.join("dtmerge-stand-in");
    std::fs::write(&path, STAND_IN).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.display().to_string()
}

/// A board with the host-merge keys, its firmware directory of compiled overlays, and the base tree.
struct HostCase {
    dir: PathBuf,
    board: String,
    firmware: String,
    base: Vec<u8>,
}

/// `dtbos` names each overlay to compile (its source text); the directory holds only those.
fn host_case(
    name: &str,
    lines: &[&str],
    overlays: &[&str],
    expect: &str,
    dtbos: &[(&str, &str)],
) -> HostCase {
    let dir = scratch(name);
    let board = write_board(&dir, &board_with(lines, overlays, expect));
    let fw = dir.join("firmware");
    pi::write_firmware(&fw, &[]);
    for (overlay, text) in dtbos {
        std::fs::write(
            fw.join(format!("{overlay}.dtbo")),
            compile(&dir, overlay, text),
        )
        .unwrap();
    }
    let base = compile(&dir, "base", BASE_DTS);
    HostCase {
        dir,
        board,
        firmware: fw.display().to_string(),
        base,
    }
}

/// The image of the case's board, its device-tree payload the compiled base tree.
fn host_image(case: &HostCase, slot: &str) -> Result<String, String> {
    let payloads = payload(&case.dir, &case.board, slot);
    std::fs::write(Path::new(&payloads).join("device-tree"), &case.base).unwrap();
    let (r, out) = boot_image(
        &case.dir,
        &case.board,
        slot,
        &payloads,
        Some(&case.firmware),
    );
    r.map_err(|e| format!("boot-partition: {e}\n{out}"))
}

fn verify_host(case: &HostCase, image: &str, tool: &str) -> (Result<(), String>, String) {
    let socs = socs();
    run(&[
        "boot-partition",
        "--verify",
        image,
        "--layout-of",
        &case.board,
        "--socs",
        &socs,
        "--dtmerge",
        tool,
    ])
}

const RAM_LINE: &str = "dtoverlay=test-ram,ram_size=16384";
const GPU_LINE: &str = "dtoverlay=test-gpu";
/// The merged tree must hold the ram size the line gives, and the GPU node's status.
const EXPECT_OK: &str = "expect_nodes = [{ path = \"/reserved-memory/test-ram\", properties = { record-size = \"$ram_size\" } }, { path = \"/soc/gpu\", properties = { status = \"okay\" } }]\n";

#[test]
fn the_declared_overlays_are_applied_on_the_host_in_order_and_the_expected_nodes_hold() {
    let case = host_case(
        "host-merge-ok",
        &[RAM_LINE, GPU_LINE],
        &["test-ram", "test-gpu"],
        EXPECT_OK,
        &[("test-ram", RAM_DTS), ("test-gpu", GPU_DTS)],
    );
    let tool = merge_tool(&case.dir);
    let image = host_image(&case, "a1").unwrap_or_else(|e| panic!("{e}"));
    let (r, out) = verify_host(&case, &image, &tool);
    r.unwrap_or_else(|e| panic!("verify: {e}\n{out}"));
    assert!(
        out.contains("ok  bay0 (a1): test-ram applies on host, not loaded by firmware"),
        "{out}"
    );
    assert!(
        out.contains("ok  bay0 (a1): test-gpu applies on host, not loaded by firmware"),
        "{out}"
    );
    assert!(
        out.contains(
            "/reserved-memory/test-ram record-size is 16384 (0x4000) after the host merge"
        ),
        "{out}"
    );
    assert!(
        out.contains("/soc/gpu status is \"okay\" after the host merge"),
        "{out}"
    );
    assert!(!out.contains("BAD"), "{out}");
}

#[test]
fn an_expected_node_value_the_merge_does_not_give_is_refused_with_both_values() {
    // The overlay gives 16384; the board expects 8192 literally.
    let expect = "expect_nodes = [{ path = \"/reserved-memory/test-ram\", properties = { record-size = 8192 } }]\n";
    let case = host_case(
        "host-merge-mismatch",
        &[RAM_LINE, GPU_LINE],
        &["test-ram", "test-gpu"],
        expect,
        &[("test-ram", RAM_DTS), ("test-gpu", GPU_DTS)],
    );
    let tool = merge_tool(&case.dir);
    let image = host_image(&case, "a1").unwrap_or_else(|e| panic!("{e}"));
    let (r, out) = verify_host(&case, &image, &tool);
    assert!(r.is_err(), "{out}");
    assert!(
        out.contains("BAD bay0 (a1): /reserved-memory/test-ram record-size is 16384 (0x4000), expected 8192 (0x2000), after the host merge"),
        "{out}"
    );
}

/// An overlay whose target is not in the tree: the image builder's firmware-layout check refuses
/// it by name, so no image carries it to the host merge.
#[test]
fn an_overlay_that_does_not_apply_is_refused_by_the_builder_by_name() {
    let expect = "expect_nodes = [{ path = \"/soc/gpu\", properties = { status = \"okay\" } }]\n";
    let case = host_case(
        "host-merge-missing",
        &[
            "dtoverlay=test-ram,ram_size=16384",
            "dtoverlay=test-missing",
        ],
        &["test-ram", "test-missing"],
        expect,
        &[("test-ram", RAM_DTS), ("test-missing", MISSING_DTS)],
    );
    let why = host_image(&case, "a1").unwrap_err();
    assert!(
        why.contains(
            "BAD overlays: slots/bay0/overlays/test-missing.dtbo does not apply to slots/bay0/bcm2712-rpi-5-b.dtb: /fragment@0: target-path /no/such/node is not in the tree"
        ),
        "{why}"
    );
    assert!(why.contains("was not written"), "{why}");
    assert!(!case.dir.join("boot-a1.img").exists());
}

/// A device-tree payload that is not a tree: the image builder refuses it by name.
#[test]
fn a_base_tree_that_is_not_a_device_tree_is_refused_by_name() {
    let case = host_case(
        "host-merge-base",
        &[GPU_LINE],
        &["test-gpu"],
        "",
        &[("test-gpu", GPU_DTS)],
    );
    let payloads = payload(&case.dir, &case.board, "a1");
    std::fs::write(Path::new(&payloads).join("device-tree"), b"not a tree").unwrap();
    let (r, out) = boot_image(
        &case.dir,
        &case.board,
        "a1",
        &payloads,
        Some(&case.firmware),
    );
    let why = r.unwrap_err();
    assert!(why.contains("firmware-layout rule"), "{why}");
    assert!(
        out.contains("BAD dtb: slots/bay0/bcm2712-rpi-5-b.dtb: no FDT magic"),
        "{out}"
    );
    assert!(!case.dir.join("boot-a1.img").exists());
}

#[test]
fn a_board_with_no_overlays_is_verified_unchanged_even_when_a_tool_is_given() {
    let dir = scratch("host-merge-none-image");
    let board = write_board(&dir, &board_text(&[], &[]));
    let payloads = payload(&dir, &board, "a1");
    let maps = firmware_dir(&dir, &[]);
    let (r, out) = boot_image(&dir, &board, "a1", &payloads, Some(&maps));
    let image = r.unwrap_or_else(|e| panic!("{e}\n{out}"));
    let (r, out) = run(&[
        "boot-partition",
        "--verify",
        &image,
        "--layout-of",
        &board,
        "--socs",
        &socs(),
        "--dtmerge",
        "/nonexistent/dtmerge",
    ]);
    r.unwrap_or_else(|e| panic!("verify: {e}\n{out}"));
    assert!(!out.contains("applies on host"), "{out}");
    assert!(!out.contains("BAD"), "{out}");
}

#[test]
fn dtmerge_without_verify_is_refused_and_expected_nodes_without_overlays_are_refused_by_name() {
    let dir = scratch("host-merge-refusals");
    let board = write_board(&dir, &board_with(&[], &[], EXPECT_OK));
    let (r, _) = run(&[
        "boot-partition",
        "--layout-of",
        &board,
        "--socs",
        &socs(),
        "--min-size",
        "--dtmerge",
        "x",
    ]);
    let why = r.unwrap_err();
    assert!(why.contains("--dtmerge goes with --verify"), "{why}");
    let (r, _) = run(&[
        "boot-partition",
        "--layout-of",
        &board,
        "--socs",
        &socs(),
        "--min-size",
    ]);
    let why = r.unwrap_err();
    assert!(
        why.contains("expect_nodes") && why.contains("declares none"),
        "{why}"
    );
}

/// The real dtmerge applies `__overrides__`: the parameter moves the node's cell from the overlay's
/// literal value to the line's value, which the stand-in cannot do. Skipped where LEMNOS_DTMERGE is
/// not set (locally); CI names the real tool and runs it.
#[test]
// The skip line is the test's own output (it is shown with `cargo test -- --nocapture`).
#[allow(clippy::print_stderr)]
fn the_real_dtmerge_applies_an_overrides_parameter_and_the_expected_node_holds() {
    let Some(tool) = real_tool() else {
        eprintln!(
            "skipped: LEMNOS_DTMERGE is not set, so the real dtmerge __overrides__ check did not run (CI runs it)"
        );
        return;
    };
    let expect = "expect_nodes = [{ path = \"/reserved-memory/test-ram\", properties = { record-size = \"$ram_size\" } }]\n";
    let case = host_case(
        "host-merge-real-override",
        &["dtoverlay=test-ram,ram_size=8192"],
        &["test-ram"],
        expect,
        &[("test-ram", RAM_DTS)],
    );
    let image = host_image(&case, "a1").unwrap_or_else(|e| panic!("{e}"));
    let (r, out) = verify_host(&case, &image, &tool);
    r.unwrap_or_else(|e| panic!("verify with the real dtmerge: {e}\n{out}"));
    assert!(
        out.contains("/reserved-memory/test-ram record-size is 8192 (0x2000) after the host merge"),
        "{out}"
    );
    assert!(!out.contains("BAD"), "{out}");
}
