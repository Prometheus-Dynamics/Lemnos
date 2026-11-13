//! The serial sources, the normalisation, the board file and the facts, on fixture files.

use std::fs;
use std::path::{Path, PathBuf};

use lemnos_identity::serial::{ByteOrder, SerialError, SerialSource, normalise, normalise_bytes};
use lemnos_identity::{Facts, gather, parse_board};

/// A fresh directory under the test's target dir, with the given files written into it.
fn fixture(name: &str, files: &[(&str, &[u8])]) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&dir);
    for (path, bytes) in files {
        let full = dir.join(path);
        fs::create_dir_all(full.parent().expect("parent")).expect("mkdir");
        fs::write(full, bytes).expect("write");
    }
    dir
}

fn refused(result: Result<String, SerialError>) -> String {
    match result {
        Err(SerialError::Refused(why)) => why,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn normalises_case_prefix_separators_and_keeps_leading_zeros() {
    assert_eq!(normalise("00000000ABCDEF12").unwrap(), "00000000abcdef12");
    assert_eq!(normalise("0x00000000abcdef12").unwrap(), "00000000abcdef12");
    assert_eq!(normalise("0X00000000ABCDEF12").unwrap(), "00000000abcdef12");
    assert_eq!(
        normalise("00:00:00:00:ab:cd:ef:12").unwrap(),
        "00000000abcdef12"
    );
    assert_eq!(
        normalise("00-00-00-00-AB-CD-EF-12\n").unwrap(),
        "00000000abcdef12"
    );
    assert_eq!(
        normalise(" \t10000000abcdef12\0\n").unwrap(),
        "10000000abcdef12"
    );
    assert_eq!(normalise(&"f".repeat(32)).unwrap(), "f".repeat(32));
}

#[test]
fn refuses_what_does_not_fit_the_format() {
    assert!(normalise("abcdef1").unwrap_err().contains("7 hex digits"));
    assert!(
        normalise(&"a".repeat(33))
            .unwrap_err()
            .contains("33 hex digits")
    );
    assert!(normalise("").unwrap_err().contains("0 hex digits"));
    assert!(
        normalise("0000000000000000")
            .unwrap_err()
            .contains("all zeros")
    );
    assert!(normalise("0x0000:0000").unwrap_err().contains("all zeros"));
    assert!(normalise("zz00112233445566").unwrap_err().contains("'z'"));
    assert!(normalise("1234567g").unwrap_err().contains("'g'"));
}

#[test]
fn device_tree_serial_is_a_nul_terminated_string() {
    let dir = fixture("dt", &[("serial-number", b"10000000abcdef12\0")]);
    let source = SerialSource::DeviceTree {
        path: dir.join("serial-number"),
    };
    assert_eq!(source.read().unwrap(), "10000000abcdef12");
}

#[test]
fn cpuinfo_serial_line_is_read_and_missing_is_refused() {
    let text = "processor\t: 0\nHardware\t: BCM2835\nSerial\t\t: 00000000ABCDEF12\n";
    let dir = fixture("cpuinfo", &[("cpuinfo", text.as_bytes())]);
    let source = SerialSource::Cpuinfo {
        path: dir.join("cpuinfo"),
    };
    assert_eq!(source.read().unwrap(), "00000000abcdef12");

    let dir = fixture("cpuinfo-none", &[("cpuinfo", b"processor\t: 0\n")]);
    let source = SerialSource::Cpuinfo {
        path: dir.join("cpuinfo"),
    };
    assert!(refused(source.read()).contains("no Serial line"));
}

#[test]
fn nvmem_cell_is_read_at_its_offset_in_both_byte_orders() {
    let cell = [
        0xff, 0xff, 0xff, 0xff, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0,
    ];
    let dir = fixture("nvmem", &[("otp", &cell)]);
    let path = dir.join("otp");
    let big = SerialSource::Nvmem {
        path: path.clone(),
        offset: 4,
        length: 8,
        byte_order: ByteOrder::Big,
    };
    assert_eq!(big.read().unwrap(), "123456789abcdef0");
    let little = SerialSource::Nvmem {
        path: path.clone(),
        offset: 4,
        length: 8,
        byte_order: ByteOrder::Little,
    };
    assert_eq!(little.read().unwrap(), "f0debc9a78563412");
}

#[test]
fn nvmem_refuses_erased_unprogrammed_and_short_cells() {
    let dir = fixture(
        "nvmem-bad",
        &[("zero", &[0; 8]), ("ones", &[0xff; 8]), ("short", &[1, 2])],
    );
    let cell = |file: &str, length: usize| SerialSource::Nvmem {
        path: dir.join(file),
        offset: 0,
        length,
        byte_order: ByteOrder::Big,
    };
    assert!(refused(cell("zero", 8).read()).contains("never programmed"));
    assert!(refused(cell("ones", 8).read()).contains("all zeros or all ones"));
    assert!(matches!(
        cell("short", 4).read(),
        Err(SerialError::Read { .. })
    ));
    assert!(refused(cell("ones", 3).read()).contains("3 bytes"));
    assert!(
        normalise_bytes(&[1; 17], ByteOrder::Big)
            .unwrap_err()
            .contains("17 bytes")
    );
}

#[test]
fn cmdline_key_is_read_and_the_last_repeat_wins() {
    let line = "console=ttyAMA0 serialno=0123456789ab root=/dev/sda2 serialno=FEDCBA987654\n";
    let dir = fixture("cmdline", &[("cmdline", line.as_bytes())]);
    let source = SerialSource::Cmdline {
        key: "serialno".into(),
        path: dir.join("cmdline"),
    };
    assert_eq!(source.read().unwrap(), "fedcba987654");

    let missing = SerialSource::Cmdline {
        key: "androidboot.serialno".into(),
        path: dir.join("cmdline"),
    };
    assert!(refused(missing.read()).contains("no androidboot.serialno="));
}

#[test]
fn board_file_names_each_source_kind_and_refuses_a_bad_one() {
    let board = parse_board(
        "format = \"lemnos.board\"\nschema_version = 1\n\
         [board]\nid = \"alpha-1\"\nname = \"Alpha\"\nrevision = \"B\"\n\
         [board.serial]\nkind = \"nvmem\"\npath = \"/sys/x/otp\"\noffset = 16\nlength = 8\nbyte_order = \"little\"\n\
         [[devices]]\nid = \"d\"\ndriver = \"x\"\n",
    )
    .unwrap();
    assert_eq!(
        board.board.serial,
        Some(SerialSource::Nvmem {
            path: "/sys/x/otp".into(),
            offset: 16,
            length: 8,
            byte_order: ByteOrder::Little,
        })
    );
    let cpu = parse_board(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"a\"\n[board.serial]\nkind = \"cpuinfo\"\n",
    )
    .unwrap();
    assert_eq!(
        cpu.board.serial,
        Some(SerialSource::Cpuinfo {
            path: "/proc/cpuinfo".into()
        })
    );
    let unknown_key = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"a\"\n[board.serial]\nkind = \"cpuinfo\"\nextra = 1\n";
    assert!(parse_board(unknown_key).is_err());
    let unknown_kind = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"a\"\n[board.serial]\nkind = \"fuse-box\"\n";
    assert!(parse_board(unknown_kind).is_err());
    let wrong_format = "format = \"other\"\nschema_version = 1\n[board]\nid = \"a\"\n";
    assert!(
        parse_board(wrong_format)
            .unwrap_err()
            .contains("not \"lemnos.board\"")
    );
}

#[test]
fn gather_reads_the_board_and_the_running_slot() {
    let dt = fixture(
        "gather",
        &[
            ("serial-number", b"10000000abcdef12\0"),
            ("data/board/slots/booted", b"os-2\n"),
            (
                "data/slots/os-2.json",
                br#"{"id":"os-2","product":"ctl-os","version":"2.0.1","image":{"size":5}}"#,
            ),
        ],
    );
    let text = format!(
        "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"alpha-1\"\nname = \"Alpha Board\"\nrevision = \"r2\"\n[board.serial]\nkind = \"device-tree\"\npath = \"{}\"\n",
        dt.join("serial-number").display()
    );
    let board = parse_board(&text).unwrap();
    let got = gather(&board, Some(&dt.join("data")));
    let pairs: Vec<(&str, &str)> = got.facts.iter().collect();
    assert_eq!(
        pairs,
        vec![
            ("family", "alpha-1"),
            ("serial", "10000000abcdef12"),
            ("model", "Alpha Board r2"),
            ("version.slot", "os-2"),
            ("version.product", "ctl-os"),
            ("version.os", "2.0.1"),
        ]
    );
    assert!(got.notes.is_empty(), "{:?}", got.notes);
    assert_eq!(got.facts.get("offers"), None);
    assert_eq!(got.facts.get("version.lemnos"), None);
}

#[test]
fn gather_leaves_out_what_cannot_be_known_and_says_why() {
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"Bad Family\"\n";
    let board = parse_board(text).unwrap();
    let got = gather(&board, Some(Path::new("/nonexistent-lemnos-data")));
    assert!(got.facts.iter().next().is_none(), "{:?}", got.facts);
    assert!(
        got.notes
            .iter()
            .any(|n| n.starts_with("family:") && n.contains("lowercase"))
    );
    assert!(
        got.notes
            .iter()
            .any(|n| n.contains("no [board.serial] source"))
    );
    assert!(got.notes.iter().any(|n| n.contains("no [board] name")));
    assert!(
        got.notes
            .iter()
            .any(|n| n.starts_with("version.slot: cannot read"))
    );
}

#[test]
fn gather_without_data_skips_the_slot_facts() {
    let text = "format = \"lemnos.board\"\nschema_version = 1\n[board]\nid = \"alpha\"\nname = \"Alpha\"\n";
    let got = gather(&parse_board(text).unwrap(), None);
    assert_eq!(got.facts.get("family"), Some("alpha"));
    assert_eq!(got.facts.get("model"), Some("Alpha"));
    assert!(got.notes.iter().all(|n| !n.contains("version")));
}

#[test]
fn facts_are_checked_against_their_formats_and_limits() {
    let mut facts = Facts::default();
    assert!(
        facts
            .set("family", &"a".repeat(65))
            .unwrap_err()
            .contains("at most 64")
    );
    assert!(
        facts
            .set("family", "Alpha")
            .unwrap_err()
            .contains("lowercase")
    );
    assert!(
        facts
            .set("model", &"m".repeat(65))
            .unwrap_err()
            .contains("at most 64")
    );
    assert!(facts.set("model", &"m".repeat(64)).is_ok());
    assert!(
        facts
            .set("model", "line\nbreak")
            .unwrap_err()
            .contains("control")
    );
    assert!(
        facts
            .set("serial", "ABCDEF12")
            .unwrap_err()
            .contains("normalised")
    );
    assert!(facts.set("serial", "abcdef12").is_ok());
    assert!(
        facts
            .set("offers", "system.slots/1  system.reboot/1")
            .is_err()
    );
    assert!(
        facts
            .set("offers", "system.slots/1 system.reboot/1")
            .is_ok()
    );
    assert!(facts.set("version.slot", "os 2").is_err());
    assert!(
        facts
            .set("version.os", &"v".repeat(257))
            .unwrap_err()
            .contains("at most 256")
    );
    assert!(
        facts
            .set("state", "running")
            .unwrap_err()
            .contains("not an identity fact")
    );
    facts.set("model", "second").unwrap();
    assert_eq!(facts.get("model"), Some("second"));
    assert_eq!(facts.iter().count(), 3);
}
