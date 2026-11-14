//! `tree --boot-dir`: the firmware files for a board whose record is `tryboot`, golden text, and
//! the check that the fixed text and the slots' own reading of it are one constant.

use super::boot_dir::{HEADER, PLACEHOLDER, files};
use lemnos_host::board::{self, BoardFile};
use lemnos_slot_store::bootfat::{self, ReadAt};
use lemnos_slot_store::fatimage;
use lemnos_slot_store::firmware::{self, CONFIG_TXT, Pick, TRYBOOT_TXT};
use lemnos_slot_store::twocopy::Kind;
use std::io;

fn reference() -> BoardFile {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testing/boards/reference.toml");
    board::parse(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn text(files: &[(String, Vec<u8>)], name: &str) -> String {
    let bytes = &files.iter().find(|(file, _)| file == name).unwrap().1;
    String::from_utf8(bytes.clone()).unwrap()
}

/// The config files are the header, the fixed text, then the board's lines; the pointers and the
/// records are as the slots read them.
#[test]
fn the_config_files_are_the_fixed_text_then_the_board_lines() {
    let board = reference();
    let out = files(&board, "gpio=5=op,dh\n", "a1").unwrap();
    assert_eq!(
        text(&out, "config.txt"),
        format!("{HEADER}{CONFIG_TXT}gpio=5=op,dh\n")
    );
    assert_eq!(
        text(&out, "tryboot.txt"),
        format!("{HEADER}{TRYBOOT_TXT}gpio=5=op,dh\n")
    );
}

/// Without board lines the files are the fixed text exactly.
#[test]
fn with_no_board_lines_the_files_are_the_fixed_text() {
    let out = files(&reference(), "", "a1").unwrap();
    assert_eq!(text(&out, "config.txt"), format!("{HEADER}{CONFIG_TXT}"));
    assert_eq!(text(&out, "tryboot.txt"), format!("{HEADER}{TRYBOOT_TXT}"));
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

/// Each pointer file is one sector that names the slot, and a placeholder names no slot yet.
#[test]
fn the_pointers_name_the_slot_in_one_sector_each() {
    let out = files(&reference(), "", "a1").unwrap();
    for name in ["default.txt", "previous.txt", "trial.txt"] {
        let bytes = &out.iter().find(|(file, _)| file == name).unwrap().1;
        assert_eq!(bytes.len(), firmware::POINTER_BYTES, "{name}");
        let sector: [u8; firmware::POINTER_BYTES] = bytes.as_slice().try_into().unwrap();
        assert_eq!(
            firmware::pointer_slot(&sector).as_deref(),
            Some("a1"),
            "{name}"
        );
    }
    let placeholder = files(&reference(), "", "").unwrap();
    let bytes = &placeholder
        .iter()
        .find(|(file, _)| file == "default.txt")
        .unwrap()
        .1;
    let sector: [u8; firmware::POINTER_BYTES] = bytes.as_slice().try_into().unwrap();
    assert_eq!(
        firmware::pointer_slot(&sector).as_deref(),
        Some(PLACEHOLDER)
    );
}

/// The record files are the three pre-created names, under their visible names, at one record's size.
#[test]
fn the_record_files_are_pre_created_under_visible_names() {
    let out = files(&reference(), "", "a1").unwrap();
    for name in ["trial.rec", "reason.rec", "episodes.rec"] {
        assert!(!name.contains(' '), "{name} has a space");
        let bytes = &out.iter().find(|(file, _)| file == name).unwrap().1;
        assert_eq!(bytes.len(), 1024, "{name}");
    }
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

/// A volume image read through the locator's interface.
struct Image(Vec<u8>);

impl ReadAt for Image {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let start = usize::try_from(offset).unwrap();
        buf.copy_from_slice(&self.0[start..start + buf.len()]);
        Ok(())
    }
}

/// The records the generator writes are found by the locator through the directory entries the
/// image builder makes from their visible names: the same eleven bytes the slot-store constants name.
#[test]
fn the_record_files_are_found_by_their_directory_entries() {
    let out = files(&reference(), "", "a1").unwrap();
    let mut entries = Vec::new();
    for (kind, visible) in [
        (Kind::TRIAL, "trial.rec"),
        (Kind::REASON, "reason.rec"),
        (Kind::EPISODES, "episodes.rec"),
    ] {
        let bytes = out
            .iter()
            .find(|(file, _)| file == visible)
            .unwrap()
            .1
            .clone();
        let entry = directory_entry(visible);
        assert_eq!(&entry, kind.file_name().unwrap(), "{visible}");
        entries.push((entry, bytes));
    }
    let refs: Vec<(&[u8; 11], Vec<u8>)> = entries.iter().map(|(e, b)| (e, b.clone())).collect();
    let mut image = Image(fatimage::fat16(&refs));
    for (kind, visible) in [
        (Kind::TRIAL, "trial.rec"),
        (Kind::REASON, "reason.rec"),
        (Kind::EPISODES, "episodes.rec"),
    ] {
        let found = bootfat::locate(&mut image, kind.file_name().unwrap(), 1024)
            .unwrap_or_else(|e| panic!("{visible}: {e:?}"));
        assert!(found.first_lba > 0, "{visible} is placed");
    }
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

/// A slot id that is not one is refused before anything is written.
#[test]
fn a_bad_slot_id_is_refused() {
    let err = files(&reference(), "", "A B").unwrap_err();
    assert!(err.contains("isn't a slot id"), "{err}");
}

/// A record other than `tryboot` has no firmware files here, and the refusal says so.
#[test]
fn a_board_with_another_record_is_refused_by_name() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testing/boards/reference.toml");
    let text = std::fs::read_to_string(path).unwrap();
    assert!(
        text.contains("record = \"tryboot\""),
        "the reference board names its record"
    );
    let board = board::parse(&text.replace("record = \"tryboot\"", "record = \"uboot\"")).unwrap();
    let err = files(&board, "", "a1").unwrap_err();
    assert!(
        err.contains("\"uboot\"") && err.contains("only \"tryboot\""),
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
    let err = files(&board, "", "a1").unwrap_err();
    assert!(err.contains("no system-slots device"), "{err}");
}

/// The command writes the files into the named directory, and the golden config.txt of the
/// reference board carries the same fixed text.
#[test]
fn the_tree_command_writes_the_files_into_the_directory() {
    let dir = std::env::temp_dir().join(format!("lemnos-ctl-boot-dir-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let args: Vec<String> = [
        "tree",
        "--socs",
        repo.join("socs").to_str().unwrap(),
        repo.join("testing/boards/reference.toml").to_str().unwrap(),
        "--boot-dir",
        dir.to_str().unwrap(),
        "--slot",
        "a1",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let mut out = Vec::new();
    crate::run(&args, &mut out).unwrap();
    let config = std::fs::read_to_string(dir.join("config.txt")).unwrap();
    assert!(config.starts_with(HEADER) && config.contains(CONFIG_TXT));
    assert!(dir.join("trial.txt").exists() && dir.join("episodes.rec").exists());
    std::fs::remove_dir_all(&dir).unwrap();
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
        assert_eq!(files.len(), 6);
        assert_eq!(
            required_for(&files, "rpi-tryboot"),
            names::RPI_TRYBOOT_REQUIRED
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_version_other_than_two_is_refused() {
        for other in ["version = 1", "version = 2", "version = 4"] {
            let text = LAYOUT.replace("version = 3", other);
            let why = read_layout(&text).unwrap_err();
            assert!(why.contains("reads 3"), "{why}");
        }
    }

    #[test]
    fn a_source_other_than_build_or_generated_is_refused() {
        let text = LAYOUT.replacen("source = \"build\"", "source = \"made\"", 1);
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
        assert_eq!(generated, [names::OVERLAY_README]);
        assert!(
            files
                .iter()
                .all(|f| f.source == "build" || f.source == "generated")
        );
    }

    #[test]
    fn the_boot_directory_writes_the_generated_readme_under_the_slot_directory() {
        use super::super::boot_dir::{OVERLAY_README_TEXT, SLOT_DIR, files};
        let out = files(&super::reference(), "", "a1").unwrap();
        let path = format!("{SLOT_DIR}/{}", names::OVERLAY_README);
        let readme = out.iter().find(|(n, _)| *n == path).unwrap();
        assert_eq!(readme.1, OVERLAY_README_TEXT.as_bytes());
        assert_eq!(
            out.iter().filter(|(n, _)| n.starts_with("slot/")).count(),
            1
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
    fn the_boot_directory_carries_the_contract() {
        use super::super::boot_dir::files;
        let board = super::reference();
        let out = files(&board, "", "a1").unwrap();
        let copy = out.iter().find(|(n, _)| n == "slot-layout.toml").unwrap();
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
