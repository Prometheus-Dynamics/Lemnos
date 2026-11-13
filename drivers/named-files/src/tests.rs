use super::*;
use lemnos_core::ConfigValue;
use lemnos_slot_store::store::Layout;
use std::fs;

/// A fake data partition, boot partition, block tree and module, in a temporary directory.
struct Fake(PathBuf);

impl Fake {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("named-files-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.0.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn path(&self, rel: &str) -> String {
        self.0.join(rel).display().to_string()
    }

    fn start(&self, extra: &[(&str, ConfigValue<'_>)]) -> NamedFilesDriver {
        let data = self.path("data");
        let boot = self.path("boot");
        let sys_block = self.path("sys/block");
        let module = self.path("sys/module/lemnos");
        let mut entries = vec![
            ("data", ConfigValue::Str(&data)),
            ("boot", ConfigValue::Str(&boot)),
            ("sys_block", ConfigValue::Str(&sys_block)),
            ("module", ConfigValue::Str(&module)),
        ];
        entries.extend_from_slice(extra);
        NamedFilesDriver::start(NamedFilesDriverNeeds {}, &Config::new(&entries)).unwrap()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Reads a whole snapshot in pieces of `piece` bytes.
fn read_all(driver: &mut NamedFilesDriver, handle: u64, piece: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; piece];
    loop {
        let n = driver.read(handle, out.len() as u64, &mut buf).unwrap();
        if n == 0 {
            return out;
        }
        out.extend_from_slice(&buf[..n]);
    }
}

#[test]
fn the_index_lists_what_is_served_with_its_bounds_and_its_total() {
    let fake = Fake::new("index");
    fake.write("boot/trial-reason", "power lost\n");
    let mut driver = fake.start(&[]);
    let mut buf = vec![0u8; 4096];
    let len = driver.index(&mut buf).unwrap();
    let text = std::str::from_utf8(&buf[..len]).unwrap();
    let (total, lines) = parse_index(text).expect("an orion-index 1 text");
    let names: Vec<&str> = lines.iter().map(|l| l.name.as_str()).collect();
    // The storage file is there whenever the partitions can be measured, as on any real board.
    assert_eq!(
        names,
        ["lemnos/storage", "lemnos/trial-reason"],
        "only what is there now is listed"
    );
    let trial = lines
        .iter()
        .find(|l| l.name == "lemnos/trial-reason")
        .unwrap();
    assert_eq!(trial.max_bytes, Name::TrialReason.bound() as u64);
    assert!(!trial.user_data);
    let bounds = (Name::Storage.bound() + Name::TrialReason.bound()) as u64;
    assert_eq!(total, bounds, "the total is the listed bounds");
}

#[test]
fn a_name_the_node_does_not_serve_is_refused_by_name() {
    let fake = Fake::new("refused");
    let mut driver = fake.start(&[]);
    assert_eq!(driver.open("lemnos/nope"), Err(Status::NOT_FOUND));
    assert_eq!(driver.open("system/kernel-log"), Err(Status::NOT_FOUND));
    // A tree file whose key is not given is absent, not empty.
    assert_eq!(driver.open("lemnos/tree/boot.txt"), Err(Status::NOT_FOUND));
}

#[test]
fn a_tree_file_is_served_as_the_board_file_names_it_and_refused_over_its_bound() {
    let fake = Fake::new("tree");
    fake.write("gen/boot.txt", "dtoverlay=x\n");
    fake.write("gen/big.txt", &"x".repeat(Name::TreeBoot.bound() + 1));
    let boot = fake.path("gen/boot.txt");
    let big = fake.path("gen/big.txt");
    let mut driver = fake.start(&[("tree_boot", ConfigValue::Str(&boot))]);
    let handle = driver.open("lemnos/tree/boot.txt").unwrap();
    assert_eq!(read_all(&mut driver, handle, 3), b"dtoverlay=x\n");
    driver.close(handle).unwrap();
    // The boot file's bound is the one this file goes over.
    let mut tight = fake.start(&[("tree_boot", ConfigValue::Str(&big))]);
    assert_eq!(
        tight.open("lemnos/tree/boot.txt"),
        Err(Status::INVALID_ARGUMENT)
    );
}

#[test]
fn a_snapshot_is_fixed_for_its_transfer_and_reads_back_in_pieces() {
    let fake = Fake::new("snapshot");
    fake.write("boot/trial-reason", "first reason\n");
    let mut driver = fake.start(&[]);
    let handle = driver.open("lemnos/trial-reason").unwrap();
    fake.write("boot/trial-reason", "a later reason\n");
    assert_eq!(driver.size(handle).unwrap(), "first reason\n".len() as u64);
    assert_eq!(
        read_all(&mut driver, handle, 4),
        b"first reason\n",
        "the snapshot holds"
    );
    // A resumed read from the middle gives the same bytes.
    let mut buf = [0u8; 64];
    let n = driver.read(handle, 6, &mut buf).unwrap();
    assert_eq!(&buf[..n], b"reason\n");
    assert_eq!(
        driver.read(handle, 999, &mut buf).unwrap(),
        0,
        "past the end is nothing"
    );
}

#[test]
fn at_most_max_open_snapshots_are_held_and_closing_one_frees_it() {
    let fake = Fake::new("busy");
    fake.write("boot/trial-reason", "r\n");
    let mut driver = fake.start(&[]);
    let mut handles = Vec::new();
    for _ in 0..MAX_OPEN {
        handles.push(driver.open("lemnos/trial-reason").unwrap());
    }
    assert_eq!(driver.open("lemnos/trial-reason"), Err(Status::BUSY));
    driver.close(handles[0]).unwrap();
    assert!(driver.open("lemnos/trial-reason").is_ok());
    assert_eq!(
        driver.close(handles[0]),
        Err(Status::NOT_FOUND),
        "closed twice"
    );
    assert_eq!(driver.size(handles[0]), Err(Status::NOT_FOUND));
}

#[test]
fn the_storage_file_has_each_partition_and_the_life_the_kernel_exposes() {
    let fake = Fake::new("storage");
    fs::create_dir_all(fake.path("data")).unwrap();
    fs::create_dir_all(fake.path("boot")).unwrap();
    fake.write("sys/block/mmcblk0/device/life_time", "0x02 0x01\n");
    fake.write("sys/block/mmcblk0/device/pre_eol_info", "0x01\n");
    fake.write("sys/block/sda/device/life_time", "ignored\n");
    let mut driver = fake.start(&[]);
    let handle = driver.open("lemnos/storage").unwrap();
    let text = String::from_utf8(read_all(&mut driver, handle, 64)).unwrap();
    assert!(text.contains("data total "), "{text}");
    assert!(text.contains("boot total "), "{text}");
    assert!(text.contains("life mmcblk0 0x02 0x01\n"), "{text}");
    assert!(text.contains("eol mmcblk0 0x01\n"), "{text}");
    assert!(!text.contains("sda"), "only mmc devices are read: {text}");
}

#[test]
fn the_kernel_file_is_the_module_parameters_sorted_and_absent_while_not_loaded() {
    let fake = Fake::new("kernel");
    let mut driver = fake.start(&[]);
    assert_eq!(
        driver.open("lemnos/kernel"),
        Err(Status::NOT_FOUND),
        "module not loaded"
    );
    fake.write("sys/module/lemnos/parameters/zeta", "2\n");
    fake.write("sys/module/lemnos/parameters/alpha", "1\n");
    let handle = driver.open("lemnos/kernel").unwrap();
    let text = String::from_utf8(read_all(&mut driver, handle, 16)).unwrap();
    assert_eq!(text, "alpha=1\nzeta=2\n");
}

#[test]
fn the_boot_history_is_served_once_a_boot_has_been_recorded() {
    let fake = Fake::new("boots");
    let mut driver = fake.start(&[]);
    assert_eq!(
        driver.open("lemnos/boots"),
        Err(Status::NOT_FOUND),
        "no boot yet"
    );
    let layout = Layout::new(fake.path("data"), fake.path("boot"));
    lemnos_slot_store::boots::append(&layout, "boot-1", "a", "unknown", "none", "-", 16).unwrap();
    let handle = driver.open("lemnos/boots").unwrap();
    let text = String::from_utf8(read_all(&mut driver, handle, 32)).unwrap();
    assert!(text.starts_with("1 "), "{text}");
    assert!(text.contains(" a unknown none - boot-1"), "{text}");
}

#[test]
fn the_index_is_itself_a_snapshot_that_reads_back() {
    let fake = Fake::new("index-snapshot");
    fake.write("boot/trial-reason", "r\n");
    let mut driver = fake.start(&[]);
    let handle = driver.open(INDEX).unwrap();
    let text = String::from_utf8(read_all(&mut driver, handle, 7)).unwrap();
    assert!(parse_index(&text).is_some(), "{text}");
}

#[test]
fn the_index_text_roundtrips_through_the_parser() {
    let text = render_index(&[Name::Boots, Name::Kernel]);
    let (total, lines) = parse_index(&text).unwrap();
    assert_eq!(total, (Name::Boots.bound() + Name::Kernel.bound()) as u64);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1].name, "lemnos/kernel");
    assert!(parse_index("not an index").is_none());
}

/// The refusal `check_config` gives for `entries`, or `None` when it accepts them.
fn refusal<'a>(entries: &'a [(&'a str, ConfigValue<'a>)]) -> Option<lemnos_core::ConfigError<'a>> {
    <NamedFilesDriver as lemnos_core::Lifecycle>::check_config(&Config::new(entries)).err()
}

#[test]
fn check_config_accepts_every_setting_and_refuses_an_unknown_one_by_name() {
    let full = [
        ("data", ConfigValue::Str("/data")),
        ("boot", ConfigValue::Str("/boot/firmware")),
        ("sys_block", ConfigValue::Str("/sys/block")),
        ("module", ConfigValue::Str("/sys/module/lemnos")),
        ("tree_overlay", ConfigValue::Str("/x/overlay")),
        ("tree_boot", ConfigValue::Str("/x/boot")),
        ("tree_modprobe", ConfigValue::Str("/x/modprobe")),
    ];
    assert_eq!(refusal(&full), None);
    let err = refusal(&[("tree_bot", ConfigValue::Str("/x"))]).unwrap();
    assert_eq!(err.key, "tree_bot");
    assert_eq!(err.suggestion, Some("tree_boot"));
    assert_eq!(
        refusal(&[("bogus", ConfigValue::Int(1))]).unwrap().key,
        "bogus"
    );
}

#[test]
fn check_config_refuses_a_path_given_as_a_number() {
    assert_eq!(
        refusal(&[("boot", ConfigValue::Int(1))]).unwrap().key,
        "boot"
    );
}
