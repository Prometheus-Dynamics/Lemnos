//! `tree --boot-dir`: the firmware files for a board whose record is `tryboot`, golden text, and
//! the check that the fixed text and the slots' own reading of it are one constant.

use super::boot_dir::{HEADER, OVERLAY_README_TEXT};
use super::boot_partition::board_parts;
use lemnos_host::board::{self, BoardFile};
use lemnos_slot_store::bootimage::Family;
use lemnos_slot_store::firmware::{self, CONFIG_TXT, Pick, TRYBOOT_TXT};

fn reference() -> BoardFile {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testing/boards/reference.toml");
    board::parse(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// A root file of the parts `boot-partition` builds, as text.
fn root_text(parts: &lemnos_slot_store::bootimage::Parts, name: &str) -> String {
    let bytes = &parts.root.iter().find(|(file, _)| *file == name).unwrap().1;
    String::from_utf8(bytes.clone()).unwrap()
}

/// The config files are the header, the fixed text, then the board's lines.
#[test]
fn the_config_files_are_the_fixed_text_then_the_board_lines() {
    let parts = board_parts(&reference(), "gpio=5=op,dh\n").unwrap();
    assert_eq!(parts.family, Family::Tryboot);
    assert_eq!(
        root_text(&parts, "config.txt"),
        format!("{HEADER}{CONFIG_TXT}gpio=5=op,dh\n")
    );
    assert_eq!(
        root_text(&parts, "tryboot.txt"),
        format!("{HEADER}{TRYBOOT_TXT}gpio=5=op,dh\n")
    );
    assert_eq!(parts.fixed.len(), 1);
    assert_eq!(parts.fixed[0].bytes, OVERLAY_README_TEXT.as_bytes());
}

/// Without board lines the files are the fixed text exactly.
#[test]
fn with_no_board_lines_the_files_are_the_fixed_text() {
    let parts = board_parts(&reference(), "").unwrap();
    assert_eq!(
        root_text(&parts, "config.txt"),
        format!("{HEADER}{CONFIG_TXT}")
    );
    assert_eq!(
        root_text(&parts, "tryboot.txt"),
        format!("{HEADER}{TRYBOOT_TXT}")
    );
}

/// The one constant: the `[boot_count&4]` filter the generated config.txt carries selects the
/// previous pointer for exactly the counts `pick` says, for every count the 8-bit register holds.
#[test]
fn the_fixed_text_and_the_slots_reading_of_it_are_one_constant() {
    let filter = CONFIG_TXT
        .lines()
        .find_map(|line| line.strip_prefix("[boot_count&"))
        .and_then(|rest| rest.strip_suffix(']'))
        .expect("the config.txt filter line");
    let mask: u8 = filter.parse().expect("a decimal mask");
    for count in 0..=255u8 {
        let firmware_picks = if count & mask != 0 {
            Pick::Previous
        } else {
            Pick::Default
        };
        assert_eq!(firmware::pick(count), firmware_picks, "count {count}");
    }
    // The file the slots write the pointers to is the one the fixed text includes.
    assert!(
        CONFIG_TXT.contains("include default.txt") && CONFIG_TXT.contains("include previous.txt")
    );
    assert!(TRYBOOT_TXT.contains("include trial.txt"));
}

/// The FAT directory name (eleven bytes, space padded, upper case) an 8.3 visible name stands for:
/// what the image builder writes into the directory, and what the locator matches.
fn directory_entry(visible: &str) -> [u8; 11] {
    let (base, ext) = visible.split_once('.').expect("an 8.3 name with a dot");
    let mut out = [b' '; 11];
    for (i, b) in base.bytes().enumerate() {
        out[i] = b.to_ascii_uppercase();
    }
    for (i, b) in ext.bytes().enumerate() {
        out[8 + i] = b.to_ascii_uppercase();
    }
    out
}

/// The contract's `[[record]]` table reads, and each visible name is the slot-store kind's.
#[test]
fn the_contract_record_table_names_the_visible_names() {
    let records = super::boot_dir::read_records(super::boot_dir::LAYOUT).unwrap();
    let names: Vec<&str> = records.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["trial.rec", "reason.rec", "episodes.rec"]);
    for record in &records {
        let kind = super::boot_dir::kind_of_role(&record.role).unwrap();
        assert_eq!(kind.visible_name(), Some(record.name.as_str()));
        assert_eq!(
            &directory_entry(&record.name),
            kind.file_name().unwrap(),
            "{}",
            record.name
        );
    }
}

/// A record other than `tryboot` or `uboot` has no boot partition here, and the refusal says so.
#[test]
fn a_board_with_another_record_is_refused_by_name() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testing/boards/reference.toml");
    let text = std::fs::read_to_string(path).unwrap();
    assert!(
        text.contains("record = \"tryboot\""),
        "the reference board names its record"
    );
    let board = board::parse(&text.replace("record = \"tryboot\"", "record = \"file\"")).unwrap();
    let err = board_parts(&board, "").unwrap_err();
    assert!(
        err.contains("\"file\"") && err.contains("\"uboot\""),
        "{err}"
    );
}

