use super::*;
use lemnos_contract_system_identity::MAX_FACTS_TEXT;
use lemnos_core::ConfigValue;
use std::fs;

/// A fake data partition and boot files, in its own directory, removed on drop.
struct Tree(PathBuf);

impl Tree {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("system-identity-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }

    fn path(&self, rel: &str) -> String {
        self.0.join(rel).display().to_string()
    }

    fn write(&self, rel: &str, bytes: &[u8]) {
        let path = self.0.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// A schema 3 manifest for slot `os-2`, one data block: one block has no hash tree, so the image
/// is that block alone.
fn manifest() -> String {
    format!(
        "{{\"schema\":3,\"id\":\"os-2\",\"product\":\"an-os\",\"version\":\"2.0\",\"board\":\"alpha-1\",\
         \"image\":{{\"size\":4096,\"sha256\":\"{HASH}\"}},\
         \"verity\":{{\"root_hash\":\"{HASH}\",\"salt\":\"{HASH}\",\"data_blocks\":1,\"hash_offset\":4096}},\
         \"boot\":{{\"files\":{{\"cmdline.txt\":\"{HASH}\"}}}}}}"
    )
}

fn start(entries: &[(&str, ConfigValue<'_>)], board: &str) -> Result<Identity> {
    Identity::start(IdentityNeeds {}, &Config::new(entries).on_board(board))
}

/// The same, on a daemon of version `version`.
fn start_on(entries: &[(&str, ConfigValue<'_>)], board: &str, version: &str) -> Result<Identity> {
    let config = Config::new(entries)
        .on_board(board)
        .on_host_version(version);
    Identity::start(IdentityNeeds {}, &config)
}

/// Calls a method into a buffer of `size` bytes and returns the text it wrote.
fn text(answer: Result<usize>, buf: &[u8]) -> String {
    String::from_utf8(buf[..answer.expect("answered")].to_vec()).unwrap()
}

#[test]
fn every_fact_comes_from_the_board_and_the_running_slot() {
    let tree = Tree::new("full");
    tree.write("cmdline", b"console=ttyS0 serialno=0000000000ABCDEF\n");
    tree.write("data/board/slots/booted", b"os-2\n");
    tree.write("data/slots/os-2.json", manifest().as_bytes());
    let cmdline = tree.path("cmdline");
    let data = tree.path("data");
    let mut id = start_on(
        &[
            ("serial", ConfigValue::Str("cmdline")),
            ("serial_key", ConfigValue::Str("serialno")),
            ("serial_path", ConfigValue::Str(&cmdline)),
            ("name", ConfigValue::Str("Alpha")),
            ("revision", ConfigValue::Str("r2")),
            ("data", ConfigValue::Str(&data)),
        ],
        "alpha-1",
        "2.0.1",
    )
    .unwrap();
    assert!(matches!(id.health(), Health::Ok));

    let mut buf = [0u8; MAX_FACTS_TEXT];
    assert_eq!(text(id.family(&mut buf), &buf), "alpha-1");
    assert_eq!(text(id.serial(&mut buf), &buf), "0000000000abcdef");
    assert_eq!(text(id.model(&mut buf), &buf), "Alpha r2");
    let facts = text(id.facts(&mut buf), &buf);
    assert_eq!(
        facts,
        "family=alpha-1\nserial=0000000000abcdef\nmodel=Alpha r2\nversion.slot=os-2\nversion.product=an-os\nversion.os=2.0\nversion.lemnos=2.0.1\n"
    );
}

#[test]
fn an_absent_fact_answers_not_found_and_the_device_is_degraded() {
    let tree = Tree::new("absent");
    let data = tree.path("data");
    let mut id = start(&[("data", ConfigValue::Str(&data))], "").unwrap();
    assert!(matches!(id.health(), Health::Degraded(_)));
    let mut buf = [0u8; MAX_FACTS_TEXT];
    assert_eq!(id.serial(&mut buf), Err(Status::NOT_FOUND));
    assert_eq!(id.family(&mut buf), Err(Status::NOT_FOUND));
    assert_eq!(id.model(&mut buf), Err(Status::NOT_FOUND));
    assert_eq!(text(id.facts(&mut buf), &buf), "");
}

#[test]
fn a_serial_that_does_not_fit_leaves_it_absent_and_binds() {
    let tree = Tree::new("refused");
    tree.write("cmdline", b"serialno=zzzz\n");
    let cmdline = tree.path("cmdline");
    let mut id = start(
        &[
            ("serial", ConfigValue::Str("cmdline")),
            ("serial_key", ConfigValue::Str("serialno")),
            ("serial_path", ConfigValue::Str(&cmdline)),
        ],
        "alpha-1",
    )
    .unwrap();
    assert!(matches!(id.health(), Health::Degraded("no serial")));
    let mut buf = [0u8; MAX_FACTS_TEXT];
    assert_eq!(id.serial(&mut buf), Err(Status::NOT_FOUND));
    assert_eq!(text(id.family(&mut buf), &buf), "alpha-1");
}

#[test]
fn a_source_that_cannot_be_read_fails_the_bind_so_the_host_retries() {
    let tree = Tree::new("unreadable");
    let missing = tree.path("no-such-device-tree/serial-number");
    let result = start(
        &[
            ("serial", ConfigValue::Str("device-tree")),
            ("serial_path", ConfigValue::Str(&missing)),
        ],
        "alpha-1",
    );
    assert!(matches!(result, Err(s) if s == Status::NOT_FOUND));
}

#[test]
fn a_manifest_that_does_not_parse_leaves_the_slot_facts_absent() {
    let tree = Tree::new("bad-manifest");
    tree.write("data/board/slots/booted", b"os-2\n");
    tree.write("data/slots/os-2.json", b"{\"schema\":2}");
    let data = tree.path("data");
    let mut id = start(&[("data", ConfigValue::Str(&data))], "alpha-1").unwrap();
    let mut buf = [0u8; MAX_FACTS_TEXT];
    assert_eq!(text(id.facts(&mut buf), &buf), "family=alpha-1\n");
}

#[test]
fn a_buffer_too_small_is_refused() {
    let tree = Tree::new("small");
    let data = tree.path("data");
    let cmdline = tree.path("cmdline");
    tree.write("cmdline", b"serialno=0123456789ab\n");
    let mut id = start(
        &[
            ("serial", ConfigValue::Str("cmdline")),
            ("serial_key", ConfigValue::Str("serialno")),
            ("serial_path", ConfigValue::Str(&cmdline)),
            ("data", ConfigValue::Str(&data)),
        ],
        "",
    )
    .unwrap();
    let mut small = [0u8; 4];
    assert_eq!(id.serial(&mut small), Err(Status::BUFFER_TOO_SMALL));
}

#[test]
fn an_nvmem_cell_is_read_at_its_offset() {
    let tree = Tree::new("nvmem");
    tree.write("otp", &[0xff, 0xff, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc]);
    let otp = tree.path("otp");
    let data = tree.path("data");
    let mut id = start(
        &[
            ("serial", ConfigValue::Str("nvmem")),
            ("serial_path", ConfigValue::Str(&otp)),
            ("serial_offset", ConfigValue::Int(2)),
            ("serial_length", ConfigValue::Int(6)),
            ("data", ConfigValue::Str(&data)),
        ],
        "",
    )
    .unwrap();
    let mut buf = [0u8; MAX_FACTS_TEXT];
    assert_eq!(text(id.serial(&mut buf), &buf), "123456789abc");
}

#[test]
fn the_running_slot_is_read_again_on_each_facts_call() {
    let tree = Tree::new("reread");
    tree.write("data/board/slots/booted", b"os-2\n");
    tree.write("data/slots/os-2.json", manifest().as_bytes());
    let data = tree.path("data");
    let mut id = start_on(&[("data", ConfigValue::Str(&data))], "alpha-1", "2.0.1").unwrap();
    let mut buf = [0u8; MAX_FACTS_TEXT];
    assert!(text(id.facts(&mut buf), &buf).contains("version.slot=os-2\n"));

    // The boot marker moves to another slot, with no rebind of the device.
    let next = manifest()
        .replace("os-2", "os-3")
        .replace("an-os", "other-os");
    tree.write("data/board/slots/booted", b"os-3\n");
    tree.write("data/slots/os-3.json", next.as_bytes());
    let facts = text(id.facts(&mut buf), &buf);
    assert!(facts.contains("version.slot=os-3\n"), "{facts}");
    assert!(facts.contains("version.product=other-os\n"), "{facts}");
    assert!(!facts.contains("os-2"), "{facts}");
    assert!(facts.starts_with("family=alpha-1\n"), "{facts}");
}

#[test]
fn a_slot_file_that_cannot_be_read_now_leaves_only_its_facts_absent() {
    let tree = Tree::new("slot-gone");
    tree.write("data/board/slots/booted", b"os-2\n");
    tree.write("data/slots/os-2.json", manifest().as_bytes());
    let data = tree.path("data");
    let mut id = start_on(&[("data", ConfigValue::Str(&data))], "alpha-1", "2.0.1").unwrap();
    // The marker is replaced by something that is not a file the reader can use: a directory.
    fs::remove_file(tree.0.join("data/board/slots/booted")).unwrap();
    fs::create_dir_all(tree.0.join("data/board/slots/booted")).unwrap();
    let mut buf = [0u8; MAX_FACTS_TEXT];
    let facts = text(id.facts(&mut buf), &buf);
    assert_eq!(facts, "family=alpha-1\nversion.lemnos=2.0.1\n", "{facts}");
}
