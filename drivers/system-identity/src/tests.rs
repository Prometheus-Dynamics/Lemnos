use super::*;
use lemnos_contract_system_identity::{MAX_FACTS_TEXT, MAX_MACHINE_ID};
use lemnos_core::{Config, ConfigError, ConfigValue, Lifecycle, RefusalKind, RefusalSink};
use std::fs;

/// A fake data partition and boot files, in its own directory, removed on drop.
struct Tree(PathBuf);

impl Tree {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("system-identity-{name}-{}", std::process::id()));
        // Whatever is at the name is not ours to keep: a file left there would stop the directory.
        if fs::symlink_metadata(&root).is_ok_and(|m| m.is_dir()) {
            let _ = fs::remove_dir_all(&root);
        } else {
            let _ = fs::remove_file(&root);
        }
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
    // The board's own bound id, so the health check does not read the host's /etc/machine-id.
    tree.write(
        "etc/machine-id",
        lemnos_identity::machine_id::derive("0000000000abcdef").as_bytes(),
    );
    id.machine_id = tree.0.join("etc/machine-id");
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
fn an_absent_fact_answers_not_found_and_the_device_is_healthy() {
    let tree = Tree::new("absent");
    let data = tree.path("data");
    let mut id = start(&[("data", ConfigValue::Str(&data))], "").unwrap();
    // No serial source is configured: that is a normal state, not a fault.
    assert!(matches!(id.health(), Health::Ok));
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

#[test]
fn check_config_names_the_bad_setting() {
    fn check(
        entries: &'static [(&'static str, ConfigValue<'static>)],
    ) -> core::result::Result<(), ConfigError<'static>> {
        <Identity as Lifecycle>::check_config(&Config::new(entries))
    }
    let nvmem = [
        ("serial", ConfigValue::Str("nvmem")),
        ("serial_path", ConfigValue::Str("/otp")),
        ("serial_offset", ConfigValue::Int(2)),
        ("serial_length", ConfigValue::Int(6)),
        ("serial_byte_order", ConfigValue::Str("little")),
        ("name", ConfigValue::Str("Alpha")),
    ];
    assert!(<Identity as Lifecycle>::check_config(&Config::new(&nvmem)).is_ok());
    assert!(check(&[]).is_ok());
    // The byte order is read only for nvmem, so another source does not refuse it.
    assert!(
        check(&[
            ("serial", ConfigValue::Str("cpuinfo")),
            ("serial_byte_order", ConfigValue::Str("x"))
        ])
        .is_ok()
    );

    assert_eq!(
        check(&[("serial", ConfigValue::Str("usb"))])
            .unwrap_err()
            .key,
        "serial"
    );
    assert_eq!(
        check(&[("serial", ConfigValue::Str("cmdline"))])
            .unwrap_err()
            .key,
        "serial_key"
    );
    assert_eq!(
        check(&[
            ("serial", ConfigValue::Str("nvmem")),
            ("serial_length", ConfigValue::Int(6))
        ])
        .unwrap_err()
        .key,
        "serial_path"
    );
    assert_eq!(
        check(&[
            ("serial", ConfigValue::Str("nvmem")),
            ("serial_path", ConfigValue::Str("/otp"))
        ])
        .unwrap_err()
        .key,
        "serial_length"
    );
    assert_eq!(
        check(&[
            ("serial", ConfigValue::Str("nvmem")),
            ("serial_path", ConfigValue::Str("/otp")),
            ("serial_length", ConfigValue::Int(-1))
        ])
        .unwrap_err()
        .key,
        "serial_length"
    );
    assert_eq!(
        check(&[
            ("serial", ConfigValue::Str("nvmem")),
            ("serial_path", ConfigValue::Str("/otp")),
            ("serial_length", ConfigValue::Int(6)),
            ("serial_offset", ConfigValue::Int(-2))
        ])
        .unwrap_err()
        .key,
        "serial_offset"
    );
    let order = check(&[
        ("serial", ConfigValue::Str("nvmem")),
        ("serial_path", ConfigValue::Str("/otp")),
        ("serial_length", ConfigValue::Int(6)),
        ("serial_byte_order", ConfigValue::Str("middle")),
    ])
    .unwrap_err();
    assert_eq!(
        (order.key, order.reason),
        ("serial_byte_order", "must be big or little")
    );
    let near = check(&[("serial_lenght", ConfigValue::Int(6))]).unwrap_err();
    assert_eq!(
        (near.key, near.suggestion),
        ("serial_lenght", Some("serial_length"))
    );
    assert_eq!(
        check(&[("serial", ConfigValue::Int(1))]).unwrap_err().key,
        "serial"
    );
    assert_eq!(
        check(&[("bogus", ConfigValue::Int(1))]).unwrap_err().key,
        "bogus"
    );
}