/// A board with no slots device has nothing to write.
#[test]
fn a_board_without_slots_has_no_boot_files() {
    let mut board = reference();
    board
        .devices
        .retain(|device| device.driver != "system-slots");
    let err = board_parts(&board, "").unwrap_err();
    assert!(err.contains("no system-slots device"), "{err}");
}

/// `tree --boot-dir` is removed (I5i8): the boot partition is `boot-partition`'s. The flag is refused
/// and nothing is written.
#[test]
fn tree_refuses_boot_dir_and_writes_nothing() {
    let dir = std::env::temp_dir().join(format!("lemnos-ctl-boot-dir-gone-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let args: Vec<String> = [
        "tree",
        "--socs",
        repo.join("socs").to_str().unwrap(),
        repo.join("testing/boards/reference.toml").to_str().unwrap(),
        "--boot-dir",
        dir.to_str().unwrap(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let mut out = Vec::new();
    assert!(crate::run(&args, &mut out).is_err());
    assert!(!dir.exists());
}

/// The reader takes version 5 only, and refuses 4 and 6 by version.
#[test]
fn the_layout_reader_reads_version_5_and_refuses_4_and_6() {
    use super::boot_dir::{LAYOUT, read_layout};
    assert!(LAYOUT.contains("\nversion = 5\n"));
    assert!(read_layout(LAYOUT).is_ok());
    for v in ["version = 4", "version = 6"] {
        let bad = LAYOUT.replacen("version = 5", v, 1);
        let why = read_layout(&bad).unwrap_err();
        assert!(why.contains("reads 5"), "{v}: {why}");
    }
}

/// The contract this tool embeds is the one the code writes (checked before every image build).
#[test]
fn the_embedded_contract_is_the_one_the_code_writes() {
    super::boot_dir::check_contract().unwrap();
}

/// A `[bay]` or `[volume]` value that disagrees with the code is refused by name.
#[test]
fn a_bay_table_that_disagrees_with_the_code_is_refused_by_name() {
    use super::boot_dir::{LAYOUT, check_bay_and_volume};
    let bad = LAYOUT.replacen("sets = 3", "sets = 4", 1);
    let err = check_bay_and_volume(&bad).unwrap_err();
    assert!(err.contains("[bay] sets"), "{err}");
}

/// The layout contract reads, names every file the firmware text names, and is the one list the
/// slot-store constants are checked against.
mod layout {
    use super::super::boot_dir::{LAYOUT, LayoutFile, read_layout};
    use lemnos_slot_store::layout as names;

    fn required_for(files: &[LayoutFile], firmware: &str) -> Vec<String> {
        files
            .iter()
            .filter(|f| f.required && f.firmware.iter().any(|k| k == firmware))
            .map(|f| f.name.clone())
            .collect()
    }

    #[test]
    fn the_contract_reads_and_its_rpi_list_is_the_slot_store_constants() {
        let files = read_layout(LAYOUT).unwrap();
        assert_eq!(files.len(), 8);
        assert_eq!(
            required_for(&files, "rpi-tryboot"),
            names::RPI_TRYBOOT_REQUIRED
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_version_other_than_five_is_refused() {
        for other in [
            "version = 1",
            "version = 2",
            "version = 3",
            "version = 4",
            "version = 6",
        ] {
            let text = LAYOUT.replace("version = 5", other);
            let why = read_layout(&text).unwrap_err();
            assert!(why.contains("reads 5"), "{why}");
        }
    }

    #[test]
    fn a_source_other_than_build_or_generated_is_refused() {
        let text = LAYOUT.replacen(
            "scope = \"set\"\nsource = \"build\"",
            "scope = \"set\"\nsource = \"made\"",
            1,
        );
        let why = read_layout(&text).unwrap_err();
        assert!(why.contains("\"made\""), "{why}");
    }

    #[test]
    fn the_generated_file_is_the_readme_and_the_others_are_build() {
        let files = read_layout(LAYOUT).unwrap();
        let generated: Vec<&str> = files
            .iter()
            .filter(|f| f.source == "generated")
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(
            generated,
            [
                names::OVERLAY_README,
                "extlinux/extlinux.conf",
                "uboot-env.txt"
            ]
        );
        assert!(
            files
                .iter()
                .all(|f| f.source == "build" || f.source == "generated")
        );
    }

    #[test]
    fn an_unknown_key_is_refused_at_both_levels() {
        let top = format!("extra = 1\n{LAYOUT}");
        assert!(
            read_layout(&top)
                .unwrap_err()
                .contains("unknown key \"extra\"")
        );
        let entry = LAYOUT.replacen("required = true", "required = true\nextra = 1", 1);
        assert!(
            read_layout(&entry)
                .unwrap_err()
                .contains("unknown key \"extra\"")
        );
    }

    #[test]
    fn an_entry_without_required_is_refused() {
        let text = LAYOUT.replacen("required = true\n", "", 1);
        assert!(read_layout(&text).unwrap_err().contains("required"));
    }

    #[test]
    fn the_image_carries_the_contract() {
        let parts = super::board_parts(&super::reference(), "").unwrap();
        let copy = parts
            .root
            .iter()
            .find(|(n, _)| *n == "slot-layout.toml")
            .unwrap();
        assert_eq!(copy.1, LAYOUT.as_bytes());
    }
}

/// `--manifest-boot`: the `boot` table the generator is the authority for, written by the same run
/// that writes the overlay. Its `board` and `sha256` are the overlay's marker, byte for byte.
mod manifest_boot {
    use super::super::boot_dir::manifest_boot;
    use lemnos_slot_store::manifest::{BLOCK, Manifest, tree_bytes};

    const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// Runs `tree` for the reference board, writing the overlay and the boot table into a directory
    /// of its own. Returns `(overlay text, boot table text)`.
    fn tree_outputs(name: &str) -> (String, String) {
        let dir = std::env::temp_dir().join(format!(
            "lemnos-ctl-manifest-boot-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let overlay = dir.join("overlay.dts");
        let boot = dir.join("boot.json");
        let args: Vec<String> = [
            "tree",
            "--socs",
            repo.join("socs").to_str().unwrap(),
            repo.join("testing/boards/reference.toml").to_str().unwrap(),
            "-o",
            overlay.to_str().unwrap(),
            "--manifest-boot",
            boot.to_str().unwrap(),
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        crate::run(&args, &mut Vec::new()).unwrap();
        let outputs = (
            std::fs::read_to_string(&overlay).unwrap(),
            std::fs::read_to_string(&boot).unwrap(),
        );
        std::fs::remove_dir_all(&dir).unwrap();
        outputs
    }

    /// The value of the marker property `key` in the overlay source: the text between the quotes
    /// of `key = "VALUE";`.
    fn marker_value(overlay: &str, key: &str) -> String {
        let line = overlay
            .lines()
            .find(|l| l.trim_start().starts_with(&format!("{key} = \"")))
            .unwrap_or_else(|| panic!("the overlay has no {key} marker"));
        let start = line.find('"').unwrap() + 1;
        let end = line.rfind('"').unwrap();
        line[start..end].to_string()
    }

    /// The table is the overlay's marker, in the manifest's encoding, newline-terminated, with the
    /// keys in this order.
    #[test]
    fn the_boot_table_is_the_overlay_marker_byte_for_byte() {
        let (overlay, boot) = tree_outputs("marker");
        let board = marker_value(&overlay, "lemnos,board");
        let digest = marker_value(&overlay, "lemnos,overlay-sha256");
        assert_eq!(board, "reference");
        assert_eq!(
            boot,
            format!("{{\"overlay\":{{\"board\":\"{board}\",\"sha256\":\"{digest}\"}}}}\n")
        );
    }

    /// The table is the same bytes on every run from the same board.
    #[test]
    fn the_boot_table_is_byte_stable_across_runs() {
        let (_, first) = tree_outputs("stable-a");
        let (_, second) = tree_outputs("stable-b");
        assert_eq!(first, second);
    }

    /// The digest is the marker's, and a board's table names the board the marker names.
    #[test]
    fn a_board_without_a_marker_gets_an_empty_boot_object() {
        assert_eq!(manifest_boot("reference", None).unwrap(), "{}\n");
    }

    /// A value that would not be a valid manifest field is refused by name and writes nothing.
    #[test]
    fn a_malformed_id_or_digest_is_refused() {
        assert!(manifest_boot("", Some(HASH)).is_err());
        assert!(manifest_boot("a\"b", Some(HASH)).is_err());
        assert!(manifest_boot("reference", Some(&HASH.to_uppercase())).is_err());
        assert!(manifest_boot("reference", Some("abc")).is_err());
    }

    /// The manifest reader takes the table as its `boot` object (with the build's `files`), and reads
    /// the board and the digest back as the live marker has them.
    #[test]
    fn the_manifest_reader_takes_the_table_as_its_boot_object() {
        let (overlay, boot) = tree_outputs("reader");
        let board = marker_value(&overlay, "lemnos,board");
        let digest = marker_value(&overlay, "lemnos,overlay-sha256");
        let data_blocks = 3u64;
        let offset = data_blocks * BLOCK;
        // The table without its closing brace, then the build's `files`.
        let table = boot.trim_end().strip_suffix('}').unwrap();
        let text = format!(
            "{{\"schema\":3,\"id\":\"os-2\",\"product\":\"an-os\",\"version\":\"2.0\",\"board\":\"{board}\",\
             \"image\":{{\"size\":{},\"sha256\":\"{HASH}\"}},\
             \"verity\":{{\"root_hash\":\"{HASH}\",\"salt\":\"{HASH}\",\"data_blocks\":{data_blocks},\"hash_offset\":{offset}}},\
             \"boot\":{table},\"files\":{{\"cmdline.txt\":\"{HASH}\"}}}}}}",
            offset + tree_bytes(data_blocks)
        );
        let m = Manifest::parse(&text).unwrap_or_else(|e| panic!("{e}"));
        let overlay = m.overlay.expect("the reader lists the board overlay");
        assert_eq!(overlay.board, board);
        assert_eq!(overlay.sha256, digest);
    }
}

/// `--cmdline-slot`: the slot's command-line words, one line, from the board's `boot_args`.
mod cmdline_slot {
    use super::super::boot_dir::cmdline_slot;
    use lemnos_host::board;

    /// A board with no system-slots device: nothing to name on a command line.
    const NO_SLOTS: &str = "format = \"lemnos.board\"\nschema_version = 1\n\n[board]\nid = \"no-slots\"\n\n[[devices]]\nid = \"gadget\"\ndriver = \"usb-gadget\"\nuses = { function = \"udc:fc000000.usb\" }\n";

    #[test]
    fn the_reference_board_names_its_slot_in_one_line() {
        let text = std::fs::read_to_string(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../testing/boards/reference.toml"),
        )
        .unwrap();
        let board = board::parse(&text).unwrap();
        assert_eq!(
            cmdline_slot(&board, "os-1").unwrap(),
            "os.slot=os-1 os.roothash=@roothash@ os.src.rpi-boot-mode-1=/soc@107c000000/mmc@fff000 panic=5\n"
        );
    }

    /// The reference board's `system-slots` text with its `boot_sources` list replaced.
    fn reference_with(sources: &str) -> lemnos_host::board::BoardFile {
        let text = std::fs::read_to_string(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../testing/boards/reference.toml"),
        )
        .unwrap();
        let old = "boot_sources = [\"rpi-boot-mode-1=/soc@107c000000/mmc@fff000\"]";
        assert!(text.contains(old));
        board::parse(&text.replace(old, sources)).unwrap()
    }

    #[test]
    fn one_src_word_per_entry_in_the_tables_order_and_no_disk_word() {
        let board = reference_with(
            "boot_sources = [\"/soc/mmc@7e202000=/soc/mmc@7e202000\", \"rpi-boot-mode-1=/soc@107c000000/mmc@fff000\"]",
        );
        let line = cmdline_slot(&board, "os-1").unwrap();
        assert_eq!(
            line,
            "os.slot=os-1 os.roothash=@roothash@ os.src./soc/mmc@7e202000=/soc/mmc@7e202000 os.src.rpi-boot-mode-1=/soc@107c000000/mmc@fff000 panic=5\n"
        );
        assert!(!line.contains(".disk="));
        assert_eq!(line.matches('\n').count(), 1);
    }

    #[test]
    fn a_malformed_or_missing_table_is_refused_by_name() {
        let error = cmdline_slot(&reference_with("boot_sources = [\"nope\"]"), "os-1").unwrap_err();
        assert!(error.contains("is not source=path"), "{error}");
        let error = cmdline_slot(
            &reference_with("boot_sources = [\"rpi-boot-mode-1=/a\", \"rpi-boot-mode-1=/b\"]"),
            "os-1",
        )
        .unwrap_err();
        assert!(error.contains("twice"), "{error}");
        let error = cmdline_slot(&reference_with("demote_after_episodes = 2"), "os-1").unwrap_err();
        assert!(error.contains("boot_sources is not set"), "{error}");
    }

    #[test]
    fn a_board_with_no_slot_device_is_refused_by_name() {
        let board = board::parse(NO_SLOTS).unwrap();
        let error = cmdline_slot(&board, "os-1").unwrap_err();
        assert!(error.contains("no system-slots device"), "{error}");
    }

    #[test]
    fn an_empty_or_spaced_slot_id_is_refused() {
        let text = std::fs::read_to_string(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../testing/boards/reference.toml"),
        )
        .unwrap();
        let board = board::parse(&text).unwrap();
        assert!(cmdline_slot(&board, "").is_err());
        assert!(cmdline_slot(&board, "os 1").is_err());
    }

    #[test]
    fn without_slot_the_tree_run_refuses_the_flag_and_writes_nothing() {
        let dir =
            std::env::temp_dir().join(format!("lemnos-ctl-cmdline-slot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let out = dir.join("cmdline-slot.txt");
        let args: Vec<String> = [
            "tree",
            "--socs",
            repo.join("socs").to_str().unwrap(),
            repo.join("testing/boards/reference.toml").to_str().unwrap(),
            "--cmdline-slot",
            out.to_str().unwrap(),
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let error = crate::run(&args, &mut Vec::new()).unwrap_err();
        assert!(error.contains("--cmdline-slot needs --slot"), "{error}");
        assert!(!out.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

const UBOOT_KEYS: &str = "record = \"uboot\", uboot_env_size = 8192, uboot_env_mmc_index = 0";

fn uboot_board_with(keys: &str) -> BoardFile {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testing/boards/reference.toml");
    let text = std::fs::read_to_string(path).unwrap();
    board::parse(&text.replace("record = \"tryboot\"", keys)).unwrap()
}

fn uboot_board() -> BoardFile {
    uboot_board_with(UBOOT_KEYS)
}

/// The `uboot` record's root holds the contract copy, the locator slot-init reads and the boot
/// script; the locator parses with the slot-store reader to the board file's place (the GPT form:
/// only the size), and the script has the board's USB controller and LED set before the static text.
#[test]
fn the_uboot_board_writes_the_locator_and_the_boot_script() {
    use lemnos_slot_store::uboot_env::Place;
    let parts = board_parts(&uboot_board(), "").unwrap();
    assert_eq!(parts.family, Family::UbootBlock);
    let names: Vec<&str> = parts.root.iter().map(|(n, _)| *n).collect();
    assert_eq!(names, ["slot-layout.toml", "uboot-env.txt", "boot.cmd"]);
    let locator = root_text(&parts, lemnos_slot_store::uboot_env::LOCATOR);
    assert_eq!(
        Place::parse(&locator).unwrap(),
        Place::Gpt { size: 8192 },
        "{locator}"
    );
    let script = root_text(&parts, "boot.cmd");
    assert!(
        script.ends_with(super::boot_dir::BOOT_CMD),
        "the static script follows"
    );
    assert!(script.contains("\nsetenv lemnos_usb 0\n"));
    assert!(
        !script.contains("setenv lemnos_led"),
        "no LED named, none set"
    );
    let led = uboot_board_with(&format!(
        "{UBOOT_KEYS}, uboot_fastboot_usb = 2, uboot_led = \"red:status\""
    ));
    let script = root_text(&board_parts(&led, "").unwrap(), "boot.cmd");
    assert!(
        script.contains("\nsetenv lemnos_usb 2\nsetenv lemnos_led red:status\n"),
        "{script}"
    );
    // The fixed-offset form writes its device and offsets.
    let device = uboot_board_with(
        "record = \"uboot\", uboot_env_size = 8192, uboot_env_device = \"/dev/mmcblk0\", uboot_env_offset = 4194304, uboot_env_offset_redund = 4202496",
    );
    let parts = board_parts(&device, "").unwrap();
    let locator = root_text(&parts, lemnos_slot_store::uboot_env::LOCATOR);
    assert_eq!(
        Place::parse(&locator).unwrap(),
        Place::device("/dev/mmcblk0", 8192, 4194304, 4202496).unwrap()
    );
}

/// The boot script reads the three variables the slot driver writes.
#[test]
fn the_boot_script_reads_the_variables_the_record_writes() {
    for key in lemnos_slot_store::records::uboot::KEYS {
        assert!(
            super::boot_dir::BOOT_CMD.contains(key),
            "boot.cmd reads {key}"
        );
    }
}

/// Every refusal is by name, and nothing is written.
#[test]
fn a_uboot_board_with_a_wrong_environment_description_is_refused_by_name() {
    let refuse = |keys: &str| board_parts(&uboot_board_with(keys), "").unwrap_err();
    assert_eq!(
        refuse("record = \"uboot\""),
        "system-slots device \"slots\": uboot_env_size is needed for the \"uboot\" record (an integer)"
    );
    let err = refuse(
        "record = \"uboot\", uboot_env_size = 8192, uboot_env_partuuid = \"0a1b2c3d-0000-4000-8000-000000000042\"",
    );
    assert!(
        err.contains("uboot_env_partuuid is not a key")
            && err.contains("3de21764-95bd-54bd-a5c3-4abe786f38a8"),
        "{err}"
    );
    let err = refuse("record = \"uboot\", uboot_env_size = 8192, uboot_env_offset = 0");
    assert!(
        err.contains("uboot_env_offset needs uboot_env_device"),
        "{err}"
    );
    let err =
        refuse("record = \"uboot\", uboot_env_size = 8192, uboot_env_device = \"/dev/mmcblk0\"");
    assert!(err.contains("uboot_env_offset"), "{err}");
    let err = refuse(
        "record = \"uboot\", uboot_env_size = 8192, uboot_env_device = \"/dev/mtd2\", uboot_env_offset = 0, uboot_env_offset_redund = 65536",
    );
    assert!(err.contains("flash or a UBI volume"), "{err}");
    let err = refuse("record = \"uboot\", uboot_env_size = 8192, uboot_led = \"a b\"");
    assert!(err.contains("not a U-Boot LED label"), "{err}");
}

/// The contract names the uboot kind's file as the slot-store locator, and the rpi list is
/// unchanged.
#[test]
fn the_contract_names_the_locator_for_the_uboot_kind() {
    let layout = super::boot_dir::read_layout(super::boot_dir::LAYOUT).unwrap();
    let uboot: Vec<&str> = layout
        .iter()
        .filter(|f| f.firmware.iter().any(|k| k == "uboot"))
        .map(|f| f.name.as_str())
        .collect();
    assert_eq!(
        uboot,
        [
            "extlinux/extlinux.conf",
            lemnos_slot_store::uboot_env::LOCATOR
        ]
    );
}

/// `--uboot-config`: the Kconfig fragment. The GPT form names the type-GUID lookup and no offsets;
/// the device form names the offsets and no GUID lookup; every board gets the environment, the boot
/// script's commands, the fastboot gadget and the watchdog. Every name is U-Boot v2026.10's: they
/// were checked against its Kconfig files (see testing/board-runs/I5i7.md).
#[test]
fn the_uboot_config_fragment_names_what_the_boot_path_needs() {
    let config = |keys: &str| super::boot_dir::uboot_config(&uboot_board_with(keys)).unwrap();
    let gpt = config(UBOOT_KEYS);
    let lines: Vec<&str> = gpt.lines().filter(|l| l.starts_with("CONFIG_")).collect();
    assert_eq!(
        lines,
        [
            "CONFIG_ENV_IS_IN_MMC=y",
            "CONFIG_ENV_MMC_DEVICE_INDEX=0",
            "CONFIG_ENV_REDUNDANT=y",
            "CONFIG_ENV_SIZE=0x2000",
            "CONFIG_CMD_SAVEENV=y",
            "CONFIG_EFI_PARTITION=y",
            "CONFIG_PARTITION_TYPE_GUID=y",
            "CONFIG_OF_CONTROL=y",
            "CONFIG_HUSH_PARSER=y",
            "CONFIG_CMD_FDT=y",
            "CONFIG_CMD_SLEEP=y",
            "CONFIG_CMD_SYSBOOT=y",
            "CONFIG_USB_GADGET=y",
            "CONFIG_USB_FUNCTION_FASTBOOT=y",
            "CONFIG_CMD_FASTBOOT=y",
            "CONFIG_FASTBOOT_USB_DEV=0",
            "CONFIG_FASTBOOT_FLASH=y",
            "CONFIG_FASTBOOT_FLASH_MMC=y",
            "CONFIG_FASTBOOT_FLASH_MMC_DEV=0",
            "CONFIG_WDT=y",
            "CONFIG_WATCHDOG=y",
            "CONFIG_WATCHDOG_AUTOSTART=y",
        ]
    );
    assert!(!gpt.contains("CONFIG_ENV_OFFSET"));
    assert!(!gpt.contains("CONFIG_LED"), "no LED named");
    assert!(!gpt.contains("VENDOR_NUM"), "no VID and PID given");
    let full = config(
        "record = \"uboot\", uboot_env_size = 16384, uboot_env_mmc_index = 1, uboot_fastboot_usb = 1, uboot_led = \"act\", uboot_usb_vendor = 4617, uboot_usb_product = 1, uboot_watchdog_ms = 20000",
    );
    for want in [
        "CONFIG_ENV_MMC_DEVICE_INDEX=1",
        "CONFIG_ENV_SIZE=0x4000",
        "CONFIG_FASTBOOT_USB_DEV=1",
        "CONFIG_FASTBOOT_FLASH_MMC_DEV=1",
        "CONFIG_USB_GADGET_VENDOR_NUM=0x1209",
        "CONFIG_USB_GADGET_PRODUCT_NUM=0x0001",
        "CONFIG_WATCHDOG_TIMEOUT_MSECS=20000",
        "CONFIG_LED=y",
        "CONFIG_CMD_LED=y",
    ] {
        assert!(full.lines().any(|l| l == want), "{want}\n{full}");
    }
    let device = config(
        "record = \"uboot\", uboot_env_size = 8192, uboot_env_device = \"/dev/mmcblk0\", uboot_env_offset = 4194304, uboot_env_offset_redund = 4202496",
    );
    assert!(
        device.lines().any(|l| l == "CONFIG_ENV_OFFSET=0x400000"),
        "{device}"
    );
    assert!(
        device
            .lines()
            .any(|l| l == "CONFIG_ENV_OFFSET_REDUND=0x402000"),
        "{device}"
    );
    assert!(!device.contains("PARTITION_TYPE_GUID"));
    // The option names in the script's own comments are the ones in the fragment.
    for needed in ["saveenv", "sysboot", "fdt", "sleep", "fastboot"] {
        assert!(super::boot_dir::BOOT_CMD.contains(needed), "{needed}");
    }
    // A board of another record, a half-given VID and PID, and a VID out of range are refused.
    let other = board::parse(
        &std::fs::read_to_string(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../testing/boards/reference.toml"),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(
        super::boot_dir::uboot_config(&other)
            .unwrap_err()
            .contains("is for \"uboot\"")
    );
    let half =
        uboot_board_with("record = \"uboot\", uboot_env_size = 8192, uboot_usb_vendor = 4617");
    assert!(
        super::boot_dir::uboot_config(&half)
            .unwrap_err()
            .contains("together or not at all")
    );
    let big = uboot_board_with(
        "record = \"uboot\", uboot_env_size = 8192, uboot_usb_vendor = 70000, uboot_usb_product = 1",
    );
    assert!(
        super::boot_dir::uboot_config(&big)
            .unwrap_err()
            .contains("0 to 65535")
    );
}

/// The command writes the fragment to the named file.
#[test]
fn tree_writes_the_uboot_config_to_its_file() {
    let repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = std::env::temp_dir().join(format!("lemnos-uboot-config-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let text = std::fs::read_to_string(repo.join("testing/boards/reference.toml")).unwrap();
    let board = dir.join("board.toml");
    std::fs::write(&board, text.replace("record = \"tryboot\"", UBOOT_KEYS)).unwrap();
    let out = dir.join("uboot.config");
    let args: Vec<String> = [
        "tree",
        "--socs",
        repo.join("socs").to_str().unwrap(),
        board.to_str().unwrap(),
        "--uboot-config",
        out.to_str().unwrap(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    crate::run(&args, &mut Vec::new()).unwrap();
    let fragment = std::fs::read_to_string(&out).unwrap();
    assert!(fragment.contains("CONFIG_ENV_REDUNDANT=y\n"), "{fragment}");
    // The tryboot reference board has no U-Boot configuration.
    let args: Vec<String> = [
        "tree",
        "--socs",
        repo.join("socs").to_str().unwrap(),
        repo.join("testing/boards/reference.toml").to_str().unwrap(),
        "--uboot-config",
        dir.join("none").to_str().unwrap(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let error = crate::run(&args, &mut Vec::new()).unwrap_err();
    assert!(
        error.contains("--uboot-config") && error.contains("is for \"uboot\""),
        "{error}"
    );
    assert!(!dir.join("none").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `tree --boot-dir` is refused by name, with the command that replaces it, whatever else is given;
/// the flags that remain are not refused.
#[test]
fn tree_boot_dir_is_refused_and_names_boot_partition() {
    let repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let board = repo.join("testing/boards/reference.toml");
    let args: Vec<String> = [
        "tree",
        "--socs",
        repo.join("socs").to_str().unwrap(),
        board.to_str().unwrap(),
        "--boot-dir",
        "/nonexistent/dir",
        "--slot",
        "a",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let error = crate::run(&args, &mut Vec::new()).unwrap_err();
    assert_eq!(error, crate::BOOT_DIR_REMOVED);
    assert!(error.contains("boot-partition") && error.contains("--manifest-boot"));
    assert!(!std::path::Path::new("/nonexistent/dir").exists());
}
