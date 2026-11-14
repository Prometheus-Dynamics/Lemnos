//! `lemnos-ctl boot-partition` against the repository's own board files: every board with a slot
//! device builds a real-size image, verifies it, and passes the run-time locator. Image files are
//! written under the test target directory only; nothing here touches a block device.

use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn socs() -> String {
    repo().join("socs").display().to_string()
}

/// The board files of `testing/boards` that name a `system-slots` device.
fn slot_boards() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(repo().join("testing/boards"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .filter(|p| {
            let text = std::fs::read_to_string(p).unwrap();
            lemnos_host::board::parse(&text)
                .map(|b| b.devices.iter().any(|d| d.driver == "system-slots"))
                .unwrap_or(false)
        })
        .collect();
    out.sort();
    out
}

fn run(args: &[&str]) -> (Result<(), String>, String) {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let mut out = Vec::new();
    let r = lemnos_ctl::run(&args, &mut out);
    (r, String::from_utf8(out).unwrap())
}

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The five payloads of a board: each the size of its bay, the kernel gzip-shaped (the Pi form takes
/// a gzip kernel, requirements section 8.7).
fn payloads(dir: &Path, board: &lemnos_host::board::BoardFile) -> PathBuf {
    let b = &board.boot;
    let sizes = [
        b.bay_kernel_bytes.unwrap(),
        b.bay_initramfs_bytes.unwrap(),
        b.bay_device_tree_bytes.unwrap(),
        b.bay_cmdline_bytes.unwrap(),
        b.bay_overlay_bytes.unwrap(),
    ];
    let roles = [
        "kernel",
        "initramfs",
        "device-tree",
        "cmdline",
        "board-overlay",
    ];
    let payload = dir.join("payload");
    std::fs::create_dir_all(&payload).unwrap();
    for (i, role) in roles.iter().enumerate() {
        let mut bytes = vec![(i as u8).wrapping_add(1); sizes[i] as usize];
        if i == 0 {
            bytes[0] = 0x1F;
            bytes[1] = 0x8B;
        }
        if i == 3 {
            bytes = b"console=serial0,115200 root=lemnos\n".to_vec();
        }
        std::fs::write(payload.join(role), bytes).unwrap();
    }
    payload
}

fn board_of(path: &Path) -> lemnos_host::board::BoardFile {
    lemnos_host::board::parse(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// The minimum for a board, through the tool.
fn minimum(board: &Path) -> u64 {
    let (r, out) = run(&[
        "boot-partition",
        "--layout-of",
        board.to_str().unwrap(),
        "--socs",
        &socs(),
        "--min-size",
    ]);
    r.unwrap();
    out.trim().parse().unwrap()
}

#[test]
fn every_board_with_a_slot_device_builds_a_real_size_image_that_verifies_and_locates() {
    let boards = slot_boards();
    assert!(
        !boards.is_empty(),
        "the repository has a board with a slot device"
    );
    for board_path in boards {
        let name = board_path
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let board = board_of(&board_path);
        let min = minimum(&board_path);
        // A real size: the minimum, rounded up to a whole MiB, as the image builder would be given.
        let size = min.div_ceil(1024 * 1024) * 1024 * 1024;
        let dir = scratch(&format!("gate-{name}"));
        let payload = payloads(&dir, &board);
        let image = dir.join("boot.img");
        let (r, out) = run(&[
            "boot-partition",
            "--layout-of",
            board_path.to_str().unwrap(),
            "--socs",
            &socs(),
            "--slot",
            "a",
            "--payload",
            payload.to_str().unwrap(),
            "--size",
            &size.to_string(),
            "--out",
            image.to_str().unwrap(),
        ]);
        r.unwrap_or_else(|e| panic!("{name}: build at {size} bytes: {e}"));
        assert!(out.contains(&format!("{} bytes", size)), "{out}");
        let (r, report) = run(&[
            "boot-partition",
            "--layout-of",
            board_path.to_str().unwrap(),
            "--socs",
            &socs(),
            "--verify",
            image.to_str().unwrap(),
        ]);
        assert!(r.is_ok(), "{name}: verify failed:\n{report}");
        assert!(
            report.lines().all(|l| l.starts_with("ok ")),
            "{name}: {report}"
        );
        assert!(
            report.contains("locator:") && report.contains("bay0 (a): the five payloads match"),
            "{name}: {report}"
        );
        assert!(
            report.contains("default.txt: names bay0"),
            "{name}: {report}"
        );
    }
}

#[test]
fn the_image_of_a_board_is_byte_identical_for_the_same_inputs() {
    let board_path = repo().join("testing/boards/reference.toml");
    let board = board_of(&board_path);
    let dir = scratch("identical");
    let payload = payloads(&dir, &board);
    let size = minimum(&board_path).div_ceil(1024 * 1024) * 1024 * 1024;
    let mut images = Vec::new();
    for name in ["one.img", "two.img"] {
        let image = dir.join(name);
        let (r, _) = run(&[
            "boot-partition",
            "--layout-of",
            board_path.to_str().unwrap(),
            "--socs",
            &socs(),
            "--slot",
            "a",
            "--payload",
            payload.to_str().unwrap(),
            "--size",
            &size.to_string(),
            "--out",
            image.to_str().unwrap(),
        ]);
        r.unwrap();
        images.push(std::fs::read(&image).unwrap());
    }
    assert!(
        images[0] == images[1],
        "the same inputs give the same bytes"
    );
}

#[test]
fn the_firmware_text_carries_the_gpio_lines_of_the_board_file_and_nothing_else() {
    let board_path = repo().join("testing/boards/reference.toml");
    let board = board_of(&board_path);
    let dir = scratch("gpio");
    let payload = payloads(&dir, &board);
    let size = minimum(&board_path).div_ceil(1024 * 1024) * 1024 * 1024;
    let image = dir.join("boot.img");
    run(&[
        "boot-partition",
        "--layout-of",
        board_path.to_str().unwrap(),
        "--socs",
        &socs(),
        "--slot",
        "a",
        "--payload",
        payload.to_str().unwrap(),
        "--size",
        &size.to_string(),
        "--out",
        image.to_str().unwrap(),
    ])
    .0
    .unwrap();
    let bytes = std::fs::read(&image).unwrap();
    let mut mem = lemnos_slot_store::bootimage::Mem(bytes);
    let config = lemnos_slot_store::bootfat::read_file(&mut mem, "config.txt", 64 * 1024).unwrap();
    let text = String::from_utf8(config).unwrap();
    // reference.toml holds gpio:pinctrl-rp1:45 high from power-on: one gpio line in the fragment.
    let lines: Vec<&str> = text.lines().filter(|l| l.starts_with("gpio=")).collect();
    assert_eq!(lines.len(), 1, "{text}");
    assert!(lines[0].ends_with("=op,dh"), "{lines:?}");
}

#[test]
fn a_size_below_the_minimum_is_refused_with_the_minimum_printed() {
    let board_path = repo().join("testing/boards/reference.toml");
    let min = minimum(&board_path);
    let dir = scratch("below");
    let board = board_of(&board_path);
    let payload = payloads(&dir, &board);
    let image = dir.join("boot.img");
    let (r, _) = run(&[
        "boot-partition",
        "--layout-of",
        board_path.to_str().unwrap(),
        "--socs",
        &socs(),
        "--slot",
        "a",
        "--payload",
        payload.to_str().unwrap(),
        "--size",
        &(min - 512).to_string(),
        "--out",
        image.to_str().unwrap(),
    ]);
    let err = r.unwrap_err();
    assert!(
        err.contains("below the minimum") && err.contains(&min.to_string()),
        "{err}"
    );
    assert!(!image.exists(), "nothing is written");
}

#[test]
fn a_board_without_a_bay_size_is_refused_by_name() {
    let text = std::fs::read_to_string(repo().join("testing/boards/reference.toml")).unwrap();
    let dir = scratch("nobay");
    let cut = text.replace("bay_kernel_bytes = 11534336\n", "");
    let path = dir.join("board.toml");
    std::fs::write(&path, cut).unwrap();
    let (r, _) = run(&[
        "boot-partition",
        "--layout-of",
        path.to_str().unwrap(),
        "--socs",
        &socs(),
        "--min-size",
    ]);
    let err = r.unwrap_err();
    assert!(err.contains("[boot] bay_kernel_bytes is not set"), "{err}");
}

#[test]
fn a_payload_directory_with_a_wrong_or_missing_role_is_refused_by_name() {
    let board_path = repo().join("testing/boards/reference.toml");
    let board = board_of(&board_path);
    let dir = scratch("roles");
    let payload = payloads(&dir, &board);
    std::fs::write(payload.join("kernel8.img"), b"x").unwrap();
    let (r, _) = run(&[
        "boot-partition",
        "--layout-of",
        board_path.to_str().unwrap(),
        "--socs",
        &socs(),
        "--min-size",
        "--payload",
        payload.to_str().unwrap(),
    ]);
    let err = r.unwrap_err();
    assert!(err.contains("\"kernel8.img\" is not a build role"), "{err}");
    std::fs::remove_file(payload.join("kernel8.img")).unwrap();
    std::fs::remove_file(payload.join("cmdline")).unwrap();
    let (r, _) = run(&[
        "boot-partition",
        "--layout-of",
        board_path.to_str().unwrap(),
        "--socs",
        &socs(),
        "--min-size",
        "--payload",
        payload.to_str().unwrap(),
    ]);
    assert!(r.unwrap_err().contains("the cmdline payload is missing"));
}

#[test]
fn a_payload_bigger_than_its_bay_is_named_by_min_size_with_a_line_per_role() {
    let board_path = repo().join("testing/boards/reference.toml");
    let board = board_of(&board_path);
    let dir = scratch("bigbay");
    let payload = payloads(&dir, &board);
    std::fs::write(
        payload.join("initramfs"),
        vec![1u8; board.boot.bay_initramfs_bytes.unwrap() as usize + 1],
    )
    .unwrap();
    let (r, out) = run(&[
        "boot-partition",
        "--layout-of",
        board_path.to_str().unwrap(),
        "--socs",
        &socs(),
        "--min-size",
        "--payload",
        payload.to_str().unwrap(),
    ]);
    let err = r.unwrap_err();
    assert!(err.contains("initramfs"), "{err}");
    // The minimum, then a line per role.
    assert_eq!(out.lines().count(), 6, "{out}");
    assert!(out.lines().next().unwrap().parse::<u64>().is_ok(), "{out}");
    assert!(out.lines().any(|l| l.starts_with("kernel ")), "{out}");
}

/// `--min-size --payload DIR` prints the minimum first, on one line of digits (the line `--min-size`
/// alone prints), then `role stored_bytes bay_bytes` for the five roles in order.
#[test]
fn min_size_with_a_payload_prints_the_minimum_then_a_line_per_role() {
    let board_path = repo().join("testing/boards/reference.toml");
    let board = board_of(&board_path);
    let dir = scratch("minline");
    let payload = payloads(&dir, &board);
    let (r, out) = run(&[
        "boot-partition",
        "--layout-of",
        board_path.to_str().unwrap(),
        "--socs",
        &socs(),
        "--min-size",
        "--payload",
        payload.to_str().unwrap(),
    ]);
    r.unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 6, "{out}");
    assert_eq!(lines[0].parse::<u64>().unwrap(), minimum(&board_path));
    let roles: Vec<&str> = lines[1..]
        .iter()
        .map(|l| l.split(' ').next().unwrap())
        .collect();
    assert_eq!(
        roles,
        [
            "kernel",
            "initramfs",
            "device-tree",
            "cmdline",
            "board-overlay"
        ]
    );
    for line in &lines[1..] {
        let parts: Vec<&str> = line.split(' ').collect();
        assert_eq!(parts.len(), 3, "{line}");
        assert!(parts[1].parse::<u64>().is_ok() && parts[2].parse::<u64>().is_ok());
    }
}

/// A U-Boot board without `uboot_env_size` is refused naming the file, the device and the key.
#[test]
fn a_uboot_board_without_its_environment_size_names_the_file_the_device_and_the_key() {
    let text = std::fs::read_to_string(repo().join("testing/boards/reference.toml")).unwrap();
    let dir = scratch("noenv");
    let path = dir.join("board.toml");
    std::fs::write(
        &path,
        text.replace("record = \"tryboot\"", "record = \"uboot\""),
    )
    .unwrap();
    let (r, _) = run(&[
        "boot-partition",
        "--layout-of",
        path.to_str().unwrap(),
        "--socs",
        &socs(),
        "--min-size",
    ]);
    assert_eq!(
        r.unwrap_err(),
        format!(
            "{}: system-slots device \"slots\": uboot_env_size is needed for the \"uboot\" record (an integer)",
            path.display()
        )
    );
}

#[test]
fn a_flipped_byte_in_a_bay_is_named_by_verify_and_fails_it() {
    let board_path = repo().join("testing/boards/reference.toml");
    let board = board_of(&board_path);
    let dir = scratch("flip");
    let payload = payloads(&dir, &board);
    let size = minimum(&board_path).div_ceil(1024 * 1024) * 1024 * 1024;
    let image = dir.join("boot.img");
    run(&[
        "boot-partition",
        "--layout-of",
        board_path.to_str().unwrap(),
        "--socs",
        &socs(),
        "--slot",
        "a",
        "--payload",
        payload.to_str().unwrap(),
        "--size",
        &size.to_string(),
        "--out",
        image.to_str().unwrap(),
    ])
    .0
    .unwrap();
    let mut bytes = std::fs::read(&image).unwrap();
    let mut mem = lemnos_slot_store::bootimage::Mem(bytes.clone());
    let lba = lemnos_slot_store::bootfat::locate_path(
        &mut mem,
        "slots/bay0/kernel8.img",
        board.boot.bay_kernel_bytes.unwrap(),
    )
    .unwrap()
    .first_lba;
    bytes[lba as usize * 512 + 100] ^= 0x40;
    let damaged = dir.join("damaged.img");
    std::fs::write(&damaged, &bytes).unwrap();
    let (r, out) = run(&[
        "boot-partition",
        "--layout-of",
        board_path.to_str().unwrap(),
        "--socs",
        &socs(),
        "--verify",
        damaged.to_str().unwrap(),
    ]);
    assert!(r.is_err(), "{out}");
    assert!(
        out.lines()
            .any(|l| l.starts_with("BAD") && l.contains("kernel")),
        "{out}"
    );
}

#[test]
fn an_image_is_never_written_over_an_existing_file() {
    let board_path = repo().join("testing/boards/reference.toml");
    let board = board_of(&board_path);
    let dir = scratch("exists");
    let payload = payloads(&dir, &board);
    let image = dir.join("boot.img");
    std::fs::write(&image, b"someone else's file").unwrap();
    let size = minimum(&board_path).div_ceil(1024 * 1024) * 1024 * 1024;
    let (r, _) = run(&[
        "boot-partition",
        "--layout-of",
        board_path.to_str().unwrap(),
        "--socs",
        &socs(),
        "--slot",
        "a",
        "--payload",
        payload.to_str().unwrap(),
        "--size",
        &size.to_string(),
        "--out",
        image.to_str().unwrap(),
    ]);
    assert!(r.unwrap_err().contains("exists"));
    assert_eq!(std::fs::read(&image).unwrap(), b"someone else's file");
}

/// A `record = "uboot"` board builds an image that carries the environment locator slot-init finds
/// the environment through (`uboot-env.txt` at the root), and `--verify` holds it to its bytes.
#[test]
fn a_uboot_board_builds_an_image_with_its_environment_locator() {
    let dir = scratch("uboot-locator");
    let text = std::fs::read_to_string(repo().join("testing/boards/reference.toml")).unwrap();
    let keys = "record = \"uboot\", uboot_env_size = 8192";
    let board_path = dir.join("uboot.toml");
    std::fs::write(&board_path, text.replace("record = \"tryboot\"", keys)).unwrap();
    let board = board_of(&board_path);
    let payload = payloads(&dir, &board);
    // The U-Boot form refuses a compressed kernel: an uncompressed one.
    let kernel = payload.join("kernel");
    let mut bytes = std::fs::read(&kernel).unwrap();
    bytes[0] = 0;
    bytes[1] = 0;
    std::fs::write(&kernel, bytes).unwrap();
    let size = minimum(&board_path).div_ceil(1024 * 1024) * 1024 * 1024;
    let image = dir.join("boot.img");
    let socs = socs();
    let (r, _) = run(&[
        "boot-partition",
        "--layout-of",
        board_path.to_str().unwrap(),
        "--socs",
        &socs,
        "--slot",
        "a",
        "--payload",
        payload.to_str().unwrap(),
        "--size",
        &size.to_string(),
        "--out",
        image.to_str().unwrap(),
    ]);
    r.unwrap();
    let mut file = std::fs::File::open(&image).unwrap();
    let locator = lemnos_slot_store::bootcheck::fat_read(&mut file, "uboot-env.txt")
        .unwrap()
        .expect("uboot-env.txt is in the image");
    let parsed =
        lemnos_slot_store::uboot_env::Place::parse(std::str::from_utf8(&locator).unwrap()).unwrap();
    assert_eq!(
        parsed,
        lemnos_slot_store::uboot_env::Place::Gpt { size: 8192 }
    );
    let (r, report) = run(&[
        "boot-partition",
        "--layout-of",
        board_path.to_str().unwrap(),
        "--socs",
        &socs,
        "--verify",
        image.to_str().unwrap(),
    ]);
    assert!(r.is_ok(), "{report}");
    assert!(
        report.contains("uboot-env.txt: holds its generated bytes"),
        "{report}"
    );
    assert!(report.lines().all(|l| l.starts_with("ok ")), "{report}");
}