/// The manifest's version bound is the identity fact's own: a manifest that installs is a version the
/// fact publishes, never one it drops quietly.
#[test]
fn the_manifests_version_bound_is_the_fact_s_limit() {
    assert_eq!(
        lemnos_slot_store::manifest::VERSION_MAX_BYTES,
        lemnos_identity::facts::MAX_VALUE_BYTES
    );
    let mut facts = lemnos_identity::facts::Facts::default();
    let longest = "v".repeat(lemnos_slot_store::manifest::VERSION_MAX_BYTES);
    assert!(lemnos_slot_store::manifest::check_version(&longest, "v").is_ok());
    assert!(facts.set("version.os", &longest).is_ok());
    for bad in [
        format!("{longest}v"),
        "a\u{1f}b".to_string(),
        "a\u{7f}".to_string(),
        String::new(),
    ] {
        assert!(lemnos_slot_store::manifest::check_version(&bad, "v").is_err());
        assert!(facts.set("version.os", &bad).is_err());
    }
}

/// The bind's result, and the refusal the host would record for it (its kind and reason).
fn refused_bind(
    entries: &[(&str, ConfigValue<'_>)],
) -> (Result<()>, Option<(RefusalKind, String)>) {
    let seen = std::sync::Mutex::new(None);
    let sink = |kind: RefusalKind, reason: core::fmt::Arguments<'_>| {
        *seen.lock().unwrap() = Some((kind, reason.to_string()));
    };
    let config = Config::new(entries)
        .on_board("alpha-1")
        .with_refusals(RefusalSink::new(&sink));
    let result = Identity::start(IdentityNeeds {}, &config).map(|_| ());
    (result, seen.into_inner().unwrap())
}

#[test]
fn a_missing_serial_source_is_refused_as_absent_and_names_its_path() {
    let tree = Tree::new("refused-missing");
    let missing = tree.path("no-such-device-tree/serial-number");
    let (result, refusal) = refused_bind(&[
        ("serial", ConfigValue::Str("device-tree")),
        ("serial_path", ConfigValue::Str(&missing)),
    ]);
    assert_eq!(result, Err(Status::NOT_FOUND));
    let (kind, reason) = refusal.expect("a refused bind gives its reason");
    assert_eq!(kind, RefusalKind::Absent);
    assert!(reason.contains(&missing), "{reason}");
    assert!(reason.starts_with("no serial source"), "{reason}");
}

#[test]
fn a_serial_source_that_cannot_be_read_is_refused_as_failed_and_names_its_path() {
    let tree = Tree::new("refused-unreadable");
    // A directory where the serial file should be: it exists, and cannot be read as a file.
    let dir = tree.0.join("serial-is-a-directory");
    fs::create_dir_all(&dir).unwrap();
    let dir_text = dir.display().to_string();
    let (result, refusal) = refused_bind(&[
        ("serial", ConfigValue::Str("device-tree")),
        ("serial_path", ConfigValue::Str(&dir_text)),
    ]);
    assert_eq!(result, Err(Status::IO));
    let (kind, reason) = refusal.expect("a refused bind gives its reason");
    assert_eq!(kind, RefusalKind::Failed);
    assert!(reason.contains(&dir_text), "{reason}");
}

/// The id slot-init bound answers `machine_id`, and no fact carries it: `facts` is what the bridge
/// publishes as `hello.*`, and it never holds the id (the answer is for local callers only).
#[test]
fn the_machine_id_answers_the_bound_id_and_no_fact_carries_it() {
    let tree = Tree::new("machine-id");
    tree.write("cmdline", b"serialno=0000000000ABCDEF\n");
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
    let bound = lemnos_identity::machine_id::derive("0000000000abcdef");
    tree.write("etc/machine-id", bound.as_bytes());
    id.machine_id = tree.0.join("etc/machine-id");
    let mut buf = [0u8; MAX_MACHINE_ID];
    assert_eq!(text(id.machine_id(&mut buf), &buf), bound);
    assert!(matches!(id.health(), Health::Ok));
    let mut facts_buf = [0u8; MAX_FACTS_TEXT];
    let facts = text(id.facts(&mut facts_buf), &facts_buf);
    assert!(!facts.contains("0123456789abcdef"), "{facts}");
    assert!(!facts.contains("machine"), "{facts}");
}

/// A root ships an empty `/etc/machine-id`, and an unbound one is absent: `machine_id` answers
/// `NOT_FOUND`, and so does a file that is not an id. The device stays healthy.
#[test]
fn an_unbound_machine_id_answers_not_found_and_the_device_is_healthy() {
    let tree = Tree::new("machine-id-absent");
    tree.write("cmdline", b"serialno=0000000000ABCDEF\n");
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
    id.machine_id = tree.0.join("etc/machine-id");
    let mut buf = [0u8; MAX_MACHINE_ID];
    assert_eq!(id.machine_id(&mut buf).err(), Some(Status::NOT_FOUND));
    // No id on the node is a normal state: a boot that is not a slot's (the QEMU guest, a dev host),
    // or a bind slot-init refused with a logged reason. The device stays healthy.
    assert!(matches!(id.health(), Health::Ok));
    for not_an_id in [
        &b""[..],
        b"0123\n",
        b"0123456789ABCDEF0123456789abcdef\n",
        b"0123456789abcdef0123456789abcdef0\n",
    ] {
        tree.write("etc/machine-id", not_an_id);
        assert_eq!(
            id.machine_id(&mut buf).err(),
            Some(Status::NOT_FOUND),
            "{not_an_id:?}"
        );
    }
    tree.write(
        "etc/machine-id",
        lemnos_identity::machine_id::derive("0000000000abcdef").as_bytes(),
    );
    assert!(id.machine_id(&mut buf).is_ok());
    assert!(matches!(id.health(), Health::Ok));
}

/// systemd's transient id is a valid id in the file's shape, but it is not the derivation of this
/// board's serial, so it is refused: `machine_id` answers `NOT_FOUND` and the device is degraded with
/// the reason. Before the check this id was answered as if it were slot-init's.
#[test]
fn a_mismatching_id_on_the_node_is_refused_as_transient() {
    let tree = Tree::new("machine-id-transient");
    tree.write("cmdline", b"serialno=0000000000ABCDEF\n");
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
    // A well-formed id that is not this board's: the one a transient boot gives.
    tree.write("etc/machine-id", b"5a1c0ffee5a1c0ffee5a1c0ffee5a1c0\n");
    id.machine_id = tree.0.join("etc/machine-id");
    let mut buf = [0u8; MAX_MACHINE_ID];
    assert_eq!(id.machine_id(&mut buf).err(), Some(Status::NOT_FOUND));
    let Health::Degraded(reason) = id.health() else {
        panic!("a transient id leaves the device degraded");
    };
    assert_eq!(reason, TRANSIENT);
}

/// With no serial source configured there is nothing to check an id against, so `machine_id` answers
/// `NOT_FOUND` for any id on the node, and the device is healthy: "no serial source" is not a fault.
#[test]
fn an_id_with_no_serial_source_configured_is_not_answered_and_not_degraded() {
    let tree = Tree::new("machine-id-no-serial");
    let mut id = start(&[], "alpha-1").unwrap();
    tree.write("etc/machine-id", b"0123456789abcdef0123456789abcdef\n");
    id.machine_id = tree.0.join("etc/machine-id");
    let mut buf = [0u8; MAX_MACHINE_ID];
    assert_eq!(id.machine_id(&mut buf).err(), Some(Status::NOT_FOUND));
    assert!(matches!(id.health(), Health::Ok));
}
