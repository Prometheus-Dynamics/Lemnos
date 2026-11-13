use super::*;
use lemnos_core::ConfigValue;
use lemnos_measure::test_alloc::{Counting, allocations};
use std::fs;

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// A fake sysfs tree in its own directory, removed on drop.
struct Tree(PathBuf);

impl Tree {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("host-clock-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }

    fn path(&self) -> String {
        self.0.display().to_string()
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.0.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// The kernel's clock source.
    fn clock_source(&self, name: &str) {
        self.write(CLOCK_SOURCE, &format!("{name}\n"));
    }

    /// An RTC device directory with its `hctosys` attribute.
    fn rtc(&self, name: &str, hctosys: &str) {
        self.write(&format!("{RTC_CLASS}/{name}/hctosys"), hctosys);
        self.write(&format!("{RTC_CLASS}/{name}/name"), "fake-rtc\n");
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Starts the driver on `root`, with `rtc` as its `rtc` setting when given.
fn start(root: &str, rtc: Option<&str>) -> Result<HostClock> {
    let mut entries = vec![("sys_root", ConfigValue::Str(root))];
    if let Some(rtc) = rtc {
        entries.push(("rtc", ConfigValue::Str(rtc)));
    }
    HostClock::start(HostClockNeeds {}, &Config::new(&entries))
}

/// The names of the items the driver describes, in order.
fn described(dev: &HostClock) -> Vec<&'static str> {
    let mut out = Description::default();
    dev.describe(&mut out);
    out.items().map(|item| item.name).collect()
}

#[test]
fn describes_clock_source_and_rtc_that_set_the_clock() {
    let tree = Tree::new("full");
    tree.clock_source("tsc");
    tree.rtc("rtc0", "1\n");
    let dev = start(&tree.path(), None).unwrap();
    assert_eq!(described(&dev), ["clock_source", "rtc", "rtc_hctosys"]);
    assert_eq!(dev.health(), Health::Ok);
}

#[test]
fn rtc_that_did_not_set_the_clock_has_no_hctosys_item() {
    let tree = Tree::new("no-hctosys");
    tree.clock_source("arch_sys_counter");
    tree.rtc("rtc0", "0\n");
    let dev = start(&tree.path(), None).unwrap();
    assert_eq!(described(&dev), ["clock_source", "rtc"]);
    assert_eq!(dev.health(), Health::Ok);
}

#[test]
fn missing_rtc_is_degraded_not_a_failure() {
    let tree = Tree::new("no-rtc");
    tree.clock_source("tsc");
    let dev = start(&tree.path(), None).unwrap();
    assert_eq!(described(&dev), ["clock_source"]);
    assert_eq!(dev.health(), Health::Degraded("no real-time clock"));
}

#[test]
fn missing_clock_source_leaves_its_item_out() {
    let tree = Tree::new("no-source");
    tree.rtc("rtc0", "1");
    let dev = start(&tree.path(), None).unwrap();
    assert_eq!(described(&dev), ["rtc", "rtc_hctosys"]);
}

#[test]
fn empty_clock_source_counts_as_missing() {
    let tree = Tree::new("empty-source");
    tree.write(CLOCK_SOURCE, "\n");
    let dev = start(&tree.path(), None).unwrap();
    assert!(described(&dev).is_empty());
}

#[test]
fn absent_sysfs_root_binds_with_nothing_described() {
    let tree = Tree::new("absent-root");
    let root = tree.0.join("not-mounted").display().to_string();
    let dev = start(&root, None).unwrap();
    assert!(described(&dev).is_empty());
    assert_eq!(dev.health(), Health::Degraded("no real-time clock"));
}

#[test]
fn named_rtc_is_the_one_described() {
    let tree = Tree::new("named");
    tree.rtc("rtc0", "1");
    tree.rtc("rtc1", "0");
    let dev = start(&tree.path(), Some("rtc1")).unwrap();
    assert_eq!(described(&dev), ["rtc"]);
}

#[test]
fn first_rtc_in_name_order_is_used_without_a_setting() {
    let tree = Tree::new("first");
    tree.rtc("rtc1", "0");
    tree.rtc("rtc0", "1");
    let dev = start(&tree.path(), None).unwrap();
    assert_eq!(described(&dev), ["rtc", "rtc_hctosys"]);
}

#[test]
fn named_rtc_that_is_absent_fails_the_bind() {
    let tree = Tree::new("absent-named");
    tree.rtc("rtc0", "1");
    let result = start(&tree.path(), Some("rtc9"));
    assert_eq!(result.err(), Some(Status::NOT_FOUND));
}

#[test]
fn unreadable_attribute_fails_the_bind_so_the_host_retries() {
    let tree = Tree::new("unreadable");
    // A directory where the `hctosys` file should be: reading it fails.
    fs::create_dir_all(tree.0.join(RTC_CLASS).join("rtc0").join("hctosys")).unwrap();
    let result = start(&tree.path(), None);
    assert_eq!(result.err(), Some(Status::IO));
}

#[test]
fn describing_allocates_nothing() {
    let tree = Tree::new("alloc");
    tree.clock_source("tsc");
    tree.rtc("rtc0", "1");
    let dev = start(&tree.path(), None).unwrap();
    let ((), allocated) = allocations(|| {
        let mut out = Description::default();
        dev.describe(&mut out);
        assert_eq!(out.items().count(), 3);
    });
    assert_eq!(allocated, 0);
}

#[test]
fn provides_no_contract() {
    let tree = Tree::new("no-contract");
    tree.clock_source("tsc");
    let mut dev = start(&tree.path(), None).unwrap();
    let mut buf = [0u8; 16];
    let mut reply = lemnos_core::Reply::new(&mut buf);
    let status = lemnos_core::Driver::dispatch(&mut dev, FACT, &[], &mut reply);
    assert_eq!(status, Status::NOT_SUPPORTED);
    assert!(
        <HostClock as lemnos_core::Driver>::MANIFEST
            .provides
            .is_empty()
    );
}

#[test]
fn check_config_accepts_a_full_config() {
    let entries = [
        ("sys_root", ConfigValue::Str("/sys")),
        ("rtc", ConfigValue::Str("rtc0")),
    ];
    assert_eq!(
        <HostClock as lemnos_core::Lifecycle>::check_config(&Config::new(&entries)),
        Ok(())
    );
}

#[test]
fn check_config_refuses_an_unknown_key_by_name() {
    let entries = [("bogus", ConfigValue::Int(1))];
    let err =
        <HostClock as lemnos_core::Lifecycle>::check_config(&Config::new(&entries)).unwrap_err();
    assert_eq!(err.key, "bogus");
}

#[test]
fn check_config_suggests_the_nearest_key() {
    let entries = [("sys_roto", ConfigValue::Str("/sys"))];
    let err =
        <HostClock as lemnos_core::Lifecycle>::check_config(&Config::new(&entries)).unwrap_err();
    assert_eq!(err.key, "sys_roto");
    assert_eq!(err.suggestion, Some("sys_root"));
}

#[test]
fn check_config_refuses_a_wrong_kind() {
    let entries = [("rtc", ConfigValue::Int(0))];
    let err =
        <HostClock as lemnos_core::Lifecycle>::check_config(&Config::new(&entries)).unwrap_err();
    assert_eq!(err.key, "rtc");
}
